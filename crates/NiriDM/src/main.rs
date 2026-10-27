//! `ndm` — NiriDM command line interface.

mod tui;

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ndm_core::config::Prefs;
use ndm_core::launcher;
use ndm_core::layout::capture_layout;
use ndm_core::paths::NiriDMPaths;
use ndm_core::popup::PopupContext;
use ndm_core::screen::{Arrangement, Screen};
use ndm_core::Niri;

#[derive(Parser)]
#[command(name = "ndm", about = "Niri Display Manager", version)]
struct Cli {
    /// Override the NiriDM config directory (mainly for tests).
    #[arg(long, global = true)]
    config_dir: Option<PathBuf>,

    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// Show connected outputs and the matching layout.
    Status,
    /// Save the current output arrangement as a named layout.
    Capture { name: String },
    /// Apply a layout by name, or `auto` to pick the matching one.
    Apply { name: String },
    /// Turn an output on or off (`ndm monitor DP-2 off`).
    Monitor {
        connector: String,
        #[arg(value_parser = ["on", "off"])]
        state: String,
    },
    /// Mirror one output onto another with wl-mirror: spawn wl-mirror, then
    /// steer its window to the target output fullscreen via `niri msg action`.
    /// Exits when wl-mirror exits cleanly (e.g. its window is closed).
    Mirror {
        /// Output to mirror.
        src: String,
        /// Output to keep the mirror window on (default: focused, else the
        /// first output other than `src`).
        dst: Option<String>,
        /// Mirror tool to use (overrides `mirror-tool` in config.kdl).
        #[arg(long, value_name = "PATH")]
        wl_mirror: Option<PathBuf>,
        /// Only mirror while this workspace is shown on the source output.
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<String>,
        /// Capture only this app's window instead of the whole source output.
        /// The window is temporarily floated so niri can report its geometry.
        #[arg(long, value_name = "APP_ID")]
        app: Option<String>,
        /// Hide the status bar while mirroring.
        #[arg(long)]
        hide_bar: bool,
        /// Suppress notifications while mirroring.
        #[arg(long)]
        hide_notification: bool,
        /// Mirror scaling mode (fit, cover, exact).
        #[arg(long, value_name = "MODE")]
        scaling: Option<String>,
    },
    /// Assign a workspace to an output in the active layout (persistent, via
    /// generated `workspace` blocks). A route means the workspace only ever
    /// appears on that output — "use this display for this workspace only".
    /// Without <output>, the route is removed.
    Route {
        /// Workspace name (or `*` to match the focused workspace).
        #[arg(value_name = "WORKSPACE")]
        workspace: String,
        /// Output connector to pin it to.
        #[arg(value_name = "OUTPUT")]
        output: Option<String>,
    },
    /// Transiently move a workspace to an output with
    /// `move-workspace-to-monitor` (no KDL is written).
    Switch {
        /// Workspace name.
        #[arg(value_name = "WORKSPACE")]
        workspace: String,
        /// Output connector.
        #[arg(value_name = "OUTPUT")]
        output: String,
    },
    /// Run the hotplug daemon (forwards to the sibling `ndm-daemon` binary).
    Daemon {
        /// Poll interval in milliseconds (default: `poll-interval-ms` in config.kdl).
        #[arg(long, value_name = "MS")]
        poll_ms: Option<u64>,
        /// Settle window in milliseconds (default: `settle-ms`).
        #[arg(long, value_name = "MS")]
        settle_ms: Option<u64>,
        /// Poll once, apply immediately, and exit.
        #[arg(long)]
        once: bool,
        /// Never prompt; log an unmatched monitor set instead.
        #[arg(long, conflicts_with = "popup_cmd")]
        no_popup: bool,
        /// Popup command other than the default `ndm popup`.
        #[arg(long, value_name = "CMD", conflicts_with = "no_popup")]
        popup_cmd: Option<PathBuf>,
    },
    /// List saved layouts.
    List,
    /// Write the default config files if missing.
    Init,
    /// Show the quick-setup popup for a monitor (the daemon runs this).
    Popup {
        /// Monitor to configure. Defaults to `$NDM_NEW` when run by the daemon.
        connector: Option<String>,
        /// Popup timeout in seconds (overrides `config.kdl`).
        #[arg(long)]
        timeout: Option<u64>,
    },
    /// Interactive per-monitor settings (the TUI popup). `ndm popup` runs this
    /// in a terminal; you can also run it by hand.
    Tui {
        /// Output to configure (default: `$NDM_NEW` when run by the popup).
        #[arg(long, value_name = "OUTPUT")]
        new: Option<String>,
        /// Write the chosen action to this file (the popup reads it); when
        /// omitted the action is printed on stdout instead.
        #[arg(long, value_name = "PATH")]
        report: Option<PathBuf>,
        /// Dismiss if unanswered after this many seconds.
        #[arg(long)]
        timeout: Option<u64>,
    },
}

fn paths(cli: &Cli) -> Result<NiriDMPaths> {
    let paths = match &cli.config_dir {
        Some(dir) => NiriDMPaths {
            config: dir.join("config.kdl"),
            layouts: dir.join("layouts.kdl"),
            monitors: dir.join("monitors.kdl"),
            dir: dir.clone(),
        },
        None => NiriDMPaths::discover(),
    };
    paths.ensure_dirs()?;
    Ok(paths)
}

fn load_layouts(path: &PathBuf) -> Result<Vec<ndm_core::layout::Layout>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    ndm_core::kdl::parse_layouts(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
}

fn save_layouts(path: &PathBuf, layouts: &[ndm_core::layout::Layout]) -> Result<()> {
    std::fs::write(path, ndm_core::kdl::emit_layouts(layouts))
        .with_context(|| format!("failed to write {}", path.display()))
}

/// Insert `layout` into `layouts`, replacing any existing `auto` layout that
/// targets the same monitor set. Without this, an older `auto` layout for the
/// set stays ahead in file order and silently shadows the new decision.
fn store_auto_layout(
    layouts: &mut Vec<ndm_core::layout::Layout>,
    layout: ndm_core::layout::Layout,
) {
    layouts.retain(|l| {
        l.name != layout.name
            && !(l.name.starts_with("auto ") && ndm_core::layout::same_connector_set(l, &layout))
    });
    layouts.push(layout);
}

/// Write `monitors.kdl` for `layout`, skipping the write when it would be a
/// no-op (niri reloads on every write) and confirming the reload afterwards.
/// Returns whether it was written.
fn write_monitors(
    paths: &NiriDMPaths,
    outputs: &[ndm_core::types::Output],
    layout: &ndm_core::layout::Layout,
) -> Result<bool> {
    let kdl = ndm_core::kdl::generate_monitors_kdl(outputs, layout);
    Ok(ndm_core::verify::write_verified(&paths.monitors, &kdl)?)
}

/// Feedback line for commands whose stdout is theirs to use.
fn report_applied(paths: &NiriDMPaths, layout: &ndm_core::layout::Layout, written: bool) {
    if written {
        println!(
            "applied layout `{}` -> {}",
            layout.name,
            paths.monitors.display()
        );
    } else {
        println!("layout `{}` already applied", layout.name);
    }
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ndm=info")),
        )
        .with_writer(std::io::stderr)
        .init();
    let cli = Cli::parse();
    let paths = paths(&cli)?;
    let niri = Niri::new();

    match cli.cmd {
        Cmd::Status => {
            let outputs = niri.outputs().context("querying niri outputs")?;
            for o in &outputs {
                let state = if o.is_on() { "on " } else { "off" };
                let mode = o.current_mode().map(|m| m.to_string()).unwrap_or_default();
                println!("{state} {:<10} {mode:<20} {}", o.name, o.identity());
            }

            let layouts = load_layouts(&paths.layouts)?;
            if layouts.is_empty() {
                println!("\nno layouts saved (try `ndm capture <name>`)");
            } else if let Ok(m) = ndm_core::layout::find_matching(&layouts, &outputs) {
                println!("\nmatching layout: {}", m.name);
            } else {
                println!("\nno layout matches the current monitor set");
            }
        }
        Cmd::Capture { name } => {
            let outputs = niri.outputs().context("querying niri outputs")?;
            if outputs.is_empty() {
                bail!("no outputs reported by niri");
            }
            let mut layouts = load_layouts(&paths.layouts)?;
            let captured = capture_layout(&name, &outputs);
            layouts.retain(|l| l.name != name);
            layouts.push(captured);
            save_layouts(&paths.layouts, &layouts)?;
            println!("captured layout `{name}` -> {}", paths.layouts.display());

            // Capturing the current set means it now matches: apply it right
            // away so the setup takes effect without a hotplug event.
            if let Ok(layout) = ndm_core::layout::find_matching(&layouts, &outputs) {
                let written = write_monitors(&paths, &outputs, layout)?;
                report_applied(&paths, layout, written);
                remind_include(&paths)?;
            }
        }
        Cmd::Apply { name } => {
            let outputs = niri.outputs().context("querying niri outputs")?;
            let layouts = load_layouts(&paths.layouts)?;
            let layout = if name == "auto" {
                ndm_core::layout::find_matching(&layouts, &outputs)
                    .map_err(|e| anyhow::anyhow!("{e}"))?
            } else {
                layouts
                    .iter()
                    .find(|l| l.name == name)
                    .with_context(|| format!("layout `{name}` not found"))?
            };

            let written = write_monitors(&paths, &outputs, layout)?;
            report_applied(&paths, layout, written);
            remind_include(&paths)?;
        }
        Cmd::Monitor { connector, state } => {
            let outputs = niri.outputs().context("querying niri outputs")?;
            if !outputs.iter().any(|o| o.name == connector) {
                bail!("output `{connector}` is not connected");
            }
            let mut layouts = load_layouts(&paths.layouts)?;

            // Turn the single connector on/off inside the active layout.
            let active = ndm_core::layout::find_matching(&layouts, &outputs)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .name
                .clone();
            {
                let Some(layout) = layouts.iter_mut().find(|l| l.name == active) else {
                    bail!("no matching layout");
                };
                let Some(output) = layout.outputs.iter_mut().find(|o| o.connector == connector)
                else {
                    bail!("layout `{active}` does not manage `{connector}`");
                };
                output.role = if state == "off" {
                    ndm_core::layout::Role::Off
                } else {
                    ndm_core::layout::Role::Normal
                };
            }
            save_layouts(&paths.layouts, &layouts)?;

            let active_layout = layouts
                .iter()
                .find(|l| l.name == active)
                .expect("layout was just edited");
            write_monitors(&paths, &outputs, active_layout)?;
            println!("{connector} {state} (layout `{active}`)");
            remind_include(&paths)?;
        }
        Cmd::List => {
            let layouts = load_layouts(&paths.layouts)?;
            if layouts.is_empty() {
                println!("no layouts saved");
            }
            for l in layouts {
                println!("{} ({} output(s))", l.name, l.outputs.len());
            }
        }
        Cmd::Init => {
            init_files(&paths)?;
            remind_include(&paths)?;
        }
        Cmd::Popup { connector, timeout } => {
            run_popup(&paths, &niri, connector, timeout)?;
        }
        Cmd::Tui {
            new,
            report,
            timeout,
        } => {
            run_tui(&paths, &niri, new, report, timeout)?;
        }
        Cmd::Mirror {
            src,
            dst,
            wl_mirror,
            workspace,
            app,
            hide_bar,
            hide_notification,
            scaling,
        } => {
            let opts = MirrorOptions {
                dst: dst.as_deref(),
                wl_mirror: wl_mirror.as_deref(),
                workspace: workspace.as_deref(),
                app: app.as_deref(),
                hide_bar,
                hide_notification,
                scaling: scaling.as_deref(),
            };
            run_mirror(&paths, &niri, &src, &opts)?;
        }
        Cmd::Route { workspace, output } => {
            run_route(&paths, &niri, &workspace, output.as_deref())?;
        }
        Cmd::Switch { workspace, output } => {
            outputs_exist(&niri, &[output.as_str()])?;
            niri.action(
                "move-workspace-to-monitor",
                &["--reference", &workspace, &output],
            )?;
            println!("{workspace} -> {output}");
        }
        Cmd::Daemon {
            poll_ms,
            settle_ms,
            once,
            no_popup,
            popup_cmd,
        } => {
            run_daemon(poll_ms, settle_ms, once, no_popup, popup_cmd)?;
        }
    }

    Ok(())
}

/// `ndm daemon`: exec the sibling `ndm-daemon` binary (installed next to
/// `ndm`), forwarding the hotplug options so a service unit can talk to a
/// single binary.
fn run_daemon(
    poll_ms: Option<u64>,
    settle_ms: Option<u64>,
    once: bool,
    no_popup: bool,
    popup_cmd: Option<PathBuf>,
) -> Result<()> {
    let bin = std::env::current_exe()
        .context("finding the ndm binary")?
        .with_file_name("ndm-daemon");
    if !bin.is_file() {
        bail!(
            "{} not found next to this binary — install the `ndm-daemon` package",
            bin.display()
        );
    }

    let mut cmd = Command::new(&bin);
    if let Some(v) = poll_ms {
        cmd.args(["--poll-ms", &v.to_string()]);
    }
    if let Some(v) = settle_ms {
        cmd.args(["--settle-ms", &v.to_string()]);
    }
    if once {
        cmd.arg("--once");
    }
    if no_popup {
        cmd.arg("--no-popup");
    }
    if let Some(v) = popup_cmd {
        cmd.args(["--popup-cmd", &v.to_string_lossy()]);
    }
    let status = cmd
        .status()
        .with_context(|| format!("running {}", bin.display()))?;
    std::process::exit(status.code().unwrap_or(1));
}

/// `ndm route <workspace> [output]`: edit the workspace routing of the active
/// layout. Giving an output pins the workspace to it (persistently, in
/// `monitors.kdl`); omitting it removes the pin.
fn run_route(
    paths: &NiriDMPaths,
    niri: &Niri,
    workspace: &str,
    output: Option<&str>,
) -> Result<()> {
    let outputs = niri.outputs().context("querying niri outputs")?;
    let mut layouts = load_layouts(&paths.layouts)?;

    let active = ndm_core::layout::find_matching(&layouts, &outputs)
        .map_err(|e| anyhow::anyhow!("{e}"))?
        .name
        .clone();
    {
        let Some(layout) = layouts.iter_mut().find(|l| l.name == active) else {
            bail!("no matching layout");
        };
        layout.workspaces.retain(|(w, _)| w != workspace);
        if let Some(output) = output {
            if !outputs.iter().any(|o| o.name == output) {
                bail!("output `{output}` is not connected");
            }
            layout
                .workspaces
                .push((workspace.to_owned(), output.to_owned()));
        }
    }
    save_layouts(&paths.layouts, &layouts)?;

    let active_layout = layouts
        .iter()
        .find(|l| l.name == active)
        .expect("layout was just edited");
    write_monitors(paths, &outputs, active_layout)?;
    match output {
        Some(output) => println!("{workspace} -> {output} (layout `{active}`)"),
        None => println!("{workspace} no longer pinned (layout `{active}`)"),
    }
    remind_include(paths)?;
    Ok(())
}

fn outputs_exist(niri: &Niri, names: &[&str]) -> Result<()> {
    let outputs = niri.outputs().context("querying niri outputs")?;
    for name in names {
        if !outputs.iter().any(|o| o.name == *name) {
            bail!("output `{name}` is not connected");
        }
    }
    Ok(())
}

/// Check whether a binary is available on PATH (used to detect bar/notification daemons).
fn command_exists(name: &str) -> bool {
    Command::new("sh")
        .args(["-c", &format!("command -v {name}")])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check whether a process with exactly this name is running.
fn is_process_running(name: &str) -> bool {
    Command::new("pgrep")
        .arg("-x")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Check whether any process whose cmdline contains `needle` is running.
///
/// Used for shells hosted by a generic runner (e.g. Quickshell's `qs -c
/// noctalia-shell`), where the executable name says nothing about the config.
fn is_process_running_f(needle: &str) -> bool {
    Command::new("pgrep")
        .arg("-f")
        .arg(needle)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn run_shell_cmd(cmd: &str) -> bool {
    tracing::info!("running: {cmd}");
    match Command::new("sh").args(["-c", cmd]).status() {
        Ok(s) => s.success(),
        Err(e) => {
            tracing::warn!("failed to run `{cmd}`: {e}");
            false
        }
    }
}

/// Noctalia v5+ (`noctalia msg ...`). The executable name is exact so a v4
/// `noctalia-shell` process is not mistaken for it.
fn noctalia_v5_running() -> bool {
    command_exists("noctalia") && is_process_running("noctalia")
}

/// Noctalia v4 (Quickshell) uses `noctalia-shell ipc call ...`. Its process is
/// either the wrapper itself or the generic `qs`/`quickshell` runner with the
/// config name on the command line.
fn noctalia_v4_running() -> bool {
    command_exists("noctalia-shell")
        && (is_process_running("noctalia-shell")
            || is_process_running_f("noctalia-shell")
            || is_process_running_f("quickshell"))
}

/// The hide command and the matching restore command for a detected bar.
fn detect_bar() -> Option<(&'static str, &'static str)> {
    if noctalia_v5_running() {
        return Some(("noctalia msg bar-hide", "noctalia msg bar-show"));
    }
    if noctalia_v4_running() {
        return Some((
            "noctalia-shell ipc call bar hideBar",
            "noctalia-shell ipc call bar showBar",
        ));
    }
    if is_process_running("waybar") {
        // Waybar has no idempotent show/hide; SIGUSR1 toggles visibility.
        return Some(("pkill -USR1 waybar", "pkill -USR1 waybar"));
    }
    None
}

/// The hide command and the matching restore command for a detected
/// notification daemon.
fn detect_notifier() -> Option<(&'static str, &'static str)> {
    if noctalia_v5_running() {
        return Some((
            "noctalia msg notification-dnd-set on",
            "noctalia msg notification-dnd-set off",
        ));
    }
    if noctalia_v4_running() {
        return Some((
            "noctalia-shell ipc call notifications enableDND",
            "noctalia-shell ipc call notifications disableDND",
        ));
    }
    if is_process_running("mako") && command_exists("makoctl") {
        return Some((
            "makoctl mode -a do-not-disturb",
            "makoctl mode -r do-not-disturb",
        ));
    }
    if is_process_running("dunst") && command_exists("dunstctl") {
        return Some(("dunstctl set-paused true", "dunstctl set-paused false"));
    }
    if is_process_running("swaync") && command_exists("swaync-client") {
        // -dn/-df set the state explicitly (unlike -d, which toggles); -sw
        // makes the client fail instead of waiting forever when swaync is down.
        return Some(("swaync-client -dn -sw", "swaync-client -df -sw"));
    }
    None
}

/// Hide the bar and return the command that restores it.
///
/// Returning the restore command (instead of re-detecting later) means cleanup
/// always undoes exactly what was done, even if the bar daemon restarts while
/// the mirror is running.
fn hide_bar_cmd(prefs: &Prefs) -> Option<String> {
    if let Some(cmd) = &prefs.mirror_hide_bar_cmd {
        if !run_shell_cmd(cmd) {
            return None;
        }
        return match &prefs.mirror_show_bar_cmd {
            Some(show) => Some(show.clone()),
            None => {
                tracing::warn!(
                    "mirror-hide-bar-cmd is set without mirror-show-bar-cmd; the bar will stay hidden"
                );
                None
            }
        };
    }
    let (hide, show) = detect_bar()?;
    run_shell_cmd(hide).then(|| show.to_owned())
}

/// Suppress notifications and return the command that restores them.
fn hide_notification_cmd(prefs: &Prefs) -> Option<String> {
    if let Some(cmd) = &prefs.mirror_hide_notification_cmd {
        if !run_shell_cmd(cmd) {
            return None;
        }
        return match &prefs.mirror_show_notification_cmd {
            Some(show) => Some(show.clone()),
            None => {
                tracing::warn!(
                    "mirror-hide-notification-cmd is set without mirror-show-notification-cmd; notifications will stay suppressed"
                );
                None
            }
        };
    }
    let (hide, show) = detect_notifier()?;
    run_shell_cmd(hide).then(|| show.to_owned())
}

struct MirrorCleanup {
    bar_show: Option<String>,
    notif_show: Option<String>,
    /// App window we moved from tiling to floating for app-only capture.
    /// Restored on every exit path (including signals and hot-unplug).
    app_unfloat: Option<u64>,
}

impl Drop for MirrorCleanup {
    fn drop(&mut self) {
        if let Some(id) = self.app_unfloat {
            let niri = Niri::new();
            let id_str = id.to_string();
            // Only toggle back if the window still exists and is still
            // floating; the user may have closed it or tiled it themselves.
            if let Ok(windows) = niri.windows() {
                if windows.iter().any(|w| w.id == id && w.is_floating) {
                    let _ = niri.action("toggle-window-floating", &["--id", &id_str]);
                }
            }
        }
        if let Some(cmd) = &self.bar_show {
            run_shell_cmd(cmd);
        }
        if let Some(cmd) = &self.notif_show {
            run_shell_cmd(cmd);
        }
    }
}

struct MirrorOptions<'a> {
    dst: Option<&'a str>,
    wl_mirror: Option<&'a std::path::Path>,
    workspace: Option<&'a str>,
    app: Option<&'a str>,
    hide_bar: bool,
    hide_notification: bool,
    scaling: Option<&'a str>,
}

struct FilterState {
    workspace_filter: Option<String>,
    target_workspace_id: Option<u64>,
}

impl FilterState {
    fn new(niri: &Niri, src: &str, workspace_filter: Option<&str>) -> Self {
        let mut s = Self {
            workspace_filter: workspace_filter.map(String::from),
            target_workspace_id: None,
        };
        s.refresh(niri, src);
        s
    }

    fn refresh(&mut self, niri: &Niri, src: &str) {
        if let Some(wf) = &self.workspace_filter {
            let workspaces = niri.workspaces().unwrap_or_default();
            self.target_workspace_id = workspaces
                .iter()
                .find(|w| {
                    w.output.as_deref() == Some(src)
                        && (w
                            .name
                            .as_deref()
                            .is_some_and(|n| n.eq_ignore_ascii_case(wf))
                            || w.idx.to_string() == *wf)
                })
                .map(|w| w.id);
        }
    }
}

/// A rectangle to capture from the source output, in global logical pixels
/// (wl-mirror's slurp-style region format).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Region {
    x: i64,
    y: i64,
    w: i64,
    h: i64,
}

impl Region {
    /// Build a region from a window's output-local geometry and the source
    /// output's global bounds `(x, y, width, height)`.
    ///
    /// niri's `tile_pos_in_workspace_view` is relative to the output, while
    /// wl-mirror interprets `--region` in the global layout (slurp) coordinate
    /// space and then translates it into the named output's coordinates. A
    /// source output at `(0, 0)` hides the difference; a secondary output does
    /// not (`output does not contain region`).
    ///
    /// The result is clamped to the output because wl-mirror refuses a region
    /// that is not fully contained in it (a floating window may hang over the
    /// edge).
    fn from_layout(x: f64, y: f64, w: f64, h: f64, bounds: (f64, f64, f64, f64)) -> Option<Self> {
        let (ox, oy, ow, oh) = bounds;
        let left = x + ox;
        let top = y + oy;
        let right = left + w;
        let bottom = top + h;
        if !left.is_finite()
            || !top.is_finite()
            || !right.is_finite()
            || !bottom.is_finite()
            || !ow.is_finite()
            || !oh.is_finite()
        {
            return None;
        }
        let left = left.max(ox).round() as i64;
        let top = top.max(oy).round() as i64;
        let right = right.min(ox + ow).round() as i64;
        let bottom = bottom.min(oy + oh).round() as i64;
        let region = Self {
            x: left,
            y: top,
            w: right - left,
            h: bottom - top,
        };
        (region.w > 0 && region.h > 0).then_some(region)
    }

    /// `"<x>,<y> <w>x<h>"`, the slurp format wl-mirror expects.
    fn wl_arg(self) -> String {
        format!("{},{} {}x{}", self.x, self.y, self.w, self.h)
    }

    /// One stream-mode line updating the captured region.
    fn stream_cmd(self) -> String {
        format!("--region \"{}\"", self.wl_arg())
    }
}

/// State for `--app`: capture exactly one floating window instead of the
/// whole source output.
struct AppCapture {
    window_id: u64,
    workspace_id: Option<u64>,
    last_region: Option<Region>,
}

impl AppCapture {
    fn region(&self) -> Option<Region> {
        self.last_region
    }
}

/// Set when `ndm mirror` receives SIGINT/SIGTERM/SIGHUP.
///
/// The handler itself only flips this flag (async-signal-safe); the main loop
/// notices it, kills wl-mirror, and returns so [`MirrorCleanup`] can restore
/// the bar and notifications.
#[cfg(unix)]
static INTERRUPTED: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

#[cfg(unix)]
extern "C" fn on_termination_signal(_: libc::c_int) {
    INTERRUPTED.store(true, std::sync::atomic::Ordering::SeqCst);
}

#[cfg(unix)]
fn install_termination_handlers() {
    INTERRUPTED.store(false, std::sync::atomic::Ordering::SeqCst);
    let handler = on_termination_signal as *const () as libc::sighandler_t;
    unsafe {
        libc::signal(libc::SIGINT, handler);
        libc::signal(libc::SIGTERM, handler);
        libc::signal(libc::SIGHUP, handler);
    }
}

#[cfg(not(unix))]
fn install_termination_handlers() {}

#[cfg(unix)]
fn termination_requested() -> bool {
    INTERRUPTED.load(std::sync::atomic::Ordering::SeqCst)
}

#[cfg(not(unix))]
fn termination_requested() -> bool {
    false
}

/// `ndm mirror <src> [dst]`: run wl-mirror and keep its window useful.
///
/// wl-mirror mirrors a source output into a normal window, which is not what
/// anyone wants attached to an extra display. NiriDM watches the event stream
/// for that window and, the moment it appears, moves it onto `dst` and puts it
/// fullscreen. When wl-mirror exits cleanly (its window was closed) this
/// command ends; an unexpected wl-mirror death is restarted.
fn run_mirror(paths: &NiriDMPaths, niri: &Niri, src: &str, opts: &MirrorOptions) -> Result<()> {
    use ndm_core::events::{Event, EventStream};

    let prefs = Prefs::load(&paths.config)?;
    let tool = opts
        .wl_mirror
        .map(PathBuf::from)
        .or_else(|| prefs.mirror_tool.clone())
        .unwrap_or_else(|| PathBuf::from("wl-mirror"));

    // Resolve the target output before touching anything.
    let outputs = niri.outputs().context("querying niri outputs")?;
    if !outputs.iter().any(|o| o.name == src && o.is_on()) {
        bail!("output `{src}` is not connected or off");
    }
    let target = match opts.dst {
        Some(d) => {
            if d == src {
                bail!("cannot mirror an output onto itself");
            }
            if !outputs.iter().any(|o| o.name == d && o.is_on()) {
                bail!("output `{d}` is not connected or off");
            }
            d.to_owned()
        }
        None => {
            let fallback = outputs.iter().find(|o| o.name != src && o.is_on());
            match outputs
                .iter()
                .find(|o| o.is_focused && o.name != src && o.is_on())
            {
                Some(o) => o.name.clone(),
                None => match fallback {
                    Some(o) => o.name.clone(),
                    None => bail!("need a second output to mirror onto"),
                },
            }
        }
    };

    eprintln!("mirroring {src} -> {target} via {}", tool.display());
    eprintln!("close the mirror window (or stop this process) to end the mirror");

    // Install termination handlers before touching the bar/notification
    // state, so Ctrl-C / systemd stop still restores what we hid.
    install_termination_handlers();

    let bar_show = if opts.hide_bar {
        hide_bar_cmd(&prefs)
    } else {
        None
    };
    let notif_show = if opts.hide_notification {
        hide_notification_cmd(&prefs)
    } else {
        None
    };
    let mut cleanup = MirrorCleanup {
        bar_show,
        notif_show,
        app_unfloat: None,
    };

    // A signal may have arrived while the hide commands were running.
    if termination_requested() {
        return Ok(());
    }

    let mut filter_state = FilterState::new(niri, src, opts.workspace);

    // If a workspace filter was specified, make sure that workspace is active on `src`
    // before launching wl-mirror, so wl-mirror captures the clean target workspace first.
    if let Some(wf) = opts.workspace {
        let workspaces = niri.workspaces().unwrap_or_default();
        let is_already_active = workspaces.iter().any(|w| {
            w.output.as_deref() == Some(src)
                && w.is_active
                && (w
                    .name
                    .as_deref()
                    .is_some_and(|n| n.eq_ignore_ascii_case(wf))
                    || w.idx.to_string() == wf)
        });
        if !is_already_active {
            let _ = niri.action("focus-workspace", &[wf]);
            std::thread::sleep(Duration::from_millis(250));
            filter_state.refresh(niri, src);
        }
    }

    // App-only mode: float the selected window so niri exposes its on-screen
    // geometry, then capture just that rectangle instead of the whole output.
    let mut app_capture = match opts.app {
        Some(app) => Some(setup_app_capture(niri, src, app, &mut cleanup)?),
        None => None,
    };
    let use_stream = opts.workspace.is_some() || app_capture.is_some();
    let initial_region = app_capture.as_ref().and_then(AppCapture::region);

    let mut child = spawn_mirror(
        &tool,
        src,
        &target,
        use_stream,
        opts.scaling,
        initial_region.as_ref(),
    )?;
    let mut stream = EventStream::open()?;
    let mut current_window: Option<u64> = None;
    // Window we already placed on the target. This is a one-shot per window:
    // `fullscreen-window` *toggles*, so re-checking on every
    // `WindowOpenedOrChanged` (including the ones our own actions cause)
    // could take a correctly fullscreened mirror back out of fullscreen.
    let mut steered_window: Option<u64> = None;
    let mut frozen = false;
    let mut last_output_check = Instant::now() - Duration::from_secs(1);
    let mut last_app_check = Instant::now() - Duration::from_secs(1);

    // Refresh the active filter and stop if the captured app window is gone.
    // This is a macro rather than a closure because it needs mutable access to
    // several loop-local values at once.
    macro_rules! tick_or_stop {
        () => {
            if !tick_mirror(
                niri,
                src,
                &mut child,
                &mut frozen,
                &mut filter_state,
                &mut app_capture,
                opts.workspace,
            )? {
                eprintln!("app window closed; stopping mirror");
                let _ = child.kill();
                let _ = child.wait();
                return Ok(());
            }
        };
    }

    // Freeze the mirror whenever the filtered workspace or captured app is not
    // visible on the source output, and unfreeze it when it returns. The
    // compositor reports focus through `WindowFocusChanged` /
    // `WorkspaceActivated`, so we re-evaluate on those (and on the wholesale
    // `*Changed` snapshots) rather than guessing.
    if use_stream {
        tick_or_stop!();
    }

    loop {
        if termination_requested() {
            eprintln!("stopping mirror");
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }

        // niri 26.04 has no output-change event, so poll. This is what closes
        // the mirror when the source or target monitor is unplugged: without
        // it, niri may move the wl-mirror window elsewhere (or wl-mirror may
        // die and be restarted) and the mirror would linger on a wrong output.
        if last_output_check.elapsed() >= Duration::from_secs(1) {
            last_output_check = Instant::now();
            match missing_mirror_output(niri, src, &target) {
                Ok(Some(gone)) => {
                    eprintln!("output `{gone}` disconnected; stopping mirror");
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
                Ok(None) => {}
                Err(e) => tracing::warn!("could not check mirror outputs: {e:#}"),
            }
        }

        // Follow the app window while it is dragged/resized, and freeze if its
        // workspace is not the one currently shown on the source output.
        if app_capture.is_some() && last_app_check.elapsed() >= Duration::from_millis(500) {
            last_app_check = Instant::now();
            tick_or_stop!();
        }

        // An existing wl-mirror window (started before us, or our own) is
        // picked up right away; afterwards new ones are caught on the stream.
        if current_window.is_none() {
            if let Ok(windows) = niri.windows() {
                if let Some(w) = windows
                    .iter()
                    .find(|w| w.matches_tool("wl-mirror", Some(child.id())))
                {
                    current_window = Some(w.id);
                }
            }
        }

        // Make sure the mirror window is on the target output and fullscreen.
        // Retrying on the next tick covers the race where the window exists
        // but niri is not ready to act on it yet.
        if let Some(id) = current_window {
            if steered_window != Some(id) {
                match ensure_mirror_on_target(niri, id, &target) {
                    Ok(()) => steered_window = Some(id),
                    Err(e) => tracing::warn!("could not place wl-mirror window {id} yet: {e:#}"),
                }
            }
        }

        let timeout = if app_capture.is_some() {
            Duration::from_millis(500)
        } else {
            Duration::from_secs(1)
        };
        match stream.next(timeout)? {
            Some(Event::WindowOpenedOrChanged { window }) => {
                if window.matches_tool("wl-mirror", Some(child.id())) {
                    current_window = Some(window.id);
                }
                if use_stream {
                    last_app_check = Instant::now();
                    tick_or_stop!();
                }
            }
            Some(Event::WindowClosed { id }) => {
                if current_window == Some(id) {
                    tracing::debug!("mirror window {id} closed");
                    current_window = None;
                }
                if steered_window == Some(id) {
                    steered_window = None;
                }
                if app_capture
                    .as_ref()
                    .is_some_and(|capture| capture.window_id == id)
                {
                    eprintln!("app window closed; stopping mirror");
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(());
                }
                if use_stream {
                    last_app_check = Instant::now();
                    tick_or_stop!();
                }
            }
            // Workspace transitions fire at the START of the animation.
            // When moving away, freeze immediately so the display locks to the last clean frame.
            // When returning, wait for the slide animation to finish settling before unfreezing.
            Some(Event::WorkspaceActivated { id, .. }) if use_stream => {
                if app_capture.is_some() {
                    std::thread::sleep(Duration::from_millis(250));
                    last_app_check = Instant::now();
                    tick_or_stop!();
                } else if let Some(target_id) = filter_state.target_workspace_id {
                    if id != target_id {
                        set_frozen(&mut child, &mut frozen, true);
                    } else {
                        std::thread::sleep(Duration::from_millis(250));
                        tick_or_stop!();
                    }
                } else {
                    set_frozen(&mut child, &mut frozen, true);
                    std::thread::sleep(Duration::from_millis(250));
                    tick_or_stop!();
                }
            }
            Some(Event::WindowFocusChanged { .. }) if use_stream => {
                // Focus can change the source output's active workspace (e.g.
                // focusing a window on another workspace), so always re-check
                // from fresh state after it settles.
                std::thread::sleep(Duration::from_millis(200));
                last_app_check = Instant::now();
                tick_or_stop!();
            }
            Some(Event::WorkspacesChanged { .. }) | Some(Event::WindowsChanged { .. })
                if use_stream =>
            {
                last_app_check = Instant::now();
                tick_or_stop!();
            }
            Some(_) => {}
            None => {
                if use_stream {
                    last_app_check = Instant::now();
                    tick_or_stop!();
                }
            }
        }

        match child.try_wait().context("waiting on wl-mirror")? {
            Some(status) if status.success() => {
                eprintln!("wl-mirror exited; mirroring stopped");
                return Ok(());
            }
            Some(status) => {
                // Don't resurrect the mirror after the monitor it belongs to
                // was unplugged (wl-mirror often dies with its output).
                match missing_mirror_output(niri, src, &target) {
                    Ok(Some(gone)) => {
                        eprintln!("output `{gone}` disconnected; stopping mirror");
                        return Ok(());
                    }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("could not check mirror outputs: {e:#}"),
                }
                tracing::warn!("wl-mirror died ({status}); restarting");
                std::thread::sleep(Duration::from_secs(1));
                let region = app_capture.as_ref().and_then(AppCapture::region);
                child = spawn_mirror(
                    &tool,
                    src,
                    &target,
                    use_stream,
                    opts.scaling,
                    region.as_ref(),
                )?;
                current_window = None;
                steered_window = None;
                frozen = false;
                if use_stream {
                    last_app_check = Instant::now();
                    tick_or_stop!();
                }
            }
            None => {}
        }
    }
}

/// Write a single stream-mode command to wl-mirror's stdin.
fn send_mirror_cmd(child: &mut Child, cmd: &str) {
    if let Some(stdin) = child.stdin.as_mut() {
        use std::io::Write;
        let _ = writeln!(stdin, "{cmd}");
        let _ = stdin.flush();
    }
}

/// Freeze or unfreeze `child` so `frozen` reflects `should_freeze`.
fn set_frozen(child: &mut Child, frozen: &mut bool, should_freeze: bool) {
    if should_freeze == *frozen {
        return;
    }
    // In stream mode each line is parsed as command-line options, so the
    // leading dashes matter: a bare `freeze` is treated as an output name.
    send_mirror_cmd(
        child,
        if should_freeze {
            "--freeze"
        } else {
            "--unfreeze"
        },
    );
    *frozen = should_freeze;
}

/// First of `src`/`target` that is no longer connected or is switched off.
///
/// niri 26.04 does not emit an output-change event, so [`run_mirror`] polls
/// this once a second. Returning `Some` is what ends the mirror when a monitor
/// is unplugged.
fn missing_mirror_output(niri: &Niri, src: &str, target: &str) -> Result<Option<String>> {
    let outputs = niri.outputs().context("querying niri outputs")?;
    for name in [src, target] {
        if !outputs.iter().any(|o| o.name == name && o.is_on()) {
            return Ok(Some(name.to_owned()));
        }
    }
    Ok(None)
}

/// Whether the workspace shown on `src` satisfies `workspace_filter`.
///
/// wl-mirror shows exactly one workspace: the one *active* on `src` (niri
/// reports `is_active` per output; `is_focused` is the single focused
/// workspace across all outputs, which the mirror window itself steals on the
/// target). Querying fresh state keeps this correct even when several changes
/// race.
fn workspace_matches(
    workspaces: &[ndm_core::events::Workspace],
    src: &str,
    workspace_filter: Option<&str>,
) -> bool {
    let Some(wf) = workspace_filter else {
        return true;
    };
    workspaces
        .iter()
        .find(|w| w.is_active && w.output.as_deref() == Some(src))
        .is_some_and(|w| {
            w.name
                .as_deref()
                .is_some_and(|n| n.eq_ignore_ascii_case(wf))
                || w.idx.to_string() == wf
        })
}

/// Re-evaluate a workspace-only filter and freeze/unfreeze to match.
fn evaluate_filters(
    niri: &Niri,
    src: &str,
    child: &mut Child,
    frozen: &mut bool,
    workspace_filter: Option<&str>,
) {
    let workspaces = niri.workspaces().unwrap_or_default();
    set_frozen(
        child,
        frozen,
        !workspace_matches(&workspaces, src, workspace_filter),
    );
}

/// Refresh whichever filter is active. Returns `false` only when the app
/// window being captured has closed.
fn tick_mirror(
    niri: &Niri,
    src: &str,
    child: &mut Child,
    frozen: &mut bool,
    filter_state: &mut FilterState,
    app_capture: &mut Option<AppCapture>,
    workspace_filter: Option<&str>,
) -> Result<bool> {
    if let Some(capture) = app_capture.as_mut() {
        service_app_capture(niri, src, child, capture, frozen, workspace_filter)
    } else {
        filter_state.refresh(niri, src);
        evaluate_filters(niri, src, child, frozen, workspace_filter);
        Ok(true)
    }
}

/// Global logical bounds `(x, y, width, height)` of `src`, used to translate a
/// window's output-local geometry into wl-mirror's global region coordinates.
fn output_bounds(niri: &Niri, src: &str) -> Result<(f64, f64, f64, f64)> {
    let outputs = niri.outputs().context("querying niri outputs")?;
    let logical = outputs
        .iter()
        .find(|o| o.name == src)
        .and_then(|o| o.logical.as_ref())
        .with_context(|| format!("source output `{src}` has no logical geometry"))?;
    Ok((
        f64::from(logical.x),
        f64::from(logical.y),
        f64::from(logical.width),
        f64::from(logical.height),
    ))
}

/// Pick the app window to capture, float it so niri reports its on-screen
/// geometry, and focus it so its workspace is active on `src`.
///
/// niri 26.04 only fills in `tile_pos_in_workspace_view` for floating windows;
/// that is the reason app-only mirroring floats the window. The window is
/// restored to tiling by [`MirrorCleanup`] on every exit path.
fn setup_app_capture(
    niri: &Niri,
    src: &str,
    app: &str,
    cleanup: &mut MirrorCleanup,
) -> Result<AppCapture> {
    let windows = niri.windows().context("querying niri windows")?;
    let workspaces = niri.workspaces().context("querying niri workspaces")?;
    let bounds = output_bounds(niri, src)?;

    let matching: Vec<&ndm_core::events::Window> = windows
        .iter()
        .filter(|w| {
            w.app_id
                .as_deref()
                .is_some_and(|candidate| candidate.eq_ignore_ascii_case(app))
        })
        .collect();
    if matching.is_empty() {
        bail!("no window for app `{app}` is open");
    }

    let on_src = |w: &ndm_core::events::Window| {
        w.workspace_id
            .and_then(|id| workspaces.iter().find(|ws| ws.id == id))
            .is_some_and(|ws| ws.output.as_deref() == Some(src))
    };
    let active_workspace = workspaces
        .iter()
        .find(|ws| ws.is_active && ws.output.as_deref() == Some(src))
        .map(|ws| ws.id);
    let chosen = matching
        .iter()
        .copied()
        .find(|w| w.is_focused && on_src(w))
        .or_else(|| {
            matching
                .iter()
                .copied()
                .find(|w| w.workspace_id == active_workspace && on_src(w))
        })
        .or_else(|| matching.iter().copied().find(|w| on_src(w)))
        .unwrap_or(matching[0]);

    if !on_src(chosen) {
        let output = chosen
            .workspace_id
            .and_then(|id| workspaces.iter().find(|ws| ws.id == id))
            .and_then(|ws| ws.output.clone())
            .unwrap_or_else(|| "another output".to_owned());
        bail!("app `{app}` is on `{output}`, not the mirror source `{src}`");
    }

    let id = chosen.id;
    let id_str = id.to_string();
    if !chosen.is_floating {
        niri.action("toggle-window-floating", &["--id", &id_str])
            .with_context(|| format!("floating app window {id} for app-only mirroring"))?;
        // From here on, every exit path must put the window back.
        cleanup.app_unfloat = Some(id);
        std::thread::sleep(Duration::from_millis(150));
    }
    niri.action("focus-window", &["--id", &id_str])
        .with_context(|| format!("focusing app window {id}"))?;
    std::thread::sleep(Duration::from_millis(250));

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        let windows = niri.windows().context("querying niri windows")?;
        let Some(window) = windows.iter().find(|w| w.id == id) else {
            bail!("app window {id} closed while starting the mirror");
        };
        let workspaces = niri.workspaces().unwrap_or_default();
        let visible = window
            .workspace_id
            .and_then(|wsid| workspaces.iter().find(|ws| ws.id == wsid))
            .is_some_and(|ws| ws.output.as_deref() == Some(src) && ws.is_active);
        if visible {
            if let Some(region) = window
                .screen_region()
                .and_then(|(x, y, w, h)| Region::from_layout(x, y, w, h, bounds))
            {
                eprintln!(
                    "capturing app `{app}` window {id} region {}",
                    region.wl_arg()
                );
                return Ok(AppCapture {
                    window_id: id,
                    workspace_id: window.workspace_id,
                    last_region: Some(region),
                });
            }
        }
        if Instant::now() >= deadline {
            bail!(
                "could not determine the on-screen geometry of app `{app}` (window {id}); \
                 is it floating and visible on `{src}`?"
            );
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

/// Follow the captured app window: update the wl-mirror region when it moves
/// or resizes, and freeze while its workspace is not shown on `src`.
///
/// Returns `false` when the window no longer exists.
fn service_app_capture(
    niri: &Niri,
    src: &str,
    child: &mut Child,
    capture: &mut AppCapture,
    frozen: &mut bool,
    workspace_filter: Option<&str>,
) -> Result<bool> {
    let windows = match niri.windows() {
        Ok(windows) => windows,
        Err(e) => {
            tracing::warn!("could not query app window: {e:#}");
            set_frozen(child, frozen, true);
            return Ok(true);
        }
    };
    let Some(window) = windows.iter().find(|w| w.id == capture.window_id) else {
        return Ok(false);
    };
    capture.workspace_id = window.workspace_id;

    let workspaces = match niri.workspaces() {
        Ok(workspaces) => workspaces,
        Err(e) => {
            tracing::warn!("could not query workspaces: {e:#}");
            set_frozen(child, frozen, true);
            return Ok(true);
        }
    };
    let visible = window
        .workspace_id
        .and_then(|id| workspaces.iter().find(|ws| ws.id == id))
        .is_some_and(|ws| ws.output.as_deref() == Some(src) && ws.is_active);
    if !visible || !workspace_matches(&workspaces, src, workspace_filter) {
        set_frozen(child, frozen, true);
        return Ok(true);
    }

    let bounds = match output_bounds(niri, src) {
        Ok(bounds) => bounds,
        Err(e) => {
            tracing::warn!("could not query source output bounds: {e:#}");
            set_frozen(child, frozen, true);
            return Ok(true);
        }
    };
    let Some(region) = window
        .screen_region()
        .and_then(|(x, y, w, h)| Region::from_layout(x, y, w, h, bounds))
    else {
        // The user tiled the window again, or niri lost its geometry.
        set_frozen(child, frozen, true);
        return Ok(true);
    };
    if capture.last_region != Some(region) {
        send_mirror_cmd(child, &region.stream_cmd());
        capture.last_region = Some(region);
    }
    set_frozen(child, frozen, false);
    Ok(true)
}

/// Current window and the output its workspace belongs to.
fn mirror_window_state(niri: &Niri, id: u64) -> Result<(ndm_core::events::Window, Option<String>)> {
    let windows = niri.windows().context("querying niri windows")?;
    let window = windows
        .into_iter()
        .find(|w| w.id == id)
        .with_context(|| format!("wl-mirror window {id} is gone"))?;
    let workspaces = niri.workspaces().context("querying niri workspaces")?;
    let output = window
        .workspace_id
        .and_then(|wsid| workspaces.iter().find(|ws| ws.id == wsid))
        .and_then(|ws| ws.output.clone());
    Ok((window, output))
}

/// Whether the window is still smaller than `target`'s logical area.
///
/// A fullscreen window's tile size equals the output size. This is used only
/// after giving wl-mirror's own `--fullscreen-output` request time to apply.
fn window_is_smaller_than_output(
    niri: &Niri,
    window: &ndm_core::events::Window,
    target: &str,
) -> Result<bool> {
    let outputs = niri.outputs().context("querying niri outputs")?;
    let Some(logical) = outputs
        .iter()
        .find(|o| o.name == target)
        .and_then(|o| o.logical.as_ref())
    else {
        return Ok(false);
    };
    let (w, h) = window.layout.tile_size;
    Ok(w + 1.0 < f64::from(logical.width) || h + 1.0 < f64::from(logical.height))
}

/// Ensure the wl-mirror window is fullscreen on `target`.
///
/// wl-mirror is spawned with `--fullscreen-output target`, so normally it opens
/// fullscreen there and this is a no-op. If the compositor placed it on another
/// output, move it and then fullscreen it. A fullscreen window cannot be moved,
/// so a successful move means it was not fullscreen and toggling fullscreen is
/// safe.
///
/// If the window is already on the target, give wl-mirror's fullscreen request
/// a moment to apply before checking the window size. This avoids toggling a
/// correctly fullscreened window back into a tile.
fn ensure_mirror_on_target(niri: &Niri, id: u64, target: &str) -> Result<()> {
    let (window, output) = mirror_window_state(niri, id)?;
    let id_str = id.to_string();

    if output.as_deref() != Some(target) {
        niri.action("move-window-to-monitor", &["--id", &id_str, target])
            .with_context(|| format!("moving wl-mirror window {id} to {target}"))?;
        // The move and wl-mirror's own fullscreen request can race. Give niri a
        // moment, then only toggle fullscreen if the window is still smaller
        // than the output; otherwise wl-mirror's request already won and
        // toggling would take the mirror back out of fullscreen.
        std::thread::sleep(Duration::from_millis(200));
        let (window, output) = mirror_window_state(niri, id)?;
        if output.as_deref() != Some(target) {
            bail!("wl-mirror window {id} did not move to {target}");
        }
        if window_is_smaller_than_output(niri, &window, target)? {
            niri.action("fullscreen-window", &["--id", &id_str])
                .with_context(|| format!("fullscreening wl-mirror window {id}"))?;
            tracing::info!("moved wl-mirror window {id} to {target} and fullscreened it");
        }
        return Ok(());
    }

    if window_is_smaller_than_output(niri, &window, target)? {
        std::thread::sleep(Duration::from_millis(200));
        let (window, output) = mirror_window_state(niri, id)?;
        if output.as_deref() == Some(target)
            && window_is_smaller_than_output(niri, &window, target)?
        {
            niri.action("fullscreen-window", &["--id", &id_str])
                .with_context(|| format!("fullscreening wl-mirror window {id}"))?;
            tracing::info!("fullscreened wl-mirror window {id} on {target}");
        }
    }
    Ok(())
}

fn spawn_mirror(
    tool: &std::path::Path,
    src: &str,
    target: &str,
    stream: bool,
    scaling: Option<&str>,
    region: Option<&Region>,
) -> Result<Child> {
    let mut cmd = Command::new(tool);
    // wl-mirror parses `[options] <output>`: options must precede the output
    // name, otherwise it exits with "unexpected trailing arguments".
    //
    // Let wl-mirror request fullscreen on the destination output itself. This
    // is more reliable than moving a windowed surface and toggling fullscreen
    // afterwards: the window is created fullscreen in one step, with no
    // windowed flash or race with the initial configure.
    cmd.arg("--fullscreen-output").arg(target);
    if let Some(s) = scaling {
        cmd.args(["--scaling", s]);
    }
    if let Some(region) = region {
        cmd.arg("--region").arg(region.wl_arg());
    }
    if stream {
        cmd.arg("--stream");
    }
    cmd.arg(src);
    cmd.stdin(if stream {
        Stdio::piped()
    } else {
        Stdio::null()
    })
    .stdout(Stdio::null())
    .stderr(Stdio::null())
    .spawn()
    .with_context(|| format!("failed to start {}", tool.display()))
}

/// `ndm popup`: open the TUI settings screen for a new monitor in a terminal.
///
/// stdout carries the chosen action's report line; everything else goes to
/// stderr. When none of the configured terminals is on `PATH` the popup is
/// skipped (there is no menu fallback).
fn run_popup(
    paths: &NiriDMPaths,
    niri: &Niri,
    connector: Option<String>,
    timeout: Option<u64>,
) -> Result<()> {
    let prefs = Prefs::load(&paths.config)?;
    let outputs = niri.outputs().context("querying niri outputs")?;

    // Which monitor is this about?
    let new: Vec<String> = match connector {
        Some(c) => vec![c],
        None => {
            let from_env = std::env::var("NDM_NEW").unwrap_or_default();
            let env: Vec<String> = from_env.split_whitespace().map(str::to_owned).collect();
            if env.is_empty() {
                bail!("usage: ndm popup <connector>  (the daemon passes $NDM_NEW)");
            }
            env
        }
    };
    let target = new[0].clone();
    if !outputs.iter().any(|o| o.name == target) {
        bail!("output `{target}` is not connected");
    }

    // The popup is always the TUI, hosted in the first configured terminal
    // that exists. No terminal means no popup.
    let term = match launcher::detect(&prefs.popup_terminals, std::env::var_os("PATH").as_deref()) {
        Some(term) => term,
        None => {
            eprintln!(
                "no terminal found for the TUI popup (tried: {}); skipping",
                prefs.popup_terminals.join(", ")
            );
            return Ok(());
        }
    };
    run_tui_popup(paths, &target, &prefs, &term, timeout)
}

/// `ndm tui`: edit the settings for one monitor in a full-screen TUI, then
/// apply (and optionally save) the result. When `--report` is given the chosen
/// action lands in that file so the daemon popup can read it; otherwise it is
/// printed, so the command is usable by hand.
fn run_tui(
    paths: &NiriDMPaths,
    niri: &Niri,
    new: Option<String>,
    report: Option<PathBuf>,
    timeout: Option<u64>,
) -> Result<()> {
    let prefs = Prefs::load(&paths.config)?;
    let outputs = niri.outputs().context("querying niri outputs")?;

    let target = match new {
        Some(c) => c,
        None => {
            let from_env = std::env::var("NDM_NEW").unwrap_or_default();
            let first = from_env.split_whitespace().next();
            match first {
                Some(c) => c.to_owned(),
                None => bail!("usage: ndm tui --new <output>  (the popup passes $NDM_NEW)"),
            }
        }
    };
    if !outputs.iter().any(|o| o.name == target) {
        bail!("output `{target}` is not connected");
    }

    let layouts = load_layouts(&paths.layouts)?;
    let mut screen = Screen::new_preselecting(outputs.clone(), &target, &layouts)?;
    // Offer the current workspaces and running apps as mirror filters.
    let mut workspaces: Vec<String> = niri
        .workspaces()
        .map(|all| {
            all.into_iter()
                .map(|w| w.name.unwrap_or_else(|| w.idx.to_string()))
                .collect()
        })
        .unwrap_or_default();
    workspaces.sort();
    workspaces.dedup();
    let mut apps: Vec<String> = niri
        .windows()
        .map(|all| all.into_iter().filter_map(|w| w.app_id).collect())
        .unwrap_or_default();
    apps.sort();
    apps.dedup();
    screen.mirror_workspaces = workspaces;
    screen.mirror_apps = apps;
    let ctx = PopupContext {
        connected: outputs,
        new: vec![target.clone()],
        layouts: Vec::new(),
    };
    let describe = format!("NiriDM · {}", ctx.describe(&target));

    let deadline = timeout.map(|s| Instant::now() + Duration::from_secs(s));
    let outcome = tui::run(&mut screen, &describe, deadline)?;

    match outcome {
        tui::Outcome::Cancel => {
            if let Some(path) = &report {
                let _ = std::fs::remove_file(path);
            }
            eprintln!("popup dismissed");
            Ok(())
        }
        tui::Outcome::Apply => {
            let layout = screen.to_layout()?;
            let do_save = prefs.remember_new_monitors;
            if do_save {
                let mut layouts = load_layouts(&paths.layouts)?;
                store_auto_layout(&mut layouts, layout.clone());
                save_layouts(&paths.layouts, &layouts)?;
                eprintln!("saved layout `{}`", layout.name);
            }

            let written = write_monitors(paths, &screen.outputs, &layout)?;
            eprintln!(
                "{}",
                if written {
                    format!("applied `{}`", layout.name)
                } else {
                    format!("layout `{}` already applied", layout.name)
                }
            );
            remind_include(paths)?;

            if screen.arrangement == Arrangement::Mirror {
                if let Some(src) = screen.mirror_from.clone() {
                    start_mirror(
                        &src,
                        &screen.target,
                        screen.mirror_workspace.as_deref(),
                        screen.mirror_app.as_deref(),
                        screen.mirror_hide_bar,
                        screen.mirror_hide_notification,
                        Some(screen.mirror_scaling.as_arg()),
                    )?;
                }
            }

            if let Some(path) = &report {
                std::fs::write(path, screen.report())?;
            } else {
                println!("{}", screen.report());
            }
            Ok(())
        }
    }
}

/// Launch `ndm mirror <src> <dst>` detached, so the mirror keeps running after
/// the TUI (and its terminal) are gone.
fn start_mirror(
    src: &str,
    dst: &str,
    workspace: Option<&str>,
    app: Option<&str>,
    hide_bar: bool,
    hide_notification: bool,
    scaling: Option<&str>,
) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("finding the ndm binary")?;
    let mut cmd = Command::new(&exe);
    cmd.args(["mirror", src, dst]);
    if let Some(w) = workspace {
        cmd.args(["--workspace", w]);
    }
    if let Some(a) = app {
        cmd.args(["--app", a]);
    }
    if hide_bar {
        cmd.arg("--hide-bar");
    }
    if hide_notification {
        cmd.arg("--hide-notification");
    }
    if let Some(s) = scaling {
        cmd.args(["--scaling", s]);
    }
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    // Detach into a session of its own so closing this terminal cannot SIGHUP
    // the wire and stop the mirror (std's `CommandExt::setsid` is still
    // unstable, so call the syscall directly).
    #[cfg(unix)]
    unsafe {
        cmd.pre_exec(|| {
            if libc::setsid() == -1 {
                return Err(std::io::Error::last_os_error());
            }
            Ok(())
        });
    }
    cmd.spawn()
        .with_context(|| format!("starting {} mirror", exe.display()))?;
    Ok(())
}

/// Run `ndm tui` for `target` inside the first available terminal, waiting up
/// to the popup timeout and reporting the chosen action on stdout.
fn run_tui_popup(
    paths: &NiriDMPaths,
    target: &str,
    prefs: &Prefs,
    term: &PathBuf,
    timeout: Option<u64>,
) -> Result<()> {
    let limit = timeout
        .map(Duration::from_secs)
        .unwrap_or(prefs.popup_timeout);
    let exe = std::env::current_exe().context("finding the ndm binary")?;
    let report = paths
        .dir
        .join(format!(".popup-{}.report", std::process::id()));

    let program: Vec<OsString> = vec![
        exe.clone().into_os_string(),
        "tui".into(),
        "--new".into(),
        OsString::from(target),
        "--report".into(),
        report.clone().into_os_string(),
        "--timeout".into(),
        OsString::from(limit.as_secs().to_string()),
    ];
    let args = launcher::terminal_args(&term.display().to_string(), &program);

    let mut child = Command::new(term)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("running terminal {}", term.display()))?;

    // Own the outer deadline; the TUI also gets `--timeout` so it dismisses itself first.
    let _finished = launcher::wait_or_kill(&mut child, limit)?;

    let choice = std::fs::read_to_string(&report)
        .ok()
        .map(|t| t.trim().to_owned())
        .filter(|t| !t.is_empty());
    let _ = std::fs::remove_file(&report);

    match choice {
        Some(choice) => println!("{choice}"),
        None => eprintln!("popup dismissed (or timed out)"),
    }
    Ok(())
}

fn remind_include(paths: &NiriDMPaths) -> Result<()> {
    let main = NiriDMPaths::niri_main_config();
    let line = format!("include optional=true \"{}\"", paths.monitors.display());
    let text = std::fs::read_to_string(&main).unwrap_or_default();
    if text.contains(&line) || text.contains("NiriDM/monitors.kdl") {
        return Ok(());
    }
    eprintln!("note: add to {}:\n  {line}", main.display());
    Ok(())
}

fn init_files(paths: &NiriDMPaths) -> Result<()> {
    paths.ensure_dirs()?;

    if !paths.layouts.exists() {
        std::fs::write(&paths.layouts, ndm_core::kdl::emit_layouts(&[]))
            .with_context(|| format!("failed to write {}", paths.layouts.display()))?;
        println!("created {}", paths.layouts.display());
    }
    if !paths.config.exists() {
        let template = "\
config {
    // Terminals tried, in order, to host the TUI popup.
    popup-terminals \"alacritty\" \"foot\" \"kitty\" \"wezterm\" \"ghostty\" \"xterm\"
    // Seconds the popup waits before giving up.
    popup-timeout 20
    // Save the chosen arrangement so this monitor set never asks again.
    remember-new-monitors true
    // Daemon polling / debouncing.
    poll-interval-ms 1000
    settle-ms 750
    // Mirror tool used by `ndm mirror` (default: wl-mirror from PATH).
    mirror-tool \"wl-mirror\"
    // Shell command to hide/restore the status bar while mirroring (e.g. Waybar).
    // mirror-hide-bar-cmd \"pkill -USR1 waybar\"
    // mirror-show-bar-cmd \"pkill -USR1 waybar\"
    // Shell command to suppress/restore notifications while mirroring (e.g. Mako).
    // mirror-hide-notification-cmd \"makoctl mode -a do-not-disturb\"
    // mirror-show-notification-cmd \"makoctl mode -r do-not-disturb\"
}
";
        std::fs::write(&paths.config, template)
            .with_context(|| format!("failed to write {}", paths.config.display()))?;
        println!("created {}", paths.config.display());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::Region;

    #[test]
    fn app_region_uses_global_coordinates_and_clamps_to_the_output() {
        let bounds = (3840.0, 0.0, 2880.0, 1800.0);

        assert_eq!(
            Region::from_layout(100.0, 100.0, 800.0, 600.0, bounds).map(Region::wl_arg),
            Some("3940,100 800x600".to_owned())
        );
        assert_eq!(
            Region::from_layout(2500.0, 100.0, 800.0, 600.0, bounds).map(Region::wl_arg),
            Some("6340,100 380x600".to_owned())
        );
        assert_eq!(
            Region::from_layout(4000.0, 100.0, 800.0, 600.0, bounds),
            None
        );
    }
}

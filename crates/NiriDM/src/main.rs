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
        /// Only mirror when this workspace is focused (freeze otherwise).
        #[arg(long, value_name = "WORKSPACE")]
        workspace: Option<String>,
        /// Only mirror when an app with this ID is focused (freeze otherwise).
        #[arg(long, value_name = "APP_ID")]
        app: Option<String>,
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
        } => {
            run_mirror(&paths, &niri, &src, dst.as_deref(), wl_mirror.as_deref(), workspace.as_deref(), app.as_deref())?;
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

/// `ndm mirror <src> [dst]`: run wl-mirror and keep its window useful.
///
/// wl-mirror mirrors a source output into a normal window, which is not what
/// anyone wants attached to an extra display. NiriDM watches the event stream
/// for that window and, the moment it appears, moves it onto `dst` and puts it
/// fullscreen. When wl-mirror exits cleanly (its window was closed) this
/// command ends; an unexpected wl-mirror death is restarted.
fn run_mirror(
    paths: &NiriDMPaths,
    niri: &Niri,
    src: &str,
    dst: Option<&str>,
    wl_mirror: Option<&std::path::Path>,
    workspace_filter: Option<&str>,
    app_filter: Option<&str>,
) -> Result<()> {
    use ndm_core::events::{Event, EventStream};

    let prefs = Prefs::load(&paths.config)?;
    let tool = wl_mirror
        .map(PathBuf::from)
        .or_else(|| prefs.mirror_tool.clone())
        .unwrap_or_else(|| PathBuf::from("wl-mirror"));

    // Resolve the target output before touching anything.
    let outputs = niri.outputs().context("querying niri outputs")?;
    let target = match dst {
        Some(d) => {
            if d == src {
                bail!("cannot mirror an output onto itself");
            }
            if !outputs.iter().any(|o| o.name == d) {
                bail!("output `{d}` is not connected");
            }
            d.to_owned()
        }
        None => {
            let fallback = outputs.iter().find(|o| o.name != src);
            match outputs.iter().find(|o| o.is_focused && o.name != src) {
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

    let use_stream = workspace_filter.is_some() || app_filter.is_some();
    let mut child = spawn_mirror(&tool, src, Some(&target), use_stream)?;
    let mut stream = EventStream::open()?;
    let mut current_window: Option<u64> = None;
    let mut frozen = false;

    // Build workspace map
    let mut workspace_map: std::collections::HashMap<u64, String> = std::collections::HashMap::new();
    if use_stream {
        if let Ok(ws) = niri.workspaces() {
            for w in ws {
                if let Some(name) = &w.name {
                    workspace_map.insert(w.id, name.clone());
                } else {
                    workspace_map.insert(w.id, w.idx.to_string());
                }
            }
        }
    }

    fn send_cmd(child: &mut Child, cmd: &str) -> Result<()> {
        if let Some(stdin) = child.stdin.as_mut() {
            use std::io::Write;
            let _ = writeln!(stdin, "{}", cmd);
        }
        Ok(())
    }

    fn update_freeze(
        child: &mut Child,
        frozen: &mut bool,
        should_freeze: bool,
    ) -> Result<()> {
        if should_freeze && !*frozen {
            send_cmd(child, "freeze")?;
            *frozen = true;
        } else if !should_freeze && *frozen {
            send_cmd(child, "unfreeze")?;
            *frozen = false;
        }
        Ok(())
    }

    fn matches_filter(
        workspace_filter: Option<&str>,
        app_filter: Option<&str>,
        window: &ndm_core::events::Window,
        workspace_map: &std::collections::HashMap<u64, String>,
    ) -> bool {
        let mut ok = true;
        if let Some(wf) = workspace_filter {
            let wname = window
                .workspace_id
                .as_ref()
                .and_then(|id| workspace_map.get(id))
                .map(|s| s.as_str())
                .unwrap_or("");
            ok = ok && wname.eq_ignore_ascii_case(wf);
        }
        if let Some(af) = app_filter {
            let app = window.app_id.as_deref().unwrap_or("");
            ok = ok && app.eq_ignore_ascii_case(af);
        }
        ok
    }

    loop {
        // An existing wl-mirror window (started before us, or our own) is
        // steered right away; afterwards new ones are caught on the stream.
        if current_window.is_none() {
            if let Ok(windows) = niri.windows() {
                if let Some(w) = windows
                    .iter()
                    .find(|w| w.matches_tool("wl-mirror", Some(child.id())))
                {
                    steer_window(niri, w.id, &target)?;
                    current_window = Some(w.id);
                }
            }
        }

        match stream.next(Duration::from_secs(5))? {
            Some(Event::WindowOpenedOrChanged { window })
                if window.matches_tool("wl-mirror", Some(child.id())) =>
            {
                steer_window(niri, window.id, &target)?;
                current_window = Some(window.id);
            }
            Some(Event::WindowClosed { id }) if current_window == Some(id) => {
                tracing::debug!("mirror window {id} closed");
                current_window = None;
            }
            Some(Event::WindowsChanged { windows }) if use_stream => {
                let focused_match = windows.iter().find(|w| w.is_focused).map(|w| matches_filter(workspace_filter, app_filter, w, &workspace_map)).unwrap_or(false);
                update_freeze(&mut child, &mut frozen, !focused_match)?;
            }
            Some(Event::WindowOpenedOrChanged { window }) if use_stream && window.is_focused => {
                let focused_match = matches_filter(workspace_filter, app_filter, &window, &workspace_map);
                update_freeze(&mut child, &mut frozen, !focused_match)?;
            }
            Some(Event::WorkspacesChanged { workspaces }) if use_stream => {
                for w in &workspaces {
                    if let Some(name) = &w.name {
                        workspace_map.insert(w.id, name.clone());
                    } else {
                        workspace_map.insert(w.id, w.idx.to_string());
                    }
                }
                if let Ok(windows) = niri.windows() {
                    let focused_match = windows.iter().find(|w| w.is_focused).map(|w| matches_filter(workspace_filter, app_filter, w, &workspace_map)).unwrap_or(false);
                    update_freeze(&mut child, &mut frozen, !focused_match)?;
                }
            }
            Some(_) => {}
            None => {}
        }

        match child.try_wait().context("waiting on wl-mirror")? {
            Some(status) if status.success() => {
                eprintln!("wl-mirror exited; mirroring stopped");
                return Ok(());
            }
            Some(status) => {
                tracing::warn!("wl-mirror died ({status}); restarting");
                std::thread::sleep(Duration::from_secs(1));
                child = spawn_mirror(&tool, src, Some(&target), use_stream)?;
                current_window = None;
                frozen = false;
                if use_stream {
                    send_cmd(&mut child, "unfreeze")?;
                }
            }
            None => {}
        }
    }
}

/// Move the given window onto `target` and fullscreen it.
fn steer_window(niri: &Niri, id: u64, target: &str) -> Result<()> {
    let id = id.to_string();
    niri.action("move-window-to-monitor", &["--id", &id, target])?;
    niri.action("fullscreen-window", &["--id", &id])?;
    tracing::info!("steered wl-mirror window {id} to {target} fullscreen");
    Ok(())
}

fn spawn_mirror(
    tool: &std::path::Path,
    src: &str,
    fullscreen_target: Option<&str>,
    stream: bool,
) -> Result<Child> {
    let mut cmd = Command::new(tool);
    // wl-mirror parses `[options] <output>`: options must precede the output
    // name, otherwise it exits with "unexpected trailing arguments".
    if let Some(tgt) = fullscreen_target {
        cmd.args(["--fullscreen-output", tgt]);
    }
    if stream {
        cmd.arg("--stream");
    }
    cmd.arg(src);
    cmd.stdin(if stream { Stdio::piped() } else { Stdio::null() })
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
        tui::Outcome::Apply { save } => {
            let layout = screen.to_layout()?;
            let do_save = save || prefs.remember_new_monitors;
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
                    start_mirror(&src, &screen.target)?;
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
fn start_mirror(src: &str, dst: &str) -> Result<()> {
    #[cfg(unix)]
    use std::os::unix::process::CommandExt;

    let exe = std::env::current_exe().context("finding the ndm binary")?;
    let mut cmd = Command::new(&exe);
    cmd.args(["mirror", src, dst])
        .stdin(Stdio::null())
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
}
";
        std::fs::write(&paths.config, template)
            .with_context(|| format!("failed to write {}", paths.config.display()))?;
        println!("created {}", paths.config.display());
    }
    Ok(())
}

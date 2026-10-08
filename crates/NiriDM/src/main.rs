//! `ndm` — NiriDM command line interface.

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ndm_core::action::Action;
use ndm_core::config::Prefs;
use ndm_core::launcher::{self, Menu};
use ndm_core::layout::capture_layout;
use ndm_core::paths::NiriDMPaths;
use ndm_core::popup::PopupContext;
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
    /// List saved layouts.
    List,
    /// Write the default config files if missing.
    Init,
    /// Show the quick-setup popup for a monitor (the daemon runs this).
    Popup {
        /// Monitor to configure. Defaults to `$NDM_NEW` when run by the daemon.
        connector: Option<String>,
        /// Launcher to use instead of the configured chain.
        #[arg(long, value_name = "NAME_OR_PATH")]
        provider: Option<String>,
        /// Popup timeout in seconds (overrides `config.kdl`).
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
    let text = ndm_core::kdl::emit_layouts(layouts);
    std::fs::write(path, text).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
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
        Cmd::Popup {
            connector,
            provider,
            timeout,
        } => {
            run_popup(&paths, &niri, connector, provider, timeout)?;
        }
        Cmd::Mirror {
            src,
            dst,
            wl_mirror,
        } => {
            run_mirror(&paths, &niri, &src, dst.as_deref(), wl_mirror.as_deref())?;
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
    }

    Ok(())
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

    let mut child = spawn_mirror(&tool, src)?;
    let mut stream = EventStream::open()?;
    let mut current_window: Option<u64> = None;

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
                child = spawn_mirror(&tool, src)?;
                current_window = None;
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

fn spawn_mirror(tool: &std::path::Path, src: &str) -> Result<Child> {
    Command::new(tool)
        .arg(src)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("failed to start {}", tool.display()))
}

/// `ndm popup`: pick a quick arrangement for a new monitor and apply it.
///
/// stdout carries exactly one line — the chosen action — because the daemon
/// parses it. Everything else goes to stderr.
fn run_popup(
    paths: &NiriDMPaths,
    niri: &Niri,
    connector: Option<String>,
    provider: Option<String>,
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

    let layouts = load_layouts(&paths.layouts)?;
    let ctx = PopupContext {
        connected: outputs,
        new,
        layouts: layouts.iter().map(|l| l.name.clone()).collect(),
    };

    let menu = Menu {
        prompt: format!("NiriDM · {}", ctx.describe(&target)),
        items: Action::ALL.iter().map(|a| a.label(&target)).collect(),
    };

    let bin = match &provider {
        Some(p) if p.contains('/') => PathBuf::from(p),
        Some(p) => launcher::detect(std::slice::from_ref(p), std::env::var_os("PATH").as_deref())
            .with_context(|| format!("launcher `{p}` not found in PATH"))?,
        None => launcher::detect(&prefs.popup_providers, std::env::var_os("PATH").as_deref())
            .with_context(|| {
                format!(
                    "no popup provider found (tried: {})",
                    prefs.popup_providers.join(", ")
                )
            })?,
    };

    let limit = timeout
        .map(Duration::from_secs)
        .unwrap_or(prefs.popup_timeout);
    let Some(choice) = launcher::prompt(&bin, &menu, limit).context("running the popup")? else {
        eprintln!("popup dismissed (or timed out)");
        return Ok(());
    };
    let Some(action) = Action::from_label(&choice) else {
        bail!("launcher returned unknown choice {choice:?}");
    };

    let layout = action.synthesize(&ctx)?;
    eprintln!("{} → {}", ctx.describe(&target), action.label(&target));

    if prefs.remember_new_monitors {
        let mut layouts = layouts;
        layouts.retain(|l| l.name != layout.name);
        let name = layout.name.clone();
        layouts.push(layout.clone());
        save_layouts(&paths.layouts, &layouts)?;
        eprintln!("saved layout `{name}`");
    }

    let written = write_monitors(paths, &ctx.connected, &layout)?;
    eprintln!(
        "{}",
        if written {
            format!("applied `{}`", layout.name)
        } else {
            format!("layout `{}` already applied", layout.name)
        }
    );
    remind_include(paths)?;

    // The daemon reads this line; nothing else may reach stdout.
    println!("{}", action.label(&target));
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
// NiriDM preferences. Every key is optional; the values below are the
// defaults. This file is only created when missing, so edit freely.
config {
    // Launchers tried in order when a new monitor needs a decision.
    popup-providers \"fuzzel\" \"wofi\" \"bemenu\" \"yad\" \"zenity\" \"dmenu\"
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

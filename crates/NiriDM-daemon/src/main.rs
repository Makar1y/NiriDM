//! `ndm-daemon` — watches for monitor changes and applies the matching
//! layout, prompting the user when no layout matches.
//!
//! The loop is: poll `niri msg --json outputs` → fingerprint the set → wait
//! out a settle window (monitors enumerate in stages) → apply. While settled
//! the desired `monitors.kdl` is recomputed each poll, so a layout saved by
//! `ndm capture` while the daemon runs is picked up without a hotplug event.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use clap::Parser;
use ndm_core::hotplug::{fingerprint, Debounce};
use ndm_core::kdl::{generate_monitors_kdl, parse_layouts};
use ndm_core::layout::{find_matching, Layout};
use ndm_core::paths::{write_if_changed, NiriDMPaths};
use ndm_core::popup::{CommandPopup, LogPopup, PopupContext, PopupProvider};
use ndm_core::Niri;

#[derive(Parser)]
#[command(name = "ndm-daemon", about = "NiriDM hotplug daemon", version)]
struct Cli {
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    poll_ms: u64,

    /// How long a change must hold still before it is applied, in
    /// milliseconds (monitors enumerate in stages).
    #[arg(long, default_value_t = 750)]
    settle_ms: u64,

    /// Command to run when no saved layout matches the connected monitors
    /// (expects the chosen action on stdout). Default: log a warning.
    ///
    /// Milestone 3 ships `ndm popup` as the intended value.
    #[arg(long)]
    popup_cmd: Option<PathBuf>,

    /// Poll once, apply immediately, and exit (useful for testing).
    #[arg(long)]
    once: bool,
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("ndm_daemon=info")),
        )
        .init();

    let cli = Cli::parse();
    let paths = NiriDMPaths::discover();
    paths.ensure_dirs()?;
    let niri = Niri::new();
    let popup: Box<dyn PopupProvider> = match &cli.popup_cmd {
        Some(cmd) => Box::new(CommandPopup { cmd: cmd.clone() }),
        None => Box::new(LogPopup),
    };

    let mut debounce = Debounce::new(Duration::from_millis(cli.settle_ms));
    let mut previous: Vec<String> = Vec::new();

    loop {
        let outputs = match niri.outputs() {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("querying niri outputs failed: {e:#}");
                if cli.once {
                    return Err(e.into());
                }
                sleep(&cli);
                continue;
            }
        };

        let fp = fingerprint(&outputs);
        let fired = if cli.once {
            debounce.settle_now(&fp)
        } else {
            debounce.poll(&fp, Instant::now())
        };

        if let Some(fp) = &fired {
            tracing::debug!("change settled: {fp:?}");
        }

        // While settled, keep monitors.kdl converging on the matching layout.
        if debounce.settled().is_some() || fired.is_some() {
            if let Err(e) = step(&paths, &outputs, fired.as_ref(), &previous, &*popup) {
                if cli.once {
                    return Err(e);
                }
                tracing::warn!("step failed: {e:#}");
            }
        }

        if fired.is_some() {
            previous = outputs.iter().map(|o| o.name.clone()).collect();
        }
        if cli.once {
            return Ok(());
        }
        sleep(&cli);
    }
}

fn sleep(cli: &Cli) {
    std::thread::sleep(Duration::from_millis(cli.poll_ms));
}

/// One step of the state machine: auto-apply the matching layout, and — only
/// when the connected set just changed — trigger the popup if nothing matches.
fn step(
    paths: &NiriDMPaths,
    outputs: &[ndm_core::types::Output],
    fired: Option<&String>,
    previous: &[String],
    popup: &dyn PopupProvider,
) -> Result<()> {
    let layouts = load_layouts(&paths.layouts)?;

    match find_matching(&layouts, outputs) {
        Ok(layout) => {
            let kdl = generate_monitors_kdl(outputs, layout);
            let written = write_if_changed(&paths.monitors, &kdl)?;
            if written {
                tracing::info!("applied layout `{}`", layout.name);
            }
        }
        Err(e) => {
            // Only prompt when the set actually changed; otherwise the popup
            // would reappear on every poll.
            let Some(_changed_at) = fired else {
                return Ok(());
            };
            tracing::debug!("{e}");
            let new = outputs
                .iter()
                .filter(|o| !previous.iter().any(|p| p == &o.name))
                .map(|o| o.name.clone())
                .collect();
            let ctx = PopupContext {
                connected: outputs.to_vec(),
                new,
                layouts: layouts.iter().map(|l| l.name.clone()).collect(),
            };
            match popup.show(&ctx) {
                Ok(Some(action)) => {
                    tracing::info!("popup chose `{action}` (applied in Milestone 3)");
                }
                Ok(None) => tracing::debug!("popup dismissed; leaving outputs as they are"),
                Err(e) => tracing::warn!("popup failed: {e:#}"),
            }
        }
    }
    Ok(())
}

fn load_layouts(path: &PathBuf) -> Result<Vec<Layout>> {
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    parse_layouts(&text).map_err(|e| anyhow::anyhow!("failed to parse {}: {e}", path.display()))
}

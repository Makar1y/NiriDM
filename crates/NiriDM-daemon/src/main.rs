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
use ndm_core::config::Prefs;
use ndm_core::hotplug::{fingerprint, Debounce};
use ndm_core::kdl::{generate_monitors_kdl, parse_layouts};
use ndm_core::layout::{find_matching, Layout};
use ndm_core::paths::{write_if_changed, NiriDMPaths};
use ndm_core::popup::{CommandPopup, LogPopup, PopupContext, PopupProvider};
use ndm_core::Niri;

#[derive(Parser)]
#[command(name = "ndm-daemon", about = "NiriDM hotplug daemon", version)]
struct Cli {
    /// Poll interval in milliseconds (default: `poll-interval-ms` in config.kdl).
    #[arg(long, value_name = "MS")]
    poll_ms: Option<u64>,

    /// How long a change must hold still before it is applied, in
    /// milliseconds — monitors enumerate in stages (default: `settle-ms`).
    #[arg(long, value_name = "MS")]
    settle_ms: Option<u64>,

    /// Command to run when no saved layout matches the connected monitors
    /// (expects the chosen action on stdout). Defaults to the `ndm` binary
    /// next to this one, so `ndm popup` is used when available.
    #[arg(long, value_name = "CMD", conflicts_with = "no_popup")]
    popup_cmd: Option<PathBuf>,

    /// Never prompt; log an unmatched monitor set instead.
    #[arg(long, conflicts_with = "popup_cmd")]
    no_popup: bool,

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
    let prefs = Prefs::load(&paths.config)?;
    let niri = Niri::new();

    let poll = Duration::from_millis(cli.poll_ms.unwrap_or(prefs.poll_interval_ms));
    let settle = Duration::from_millis(cli.settle_ms.unwrap_or(prefs.settle_ms));
    tracing::info!(
        "polling every {poll:?}, settling for {settle:?} (from {})",
        paths.config.display()
    );

    let popup: Box<dyn PopupProvider> = if cli.no_popup {
        Box::new(LogPopup)
    } else {
        let cmd = cli.popup_cmd.clone().or_else(|| sibling_binary("ndm"));
        match cmd {
            Some(cmd) => {
                // A user-supplied command is the complete command; the
                // built-in helper is `ndm popup` and needs its subcommand.
                let is_ndm = cli.popup_cmd.is_none();
                Box::new(CommandPopup {
                    cmd,
                    args: if is_ndm {
                        vec!["popup".to_owned()]
                    } else {
                        Vec::new()
                    },
                    // The popup enforces its own (config.kdl) timeout; this is
                    // the outer safety net so a wedged launcher can't stall us.
                    timeout: prefs.popup_timeout + Duration::from_secs(10),
                })
            }
            None => {
                tracing::warn!("no popup helper found; use --popup-cmd or --no-popup");
                Box::new(LogPopup)
            }
        }
    };

    let mut debounce = Debounce::new(settle);
    let mut previous: Vec<String> = Vec::new();

    loop {
        let outputs = match niri.outputs() {
            Ok(o) => o,
            Err(e) => {
                tracing::warn!("querying niri outputs failed: {e:#}");
                if cli.once {
                    return Err(e.into());
                }
                std::thread::sleep(poll);
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
        std::thread::sleep(poll);
    }
}

/// A sibling binary of this executable, e.g. `ndm` next to `ndm-daemon`.
fn sibling_binary(name: &str) -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let sibling = exe.with_file_name(name);
    sibling.is_file().then_some(sibling)
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
            let new: Vec<String> = outputs
                .iter()
                .filter(|o| !previous.iter().any(|p| p == &o.name))
                .map(|o| o.name.clone())
                .collect();
            // An unrecognised set with nothing newly plugged in (cables
            // shuffled between known monitors): there is no decision to make.
            if new.is_empty() {
                tracing::warn!(
                    "no matching layout and no new monitor; leaving outputs as they are"
                );
                return Ok(());
            }
            let ctx = PopupContext {
                connected: outputs.to_vec(),
                new,
                layouts: layouts.iter().map(|l| l.name.clone()).collect(),
            };
            match popup.show(&ctx) {
                Ok(Some(choice)) => {
                    tracing::info!(
                        "popup chose `{choice}`; the layout is saved, next poll converges"
                    );
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

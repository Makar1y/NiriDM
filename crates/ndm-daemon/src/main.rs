//! `ndm-daemon` — watches for monitor changes and applies the matching
//! layout, showing a popup when no layout matches.
//!
//! Milestone 1 ships only the skeleton: poll once, apply `auto`, then exit.
//! The real hotplug loop (settle window, popup steering, wl-mirror control)
//! lands in Milestone 2.

use std::time::Duration;

use anyhow::{Context, Result};
use clap::Parser;
use ndm_core::paths::NdmPaths;
use ndm_core::Niri;

#[derive(Parser)]
#[command(name = "ndm-daemon", about = "NiriDM hotplug daemon", version)]
struct Cli {
    /// Poll interval in milliseconds.
    #[arg(long, default_value_t = 1000)]
    poll_ms: u64,

    /// Run a single poll and exit (useful for testing).
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
    let paths = NdmPaths::discover();
    paths.ensure_dirs()?;
    let niri = Niri::new();

    loop {
        match poll(&niri, &paths) {
            Ok(()) => {}
            Err(e) => tracing::warn!("poll failed: {e:#}"),
        }
        if cli.once {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(cli.poll_ms));
    }
}

fn poll(niri: &Niri, paths: &NdmPaths) -> Result<()> {
    let outputs = niri.outputs().context("querying niri outputs")?;
    let names: Vec<&str> = outputs.iter().map(|o| o.name.as_str()).collect();
    tracing::debug!("connected: {names:?}");

    if !paths.layouts.exists() {
        tracing::debug!("no layouts file yet; nothing to apply");
        return Ok(());
    }

    let text = std::fs::read_to_string(&paths.layouts)
        .with_context(|| format!("failed to read {}", paths.layouts.display()))?;
    let layouts = ndm_core::kdl::parse_layouts(&text)
        .map_err(|e| anyhow::anyhow!("failed to parse layouts: {e}"))?;

    let Ok(layout) = ndm_core::layout::find_matching(&layouts, &outputs) else {
        tracing::debug!("no layout matches {names:?} (popup comes in Milestone 2)");
        return Ok(());
    };

    let kdl = ndm_core::kdl::generate_monitors_kdl(&outputs, layout);
    let current = std::fs::read_to_string(&paths.monitors).unwrap_or_default();
    if current == kdl {
        return Ok(());
    }
    std::fs::write(&paths.monitors, &kdl)
        .with_context(|| format!("failed to write {}", paths.monitors.display()))?;
    tracing::info!("applied layout `{}`", layout.name);
    Ok(())
}
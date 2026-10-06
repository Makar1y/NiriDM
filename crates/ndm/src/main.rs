//! `ndm` — NiriDM command line interface.

use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ndm_core::layout::capture_layout;
use ndm_core::paths::NdmPaths;
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
    /// List saved layouts.
    List,
    /// Write the default config files if missing.
    Init,
}

fn paths(cli: &Cli) -> Result<NdmPaths> {
    let paths = match &cli.config_dir {
        Some(dir) => NdmPaths {
            config: dir.join("config.kdl"),
            layouts: dir.join("layouts.kdl"),
            monitors: dir.join("monitors.kdl"),
            dir: dir.clone(),
        },
        None => NdmPaths::discover(),
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
    std::fs::write(path, text)
        .with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}

fn main() -> Result<()> {
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

            let kdl = ndm_core::kdl::generate_monitors_kdl(&outputs, layout);
            std::fs::write(&paths.monitors, &kdl)
                .with_context(|| format!("failed to write {}", paths.monitors.display()))?;
            println!("applied layout `{}` -> {}", layout.name, paths.monitors.display());
            remind_include(&paths)?;
        }
        Cmd::Monitor { connector, state } => {
            let outputs = niri.outputs().context("querying niri outputs")?;
            if !outputs.iter().any(|o| o.name == connector) {
                bail!("output `{connector}` is not connected");
            }
            let layouts = load_layouts(&paths.layouts)?;
            let mut layouts = layouts;

            // Turn the single connector on/off inside the active layout.
            let active = ndm_core::layout::find_matching(&layouts, &outputs)
                .map_err(|e| anyhow::anyhow!("{e}"))?
                .name
                .clone();
            let Some(layout) = layouts.iter_mut().find(|l| l.name == active) else {
                bail!("no matching layout");
            };
            let Some(output) = layout.outputs.iter_mut().find(|o| o.connector == connector)
            else {
                bail!("layout `{active}` does not manage `{connector}`");
            };
            if state == "off" {
                output.role = ndm_core::layout::Role::Off;
            } else {
                output.role = ndm_core::layout::Role::Normal;
            }
            save_layouts(&paths.layouts, &layouts)?;

            let kdl = ndm_core::kdl::generate_monitors_kdl(&outputs, layout);
            std::fs::write(&paths.monitors, &kdl)
                .with_context(|| format!("failed to write {}", paths.monitors.display()))?;
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
    }

    Ok(())
}

fn remind_include(paths: &NdmPaths) -> Result<()> {
    let main = NdmPaths::niri_main_config();
    let line = format!("include optional=true \"{}\"", paths.monitors.display());
    let text = std::fs::read_to_string(&main).unwrap_or_default();
    if text.contains(&line) || text.contains("NiriDM/monitors.kdl") {
        return Ok(());
    }
    println!(
        "note: add to {}:\n  {line}",
        main.display()
    );
    Ok(())
}

fn init_files(paths: &NdmPaths) -> Result<()> {
    paths.ensure_dirs()?;

    if !paths.layouts.exists() {
        std::fs::write(&paths.layouts, ndm_core::kdl::emit_layouts(&[]))
            .with_context(|| format!("failed to write {}", paths.layouts.display()))?;
        println!("created {}", paths.layouts.display());
    }
    if !paths.config.exists() {
        std::fs::write(
            &paths.config,
            "// NiriDM preferences.\n",
        )
        .with_context(|| format!("failed to write {}", paths.config.display()))?;
        println!("created {}", paths.config.display());
    }
    Ok(())
}
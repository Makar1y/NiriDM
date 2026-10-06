//! The popup trigger: how the daemon asks the user when no saved layout
//! matches the connected set.
//!
//! Milestone 2 ships the *seam*: the daemon knows when a popup is needed and
//! can route it through a provider. The actual launcher chain (fuzzel → wofi →
//! bemenu → yad → zenity → dmenu), the action list, and the timeout UX are
//! Milestone 3; today the only providers are a logger (default) and an
//! external command (so `--popup-cmd` can be exercised end-to-end).

use std::path::PathBuf;
use std::process::Command;

use crate::error::{NdError, Result};
use crate::types::Output;

/// Everything the popup needs to describe the situation to the user.
#[derive(Debug, Clone)]
pub struct PopupContext {
    /// Connectors that are attached right now.
    pub connected: Vec<Output>,
    /// Connectors that appeared since the previous state (the trigger).
    pub new: Vec<String>,
    /// Names of layouts the user has already saved.
    pub layouts: Vec<String>,
}

impl PopupContext {
    /// One-line summary of a monitor, e.g. `DP-2 · Dell Inc. U2723QE ·
    /// 3840x2160@143.912` — the header of the popup.
    pub fn describe(&self, connector: &str) -> String {
        let Some(o) = self.connected.iter().find(|o| o.name == connector) else {
            return connector.to_owned();
        };
        let make_model = match (&o.make, &o.model) {
            (Some(m), Some(model)) => format!("{m} {model}"),
            (None, Some(model)) => model.clone(),
            (Some(m), None) => m.clone(),
            (None, None) => "unknown".to_owned(),
        };
        let mode = o
            .current_mode()
            .map(|m| m.to_string())
            .unwrap_or_else(|| "off".into());
        format!("{} · {make_model} · {mode}", o.name)
    }
}

/// Asks the user what to do about a monitor set no layout matches.
pub trait PopupProvider {
    /// Returns the chosen action, or `None` when the user dismissed the popup
    /// (or nothing could be shown).
    fn show(&self, ctx: &PopupContext) -> Result<Option<String>>;
}

/// Default provider: records the situation instead of prompting. Used when no
/// popup is configured or the session is non-interactive.
#[derive(Debug, Default, Clone, Copy)]
pub struct LogPopup;

impl PopupProvider for LogPopup {
    fn show(&self, ctx: &PopupContext) -> Result<Option<String>> {
        let who = if ctx.new.is_empty() {
            "no saved layout matches".to_owned()
        } else {
            format!(
                "no saved layout matches after {} appeared",
                ctx.new.join(", ")
            )
        };
        tracing::warn!(
            "{who}; connected: [{}]. Run `ndm capture <name>` to save this setup.",
            ctx.connected
                .iter()
                .map(|o| o.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
        Ok(None)
    }
}

/// Runs an external command (the `ndm popup` helper).
///
/// The command receives the situation as environment variables
/// (`NDM_CONNECTED`, `NDM_NEW`, `NDM_LAYOUTS` — space separated) and is
/// expected to print the chosen action on stdout. A non-zero exit, a missing
/// binary, empty stdout, or a blown deadline all mean "no choice made".
#[derive(Debug, Clone)]
pub struct CommandPopup {
    pub cmd: PathBuf,
    /// Extra arguments for the command (e.g. the `popup` subcommand when the
    /// command is `ndm` itself).
    pub args: Vec<String>,
    /// Safety net: the child is killed after this long, even if it enforces
    /// its own (shorter) popup timeout internally.
    pub timeout: std::time::Duration,
}

impl PopupProvider for CommandPopup {
    fn show(&self, ctx: &PopupContext) -> Result<Option<String>> {
        let mut child = Command::new(&self.cmd)
            .args(&self.args)
            .env(
                "NDM_CONNECTED",
                join(
                    &ctx.connected
                        .iter()
                        .map(|o| o.name.clone())
                        .collect::<Vec<_>>(),
                ),
            )
            .env("NDM_NEW", join(&ctx.new))
            .env("NDM_LAYOUTS", join(&ctx.layouts))
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .map_err(|e| NdError::SpawnFailed("popup command", e))?;

        let Some(status) = crate::launcher::wait_or_kill(&mut child, self.timeout)? else {
            tracing::warn!("popup command timed out after {:?}", self.timeout);
            return Ok(None);
        };

        let mut out = String::new();
        if let Some(mut stdout) = child.stdout.take() {
            use std::io::Read;
            let _ = stdout.read_to_string(&mut out);
        }
        let mut err = String::new();
        if let Some(mut stderr) = child.stderr.take() {
            use std::io::Read;
            let _ = stderr.read_to_string(&mut err);
        }
        if !status.success() {
            tracing::warn!(
                "popup command failed (status {}): {}",
                status.code().unwrap_or(-1),
                err.trim()
            );
            return Ok(None);
        }
        let text = out.trim().to_owned();
        Ok((!text.is_empty()).then_some(text))
    }
}

fn join(items: &[String]) -> String {
    items.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ctx() -> PopupContext {
        PopupContext {
            connected: vec![Output {
                name: "DP-2".into(),
                make: Some("Dell Inc.".into()),
                model: Some("U2723QE".into()),
                current_mode: Some(0),
                modes: vec![crate::types::Mode {
                    width: 3840,
                    height: 2160,
                    refresh_rate_mhz: 143_912,
                }],
                ..Default::default()
            }],
            new: vec!["DP-2".into()],
            layouts: vec!["docked".into()],
        }
    }

    #[test]
    fn describes_a_monitor_for_the_popup_header() {
        assert_eq!(
            ctx().describe("DP-2"),
            "DP-2 · Dell Inc. U2723QE · 3840x2160@143.912"
        );
        assert_eq!(ctx().describe("eDP-1"), "eDP-1");
    }

    #[test]
    fn log_provider_never_chooses_an_action() {
        assert_eq!(LogPopup.show(&ctx()).unwrap(), None);
    }
}

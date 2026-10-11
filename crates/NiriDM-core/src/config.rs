//! User preferences, stored in `~/.config/NiriDM/config.kdl`.
//!
//! ```kdl
//! config {
//!     popup-providers "fuzzel" "wofi" "bemenu" "yad" "zenity" "dmenu"
//!     popup-timeout 20
//!     popup-ui "tui"
//!     popup-terminals "alacritty" "foot" "kitty" "wezterm" "ghostty" "xterm"
//!     remember-new-monitors true
//!     poll-interval-ms 1000
//!     settle-ms 750
//!     mirror-tool "wl-mirror"
//! }
//! ```
//!
//! A missing file means "all defaults"; unknown keys are warnings so newer
//! NiriDM versions can add options without breaking older files.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{NdError, Result};
use crate::kdl::{kdl_nodes, Val};

/// How the popup asks the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PopupUi {
    /// Full-screen per-monitor settings ("`ndm tui`") in a terminal.
    Tui,
    /// Single-answer launcher menu (`fuzzel`, `wofi`, `zenity`, ...).
    Menu,
}

/// Preferences read from `config.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// Launcher chain, most preferred first.
    pub popup_providers: Vec<String>,
    /// How long the popup waits for an answer before giving up.
    pub popup_timeout: Duration,
    /// Whether the popup is the TUI settings screen or a launcher menu.
    pub popup_ui: PopupUi,
    /// Terminals tried (in order) to host the TUI popup.
    pub popup_terminals: Vec<String>,
    /// Automatically save the arrangement chosen in the popup as a layout,
    /// so the next time this monitor set appears no popup is needed.
    pub remember_new_monitors: bool,
    /// How often the daemon asks niri for the connected set.
    pub poll_interval_ms: u64,
    /// How long a change must hold still before the daemon acts on it.
    pub settle_ms: u64,
    /// The mirror tool `ndm mirror` spawns (default: `wl-mirror` from PATH).
    pub mirror_tool: Option<PathBuf>,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            popup_providers: ["fuzzel", "wofi", "bemenu", "yad", "zenity", "dmenu"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            popup_timeout: Duration::from_secs(20),
            popup_ui: PopupUi::Tui,
            popup_terminals: ["alacritty", "foot", "kitty", "wezterm", "ghostty", "xterm"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            remember_new_monitors: true,
            poll_interval_ms: 1000,
            settle_ms: 750,
            mirror_tool: None,
        }
    }
}

impl Prefs {
    /// Load preferences from `path`. A missing file yields the defaults.
    pub fn load(path: &Path) -> Result<Self> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(e) => {
                return Err(NdError::Read {
                    path: path.to_path_buf(),
                    source: e,
                })
            }
        };
        Self::parse(&text).map_err(|e| NdError::Other(format!("{}: {e}", path.display())))
    }

    /// Parse the contents of `config.kdl`.
    pub fn parse(text: &str) -> Result<Self> {
        let mut prefs = Self::default();
        for node in kdl_nodes(text)? {
            if node.name != "config" {
                tracing::warn!("ignoring top-level `{}` node in config.kdl", node.name);
                continue;
            }
            if !node.args.is_empty() {
                return Err(NdError::Other(
                    "`config` takes no positional arguments".into(),
                ));
            }
            for child in &node.children {
                match child.name.as_str() {
                    "popup-providers" => {
                        prefs.popup_providers = child
                            .args
                            .iter()
                            .map(|v| string_arg("popup-providers", v))
                            .collect::<Result<Vec<_>>>()?;
                        if prefs.popup_providers.is_empty() {
                            return Err(NdError::Other(
                                "`popup-providers` needs at least one name".into(),
                            ));
                        }
                    }
                    "popup-timeout" => match child.args.as_slice() {
                        [Val::Num(n)] if *n >= 0.0 => {
                            prefs.popup_timeout = Duration::from_secs_f64(*n)
                        }
                        _ => {
                            return Err(NdError::Other(
                                "`popup-timeout` needs a number of seconds".into(),
                            ))
                        }
                    },
                    "remember-new-monitors" => match child.args.as_slice() {
                        [Val::Str(s)] if s.eq_ignore_ascii_case("true") => {
                            prefs.remember_new_monitors = true
                        }
                        [Val::Str(s)] if s.eq_ignore_ascii_case("false") => {
                            prefs.remember_new_monitors = false
                        }
                        _ => {
                            return Err(NdError::Other(
                                "`remember-new-monitors` needs true or false".into(),
                            ))
                        }
                    },
                    "popup-ui" => match child.args.as_slice() {
                        [Val::Str(s)] if s.eq_ignore_ascii_case("tui") => {
                            prefs.popup_ui = PopupUi::Tui
                        }
                        [Val::Str(s)] if s.eq_ignore_ascii_case("menu") => {
                            prefs.popup_ui = PopupUi::Menu
                        }
                        _ => return Err(NdError::Other("`popup-ui` needs `tui` or `menu`".into())),
                    },
                    "popup-terminals" => {
                        prefs.popup_terminals = child
                            .args
                            .iter()
                            .map(|v| string_arg("popup-terminals", v))
                            .collect::<Result<Vec<_>>>()?;
                        if prefs.popup_terminals.is_empty() {
                            return Err(NdError::Other(
                                "`popup-terminals` needs at least one name".into(),
                            ));
                        }
                    }
                    "poll-interval-ms" => {
                        prefs.poll_interval_ms = millis_arg(child, "poll-interval-ms")?
                    }
                    "settle-ms" => prefs.settle_ms = millis_arg(child, "settle-ms")?,
                    "mirror-tool" => match child.args.as_slice() {
                        [Val::Str(s)] => prefs.mirror_tool = Some(PathBuf::from(s)),
                        _ => {
                            return Err(NdError::Other(
                                "`mirror-tool` needs a binary name or path".into(),
                            ))
                        }
                    },
                    other => tracing::warn!("unknown config key `{other}`"),
                }
            }
        }
        Ok(prefs)
    }
}

fn string_arg(what: &str, v: &Val) -> Result<String> {
    match v {
        Val::Str(s) => Ok(s.clone()),
        Val::Num(n) => Err(NdError::Other(format!(
            "`{what}` needs string arguments, found number {n}"
        ))),
    }
}

fn millis_arg(node: &crate::kdl::Node, what: &str) -> Result<u64> {
    match node.args.as_slice() {
        [Val::Num(n)] if *n >= 0.0 && n.fract() == 0.0 => Ok(*n as u64),
        _ => Err(NdError::Other(format!(
            "`{what}` needs a whole number of milliseconds"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_key_uses_defaults() {
        let p = Prefs::parse("").unwrap();
        assert_eq!(p, Prefs::default());
        assert_eq!(p.popup_providers[0], "fuzzel");
        assert_eq!(p.popup_timeout, Duration::from_secs(20));
        assert_eq!(p.popup_ui, PopupUi::Tui);
        assert_eq!(p.popup_terminals[0], "alacritty");
        assert!(p.remember_new_monitors);
        assert_eq!(p.poll_interval_ms, 1000);
        assert_eq!(p.settle_ms, 750);
    }

    #[test]
    fn parses_every_preference() {
        let p = Prefs::parse(
            r#"
            config {
                popup-providers "wofi" "fuzzel"
                popup-timeout 5
                popup-ui "menu"
                popup-terminals "foot" "kitty"
                remember-new-monitors false
                poll-interval-ms 250
                settle-ms 100
                mirror-tool "mymirror"
            }
            "#,
        )
        .unwrap();
        assert_eq!(
            p.popup_providers,
            vec!["wofi".to_owned(), "fuzzel".to_owned()]
        );
        assert_eq!(p.popup_timeout, Duration::from_secs(5));
        assert_eq!(p.popup_ui, PopupUi::Menu);
        assert_eq!(
            p.popup_terminals,
            vec!["foot".to_owned(), "kitty".to_owned()]
        );
        assert!(!p.remember_new_monitors);
        assert_eq!(p.poll_interval_ms, 250);
        assert_eq!(p.settle_ms, 100);
        assert_eq!(p.mirror_tool.as_deref(), Some(Path::new("mymirror")));
    }

    #[test]
    fn unknown_keys_are_not_fatal() {
        let p = Prefs::parse(
            r#"
            config {
                popup-timeout 7
                future-knob "x"
            }
            "#,
        )
        .unwrap();
        assert_eq!(p.popup_timeout, Duration::from_secs(7));
    }

    #[test]
    fn rejects_malformed_values() {
        assert!(Prefs::parse(r#"config { popup-timeout "soon" }"#).is_err());
        assert!(Prefs::parse(r#"config { remember-new-monitors maybe }"#).is_err());
        assert!(Prefs::parse(r#"config { popup-providers }"#).is_err());
        assert!(Prefs::parse(r#"config { popup-ui "web" }"#).is_err());
        assert!(Prefs::parse(r#"config { popup-terminals }"#).is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let p = Prefs::load(Path::new("/nonexistent/ndm-config.kdl")).unwrap();
        assert_eq!(p, Prefs::default());
    }
}

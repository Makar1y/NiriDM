//! User preferences, stored in `~/.config/NiriDM/config.kdl`.
//!
//! ```kdl
//! config {
//!     popup-timeout 20
//!     popup-terminals "alacritty" "foot" "kitty" "wezterm" "ghostty" "xterm"
//!     remember-new-monitors true
//!     poll-interval-ms 1000
//!     settle-ms 750
//!     mirror-tool "wl-mirror"
//!     sync-wallpaper true
//! }
//! ```
//!
//! A missing file means "all defaults"; unknown keys are warnings so newer
//! NiriDM versions can add options without breaking older files.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{NdError, Result};
use crate::kdl::{kdl_nodes, Val};

/// Preferences read from `config.kdl`.
#[derive(Debug, Clone, PartialEq)]
pub struct Prefs {
    /// How long the popup waits for an answer before giving up.
    pub popup_timeout: Duration,
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
    /// Copy the wallpaper currently shown on an existing output to a newly
    /// plugged output (Noctalia v4/v5). Enabled by default.
    pub sync_wallpaper: bool,
}

impl Default for Prefs {
    fn default() -> Self {
        Self {
            popup_timeout: Duration::from_secs(20),
            popup_terminals: ["alacritty", "foot", "kitty", "wezterm", "ghostty", "xterm"]
                .iter()
                .map(|s| (*s).to_owned())
                .collect(),
            remember_new_monitors: true,
            poll_interval_ms: 1000,
            settle_ms: 750,
            mirror_tool: None,
            sync_wallpaper: true,
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
                    "sync-wallpaper" => match child.args.as_slice() {
                        [Val::Str(s)] if s.eq_ignore_ascii_case("true") => {
                            prefs.sync_wallpaper = true
                        }
                        [Val::Str(s)] if s.eq_ignore_ascii_case("false") => {
                            prefs.sync_wallpaper = false
                        }
                        _ => {
                            return Err(NdError::Other(
                                "`sync-wallpaper` needs true or false".into(),
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
        assert_eq!(p.popup_timeout, Duration::from_secs(20));
        assert_eq!(p.popup_terminals[0], "alacritty");
        assert!(p.remember_new_monitors);
        assert_eq!(p.poll_interval_ms, 1000);
        assert_eq!(p.settle_ms, 750);
        assert!(p.sync_wallpaper);
    }

    #[test]
    fn parses_every_preference() {
        let p = Prefs::parse(
            r#"
            config {
                popup-timeout 5
                popup-terminals "foot" "kitty"
                remember-new-monitors false
                poll-interval-ms 250
                settle-ms 100
                mirror-tool "mymirror"
                sync-wallpaper false
            }
            "#,
        )
        .unwrap();
        assert_eq!(p.popup_timeout, Duration::from_secs(5));
        assert_eq!(
            p.popup_terminals,
            vec!["foot".to_owned(), "kitty".to_owned()]
        );
        assert!(!p.remember_new_monitors);
        assert_eq!(p.poll_interval_ms, 250);
        assert_eq!(p.settle_ms, 100);
        assert_eq!(p.mirror_tool.as_deref(), Some(Path::new("mymirror")));
        assert!(!p.sync_wallpaper);
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
        assert!(Prefs::parse(r#"config { popup-terminals }"#).is_err());
        assert!(Prefs::parse(r#"config { sync-wallpaper maybe }"#).is_err());
    }

    #[test]
    fn missing_file_yields_defaults() {
        let p = Prefs::load(Path::new("/nonexistent/ndm-config.kdl")).unwrap();
        assert_eq!(p, Prefs::default());
    }
}

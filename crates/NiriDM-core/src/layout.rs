//! The NiriDM layout model and layout-matching logic.
//!
//! A *layout* describes a physical arrangement of monitors: which connectors
//! are enabled (and how), which are turned off, which mirrors another output,
//! and which workspaces belong to a specific output. Layouts are stored in
//! `~/.config/NiriDM/layouts.kdl`.

use std::collections::HashSet;

use crate::error::Result;
use crate::types::{Output, Transform};

/// A single match clause. Empty fields are ignored; a clause *satisfies* a
/// connected monitor when every non-empty field matches it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MonitorMatch {
    pub name: Option<String>,
    pub make: Option<String>,
    pub model: Option<String>,
    pub serial: Option<String>,
}

impl MonitorMatch {
    /// Does `output` satisfy every field of this clause?
    pub fn matches(&self, output: &Output) -> bool {
        let name_ok = self
            .name
            .as_deref()
            .is_none_or(|v| v.eq_ignore_ascii_case(&output.name));
        let make_ok = self.make.as_deref().is_none_or(|v| {
            output
                .make
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(v))
        });
        let model_ok = self.model.as_deref().is_none_or(|v| {
            output
                .model
                .as_deref()
                .is_some_and(|m| m.eq_ignore_ascii_case(v))
        });
        let serial_ok = self
            .serial
            .as_deref()
            .is_none_or(|v| v.eq_ignore_ascii_case(&output.serial));
        name_ok && make_ok && model_ok && serial_ok
    }

    pub fn is_empty(&self) -> bool {
        self.name.is_none() && self.make.is_none() && self.model.is_none() && self.serial.is_none()
    }
}

/// How an output participates in a layout.
#[derive(Debug, Clone, PartialEq)]
pub enum Role {
    /// Regular output, or turned off entirely.
    Normal,
    /// Mirrors another connector via wl-mirror (daemon-driven).
    MirrorOf(String),
    /// Output is off.
    Off,
}

/// Settings for one connector within a layout.
#[derive(Debug, Clone, PartialEq)]
pub struct ManagedOutput {
    /// Connector name as niri reports it (`eDP-1`, `DP-2`, ...).
    pub connector: String,
    /// Display-mode string, either `WxH` or `WxH@refresh` (exact 3 decimals).
    pub mode: Option<String>,
    pub scale: Option<f64>,
    pub transform: Option<Transform>,
    /// Position in logical pixels.
    pub position: Option<(i64, i64)>,
    pub vrr: bool,
    pub role: Role,
}

impl ManagedOutput {
    pub fn enabled(&self) -> bool {
        !matches!(self.role, Role::Off)
    }
}

/// A named arrangement of monitors.
#[derive(Debug, Clone, PartialEq)]
pub struct Layout {
    pub name: String,
    /// Exact-set identifiers (all must be present, nothing else may be).
    pub set: Vec<MonitorMatch>,
    /// Any-of criteria (kanshi style): layout applies if any is satisfied.
    pub any: Vec<MonitorMatch>,
    pub outputs: Vec<ManagedOutput>,
    /// (workspace, output connector) routing.
    pub workspaces: Vec<(String, String)>,
}

impl Layout {
    /// True when this layout applies to the current connected set.
    pub fn matches(&self, connected: &[Output]) -> bool {
        // Referenced connectors must be connected.
        let names: HashSet<&str> = connected.iter().map(|o| o.name.as_str()).collect();
        if !self
            .outputs
            .iter()
            .all(|o| names.contains(o.connector.as_str()))
        {
            return false;
        }

        if !self.set.is_empty() {
            // Exact set: each `set` clause maps onto a distinct connected
            // monitor, and no connected monitor is left over.
            let mut used = vec![false; connected.len()];
            for clause in &self.set {
                let mut found = None;
                for (i, out) in connected.iter().enumerate() {
                    if !used[i] && clause.matches(out) {
                        found = Some(i);
                        break;
                    }
                }
                let Some(i) = found else { return false };
                used[i] = true;
            }
            return used.iter().all(|&u| u);
        }

        if !self.any.is_empty() {
            return self
                .any
                .iter()
                .any(|c| connected.iter().any(|o| c.matches(o)));
        }

        true
    }
}

/// Build a layout from the current live state (used by `NiriDM capture`).
pub fn capture_layout(name: &str, outputs: &[Output]) -> Layout {
    let set = outputs.iter().map(identity_clause).collect();
    let mut managed = Vec::new();
    for o in outputs {
        let role = if o.is_on() { Role::Normal } else { Role::Off };
        managed.push(dump_output(o, role));
    }
    Layout {
        name: name.to_owned(),
        set,
        any: Vec::new(),
        outputs: managed,
        workspaces: Vec::new(),
    }
}

/// "Freeze" one live output into a ManagedOutput the way `NiriDM capture` stores it.
pub fn dump_output(o: &Output, role: Role) -> ManagedOutput {
    let logical = o.logical;
    ManagedOutput {
        connector: o.name.clone(),
        mode: o.current_mode().map(|m| m.to_string()),
        scale: logical.map(|l| l.scale),
        transform: logical.map(|l| l.transform),
        position: logical.map(|l| (l.x as i64, l.y as i64)),
        vrr: o.vrr_enabled,
        role,
    }
}

fn identity_clause(o: &Output) -> MonitorMatch {
    if !o.serial.is_empty() {
        MonitorMatch {
            serial: Some(o.serial.clone()),
            ..Default::default()
        }
    } else if let Some(model) = &o.model {
        if !model.is_empty() {
            MonitorMatch {
                model: Some(model.clone()),
                ..Default::default()
            }
        } else {
            MonitorMatch {
                name: Some(o.name.clone()),
                ..Default::default()
            }
        }
    } else {
        MonitorMatch {
            name: Some(o.name.clone()),
            ..Default::default()
        }
    }
}

/// Find the first layout (in file order) that applies to `connected`.
pub fn find_matching<'a>(layouts: &'a [Layout], connected: &[Output]) -> Result<&'a Layout> {
    layouts
        .iter()
        .find(|l| l.matches(connected))
        .ok_or(crate::error::NdError::NoMatchingLayout(layouts.len()))
}

/// True when both layouts manage the same set of connectors (order-insensitive).
///
/// Two `auto` layouts that cover the same set shadow each other — only the
/// first in file order ever wins — so a fresh decision for a set replaces any
/// earlier `auto` layout for it.
pub fn same_connector_set(a: &Layout, b: &Layout) -> bool {
    let mut x: Vec<&str> = a.outputs.iter().map(|o| o.connector.as_str()).collect();
    let mut y: Vec<&str> = b.outputs.iter().map(|o| o.connector.as_str()).collect();
    x.sort_unstable();
    y.sort_unstable();
    x == y
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tied_output(name: &str, model: &str, serial: &str, on: bool) -> Output {
        Output {
            name: name.into(),
            model: Some(model.into()),
            serial: serial.into(),
            current_mode: if on { Some(0) } else { None },
            modes: vec![crate::types::Mode {
                width: 1920,
                height: 1080,
                refresh_rate_mhz: 144_000,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn exact_set_requires_full_coverage() {
        let l = Layout {
            name: "dock".into(),
            set: vec![MonitorMatch {
                serial: Some("A".into()),
                ..Default::default()
            }],
            any: vec![],
            outputs: vec![ManagedOutput {
                connector: "DP-1".into(),
                mode: None,
                scale: None,
                transform: None,
                position: None,
                vrr: false,
                role: Role::Normal,
            }],
            workspaces: vec![],
        };
        // Two monitors connected, layout only mentions one => no match.
        let two = vec![
            tied_output("DP-1", "Dell", "A", true),
            tied_output("eDP-1", "Laptop", "B", true),
        ];
        assert!(!l.matches(&two));
        // Single monitor, exact fit => match.
        let one = vec![tied_output("DP-1", "Dell", "A", true)];
        assert!(l.matches(&one));
    }

    #[test]
    fn any_criteria_is_or() {
        let l = Layout {
            name: "ext".into(),
            set: vec![],
            any: vec![
                MonitorMatch {
                    model: Some("U2723QE".into()),
                    ..Default::default()
                },
                MonitorMatch {
                    model: Some("P2723QE".into()),
                    ..Default::default()
                },
            ],
            outputs: vec![ManagedOutput {
                connector: "DP-1".into(),
                mode: None,
                scale: None,
                transform: None,
                position: None,
                vrr: false,
                role: Role::Normal,
            }],
            workspaces: vec![],
        };
        let p = vec![tied_output("DP-1", "P2723QE", "C", true)];
        assert!(l.matches(&p));
    }

    #[test]
    fn same_connector_set_ignores_order_and_roles() {
        let a = capture_layout(
            "auto extend-right DP-1",
            &[
                tied_output("DP-1", "Dell", "A", true),
                tied_output("eDP-1", "Laptop", "B", true),
            ],
        );
        let mut b = capture_layout(
            "auto turn-off DP-1",
            &[
                tied_output("eDP-1", "Laptop", "B", true),
                tied_output("DP-1", "Dell", "A", true),
            ],
        );
        b.outputs[1].role = Role::Off;
        assert!(same_connector_set(&a, &b));

        let single = capture_layout(
            "auto extend-right eDP-1",
            &[tied_output("eDP-1", "Laptop", "B", true)],
        );
        assert!(!same_connector_set(&a, &single));
    }

    #[test]
    fn capture_builds_exact_set_and_preserves_settings() {
        let out = tied_output("eDP-1", "Laptop", "LAP123", true);
        let l = capture_layout("laptop", std::slice::from_ref(&out));
        assert_eq!(
            l.set,
            vec![MonitorMatch {
                serial: Some("LAP123".into()),
                ..Default::default()
            }]
        );
        assert_eq!(l.outputs[0].mode.as_deref(), Some("1920x1080@144.000"));
        assert!(l.outputs[0].enabled());
    }
}

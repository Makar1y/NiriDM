//! The quick actions the popup offers for a freshly connected monitor.
//!
//! An action turns "there is a new screen" into a concrete [`Layout`]: it
//! starts from the current state (`capture`) and moves/disables exactly the
//! new monitor. The synthesised layout can be applied directly and, when
//! `remember-new-monitors` is on, saved under a stable name so the same
//! monitor set is recognised next time.

use crate::error::{NdError, Result};
use crate::layout::{capture_layout, Layout, Role};
use crate::popup::PopupContext;

/// A quick arrangement for a newly connected monitor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    ExtendRight,
    ExtendLeft,
    /// The new monitor is the only enabled output.
    OnlyThis,
    /// Disable the new monitor (keep everything else).
    TurnOff,
}

impl Action {
    /// The menu, in display order.
    pub const ALL: [Action; 4] = [
        Action::ExtendRight,
        Action::ExtendLeft,
        Action::OnlyThis,
        Action::TurnOff,
    ];

    /// Human label shown in the launcher. `target` is the new connector.
    pub fn label(&self, target: &str) -> String {
        match self {
            Action::ExtendRight => "Extend right".to_owned(),
            Action::ExtendLeft => "Extend left".to_owned(),
            Action::OnlyThis => "Only this monitor".to_owned(),
            Action::TurnOff => format!("Turn off {target}"),
        }
    }

    /// Parse a label picked from the menu back into an action.
    pub fn from_label(label: &str) -> Option<Action> {
        match label {
            "Extend right" => Some(Action::ExtendRight),
            "Extend left" => Some(Action::ExtendLeft),
            "Only this monitor" => Some(Action::OnlyThis),
            other if other.starts_with("Turn off ") => Some(Action::TurnOff),
            _ => None,
        }
    }

    /// Stable name for a layout synthesised by this action, e.g.
    /// `auto extend-right DP-9`. Re-running the action replaces the layout.
    pub fn layout_name(&self, target: &str) -> String {
        let slug = match self {
            Action::ExtendRight => "extend-right",
            Action::ExtendLeft => "extend-left",
            Action::OnlyThis => "only",
            Action::TurnOff => "turn-off",
        };
        format!("auto {slug} {target}")
    }

    /// Build the layout this action implies for the monitor set in `ctx`.
    ///
    /// The "new" monitor is `ctx.new[0]`; everything else keeps the settings
    /// it currently has.
    pub fn synthesize(&self, ctx: &PopupContext) -> Result<Layout> {
        let target = ctx
            .new
            .first()
            .ok_or_else(|| NdError::Other("popup has no new monitor to act on".into()))?;

        let mut layout = capture_layout(&self.layout_name(target), &ctx.connected);
        let idx = layout
            .outputs
            .iter()
            .position(|o| o.connector == *target)
            .ok_or_else(|| NdError::Other(format!("`{target}` is not connected")))?;

        match self {
            Action::TurnOff => {
                layout.outputs[idx].role = Role::Off;
            }
            Action::OnlyThis => {
                for (i, o) in layout.outputs.iter_mut().enumerate() {
                    if i != idx {
                        o.role = Role::Off;
                    }
                }
            }
            Action::ExtendRight | Action::ExtendLeft => {
                layout.outputs[idx].role = Role::Normal;
                let (x, y) = self.target_origin(ctx, target);
                layout.outputs[idx].position = Some((x, y));
            }
        }
        Ok(layout)
    }

    /// Where to place `target`: just beyond the right edge of everything, or
    /// just before the left edge, aligned with the top-most other output.
    fn target_origin(&self, ctx: &PopupContext, target: &str) -> (i64, i64) {
        let others: Vec<_> = ctx
            .connected
            .iter()
            .filter(|o| o.name != target)
            .filter(|o| o.logical.is_some())
            .collect();

        let y = others
            .iter()
            .filter_map(|o| o.logical.map(|l| l.y as i64))
            .min()
            .unwrap_or(0);

        let x = match self {
            Action::ExtendLeft => {
                let left = others
                    .iter()
                    .filter_map(|o| o.logical.map(|l| l.x as i64))
                    .min();
                let width = ctx
                    .connected
                    .iter()
                    .find(|o| o.name == target)
                    .and_then(|o| {
                        o.logical
                            .map(|l| l.width as i64)
                            .or_else(|| o.current_mode().map(|m| m.width as i64))
                    })
                    .unwrap_or(0);
                left.map(|l| l - width).unwrap_or(0)
            }
            _ => others
                .iter()
                .filter_map(|o| o.logical.map(|l| (l.x as i64) + (l.width as i64)))
                .max()
                .unwrap_or(0),
        };
        (x, y)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogicalOutput, Mode, Output, Transform};

    fn output(name: &str, x: i32, w: u32, on: bool) -> Output {
        Output {
            name: name.into(),
            serial: format!("SER-{name}"),
            current_mode: on.then_some(0),
            modes: vec![Mode {
                width: w,
                height: 1080,
                refresh_rate_mhz: 60_000,
            }],
            logical: on.then_some(LogicalOutput {
                x,
                y: 0,
                width: w,
                height: 1080,
                scale: 1.0,
                transform: Transform::Normal,
            }),
            ..Default::default()
        }
    }

    fn ctx(new: &str) -> PopupContext {
        PopupContext {
            connected: vec![
                output("eDP-1", 0, 1920, true),
                output("DP-1", 1920, 2560, true),
                output(new, 4480, 1920, true),
            ],
            new: vec![new.to_owned()],
            layouts: vec![],
        }
    }

    #[test]
    fn labels_round_trip() {
        for action in Action::ALL {
            let label = action.label("DP-9");
            assert_eq!(Action::from_label(&label), Some(action), "label {label:?}");
        }
        assert_eq!(Action::from_label("something else"), None);
    }

    #[test]
    fn extend_right_places_past_the_right_edge() {
        let layout = Action::ExtendRight.synthesize(&ctx("DP-9")).unwrap();
        let target = layout
            .outputs
            .iter()
            .find(|o| o.connector == "DP-9")
            .unwrap();
        assert_eq!(target.position, Some((4480, 0))); // 1920 + 2560
        assert_eq!(target.role, Role::Normal);
        // Everyone else is untouched.
        assert_eq!(layout.outputs[0].position, Some((0, 0)));
        assert_eq!(layout.outputs[1].position, Some((1920, 0)));
    }

    #[test]
    fn extend_left_places_before_everything() {
        let layout = Action::ExtendLeft.synthesize(&ctx("DP-9")).unwrap();
        let target = layout
            .outputs
            .iter()
            .find(|o| o.connector == "DP-9")
            .unwrap();
        assert_eq!(target.position, Some((-1920, 0)));
    }

    #[test]
    fn only_this_disables_the_others() {
        let layout = Action::OnlyThis.synthesize(&ctx("DP-9")).unwrap();
        assert_eq!(layout.outputs[2].role, Role::Normal);
        assert_eq!(layout.outputs[0].role, Role::Off);
        assert_eq!(layout.outputs[1].role, Role::Off);
    }

    #[test]
    fn turn_off_disables_only_the_target() {
        let layout = Action::TurnOff.synthesize(&ctx("DP-9")).unwrap();
        assert_eq!(layout.outputs[2].role, Role::Off);
        assert_eq!(layout.outputs[0].role, Role::Normal);
        assert_eq!(layout.outputs[1].role, Role::Normal);
    }

    #[test]
    fn synthesised_layout_matches_the_current_set() {
        let c = ctx("DP-9");
        let layout = Action::ExtendRight.synthesize(&c).unwrap();
        assert!(
            layout.matches(&c.connected),
            "synthesised layout must apply to the set it was built for"
        );
        assert_eq!(layout.name, "auto extend-right DP-9");
    }

    #[test]
    fn refuses_without_a_new_monitor() {
        let c = PopupContext {
            connected: vec![output("eDP-1", 0, 1920, true)],
            new: vec![],
            layouts: vec![],
        };
        assert!(Action::ExtendRight.synthesize(&c).is_err());
    }
}

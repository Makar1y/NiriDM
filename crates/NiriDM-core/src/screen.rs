//! The per-monitor settings model behind `ndm tui`.
//!
//! [`Screen`] holds everything the UI needs to edit one monitor's placement
//! and mode, and to turn that into a [`Layout`]. The rendering and key
//! handling live in the `ndm` binary; keeping the state machine here makes it
//! testable without a terminal.

use crate::action::Action;
use crate::error::{NdError, Result};
use crate::layout::{capture_layout, Layout, Role};
use crate::popup::PopupContext;
use crate::types::{Output, Transform};

/// How the target monitor is arranged relative to the others.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Arrangement {
    ExtendRight,
    ExtendLeft,
    Mirror,
    OnlyThis,
    Off,
}

impl Arrangement {
    /// Menu order.
    pub const ALL: [Arrangement; 5] = [
        Arrangement::ExtendRight,
        Arrangement::ExtendLeft,
        Arrangement::Mirror,
        Arrangement::OnlyThis,
        Arrangement::Off,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Arrangement::ExtendRight => "Extend right",
            Arrangement::ExtendLeft => "Extend left",
            Arrangement::Mirror => "Mirror",
            Arrangement::OnlyThis => "Only this monitor",
            Arrangement::Off => "Turn off",
        }
    }

    /// The quick [`Action`] this arrangement maps to, if any (`Mirror` is
    /// driven by wl-mirror, not a plain layout).
    fn action(self) -> Option<Action> {
        match self {
            Arrangement::ExtendRight => Some(Action::ExtendRight),
            Arrangement::ExtendLeft => Some(Action::ExtendLeft),
            Arrangement::OnlyThis => Some(Action::OnlyThis),
            Arrangement::Off => Some(Action::TurnOff),
            Arrangement::Mirror => None,
        }
    }
}

/// Mirror scaling modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MirrorScaleMode {
    Fit,
    Cover,
    Exact,
}

impl MirrorScaleMode {
    pub const ALL: [MirrorScaleMode; 3] = [
        MirrorScaleMode::Fit,
        MirrorScaleMode::Cover,
        MirrorScaleMode::Exact,
    ];

    pub fn label(self) -> &'static str {
        match self {
            MirrorScaleMode::Fit => "fit",
            MirrorScaleMode::Cover => "cover",
            MirrorScaleMode::Exact => "exact",
        }
    }

    pub fn as_arg(self) -> &'static str {
        self.label()
    }

    pub fn index(self) -> usize {
        match self {
            MirrorScaleMode::Fit => 0,
            MirrorScaleMode::Cover => 1,
            MirrorScaleMode::Exact => 2,
        }
    }
}

/// An adjustable row of the settings screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    Arrangement,
    MirrorFrom,
    MirrorWorkspace,
    MirrorApp,
    MirrorScaling,
    Resolution,
    Scale,
    Position,
    Transform,
    Vrr,
}

impl Field {
    pub fn label(self) -> &'static str {
        match self {
            Field::Arrangement => "Arrangement",
            Field::MirrorFrom => "Mirror from",
            Field::MirrorWorkspace => "Workspace filter",
            Field::MirrorApp => "App filter",
            Field::MirrorScaling => "Mirror scaling",
            Field::Resolution => "Resolution",
            Field::Scale => "Scale",
            Field::Position => "Position",
            Field::Transform => "Transform",
            Field::Vrr => "Variable refresh",
        }
    }
}

/// Scale presets, coarsest-to-finest around the common values.
const SCALE_STEPS: [f64; 7] = [1.0, 1.25, 1.5, 1.75, 2.0, 2.5, 3.0];

/// Transform presets in menu order.
const TRANSFORMS: [Transform; 8] = [
    Transform::Normal,
    Transform::Deg90,
    Transform::Deg180,
    Transform::Deg270,
    Transform::Flipped,
    Transform::Flipped90,
    Transform::Flipped180,
    Transform::Flipped270,
];

/// Cycle a tri-state filter value: `None` (off) and the candidate list, moving
/// by `delta`. Empty candidate list keeps the filter off.
fn cycle_filter(current: Option<String>, candidates: &[String], delta: i32) -> Option<String> {
    if candidates.is_empty() {
        return None;
    }
    let n = candidates.len() as i32 + 1; // +1 slot for "off"
    let cur = match current {
        None => 0,
        Some(v) => candidates
            .iter()
            .position(|c| *c == v)
            .map(|i| i as i32 + 1)
            .unwrap_or(0),
    };
    let next = (cur + delta).rem_euclid(n);
    if next == 0 {
        None
    } else {
        Some(candidates[(next - 1) as usize].clone())
    }
}

/// Editable state for the monitor `target` within the connected `outputs`.
#[derive(Debug, Clone, PartialEq)]
pub struct Screen {
    pub outputs: Vec<Output>,
    pub target: String,
    pub arrangement: Arrangement,
    /// Source connector when `arrangement == Mirror`.
    pub mirror_from: Option<String>,
    /// Only mirror while this workspace is focused (`None` = mirror always).
    pub mirror_workspace: Option<String>,
    /// Only mirror while an app with this ID is focused (`None` = always).
    pub mirror_app: Option<String>,
    /// Workspace names offered for the workspace filter.
    pub mirror_workspaces: Vec<String>,
    /// App IDs offered for the app filter.
    pub mirror_apps: Vec<String>,
    pub mirror_scaling: MirrorScaleMode,
    /// Index into the target's `modes`.
    pub mode_idx: usize,
    pub scale: f64,
    pub transform: Transform,
    pub vrr: bool,
    /// Manual position; `None` means "derive from the arrangement".
    pub position: Option<(i64, i64)>,
    /// Cursor over [`Screen::fields`].
    field: usize,
}

impl Screen {
    /// Build a screen for `target`; requires `target` to be connected.
    pub fn new(outputs: Vec<Output>, target: &str) -> Result<Self> {
        let t = outputs
            .iter()
            .find(|o| o.name == target)
            .ok_or_else(|| NdError::Other(format!("output `{target}` is not connected")))?;

        let mode_idx = t
            .current_mode
            .unwrap_or(0)
            .min(t.modes.len().saturating_sub(1));
        let scale = t
            .logical
            .map(|l| l.scale)
            .filter(|s| *s > 0.0)
            .unwrap_or(1.0);
        let transform = t.logical.map(|l| l.transform).unwrap_or(Transform::Normal);
        let vrr = t.vrr_enabled;
        let target = target.to_owned();
        let mirror_from = outputs
            .iter()
            .find(|o| o.name != target)
            .map(|o| o.name.clone());

        Ok(Screen {
            outputs,
            target,
            arrangement: Arrangement::ExtendRight,
            mirror_from,
            mirror_workspace: None,
            mirror_app: None,
            mirror_workspaces: Vec::new(),
            mirror_apps: Vec::new(),
            mirror_scaling: MirrorScaleMode::Fit,
            mode_idx,
            scale,
            transform,
            vrr,
            position: None,
            field: 0,
        })
    }

    /// Like [`Screen::new`], but pre-selects the arrangement, mode and mirror
    /// source from the first saved layout that applies to the connected set, so
    /// the popup opens on the last decision instead of a blank default.
    pub fn new_preselecting(
        outputs: Vec<Output>,
        target: &str,
        layouts: &[Layout],
    ) -> Result<Self> {
        let mut screen = Self::new(outputs, target)?;
        if let Ok(layout) = crate::layout::find_matching(layouts, &screen.outputs) {
            screen.preselect(layout);
        }
        Ok(screen)
    }

    /// Adopt the placement encoded in `layout` for the target monitor.
    fn preselect(&mut self, layout: &Layout) {
        let Some(t) = layout.outputs.iter().find(|o| o.connector == self.target) else {
            return;
        };

        if let Some(mode) = &t.mode {
            if let Some(i) = self
                .target_output()
                .and_then(|o| o.modes.iter().position(|m| m.to_string() == *mode))
            {
                self.mode_idx = i;
            }
        }
        if let Some(s) = t.scale {
            self.scale = s;
        }
        if let Some(tr) = t.transform {
            self.transform = tr;
        }
        self.vrr = t.vrr;

        match &t.role {
            Role::Off => self.arrangement = Arrangement::Off,
            Role::MirrorOf(src) => {
                self.arrangement = Arrangement::Mirror;
                self.mirror_from = Some(src.clone());
            }
            Role::Normal => {
                let others: Vec<&crate::layout::ManagedOutput> = layout
                    .outputs
                    .iter()
                    .filter(|o| o.connector != self.target && o.enabled())
                    .collect();
                if others.is_empty() {
                    self.arrangement = Arrangement::OnlyThis;
                } else {
                    let tx = t.position.map(|(x, _)| x).unwrap_or(0);
                    let left = others
                        .iter()
                        .filter_map(|o| o.position.map(|(x, _)| x))
                        .min();
                    self.arrangement = match left {
                        Some(mx) if tx < mx => Arrangement::ExtendLeft,
                        _ => Arrangement::ExtendRight,
                    };
                }
            }
        }
    }

    pub fn target_output(&self) -> Option<&Output> {
        self.outputs.iter().find(|o| o.name == self.target)
    }

    /// Connectors that can be the mirror source (every output but the target).
    pub fn mirror_candidates(&self) -> Vec<String> {
        self.outputs
            .iter()
            .filter(|o| o.name != self.target)
            .map(|o| o.name.clone())
            .collect()
    }

    /// The rows shown for the current arrangement.
    pub fn fields(&self) -> Vec<Field> {
        let mut fields = vec![Field::Arrangement];
        if self.arrangement == Arrangement::Mirror {
            fields.push(Field::MirrorFrom);
            fields.push(Field::MirrorWorkspace);
            fields.push(Field::MirrorApp);
            fields.push(Field::MirrorScaling);
        }
        if self.arrangement != Arrangement::Off {
            fields.push(Field::Resolution);
            fields.push(Field::Scale);
            if matches!(
                self.arrangement,
                Arrangement::ExtendRight | Arrangement::ExtendLeft | Arrangement::Mirror
            ) {
                fields.push(Field::Position);
            }
            fields.push(Field::Transform);
            fields.push(Field::Vrr);
        }
        fields
    }

    pub fn field_index(&self) -> usize {
        self.field.min(self.fields().len().saturating_sub(1))
    }

    pub fn current_field(&self) -> Field {
        let fields = self.fields();
        fields[self.field_index()]
    }

    pub fn next_field(&mut self) {
        let len = self.fields().len();
        if len > 0 {
            self.field = (self.field_index() + 1) % len;
        }
    }

    pub fn prev_field(&mut self) {
        let len = self.fields().len();
        if len > 0 {
            self.field = (self.field_index() + len - 1) % len;
        }
    }

    /// Change the focused value by `delta` steps (left = -1, right = +1).
    pub fn adjust(&mut self, delta: i32) {
        if delta == 0 {
            return;
        }
        match self.current_field() {
            Field::Arrangement => {
                let n = Arrangement::ALL.len() as i32;
                let cur = Arrangement::ALL
                    .iter()
                    .position(|a| *a == self.arrangement)
                    .unwrap_or(0) as i32;
                self.arrangement = Arrangement::ALL[(cur + delta).rem_euclid(n) as usize];
                self.clamp_field();
            }
            Field::MirrorFrom => {
                let candidates = self.mirror_candidates();
                if candidates.is_empty() {
                    self.mirror_from = None;
                } else {
                    let cur = self
                        .mirror_from
                        .as_ref()
                        .and_then(|m| candidates.iter().position(|c| c == m))
                        .unwrap_or(0) as i32;
                    let n = candidates.len() as i32;
                    self.mirror_from =
                        Some(candidates[(cur + delta).rem_euclid(n) as usize].clone());
                }
            }
            Field::Resolution => {
                let len = self.target_output().map(|o| o.modes.len()).unwrap_or(0);
                if len > 0 {
                    self.mode_idx =
                        ((self.mode_idx as i32 + delta).rem_euclid(len as i32)) as usize;
                }
            }
            Field::Scale => {
                let cur = self.scale_index() as i32;
                let n = SCALE_STEPS.len() as i32;
                self.scale = SCALE_STEPS[(cur + delta).rem_euclid(n) as usize];
            }
            Field::Position => {
                let w = self.target_width().max(1);
                let (x, y) = self.position.unwrap_or_else(|| self.auto_position());
                self.position = Some((x + delta as i64 * w, y));
            }
            Field::Transform => {
                let cur = self.transform_index() as i32;
                let n = TRANSFORMS.len() as i32;
                self.transform = TRANSFORMS[(cur + delta).rem_euclid(n) as usize];
            }
            Field::Vrr => self.vrr = !self.vrr,
            Field::MirrorWorkspace => {
                self.mirror_workspace = cycle_filter(
                    self.mirror_workspace.clone(),
                    &self.mirror_workspaces,
                    delta,
                )
            }
            Field::MirrorApp => {
                self.mirror_app = cycle_filter(self.mirror_app.clone(), &self.mirror_apps, delta)
            }
            Field::MirrorScaling => {
                let idx = self
                    .mirror_scaling
                    .index()
                    .saturating_add(1)
                    .min(MirrorScaleMode::ALL.len() - 1);
                self.mirror_scaling = MirrorScaleMode::ALL[idx];
            }
        }
    }

    /// Reset the manual position so it follows the arrangement again.
    pub fn reset_position(&mut self) {
        self.position = None;
    }

    /// The mode label currently selected, e.g. `1920x1080@60.000`.
    pub fn mode_string(&self) -> Option<String> {
        self.target_output()
            .and_then(|o| o.modes.get(self.mode_idx))
            .map(|m| m.to_string())
    }

    /// Where the target would be placed for the current arrangement.
    pub fn auto_position(&self) -> (i64, i64) {
        let others: Vec<&Output> = self
            .outputs
            .iter()
            .filter(|o| o.name != self.target && o.logical.is_some())
            .collect();

        let y = others
            .iter()
            .filter_map(|o| o.logical.map(|l| l.y as i64))
            .min()
            .unwrap_or(0);

        let x = match self.arrangement {
            Arrangement::ExtendLeft => {
                let left = others
                    .iter()
                    .filter_map(|o| o.logical.map(|l| l.x as i64))
                    .min();
                left.map(|l| l - self.target_width()).unwrap_or(0)
            }
            _ => others
                .iter()
                .filter_map(|o| o.logical.map(|l| (l.x as i64) + (l.width as i64)))
                .max()
                .unwrap_or(0),
        };
        (x, y)
    }

    /// The effective position: manual override or the arrangement default.
    pub fn effective_position(&self) -> (i64, i64) {
        self.position.unwrap_or_else(|| self.auto_position())
    }

    /// Turn the edited state into a concrete [`Layout`].
    pub fn to_layout(&self) -> Result<Layout> {
        let ctx = PopupContext {
            connected: self.outputs.clone(),
            new: vec![self.target.clone()],
            layouts: Vec::new(),
        };

        let mut layout = match self.arrangement {
            Arrangement::Mirror => {
                let src = self
                    .mirror_from
                    .clone()
                    .ok_or_else(|| NdError::Other("no monitor available to mirror from".into()))?;
                if src == self.target {
                    return Err(NdError::Other("cannot mirror a monitor onto itself".into()));
                }
                let mut layout = capture_layout(&self.layout_name(), &self.outputs);
                let idx = self.target_index(&layout)?;
                layout.outputs[idx].role = Role::MirrorOf(src);
                layout
            }
            other => other
                .action()
                .expect("non-mirror arrangements have an action")
                .synthesize(&ctx)?,
        };
        layout.name = self.layout_name();

        if self.arrangement != Arrangement::Off {
            let idx = self.target_index(&layout)?;
            let output = &mut layout.outputs[idx];
            output.mode = self.mode_string();
            output.scale = Some(self.scale);
            output.transform = Some(self.transform);
            output.vrr = self.vrr;
            if matches!(
                self.arrangement,
                Arrangement::ExtendRight | Arrangement::ExtendLeft | Arrangement::Mirror
            ) {
                output.position = Some(self.effective_position());
            }
        }
        Ok(layout)
    }

    /// Human label handed back to the daemon (and shown on apply).
    pub fn report(&self) -> String {
        match self.arrangement {
            Arrangement::Mirror => format!(
                "Mirror {} from {}",
                self.target,
                self.mirror_from.as_deref().unwrap_or("?")
            ),
            other => format!("{} {}", other.label(), self.target),
        }
    }

    fn layout_name(&self) -> String {
        match self.arrangement {
            Arrangement::Mirror => format!(
                "auto mirror {} of {}",
                self.target,
                self.mirror_from.as_deref().unwrap_or("?")
            ),
            other => other
                .action()
                .expect("non-mirror arrangements have an action")
                .layout_name(&self.target),
        }
    }

    fn target_index(&self, layout: &Layout) -> Result<usize> {
        layout
            .outputs
            .iter()
            .position(|o| o.connector == self.target)
            .ok_or_else(|| NdError::Other(format!("`{}` is not in the layout", self.target)))
    }

    fn target_width(&self) -> i64 {
        self.target_output()
            .and_then(|o| {
                o.logical
                    .map(|l| l.width as i64)
                    .or_else(|| o.current_mode().map(|m| m.width as i64))
            })
            .unwrap_or(0)
            .max(1)
    }

    fn scale_index(&self) -> usize {
        SCALE_STEPS
            .iter()
            .enumerate()
            .min_by(|(_, a), (_, b)| {
                (*a - self.scale)
                    .abs()
                    .partial_cmp(&(*b - self.scale).abs())
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .map(|(i, _)| i)
            .unwrap_or(0)
    }

    fn transform_index(&self) -> usize {
        TRANSFORMS
            .iter()
            .position(|t| *t == self.transform)
            .unwrap_or(0)
    }

    fn clamp_field(&mut self) {
        let len = self.fields().len();
        if len == 0 {
            self.field = 0;
        } else if self.field >= len {
            self.field = len - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogicalOutput, Mode};

    fn output(name: &str, x: i32, w: u32, on: bool) -> Output {
        Output {
            name: name.into(),
            serial: format!("SER-{name}"),
            current_mode: on.then_some(0),
            modes: vec![
                Mode {
                    width: w,
                    height: 1080,
                    refresh_rate_mhz: 60_000,
                },
                Mode {
                    width: w / 2,
                    height: 720,
                    refresh_rate_mhz: 59_940,
                },
            ],
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

    fn screen() -> Screen {
        Screen::new(
            vec![
                output("eDP-1", 0, 1920, true),
                output("DP-9", 4480, 2560, true),
            ],
            "DP-9",
        )
        .unwrap()
    }

    #[test]
    fn requires_a_connected_target() {
        let e = Screen::new(vec![output("eDP-1", 0, 1920, true)], "DP-2");
        assert!(e.is_err());
    }

    #[test]
    fn extend_right_places_past_the_right_edge() {
        let s = screen();
        let layout = s.to_layout().unwrap();
        let dp9 = layout
            .outputs
            .iter()
            .find(|o| o.connector == "DP-9")
            .unwrap();
        assert_eq!(dp9.position, Some((1920, 0)));
        assert_eq!(dp9.role, Role::Normal);
        assert_eq!(layout.name, "auto extend-right DP-9");
    }

    #[test]
    fn arrangement_cycles_and_reclamps_cursor() {
        let mut s = screen();
        // Move to the last field, then cycle the arrangement to Off (fewest rows).
        for _ in 0..10 {
            s.next_field();
        }
        assert!(s.field_index() < s.fields().len());
        s.adjust(1);
        assert!(s.field_index() < s.fields().len());
    }

    #[test]
    fn mirror_field_only_appears_for_mirror() {
        let mut s = screen();
        assert!(!s.fields().contains(&Field::MirrorFrom));
        s.arrangement = Arrangement::Mirror;
        assert!(s.fields().contains(&Field::MirrorFrom));
    }

    #[test]
    fn mirror_filters_cycle_from_off_through_candidates() {
        let candidates = vec!["1".to_owned(), "2".to_owned()];
        // off -> 1 -> 2 -> off, and back.
        assert_eq!(cycle_filter(None, &candidates, 1).as_deref(), Some("1"));
        assert_eq!(
            cycle_filter(Some("1".into()), &candidates, 1).as_deref(),
            Some("2")
        );
        assert_eq!(cycle_filter(Some("2".into()), &candidates, 1), None);
        assert_eq!(cycle_filter(None, &candidates, -1).as_deref(), Some("2"));
        // No candidates keeps the filter off.
        assert_eq!(cycle_filter(Some("x".into()), &[], 1), None);
    }

    #[test]
    fn mirror_filter_fields_appear_only_for_mirror() {
        let mut s = screen();
        assert!(!s.fields().contains(&Field::MirrorWorkspace));
        s.arrangement = Arrangement::Mirror;
        assert!(s.fields().contains(&Field::MirrorWorkspace));
        assert!(s.fields().contains(&Field::MirrorApp));
    }

    #[test]
    fn mirror_layout_enables_target_and_records_source() {
        let mut s = screen();
        s.arrangement = Arrangement::Mirror;
        s.mirror_from = Some("eDP-1".into());
        let layout = s.to_layout().unwrap();
        let dp9 = layout
            .outputs
            .iter()
            .find(|o| o.connector == "DP-9")
            .unwrap();
        assert_eq!(dp9.role, Role::MirrorOf("eDP-1".into()));
        // MirrorOf is not `off`, so it stays enabled in monitors.kdl.
        let kdl = crate::kdl::generate_monitors_kdl(&s.outputs, &layout);
        assert!(kdl.contains("output \"DP-9\""));
        assert!(!kdl.contains("output \"DP-9\" {\n    off"));
    }

    #[test]
    fn resolution_cycles_through_modes() {
        let mut s = screen();
        s.field = s
            .fields()
            .iter()
            .position(|f| *f == Field::Resolution)
            .unwrap();
        assert_eq!(s.mode_string().as_deref(), Some("2560x1080@60.000"));
        s.adjust(1);
        assert_eq!(s.mode_string().as_deref(), Some("1280x720@59.940"));
        s.adjust(1);
        assert_eq!(s.mode_string().as_deref(), Some("2560x1080@60.000"));
    }

    #[test]
    fn turning_off_disables_only_the_target() {
        let mut s = screen();
        s.arrangement = Arrangement::Off;
        let layout = s.to_layout().unwrap();
        let dp9 = layout
            .outputs
            .iter()
            .find(|o| o.connector == "DP-9")
            .unwrap();
        let edp = layout
            .outputs
            .iter()
            .find(|o| o.connector == "eDP-1")
            .unwrap();
        assert_eq!(dp9.role, Role::Off);
        assert_eq!(edp.role, Role::Normal);
    }

    #[test]
    fn preselects_the_matching_saved_layout() {
        let outputs = vec![
            output("eDP-1", 0, 1920, true),
            output("DP-9", 1920, 1080, true),
        ];
        let mut saved = capture_layout("auto mirror DP-9 of eDP-1", &outputs);
        let idx = saved
            .outputs
            .iter()
            .position(|o| o.connector == "DP-9")
            .unwrap();
        saved.outputs[idx].role = Role::MirrorOf("eDP-1".into());

        let s = Screen::new_preselecting(outputs, "DP-9", &[saved]).unwrap();
        assert_eq!(s.arrangement, Arrangement::Mirror);
        assert_eq!(s.mirror_from.as_deref(), Some("eDP-1"));
    }

    #[test]
    fn preselecting_without_a_match_keeps_the_default() {
        let outputs = vec![
            output("eDP-1", 0, 1920, true),
            output("DP-9", 1920, 1080, true),
        ];
        let s = Screen::new_preselecting(outputs, "DP-9", &[]).unwrap();
        assert_eq!(s.arrangement, Arrangement::ExtendRight);
    }

    #[test]
    fn manual_position_overrides_and_resets() {
        let mut s = screen();
        s.field = s
            .fields()
            .iter()
            .position(|f| *f == Field::Position)
            .unwrap();
        s.adjust(1); // nudge x by +width from auto (1920) -> 4480
        assert_eq!(s.effective_position(), (4480, 0));
        s.reset_position();
        assert_eq!(s.effective_position(), (1920, 0));
    }
}

//! Types mirroring the JSON shape of `niri msg --json outputs` (niri >= 26.04,
//! niri-ipc 26.x revision of the output structs).
//!
//! All fields default so the parser tolerates both missing fields and future
//! additions: niri's JSON is stable but grows new fields across releases, and
//! we would rather display NiriDM's best-effort view than crash.

use std::fmt;

use serde::Deserialize;

/// Deserialise a string, mapping JSON `null` to the empty string.
fn de_null_to_default<'de, D>(deserializer: D) -> std::result::Result<String, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<String>::deserialize(deserializer)?.unwrap_or_default())
}

/// A single connector currently known to niri.
#[derive(Debug, Clone, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Output {
    pub name: String,
    pub make: Option<String>,
    pub model: Option<String>,
    /// Serial number; niri sends `null` for outputs without one.
    #[serde(default, deserialize_with = "de_null_to_default")]
    pub serial: String,
    /// Physical size in millimetres (two element tuple: width, height).
    pub physical_size: (u32, u32),
    pub modes: Vec<Mode>,
    /// Index into `modes`, if the output is on.
    pub current_mode: Option<usize>,
    pub is_custom_mode: bool,
    pub vrr_supported: bool,
    pub vrr_enabled: bool,
    /// Whether this output currently has keyboard focus.
    pub is_focused: bool,
    pub logical: Option<LogicalOutput>,
}

impl Output {
    pub fn is_on(&self) -> bool {
        self.current_mode.is_some()
    }

    pub fn current_mode(&self) -> Option<&Mode> {
        self.current_mode.and_then(|i| self.modes.get(i))
    }

    /// Identity used when matching captured layouts.
    ///
    /// Prefers the strongest stable identifier (serial, then model, then
    /// make+model, then the volatile connector name).
    pub fn identity(&self) -> String {
        if !self.serial.is_empty() {
            return format!("serial {}", self.serial);
        }
        if let Some(model) = &self.model {
            if !model.is_empty() {
                return format!("model {model}");
            }
        }
        if let Some(make) = &self.make {
            if !make.is_empty() {
                return format!("make {make}");
            }
        }
        format!("name {}", self.name)
    }
}

/// A single video mode offered by an output.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default)]
#[derive(Default)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    /// Refresh rate in mHz (e.g. 143_912 for 143.912 Hz). niri calls this
    /// field `refresh_rate`; older snapshots used `refresh_rate_mhz`.
    #[serde(alias = "refresh_rate")]
    pub refresh_rate_mhz: u32,
}

impl fmt::Display for Mode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.refresh_rate_mhz == 0 {
            write!(f, "{}x{}", self.width, self.height)
        } else {
            write!(
                f,
                "{}x{}@{}",
                self.width,
                self.height,
                display_hz(self.refresh_rate_mhz)
            )
        }
    }
}

/// Current logical (post-scale/rotation) geometry of an enabled output.
#[derive(Debug, Clone, Copy, Deserialize, PartialEq)]
#[serde(default)]
pub struct LogicalOutput {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
    pub scale: f64,
    pub transform: Transform,
}

impl Default for LogicalOutput {
    fn default() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            scale: 1.0,
            transform: Transform::Normal,
        }
    }
}

/// Output rotation, matching niri's `transform` config values.
///
/// niri-ipc serialises `Transform` as a single JSON integer (smithay-style);
/// we accept both that and the descriptive strings so we never depend on the
/// exact representation chosen by a given niri release.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transform {
    Normal,
    Deg90,
    Deg180,
    Deg270,
    Flipped,
    Flipped90,
    Flipped180,
    Flipped270,
}

impl Transform {
    /// The value used inside niri's KDL config (`transform "90"` etc.).
    pub fn as_kdl_str(&self) -> &'static str {
        match self {
            Transform::Normal => "normal",
            Transform::Deg90 => "90",
            Transform::Deg180 => "180",
            Transform::Deg270 => "270",
            Transform::Flipped => "flipped",
            Transform::Flipped90 => "flipped-90",
            Transform::Flipped180 => "flipped-180",
            Transform::Flipped270 => "flipped-270",
        }
    }
}

impl<'de> Deserialize<'de> for Transform {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error;
        struct V;

        impl<'de> serde::de::Visitor<'de> for V {
            type Value = Transform;

            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("a transform as integer or string")
            }

            fn visit_u64<E: Error>(self, v: u64) -> std::result::Result<Transform, E> {
                match v {
                    0 => Ok(Transform::Normal),
                    90 => Ok(Transform::Deg90),
                    180 => Ok(Transform::Deg180),
                    270 => Ok(Transform::Deg270),
                    1 => Ok(Transform::Flipped),
                    91 => Ok(Transform::Flipped90),
                    181 => Ok(Transform::Flipped180),
                    271 => Ok(Transform::Flipped270),
                    _ => Err(E::custom(format!("unknown transform value {v}"))),
                }
            }

            fn visit_i64<E: Error>(self, v: i64) -> std::result::Result<Transform, E> {
                self.visit_u64(v as u64)
            }

            fn visit_str<E: Error>(self, v: &str) -> std::result::Result<Transform, E> {
                // niri serialises these as smithay `Transform` debug names
                // ("Normal", "Flipped90", ...), but config-style lowercase
                // forms ("flipped-90") also occur. Normalise both.
                let norm = v.to_ascii_lowercase().replace(['-', '_'], "");
                match norm.as_str() {
                    "normal" => Ok(Transform::Normal),
                    "90" => Ok(Transform::Deg90),
                    "180" => Ok(Transform::Deg180),
                    "270" => Ok(Transform::Deg270),
                    "flipped" => Ok(Transform::Flipped),
                    "flipped90" => Ok(Transform::Flipped90),
                    "flipped180" => Ok(Transform::Flipped180),
                    "flipped270" => Ok(Transform::Flipped270),
                    _ => Err(E::custom(format!("unknown transform value {v:?}"))),
                }
            }
        }

        deserializer.deserialize_any(V)
    }
}

/// Render a refresh rate in mHz down to three decimals ("143.912").
pub fn display_hz(refresh_rate_mhz: u32) -> String {
    let hz = refresh_rate_mhz / 1000;
    format!("{hz}.{:03}", refresh_rate_mhz % 1000)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn transform_accepts_integer_and_string() {
        let t: Transform = serde_json::from_value(json!(90)).unwrap();
        assert_eq!(t, Transform::Deg90);
        let t: Transform = serde_json::from_value(json!("180")).unwrap();
        assert_eq!(t, Transform::Deg180);
        let t: Transform = serde_json::from_value(json!("flipped-90")).unwrap();
        assert_eq!(t, Transform::Flipped90);
    }

    #[test]
    fn output_parses_with_defaults() {
        let o: Output = serde_json::from_value(json!({
            "name": "eDP-1",
            "serial": "abc",
            "current_mode": 0,
            "modes": [{ "width": 1920, "height": 1080, "refresh_rate_mhz": 143912 }]
        }))
        .unwrap();
        assert_eq!(o.make, None);
        assert!(!o.vrr_enabled);
        assert!(o.is_on());
        assert_eq!(o.current_mode().unwrap().to_string(), "1920x1080@143.912");
    }

    #[test]
    fn output_parses_real_niri_shape() {
        // Shape captured from niri 26.04: `refresh_rate` (not _mhz), null
        // serial, smithay-style capitalised transform.
        let o: Output = serde_json::from_value(json!({
            "name": "eDP-1",
            "make": "BOE",
            "model": "0x0B76",
            "serial": null,
            "physical_size": [340, 220],
            "modes": [{ "width": 1920, "height": 1200, "refresh_rate": 60002, "is_preferred": true }],
            "current_mode": 0,
            "is_custom_mode": false,
            "vrr_supported": true,
            "vrr_enabled": false,
            "logical": { "x": 0, "y": 0, "width": 1920, "height": 1200, "scale": 1.0, "transform": "Normal" }
        }))
        .unwrap();
        assert_eq!(o.serial, "");
        assert_eq!(o.current_mode().unwrap().to_string(), "1920x1200@60.002");
        assert_eq!(o.logical.unwrap().transform, Transform::Normal);
    }

    #[test]
    fn display_hz_pads_to_three_decimals() {
        assert_eq!(display_hz(60_000), "60.000");
        assert_eq!(display_hz(143_912), "143.912");
        assert_eq!(display_hz(143_900), "143.900");
    }
}

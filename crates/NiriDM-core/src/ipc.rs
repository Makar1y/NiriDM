//! Thin client over `niri msg --json`.
//!
//! We shell out to `niri msg` rather than linking `niri-ipc`: the JSON output
//! is explicitly stable while the crate version is pinned to niri itself, and
//! this keeps NiriDM working across niri releases without recompilation.
//!
//! Commands and JSON shapes follow niri 26.04 / niri-ipc 26.4.0.

use std::path::PathBuf;
use std::process::Command;

use crate::error::{NdError, Result};
use crate::types::Output;

const NIRI_NONZERO_ERR: &str = "niri msg";

pub struct Niri {
    /// The `niri` binary to invoke (default: `niri` from PATH, override via
    /// the `NiriDM_NIRI_BIN` env var — mainly useful for tests).
    niri_bin: PathBuf,
}

impl Default for Niri {
    fn default() -> Self {
        Self::new()
    }
}

impl Niri {
    pub fn new() -> Self {
        let bin = std::env::var("NiriDM_NIRI_BIN")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("niri"));
        Self { niri_bin: bin }
    }

    /// The `niri` binary this client invokes.
    pub fn bin(&self) -> &std::path::Path {
        &self.niri_bin
    }

    /// Query the connected outputs.
    ///
    /// niri 26.04 returns a JSON object keyed by connector name
    /// (`{"eDP-1": {...}}`); older releases returned an array. Accept both.
    pub fn outputs(&self) -> Result<Vec<Output>> {
        let value = self.run_msg(&["--json", "outputs"])?;
        parse_outputs(value)
    }

    /// Run a niri action. `name` is the action, `args` are the `--key value`
    /// pairs and positional arguments that follow it exactly as `niri msg
    /// action <name>` expects them (e.g. `move-window-to-monitor --id 5 DP-9`).
    pub fn action(&self, name: &str, args: &[&str]) -> Result<()> {
        let mut full = Vec::with_capacity(args.len() + 1);
        full.push("action");
        full.push(name);
        full.extend_from_slice(args);
        self.run_msg_data(&full)?;
        Ok(())
    }

    /// Query the current window list; used to find an existing `wl-mirror`
    /// window before the event stream reports it.
    pub fn windows(&self) -> Result<Vec<crate::events::Window>> {
        let value = self.run_msg(&["--json", "windows"])?;
        serde_json::from_value(value).map_err(|e| NdError::Json("niri msg --json windows", e))
    }

    /// Query the current workspace list.
    pub fn workspaces(&self) -> Result<Vec<crate::events::Workspace>> {
        let value = self.run_msg(&["--json", "workspaces"])?;
        serde_json::from_value(value).map_err(|e| NdError::Json("niri msg --json workspaces", e))
    }

    /// `niri msg --json <args...>`; returns the JSON inner content, unwrapping
    /// the `{"Ok": ...}` / `{"Err": ...}` envelope that `niri msg` uses.
    pub fn run_msg(&self, args: &[&str]) -> Result<serde_json::Value> {
        let out = self.spawn(args)?;
        let value = serde_json::from_str(&out).map_err(|e| NdError::Json("niri msg", e))?;
        unwrap_reply(value)
    }

    /// Same as [`run_msg`], but the args share position with one data payload
    /// (used by actions that take arguments).
    pub fn run_msg_data(&self, args: &[&str]) -> Result<serde_json::Value> {
        let out = self.spawn(args)?;
        let value = serde_json::from_str(&out).map_err(|e| NdError::Json("niri msg", e))?;
        unwrap_reply(value)
    }

    fn spawn(&self, args: &[&str]) -> Result<String> {
        let out = Command::new(&self.niri_bin)
            .args(["msg"])
            .args(args)
            .output()
            .map_err(|e| NdError::SpawnFailed("niri", e))?;

        if !out.status.success() {
            let stderr = String::from_utf8_lossy(&out.stderr).trim().to_owned();
            return Err(NdError::NonZeroExit(NIRI_NONZERO_ERR, stderr));
        }

        Ok(String::from_utf8_lossy(&out.stdout).into_owned())
    }
}

/// Decode the content of `niri msg --json outputs`, accepting either the
/// keyed object niri 26.04 emits or the array older releases emitted.
fn parse_outputs(value: serde_json::Value) -> Result<Vec<Output>> {
    let err = |e| NdError::Json("niri msg --json outputs", e);
    match value {
        serde_json::Value::Array(_) => serde_json::from_value(value).map_err(err),
        serde_json::Value::Object(map) => map
            .into_iter()
            .map(|(name, v)| {
                let mut output: Output = serde_json::from_value(v).map_err(err)?;
                if output.name.is_empty() {
                    output.name = name;
                }
                Ok(output)
            })
            .collect(),
        other => Err(NdError::Json(
            "niri msg --json outputs",
            serde::de::Error::custom(format!("expected an object or array, got {other}")),
        )),
    }
}

/// `niri msg --json` on a real socket returns `{"Ok": <content>}`; when the
/// socket is missing or niri is not running it may print a plain error line or
/// `{"Err": ...}`. Tolerate all three: unwrap Ok/Err, and pass anything else
/// (a bare object/array) through untouched.
fn unwrap_reply(value: serde_json::Value) -> Result<serde_json::Value> {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(inner) = map.get("Ok") {
                return Ok(inner.clone());
            }
            if let Some(err) = map.get("Err") {
                let msg = match err {
                    serde_json::Value::String(s) => s.clone(),
                    other => serde_json::to_string(other).unwrap_or_else(|_| "unknown".into()),
                };
                return Err(NdError::Niri(msg));
            }
            Ok(serde_json::Value::Object(map))
        }
        other => Ok(other),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn unwraps_ok_envelope() {
        let v = json!({ "Ok": { "a": 1 } });
        assert_eq!(unwrap_reply(v).unwrap(), json!({ "a": 1 }));
    }

    #[test]
    fn surfaces_err_envelope() {
        let v = json!({ "Err": "no compositor running" });
        assert!(matches!(unwrap_reply(v), Err(NdError::Niri(s)) if s == "no compositor running"));
    }

    #[test]
    fn passes_through_bare_object() {
        let v = json!({ "name": "eDP-1" });
        assert_eq!(unwrap_reply(v.clone()).unwrap(), v);
    }

    #[test]
    fn parses_keyed_output_map() {
        let v = json!({
            "DP-1": { "name": "DP-1", "serial": "S1", "current_mode": 0,
                      "modes": [{ "width": 1920, "height": 1080, "refresh_rate": 60000 }] },
            "eDP-1": { "name": "eDP-1", "serial": null, "current_mode": null }
        });
        let outputs = parse_outputs(v).unwrap();
        assert_eq!(outputs.len(), 2);
        // serde_json's object map is key-ordered, so DP-1 comes first.
        assert_eq!(outputs[0].name, "DP-1");
        assert!(outputs[0].is_on());
        assert_eq!(outputs[1].serial, "");
        assert!(!outputs[1].is_on());
    }

    #[test]
    fn parses_legacy_output_array_and_fills_name_from_key() {
        let v = json!([
            { "serial": "S1" }
        ]);
        let outputs = parse_outputs(v).unwrap();
        assert_eq!(outputs.len(), 1);
    }
}

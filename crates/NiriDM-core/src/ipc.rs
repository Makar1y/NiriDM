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

    /// Query the connected outputs.
    pub fn outputs(&self) -> Result<Vec<Output>> {
        let value = self.run_msg(&["--json", "outputs"])?;
        serde_json::from_value(value).map_err(|e| NdError::Json("niri msg --json outputs", e))
    }

    /// Run a niri action. Takes the action name and any `[key value]` pairs
    /// that follow it (matching `niri msg action`'s `--key value` style).
    pub fn action(&self, name: &str) -> Result<()> {
        self.run_msg_data(&["action", name])?;
        Ok(())
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
}

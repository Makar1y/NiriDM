//! Confirm that niri live-reloaded the generated configuration.
//!
//! niri watches every included file (niri >= 25.11) and emits
//! `ConfigLoaded { failed }` after each reload — but it also emits one
//! immediately on event-stream connect, reporting the *last* load attempt.
//! So the watch must be opened before the write; only the next
//! `ConfigLoaded` after it belongs to us.

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::error::{NdError, Result};
use crate::events::EventStream;
use crate::ipc::Niri;
use crate::paths::write_if_changed;

/// How long niri gets to reload after a write before we stop waiting.
const RELOAD_TIMEOUT: Duration = Duration::from_secs(5);

/// Watches for the reload caused by a write. Open it *before* writing.
pub struct ReloadWatch {
    stream: Option<EventStream>,
}

impl ReloadWatch {
    /// Open the event stream and consume the baseline `ConfigLoaded`.
    ///
    /// Verification is best effort: if the stream cannot be opened (very old
    /// niri, missing socket) the watch stays inert and writes proceed
    /// unverified rather than failing.
    pub fn open() -> Self {
        Self::open_with(Niri::new().bin().to_owned())
    }

    pub(crate) fn open_with(bin: PathBuf) -> Self {
        let mut stream = match EventStream::open_with(&bin) {
            Ok(stream) => stream,
            Err(e) => {
                tracing::warn!("no event stream; config reloads will not be verified: {e}");
                return Self { stream: None };
            }
        };
        match stream.wait_reload(RELOAD_TIMEOUT) {
            Ok(Some(failed)) => {
                if failed {
                    tracing::warn!("niri's current config is failing to load (before our write)");
                }
                Self {
                    stream: Some(stream),
                }
            }
            Ok(None) => {
                tracing::warn!("no ConfigLoaded on connect; skipping reload verification");
                Self { stream: None }
            }
            Err(e) => {
                tracing::warn!("event stream ended early; skipping reload verification: {e}");
                Self { stream: None }
            }
        }
    }

    /// After the write: wait for the next `ConfigLoaded`.
    ///
    /// `Ok(true)` — niri reloaded our write. `Ok(false)` — nothing happened
    /// within the deadline (the file may not be included anywhere). `Err` —
    /// niri tried and rejected the generated config.
    pub fn confirm(&mut self) -> Result<bool> {
        let Some(stream) = &mut self.stream else {
            return Ok(false);
        };
        match stream.wait_reload(RELOAD_TIMEOUT)? {
            Some(false) => {
                tracing::info!("niri reloaded the configuration");
                Ok(true)
            }
            Some(true) => Err(NdError::Niri(
                "config reload failed — niri rejected the generated monitors.kdl".to_owned(),
            )),
            None => {
                tracing::warn!(
                    "no config reload within {RELOAD_TIMEOUT:?} — is monitors.kdl included?"
                );
                Ok(false)
            }
        }
    }
}

/// Write `kdl` to `path` (no-op writes stay no-ops) and verify the reload.
///
/// When niri rejects the config, the previous file content is restored —
/// niri reloads again on the rollback — so a bad layout never leaves the
/// session with a rejected config on disk.
pub fn write_verified(path: &Path, kdl: &str) -> Result<bool> {
    write_verified_with(Niri::new().bin().to_owned(), path, kdl)
}

pub(crate) fn write_verified_with(bin: PathBuf, path: &Path, kdl: &str) -> Result<bool> {
    // Cheap pre-check: unchanged content must not pay for the event stream.
    if std::fs::read_to_string(path).is_ok_and(|current| current == kdl) {
        return Ok(false);
    }

    let previous = std::fs::read_to_string(path).ok();
    let mut watch = ReloadWatch::open_with(bin);
    let written = write_if_changed(path, kdl)?;
    if written {
        if let Err(e) = watch.confirm() {
            tracing::error!("rolling back {path:?}: {e}");
            let restored = match &previous {
                Some(previous) => write_if_changed(path, previous),
                None => std::fs::remove_file(path)
                    .map(|()| false)
                    .map_err(|e| NdError::Write {
                        path: path.to_owned(),
                        source: e,
                    }),
            };
            if let Err(e) = restored {
                tracing::error!("rollback failed: {e}");
            }
            return Err(e);
        }
    }
    Ok(written)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Private temp dir with `monitors.kdl` and a fake `niri` script.
    fn fixture(tag: &str, script_body: &str) -> (PathBuf, PathBuf, PathBuf) {
        let dir =
            std::env::temp_dir().join(format!("ndm-verify-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("fake-niri");
        std::fs::write(&bin, format!("#!/bin/sh\n{script_body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        let file = dir.join("monitors.kdl");
        (dir, file, bin)
    }

    const BASELINE: &str = r#"printf '{"ConfigLoaded":{"failed":false}}\n'"#;

    #[test]
    fn writes_and_confirms_the_reload() {
        let (dir, file, bin) = fixture(
            "confirm",
            &format!("{BASELINE}\nsleep 0.2\n{BASELINE}\nsleep 5"),
        );

        let written = write_verified_with(bin, &file, "output \"DP-1\" {}\n").unwrap();
        assert!(written);
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "output \"DP-1\" {}\n"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn unchanged_content_never_opens_the_stream() {
        let (dir, file, bin) = fixture(
            "noop",
            &format!("echo x >> \"$(dirname \"$0\")/spawned\"\n{BASELINE}\nsleep 5"),
        );

        std::fs::write(&file, "same\n").unwrap();
        let written = write_verified_with(bin, &file, "same\n").unwrap();
        assert!(!written);
        assert!(
            !dir.join("spawned").exists(),
            "event stream must not spawn for a no-op"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_failed_reload_rolls_the_file_back() {
        let (dir, file, bin) = fixture(
            "fail",
            &format!(
                "{BASELINE}\nsleep 0.2\nprintf '{{\"ConfigLoaded\":{{\"failed\":true}}}}\\n'\nsleep 5"
            ),
        );
        std::fs::write(&file, "previous content\n").unwrap();

        let err = write_verified_with(bin, &file, "broken content\n").unwrap_err();
        assert!(matches!(err, NdError::Niri(_)), "got: {err}");
        assert_eq!(
            std::fs::read_to_string(&file).unwrap(),
            "previous content\n",
            "rejected config must be rolled back"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_silent_niri_times_out_without_error() {
        let (dir, file, bin) = fixture("timeout", &format!("{BASELINE}\nsleep 5"));

        // File written, no confirmation, but not an error: the include may
        // simply not be active in this config.
        let written = write_verified_with(bin, &file, "content\n").unwrap();
        assert!(written);
        assert_eq!(std::fs::read_to_string(&file).unwrap(), "content\n");
        let _ = std::fs::remove_dir_all(dir);
    }
}

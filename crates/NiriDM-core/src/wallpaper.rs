//! Best-effort wallpaper sync for newly plugged outputs.
//!
//! Noctalia keeps wallpaper state per output. A connector it has never seen
//! can fall back to Noctalia's default wallpaper, which may differ from the
//! one currently shown on the existing outputs. After a hotplug settles, copy
//! the current output's wallpaper path to the new connector.
//!
//! Both Noctalia v5 (`noctalia msg ...`) and the legacy v4 shell
//! (`noctalia-shell ipc call ...`) are supported. Failure is never fatal: if
//! neither backend is available, NiriDM simply leaves the wallpaper alone.

use std::path::Path;
use std::process::{Command, Stdio};

use crate::error::Result;
use crate::types::Output;

const NOCTALIA_V5: &str = "noctalia";
const NOCTALIA_V4: &str = "noctalia-shell";

/// Copy the wallpaper shown on an existing output to every newly plugged
/// output in `outputs` (an output is "new" when its connector is not in
/// `previous`).
pub fn sync_new_outputs(outputs: &[Output], previous: &[String]) -> Result<()> {
    sync_new_outputs_with(
        Path::new(NOCTALIA_V5),
        Path::new(NOCTALIA_V4),
        outputs,
        previous,
    )
}

/// Testable form of [`sync_new_outputs`] with explicit backend binaries.
pub fn sync_new_outputs_with(
    v5: &Path,
    v4: &Path,
    outputs: &[Output],
    previous: &[String],
) -> Result<()> {
    let new: Vec<&Output> = outputs
        .iter()
        .filter(|o| o.is_on() && !previous.iter().any(|p| p == &o.name))
        .collect();
    if new.is_empty() {
        return Ok(());
    }

    let existing: Vec<&Output> = outputs
        .iter()
        .filter(|o| o.is_on() && !new.iter().any(|n| n.name == o.name))
        .collect();
    let source = existing
        .iter()
        .copied()
        .find(|o| o.is_focused)
        .or_else(|| existing.first().copied());

    if sync_v5(v5, source.map(|o| o.name.as_str()), &new) {
        return Ok(());
    }
    if sync_v4(v4, source.map(|o| o.name.as_str()), &new) {
        return Ok(());
    }

    tracing::debug!("no Noctalia wallpaper backend available; skipping wallpaper sync");
    Ok(())
}

fn sync_v5(bin: &Path, source: Option<&str>, new: &[&Output]) -> bool {
    let Some(path) = get_v5(bin, source) else {
        return false;
    };
    for output in new {
        if get_v5_output(bin, &output.name).as_deref() == Some(path.as_str()) {
            continue;
        }
        if !run_ok(bin, &["msg", "wallpaper-set", &output.name, &path]) {
            tracing::warn!(
                "could not set wallpaper for new output `{}` via Noctalia",
                output.name
            );
        }
    }
    true
}

fn get_v5(bin: &Path, output: Option<&str>) -> Option<String> {
    if let Some(output) = output {
        if let Some(path) = get_v5_output(bin, output) {
            return Some(path);
        }
    }
    run_stdout(bin, &["msg", "wallpaper-get"])
}

fn get_v5_output(bin: &Path, output: &str) -> Option<String> {
    run_stdout(bin, &["msg", "wallpaper-get", output])
}

fn sync_v4(bin: &Path, source: Option<&str>, new: &[&Output]) -> bool {
    let Some(path) = get_v4(bin, source) else {
        return false;
    };
    for output in new {
        if get_v4_output(bin, &output.name).as_deref() == Some(path.as_str()) {
            continue;
        }
        if !run_ok(
            bin,
            &["ipc", "call", "wallpaper", "set", &path, &output.name],
        ) {
            tracing::warn!(
                "could not set wallpaper for new output `{}` via Noctalia",
                output.name
            );
        }
    }
    true
}

fn get_v4(bin: &Path, output: Option<&str>) -> Option<String> {
    if let Some(output) = output {
        if let Some(path) = get_v4_output(bin, output) {
            return Some(path);
        }
    }
    run_stdout(bin, &["ipc", "call", "wallpaper", "get"])
}

fn get_v4_output(bin: &Path, output: &str) -> Option<String> {
    run_stdout(bin, &["ipc", "call", "wallpaper", "get", output])
}

/// Run `bin args...` and return trimmed stdout on success.
///
/// A missing binary or a non-zero exit means "this backend is not available";
/// that is expected while probing v5 and v4 in turn, so it is not logged as a
/// warning.
fn run_stdout(bin: &Path, args: &[&str]) -> Option<String> {
    let output = match Command::new(bin).args(args).stdin(Stdio::null()).output() {
        Ok(output) => output,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
        Err(e) => {
            tracing::debug!("failed to run {}: {e}", bin.display());
            return None;
        }
    };
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    (!text.is_empty()).then_some(text)
}

fn run_ok(bin: &Path, args: &[&str]) -> bool {
    match Command::new(bin)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
    {
        Ok(status) => status.success(),
        Err(e) => {
            tracing::debug!("failed to run {}: {e}", bin.display());
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{LogicalOutput, Mode, Transform};
    use std::path::PathBuf;

    fn output(name: &str, focused: bool) -> Output {
        Output {
            name: name.into(),
            current_mode: Some(0),
            modes: vec![Mode {
                width: 1920,
                height: 1080,
                refresh_rate_mhz: 60_000,
            }],
            logical: Some(LogicalOutput {
                x: 0,
                y: 0,
                width: 1920,
                height: 1080,
                scale: 1.0,
                transform: Transform::Normal,
            }),
            is_focused: focused,
            ..Default::default()
        }
    }

    fn script(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ndm-wallpaper-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn syncs_new_output_to_the_existing_outputs_wallpaper() {
        let dir = temp_dir("v5");
        let log = dir.join("log");
        let bin = script(
            &dir,
            "noctalia",
            &format!(
                r#"printf '%s\n' "$*" >> '{}'
case "$*" in
  "msg wallpaper-get eDP-1") echo /wall/current.jpg ;;
  "msg wallpaper-get HDMI-A-1") echo /wall/default.jpg ;;
  "msg wallpaper-get") echo /wall/current.jpg ;;
  "msg wallpaper-set HDMI-A-1 /wall/current.jpg") exit 0 ;;
  *) exit 1 ;;
esac"#,
                log.display()
            ),
        );

        let outputs = vec![output("eDP-1", true), output("HDMI-A-1", false)];
        sync_new_outputs_with(
            &bin,
            Path::new("/nonexistent/noctalia-shell"),
            &outputs,
            &["eDP-1".into()],
        )
        .unwrap();

        let log = std::fs::read_to_string(log).unwrap();
        assert!(log.contains("msg wallpaper-get eDP-1"), "{log}");
        assert!(
            log.contains("msg wallpaper-set HDMI-A-1 /wall/current.jpg"),
            "{log}"
        );
        assert!(
            !log.contains("wallpaper-set HDMI-A-1 /wall/default.jpg"),
            "{log}"
        );
    }

    #[test]
    fn sets_even_when_the_new_output_has_no_wallpaper_yet() {
        let dir = temp_dir("unknown-output");
        let log = dir.join("log");
        let bin = script(
            &dir,
            "noctalia",
            &format!(
                r#"printf '%s\n' "$*" >> '{}'
case "$*" in
  "msg wallpaper-get eDP-1") echo /wall/current.jpg ;;
  "msg wallpaper-get HDMI-A-1") exit 1 ;;
  "msg wallpaper-get") echo /wall/current.jpg ;;
  "msg wallpaper-set HDMI-A-1 /wall/current.jpg") exit 0 ;;
  *) exit 1 ;;
esac"#,
                log.display()
            ),
        );

        let outputs = vec![output("eDP-1", true), output("HDMI-A-1", false)];
        sync_new_outputs_with(
            &bin,
            Path::new("/nonexistent/noctalia-shell"),
            &outputs,
            &["eDP-1".into()],
        )
        .unwrap();

        let log = std::fs::read_to_string(log).unwrap();
        assert!(
            log.contains("msg wallpaper-set HDMI-A-1 /wall/current.jpg"),
            "{log}"
        );
    }

    #[test]
    fn does_nothing_without_a_new_output() {
        let dir = temp_dir("none");
        let log = dir.join("log");
        let bin = script(
            &dir,
            "noctalia",
            &format!("printf '%s\\n' \"$*\" >> '{}'", log.display()),
        );

        sync_new_outputs_with(
            &bin,
            Path::new("/nonexistent/noctalia-shell"),
            &[output("eDP-1", true)],
            &["eDP-1".into()],
        )
        .unwrap();
        assert!(!log.exists());
    }

    #[test]
    fn falls_back_to_noctalia_v4() {
        let dir = temp_dir("v4");
        let log = dir.join("log");
        let bin = script(
            &dir,
            "noctalia-shell",
            &format!(
                r#"printf '%s\n' "$*" >> '{}'
case "$*" in
  "ipc call wallpaper get eDP-1") echo /wall/current.jpg ;;
  "ipc call wallpaper get HDMI-A-1") echo /wall/default.jpg ;;
  "ipc call wallpaper get") echo /wall/current.jpg ;;
  "ipc call wallpaper set /wall/current.jpg HDMI-A-1") exit 0 ;;
  *) exit 1 ;;
esac"#,
                log.display()
            ),
        );

        let outputs = vec![output("eDP-1", true), output("HDMI-A-1", false)];
        sync_new_outputs_with(
            Path::new("/nonexistent/noctalia"),
            &bin,
            &outputs,
            &["eDP-1".into()],
        )
        .unwrap();

        let log = std::fs::read_to_string(log).unwrap();
        assert!(
            log.contains("ipc call wallpaper set /wall/current.jpg HDMI-A-1"),
            "{log}"
        );
    }
}

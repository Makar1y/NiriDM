//! The launcher chain: finding a popup provider and driving it.
//!
//! Providers come in two families:
//!
//! * **dmenu-style** (`fuzzel`, `wofi`, `bemenu`, `dmenu`): items on stdin,
//!   prompt via `-p`, selection on stdout.
//! * **GTK dialogs** (`zenity`, `yad`): items as command-line arguments,
//!   selection on stdout.
//!
//! Detection and invocation are deliberately tool-agnostic — no
//! provider-specific flags beyond the prompt — so the chain keeps working
//! across launcher versions. The timeout is enforced by us (kill the child)
//! rather than by launcher options, because not every launcher has one.

use std::ffi::OsStr;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::error::{NdError, Result};

/// What the popup menu looks like.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Menu {
    /// Header/prompt, e.g. `DP-9 · AOC 24G2 · 1920x1080@60.000`.
    pub prompt: String,
    /// One label per line; the picked label comes back verbatim.
    pub items: Vec<String>,
}

/// How a given launcher is driven.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Style {
    /// stdin items, `-p <prompt>`, selection on stdout.
    Dmenu,
    /// `--list --title … --column Action --print-column 1 <items>`.
    Zenity,
    /// `--list --title … --column Action --print-column=1 <items>`.
    Yad,
}

impl Style {
    pub fn for_binary(name: &str) -> Style {
        // Callers may pass a full path; match on the file name.
        let base = std::path::Path::new(name)
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| name.to_owned());
        match base.as_str() {
            "zenity" => Style::Zenity,
            "yad" => Style::Yad,
            _ => Style::Dmenu,
        }
    }

    fn args(&self, menu: &Menu) -> Vec<std::ffi::OsString> {
        use std::ffi::OsString as O;
        let prompt = menu.prompt.as_str();
        match self {
            Style::Dmenu => vec![O::from("-p"), O::from(prompt)],
            Style::Zenity => {
                let mut a: Vec<O> = [
                    "--list",
                    "--title",
                    prompt,
                    "--column",
                    "Action",
                    "--print-column",
                    "1",
                ]
                .iter()
                .map(O::from)
                .collect();
                a.extend(menu.items.iter().map(O::from));
                a
            }
            Style::Yad => {
                let mut a: Vec<O> = [
                    "--list",
                    "--title",
                    prompt,
                    "--column",
                    "Action",
                    "--print-column=1",
                ]
                .iter()
                .map(O::from)
                .collect();
                a.extend(menu.items.iter().map(O::from));
                a
            }
        }
    }
}

/// First provider of `order` that is an executable file somewhere in `path`
/// (a `PATH`-style value). Providers are checked in preference order, not
/// directory order.
/// Find the first provider in preference order.
///
/// An entry containing a `/` is a path and is checked directly; anything else
/// is looked up in `path` (normally `$PATH`).
pub fn detect(order: &[String], path: Option<&OsStr>) -> Option<PathBuf> {
    for provider in order {
        if provider.contains('/') {
            let candidate = PathBuf::from(provider);
            if is_executable(&candidate) {
                return Some(candidate);
            }
            continue;
        }
        let Some(path) = path else { continue };
        for dir in std::env::split_paths(path) {
            let candidate = dir.join(provider);
            if is_executable(&candidate) {
                return Some(candidate);
            }
        }
    }
    None
}

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::PermissionsExt;
    std::fs::metadata(path)
        .map(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path)
        .map(|m| m.is_file())
        .unwrap_or(false)
}

/// Show `menu` through `bin`, giving up after `timeout`.
///
/// Returns the picked item, or `Ok(None)` when the user dismissed the popup,
/// the launcher failed, or the timeout elapsed. A spawn failure is an error.
pub fn prompt(bin: &Path, menu: &Menu, timeout: Duration) -> Result<Option<String>> {
    let style = Style::for_binary(&binary_name(bin));
    let mut child =
        spawn_launcher(bin, &style, menu).map_err(|e| NdError::SpawnFailed("popup launcher", e))?;

    if let Some(mut stdin) = child.stdin.take() {
        let _ = write!(stdin, "{}", menu.items.join("\n"));
        // Dropping stdin sends EOF: dmenu-style launchers only open their
        // window once the item list is complete.
    }

    let Some(status) = wait_or_kill(&mut child, timeout)? else {
        tracing::warn!("popup timed out after {timeout:?}");
        return Ok(None);
    };

    let mut out = String::new();
    if let Some(mut stdout) = child.stdout.take() {
        use std::io::Read;
        let _ = stdout.read_to_string(&mut out);
    }
    if !status.success() {
        // Esc / Cancel / window closed.
        return Ok(None);
    }
    let choice = out.trim_end_matches('\n');
    Ok((!choice.is_empty()).then(|| choice.to_owned()))
}

/// Spawn the launcher, retrying if the binary is briefly not runnable.
///
/// Tests write small launcher scripts and exec them immediately; under
/// parallel load the exec can hit `ETXTBSY` ("text file busy") while another
/// thread still holds the file open for writing. The same can happen in real
/// use when a provider is being replaced. Both cases are transient.
fn spawn_launcher(bin: &Path, style: &Style, menu: &Menu) -> std::io::Result<std::process::Child> {
    let mut attempt = 0;
    loop {
        match Command::new(bin)
            .args(style.args(menu))
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
        {
            Ok(child) => return Ok(child),
            Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy && attempt < 10 => {
                attempt += 1;
                std::thread::sleep(Duration::from_millis(10 * attempt));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Wait for `child`, killing it if it outlives `timeout`.
///
/// `Ok(Some(status))` when it exited in time, `Ok(None)` when it had to be
/// killed. Used by the launcher prompt and by the daemon's popup command, so
/// a wedged popup can never stall its caller.
pub fn wait_or_kill(
    child: &mut std::process::Child,
    timeout: Duration,
) -> Result<Option<std::process::ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return Ok(Some(status)),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(25));
            }
            Err(e) => return Err(NdError::SpawnFailed("waiting for child process", e)),
        }
    }
}

fn binary_name(path: &Path) -> String {
    path.file_name()
        .unwrap_or_else(|| OsStr::new(""))
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    /// A scratch directory with one executable script per name.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir()
                .join(format!("ndm-launcher-test-{tag}-{}", std::process::id()));
            let _ = std::fs::remove_dir_all(&dir);
            std::fs::create_dir_all(&dir).unwrap();
            Self(dir)
        }

        fn script(&self, name: &str, body: &str) -> PathBuf {
            use std::os::unix::fs::PermissionsExt;
            let path = self.0.join(name);
            std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
            path
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn path_of(dir: &Path) -> std::ffi::OsString {
        let mut p = dir.as_os_str().to_owned();
        p.push(":");
        p.push("/usr/bin:/bin");
        p
    }

    #[test]
    fn picks_the_first_available_provider_in_preference_order() {
        let s = Scratch::new("detect");
        s.script("wofi", "exit 0");
        s.script("fuzzel", "exit 0");
        let path = path_of(&s.0);

        // fuzzel is preferred and present.
        let order = vec!["fuzzel".to_owned(), "wofi".to_owned()];
        assert_eq!(detect(&order, Some(&path)).unwrap(), s.0.join("fuzzel"));

        // Without fuzzel present in the order, wofi wins.
        let order = vec!["dmenu".to_owned(), "wofi".to_owned()];
        assert_eq!(detect(&order, Some(&path)).unwrap(), s.0.join("wofi"));

        // Nothing available.
        assert_eq!(detect(&["zenity".to_owned()], Some(&path)), None);
        // No PATH at all.
        assert_eq!(detect(&order, None), None);
    }

    #[test]
    fn non_executables_are_ignored() {
        let s = Scratch::new("nonexec");
        std::fs::write(s.0.join("fuzzel"), "#!/bin/sh\nexit 0").unwrap(); // not +x
        let path = path_of(&s.0);
        assert_eq!(detect(&["fuzzel".to_owned()], Some(&path)), None);
    }

    #[test]
    fn entries_with_a_slash_are_used_as_paths() {
        let s = Scratch::new("slashpath");
        let bin = s.script("my-launcher", "exit 0");
        let path = path_of(&s.0);

        // Absolute paths are checked directly, never looked up in PATH.
        let order = vec![bin.display().to_string(), "fuzzel".to_owned()];
        assert_eq!(detect(&order, Some(&path)).unwrap(), bin);

        // A missing path entry is skipped like a missing binary.
        assert_eq!(detect(&["/no/such/launcher".to_owned()], Some(&path)), None);
        assert_eq!(detect(&["/no/such/launcher".to_owned()], None), None);
    }

    #[test]
    fn returns_the_selected_item() {
        let s = Scratch::new("select");
        // Ignores stdin, prints a choice — what a launcher does on Enter.
        let bin = s.script("fuzzel", "echo 'Extend left'");
        let menu = Menu {
            prompt: "DP-9".into(),
            items: vec!["Extend right".into(), "Extend left".into()],
        };
        assert_eq!(
            prompt(&bin, &menu, Duration::from_secs(5)).unwrap(),
            Some("Extend left".to_owned())
        );
    }

    #[test]
    fn dismissal_and_failure_yield_none() {
        let s = Scratch::new("dismiss");
        let menu = Menu {
            prompt: "p".into(),
            items: vec!["a".into()],
        };

        let esc = s.script("esc", "exit 1"); // Esc / cancel
        assert_eq!(prompt(&esc, &menu, Duration::from_secs(5)).unwrap(), None);

        let empty = s.script("empty", "cat >/dev/null; exit 0"); // no selection
        assert_eq!(prompt(&empty, &menu, Duration::from_secs(5)).unwrap(), None);
    }

    #[test]
    fn the_menu_is_fed_to_the_launcher_on_stdin() {
        let s = Scratch::new("stdin");
        // Echo the first line it receives, so we know the menu arrived.
        let bin = s.script("reader", "head -n 1");
        let menu = Menu {
            prompt: "p".into(),
            items: vec!["Extend right".into(), "Extend left".into()],
        };
        assert_eq!(
            prompt(&bin, &menu, Duration::from_secs(5)).unwrap(),
            Some("Extend right".to_owned())
        );
    }

    #[test]
    fn a_hung_launcher_is_killed_at_the_deadline() {
        let s = Scratch::new("hang");
        let bin = s.script("slow", "sleep 30");
        let menu = Menu {
            prompt: "p".into(),
            items: vec!["a".into()],
        };

        let started = Instant::now();
        let got = prompt(&bin, &menu, Duration::from_millis(300)).unwrap();
        let elapsed = started.elapsed();

        assert_eq!(got, None);
        assert!(
            elapsed < Duration::from_secs(3),
            "kill should not wait for the child to finish (took {elapsed:?})"
        );
    }

    #[test]
    fn styles_produce_expected_arguments() {
        let menu = Menu {
            prompt: "DP-9 · AOC 24G2".into(),
            items: vec!["Extend right".into()],
        };
        let expected: Vec<std::ffi::OsString> = vec!["-p".into(), "DP-9 · AOC 24G2".into()];
        assert_eq!(Style::Dmenu.args(&menu), expected);
        assert_eq!(Style::for_binary("/usr/bin/zenity"), Style::Zenity);
        assert_eq!(Style::for_binary("yad"), Style::Yad);
        assert_eq!(Style::for_binary("fuzzel"), Style::Dmenu);
        assert!(Style::Yad
            .args(&menu)
            .iter()
            .any(|a| a.to_str() == Some("--print-column=1")));
    }
}

//! Client for `niri msg --json event-stream`.
//!
//! niri has no output-change event, so the daemon still polls; the stream is
//! used where it shines: confirming config reloads (`ConfigLoaded`) and
//! catching new windows (the `wl-mirror` window) the moment they appear.
//!
//! Events are JSON objects, one per line, with no `Ok` envelope. niri
//! guarantees JSON stability only in the additive sense, so unknown variants
//! and fields must not be fatal — see [`Event::Other`].

use std::io::{BufRead, BufReader};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::error::{NdError, Result};
use crate::ipc::Niri;

/// A compositor event. Only the variants NiriDM cares about are named; every
/// other variant (there are many, and new ones keep arriving) collapses into
/// [`Event::Other`] instead of failing the parse.
#[derive(Debug, Clone)]
pub enum Event {
    ConfigLoaded {
        failed: bool,
    },
    WindowsChanged {
        windows: Vec<Window>,
    },
    WindowOpenedOrChanged {
        window: Window,
    },
    WindowClosed {
        id: u64,
    },
    /// Focus moved to another window (`id` is `None` when focus was cleared).
    WindowFocusChanged {
        id: Option<u64>,
    },
    /// A workspace became active; `focused` distinguishes the focused output's
    /// workspace from ones that merely became visible.
    WorkspaceActivated {
        focused: bool,
    },
    WorkspacesChanged {
        workspaces: Vec<Workspace>,
    },
    /// Any variant this build does not know about (forward compatibility).
    Other,
}

/// Wire shape of an event: every known variant as an optional field keyed by
/// its name, unknown keys ignored. No variant present → [`Event::Other`].
///
/// This is what lets a newer niri stream through an older NiriDM: serde's
/// `#[serde(other)]` cannot swallow data-carrying variants in an externally
/// tagged enum, but an all-`Option` struct handles them for free.
#[derive(Deserialize)]
struct RawEvent {
    #[serde(rename = "ConfigLoaded")]
    config_loaded: Option<ConfigLoadedBody>,
    #[serde(rename = "WindowsChanged")]
    windows_changed: Option<WindowsChangedBody>,
    #[serde(rename = "WindowOpenedOrChanged")]
    window_opened_or_changed: Option<WindowOpenedOrChangedBody>,
    #[serde(rename = "WindowClosed")]
    window_closed: Option<WindowClosedBody>,
    #[serde(rename = "WindowFocusChanged")]
    window_focus_changed: Option<WindowFocusChangedBody>,
    #[serde(rename = "WorkspaceActivated")]
    workspace_activated: Option<WorkspaceActivatedBody>,
    #[serde(rename = "WorkspacesChanged")]
    workspaces_changed: Option<WorkspacesChangedBody>,
}

#[derive(Deserialize)]
struct ConfigLoadedBody {
    failed: bool,
}
#[derive(Deserialize)]
struct WindowsChangedBody {
    windows: Vec<Window>,
}
#[derive(Deserialize)]
struct WindowOpenedOrChangedBody {
    window: Window,
}
#[derive(Deserialize)]
struct WindowClosedBody {
    id: u64,
}
#[derive(Deserialize)]
struct WindowFocusChangedBody {
    #[serde(default)]
    id: Option<u64>,
}
#[derive(Deserialize)]
struct WorkspaceActivatedBody {
    #[serde(default)]
    focused: bool,
}
#[derive(Deserialize)]
struct WorkspacesChangedBody {
    workspaces: Vec<Workspace>,
}

impl From<RawEvent> for Event {
    fn from(raw: RawEvent) -> Self {
        if let Some(b) = raw.config_loaded {
            Event::ConfigLoaded { failed: b.failed }
        } else if let Some(b) = raw.windows_changed {
            Event::WindowsChanged { windows: b.windows }
        } else if let Some(b) = raw.window_opened_or_changed {
            Event::WindowOpenedOrChanged { window: b.window }
        } else if let Some(b) = raw.window_closed {
            Event::WindowClosed { id: b.id }
        } else if let Some(b) = raw.window_focus_changed {
            Event::WindowFocusChanged { id: b.id }
        } else if let Some(b) = raw.workspace_activated {
            Event::WorkspaceActivated { focused: b.focused }
        } else if let Some(b) = raw.workspaces_changed {
            Event::WorkspacesChanged {
                workspaces: b.workspaces,
            }
        } else {
            Event::Other
        }
    }
}

impl<'de> Deserialize<'de> for Event {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        RawEvent::deserialize(deserializer).map(Into::into)
    }
}

/// The subset of niri's window fields NiriDM needs. Unknown fields are
/// ignored; optional ones default so a shrunken payload still parses.
#[derive(Debug, Clone, Deserialize)]
pub struct Window {
    pub id: u64,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub app_id: Option<String>,
    #[serde(default)]
    pub pid: Option<u32>,
    #[serde(default)]
    pub workspace_id: Option<u64>,
    #[serde(default)]
    pub is_focused: bool,
}

impl Window {
    /// Whether this window looks like the given tool. A known pid is the
    /// strongest signal (we spawned it ourselves); app id / title are the
    /// fallback for windows we did not spawn.
    pub fn matches_tool(&self, tool: &str, pid: Option<u32>) -> bool {
        if let (Some(pid), Some(my)) = (pid, self.pid) {
            if pid == my {
                return true;
            }
        }
        let app = self.app_id.as_deref().unwrap_or_default();
        let title = self.title.as_deref().unwrap_or_default();
        app.eq_ignore_ascii_case(tool) || title.to_lowercase().contains(&tool.to_lowercase())
    }
}

/// The subset of niri's workspace fields used for output routing.
#[derive(Debug, Clone, Deserialize)]
pub struct Workspace {
    pub id: u64,
    #[serde(default)]
    pub idx: i64,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub output: Option<String>,
    /// Whether this workspace is the one currently visible on its output.
    #[serde(default)]
    pub is_active: bool,
    /// Whether this workspace is the single focused one across all outputs.
    #[serde(default)]
    pub is_focused: bool,
    /// ID of the window currently active on this workspace, if any.
    #[serde(default)]
    pub active_window_id: Option<u64>,
}

/// A live `niri msg --json event-stream` connection.
///
/// Lines are read on a background thread so callers can impose their own
/// deadlines; dropping the stream kills the child.
pub struct EventStream {
    child: Child,
    rx: Receiver<String>,
}

impl EventStream {
    /// Connect using the default `niri` binary (see [`Niri::new`]).
    pub fn open() -> Result<Self> {
        Self::open_with(Niri::new().bin())
    }

    /// Connect using a specific `niri` binary (mainly for tests).
    pub fn open_with(bin: &Path) -> Result<Self> {
        // The test helpers write their fake `niri` script immediately before
        // connecting; under parallel load the exec can hit ETXTBSY so retry
        // transiently, same as `launcher::prompt`.
        let mut child = None;
        let mut attempt = 0;
        while attempt < 10 {
            match Command::new(bin)
                .args(["msg", "--json", "event-stream"])
                .stdout(Stdio::piped())
                .stderr(Stdio::null())
                .spawn()
            {
                Ok(c) => {
                    child = Some(c);
                    break;
                }
                Err(e) if e.kind() == std::io::ErrorKind::ExecutableFileBusy => {
                    attempt += 1;
                    std::thread::sleep(Duration::from_millis(10 * attempt));
                }
                Err(e) => return Err(NdError::SpawnFailed("niri msg event-stream", e)),
            }
        }
        let mut child = child.ok_or_else(|| {
            NdError::SpawnFailed("niri msg event-stream", std::io::Error::other("ETXTBSY"))
        })?;

        let stdout = child.stdout.take().expect("stdout was piped");
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            for line in BufReader::new(stdout)
                .lines()
                .map_while(std::result::Result::ok)
            {
                if tx.send(line).is_err() {
                    break;
                }
            }
        });

        Ok(Self { child, rx })
    }

    /// The next event, or `None` when `timeout` elapses. Unparsable lines are
    /// logged and skipped rather than killing the stream.
    pub fn next(&mut self, timeout: Duration) -> Result<Option<Event>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            match self.rx.recv_timeout(remaining) {
                Ok(line) => match serde_json::from_str::<Event>(&line) {
                    Ok(event) => return Ok(Some(event)),
                    Err(e) => tracing::debug!("ignoring unparsable event: {e}"),
                },
                Err(RecvTimeoutError::Timeout) => return Ok(None),
                Err(RecvTimeoutError::Disconnected) => {
                    return Err(NdError::Other("niri event stream closed".to_owned()))
                }
            }
        }
    }

    /// Wait for the next `ConfigLoaded` event. `Ok(None)` = timed out.
    ///
    /// On connect, niri immediately sends its *last* config load attempt; only
    /// a `ConfigLoaded` seen after a config write is a reaction to that write,
    /// so callers must consume the baseline first (see [`crate::verify`]).
    pub fn wait_reload(&mut self, timeout: Duration) -> Result<Option<bool>> {
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Ok(None);
            }
            match self.next(remaining)? {
                Some(Event::ConfigLoaded { failed }) => return Ok(Some(failed)),
                Some(_) => continue,
                None => return Ok(None),
            }
        }
    }
}

impl Drop for EventStream {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    /// Write an executable shell script into a private temp directory.
    fn script(tag: &str, body: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("ndm-events-test-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("fake-niri");
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        path
    }

    #[test]
    fn parses_known_events() {
        let e: Event = serde_json::from_str(r#"{"ConfigLoaded":{"failed":true}}"#).unwrap();
        assert!(matches!(e, Event::ConfigLoaded { failed: true }));

        let line = r#"{"WindowOpenedOrChanged":{"window":{"id":7,"title":"wl-mirror",
            "app_id":"wl-mirror","pid":1234,"workspace_id":1,"is_focused":false,
            "is_floating":false,"is_urgent":false,"layout":{"tile_size":[1,1]}}}}"#;
        let e: Event = serde_json::from_str(line).unwrap();
        match e {
            Event::WindowOpenedOrChanged { window } => {
                assert_eq!(window.id, 7);
                assert_eq!(window.pid, Some(1234));
                assert_eq!(window.app_id.as_deref(), Some("wl-mirror"));
            }
            other => panic!("wrong variant: {other:?}"),
        }
    }

    #[test]
    fn unknown_variants_and_fields_are_not_fatal() {
        // Unknown variant: collapses into Other.
        let e: Event = serde_json::from_str(r#"{"CastStartedOrChanged":{"cast":{"stream_id":1}}}"#)
            .expect("unknown variant must parse");
        assert!(matches!(e, Event::Other));

        // Even a bare envelope-looking object parses as Other.
        let e: Event = serde_json::from_str(r#"{"Ok":null}"#).unwrap();
        assert!(matches!(e, Event::Other));

        // Unknown fields inside a known variant are ignored.
        let e: Event =
            serde_json::from_str(r#"{"WindowClosed":{"id":3,"future_field":{"deep":[1,2]}}}"#)
                .unwrap();
        assert!(matches!(e, Event::WindowClosed { id: 3 }));
    }

    #[test]
    fn stream_delivers_events_and_times_out() {
        let bin = script(
            "deliver",
            r#"printf '{"ConfigLoaded":{"failed":false}}\n'
sleep 0.1
printf '{"WindowClosed":{"id":9}}\n'
sleep 5"#,
        );
        let mut s = EventStream::open_with(&bin).unwrap();
        let e = s.next(Duration::from_secs(5)).unwrap().unwrap();
        assert!(matches!(e, Event::ConfigLoaded { failed: false }));
        let e = s.next(Duration::from_secs(5)).unwrap().unwrap();
        assert!(matches!(e, Event::WindowClosed { id: 9 }));
        assert!(s.next(Duration::from_millis(150)).unwrap().is_none());
    }

    #[test]
    fn wait_reload_distinguishes_baseline_from_silence() {
        let bin = script(
            "reload",
            r#"printf '{"WorkspacesChanged":{"workspaces":[]}}\n'
printf '{"ConfigLoaded":{"failed":false}}\n'
sleep 5"#,
        );
        let mut s = EventStream::open_with(&bin).unwrap();
        // Baseline arrives even though the "reload" never happens.
        assert_eq!(s.wait_reload(Duration::from_secs(5)).unwrap(), Some(false));
        // Nothing follows within the deadline.
        assert_eq!(s.wait_reload(Duration::from_millis(150)).unwrap(), None);
    }

    #[test]
    fn matches_tool_prefers_pid_then_app_id_then_title() {
        let w = Window {
            id: 1,
            title: Some("Some Window".into()),
            app_id: Some("wl-mirror".into()),
            pid: Some(42),
            workspace_id: None,
            is_focused: false,
        };
        assert!(w.matches_tool("wl-mirror", Some(42)));
        // Our pid, but different tool name: pid wins (we spawned it).
        assert!(w.matches_tool("anything", Some(42)));
        // No pid: app id matches case-insensitively.
        assert!(w.matches_tool("WL-Mirror", None));
        // No pid, no app id: title fallback.
        let w = Window {
            app_id: None,
            pid: None,
            title: Some("wl-mirror — eDP-1".into()),
            ..w
        };
        assert!(w.matches_tool("wl-mirror", None));
        assert!(!w.matches_tool("foot", None));
    }
}

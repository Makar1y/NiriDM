//! Hotplug detection: describing the connected set and deciding when a
//! change has actually settled.
//!
//! niri has no `OutputsChanged` event, so the daemon polls. A single poll is
//! not trustworthy: monitors enumerate in stages and the very first query
//! after a dock event can be partial. The [`Debounce`] window makes a change
//! "real" only once the fingerprint holds steady for a configurable settle
//! period.

use std::time::{Duration, Instant};

use crate::types::Output;

/// A stable description of what is connected and how it is currently running.
///
/// Two polls with the same fingerprint mean "nothing changed". Connector
/// order is normalised (niri does not guarantee it) and the fingerprint covers
/// both presence and state, so an output going off or switching mode counts as
/// a change too.
pub fn fingerprint(outputs: &[Output]) -> String {
    let mut parts: Vec<String> = outputs
        .iter()
        .map(|o| {
            let mode = o
                .current_mode()
                .map(|m| m.to_string())
                .unwrap_or_else(|| "off".to_owned());
            let logical = match o.logical {
                Some(l) => format!("{}x{}+{}+{}@{}", l.width, l.height, l.x, l.y, l.scale),
                None => "-".to_owned(),
            };
            format!("{}|{mode}|{logical}", o.name)
        })
        .collect();
    parts.sort();
    parts.join("\n")
}

/// Debounces a changing fingerprint: a difference must hold steady for a
/// settle window before it is reported once.
#[derive(Debug)]
pub struct Debounce {
    settle: Duration,
    /// Fingerprint last reported (i.e. applied).
    settled: Option<String>,
    /// Change waiting out the settle window: `(fingerprint, seen_at)`.
    pending: Option<(String, Instant)>,
}

impl Debounce {
    pub fn new(settle: Duration) -> Self {
        Self {
            settle,
            settled: None,
            pending: None,
        }
    }

    /// Report the fingerprint observed at `now`.
    ///
    /// Returns the fingerprint exactly once, after it has differed from the
    /// last reported state for the whole settle window. Observing the
    /// fingerprint returning to the reported state cancels a pending change.
    pub fn poll(&mut self, current: &str, now: Instant) -> Option<String> {
        if self.settled.as_deref() == Some(current) {
            // Back to what was applied; anything pending is obsolete.
            self.pending = None;
            return None;
        }

        match &self.pending {
            // Same change as before: check whether it has settled.
            Some((seen, since)) if seen == current => {
                if now.duration_since(*since) < self.settle {
                    return None;
                }
                let fp = current.to_owned();
                self.settled = Some(fp.clone());
                self.pending = None;
                Some(fp)
            }
            // New change (or first one): (re)start the window.
            _ => {
                self.pending = Some((current.to_owned(), now));
                None
            }
        }
    }

    /// The fingerprint last reported through [`poll`].
    pub fn settled(&self) -> Option<&str> {
        self.settled.as_deref()
    }

    /// Skip the settle window and report `current` immediately (used by
    /// `ndm-daemon --once` and by the CLI after a manual apply).
    pub fn settle_now(&mut self, current: &str) -> Option<String> {
        if self.settled.as_deref() == Some(current) {
            return None;
        }
        let fp = current.to_owned();
        self.settled = Some(fp.clone());
        self.pending = None;
        Some(fp)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A shared, arbitrary timeline so tests can use `at(ms)` offsets.
    fn t0() -> Instant {
        static T0: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        *T0.get_or_init(Instant::now)
    }

    fn at(ms: u64) -> Instant {
        t0() + Duration::from_millis(ms)
    }

    #[test]
    fn first_change_waits_out_the_settle_window() {
        let mut d = Debounce::new(Duration::from_millis(500));
        assert_eq!(d.poll("a", at(0)), None);
        assert_eq!(d.poll("a", at(499)), None);
        assert_eq!(d.poll("a", at(500)), Some("a".into()));
        // Reported once only.
        assert_eq!(d.poll("a", at(900)), None);
    }

    #[test]
    fn a_flapping_fingerprint_restarts_the_window() {
        let mut d = Debounce::new(Duration::from_millis(500));
        assert_eq!(d.poll("a", at(0)), None);
        assert_eq!(d.poll("b", at(400)), None); // new change: window restarts
        assert_eq!(d.poll("b", at(899)), None); // 499ms since "b"
        assert_eq!(d.poll("b", at(900)), Some("b".into()));
    }

    #[test]
    fn returning_to_the_applied_state_cancels_the_pending_change() {
        let mut d = Debounce::new(Duration::from_millis(500));
        d.poll("a", at(0));
        assert_eq!(d.poll("a", at(500)), Some("a".into()));

        d.poll("b", at(600)); // unplug starts
        assert_eq!(d.poll("a", at(700)), None); // replug: back to applied state
        assert_eq!(d.poll("a", at(2000)), None); // never fires for "b" or "a"
        assert_eq!(d.settled(), Some("a"));
    }

    #[test]
    fn settle_now_skips_the_window() {
        let mut d = Debounce::new(Duration::from_secs(30));
        assert_eq!(d.settle_now("a"), Some("a".into()));
        assert_eq!(d.settle_now("a"), None);
        assert_eq!(d.poll("a", at(0)), None);
    }

    #[test]
    fn fingerprint_is_order_independent_and_state_aware() {
        let a = Output {
            name: "DP-1".into(),
            current_mode: Some(0),
            modes: vec![crate::types::Mode {
                width: 3840,
                height: 2160,
                refresh_rate_mhz: 143_912,
            }],
            ..Default::default()
        };
        let b = Output {
            name: "eDP-1".into(),
            current_mode: Some(0),
            modes: vec![crate::types::Mode {
                width: 1920,
                height: 1080,
                refresh_rate_mhz: 60_000,
            }],
            ..Default::default()
        };

        assert_eq!(
            fingerprint(&[a.clone(), b.clone()]),
            fingerprint(&[b.clone(), a.clone()])
        );

        // Losing the output is a change.
        assert_ne!(
            fingerprint(std::slice::from_ref(&a)),
            fingerprint(&[a.clone(), b])
        );

        // Turning it off is a change.
        let mut off = a.clone();
        off.current_mode = None;
        assert_ne!(fingerprint(&[a]), fingerprint(&[off]));
    }
}

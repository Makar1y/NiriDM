#!/usr/bin/env bash
# Milestone 4b smoke test: ndm mirror window steering via the event stream.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4b.XXXXXX")
CFG=$H/NiriDM
ACTIONS=$H/actions.log

chmod +x "$F/fake-wlmirror"
mkdir -p "$H"; touch "$H/main.kdl"

export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export NDM_MOCK_ACTION_LOG=$ACTIONS

echo "== init + capture =="
"$NDM" init >/dev/null
"$NDM" capture docked >/dev/null

echo
echo "== ndm mirror DP-1 eDP-1 (fake wl-mirror exits 0 after 3s) =="
export NDM_MOCK_WINDOWS=mirror
# A window change for the same mirror window arrives after NDM has already
# steered it. Steering must stay one-shot: `fullscreen-window` toggles, so a
# second call would take the mirror back out of fullscreen.
cat >"$H/events" <<'EOF'
sleep 0.2
event {"WindowOpenedOrChanged":{"window":{"id":42,"title":"wl-mirror — DP-1","app_id":"wl-mirror","pid":9999,"workspace_id":1,"is_focused":false}}}
EOF
export NDM_MOCK_EVENTS=$H/events
"$NDM" mirror DP-1 eDP-1 --wl-mirror "$F/fake-wlmirror" 2>"$H/mirror.txt"
echo "stderr:"
cat "$H/mirror.txt"
grep -q "mirroring DP-1 -> eDP-1" "$H/mirror.txt" && echo "OK: source/target reported"
grep -q "wl-mirror exited; mirroring stopped" "$H/mirror.txt" && echo "OK: clean wl-mirror exit ends the command"
echo "--- actions issued:"
cat "$ACTIONS"
test "$(grep -c 'move-window-to-monitor' "$ACTIONS")" -eq 1 && echo "OK: mirror window moved to eDP-1 once"
grep -q 'move-window-to-monitor --id 42 eDP-1' "$ACTIONS" && echo "OK: move-window-to-monitor called with the wl-mirror window id"
test "$(grep -c 'fullscreen-window' "$ACTIONS")" -eq 1 && echo "OK: mirror window fullscreened once (no toggle-off on later events)"
grep -q 'fullscreen-window --id 42' "$ACTIONS" && echo "OK: fullscreen-window called with the wl-mirror window id"

echo
echo "== default dst = first other output (no focused output in mock) =="
rm -f "$ACTIONS"
"$NDM" mirror DP-1 --wl-mirror "$F/fake-wlmirror" 2>"$H/default.txt" &
pid=$!
sleep 4
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
grep -q "mirroring DP-1 -> eDP-1" "$H/default.txt" && echo "OK: default target is the other output"
grep -q 'move-window-to-monitor --id 42 eDP-1' "$ACTIONS" && echo "OK: steering still happened"

echo
echo "== self-mirror is rejected before anything runs =="
rm -f "$ACTIONS"
if "$NDM" mirror eDP-1 eDP-1 --wl-mirror "$F/fake-wlmirror" 2>"$H/self.txt"; then
  echo "FAIL: self-mirror should be rejected"; exit 1
fi
grep -q "cannot mirror an output onto itself" "$H/self.txt" && echo "OK: self-mirror rejected"
test ! -s "$ACTIONS" && echo "OK: no actions issued"

echo
echo "== --hide-bar / --hide-notification run their commands and restore on SIGTERM =="
cat >"$CFG/config.kdl" <<EOF
config {
    mirror-hide-bar-cmd "echo hide-bar >> '$H/hide.log'"
    mirror-show-bar-cmd "echo show-bar >> '$H/hide.log'"
    mirror-hide-notification-cmd "echo hide-notif >> '$H/hide.log'"
    mirror-show-notification-cmd "echo show-notif >> '$H/hide.log'"
}
EOF
rm -f "$H/hide.log"
"$NDM" mirror DP-1 eDP-1 --hide-bar --hide-notification \
  --wl-mirror "$F/fake-wlmirror" 2>"$H/hide.txt" &
pid=$!
sleep 1
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
echo "--- hide/show commands:"
cat "$H/hide.log" 2>/dev/null || echo "(none)"
grep -qx hide-bar "$H/hide.log" && echo "OK: bar hide command ran"
grep -qx hide-notif "$H/hide.log" && echo "OK: notification hide command ran"
grep -qx show-bar "$H/hide.log" && echo "OK: bar restored when the mirror was stopped"
grep -qx show-notif "$H/hide.log" && echo "OK: notifications restored when the mirror was stopped"

echo
echo "ALL SMOKE CHECKS PASSED"

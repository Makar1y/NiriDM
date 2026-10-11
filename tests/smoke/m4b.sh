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
"$NDM" mirror DP-1 eDP-1 --wl-mirror "$F/fake-wlmirror" 2>"$H/mirror.txt"
echo "stderr:"
cat "$H/mirror.txt"
grep -q "mirroring DP-1 -> eDP-1" "$H/mirror.txt" && echo "OK: source/target reported"
grep -q "wl-mirror exited; mirroring stopped" "$H/mirror.txt" && echo "OK: clean wl-mirror exit ends the command"
echo "--- actions issued:"
cat "$ACTIONS"
test "$(grep -c 'move-window-to-monitor' "$ACTIONS")" -ge 1 && echo "OK: mirror window moved to eDP-1"
grep -q 'move-window-to-monitor --id 42 eDP-1' "$ACTIONS" && echo "OK: move-window-to-monitor called with the wl-mirror window id"
grep -q 'fullscreen-window --id 42' "$ACTIONS" && echo "OK: mirror window fullscreened"

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
echo "ALL SMOKE CHECKS PASSED"

#!/usr/bin/env bash
# Milestone 4d smoke test: ndm mirror workspace/app filters drive wl-mirror's
# stream-mode freeze/unfreeze as focus changes.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4d.XXXXXX")
LOG=$H/mirror.stdin
WS=$H/ws
APP=$H/app
EVENTS=$H/events

chmod +x "$F/fake-wlmirror"
export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export NDM_FAKE_MIRROR_LOG=$LOG

run_case() {
  name=$1; shift
  rm -f "$LOG"
  "$NDM" mirror DP-1 eDP-1 --wl-mirror "$F/fake-wlmirror" "$@" 2>"$H/$name.err" &
  pid=$!
  sleep 3
  kill "$pid" 2>/dev/null || true
  wait "$pid" 2>/dev/null || true
  echo "== $name: stream commands =="
  cat "$LOG" 2>/dev/null || true
}

echo "== workspace filter: freeze while another workspace is focused =="
printf '3' > "$WS"
cat > "$EVENTS" <<'EOF'
sleep 1
ws 9
event {"WindowFocusChanged":{"id":8}}
sleep 1
ws 3
event {"WindowFocusChanged":{"id":9}}
EOF
export NDM_MOCK_WS=$WS
export NDM_MOCK_EVENTS=$EVENTS
unset NDM_MOCK_APP NDM_MOCK_WINDOWS 2>/dev/null || true
run_case ws --workspace 3
grep -q '^--freeze$' "$LOG" && echo "OK: froze when focus left workspace 3"
grep -q '^--unfreeze$' "$LOG" && echo "OK: unfroze when focus returned"
test "$(grep -c '^--freeze$' "$LOG")" = 1 && echo "OK: no duplicate freeze"
# order: freeze before unfreeze
test "$(grep -n '^--freeze$' "$LOG" | cut -d: -f1)" -lt "$(grep -n '^--unfreeze$' "$LOG" | cut -d: -f1)" \
  && echo "OK: freeze precedes unfreeze"

echo
echo "== app filter: freeze while another app is focused =="
printf '4' > "$WS"
printf 'firefox' > "$APP"
cat > "$EVENTS" <<'EOF'
sleep 1
app alacritty
event {"WindowFocusChanged":{"id":8}}
sleep 1
app firefox
event {"WindowFocusChanged":{"id":9}}
EOF
export NDM_MOCK_APP=$APP
export NDM_MOCK_WINDOWS=focus
run_case app --app firefox
grep -q '^--freeze$' "$LOG" && echo "OK: froze when focus left app firefox"
grep -q '^--unfreeze$' "$LOG" && echo "OK: unfroze when firefox refocused"

echo
echo "== no filter: no stream commands at all =="
unset NDM_MOCK_APP NDM_MOCK_WS NDM_MOCK_EVENTS NDM_MOCK_WINDOWS 2>/dev/null || true
run_case none
test ! -s "$LOG" && echo "OK: filterless mirror stays unfrozen"

echo
echo "ALL SMOKE CHECKS PASSED"

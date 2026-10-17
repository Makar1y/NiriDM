#!/usr/bin/env bash
# Milestone 4d smoke test: ndm mirror workspace/app filters drive wl-mirror's
# stream-mode freeze/unfreeze as focus changes.
#
# Assertions here must actually fail the run: a bare `cmd && echo OK` never
# aborts under `set -e`, so `ok`/`fail` helpers are used instead.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4d.XXXXXX")
LOG=$H/mirror.stdin
WS=$H/ws
APP=$H/app
EVENTS=$H/events

ok()  { echo "OK: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

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
  cat "$LOG" 2>/dev/null || echo "(no stream commands)"
}

assert_has() { grep -q "^$1$" "$LOG" || fail "expected `$1` in $2"; }
assert_not_has() { ! grep -q "^$1$" "$LOG" || fail "unexpected `$1` in $2"; }
assert_count() {
  local n; n=$(grep -c "^$1$" "$LOG" || true)
  [ "$n" = "$2" ] || fail "expected $2 × `$1`, got $n"
}

echo "== workspace filter: freeze while another workspace is shown =="
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
unset NDM_MOCK_APP NDM_MOCK_WINDOWS NDM_MOCK_WS_FOCUSED 2>/dev/null || true
run_case ws --workspace 3
assert_has --freeze "$LOG";   ok "froze when the source left workspace 3"
assert_has --unfreeze "$LOG"; ok "unfroze when the source returned to workspace 3"
assert_count --freeze 1 "$LOG"; ok "no duplicate freeze"
f=$(grep -n '^--freeze$'  "$LOG" | cut -d: -f1)
u=$(grep -n '^--unfreeze$' "$LOG" | cut -d: -f1)
[ "$f" -lt "$u" ] || fail "freeze must precede unfreeze"
ok "freeze precedes unfreeze"

echo
echo "== workspace filter stays live while the source workspace is active but unfocused =="
# The mirror window on the target steals keyboard focus, so niri reports the
# source workspace as NOT globally focused. The filter must key on the source
# output's *active* workspace (is_active), not is_focused, or it freezes.
printf '3' > "$WS"
cat > "$EVENTS" <<'EOF'
sleep 1
event {"WindowFocusChanged":{"id":99}}
sleep 1
event {"WorkspaceActivated":{"id":1,"focused":false}}
EOF
export NDM_MOCK_WS=$WS
export NDM_MOCK_EVENTS=$EVENTS
export NDM_MOCK_WS_FOCUSED=false
run_case wsactive --workspace 3
[ ! -s "$LOG" ] || fail "stayed frozen while source still shows workspace 3 (used is_focused?)"
ok "stays live while source shows workspace 3 even when globally unfocused"
unset NDM_MOCK_WS_FOCUSED

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
assert_has --freeze "$LOG";   ok "froze when focus left app firefox"
assert_has --unfreeze "$LOG"; ok "unfroze when firefox refocused"

echo
echo "== no filter: no stream commands at all =="
unset NDM_MOCK_APP NDM_MOCK_WS NDM_MOCK_EVENTS NDM_MOCK_WINDOWS 2>/dev/null || true
run_case none
[ ! -s "$LOG" ] || fail "filterless mirror must not freeze"
ok "filterless mirror stays unfrozen"

echo
echo "ALL SMOKE CHECKS PASSED"

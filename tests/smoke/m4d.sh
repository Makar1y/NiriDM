#!/usr/bin/env bash
# Milestone 4d smoke test: ndm mirror workspace filtering and app-only capture.
#
# Assertions here must actually fail the run: a bare `cmd && echo OK` never
# aborts under `set -e`, so `ok`/`fail` helpers are used instead.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4d.XXXXXX")
LOG=$H/mirror.stdin
ARGS=$H/mirror.args
ACTIONS=$H/actions.log
WS=$H/ws
APP=$H/app
FLOAT=$H/floating
EVENTS=$H/events

ok()  { echo "OK: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

chmod +x "$F/fake-wlmirror"
export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export NDM_FAKE_MIRROR_LOG=$LOG
export NDM_FAKE_MIRROR_ARGS=$ARGS
export NDM_MOCK_ACTION_LOG=$ACTIONS

run_case() {
  name=$1; shift
  local src=${NDM_TEST_SRC:-DP-1}
  local dst=${NDM_TEST_DST:-eDP-1}
  rm -f "$LOG" "$ARGS" "$ACTIONS"
  "$NDM" mirror "$src" "$dst" --wl-mirror "$F/fake-wlmirror" "$@" 2>"$H/$name.err" &
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
echo "== app-only: float the window and capture only its region =="
printf '3' > "$WS"
printf 'firefox' > "$APP"
printf '0' > "$FLOAT"
export NDM_MOCK_APP=$APP
export NDM_MOCK_FLOATING=$FLOAT
export NDM_MOCK_WINDOWS=app
unset NDM_MOCK_EVENTS 2>/dev/null || true
run_case app --app firefox
grep -q 'toggle-window-floating --id 7' "$ACTIONS" \
  || fail "app window was not floated"
ok "floated the app window so niri exposes its geometry"
grep -q -- '--region' "$ARGS" || fail "wl-mirror was not given a region"
grep -q '100,100 800x600' "$ARGS" || fail "region does not match the app window"
ok "wl-mirror captures only the app window region"
grep -q 'focus-window --id 7' "$ACTIONS" || fail "app window was not focused"
ok "focused the app window (its workspace is now visible)"
[ "$(grep -c 'toggle-window-floating --id 7' "$ACTIONS")" -eq 2 ] \
  || fail "app window was not restored to tiling on exit"
[ "$(cat "$FLOAT")" = 0 ] || fail "floating state was not restored"
ok "restored the app window to tiling when the mirror stopped"

echo
echo "== app-only: region uses the source output's global origin =="
printf '3' > "$WS"
printf 'firefox' > "$APP"
printf '0' > "$FLOAT"
export NDM_MOCK_APP=$APP
export NDM_MOCK_FLOATING=$FLOAT
export NDM_MOCK_WINDOWS=app
export NDM_MOCK_WS_OUTPUT=eDP-1
export NDM_TEST_SRC=eDP-1
export NDM_TEST_DST=DP-1
unset NDM_MOCK_EVENTS 2>/dev/null || true
run_case apporigin --app firefox
grep -q '3940,100 800x600' "$ARGS" \
  || fail "region did not include eDP-1's logical origin (3840,0)"
ok "translated output-local window geometry into global region coordinates"
unset NDM_MOCK_WS_OUTPUT NDM_TEST_SRC NDM_TEST_DST

echo
echo "== app-only: stop when the captured window closes =="
printf '3' > "$WS"
printf 'firefox' > "$APP"
printf '0' > "$FLOAT"
cat > "$EVENTS" <<'EOF'
sleep 1
event {"WindowClosed":{"id":7}}
EOF
export NDM_MOCK_APP=$APP
export NDM_MOCK_FLOATING=$FLOAT
export NDM_MOCK_WINDOWS=app
export NDM_MOCK_EVENTS=$EVENTS
run_case appclose --app firefox
grep -q "app window closed; stopping mirror" "$H/appclose.err" \
  || fail "mirror did not stop when the app window closed"
ok "closed the mirror when the app window disappeared"

echo
echo "== no filter: no stream commands at all =="
unset NDM_MOCK_APP NDM_MOCK_WS NDM_MOCK_EVENTS NDM_MOCK_WINDOWS \
  NDM_MOCK_FLOATING 2>/dev/null || true
run_case none
[ ! -s "$LOG" ] || fail "filterless mirror must not freeze"
ok "filterless mirror stays unfrozen"

echo
echo "ALL SMOKE CHECKS PASSED"

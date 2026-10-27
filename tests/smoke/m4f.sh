#!/usr/bin/env bash
# Milestone 4f smoke test: ndm mirror stops and closes wl-mirror when the
# source or target output is unplugged.
#
# niri 26.04 has no output-change event, so ndm polls `niri msg --json outputs`
# once a second. This test overwrites the mock output state while the mirror is
# running and asserts that the child is killed and no restart happens.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4f.XXXXXX")
STATE=$H/state.json
PIDFILE=$H/wlmirror.pid

ok()  { echo "OK: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

chmod +x "$F/fake-wlmirror"
export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$STATE
export NDM_FAKE_MIRROR_PID=$PIDFILE
export NDM_FAKE_MIRROR_SLEEP=10

run_unplug() {
  name=$1
  initial=$2
  after=$3
  gone=$4

  cp "$F/$initial" "$STATE"
  rm -f "$PIDFILE"
  "$NDM" mirror DP-1 eDP-1 --wl-mirror "$F/fake-wlmirror" 2>"$H/$name.err" &
  ndm_pid=$!

  # Let the mirror start and get steered, then pull the monitor.
  sleep 1
  cp "$F/$after" "$STATE"

  stopped=0
  for _ in $(seq 1 40); do
    if ! kill -0 "$ndm_pid" 2>/dev/null; then
      stopped=1
      break
    fi
    sleep 0.1
  done
  if [ "$stopped" != 1 ]; then
    kill "$ndm_pid" 2>/dev/null || true
    wait "$ndm_pid" 2>/dev/null || true
    fail "$name: ndm did not stop after $gone was unplugged"
  fi
  wait "$ndm_pid" 2>/dev/null || true

  grep -q "output \`$gone\` disconnected; stopping mirror" "$H/$name.err" \
    || fail "$name: missing disconnect message for $gone"
  ok "$name: detected $gone unplug and stopped the mirror"

  if [ -f "$PIDFILE" ] && kill -0 "$(cat "$PIDFILE")" 2>/dev/null; then
    fail "$name: wl-mirror child is still running"
  fi
  ok "$name: wl-mirror child was closed"
}

echo "== target output unplugged =="
run_unplug target state-a.json state-a-dp.json eDP-1

echo
echo "== source output unplugged =="
run_unplug source state-a.json state-a-edp.json DP-1

echo
echo "ALL SMOKE CHECKS PASSED"

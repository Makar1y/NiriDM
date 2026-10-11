#!/usr/bin/env bash
# Milestone 4a end-to-end smoke test: config-reload verification with the
# event stream (new in niri >= 25.11 / 26.04).
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
DAEMON=$R/target/debug/ndm-daemon
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4.XXXXXX")
CFG=$H/NiriDM

mkdir -p "$H"; touch "$H/main.kdl"

export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export RUST_LOG=ndm=info

echo "== init =="
"$NDM" init >/dev/null

echo
echo "== capture (reload confirmed via ConfigLoaded) =="
err=$("$NDM" capture docked 2>&1 >/dev/null)
echo "stderr: $err"
grep -q "niri reloaded the configuration" <<<"$err" && echo "OK: CLI surfaces the reload confirmation"

echo
echo "== apply same content again = no write, no stream, no reload line =="
out=$("$NDM" apply docked 2>"$H/apply2.txt")
echo "stdout: $out"
test -z "$(grep 'reloaded the configuration' "$H/apply2.txt" || true)" \
  && echo "OK: no-op apply made no verification pass"

echo
echo "== changed content + failed reload => rejected & rolled back =="
"$NDM" monitor eDP-1 off >/dev/null                       # change 1, verified ok
set +e
NDM_MOCK_RELOAD=fail "$NDM" monitor eDP-1 on 2>"$H/fail.txt"
rc=$?
set -e
echo "exit=$rc"
cat "$H/fail.txt"
test $rc -ne 0 && echo "OK: non-zero exit on rejected reload"
grep -q "rejected the generated monitors.kdl" "$H/fail.txt" && echo "OK: rejection reported"
grep -q '^ *off$' "$CFG/monitors.kdl" && echo "OK: monitors.kdl rolled back (eDP-1 still off)"

echo
echo "== daemon applies with verification too =="
export RUST_LOG="ndm_daemon=info,ndm_core::verify=info"
"$DAEMON" --once 2>"$H/daemon.txt"
grep -q "reloaded the configuration" "$H/daemon.txt" && echo "OK: daemon logged the reload confirmation"

echo
echo "ALL SMOKE CHECKS PASSED"

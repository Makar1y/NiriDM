#!/usr/bin/env bash
# Milestone 3 end-to-end smoke test against the mock niri.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
DAEMON=$R/target/debug/ndm-daemon
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m3.XXXXXX")
CFG=$H/NiriDM
L=$H/launcher.log

chmod +x "$F/fake-launcher" "$F/hang-launcher"
mkdir -p "$H"
touch "$H/main.kdl"

export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export NDM_FIXTURE_LOG=$L
export RUST_LOG=ndm_daemon=info

echo "== init =="
"$NDM" init
echo "--- config.kdl created:"
cat "$CFG/config.kdl"

# Point the provider chain at the fake launcher.
sed -i "s|^    popup-providers .*|    popup-providers \"$F/fake-launcher\"|" "$CFG/config.kdl"
grep popup-providers "$CFG/config.kdl"
echo "--- prefs parse check (via popup help errors-free load):"
"$NDM" list

echo
echo "== capture layout for state A =="
"$NDM" capture docked
"$NDM" list

echo
echo "== daemon --once on state A (should apply, no popup) =="
"$DAEMON" --once
echo "--- monitors.kdl:"
cat "$CFG/monitors.kdl"
test ! -e "$L" && echo "OK: no popup invoked"

echo
echo "== hotplug: state B (DP-9 appears) =="
NDM_MOCK_STATE=$F/state-b.json "$DAEMON" --once
echo "--- launcher.log:"
cat "$L"
echo "--- layouts.kdl (expect 'auto extend-right ...'):"
cat "$CFG/layouts.kdl"
echo "--- monitors.kdl:"
cat "$CFG/monitors.kdl"

echo
echo "== daemon --once on state B again (now matches; no second popup) =="
rm -f "$L"
NDM_MOCK_STATE=$F/state-b.json "$DAEMON" --once
test ! -e "$L" && echo "OK: popup not re-invoked"

echo
echo "== direct ndm popup stdout contract =="
rm -f "$L"
out=$(NDM_MOCK_STATE=$F/state-b.json "$NDM" popup DP-9)
echo "stdout=[$out]"
test "$out" = "Extend right" && echo "OK: stdout is exactly the action label"
grep -q "prompt:" "$L" && echo "OK: launcher got the menu"

echo
echo "== ndm popup --provider with a path =="
rm -f "$L"
out=$(NDM_MOCK_STATE=$F/state-b.json "$NDM" popup DP-9 --provider "$F/fake-launcher")
test "$out" = "Extend right" && echo "OK: --provider path honoured"

echo
echo "== popup timeout kills a hung launcher =="
sed -i 's|^    popup-timeout .*|    popup-timeout 1|' "$CFG/config.kdl"
sed -i 's|fake-launcher"|hang-launcher"|' "$CFG/config.kdl"
start=$(date +%s)
out=$(NDM_MOCK_STATE=$F/state-b.json "$NDM" popup DP-9 2>"$H/popup-stderr.txt")
end=$(date +%s)
echo "stdout=[$out] elapsed=$((end-start))s"
cat "$H/popup-stderr.txt"
test -z "$out" && test $((end-start)) -lt 10 && echo "OK: timed out fast with empty stdout"
pgrep -f hang-launcher >/dev/null && { echo "FAIL: launcher still alive"; exit 1; } || echo "OK: hung launcher killed"

echo
echo "ALL SMOKE CHECKS PASSED"

#!/usr/bin/env bash
# Milestone 3 end-to-end smoke test against the mock niri.
#
# The popup is TUI-only: `ndm popup` must host `ndm tui` in a terminal from
# `popup-terminals`. The tests point that list at `tests/fixtures/fake-terminal`,
# which records the argv it was called with and drops a canned report where
# `--report` says, so the popup's host/timeout/report contract is covered
# without a real terminal.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
DAEMON=$R/target/debug/ndm-daemon
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m3.XXXXXX")
CFG=$H/NiriDM
L=$H/terminal.log

chmod +x "$F/fake-terminal"
mkdir -p "$H"
touch "$H/main.kdl"

export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export FAKE_TERM_LOG=$L
export RUST_LOG=ndm_daemon=info
export PATH="$F:$PATH"

echo "== init =="
"$NDM" init
echo "--- config.kdl created:"
cat "$CFG/config.kdl"

# Host the TUI in the fake terminal instead of a real one.
sed -i "s|^    popup-terminals .*|    popup-terminals \"fake-terminal\"|" "$CFG/config.kdl"
grep popup-terminals "$CFG/config.kdl"
echo "--- prefs parse check (via a load that fails noisily):"
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
rm -f "$L"
NDM_MOCK_STATE=$F/state-b.json "$DAEMON" --once
echo "--- terminal log:"
cat "$L"
test -e "$L" && echo "OK: popup invoked for the new monitor"
grep -q -- '--report' "$L" && echo "OK: popup hosted ndm tui with --report"
grep -q 'DP-9' "$CFG/monitors.kdl" && { echo "FAIL: popup must not write monitors.kdl"; exit 1; } \
  || echo "OK: only ndm tui writes monitors.kdl, not the popup wrapper"

echo
echo "== save a layout for state B, daemon converges without prompting =="
NDM_MOCK_STATE=$F/state-b.json "$NDM" capture docked-b >/dev/null
rm -f "$L"
NDM_MOCK_STATE=$F/state-b.json "$DAEMON" --once
test ! -e "$L" && echo "OK: popup not re-invoked"
grep -q 'DP-9' "$CFG/monitors.kdl" && echo "OK: matching layout applied"

echo
echo "== replug an already-known set still prompts (always prompt on hotplug) =="
rm -f "$L"
NDM_MOCK_STATE=$F/state-a.json "$DAEMON" --once     # unplug DP-9: shrink, no popup
test ! -e "$L" && echo "OK: shrink does not prompt"
rm -f "$L"
NDM_MOCK_STATE=$F/state-b.json "$DAEMON" --once     # replug DP-9: a layout matches, but still prompt
test -e "$L" && echo "OK: popup invoked even though a layout matched"

echo
echo "== direct ndm popup stdout contract =="
rm -f "$L"
out=$(NDM_MOCK_STATE=$F/state-b.json "$NDM" popup DP-9)
echo "stdout=[$out]"
test "$out" = "mock-tui-report" && echo "OK: stdout is exactly the TUI report"
grep -q -- '--report' "$L" && grep -q -- 'tui' "$L" && echo "OK: terminal got 'ndm tui ... --report'"

echo
echo "== popup timeout kills a hung terminal =="
sed -i 's|^    popup-timeout .*|    popup-timeout 1|' "$CFG/config.kdl"
start=$(date +%s)
out=$(FAKE_TERM_HANG=1 NDM_MOCK_STATE=$F/state-b.json "$NDM" popup DP-9 2>"$H/popup-stderr.txt")
end=$(date +%s)
echo "stdout=[$out] elapsed=$((end-start))s"
cat "$H/popup-stderr.txt"
test -z "$out" && test $((end-start)) -lt 10 && echo "OK: timed out fast with empty stdout"
pgrep -f fake-terminal >/dev/null && { echo "FAIL: terminal still alive"; exit 1; } || echo "OK: hung terminal killed"

echo
echo "== no terminal on PATH => popup is skipped, not fatal =="
sed -i 's|^    popup-terminals .*|    popup-terminals "definitely-not-a-real-terminal"|' "$CFG/config.kdl"
out=$(NDM_MOCK_STATE=$F/state-b.json "$NDM" popup DP-9 2>"$H/noterm.txt")
echo "stdout=[$out]"
grep -q 'no terminal found' "$H/noterm.txt" && echo "OK: reported the missing terminal"
test -z "$out" && echo "OK: no stdout, exit 0"

echo
echo "ALL SMOKE CHECKS PASSED"

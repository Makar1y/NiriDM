#!/usr/bin/env bash
# Milestone 4e smoke test: sync a newly plugged output to the current wallpaper.
#
# Uses a fake `noctalia` so no real session is touched. The daemon must copy
# the existing output's wallpaper path to the new connector, and the config
# switch must be able to disable that behaviour.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
DAEMON=$R/target/debug/ndm-daemon
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4e.XXXXXX")
CFG=$H/NiriDM
BIN=$H/bin
LOG=$H/noctalia.log
STATE=$H/dp1.wallpaper

ok()  { echo "OK: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

mkdir -p "$CFG" "$BIN"
touch "$H/main.kdl"

cat >"$CFG/config.kdl" <<'EOF'
config {
    sync-wallpaper true
}
EOF

cat >"$BIN/noctalia" <<EOF
#!/bin/sh
printf '%s\n' "\$*" >> "$LOG"
case "\$*" in
  "msg wallpaper-get eDP-1") echo /wall/current.jpg ;;
  "msg wallpaper-get DP-1")
    if [ -f "$STATE" ]; then cat "$STATE"; else echo /wall/default.jpg; fi ;;
  "msg wallpaper-get") echo /wall/current.jpg ;;
  "msg wallpaper-set DP-1 /wall/current.jpg") echo /wall/current.jpg > "$STATE"; exit 0 ;;
  *) exit 1 ;;
esac
EOF
chmod +x "$BIN/noctalia"

export PATH="$BIN:$F:$PATH"
export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json

# Pretend the daemon already knew about eDP-1; DP-1 is the new output.
printf 'eDP-1\n' > "$CFG/last-set"

echo "== hotplug syncs the new output to the current wallpaper =="
rm -f "$LOG"
"$DAEMON" --once --no-popup
echo "--- noctalia calls:"
cat "$LOG" 2>/dev/null || echo "(none)"
grep -q '^msg wallpaper-get eDP-1$' "$LOG" || fail "did not read the existing output's wallpaper"
grep -q '^msg wallpaper-set DP-1 /wall/current.jpg$' "$LOG" \
  || fail "did not set the new output to the current wallpaper"
[ "$(grep -c '^msg wallpaper-set DP-1 /wall/current.jpg$' "$LOG")" -eq 1 ] \
  || fail "set the same wallpaper more than once"
ok "copied eDP-1's wallpaper to DP-1 (once)"

echo
echo "== sync-wallpaper false disables the sync =="
sed -i 's/^    sync-wallpaper true$/    sync-wallpaper false/' "$CFG/config.kdl"
printf 'eDP-1\n' > "$CFG/last-set"
rm -f "$LOG"
"$DAEMON" --once --no-popup
[ ! -s "$LOG" ] || fail "noctalia was called even though sync-wallpaper is false"
ok "left the wallpaper alone when disabled"

echo
echo "ALL SMOKE CHECKS PASSED"

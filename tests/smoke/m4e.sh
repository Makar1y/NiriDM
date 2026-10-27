#!/usr/bin/env bash
# Milestone 4e smoke test: --hide-bar / --hide-notification pick the right
# commands for the detected bar and notification daemon.
#
# Uses fake `pgrep` and fake daemon clients so no real session is touched.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4e.XXXXXX")
BIN=$H/bin
RUNNING=$H/running
CMD_LOG=$H/cmd.log

ok()  { echo "OK: $*"; }
fail() { echo "FAIL: $*"; exit 1; }

mkdir -p "$BIN" "$RUNNING"

# `pgrep -x NAME` / `pgrep -f NEEDLE`: the last argument is what to match.
cat >"$BIN/pgrep" <<'EOF'
#!/bin/sh
for last in "$@"; do :; done
[ -n "${NDM_FAKE_RUNNING:-}" ] && [ -e "$NDM_FAKE_RUNNING/$last" ]
EOF

# Every daemon client just records the exact invocation.
for name in noctalia noctalia-shell makoctl dunstctl swaync-client pkill; do
  cat >"$BIN/$name" <<'EOF'
#!/bin/sh
printf '%s %s\n' "$(basename "$0")" "$*" >> "${NDM_FAKE_CMD_LOG:?}"
EOF
done

# A mirror that exits quickly; the initial `niri msg --json windows` query
# already reports the mirror window, so the placement path is exercised too.
cat >"$BIN/quick-wlmirror" <<'EOF'
#!/bin/sh
sleep 0.2
EOF
chmod +x "$BIN"/*

export PATH="$BIN:$PATH"
export NDM_FAKE_RUNNING=$RUNNING
export NDM_FAKE_CMD_LOG=$CMD_LOG
export NiriDM_NIRI_BIN=$F/mock-niri
export NiriDM_CONFIG_DIR=$H
export NIRI_CONFIG=$H/main.kdl
export NDM_MOCK_STATE=$F/state-a.json
export NDM_MOCK_WINDOWS=mirror
touch "$H/main.kdl"

run_case() {
  name=$1; shift
  rm -f "$CMD_LOG"
  rm -rf "$RUNNING"; mkdir -p "$RUNNING"
  for proc in "$@"; do : >"$RUNNING/$proc"; done
  "$NDM" mirror DP-1 eDP-1 --hide-bar --hide-notification \
    --wl-mirror "$BIN/quick-wlmirror" 2>"$H/$name.err"
  echo "== $name: commands issued =="
  cat "$CMD_LOG" 2>/dev/null || true
}

has()    { grep -qx "$1" "$CMD_LOG" || fail "expected `$1` ($2)"; }
has_not(){ ! grep -qx "$1" "$CMD_LOG" || fail "unexpected `$1` ($2)"; }
count()  { local n; n=$(grep -cx "$1" "$CMD_LOG" || true); [ "$n" = "$2" ] || fail "expected $2 × `$1`, got $n"; }

echo "== swaync: set DND explicitly, never the blocking -s subscribe mode =="
run_case swaync swaync
has "swaync-client -dn -sw" swaync
has "swaync-client -df -sw" swaync
has_not "swaync-client -d -s" swaync
ok "swaync DND is enabled and disabled explicitly"

echo
echo "== Noctalia v5: bar and notification IPC =="
run_case noctalia-v5 noctalia
has "noctalia msg bar-hide" noctalia-v5
has "noctalia msg bar-show" noctalia-v5
has "noctalia msg notification-dnd-set on" noctalia-v5
has "noctalia msg notification-dnd-set off" noctalia-v5
ok "Noctalia v5 uses bar-hide/show and notification-dnd-set"

echo
echo "== Noctalia v4: legacy ipc call syntax =="
run_case noctalia-v4 noctalia-shell
has "noctalia-shell ipc call bar hideBar" noctalia-v4
has "noctalia-shell ipc call bar showBar" noctalia-v4
has "noctalia-shell ipc call notifications enableDND" noctalia-v4
has "noctalia-shell ipc call notifications disableDND" noctalia-v4
ok "Noctalia v4 uses the legacy ipc call syntax"

echo
echo "== mako =="
run_case mako mako
has "makoctl mode -a do-not-disturb" mako
has "makoctl mode -r do-not-disturb" mako
ok "mako DND mode is added and removed"

echo
echo "== dunst =="
run_case dunst dunst
has "dunstctl set-paused true" dunst
has "dunstctl set-paused false" dunst
ok "dunst is paused and resumed"

echo
echo "== waybar =="
run_case waybar waybar
count "pkill -USR1 waybar" 2
ok "waybar is toggled off and back on"

echo
echo "ALL SMOKE CHECKS PASSED"

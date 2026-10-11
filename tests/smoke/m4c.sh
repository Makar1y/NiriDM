#!/usr/bin/env bash
# Milestone 4c smoke test: workspace-to-output routing.
set -euo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
F=$R/tests/fixtures
NDM=$R/target/debug/ndm
H=$(mktemp -d "${TMPDIR:-/tmp}/niridm-m4c.XXXXXX")
CFG=$H/NiriDM
ACTIONS=$H/actions.log

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
echo "== ndm route Chat eDP-1 (partition off a workspace) =="
"$NDM" route Chat eDP-1 2>/dev/null
echo "--- monitors.kdl:"
cat "$CFG/monitors.kdl"
grep -q 'workspace "Chat" {' "$CFG/monitors.kdl" && grep -A1 'workspace "Chat" {' "$CFG/monitors.kdl" | grep -q 'output "eDP-1"' && echo "OK: monitors.kdl pins Chat to eDP-1"
grep -q 'workspace "Chat" output="eDP-1"' "$CFG/layouts.kdl" && echo "OK: layouts.kdl holds the route"

echo
echo "== route a workspace to an unknown output is rejected =="
if "$NDM" route Chat DP-9 2>"$H/bad.txt"; then
  echo "FAIL: unknown output should be rejected"; exit 1
fi
grep -q "not connected" "$H/bad.txt" && echo "OK: unknown output rejected"

echo
echo "== ndm route Chat (no output) removes the pin =="
"$NDM" route Chat 2>/dev/null
if grep -q 'workspace "Chat" {' "$CFG/monitors.kdl"; then
  echo "FAIL: route still present"; exit 1
fi
echo "OK: route removed"

echo
echo "== ndm switch Chat DP-1 (transient, no KDL write) =="
"$NDM" switch Chat DP-1 2>/dev/null
cat "$ACTIONS"
grep -q 'move-workspace-to-monitor --reference Chat DP-1' "$ACTIONS" && echo "OK: transient move issued"
if grep -q 'workspace "Chat" {' "$CFG/monitors.kdl"; then
  echo "FAIL: switch must not touch monitors.kdl"; exit 1
fi
echo "OK: switch wrote no KDL"

echo
echo "ALL SMOKE CHECKS PASSED"

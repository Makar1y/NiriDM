#!/usr/bin/env bash
# Build the workspace and run every end-to-end smoke test.
#
# Run inside `nix develop` (needs cargo on PATH). The smokes exercise the real
# `ndm` / `ndm-daemon` binaries against the mock niri in tests/fixtures, so no
# real niri session is required.
set -uo pipefail

R=$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)

echo "== cargo build =="
cargo build --workspace --manifest-path "$R/Cargo.toml"

fail=0
for t in m3 m4 m4b m4c; do
  echo
  echo "########## smoke: $t ##########"
  if bash "$R/tests/smoke/$t.sh"; then
    echo ">>> PASS: $t"
  else
    echo ">>> FAIL: $t"
    fail=1
  fi
done

echo
if [ "$fail" -eq 0 ]; then
  echo "ALL SMOKE TESTS PASSED"
else
  echo "SOME SMOKE TESTS FAILED"
fi
exit "$fail"

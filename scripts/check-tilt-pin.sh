#!/usr/bin/env bash
# Gate: the Cloud Agent bootstrap installs tilt only from a digest-verified
# archive. Runs the mutation tests for the checker first (so the gate's own
# logic can't silently rot), then applies it to the committed .cursor/install.sh.
# Logic + rationale live in scripts/check_tilt_pin.py.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"

if command -v python3 >/dev/null 2>&1; then
  if ! python3 "$SCRIPT_DIR/check_tilt_pin_tests.py" >/dev/null 2>&1; then
    echo "check-tilt-pin: mutation tests FAILED — the gate itself is broken." >&2
    echo "           python3 scripts/check_tilt_pin_tests.py" >&2
    exit 1
  fi
  python3 "$SCRIPT_DIR/check_tilt_pin.py" --check
else
  echo "check-tilt-pin: python3 not found; skipping (gate requires python3)." >&2
  exit 1
fi

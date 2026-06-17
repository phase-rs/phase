#!/usr/bin/env bash
set -euo pipefail

# Delegate to the isolated-target wrapper so the baseline rebuild never queues
# behind Tilt's shared target/debug builds (see scripts/ai-gate.sh).
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec "$ROOT/scripts/ai-gate.sh" --refresh-baseline "$@"

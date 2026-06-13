#!/usr/bin/env bash
set -euo pipefail

cargo run --bin ai-gate -- --refresh-baseline "$@"

#!/usr/bin/env bash
set -euo pipefail

mode="${1:---check}"
if [[ "$mode" != "--check" && "$mode" != "--write" ]]; then
  echo "usage: $0 [--check|--write]" >&2
  exit 2
fi

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"
cargo run -p engine --features interaction-bindings --bin interaction-bindings -- "$mode"

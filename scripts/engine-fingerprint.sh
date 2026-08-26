#!/usr/bin/env bash
# Computes the preview-native engine compatibility key from source and data.
set -euo pipefail

if [[ $# -ne 3 ]]; then
    echo "usage: $0 <commit-sha> <card-data.json path> <draft-pools.json path>" >&2
    exit 2
fi

commit_sha="$1"
card_data_path="$2"
draft_pools_path="$3"

card_data_sha256=$(sha256sum "$card_data_path" | awk '{print $1}')
draft_pools_sha256=$(sha256sum "$draft_pools_path" | awk '{print $1}')

{
    git ls-tree --full-tree -r "$commit_sha" -- crates Cargo.lock rust-toolchain.toml
    printf '%s\n' "$card_data_sha256"
    printf '%s\n' "$draft_pools_sha256"
} | sha256sum | awk '{print substr($1, 1, 16)}'

#!/usr/bin/env bash
# Shared pnpm version preflight for scripts/setup.sh.
#
# The pin lives in client/package.json's `packageManager` field, and that field
# is *directory-scoped*: corepack and pnpm >= 10 both resolve it from the
# nearest package.json walking up from the current directory. This repo has no
# root package.json, so `pnpm --version` at the repo root reports whatever
# global pnpm is on PATH, while the same command inside client/ reports the
# pinned version. Those genuinely differ — CI measured 11.24.0 at the root and
# 9.15.9 in client/ on the same machine.
#
# Every real pnpm invocation in this repo runs from client/ (`cd client &&
# pnpm ...`, or Tiltfile `dir=client`), so client/ is the only context whose
# resolved version predicts what will actually install. Resolving anywhere else
# rejects a valid environment.
#
# Sourced by scripts/setup.sh; exercised by scripts/lib/pnpm_preflight_tests.sh.

# Major version pinned by $1/package.json's `packageManager`, or empty if the
# file, the field, or a numeric major is absent. Accepts both "pnpm@9.15.9" and
# the equally legal bare "pnpm@10", so a shorthand pin cannot silently disarm
# the caller's check.
pnpm_pinned_major() {
  local dir="$1"
  sed -n 's/.*"packageManager"[[:space:]]*:[[:space:]]*"pnpm@\([0-9][0-9]*\).*/\1/p' \
    "$dir/package.json" 2>/dev/null | head -1 || true
}

# Major version pnpm actually resolves to *when run from $1*, or empty if pnpm
# is absent or its --version fails. The `|| true` matters under the caller's
# `set -euo pipefail`: a corepack shim with no network exits nonzero, and
# without it the failing pipeline would abort setup with no diagnostic.
pnpm_resolved_major() {
  local dir="$1"
  ( cd "$dir" && pnpm --version ) 2>/dev/null | cut -d. -f1 || true
}

# Compare the two. Returns 0 when they agree, when the pin is unreadable, or
# when pnpm cannot report a version (let the real `pnpm install` surface that
# with its own message); returns 1 only on a genuine major mismatch.
pnpm_preflight_check() {
  local dir="${1:-client}" want have
  want="$(pnpm_pinned_major "$dir")"
  have="$(pnpm_resolved_major "$dir")"

  if [ -n "$want" ] && [ -n "$have" ] && [ "$have" != "$want" ]; then
    echo "ERROR: pnpm $have resolves in $dir/, but it pins pnpm $want." >&2
    echo "  pnpm >= 10 ignores the \"pnpm\" field in $dir/package.json and will" >&2
    echo "  rewrite $dir/pnpm-lock.yaml without its security overrides." >&2
    echo "  Fix: pnpm self-update $want   (or: corepack enable)" >&2
    return 1
  fi
  return 0
}

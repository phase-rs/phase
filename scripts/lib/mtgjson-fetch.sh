# shellcheck shell=bash
# Shared hardened fetch helpers for MTGJSON downloads — used by
# gen-card-data.sh, fetch-token-sets.sh, fetch-draft-sets.sh, and the CI
# card-data jobs. Mirrors scripts/lib/scryfall-fetch.sh.
#
# MTGJSON serves its bulk files (AtomicCards.json is ~156 MB uncompressed)
# from a host that drops large/bursty anonymous connections mid-transfer —
# `curl: (56) Recv failure: Connection reset by peer` part way through a
# multi-minute download. A bare `curl -fSL` has no retry, so a single reset
# fails the whole CI job, and re-downloading 156 MB on every cache miss makes
# the reset far more likely.
#
# These helpers close that gap by:
#   1. Preferring the gzipped artifact (AtomicCards.json.gz is ~50 MB — 3x
#      less to transfer and 3x less reset surface) and decompressing locally.
#   2. Retrying transient resets/throttles with backoff via --retry-all-errors
#      (curl >= 7.71), which retries the mid-transfer reset (exit 56) that
#      plain --retry classes as non-transient and skips.
#   3. Downloading through a temp file + atomic rename, so an interrupted
#      transfer never leaves a truncated JSON in place for a later run to
#      "cache-hit" on.
#   4. A polite inter-request delay (mtgjson_rate_limit) for the per-set fetch
#      loops, so a long sweep doesn't trip mtgjson's connection throttling.
#
# Source this file; do not execute it. Callers keep their own `set -euo
# pipefail`; a non-zero return propagates as a fail-fast exit.

MTGJSON_BASE="${MTGJSON_BASE:-https://mtgjson.com/api/v5}"

MTGJSON_CURL=(
  curl --fail --retry 5 --retry-all-errors --retry-delay 3
  --connect-timeout 30 -sSL
  -H 'User-Agent: phase-rs-card-data/1.0 (+https://github.com/phase-rs/phase)'
)

# Delay between successive per-set requests. Override with MTGJSON_RATE_DELAY.
MTGJSON_RATE_DELAY="${MTGJSON_RATE_DELAY:-1}"

# mtgjson_rate_limit — sleep MTGJSON_RATE_DELAY seconds between per-set fetches.
mtgjson_rate_limit() {
  sleep "$MTGJSON_RATE_DELAY"
}

# mtgjson_download NAME DEST — download MTGJSON file NAME (e.g. AtomicCards.json)
# to DEST, preferring the gzipped artifact and decompressing locally. Retries
# transient failures, writes through a temp, and only renames a complete file
# into place. gzip's CRC validates the compressed path end-to-end; curl --fail
# guards the uncompressed fallback against a truncated/error body. Returns
# non-zero only if both the compressed and uncompressed artifacts fail.
mtgjson_download() {
  local name="$1" dest="$2" tmp
  tmp=$(mktemp "${dest}.XXXXXX")
  # Preferred path: gzipped artifact -> file (retryable), then decompress.
  if "${MTGJSON_CURL[@]}" -o "$tmp.gz" "$MTGJSON_BASE/$name.gz" 2>/dev/null \
     && gunzip -c "$tmp.gz" > "$tmp" 2>/dev/null; then
    rm -f "$tmp.gz"
    mv -f "$tmp" "$dest"
    return 0
  fi
  rm -f "$tmp.gz"
  # Fallback: uncompressed artifact (a few legacy files lack a .gz sibling).
  if "${MTGJSON_CURL[@]}" -o "$tmp" "$MTGJSON_BASE/$name"; then
    mv -f "$tmp" "$dest"
    return 0
  fi
  rm -f "$tmp"
  return 1
}

#!/usr/bin/env bash
set -euo pipefail

# Generate the Simplified-Chinese card-content sidecar (`card-data.zh.json`).
#
# Sibling of the MTGJSON-derived sidecars that `oracle-gen --sidecar-dir` writes.
# Those cover es/fr/de/it/ja/pt from MTGJSON `foreignData`; Chinese comes from a
# different source (see scripts/lib/magic_cards_zhs.py for why), so it needs its
# own generator rather than another entry in oracle-gen's locale map.
#
# Source: https://github.com/HeliumOctahelide/magic-cards-zhs
# Licensed CC-BY-SA-4.0. See NOTICE and DMCA.md for the attribution and the
# Wizards-of-the-Coast provenance caveat that source does not itself address.
#
# The upstream export is a ~78 MB tar.gz republished every Saturday, containing
# ~381 MB of JSONL. Only `zhs_oracle.json` is needed, so the archive is cached
# and that one member is streamed out of it rather than extracting all seven
# files.

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# Pinned deliberately. Upstream rotates old releases out of the releases API
# (only 3 of its 44 tags still carry assets), so "always resolve latest" would
# make this generator fail on a week when the newest release is briefly absent,
# and would silently change the committed data. Bump this tag on a data refresh.
# The reviewed release and the SHA-256 of its archive live in a pin file, so a
# tag bump and a digest bump cannot drift apart. See
# docs/CHINESE-CARD-SOURCES.md for provenance and why the pin is corroborated
# against GitHub's own published asset digest rather than trusted as "whatever
# we downloaded once".
LOCK_FILE="${MAGIC_CARDS_ZHS_LOCK:-$SCRIPT_DIR/magic-cards-zhs.lock}"
if [ ! -f "$LOCK_FILE" ]; then
  echo "ERROR: missing pin file $LOCK_FILE (records the reviewed release + digest)." >&2
  exit 1
fi
# shellcheck disable=SC1090
. "$LOCK_FILE"
if [ -z "${release:-}" ] || [ -z "${sha256:-}" ]; then
  echo "ERROR: $LOCK_FILE must define both 'release' and 'sha256'." >&2
  exit 1
fi
RELEASE_TAG="${MAGIC_CARDS_ZHS_RELEASE:-$release}"
PINNED_SHA256="$sha256"
# Upstream's asset name repeats the `data-` prefix inside the filename:
# tag `data-2026-10-11` -> asset `magic-cards-zhs-data-2026-10-11.tar.gz`.
ASSET_NAME="magic-cards-zhs-${RELEASE_TAG}.tar.gz"
ARCHIVE_URL="${MAGIC_CARDS_ZHS_URL:-https://github.com/HeliumOctahelide/magic-cards-zhs/releases/download/$RELEASE_TAG/$ASSET_NAME}"

CACHE_DIR="${MAGIC_CARDS_ZHS_CACHE_DIR:-data/magic-cards-zhs}"
ARCHIVE="$CACHE_DIR/$ASSET_NAME"
SOURCE_MEMBER="zhs_oracle.json"
OUTPUT="${MAGIC_CARDS_ZHS_OUTPUT:-client/public/card-data.zh.json}"

echo "=== Simplified Chinese Card Data Generation ($RELEASE_TAG) ==="

mkdir -p "$CACHE_DIR"

# Only the ARCHIVE is cached. The extracted member is deliberately re-derived on
# every run from a scratch directory rather than being kept beside it: an
# existence-gated extracted file cannot tell one release from another, so a
# pinned-tag bump (or a workflow restoring an older cache under a fallback key)
# would keep rebuilding the sidecar from stale data forever. Re-extracting one
# tar member costs a second; getting the vintage wrong costs a silent, permanent
# wrong answer. The archive name carries the tag, so its existence IS a
# release-scoped fact.
if [ ! -f "$ARCHIVE" ]; then
  echo "Downloading $ASSET_NAME..."
  # --http1.1: GitHub's HTTP/2 edge aborts this particular asset mid-transfer
  # with PROTOCOL_ERROR often enough to matter; --retry-all-errors covers the
  # remaining resets. The download is resumable, so a partial file is kept.
  curl --fail --http1.1 --retry 5 --retry-all-errors --retry-delay 3 \
    --connect-timeout 30 -L -C - \
    -H 'User-Agent: phase-rs-card-data/1.0 (+https://github.com/phase-rs/phase)' \
    -o "$ARCHIVE" "$ARCHIVE_URL"
  echo "  downloaded $(du -h "$ARCHIVE" | cut -f1)"
fi

# Integrity gate, in order: the bytes must be the pinned artifact, and must be a
# whole archive. Both checks delete the file on failure, because leaving a bad
# archive in place would let a later  resume append to a bad prefix
# and would let the deploy cache freeze it under a key later runs restore.
if ! command -v shasum >/dev/null 2>&1 && ! command -v sha256sum >/dev/null 2>&1; then
  echo "ERROR: need shasum or sha256sum to verify $ASSET_NAME against its pin." >&2
  exit 1
fi
actual_sha256=$(if command -v shasum >/dev/null 2>&1; then shasum -a 256 "$ARCHIVE"; else sha256sum "$ARCHIVE"; fi | awk '{print $1}')
if [ "$actual_sha256" != "$PINNED_SHA256" ]; then
  echo "ERROR: $ASSET_NAME does not match the pinned digest." >&2
  echo "  expected: $PINNED_SHA256" >&2
  echo "  actual  : $actual_sha256" >&2
  rm -f "$ARCHIVE"
  echo "  Removed it. If upstream legitimately re-released $RELEASE_TAG, refresh" >&2
  echo "  scripts/magic-cards-zhs.lock and docs/CHINESE-CARD-SOURCES.md together." >&2
  exit 1
fi
echo "  sha256 verified against $LOCK_FILE"

if ! gzip -t "$ARCHIVE"; then
  echo "ERROR: $ARCHIVE is not a valid gzip archive (truncated download?)." >&2
  rm -f "$ARCHIVE"
  echo "  Removed it; re-run to download again." >&2
  exit 1
fi

WORK_DIR="$(mktemp -d)"
trap 'rm -rf "$WORK_DIR"' EXIT
EXTRACTED="$WORK_DIR/$SOURCE_MEMBER"

echo "Extracting $SOURCE_MEMBER..."
# The member path inside the archive is top-level (`./zhs_oracle.json`), so
# --strip-components has nothing to strip.
tar -xzf "$ARCHIVE" -C "$WORK_DIR" "./$SOURCE_MEMBER"

if [ ! -s "$EXTRACTED" ]; then
  echo "ERROR: $SOURCE_MEMBER is missing or empty after extraction." >&2
  exit 1
fi

echo "Building $OUTPUT..."
mkdir -p "$(dirname "$OUTPUT")"
python3 "$SCRIPT_DIR/lib/magic_cards_zhs.py" --source "$EXTRACTED" --output "$OUTPUT.tmp"

# Promote only after the builder has written a complete file, mirroring
# oracle-gen's tmp+rename so a reader never sees a partial sidecar.
mv -f "$OUTPUT.tmp" "$OUTPUT"

echo "Generated $OUTPUT ($(du -h "$OUTPUT" | cut -f1))"

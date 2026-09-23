#!/usr/bin/env bash
set -euo pipefail

# Upload the gzip objects prepared by build-onedeck-cloudflare.sh. Keeping this
# in a separate workflow step means Cloudflare credentials are absent while
# dependencies and the frontend build execute.

cd "$(dirname "${BASH_SOURCE[0]}")/.."

: "${ONEDECK_R2_BUCKET:?set ONEDECK_R2_BUCKET to the OneDeck R2 bucket}"
: "${ONEDECK_R2_STAGING_DIR:?set ONEDECK_R2_STAGING_DIR to the prepared gzip directory}"

STAGING_DIR="$ONEDECK_R2_STAGING_DIR"
MANIFEST="$STAGING_DIR/manifest.json"
[ -s "$MANIFEST" ] || {
  echo "ERROR: R2 staging manifest is missing: $MANIFEST" >&2
  exit 1
}

while IFS= read -r asset; do
  key="$(jq -r '.key' <<<"$asset")"
  file="$(jq -r '.file' <<<"$asset")"
  content_type="$(jq -r '.content_type' <<<"$asset")"
  cache_control="$(jq -r '.cache_control' <<<"$asset")"
  [ -s "$STAGING_DIR/$file" ] || {
    echo "ERROR: staged R2 object is missing: $STAGING_DIR/$file" >&2
    exit 1
  }
  (cd client && pnpm wrangler r2 object put "$ONEDECK_R2_BUCKET/$key" \
    --file "$STAGING_DIR/$file" --remote --content-type "$content_type" \
    --content-encoding gzip --cache-control "$cache_control")
done < <(jq -c '.assets[]' "$MANIFEST")

if [ "${ONEDECK_VERIFY_R2:-0}" = "1" ]; then
  : "${ONEDECK_R2_PUBLIC_URL:?set ONEDECK_R2_PUBLIC_URL when verifying R2}"
  R2_PUBLIC_URL="${ONEDECK_R2_PUBLIC_URL%/}"
  case "$R2_PUBLIC_URL" in
    https://*) ;;
    *) echo "ERROR: ONEDECK_R2_PUBLIC_URL must be an https URL" >&2; exit 2 ;;
  esac
  VERIFY_DIR="$(mktemp -d "${TMPDIR:-/tmp}/onedeck-r2-verify.XXXXXX")"
  cleanup() { rm -rf "$VERIFY_DIR"; }
  trap cleanup EXIT
  while IFS= read -r asset; do
    key="$(jq -r '.key' <<<"$asset")"
    file="$(jq -r '.file' <<<"$asset")"
    url="$R2_PUBLIC_URL/$key"
    curl -fsSI "$url" | tr -d '\r' | grep -qi '^content-encoding: gzip$' || {
      echo "ERROR: $url is not served with Content-Encoding: gzip" >&2
      exit 1
    }
    curl --compressed -fsS "$url" -o "$VERIFY_DIR/$file"
    gzip -dc "$STAGING_DIR/$file" | cmp -s - "$VERIFY_DIR/$file" || {
      echo "ERROR: gzip round-trip differs for $key" >&2
      exit 1
    }
  done < <(jq -c '.assets[]' "$MANIFEST")
fi

echo "Uploaded $(jq '.assets | length' "$MANIFEST") gzip R2 objects."

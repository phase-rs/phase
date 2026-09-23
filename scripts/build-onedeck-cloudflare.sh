#!/usr/bin/env bash
set -euo pipefail

# Build the OneDeck Cloudflare profile. The Pages artifact is intentionally
# assembled only after the large runtime inputs have been uploaded to the
# profile-owned R2 bucket, so Vite can pin the exact card pool and engine.
#
# Required environment:
#   ONEDECK_R2_PUBLIC_URL       public R2 prefix, without a trailing slash
#   ONEDECK_R2_BUCKET            bucket name used by wrangler
#   ONEDECK_WORKER_URL           HTTPS base URL of the dedicated Worker
#   ONEDECK_PAGES_ORIGIN         exact Pages origin allowed by the Worker
#
# Optional switches:
#   ONEDECK_GENERATE_DATA=1      run the card-data generator first
#   ONEDECK_UPLOAD_R2=0          prepare and verify locally, do not upload
#   ONEDECK_VERIFY_R2=0          skip remote header/round-trip checks
#   ONEDECK_R2_STAGING_DIR      persist gzip objects for a separate upload step

cd "$(dirname "${BASH_SOURCE[0]}")/.."

: "${ONEDECK_R2_PUBLIC_URL:?set ONEDECK_R2_PUBLIC_URL to the OneDeck R2 public prefix}"
: "${ONEDECK_R2_BUCKET:?set ONEDECK_R2_BUCKET to the OneDeck R2 bucket}"
: "${ONEDECK_WORKER_URL:?set ONEDECK_WORKER_URL to the dedicated Worker HTTPS URL}"
: "${ONEDECK_PAGES_ORIGIN:?set ONEDECK_PAGES_ORIGIN to the exact Pages origin}"

R2_PUBLIC_URL="${ONEDECK_R2_PUBLIC_URL%/}"
WORKER_URL="${ONEDECK_WORKER_URL%/}"
PAGES_ORIGIN="$(node scripts/normalize-web-origin.mjs "$ONEDECK_PAGES_ORIGIN")" || {
  echo "ERROR: ONEDECK_PAGES_ORIGIN must be an https origin without a path, query, fragment, userinfo, or trailing slash" >&2
  exit 2
}
UPLOAD_R2="${ONEDECK_UPLOAD_R2:-0}"
VERIFY_R2="${ONEDECK_VERIFY_R2:-0}"
STAGING_DIR="${ONEDECK_R2_STAGING_DIR:-}"

case "$WORKER_URL" in
  https://*) ;;
  *) echo "ERROR: ONEDECK_WORKER_URL must be an https URL" >&2; exit 2 ;;
esac

sha256_file() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

if [ "${ONEDECK_GENERATE_DATA:-0}" = "1" ]; then
  ./scripts/gen-card-data.sh
fi

if [ "${ONEDECK_RUN_SEMANTIC_AUDIT:-1}" = "1" ]; then
  cargo semantic-audit data/
  card_hash="$(sha256_file client/public/card-data.json)"
  jq --arg h "$card_hash" '. + {card_data_hash: $h}' \
    data/semantic-audit.json > client/public/semantic-audit.json
fi

[ -s client/public/card-data.json ] || {
  echo "ERROR: client/public/card-data.json is missing; run with ONEDECK_GENERATE_DATA=1." >&2
  exit 1
}

./scripts/build-wasm.sh release
[ -s client/src/wasm/engine_wasm_bg.wasm ] || {
  echo "ERROR: engine WASM was not produced." >&2
  exit 1
}
[ -s client/src/wasm/draft_wasm_bg.wasm ] || {
  echo "ERROR: draft WASM was not produced." >&2
  exit 1
}

CARD_HASH16="$(sha256_file client/public/card-data.json | cut -c1-16)"
WASM_HASH16="$(sha256_file client/src/wasm/engine_wasm_bg.wasm | cut -c1-16)"
DRAFT_WASM_HASH16="$(sha256_file client/src/wasm/draft_wasm_bg.wasm | cut -c1-16)"
CARD_DATA_KEY="card-data-${CARD_HASH16}.json"
ENGINE_WASM_KEY="engine_wasm_bg-${WASM_HASH16}.wasm"
DRAFT_WASM_KEY="draft_wasm_bg-${DRAFT_WASM_HASH16}.wasm"
CARD_DATA_URL="$R2_PUBLIC_URL/$CARD_DATA_KEY"
ENGINE_WASM_URL="$R2_PUBLIC_URL/$ENGINE_WASM_KEY"
DRAFT_WASM_URL="$R2_PUBLIC_URL/$DRAFT_WASM_KEY"
MULTIPLAYER_URL="${ONEDECK_MULTIPLAYER_SERVER_URL:-${WORKER_URL/https:/wss:}/ws}"
TURN_CREDENTIALS_URL="$WORKER_URL/turn-credentials"

TMP_DIR="$(mktemp -d "${TMPDIR:-/tmp}/onedeck-cloudflare.XXXXXX")"
cleanup() { rm -rf "$TMP_DIR"; }
trap cleanup EXIT

gzip_asset() {
  local source="$1"
  local output="$2"
  gzip -9 -n -c "$source" > "$output"
  gzip -dc "$output" | cmp -s - "$source"
}

CARD_GZIP="$TMP_DIR/$CARD_DATA_KEY.gz"
WASM_GZIP="$TMP_DIR/$ENGINE_WASM_KEY.gz"
DRAFT_WASM_GZIP="$TMP_DIR/$DRAFT_WASM_KEY.gz"
gzip_asset client/public/card-data.json "$CARD_GZIP"
gzip_asset client/src/wasm/engine_wasm_bg.wasm "$WASM_GZIP"
gzip_asset client/src/wasm/draft_wasm_bg.wasm "$DRAFT_WASM_GZIP"

r2_put_gzip() {
  local source="$1"
  local key="$2"
  local content_type="$3"
  local cache_control="$4"
  (cd client && pnpm wrangler r2 object put "$ONEDECK_R2_BUCKET/$key" \
    --file "$source" --remote --content-type "$content_type" \
    --content-encoding gzip --cache-control "$cache_control")
}

if [ "$UPLOAD_R2" = "1" ] || [ -n "$STAGING_DIR" ]; then
  if [ -n "$STAGING_DIR" ]; then
    mkdir -p "$STAGING_DIR"
    rm -f "$STAGING_DIR/manifest.json" \
      "$STAGING_DIR/$CARD_DATA_KEY.gz" \
      "$STAGING_DIR/$ENGINE_WASM_KEY.gz" \
      "$STAGING_DIR/$DRAFT_WASM_KEY.gz"
    cp "$CARD_GZIP" "$STAGING_DIR/$CARD_DATA_KEY.gz"
    cp "$WASM_GZIP" "$STAGING_DIR/$ENGINE_WASM_KEY.gz"
    cp "$DRAFT_WASM_GZIP" "$STAGING_DIR/$DRAFT_WASM_KEY.gz"
    staged_assets='[]'
    staged_assets="$(jq --arg key "$CARD_DATA_KEY" \
      --arg file "$CARD_DATA_KEY.gz" \
      '. + [{key: $key, file: $file, content_type: "application/json", cache_control: "public, max-age=31536000, immutable"}]' \
      <<<"$staged_assets")"
    staged_assets="$(jq --arg key "$ENGINE_WASM_KEY" \
      --arg file "$ENGINE_WASM_KEY.gz" \
      '. + [{key: $key, file: $file, content_type: "application/wasm", cache_control: "public, max-age=31536000, immutable"}]' \
      <<<"$staged_assets")"
    staged_assets="$(jq --arg key "$DRAFT_WASM_KEY" \
      --arg file "$DRAFT_WASM_KEY.gz" \
      '. + [{key: $key, file: $file, content_type: "application/wasm", cache_control: "public, max-age=31536000, immutable"}]' \
      <<<"$staged_assets")"
  fi

  if [ "$UPLOAD_R2" = "1" ]; then
    r2_put_gzip "$CARD_GZIP" "$CARD_DATA_KEY" application/json \
      "public, max-age=31536000, immutable"
    r2_put_gzip "$WASM_GZIP" "$ENGINE_WASM_KEY" application/wasm \
      "public, max-age=31536000, immutable"
    r2_put_gzip "$DRAFT_WASM_GZIP" "$DRAFT_WASM_KEY" application/wasm \
      "public, max-age=31536000, immutable"
  fi

  while IFS= read -r filename; do
    source="client/public/$filename"
    [ -s "$source" ] || {
      echo "ERROR: data-files.json lists missing $source" >&2
      exit 1
    }
    output="$TMP_DIR/$filename.gz"
    gzip_asset "$source" "$output"
    if [ "$UPLOAD_R2" = "1" ]; then
      r2_put_gzip "$output" "$filename" application/json \
        "public, max-age=60, must-revalidate"
    fi
    if [ -n "$STAGING_DIR" ]; then
      cp "$output" "$STAGING_DIR/$filename.gz"
      staged_assets="$(jq --arg key "$filename" \
        --arg file "$filename.gz" \
        '. + [{key: $key, file: $file, content_type: "application/json", cache_control: "public, max-age=60, must-revalidate"}]' \
        <<<"$staged_assets")"
    fi
  done < <(jq -r '.[]' data-files.json)

  if [ -n "$STAGING_DIR" ]; then
    jq -n --argjson assets "$staged_assets" '{assets: $assets}' \
      > "$STAGING_DIR/manifest.json"
  fi
fi

if [ "$VERIFY_R2" = "1" ]; then
  require_gzip_header() {
    local url="$1"
    curl -fsSI "$url" | tr -d '\r' | grep -qi '^content-encoding: gzip$' || {
      echo "ERROR: $url is not served with Content-Encoding: gzip" >&2
      exit 1
    }
  }
  require_gzip_header "$CARD_DATA_URL"
  require_gzip_header "$ENGINE_WASM_URL"
  require_gzip_header "$DRAFT_WASM_URL"
  curl --compressed -fsS "$CARD_DATA_URL" | jq -e 'type == "object"' >/dev/null
  curl --compressed -fsS "$ENGINE_WASM_URL" -o "$TMP_DIR/wasm-roundtrip"
  cmp -s client/src/wasm/engine_wasm_bg.wasm "$TMP_DIR/wasm-roundtrip" || {
    echo "ERROR: gzip WASM round-trip differs from the source bytes." >&2
    exit 1
  }
  curl --compressed -fsS "$DRAFT_WASM_URL" -o "$TMP_DIR/draft-wasm-roundtrip"
  cmp -s client/src/wasm/draft_wasm_bg.wasm "$TMP_DIR/draft-wasm-roundtrip" || {
    echo "ERROR: gzip draft WASM round-trip differs from the source bytes." >&2
    exit 1
  }
fi

if [ ! -d client/node_modules ]; then
  echo "ERROR: client/node_modules is missing; run (cd client && pnpm install --frozen-lockfile)." >&2
  exit 1
fi

(
  export DATA_BASE_URL="$R2_PUBLIC_URL"
  export CARD_DATA_URL="$CARD_DATA_URL"
  export ENGINE_WASM_URL="$ENGINE_WASM_URL"
  export DRAFT_WASM_URL="$DRAFT_WASM_URL"
  export VITE_IMPORT_DECK_URL="$WORKER_URL"
  export OFFICIAL_MULTIPLAYER_SERVER_URL="$MULTIPLAYER_URL"
  export DEFAULT_MULTIPLAYER_SERVER_URL="$MULTIPLAYER_URL"
  export TURN_CREDENTIALS_URL="$TURN_CREDENTIALS_URL"
  export SUPABASE_URL=""
  export SUPABASE_ANON_KEY=""
  export TELEMETRY_URL=""
  export AUDIO_BASE_URL=""
  cd client
  pnpm build
)

# R2 is the sole source for generated JSON and the engine in this profile.
# Never let Vite's public/ copy become part of the Pages artifact.
shopt -s nullglob
while IFS= read -r filename; do
  rm -f "client/dist/$filename" "client/dist/$filename.br" "client/dist/$filename.gz"
done < <(jq -r '.[]' data-files.json)
rm -f client/dist/card-data.json client/dist/card-data.json.br client/dist/card-data.json.gz
for file in client/dist/card-data-*.json client/dist/card-data-*.json.br client/dist/card-data-*.json.gz; do
  base="$(basename "$file")"
  [[ "$base" =~ ^card-data-[0-9a-f]{16}\.json(\.(br|gz))?$ ]] && rm -f "$file"
done
rm -f client/dist/engine_wasm_bg*.wasm client/dist/engine_wasm_bg*.wasm.br client/dist/engine_wasm_bg*.wasm.gz
rm -f client/dist/draft_wasm_bg*.wasm client/dist/draft_wasm_bg*.wasm.br client/dist/draft_wasm_bg*.wasm.gz
cp client/deploy/cloudflare-pages/_headers client/dist/_headers
cp client/dist/index.html client/dist/404.html

oversized="$(find client/dist -type f -size +25M -print -quit)"
if [ -n "$oversized" ]; then
  echo "ERROR: Cloudflare Pages 25 MiB file limit exceeded: $oversized" >&2
  exit 1
fi

forbidden="$(find client/dist -type f \( -name '*.gz' -o -name 'card-data*.json' -o -name 'engine_wasm_bg*.wasm' -o -name 'draft_wasm_bg*.wasm' -o -name '.env*' \) -print -quit)"
if [ -n "$forbidden" ]; then
  echo "ERROR: generated runtime data/WASM or environment file leaked into Pages: $forbidden" >&2
  exit 1
fi
if rg -n --hidden -g '!*.map' \
  'TURN_KEY_API_TOKEN|CLOUDFLARE_API_TOKEN|SUPABASE_SERVICE_ROLE|BEGIN (RSA|OPENSSH|EC) PRIVATE KEY|https://lobby\.phase-rs\.dev|https://phase-rs\.dev/turn-credentials' \
  client/dist >/dev/null; then
  echo "ERROR: a credential or official lobby/TURN endpoint marker was found in client/dist." >&2
  exit 1
fi

echo "OneDeck Cloudflare build ready:"
echo "  card data: $CARD_DATA_URL"
echo "  engine WASM: $ENGINE_WASM_URL"
echo "  draft WASM: $DRAFT_WASM_URL"
echo "  multiplayer: $MULTIPLAYER_URL"
echo "  Pages files over 25 MiB: none"

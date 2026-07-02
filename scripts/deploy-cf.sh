#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")/.."

# r2.dev does not compress on the fly; we pre-compress every uploaded JSON with
# brotli and store Content-Encoding: br (consumers fetch via browser/Bun fetch,
# which decodes transparently). Fail early if brotli is missing.
command -v brotli >/dev/null || { echo "ERROR: brotli required (brew install brotli)"; exit 1; }

PROJECT_NAME="${1:-phase-rs}"
R2_BUCKET="phase-rs-data"
R2_PUBLIC="https://data.phase-rs.dev"

export CARD_DATA_URL="${CARD_DATA_URL:-$R2_PUBLIC/card-data.json}"
export COVERAGE_DATA_URL="${COVERAGE_DATA_URL:-$R2_PUBLIC/coverage-data.json}"
export COVERAGE_SUMMARY_URL="${COVERAGE_SUMMARY_URL:-$R2_PUBLIC/coverage-summary.json}"
export AUDIO_BASE_URL="${AUDIO_BASE_URL:-$R2_PUBLIC/audio}"
# Per-locale content-i18n sidecars are offloaded to R2 like card-data.json;
# the {lng} template resolves to where the upload loop below PUTs them.
export CARD_DATA_LOCALE_URL_TEMPLATE="${CARD_DATA_LOCALE_URL_TEMPLATE:-$R2_PUBLIC/card-data.{lng}.json}"

DEPLOY_CACHE=".deploy-cache"
touch "$DEPLOY_CACHE"

# --- Generate lightweight coverage summary for menu page ---
echo "Generating coverage summary..."
jq '{total_cards, supported_cards, coverage_pct, coverage_by_format}' \
  client/public/coverage-data.json > client/public/coverage-summary.json

# --- R2 uploads (run in background, parallel to WASM build) ---
upload_to_r2() {
  # Compress into a temp dir, never client/public: a stray .br there would be
  # copied into dist by the later `pnpm build` and shipped to Pages.
  local BRDIR
  BRDIR="$(mktemp -d)"
  trap 'rm -rf "$BRDIR"' RETURN
  # Upload JSON data files in parallel, skipping unchanged
  local json_pids=()
  for entry in \
    "card-data.json:public/card-data.json" \
    "card-data.de.json:public/card-data.de.json" \
    "card-data.es.json:public/card-data.es.json" \
    "card-data.fr.json:public/card-data.fr.json" \
    "card-data.it.json:public/card-data.it.json" \
    "card-data.pt.json:public/card-data.pt.json" \
    "coverage-data.json:public/coverage-data.json" \
    "coverage-summary.json:public/coverage-summary.json"; do
    key="${entry%%:*}"
    file="${entry#*:}"
    (
      # The -br9 tag suffix versions the cache on encoding: a pre-existing entry
      # recorded as a bare md5 (uncompressed upload) won't match, so the file is
      # re-uploaded compressed exactly once, then stays cached.
      local_tag="$(md5 -q "client/$file")-br9"
      cached_tag=$(grep "^$key:" "$DEPLOY_CACHE" 2>/dev/null | cut -d: -f2 || true)
      if [ "$local_tag" = "$cached_tag" ]; then
        echo "  = $key (unchanged)"
      else
        echo "  ^ $key (compress + upload)"
        brotli -q 9 -c "client/$file" > "$BRDIR/$key.br"
        (cd client && pnpm wrangler r2 object put "$R2_BUCKET/$key" \
          --file "$BRDIR/$key.br" --content-type application/json --content-encoding br --remote)
        # Update cache atomically
        grep -v "^$key:" "$DEPLOY_CACHE" > "$DEPLOY_CACHE.tmp" 2>/dev/null || true
        echo "$key:$local_tag" >> "$DEPLOY_CACHE.tmp"
        mv "$DEPLOY_CACHE.tmp" "$DEPLOY_CACHE"
      fi
    ) &
    json_pids+=($!)
  done

  # Upload audio files in parallel, skipping existing
  echo "Uploading audio to R2 (skipping existing)..."
  local audio_pids=()
  for f in client/public/audio/music/planeswalker-*.m4a; do
    (
      name=$(basename "$f")
      if curl -sf --head "$R2_PUBLIC/audio/$name" >/dev/null 2>&1; then
        echo "  = $name (exists)"
      else
        echo "  ^ $name (uploading)"
        (cd client && pnpm wrangler r2 object put "$R2_BUCKET/audio/$name" \
          --file "public/audio/music/$name" --content-type audio/mp4 --remote)
      fi
    ) &
    audio_pids+=($!)
  done

  # Wait for all uploads
  for pid in "${json_pids[@]}" "${audio_pids[@]}"; do
    wait "$pid"
  done
  echo "R2 uploads complete."

  # Verify uploads actually reached remote R2 (not local emulator)
  echo "Verifying R2 uploads are accessible..."
  if ! curl -sf --head "$R2_PUBLIC/coverage-summary.json" >/dev/null 2>&1; then
    echo "ERROR: R2 upload verification failed — coverage-summary.json not accessible at $R2_PUBLIC"
    echo "  Uploads may have gone to local emulator. Ensure --remote flag is present on all r2 object put commands."
    exit 1
  fi
}

echo "Starting R2 uploads (background) and WASM build (foreground)..."
upload_to_r2 &
R2_PID=$!

# --- WASM build (foreground) ---
echo "Building WASM (release)..."
./scripts/build-wasm.sh release

# --- Wait for R2 uploads before frontend build ---
wait $R2_PID
echo "All R2 uploads finished."

# --- Frontend build ---
echo "Building frontend..."
echo "  CARD_DATA_URL=$CARD_DATA_URL"
echo "  COVERAGE_DATA_URL=$COVERAGE_DATA_URL"
echo "  COVERAGE_SUMMARY_URL=$COVERAGE_SUMMARY_URL"
echo "  AUDIO_BASE_URL=$AUDIO_BASE_URL"
(cd client && pnpm build)

# Remove large data/audio files and their compressed variants — served from R2
rm -f client/dist/card-data.json client/dist/card-data.json.br
# Locale sidecars (card-data.<lng>.json) — served from R2, strip from bundle.
rm -f client/dist/card-data.??.json client/dist/card-data.??.json.br
rm -f client/dist/coverage-data.json client/dist/coverage-data.json.br
rm -f client/dist/coverage-summary.json client/dist/coverage-summary.json.br
rm -f client/dist/audio/music/planeswalker-*.m4a

# --- Deploy ---
echo "Deploying to Cloudflare Pages ($PROJECT_NAME)..."
(cd client && pnpm wrangler pages deploy dist --project-name="$PROJECT_NAME" --branch=main --commit-dirty=true)

#!/usr/bin/env bash
set -euo pipefail

# Builds the self-hosted web client into client/dist, and optionally packages it
# as a container image for the phase-server chart's `web.enabled` sidecar.
#
#   ./scripts/build-selfhost-web.sh
#   IMAGE=ghcr.io/you/phase-web:v0.59.0 ./scripts/build-selfhost-web.sh --push
#
# The bundle is deliberately server-agnostic: it carries no lobby address. The
# chart supplies one at runtime through /config.js, so one image serves every
# deployment. See deploy/helm/phase-server/README.md.

cd "$(dirname "$0")/.."

push=()
case "${1:-}" in
  --push) push=(--push) ;;
  "") ;;
  *) echo "usage: $0 [--push]" >&2; exit 2 ;;
esac

# Fail before the build, not after it. Packaging needs a docker-container
# builder: the default "docker" driver cannot do multi-platform at all, and even
# with a container builder a two-platform result has nowhere to go locally —
# docker's image store holds one platform, so without --push buildx leaves the
# result in the build cache and exits 0 having produced nothing. Both of those
# are ~25 minutes of wasm and vite away if we only find out at the end.
if [ -n "${IMAGE:-}" ]; then
  if [ ${#push[@]} -eq 0 ]; then
    echo "ERROR: IMAGE is set but --push was not passed." >&2
    echo "  A two-platform image cannot be loaded into the local docker image store," >&2
    echo "  so the build would produce nothing. Re-run with --push, or unset IMAGE to" >&2
    echo "  build client/dist only." >&2
    exit 2
  fi
  driver=$(docker buildx inspect 2>/dev/null | awk '/^Driver:/ { print $2 }')
  if [ "$driver" != "docker-container" ]; then
    echo "ERROR: the active buildx builder uses the '${driver:-unknown}' driver, which cannot" >&2
    echo "  build multi-platform images. Create one that can, then re-run:" >&2
    echo "    docker buildx create --use --driver docker-container --bootstrap" >&2
    exit 2
  fi
fi

# Reuse the upstream data plane by default: these JSONs are large, versioned
# with the card pool rather than the engine, and served with
# `access-control-allow-origin: *`. Point this at your own bucket to self-host
# them too — the manifest below drives every URL from it.
export DATA_BASE_URL="${DATA_BASE_URL:-https://data.phase-rs.dev}"
export AUDIO_BASE_URL="${AUDIO_BASE_URL:-$DATA_BASE_URL/audio}"

# card-data.json is not in data-files.json — the deployed copies are
# content-addressed per release, so it is resolved on its own. A locally
# generated one (./scripts/gen-card-data.sh) is exactly the pool this checkout's
# engine parses, so prefer it and let it ship in the bundle; otherwise fall back
# to the shared copy, which tracks upstream's pool rather than yours.
if [ -f client/public/card-data.json ]; then
  echo "card data: bundling client/public/card-data.json (matches this checkout)"
else
  # Resolve the pool through card-data-meta.json rather than naming a file.
  # The deployed pools are content-addressed, and the unversioned card-data.json
  # is a stale relic that still answers 200 — pinning it hands the locally built
  # engine a pool from an older schema, which serde rejects at load. That failure
  # is swallowed into a console warning, so it reaches the user much later and in
  # disguise, as "Card database not loaded" from the engine worker.
  if [ -z "${CARD_DATA_URL:-}" ]; then
    meta_url="$DATA_BASE_URL/card-data-meta.json"
    if ! meta=$(curl -fsSL --connect-timeout 10 --max-time 60 "$meta_url"); then
      echo "ERROR: could not fetch $meta_url to resolve the card pool." >&2
      echo "  Set CARD_DATA_URL to a content-addressed card-data-<hash>.json, or" >&2
      echo "  generate a pool matching this checkout: ./scripts/gen-card-data.sh" >&2
      exit 2
    fi
    if ! card_data_file=$(printf '%s' "$meta" | jq -er '
      if (.data_filename | type) == "string"
        and (.data_filename | test("^card-data-[0-9a-f]{16}\\.json$"))
      then .data_filename
      else error("invalid data_filename")
      end
    '); then
      echo "ERROR: $meta_url has no valid content-addressed .data_filename." >&2
      echo "  Set CARD_DATA_URL to a content-addressed card-data-<hash>.json, or" >&2
      echo "  generate a pool matching this checkout: ./scripts/gen-card-data.sh" >&2
      exit 2
    fi
    export CARD_DATA_URL="$DATA_BASE_URL/$card_data_file"
    # The pool matches the commit it was generated from, not necessarily this
    # checkout. Print it so a schema mismatch is visible here rather than as a
    # runtime load failure in the browser.
    echo "card data: $CARD_DATA_URL"
    echo "  (upstream pool generated at commit $(printf '%s' "$meta" | jq -r '.commit_short // "unknown"'); \
run ./scripts/gen-card-data.sh to bundle one matching this checkout instead)"
  else
    echo "card data: $CARD_DATA_URL (caller-supplied)"
  fi
fi

# ENGINE_WASM_URL is deliberately left unset so the engine is bundled locally
# rather than pinned to an external object — a self-hosted site should not
# depend on someone else's CDN to start a game.
./scripts/build-wasm.sh release

[ -d client/node_modules ] || (cd client && pnpm install --frozen-lockfile)
(cd client && pnpm build)

# Strip what DATA_BASE_URL now points elsewhere, so the image never double-ships
# those bytes. data-files.json is the single source of truth for the set, the
# same way release.yml's strip step uses it.
shopt -s nullglob
while IFS= read -r f; do
  rm -f "client/dist/$f" "client/dist/$f.br"
done < <(jq -r '.[]' data-files.json)
if [ -n "${CARD_DATA_URL:-}" ]; then
  rm -f client/dist/card-data.json client/dist/card-data.json.br
fi

echo "client/dist: $(du -sh client/dist | cut -f1)"

[ -n "${IMAGE:-}" ] || { echo "set IMAGE=<repo>:<tag> to package it"; exit 0; }

# amd64 is not optional: self-hosters run this on whatever they have, and a
# single-arch manifest is an image most of them cannot pull. The Dockerfile is
# RUN-free precisely so both architectures build here without qemu.
docker buildx build \
  --platform linux/amd64,linux/arm64 \
  -f deploy/phase-web.Dockerfile \
  -t "$IMAGE" \
  "${push[@]}" \
  client

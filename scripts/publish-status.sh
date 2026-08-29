#!/usr/bin/env bash
#
# Publish the operator status message consumed by `client/src/services/status.ts`.
#
# The payload is deliberately NOT part of any deploy: `data-files.json` (repo ROOT,
# not client/public) drives the upload loop in BOTH `.github/workflows/deploy.yml`
# (staging, on CI completion on main) and `release.yml` (production), and that loop
# also hard-errors when a listed file is missing from client/public. A status message
# routed through that manifest would therefore be silently clobbered on every deploy.
# This script is the only writer.
#
set -euo pipefail

# Anchor at the repo root: `client/public/status.json` and every
# `(cd client && pnpm wrangler ...)` invocation below are repo-root-relative.
cd "$(dirname "$0")/.."

R2_BUCKET="phase-rs-data"
PREVIEW_KEY="$R2_BUCKET/staging/status.json"
RELEASE_KEY="$R2_BUCKET/status.json"
LOCAL_FILE="client/public/status.json"
CACHE_CONTROL="public, max-age=60, must-revalidate"

usage() {
  cat <<'EOF'
Usage: scripts/publish-status.sh --severity <s> --title <t> --body <b> [options]
       scripts/publish-status.sh --clear [--channel <c>] [--dry-run]

Publishes the operator status banner shown on / and /multiplayer.

Channels (--channel, default: local)
  local     client/public/status.json          served by `pnpm dev` at /status.json
  preview   phase-rs-data/staging/status.json  the PREVIEW site's data prefix
  release   phase-rs-data/status.json          production
  both      preview first, then release, with an identical payload and id

  NOTE: `--channel preview` writes the "staging/" prefix, NOT the unrelated
  "preview/" prefix that CI uses for per-PR preview data. "staging/" is what the
  preview site's DATA_BASE_URL resolves to.

Required for a publish
  --severity info|warning|critical
  --title <text>              One line. Whitespace-only is rejected.
  --body <text>               ONE PARAGRAPH — the banner renders it in a <p>
                              without whitespace-pre-line, so embedded newlines
                              do not display as breaks.

Options
  --until <ISO 8601>          Expiry instant; must parse under JS Date.parse.
                              Omitted => shows until cleared.
  --link <url>                http:// or https:// only. Requires --link-label.
  --link-label <text>         Requires --link.
  --dismissible               Force a dismiss button.
  --no-dismissible            Force no dismiss button.
                              Default: true, except --severity critical => false.
  --clear                     Remove the published message from the channel.
                              Cannot be combined with any payload flag.
  --dry-run                   Print what would happen; write nothing, call nothing.
  -h, --help                  This text.

Every publish mints a fresh epoch-ms `id`, and dismissal is compared by EQUALITY,
so re-running re-shows the banner to every player who had already dismissed it.
EOF
}

# Runtime failure: message only. Deliberately NOT `die` — a partial-channel
# failure's recovery instruction is the most important line the operator will
# read, and appending the ~39-line usage block would scroll it off screen.
fail() {
  echo "ERROR: $*" >&2
  exit 1
}

die() {
  echo "ERROR: $*" >&2
  echo >&2
  usage >&2
  exit 1
}

CHANNEL="local"
SEVERITY=""
TITLE=""
BODY=""
UNTIL=""
URL=""
LABEL=""
DISMISSIBLE=""
CLEAR=false
DRY_RUN=false

while [ $# -gt 0 ]; do
  case "$1" in
    --channel) CHANNEL="${2:-}"; [ -n "$CHANNEL" ] || die "--channel requires a value"; shift 2 ;;
    --severity) SEVERITY="${2:-}"; [ -n "$SEVERITY" ] || die "--severity requires a value"; shift 2 ;;
    --title) TITLE="${2:-}"; [ -n "$TITLE" ] || die "--title requires a value"; shift 2 ;;
    --body) BODY="${2:-}"; [ -n "$BODY" ] || die "--body requires a value"; shift 2 ;;
    --until) UNTIL="${2:-}"; [ -n "$UNTIL" ] || die "--until requires a value"; shift 2 ;;
    --link) URL="${2:-}"; [ -n "$URL" ] || die "--link requires a value"; shift 2 ;;
    --link-label) LABEL="${2:-}"; [ -n "$LABEL" ] || die "--link-label requires a value"; shift 2 ;;
    --dismissible) DISMISSIBLE=true; shift ;;
    --no-dismissible) DISMISSIBLE=false; shift ;;
    --clear) CLEAR=true; shift ;;
    --dry-run) DRY_RUN=true; shift ;;
    -h|--help) usage; exit 0 ;;
    *) die "unknown flag '$1'" ;;
  esac
done

case "$CHANNEL" in
  local|preview|release|both) ;;
  *) die "--channel must be one of local, preview, release, both (got '$CHANNEL')" ;;
esac

# --- Clear takes an early branch: it carries no payload to validate ------------

clear_local() {
  if [ "$DRY_RUN" = true ]; then
    echo "rm -f $LOCAL_FILE"
    return 0
  fi
  rm -f "$LOCAL_FILE"
  echo "Cleared $LOCAL_FILE"
}

# `--remote` and `--force` are both load-bearing: without --remote the delete hits
# the local emulator and the LIVE object survives, so the operator believes a stale
# banner is gone while every player still sees it. --force skips the confirmation
# prompt. Precedent: .github/workflows/preview-server.yml:431.
clear_remote() {
  local key="$1"
  if [ "$DRY_RUN" = true ]; then
    echo "(cd client && pnpm wrangler r2 object delete \"$key\" --remote --force)"
    return 0
  fi
  (cd client && pnpm wrangler r2 object delete "$key" --remote --force) || return 1
  # Mirror the publish path's read-back. --remote alone only proves we did not
  # address the local emulator; it does not prove the live object is GONE, and a
  # stale banner the operator believes they took down is the failure this whole
  # script treats as worst. The get must now FAIL.
  if (cd client && pnpm wrangler r2 object get "$key" --remote --pipe) >/dev/null 2>&1; then
    echo "ERROR: $key still readable after delete; it was NOT cleared" >&2
    return 1
  fi
  echo "Cleared $key"
}

if [ "$CLEAR" = true ]; then
  [ -z "$SEVERITY$TITLE$BODY$UNTIL$URL$LABEL$DISMISSIBLE" ] ||
    die "--clear cannot be combined with a payload flag"

  case "$CHANNEL" in
    local) clear_local ;;
    preview) clear_remote "$PREVIEW_KEY" ;;
    release) clear_remote "$RELEASE_KEY" ;;
    both)
      # Inverse of the publish order: for a takedown the failure that matters is
      # the one that leaves PRODUCTION stale, so release goes first.
      clear_remote "$RELEASE_KEY" ||
        fail "release clear failed; nothing was cleared (preview NOT attempted)"
      clear_remote "$PREVIEW_KEY" ||
        fail "preview clear failed; cleared so far: release"
      ;;
  esac
  exit 0
fi

# --- Validate the payload BEFORE any write or network call --------------------
#
# Every check below mirrors `fetchStatus`'s reject-the-whole-message rule: without
# it an operator gets a well-formed file the client discards ENTIRELY — no banner
# and no error anywhere.

case "$SEVERITY" in
  info|warning|critical) ;;
  "") die "--severity is required (info, warning, or critical)" ;;
  *) die "--severity must be one of info, warning, critical (got '$SEVERITY')" ;;
esac

[ -n "$TITLE" ] || die "--title is required"
# The landed validator rejects via `title.trim().length === 0`, so a
# whitespace-only title is the same silent-discard shape as an absent one.
[ -n "${TITLE//[[:space:]]/}" ] || die "--title must not be whitespace-only"
[ -n "$BODY" ] || die "--body is required"
[ -n "${BODY//[[:space:]]/}" ] || die "--body must not be whitespace-only"

# All-or-nothing. `isStatusLink` accepts ANY string, so a bad-scheme URL publishes
# a banner whose button `openExternal` then inertly refuses — a dead control with
# no error anywhere.
if [ -n "$URL" ] || [ -n "$LABEL" ]; then
  [ -n "$URL" ] || die "--link-label requires --link"
  [ -n "$LABEL" ] || die "--link requires --link-label"
  [ -n "${LABEL//[[:space:]]/}" ] ||
    die "--link-label must not be whitespace-only; it would render an invisible clickable button"
  # Defer to the SAME rule the client's single URL authority uses. A prefix glob
  # is laxer: `https://` and `http://a b` pass it, pass the payload validator
  # (isStatusLink only checks `typeof === "string"`), render a clickable button —
  # and are then refused by isOpenableExternalUrl, which parses with `new URL()`.
  # That dead button with no error anywhere is precisely what this guard exists
  # to prevent, so the guard must not be more permissive than the authority.
  node -e 'try{const u=new URL(process.argv[1]);process.exit(u.protocol==="http:"||u.protocol==="https:"?0:1)}catch{process.exit(1)}' "$URL" ||
    die "--link must be a URL openExternal accepts — http/https with a valid host (got '$URL')"
fi

# Node is the only oracle with the same acceptance as the client's `Date.parse`:
# BSD `date -d` does not exist on darwin, and every substitute accepts a different
# set of strings — which is the exact mismatch this check exists to close.
# The `if` guard is load-bearing: UNTIL is "" when the flag is omitted and
# `Date.parse("")` is NaN, so an unguarded check would fail every publish.
if [ -n "$UNTIL" ]; then
  # Two distinct failures, because "accepted by the validator" is not the same as
  # "will ever render": a past expiry passes isStatusMessage, uploads, reads back
  # clean, and prints a full success line for a banner isStatusLive hides forever.
  set +e
  node -e 'const t=Date.parse(process.argv[1]); process.exit(Number.isNaN(t) ? 1 : (t <= Date.now() ? 2 : 0))' "$UNTIL"
  until_rc=$?
  set -e
  [ "$until_rc" = 1 ] && die "--until '$UNTIL' is not parseable by Date.parse"
  [ "$until_rc" = 2 ] && die "--until '$UNTIL' is already in the past; the message would never render"
fi

if [ -z "$DISMISSIBLE" ]; then
  if [ "$SEVERITY" = critical ]; then DISMISSIBLE=false; else DISMISSIBLE=true; fi
fi

# --- Compose the payload ONCE -------------------------------------------------
#
# One composition means `--channel both` publishes an identical payload and id by
# construction; re-composing per channel would mint a second id and re-show the
# banner to everyone who had dismissed it on the other channel.
#
# The temp file is ABSOLUTE because the upload runs inside `(cd client && ...)`,
# where a repo-root-relative --file path would resolve against client/ and fail.
# Never write client/public/status.json for a remote publish: a leftover there is
# copied into dist/ by a later local build.
ID=$(( $(date +%s) * 1000 ))  # `date +%s%3N` is GNU-only; darwin emits a literal 3N
TMP="$(mktemp "${TMPDIR:-/tmp}/status-payload.XXXXXX")"
trap 'rm -f "$TMP"' EXIT

# Optional fields are OMITTED, never null: the validator rejects the WHOLE message
# on `"link": null` / `"expiresAt": null`, which is what jq emits for a null arg.
# `--argjson` (not `--arg`) for id and dismissible — `--arg` is string-valued and
# the contract requires a JSON number and a JSON boolean.
jq -n --argjson id "$ID" --arg severity "$SEVERITY" --arg title "$TITLE" \
      --arg body "$BODY" --argjson dismissible "$DISMISSIBLE" \
      --arg until "$UNTIL" --arg url "$URL" --arg label "$LABEL" \
  '{id:$id, severity:$severity, title:$title, body:$body, dismissible:$dismissible}
   + (if $until == "" then {} else {expiresAt:$until} end)
   + (if $url   == "" then {} else {link:{url:$url,label:$label}} end)' > "$TMP"

# --- Publish ------------------------------------------------------------------

publish_local() {
  if [ "$DRY_RUN" = true ]; then
    # No wrangler argv exists for this channel, so the composed payload is what
    # there is to show — and nothing is written.
    cat "$TMP"
    return 0
  fi
  cp "$TMP" "$LOCAL_FILE"
  echo "Wrote $LOCAL_FILE (id $ID)"
  echo
  echo "  Load / or /multiplayer under \`cd client && pnpm dev\` to see it."
  echo "  Promotion path: local -> preview -> release"
  echo "    scripts/publish-status.sh --channel preview ...   (staging/status.json)"
  echo "    scripts/publish-status.sh --channel release ...   (status.json)"
  echo "  Run \`scripts/publish-status.sh --clear --channel local\` before any local"
  echo "  \`pnpm build\` / Tauri bundle: Vite copies client/public into dist/, so a"
  echo "  leftover test payload would be baked into the artifact."
}

publish_remote() {
  local key="$1" body
  if [ "$DRY_RUN" = true ]; then
    echo "(cd client && pnpm wrangler r2 object put \"$key\" --file \"$TMP\" --remote --content-type application/json --cache-control \"$CACHE_CONTROL\")"
    echo "(cd client && pnpm wrangler r2 object get \"$key\" --remote --pipe) | jq -e --argjson id $ID '.id == \$id'"
    return 0
  fi
  # Uncompressed: the payload is ~300 bytes, so brotli would add a dependency for
  # no gain. `pnpm wrangler` from client/ (not npx) — a maintainer's shell has no
  # guaranteed node env; precedent scripts/deploy-cf.sh:69,93,158.
  (cd client && pnpm wrangler r2 object put "$key" --file "$TMP" --remote \
    --content-type application/json \
    --cache-control "$CACHE_CONTROL") || return 1

  # Read back through the BUCKET and assert payload IDENTITY, not reachability. A
  # public-URL curl cannot serve as the emulator guard: data.phase-rs.dev returns
  # cf-cache-status: HIT under this very max-age=60 even for a no-cache client, so
  # on any publish after the first a 200 can come from the edge holding the
  # PREVIOUS message. The existing curl precedents in deploy.yml/release.yml are
  # sound only because they verify content-hash-named objects; status.json is the
  # first mutable fixed-key object in this bucket.
  body="$(cd client && pnpm wrangler r2 object get "$key" --remote --pipe)" || return 1
  printf '%s' "$body" | jq -e --argjson id "$ID" '.id == $id' >/dev/null || {
    echo "ERROR: $key did not land remotely (or is not the payload just composed)" >&2
    echo "  read-back returned: $body" >&2
    return 1
  }
  echo "Published $key (id $ID)"
}

case "$CHANNEL" in
  local) publish_local ;;
  preview) publish_remote "$PREVIEW_KEY" ;;
  release) publish_remote "$RELEASE_KEY" ;;
  both)
    # Preview first: a preview failure must leave production untouched. Do NOT
    # advise "just re-run" on a partial failure — a re-run mints a NEW id and,
    # because dismissal is equality-compared, re-shows the banner to every player
    # who had already dismissed the channel that DID land.
    publish_remote "$PREVIEW_KEY" ||
      fail "preview publish failed; nothing was published (release NOT attempted)"
    publish_remote "$RELEASE_KEY" ||
      fail "release publish failed; published so far: preview (id $ID). Re-run with --channel release --title ... to finish; a full re-run would mint a new id and re-show the banner to players who dismissed the preview one."
    ;;
esac

# shellcheck shell=bash
# Shared hardened fetch helpers for the gen-scryfall-*.sh scripts.
#
# Scryfall's API is fronted by Cloudflare, which throttles bursty or
# anonymous-looking traffic by returning a NON-JSON body (e.g.
# "error code: 1015") while a bare `curl -s` still treats the request as
# successful (no --fail). Piping that body into jq fails with
#   jq: parse error: Invalid numeric literal at line 1, column N
# and takes the whole build down. The failure is transient, which is why a
# rerun "fixes" it.
#
# These helpers close that gap: they fail-fast on HTTP errors, retry transient
# throttles (429 / 5xx / 1015) with backoff, send the User-Agent + Accept
# headers Scryfall's API guidelines ask for (anonymous traffic is throttled
# harder), and validate that a downloaded file is real JSON before any
# downstream jq transform touches it.
#
# Source this file; do not execute it. Callers keep their own `set -euo
# pipefail`, and a non-zero return here propagates as a fail-fast exit.

# Custom UA + explicit Accept per Scryfall API guidelines; --retry-all-errors
# (curl >= 7.71) retries the Cloudflare throttle bodies that --retry alone
# would skip because they can arrive with a non-5xx status.
SCRYFALL_CURL=(
  curl --fail --retry 5 --retry-all-errors --retry-delay 2
  --connect-timeout 30 -sSL
  -H 'User-Agent: phase-rs-card-data/1.0 (+https://github.com/phase-rs/phase)'
  -H 'Accept: application/json'
)

# scryfall_validate_json FILE — true iff FILE parses as JSON. Pure check; the
# caller owns cleanup. Guards against a throttled/truncated body reaching a
# downstream jq transform as a cryptic parse error.
scryfall_validate_json() {
  jq -e 'type' "$1" >/dev/null 2>&1
}

# scryfall_download URL FILE — download URL with retries to a unique temp,
# validate it, then atomically rename into place. The temp+rename keeps
# concurrent writers (setup.sh fetches default-cards.json from two scripts at
# once) and interrupted/throttled downloads from corrupting or clobbering a
# good FILE — readers only ever see the old or new complete file.
scryfall_download() {
  local url="$1" file="$2" tmp
  tmp=$(mktemp "${file}.XXXXXX")
  if ! "${SCRYFALL_CURL[@]}" -o "$tmp" "$url"; then
    rm -f "$tmp"
    return 1
  fi
  if ! scryfall_validate_json "$tmp"; then
    echo "scryfall: download of $url is not valid JSON (throttled or truncated?)" >&2
    rm -f "$tmp"
    return 1
  fi
  mv -f "$tmp" "$file"
}

# jq prelude shared by the gen-scryfall-*.sh transforms. Prepend it to a jq
# program with shell string adjacency: jq -c "$SCRYFALL_JQ_PRELUDE"'<program>'.
#
# js_downcase replicates JavaScript's String.prototype.toLowerCase() for the
# character set MTG card names use. jq's built-in ascii_downcase only folds A-Z
# (it leaves every byte >= 0x80 untouched), so an accented capital like É
# (U+00C9) survives as-is in the generated lookup keys. But the frontend
# resolves every image by `name.toLowerCase()` / `faceName.toLowerCase()`, and
# JS folds É -> é (U+00E9). The two byte strings never match, so name-keyed
# image lookups for Éomer / Éowyn (and any card with an uppercase accented
# letter) silently miss. js_downcase folds ASCII via ascii_downcase, then the
# Latin-1 Supplement uppercase block (À..Þ = U+00C0..U+00DE, excluding the
# × sign at U+00D7) by +0x20 — the complete set of accented capitals in MTG's
# English card names, verified to match JS toLowerCase byte-for-byte.
SCRYFALL_JQ_PRELUDE='
def js_downcase:
  ascii_downcase
  | explode
  | map(if . >= 192 and . <= 222 and . != 215 then . + 32 else . end)
  | implode;
'

# scryfall_fetch_bulk TYPE FILE — resolve a bulk-data download_uri by type
# (e.g. oracle_cards, default_cards) and download it to FILE.
scryfall_fetch_bulk() {
  local type="$1" file="$2" uri
  uri=$("${SCRYFALL_CURL[@]}" "https://api.scryfall.com/bulk-data" \
    | jq -r --arg t "$type" '.data[] | select(.type == $t) | .download_uri') \
    || return 1
  if [ -z "$uri" ] || [ "$uri" = "null" ]; then
    echo "scryfall: no download_uri for bulk-data type '$type'" >&2
    return 1
  fi
  scryfall_download "$uri" "$file"
}

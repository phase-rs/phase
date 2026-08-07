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

# scryfall_finalize_download TMP FILE VALIDATOR — atomically install TMP, or
# accept an existing valid FILE when a concurrent writer wins a Windows rename
# race. On an actual failure, preserve the move diagnostic for the caller.
scryfall_finalize_download() {
  local tmp="$1" file="$2" validator="$3" move_error
  move_error=$(mktemp "${file}.mv-error.XXXXXX")
  if mv -f "$tmp" "$file" 2>"$move_error"; then
    rm -f "$move_error"
    return 0
  fi
  if [ -f "$file" ] && ( "$validator" "$file" ); then
    rm -f "$tmp" "$move_error"
    return 0
  fi
  echo "scryfall: could not rename $tmp into $file:" >&2
  cat "$move_error" >&2
  rm -f "$tmp" "$move_error"
  return 1
}

# scryfall_download URL FILE [VALIDATOR] — download URL with retries to a
# unique temp, validate it, then atomically rename into place. The temp+rename
# keeps concurrent writers (setup.sh fetches default-cards.json from two
# scripts at once) and interrupted/throttled downloads from corrupting or
# clobbering a good FILE — readers only ever see the old or new complete file.
#
# VALIDATOR is the name of a function to run instead of the default
# scryfall_validate_json when deciding whether a downloaded/pre-existing FILE
# is trustworthy enough to keep (e.g. a caller that needs a specific
# top-level key, not just "is this JSON"). It gates both the common path --
# the freshly-downloaded tmp file BEFORE the mv into FILE, so a body that
# fails it never lands where a later non-validating reader would trust it --
# and the mv-failure recovery path below, which re-checks a pre-existing
# FILE left behind by a concurrent writer. scryfall_validate_json itself
# always gates the freshly-downloaded tmp file first -- that check exists to
# catch a throttled/truncated Cloudflare body, a transport-level concern
# independent of the caller's semantic shape.
scryfall_download() {
  local url="$1" file="$2" validator="${3:-scryfall_validate_json}" tmp
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
  # Caller-supplied semantic validation runs on the tmp file, before the mv.
  # Skipped for the default validator: the identical bytes already passed the
  # identical check above, and default_cards.json-sized bodies make a second
  # full jq parse measurably wasteful. Run in a subshell for the same
  # scope-isolation reason as the recovery path below.
  if [ "$validator" != scryfall_validate_json ] && ! ( "$validator" "$tmp" ); then
    echo "scryfall: download of $url failed $validator" >&2
    rm -f "$tmp"
    return 1
  fi
  # POSIX rename silently replaces FILE even if another process has it open,
  # while Windows can reject the losing writer. The shared finalizer preserves
  # that concurrent-writer recovery for both JSON and JSONL bulk downloads.
  scryfall_finalize_download "$tmp" "$file" "$validator"
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

# scryfall_download_jsonl_gzip URL FILE [VALIDATOR] — download Scryfall's gzip-compressed
# JSON Lines bulk format, validate its card-object records, and stream it into
# the JSON array the generators consume. The temp+rename has the same atomicity
# guarantee and optional semantic validator as scryfall_download.
scryfall_download_jsonl_gzip() {
  local url="$1" file="$2" validator="${3:-scryfall_validate_json}" archive tmp
  archive=$(mktemp "${file}.jsonl.gz.XXXXXX")
  tmp=$(mktemp "${file}.XXXXXX")
  if ! "${SCRYFALL_CURL[@]}" -o "$archive" "$url"; then
    rm -f "$archive" "$tmp"
    return 1
  fi
  if ! gzip -dc "$archive" \
    | jq -ce 'if .object == "card" then . else error("Scryfall JSONL bulk data must contain card objects") end' \
    | awk 'BEGIN { print "[" } NR > 1 { printf "," } { print } END { print "]" }' > "$tmp"; then
    echo "scryfall: download of $url is not valid gzip-compressed JSON Lines" >&2
    rm -f "$archive" "$tmp"
    return 1
  fi
  if [ "$validator" != scryfall_validate_json ] && ! ( "$validator" "$tmp" ); then
    echo "scryfall: download of $url failed $validator" >&2
    rm -f "$archive" "$tmp"
    return 1
  fi
  rm -f "$archive"
  scryfall_finalize_download "$tmp" "$file" "$validator"
}

# scryfall_fetch_bulk TYPE FILE [VALIDATOR] — resolve and download a bulk-data export by
# type (e.g. oracle_cards, default_cards). Prefer Scryfall's legacy JSON array
# download when present; otherwise normalize its JSON Lines export to that
# same array shape for existing generators. VALIDATOR is applied to either
# completed output shape before it is promoted.
scryfall_fetch_bulk() {
  local type="$1" file="$2" validator="${3:-scryfall_validate_json}" metadata uri jsonl_uri
  metadata=$("${SCRYFALL_CURL[@]}" "https://api.scryfall.com/bulk-data" \
    | jq -cer --arg t "$type" '.data[] | select(.type == $t) | {
      download_uri,
      jsonl_download_uri
    }') \
    || return 1
  uri=$(jq -r '.download_uri // empty' <<< "$metadata")
  if [ -n "$uri" ]; then
    scryfall_download "$uri" "$file" "$validator"
    return
  fi
  jsonl_uri=$(jq -r '.jsonl_download_uri // empty' <<< "$metadata")
  if [ -n "$jsonl_uri" ]; then
    scryfall_download_jsonl_gzip "$jsonl_uri" "$file" "$validator"
    return
  fi
  echo "scryfall: no bulk download URI for type '$type'" >&2
  return 1
}

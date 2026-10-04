#!/usr/bin/env bash
set -euo pipefail

ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ORACLE_GEN_BIN="${ORACLE_GEN_BIN:-$ROOT_DIR/target/tool/oracle-gen}"
COVERAGE_REPORT_BIN="${COVERAGE_REPORT_BIN:-$ROOT_DIR/target/tool/coverage-report}"
FIXTURE="$ROOT_DIR/data/mtgjson/test_fixture.json"

if [[ ! -x "$ORACLE_GEN_BIN" || ! -x "$COVERAGE_REPORT_BIN" ]]; then
  echo "Build oracle-gen and coverage-report before running this test." >&2
  exit 2
fi

tmpdir="$(mktemp -d)"
trap 'rm -rf "$tmpdir"' EXIT

mkdir -p "$tmpdir/default/data/mtgjson"
cp "$FIXTURE" "$tmpdir/default/data/mtgjson/AtomicCards.json"

# B is a valid corpus with different bytes and a different generated export.
jq '.data["Lightning Bolt"][0].manaCost = "{1}{R}"' "$FIXTURE" >"$tmpdir/corpus-b.json"

sha256() {
  shasum -a 256 "$1" | awk '{print $1}'
}

hash_a="$(sha256 "$tmpdir/default/data/mtgjson/AtomicCards.json")"
hash_b="$(sha256 "$tmpdir/corpus-b.json")"
[[ "$hash_a" != "$hash_b" ]]

# H: an explicit --mtgjson override wins over the default data-root input.
mkdir -p "$tmpdir/override"
"$ORACLE_GEN_BIN" "$tmpdir/default/data" \
  --mtgjson "$tmpdir/corpus-b.json" \
  --output "$tmpdir/override/card-data.json" \
  --provenance-out "$tmpdir/override/card-data.provenance.json" \
  --names-out "$tmpdir/override/card-names.json" >/dev/null
[[ "$(jq -r '.source_corpus_sha256' "$tmpdir/override/card-data.provenance.json")" == "$hash_b" ]]
[[ "$(jq -r '.source_corpus_sha256' "$tmpdir/override/card-data.provenance.json")" != "$hash_a" ]]
[[ "$(jq -r '.card_data_sha256' "$tmpdir/override/card-data.provenance.json")" == "$(sha256 "$tmpdir/override/card-data.json")" ]]

# K: the default path still records the exact AtomicCards.json bytes.
mkdir -p "$tmpdir/default/output"
"$ORACLE_GEN_BIN" "$tmpdir/default/data" \
  --output "$tmpdir/default/output/card-data.json" \
  --provenance-out "$tmpdir/default/output/card-data.provenance.json" >/dev/null
[[ "$(jq -r '.source_corpus_sha256' "$tmpdir/default/output/card-data.provenance.json")" == "$hash_a" ]]

# I: replacing the nearby default corpus after generation cannot change the
# source identity emitted from the override provenance.
mkdir -p "$tmpdir/identity/data/mtgjson"
cp "$tmpdir/override/card-data.json" "$tmpdir/identity/data/card-data.json"
cp "$tmpdir/override/card-data.provenance.json" \
  "$tmpdir/identity/data/card-data.provenance.json"
cp "$tmpdir/default/data/mtgjson/AtomicCards.json" \
  "$tmpdir/identity/data/mtgjson/AtomicCards.json"
"$COVERAGE_REPORT_BIN" "$tmpdir/identity/data" --brief >"$tmpdir/identity/coverage.json"
[[ "$(jq -r '.source_corpus_hash' "$tmpdir/identity/coverage.json")" == "$hash_b" ]]

# Output/provenance destinations must be distinct before either file is written.
mkdir -p "$tmpdir/collision/relative"
printf 'sentinel\n' >"$tmpdir/collision/card-data.json"
if "$ORACLE_GEN_BIN" "$tmpdir/default/data" \
  --mtgjson "$tmpdir/corpus-b.json" \
  --output "$tmpdir/collision/card-data.json" \
  --provenance-out "$tmpdir/collision/card-data.json" \
  >"$tmpdir/collision/literal.out" 2>"$tmpdir/collision/literal.err"; then
  echo "expected literal output/provenance collision to fail" >&2
  exit 1
fi
grep -Fq -- '--output and --provenance-out must be different paths' \
  "$tmpdir/collision/literal.err"
[[ "$(cat "$tmpdir/collision/card-data.json")" == 'sentinel' ]]

if "$ORACLE_GEN_BIN" "$tmpdir/default/data" \
  --mtgjson "$tmpdir/corpus-b.json" \
  --output "$tmpdir/collision/relative/card-data.json" \
  --provenance-out "$tmpdir/collision/relative/./card-data.json" \
  >"$tmpdir/collision/relative.out" 2>"$tmpdir/collision/relative.err"; then
  echo "expected relative output/provenance collision to fail" >&2
  exit 1
fi
grep -Fq -- '--output and --provenance-out must be different paths' \
  "$tmpdir/collision/relative.err"
[[ ! -e "$tmpdir/collision/relative/card-data.json" ]]

if ln -s relative "$tmpdir/collision/alias" 2>/dev/null; then
  if "$ORACLE_GEN_BIN" "$tmpdir/default/data" \
    --mtgjson "$tmpdir/corpus-b.json" \
    --output "$tmpdir/collision/relative/card-data-symlink.json" \
    --provenance-out "$tmpdir/collision/alias/card-data-symlink.json" \
    >"$tmpdir/collision/symlink.out" 2>"$tmpdir/collision/symlink.err"; then
    echo "expected symlink-equivalent output/provenance collision to fail" >&2
    exit 1
  fi
  grep -Fq -- '--output and --provenance-out must be different paths' \
    "$tmpdir/collision/symlink.err"
  [[ ! -e "$tmpdir/collision/relative/card-data-symlink.json" ]]
fi

# J: a valid replacement card-data export with the old provenance is rejected.
if cmp -s "$tmpdir/override/card-data.json" "$tmpdir/default/output/card-data.json"; then
  echo "fixture mutation did not change generated card-data bytes" >&2
  exit 1
fi
cp "$tmpdir/default/output/card-data.json" "$tmpdir/identity/data/card-data.json"
if "$COVERAGE_REPORT_BIN" "$tmpdir/identity/data" --brief \
  >"$tmpdir/j.out" 2>"$tmpdir/j.err"; then
  echo "expected card-data provenance mismatch to fail" >&2
  exit 1
fi
grep -Fq 'CARD_DATA_PROVENANCE_MISMATCH' "$tmpdir/j.err"

# Missing provenance is a separate fail-closed error, not a fallback to the
# nearby AtomicCards.json.
cp "$tmpdir/override/card-data.json" "$tmpdir/identity/data/card-data.json"
rm "$tmpdir/identity/data/card-data.provenance.json"
if "$COVERAGE_REPORT_BIN" "$tmpdir/identity/data" --brief \
  >"$tmpdir/missing.out" 2>"$tmpdir/missing.err"; then
  echo "expected missing card-data provenance to fail" >&2
  exit 1
fi
grep -Fq 'CARD_DATA_PROVENANCE_MISSING' "$tmpdir/missing.err"

echo "card-data provenance H-K matrix: ok"

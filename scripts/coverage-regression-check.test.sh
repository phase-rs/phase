#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/coverage-regression-check.sh"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

write_pair() {
    local name="$1"
    local baseline="$2"
    local current="$3"
    printf '%s\n' "$baseline" >"$TMPDIR_TEST/${name}-baseline.json"
    printf '%s\n' "$current" >"$TMPDIR_TEST/${name}-current.json"
}

run_check() {
    local name="$1"
    "$CHECK" "$TMPDIR_TEST/${name}-baseline.json" "$TMPDIR_TEST/${name}-current.json" \
        --fail-on-engine >"$TMPDIR_TEST/${name}.out" 2>"$TMPDIR_TEST/${name}.err"
}

# A. Same external corpus, no regression: the ordinary comparison passes.
write_pair "same" \
    '{"card_data_hash":"parser-old","source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Stable","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"card_data_hash":"parser-new","source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Stable","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}'
run_check same

# B. Same corpus, real engine regression: the existing fail-on-engine path stays live.
write_pair "engine" \
    '{"source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Engine","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"source_corpus_hash":"AAA","supported_cards":0,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Engine","supported":false,"gap_details":[{"handler":"Effect:Tap"}],"parse_details":[{"category":"ability","label":"Tap","supported":false}]}]}'
if run_check engine; then
    echo "expected same-corpus engine regression to fail" >&2
    exit 1
fi
grep -Fq 'FAIL: 1 cards regressed with new engine-level gaps.' "$TMPDIR_TEST/engine.err"

# C. Same corpus, diagnostic regression: a newly affected supported card fails
# the existing diagnostic ratchet rather than being hidden by the fence.
write_pair "diagnostic" \
    '{"source_corpus_hash":"AAA","supported_cards":2,"total_cards":2,"diagnostics":{"swallowed-clause":1},"cards":[{"card_name":"Existing","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"swallowed-clause","version":1}]},{"card_name":"NewlyAffected","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"none"}]}]}' \
    '{"source_corpus_hash":"AAA","supported_cards":2,"total_cards":2,"diagnostics":{"swallowed-clause":2},"cards":[{"card_name":"Existing","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"swallowed-clause","version":1}]},{"card_name":"NewlyAffected","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"swallowed-clause","version":2}]}]}'
if run_check diagnostic; then
    echo "expected same-corpus diagnostic regression to fail" >&2
    exit 1
fi
grep -Fq 'DIAGNOSTIC REGRESSION' "$TMPDIR_TEST/diagnostic.err"

# D. Different corpus: reject before semantic classification, even when the
# payload itself contains a regression-shaped delta.
write_pair "different" \
    '{"source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{"swallowed-clause":1},"cards":[{"card_name":"Engine","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"source_corpus_hash":"BBB","supported_cards":0,"total_cards":1,"diagnostics":{"swallowed-clause":2},"cards":[{"card_name":"Engine","supported":false,"gap_details":[{"handler":"Effect:Tap"}],"parse_details":[{"category":"ability","label":"Tap","supported":false}]}]}'
if run_check different; then
    echo "expected different-corpus comparison to fail closed" >&2
    exit 1
fi
grep -Fq 'CORPUS_MISMATCH' "$TMPDIR_TEST/different.err"
if grep -Fq 'DIAGNOSTIC REGRESSION' "$TMPDIR_TEST/different.err"; then
    echo "different corpus was classified as a semantic regression" >&2
    exit 1
fi

# E. Baseline corpus identity missing: fail closed.
write_pair "missing-baseline" \
    '{"card_data_hash":"old-parser-output","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}' \
    '{"card_data_hash":"new-parser-output","source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}'
if run_check missing-baseline; then
    echo "expected missing baseline corpus identity to fail" >&2
    exit 1
fi
grep -Fq 'CORPUS_IDENTITY_MISSING' "$TMPDIR_TEST/missing-baseline.err"
grep -Fq 'CARD DATA DRIFT' "$TMPDIR_TEST/missing-baseline.err"

# F. Current corpus identity missing: fail closed.
write_pair "missing-current" \
    '{"source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}' \
    '{"card_data_hash":"new-parser-output","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}'
if run_check missing-current; then
    echo "expected missing current corpus identity to fail" >&2
    exit 1
fi
grep -Fq 'CORPUS_IDENTITY_MISSING' "$TMPDIR_TEST/missing-current.err"
grep -Fq 'CARD DATA DRIFT' "$TMPDIR_TEST/missing-current.err"

# G. Same external corpus, changed parser output: different generated artifact
# hashes do not trip the corpus fence; ordinary comparison is reached.
write_pair "parser-output" \
    '{"card_data_hash":"parser-old","source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Parser","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"OldLowering","supported":true}]}]}' \
    '{"card_data_hash":"parser-new","source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Parser","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"NewLowering","supported":true}]}]}'
run_check parser-output
grep -Fq 'Baseline supported: 1' "$TMPDIR_TEST/parser-output.out"
if grep -Eq 'CORPUS_(IDENTITY_MISSING|MISMATCH)' "$TMPDIR_TEST/parser-output.err"; then
    echo "same-corpus parser-output change was fenced as corpus drift" >&2
    exit 1
fi

echo "coverage-regression-check corpus identity A-G matrix: ok"

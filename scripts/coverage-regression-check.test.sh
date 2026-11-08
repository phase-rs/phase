#!/usr/bin/env bash
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
CHECK="$SCRIPT_DIR/coverage-regression-check.sh"
TMPDIR_TEST="$(mktemp -d)"
trap 'rm -rf "$TMPDIR_TEST"' EXIT

# Two distinct well-formed SHA-256 corpus identities. Fixtures name them as
# @CORPUS_A@ / @CORPUS_B@ so the JSON stays readable.
CORPUS_A="$(printf 'a%.0s' {1..64})"
CORPUS_B="$(printf 'b%.0s' {1..64})"

write_pair() {
    local name="$1"
    local baseline="${2//@CORPUS_A@/$CORPUS_A}"
    local current="${3//@CORPUS_A@/$CORPUS_A}"
    baseline="${baseline//@CORPUS_B@/$CORPUS_B}"
    current="${current//@CORPUS_B@/$CORPUS_B}"
    printf '%s\n' "$baseline" >"$TMPDIR_TEST/${name}-baseline.json"
    printf '%s\n' "$current" >"$TMPDIR_TEST/${name}-current.json"
}

# Fail unless the named run printed no corpus-fence or bootstrap marker.
assert_no_corpus_markers() {
    local name="$1"
    if grep -Eq 'CORPUS_(IDENTITY_MISSING|IDENTITY_MALFORMED|IDENTITY_UNVERIFIED|MISMATCH)' \
        "$TMPDIR_TEST/${name}.out" "$TMPDIR_TEST/${name}.err"; then
        echo "$name: unexpected corpus-identity marker" >&2
        exit 1
    fi
}

run_check() {
    local name="$1"
    "$CHECK" "$TMPDIR_TEST/${name}-baseline.json" "$TMPDIR_TEST/${name}-current.json" \
        --fail-on-engine >"$TMPDIR_TEST/${name}.out" 2>"$TMPDIR_TEST/${name}.err"
}

# A. Same external corpus, no regression: the ordinary comparison passes.
write_pair "same" \
    '{"card_data_hash":"parser-old","source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Stable","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"card_data_hash":"parser-new","source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Stable","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}'
run_check same
assert_no_corpus_markers same
grep -Fq 'Baseline supported: 1' "$TMPDIR_TEST/same.out"

# B. Same corpus, real engine regression: the existing fail-on-engine path stays live.
write_pair "engine" \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Engine","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":0,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Engine","supported":false,"gap_details":[{"handler":"Effect:Tap"}],"parse_details":[{"category":"ability","label":"Tap","supported":false}]}]}'
if run_check engine; then
    echo "expected same-corpus engine regression to fail" >&2
    exit 1
fi
grep -Fq 'FAIL: 1 cards regressed with new engine-level gaps.' "$TMPDIR_TEST/engine.err"

# C. Same corpus, diagnostic regression: a newly affected supported card fails
# the existing diagnostic ratchet rather than being hidden by the fence.
write_pair "diagnostic" \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":2,"total_cards":2,"diagnostics":{"swallowed-clause":1},"cards":[{"card_name":"Existing","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"swallowed-clause","version":1}]},{"card_name":"NewlyAffected","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"none"}]}]}' \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":2,"total_cards":2,"diagnostics":{"swallowed-clause":2},"cards":[{"card_name":"Existing","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"swallowed-clause","version":1}]},{"card_name":"NewlyAffected","supported":true,"gap_details":[],"parse_details":[{"diagnostic":"swallowed-clause","version":2}]}]}'
if run_check diagnostic; then
    echo "expected same-corpus diagnostic regression to fail" >&2
    exit 1
fi
grep -Fq 'DIAGNOSTIC REGRESSION' "$TMPDIR_TEST/diagnostic.err"

# D. Different corpus: warn and skip semantic classification, even when the
# payload itself contains a regression-shaped delta. Non-blocking (exit 0):
# corpus drift is routine and a hard failure would wedge the merge queue.
write_pair "different" \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{"swallowed-clause":1},"cards":[{"card_name":"Engine","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"source_corpus_hash":"@CORPUS_B@","supported_cards":0,"total_cards":1,"diagnostics":{"swallowed-clause":2},"cards":[{"card_name":"Engine","supported":false,"gap_details":[{"handler":"Effect:Tap"}],"parse_details":[{"category":"ability","label":"Tap","supported":false}]}]}'
run_check different
grep -Fq "::warning title=Coverage corpus mismatch::CORPUS_MISMATCH: baseline corpus $CORPUS_A != current corpus $CORPUS_B" \
    "$TMPDIR_TEST/different.out"
grep -Fq 'CARD DATA DRIFT' "$TMPDIR_TEST/different.err"
if grep -Eq 'DIAGNOSTIC REGRESSION|FAIL:' "$TMPDIR_TEST/different.err"; then
    echo "different corpus was classified as a semantic regression" >&2
    exit 1
fi
# A stamped baseline never takes the legacy fallback, and the semantic
# comparison is never reached across corpora.
if grep -Fq 'CORPUS_IDENTITY_UNVERIFIED' "$TMPDIR_TEST/different.out" \
    || grep -Fq 'Baseline supported:' "$TMPDIR_TEST/different.out"; then
    echo "different corpus reached the legacy fallback or the comparison" >&2
    exit 1
fi

# E. Legacy baseline (published before corpus stamping, no source_corpus_hash):
# bootstrap fallback compares exactly as before and warns that corpus identity
# could not be checked, instead of failing every PR until a stamp exists.
write_pair "legacy-baseline" \
    '{"card_data_hash":"old-parser-output","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Stable","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"card_data_hash":"new-parser-output","source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Stable","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}'
run_check legacy-baseline
grep -Fq '::warning title=Coverage corpus identity unverified::CORPUS_IDENTITY_UNVERIFIED' \
    "$TMPDIR_TEST/legacy-baseline.out"
grep -Fq 'Baseline supported: 1' "$TMPDIR_TEST/legacy-baseline.out"
if grep -Eq 'CORPUS_(IDENTITY_MISSING|MISMATCH)|CARD DATA DRIFT' "$TMPDIR_TEST/legacy-baseline.err"; then
    echo "legacy baseline was fenced instead of compared" >&2
    exit 1
fi

# E2. An explicit null is the same legacy state as an absent field.
write_pair "null-baseline" \
    '{"source_corpus_hash":null,"supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}' \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}'
run_check null-baseline
grep -Fq 'CORPUS_IDENTITY_UNVERIFIED' "$TMPDIR_TEST/null-baseline.out"

# E3. The legacy fallback is not a waiver: the ordinary engine ratchet still
# runs and fails a real regression against an unstamped baseline.
write_pair "legacy-engine" \
    '{"supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Engine","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"Tap","supported":true}]}]}' \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":0,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Engine","supported":false,"gap_details":[{"handler":"Effect:Tap"}],"parse_details":[{"category":"ability","label":"Tap","supported":false}]}]}'
if run_check legacy-engine; then
    echo "expected engine regression against a legacy baseline to fail" >&2
    exit 1
fi
grep -Fq 'CORPUS_IDENTITY_UNVERIFIED' "$TMPDIR_TEST/legacy-engine.out"
grep -Fq 'FAIL: 1 cards regressed with new engine-level gaps.' "$TMPDIR_TEST/legacy-engine.err"

# E4. A baseline that records an identity in the wrong shape is a publisher
# defect, not a legacy baseline: fail closed before comparing.
for malformed in '""' '"AAA"' '123'; do
    write_pair "malformed-baseline" \
        "{\"source_corpus_hash\":${malformed},\"supported_cards\":1,\"total_cards\":1,\"diagnostics\":{},\"cards\":[]}" \
        '{"source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}'
    if run_check malformed-baseline; then
        echo "expected malformed baseline corpus identity ${malformed} to fail" >&2
        exit 1
    fi
    grep -Fq 'CORPUS_IDENTITY_MALFORMED' "$TMPDIR_TEST/malformed-baseline.err"
    if grep -Fq 'CORPUS_IDENTITY_UNVERIFIED' "$TMPDIR_TEST/malformed-baseline.out"; then
        echo "malformed baseline identity ${malformed} took the legacy fallback" >&2
        exit 1
    fi
done

# F. Current corpus identity missing: fail closed.
write_pair "missing-current" \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}' \
    '{"card_data_hash":"new-parser-output","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}'
if run_check missing-current; then
    echo "expected missing current corpus identity to fail" >&2
    exit 1
fi
grep -Fq 'CORPUS_IDENTITY_MISSING' "$TMPDIR_TEST/missing-current.err"
grep -Fq 'CARD DATA DRIFT' "$TMPDIR_TEST/missing-current.err"

# F2. A malformed current identity is a pipeline defect: fail closed.
write_pair "malformed-current" \
    '{"source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}' \
    '{"source_corpus_hash":"AAA","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[]}'
if run_check malformed-current; then
    echo "expected malformed current corpus identity to fail" >&2
    exit 1
fi
grep -Fq 'CORPUS_IDENTITY_MALFORMED' "$TMPDIR_TEST/malformed-current.err"

# G. Same external corpus, changed parser output: different generated artifact
# hashes do not trip the corpus fence; ordinary comparison is reached.
write_pair "parser-output" \
    '{"card_data_hash":"parser-old","source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Parser","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"OldLowering","supported":true}]}]}' \
    '{"card_data_hash":"parser-new","source_corpus_hash":"@CORPUS_A@","supported_cards":1,"total_cards":1,"diagnostics":{},"cards":[{"card_name":"Parser","supported":true,"gap_details":[],"parse_details":[{"category":"ability","label":"NewLowering","supported":true}]}]}'
run_check parser-output
grep -Fq 'Baseline supported: 1' "$TMPDIR_TEST/parser-output.out"
if grep -Eq 'CORPUS_(IDENTITY_MISSING|IDENTITY_MALFORMED|IDENTITY_UNVERIFIED|MISMATCH)' "$TMPDIR_TEST/parser-output.out" "$TMPDIR_TEST/parser-output.err"; then
    echo "same-corpus parser-output change was fenced as corpus drift" >&2
    exit 1
fi

echo "coverage-regression-check corpus identity A-G matrix (with legacy-baseline bootstrap): ok"

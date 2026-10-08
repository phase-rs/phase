#!/usr/bin/env bash
# Regenerates a saved-game test fixture under crates/engine/tests/fixtures/ from
# its READ-ONLY pristine dump, stamping the `effect_kind` field that upstream
# #6718 (0468df1f4) added to `TargetSelectionSlot` without `#[serde(default)]`.
#
# THE `deck_size` STAGE. U5 typed `FormatConfig::deck_size` as `DeckSizeRule`, an
# adjacently-tagged enum (`#[serde(tag = "type", content = "data")]`,
# crates/engine/src/types/format.rs) carrying no `#[serde(default)]` and no untagged
# fallback. Every pristine dump in this corpus predates that change and holds a bare
# `"deck_size": N`, which the strict decoder REJECTS. `--deck-size
# <Minimum|Exactly>:<count>` rewrites that bare value into
# `{"type":"<variant>","data":<count>}` at EVERY depth, on the filter tail SHARED
# by the migration and both control regenerations. The predicate keys on the TRAILING KEY
# at any path, so it is not keyed on an enumeration of the depths the corpus happens to
# hold -- which is more than the self-tests below witness (see `deck_size_gate`).
#
# THE ENGINE STILL OWNS THE format->variant MAPPING, and the dump's shape does not
# predict it: CR 903.13f(1) makes Commander Draft a command-zone format with a MINIMUM
# deck size, so `command_zone` does not predict exactness. So the variant arrives as an
# OPERATOR ARGUMENT, validated for SHAPE ONLY -- an identifier, a colon, digits -- never
# against a variant list, which would re-copy an engine enum into bash. `deck_size_gate`
# below still refuses to emit an untagged artifact, so an omitted or inapplicable
# argument fails closed instead of shipping.
#
# WHAT HOLDS THE OPERATOR TO THE ARGUMENT is a tracked row, exactly as for
# `--effect-kind`: `fixture_deck_size_conformance` decodes every committed `*.json.gz`
# through `FormatConfig` and, for a non-`Custom` format, requires the persisted
# `deck_size` to equal the one `FormatConfig::for_format` defines. A wrong
# `--deck-size` argument fails that row rather than shipping.
#
# WHY A REGENERATION AND NOT A SERDE SHIM. The maintainer publicly declined both
# `#[serde(default)]` and an upstream save migration for this field
# (https://github.com/phase-rs/phase/pull/6718#issuecomment-5111207689 — "alpha
# means it may not load ... if you have a use-case then do the save changes
# locally"). Migrating the fixture locally is the maintainer's own named path,
# and it keeps the production decoder STRICT: an un-migrated save must still be
# rejected, which a serde default would silently prevent.
#
# WHY `--effect-kind` IS AN EXPLICIT ARGUMENT and never a jq name->variant table:
# such a table would re-derive `impl From<&Effect> for EffectKind` in jq, and
# that mapping is not the identity (`Effect::SetTapState` fans out to several
# kinds). The ENGINE stays the authority for the migrated value; the reading
# test beside `load_dellian_dump` in `game/engine.rs` asserts the stamped slots
# equal what `ability_utils::build_target_slots` builds for that board, so a
# wrong `--effect-kind` argument fails a tracked row rather than shipping.
#
# The pristine directory is READ-ONLY: this script only ever reads from it.
#
# Usage:
#   scripts/migrate-dump-fixture.sh \
#     --pristine  /path/to/dump.zip \
#     --expect-sha256 <sha256 of that zip> \
#     --effect-kind LoseLife \
#     --deck-size Exactly:100 \
#     --out crates/engine/tests/fixtures/name.json.gz
#
#   `--deck-size` is OPTIONAL: a post-U5 dump needs no stage, and `deck_size_gate` stays
#   the authority on whether one was needed.
#
#   # Control mode: re-run the FULL recipe and check it against the committed
#   # fixture, then check the patch had teeth. Runnable by anyone, at any time,
#   # with no engine build.
#   scripts/migrate-dump-fixture.sh --pristine ... --expect-sha256 ... \
#     --effect-kind LoseLife --deck-size Exactly:100 \
#     --out crates/engine/tests/fixtures/name.json.gz --control
#
# A PARTIAL CHECK CERTIFIES NOTHING. Each arm reports its own verdict, and a conclusion
# may only be drawn from the arms that actually RAN -- an arm reporting `n/a` has
# certified nothing, and one arm alone can pass vacuously. The arms are labelled where
# they run rather than inventoried here, because a private copy drifts from the code:
#   grep -inE '^ *# arm [0-9]' scripts/migrate-dump-fixture.sh scripts/stamp-fixture-firing.sh
#
# ⚠ ARM 1 IS BASELINED ON THE MIGRATED FIXTURE, and it has to be. The committed
# fixture IS the patched artifact; comparing an UNPATCHED regeneration against it
# fails by construction post-migration (measured: BYTE_IDENTICAL=false, exit 1),
# which reads as "the fixture is corrupt" when it means "migrated, as designed".
# So the unpatched regeneration is arm 2's operand, never arm 1's expectation.
#
# TOOLCHAIN. Byte-identity is toolchain-coupled: gzip's deflate output and jq's
# key ordering are implementation details, not standards. The recipe below was
# established under the pinned versions; on any other version the control falls
# back to a canonical `jq -S` content comparison, which is toolchain-independent
# and still discriminating (it just cannot certify byte equality).

set -euo pipefail

PINNED_JQ="jq-1.7.1"
PINNED_GZIP="gzip 1.14"

PRISTINE=""
EXPECT_SHA=""
EFFECT_KIND=""
DECK_SIZE=""
DS_TYPE=""
DS_DATA=""
OUT=""
CONTROL_MODE=0

usage() {
  awk 'NR>1 && /^#/ {sub(/^# ?/,""); print; next} NR>1 {exit}' "${BASH_SOURCE[0]}"
  exit "${1:-1}"
}

while [ $# -gt 0 ]; do
  case "$1" in
    --pristine)      PRISTINE="${2:?--pristine needs a path}"; shift 2 ;;
    --expect-sha256) EXPECT_SHA="${2:?--expect-sha256 needs a hash}"; shift 2 ;;
    --effect-kind)   EFFECT_KIND="${2:?--effect-kind needs an EffectKind variant name}"; shift 2 ;;
    --deck-size)     DECK_SIZE="${2:?--deck-size needs <Minimum|Exactly>:<count>}"; shift 2 ;;
    --out)           OUT="${2:?--out needs a path}"; shift 2 ;;
    --control)       CONTROL_MODE=1; shift ;;
    -h|--help)       usage 0 ;;
    *) echo "unknown argument: $1" >&2; usage 1 ;;
  esac
done

[ -n "$PRISTINE" ]   || { echo "missing --pristine" >&2; exit 1; }
[ -n "$EXPECT_SHA" ] || { echo "missing --expect-sha256" >&2; exit 1; }
[ -n "$OUT" ]        || { echo "missing --out" >&2; exit 1; }
# Control mode needs --effect-kind too: arm 1 re-runs the FULL recipe, patch
# included, because the committed fixture is the patched artifact.
[ -n "$EFFECT_KIND" ] || { echo "missing --effect-kind" >&2; exit 1; }
# SHAPE ONLY, never a variant list: pinning `Minimum|Exactly` here would re-copy an
# engine enum into bash and rot silently when a variant is added. A tag naming no live
# variant is `fixture_deck_size_conformance`'s business, the same division of labour
# `deck_size_gate` below already keeps.
if [ -n "$DECK_SIZE" ]; then
  if [[ ! "$DECK_SIZE" =~ ^[A-Za-z_][A-Za-z0-9_]*:[0-9]+$ ]]; then
    echo "--deck-size must be <identifier>:<digits>, e.g. Exactly:100 — got: $DECK_SIZE" >&2
    exit 1
  fi
  DS_TYPE="${DECK_SIZE%%:*}"
  DS_DATA="${DECK_SIZE#*:}"
fi

for tool in unzip jq gzip sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || { echo "required tool not found: $tool" >&2; exit 1; }
done

# 1. Verify the pristine input. Abort rather than migrate an unexpected dump —
#    fixture<->dump correspondence is by CONTENT, never by filename (the name
#    trap is real: witherbloom-sprout-lumaret-works-slow.zip maps to
#    witherbloom_sprout_lumaret_SIMPLE_4p.json.gz).
ACTUAL_SHA="$(sha256sum "$PRISTINE" | cut -d' ' -f1)"
if [ "$ACTUAL_SHA" != "$EXPECT_SHA" ]; then
  echo "pristine sha256 mismatch for $PRISTINE" >&2
  echo "  expected: $EXPECT_SHA" >&2
  echo "  actual:   $ACTUAL_SHA" >&2
  exit 1
fi

JQ_VERSION="$(jq --version)"
GZIP_VERSION="$(gzip --version | head -1)"

# 2. Patch + 3. compress. ONE filter, applied to every slot in the prompt.
#    ONE definition of the recipe, used by BOTH the migration and the control —
#    a control that re-spelled the recipe would certify its own copy.
#
# STAGE 2b — CR 603.7 firing carriers (upstream #6842, 8121fd1c6).
# The derivation lives in ONE place, scripts/lib/trigger-firing.jq, loaded by
# both this script's pristine path and its --in-place path. See that file for
# the CR 603.1 vs CR 603.7a discriminant and why UnknownLegacy is not legal.
FIRING_LIB="$(dirname "${BASH_SOURCE[0]}")/lib/trigger-firing.jq"
[ -f "$FIRING_LIB" ] || { echo "missing $FIRING_LIB" >&2; exit 1; }

# The final projection PRESERVES a non-`gameState` envelope instead of replacing it.
#
# `{gameState:.gameState}` is a REWRITE, not a projection, for any dump that is not
# `gameState`-shaped: `.gameState` is null on those, so the whole document became
# `{"gameState":null}` — the committed fixture destroyed and replaced by a one-key
# husk. That contradicted the pass-through `trigger-firing.jq` already implements for
# its own stages, and it is silent: the output is valid JSON, so nothing downstream
# objects. Several fixtures in this corpus really do use the other envelope (top level
# `turn_number`), which is why that guard exists in the first place.
#
# Keyed on PRESENCE, not on truthiness — a dump carrying an explicitly null
# `gameState` is malformed and must not be quietly normalised into the husk shape.
PROJECT='if (type == "object" and has("gameState")) then {gameState:.gameState} else . end'

# THE DECK-SIZE STAGE, and it belongs to the tail SHARED by both modes rather than to the
# patched branch. The rewrite is a SHAPE MIGRATION, not a content patch: the patched
# branch's subjects are `effect_kind` and stage 2b, and neither reads `deck_size`. Put it
# in the patched branch alone and `--control` dies at its UNPATCHED regeneration — which
# `regenerate` gates too — before arm 1 can compare anything.
#
# The predicate is `deck_size_gate`'s own walk NARROWED TO THE NUMBER CASE, so the stage
# repairs exactly the one legacy shape and every other malformed value stays the gate's
# business. It is the identity when no `--deck-size` was given, and it is idempotent on an
# already-tagged value (whose numbers sit at `.deck_size.data`, not at `.deck_size`).
DECK_SIZE_STAGE='.'
if [ -n "$DECK_SIZE" ]; then
  DECK_SIZE_STAGE='reduce (paths(type == "number") | select(.[-1] == "deck_size")) as $p
                     (.; setpath($p; {type: $ds_type, data: $ds_data}))'
fi

# Applies the recipe to stdin, writing to stdout. ONE definition of the transform, so
# the migration, the control arms, and the self-tests cannot drift apart.
transform() {   # transform <patched|unpatched>
  local mode="$1" tail filter
  tail="$DECK_SIZE_STAGE | $PROJECT"
  filter="$tail"
  if [ "$mode" = patched ]; then
    filter="(if (.gameState.waiting_for.data.target_slots? // null) != null
               then .gameState.waiting_for.data.target_slots |= map(. + {effect_kind: \$k})
               else . end) | stamp_trigger_firing | stamp_delayed_allocators | $tail"
  fi
  jq -c --arg k "$EFFECT_KIND" --arg ds_type "$DS_TYPE" --argjson ds_data "${DS_DATA:-0}" \
    -f <(printf '%s\n%s\n' "$(cat "$FIRING_LIB")" "$filter")
}

# The `effect_kind` stage applies only to a dump whose prompt actually carries
# `target_slots`. Several dumps in this corpus are paused at a beat with no target
# prompt at all; for them this stage is vacuously absent, and `--effect-kind` is
# inert. Guarding it (rather than letting `map` abort on null) is what lets ONE
# recipe cover the whole corpus — an unguarded `|=` here made the script usable only
# on the dumps that happen to have a prompt, which is why the rest were never
# regenerable through it.
#
# WRITES ATOMICALLY: stage to a temp file, `mv` only after the WHOLE pipeline
# succeeded.
#
# The struck form redirected the pipeline straight into `$dest`. The shell creates
# and TRUNCATES a redirection target before the first command in the pipeline runs,
# and on the production path `$dest` is the committed fixture (`$OUT`). The recipe
# aborts BY DESIGN — `_firing` raises `UNDETERMINED firing carrier` and
# `stamp_delayed_allocators` raises `UNDETERMINED delayed-trigger allocators` — so
# `set -e` / `pipefail` stopped the script only AFTER the fixture had already been
# truncated and a partial gzip stream written over it. The failure mode of a
# fail-closed recipe was destruction of the very artifact it refused to rewrite.
# `stamp-fixture-firing.sh` already had the right shape; this matches it.
# SINGLE DEFINITION of the staging rule, because the previous code had three copies of
# it and they drifted apart in the one way that mattered.
#
# The stage file MUST live in `dirname "$dest"`. `mv` is atomic only WITHIN a filesystem;
# across a boundary it degrades to copy-then-unlink, and an interruption mid-copy leaves
# `$dest` truncated — the exact destruction this staging exists to prevent. `mktemp -t`
# resolves to `$TMPDIR` (`/tmp` here, a separate mount from the checkout: measured
# `df --output=target` gives `/tmp` vs `/home`), so the previous form ADVERTISED atomicity
# it could not deliver, and elsewhere depended silently on the operator's mount layout.
#
# The self-tests below call this too. They used to re-spell the recipe with their own
# `mktemp -t`, which is why a self-test whose whole subject is atomicity could still pass
# against a non-atomic production path: it was exercising its own copy, and its `$tmp` and
# its destination happened to share a filesystem. A control that re-implements the thing
# it controls is not a control.
stage_path() {   # stage_path <destination> — a stage file on $destination's OWN filesystem
  mktemp "$(dirname "$1")/.migrate-dump-stage-XXXXXX.json.gz"
}

# Stage files live BESIDE their destination (stage_path, for mv atomicity), and on the
# production path that directory is the tracked `crates/engine/tests/fixtures/`. A signal
# during the unzip|transform|gzip pipeline, or a failed `mv`, would otherwise strand a
# `.migrate-dump-stage-XXXXXX.json.gz` inside version control. The trap covers what the
# explicit `rm -f` cannot: death before the next statement runs.
STAGE_FILES=""
cleanup_stage_files() {
  [ -n "$STAGE_FILES" ] || return 0
  # shellcheck disable=SC2086 # deliberate word-splitting over the staged-path list
  rm -f $STAGE_FILES
  STAGE_FILES=""
}
trap cleanup_stage_files EXIT INT TERM

# DECK-SIZE GATE. The check that no artifact leaves here still carrying the pre-U5 bare
# `N`. Without it the script exits 0 and prints its `MIGRATED` banner over an artifact
# the strict decoder cannot load (measured on genuine pre-U5 dump content: exit 0,
# `deck_size` still a bare `100`). A silent success is the worst failure shape here,
# because the artifact it blesses is a committed fixture.
#
# THE GATE AND THE STAGE ARE TWO HALVES, not alternatives. The stage above supplies the
# variant from `--deck-size`; this gate asks, of the bytes actually about to be written,
# whether the tagged shape is there — so an omitted argument, an argument that did not
# apply, or a stage that stopped matching all fail closed at the same point.
#
# THE `EffectKind` PRECEDENT IS FOLLOWED HERE, remedy and all. `EffectKind` resolved the
# same tension by PARAMETERIZING: an explicit `--effect-kind`, precisely so the ENGINE
# stays the authority for the migrated value. `deck_size` takes the symmetric route, and
# what makes it safe is the condition that route always carried — that a wrong operator
# argument fail a tracked row rather than ship. A wrong `--effect-kind` fails the reading
# test beside `load_dellian_dump` in `game/engine.rs`, which compares the WHOLE slot
# (`TargetSelectionSlot` derives `PartialEq`/`Eq`, and `effect_kind` is one of its
# fields) against `ability_utils::build_target_slots`. A wrong `--deck-size` now fails
# `fixture_deck_size_conformance` in the engine's integration suite, which decodes every
# committed `*.json.gz` through `FormatConfig` and, for a non-`Custom` format, requires
# the persisted `deck_size` to equal the one `FormatConfig::for_format` defines. That row
# is deliberately not written against `min_cards()`, which returns the payload for BOTH
# variants and so cannot discriminate them at equal payloads — the very blindness that
# left an operator typo unobservable before it existed.
#
# One authority, guarded here.
#
# GUARDS THE AUTHORITY, NOT THE CALL SITES. Every write in this script — the
# production `regenerate patched "$OUT"`, both control arms, and any caller added
# later — funnels through `regenerate`, which calls this, so one gate covers them
# all. Patching the known callers instead would leave the next one unprotected.
#
# A FUNCTION, NOT AN INLINE BLOCK, so the self-test below can drive the SAME rule the
# production path runs — the reason `transform` is one definition too. Inline, no
# corpus fixture could witness a refusal (every committed container that carries a
# `deck_size` is already tagged, and the pristine root is external to this
# checkout), so nothing in the tree failed when the gate was deleted or inverted.
# RESIDUAL of that arm, measured: neutering the RULE turns the self-test red every
# way tried (refusal dropped, comparison inverted, probe blinded, shape validation
# removed, whole function deleted), but removing ONLY the call below leaves it green.
# Arm (c″) is what makes the "shape validation removed" case true. With only (c)/(c′)
# — bare `N` against fully-tagged — the two shape clauses were unreachable from the
# self-test: dropping both still reported DECK_SIZE_REFUSED=true while the gate began
# accepting `{"type":"Exactly"}`, `{}` and `{"type":123,"data":"x"}`.
#
# THE CALL-SITE RESIDUAL IS UNANSWERED, and the `stage_path` precedent does NOT cover
# it. `stage_path`'s call-site blind spot IS answered, by a RUNTIME GUARD in
# `regenerate` (the `dirname` comparison below): reverting that one binding to
# `mktemp -t` aborts with "stage not beside destination" — measured. `deck_size_gate`
# has no analogue. Deleting its call leaves every self-test green (measured), and no
# test or CI job in this tree invokes this script at all, so nothing else fails either.
# Adjacency to the write it guards is a convention, not a check.
#
# CLOSING IT COSTS A `zip` DEPENDENCY, not a pristine archive. An arm that points
# `regenerate`'s `$PRISTINE` global at a SYNTHETIC zip built in the self-test's own
# `$tmp` closes it in a dozen-odd lines — measured green with the call below, red
# without it, and no real dump is required. But `zip` is absent from the required-tool
# preflight above, so that arm would make every run of this script depend on a tool it
# does not otherwise need. The option is recorded here rather than taken.
#
# Keyed on SHAPE AT EVERY DEPTH rather than a fixed path, and the depths are not a closed
# pair -- regenerate the ones a `deck_size` is actually carried at with
#   for f in $(git ls-files '*.json.gz'); do gzip -dc "$f" \
#     | jq -r '[paths as $p | select($p[-1]=="deck_size")
#               | ($p | map(if type=="number" then "N" else . end) | join("."))] | .[]'
#   done | sort -u
# and the same walk over `unzip -p` for the pristine dumps. The two corpora do not carry
# the same set, and neither is the pair the self-tests below drive on.
# ABSENCE IS LEGAL and passes: some committed fixtures carry no `deck_size` at any
# depth, so keying on PRESENCE would fail exactly those spuriously. A transcribed
# population figure rots — regenerate the two it would have quoted with
#   find crates/engine/tests/fixtures -name '*.json.gz'
# for the fixture population, piped through this gate's own `deck_size` predicate for
# the carriers among them. Recursive, not depth-1: the two populations differ.
#
# RESIDUAL, so this is not read as more than it is: it checks the tagged SHAPE
# (`type` a string, `data` a number), not that the tag names a live variant.
# Pinning `Minimum|Exactly` here would re-copy an engine enum into bash and rot
# silently when a variant is added; a bogus tag is the decoder's business. Over the
# only input this corpus produces — bare `N` from a pre-U5 dump — the shape check
# is fully discriminating.
deck_size_gate() {   # deck_size_gate <staged> <dest> — refuse, and reap the stage
  local staged="$1" dest="$2" bad
  bad="$(gzip -dc "$staged" | jq -c '
    [ paths as $p | select($p[-1] == "deck_size") | getpath($p) as $v
      | select(($v | type) != "object"
               or ($v.type | type) != "string"
               or ($v.data | type) != "number")
      | {at: ($p | join(".")), value: $v} ]')"
  [ "$bad" != "[]" ] || return 0
  echo "REFUSING TO EMIT $dest — untagged deck_size" >&2
  echo "  offending: $bad" >&2
  echo "  required:  {\"type\":\"<Minimum|Exactly>\",\"data\":N} — DeckSizeRule, serde tag=\"type\" content=\"data\"" >&2
  echo "  REMEDY: pass --deck-size <Minimum|Exactly>:<count> for this dump's format. The" >&2
  echo "  variant follows from the sibling \`format\` field; the ENGINE owns that mapping" >&2
  echo "  (crates/engine/src/types/format.rs), so this script takes it as an argument rather" >&2
  echo "  than inferring it, and a wrong argument fails the tracked" >&2
  echo "  fixture_deck_size_conformance row." >&2
  echo "  If --deck-size WAS passed, the offending value above was not the bare pre-U5 shape" >&2
  echo "  the stage repairs — read the path, not the argument." >&2
  rm -f "$staged"
  return 1
}

regenerate() {   # regenerate <patched|unpatched> <destination>
  local mode="$1" dest="$2" staged
  mkdir -p "$(dirname "$dest")"
  staged="$(stage_path "$dest")"
  STAGE_FILES="$STAGE_FILES $staged"
  # CALL-SITE guard. The a0 self-test proves `stage_path` RETURNS a beside-destination
  # path; it cannot see whether this line still calls it. Reverting only this binding to
  # `mktemp -t` leaves the helper and the self-test intact and green while restoring the
  # non-atomic write — the same blind spot the sibling stamper carries, fixed at both
  # sites so the class is closed rather than one instance of it.
  if [ "$(dirname "$staged")" != "$(dirname "$dest")" ]; then
    echo "stage not beside destination: $staged vs $dest — mv would not be atomic" >&2
    rm -f "$staged"
    return 1
  fi
  if ! unzip -p "$PRISTINE" | transform "$mode" | gzip -9 -n > "$staged"; then
    rm -f "$staged"
    echo "REGENERATION FAILED (fail-closed, $dest left untouched)" >&2
    return 1
  fi
  deck_size_gate "$staged" "$dest" || return 1
  # A failed `mv` leaves `$staged` in place; the trap reaps it on exit.
  mv "$staged" "$dest" || return 1
}

# PRE-FLIGHT SELF-TESTS for the three properties that no corpus fixture can witness,
# because each is about what happens to inputs this corpus does not contain.
#
# They run before anything is written, on synthetic inputs, through the SAME
# `transform` the migration uses — a self-test that re-spelled the recipe would be
# certifying its own copy.
#
#   (a) FAILURE  — when the recipe aborts, the destination must be left EXACTLY as it
#                  was. Asserted byte-wise against a sentinel, because "the file still
#                  exists" is not the claim; "the file is unchanged" is.
#   (b) PASS-THROUGH — a non-`gameState` envelope must survive the projection
#                  unchanged, not become `{"gameState":null}`.
#   (c) DECK-SIZE GATE — a bare `deck_size` must be REFUSED, and so must a MALFORMED
#                  tagged one, which is what reaches the shape clauses. Every committed
#                  fixture is already tagged and the pristine dumps are external, so this
#                  is the only thing in the tree that fails if the gate stops refusing.
#                  With `--deck-size` supplied the same bare input must instead be
#                  MIGRATED and then ACCEPTED, which is what proves the stage ran.
#
# Each has a paired POSITIVE control, or it would pass against a transform that did
# nothing at all.
selftests() {
  local tmp sentinel out rc probe_stage shape
  tmp="$(mktemp -d -t migrate-dump-selftest-XXXXXX)"

  # (a0) THE MECHANISM ITSELF: the stage file must be minted in the destination's OWN
  # directory. Arms (a)/(a') below exercise the failure and success PATHS, and they pass
  # under a non-atomic staging just as happily — their destination lives under
  # `mktemp -d -t`, so a `stage_path` reverted to `mktemp -t` puts stage and destination
  # on the same filesystem (measured: both device 50 here) and the `mv` is atomic BY
  # ACCIDENT of where the test put its own files. That is a second, independent reason
  # the original control could not fail on its subject, beyond the three-copies one:
  # even one shared recipe would have been checked on a layout that cannot expose it.
  #
  # Compares DIRECTORIES, not devices. Device equality is the property that makes `mv`
  # atomic, but it does NOT discriminate here: under a `-t` revert the stage lands in
  # `/tmp` and this test's destination lives in a SUBDIRECTORY of `/tmp`, so the two
  # still share a device and the check would pass. Same-directory is strictly stronger
  # and is what actually flips.
  probe_stage="$(stage_path "$tmp/dest")"
  if [ "$(dirname "$probe_stage")" != "$(dirname "$tmp/dest")" ]; then
    echo "SELFTEST STAGE_BESIDE_DEST=false — stage $(dirname "$probe_stage") vs dest $(dirname "$tmp/dest")" >&2
    echo "  a cross-directory stage makes \`mv\` non-atomic whenever the two differ in filesystem" >&2
    rm -f "$probe_stage"; rm -rf "$tmp"; return 1
  fi
  rm -f "$probe_stage"

  # (a) FAILURE leaves the destination untouched.
  # `stamp_delayed_allocators` aborts by name on a dump with install roots it cannot
  # collapse, which is the real abort shape, reached through the real recipe.
  printf '%s' 'COMMITTED-FIXTURE-SENTINEL' > "$tmp/dest"
  sentinel="$(sha256sum "$tmp/dest" | cut -d' ' -f1)"
  printf '%s\n' '{"gameState":{"delayed_triggers":[],"next_delayed_trigger_token":0,
                  "resolved_rules_journal":{"entries":[{"command":{"DelayedTriggerInstall":{}}}]}}}' \
    > "$tmp/in.json"
  set +e
  # Same staged-write discipline as `regenerate`; the point is that `$tmp/dest` is
  # never the redirection target, so an abort cannot reach it.
  ( staged="$(stage_path "$tmp/dest")"
    if ! transform patched < "$tmp/in.json" | gzip -9 -n > "$staged"; then
      rm -f "$staged"; exit 1
    fi
    mv "$staged" "$tmp/dest" ) >/dev/null 2>&1
  rc=$?
  set -e
  if [ "$rc" -eq 0 ]; then
    echo "SELFTEST ATOMIC_ON_FAILURE=inconclusive — the abort input did not abort; the row cannot certify atomicity" >&2
    rm -rf "$tmp"; return 1
  fi
  if [ "$(sha256sum "$tmp/dest" | cut -d' ' -f1)" != "$sentinel" ]; then
    echo "SELFTEST ATOMIC_ON_FAILURE=false — a failed regeneration modified its destination" >&2
    rm -rf "$tmp"; return 1
  fi

  # (a′) POSITIVE control — a SUCCEEDING run must actually replace the destination,
  # or (a) would pass simply because nothing ever writes.
  printf '%s\n' '{"gameState":{"turn_number":7}}' > "$tmp/ok.json"
  ( staged="$(stage_path "$tmp/dest")"
    transform patched < "$tmp/ok.json" | gzip -9 -n > "$staged"
    mv "$staged" "$tmp/dest" ) >/dev/null 2>&1
  if [ "$(sha256sum "$tmp/dest" | cut -d' ' -f1)" = "$sentinel" ]; then
    echo "SELFTEST ATOMIC_ON_FAILURE=vacuous — a SUCCESSFUL run also left the destination unchanged" >&2
    rm -rf "$tmp"; return 1
  fi

  # (b) PASS-THROUGH — the other envelope in this corpus (top level `turn_number`).
  printf '%s\n' '{"turn_number":7,"players":[]}' > "$tmp/env.json"
  out="$(transform patched < "$tmp/env.json")"
  if [ "$(printf '%s' "$out" | jq -S -c .)" != "$(jq -S -c . "$tmp/env.json")" ]; then
    echo "SELFTEST ENVELOPE_PRESERVED=false — a non-gameState dump was rewritten: $out" >&2
    rm -rf "$tmp"; return 1
  fi

  # (b′) POSITIVE control — a `gameState` dump IS still projected, so (b) is not
  # passing because the transform became a no-op for everything.
  if [ "$(transform patched < "$tmp/ok.json" | jq -c 'has("gameState")')" != "true" ]; then
    echo "SELFTEST ENVELOPE_PRESERVED=vacuous — the gameState projection stopped working" >&2
    rm -rf "$tmp"; return 1
  fi

  # (c) DECK-SIZE GATE refuses the pre-U5 shape, driven through `deck_size_gate` —
  # the same function `regenerate` calls, not a re-spelling of its rule.
  printf '%s\n' '{"format_config":{"deck_size":100}}' | gzip -9 -n > "$tmp/bare.json.gz"
  if deck_size_gate "$tmp/bare.json.gz" "$tmp/never" 2>/dev/null; then
    echo "SELFTEST DECK_SIZE_REFUSED=false — a bare deck_size was accepted; the script would" >&2
    echo "  print MIGRATED over an artifact the strict decoder cannot load" >&2
    rm -rf "$tmp"; return 1
  fi
  # (c′) POSITIVE control — the tagged shape must PASS, or (c) is equally satisfied by
  # a gate that refuses everything, which would block the whole corpus instead.
  printf '%s\n' '{"gameState":{"format_config":{"deck_size":{"type":"Exactly","data":100}}}}' \
    | gzip -9 -n > "$tmp/tagged.json.gz"
  if ! deck_size_gate "$tmp/tagged.json.gz" "$tmp/never"; then
    echo "SELFTEST DECK_SIZE_REFUSED=vacuous — the gate also refuses a correctly tagged value" >&2
    rm -rf "$tmp"; return 1
  fi

  # (c″) The SHAPE clauses, which neither (c) nor (c′) can reach: those two span bare
  # `N` against fully-tagged, so dropping `($v.type|type) != "string"` and
  # `($v.data|type) != "number"` left both of them green while the gate started
  # accepting `{"type":"Exactly"}`, `{}` and `{"type":123,"data":"x"}`. ONE SHAPE PER
  # CLAUSE, because a single malformed value only proves the OR fired, not which arm of
  # it: `{"type":"Exactly"}` is refused by the `data` clause ALONE and
  # `{"type":123,"data":100}` by the `type` clause ALONE (measured, dropping each in
  # turn). Driven through `deck_size_gate` itself, like (c), on the same in-memory path.
  # ROUTED THROUGH `transform patched`, so the deck-size stage sits in this leg's path
  # too and its subject becomes the stage's NARROWNESS as well as the gate's clauses: a
  # stage widened past the number case repairs these malformed values and the gate then
  # accepts them.
  for shape in '{"type":"Exactly"}' '{"type":123,"data":100}'; do
    printf '%s\n' "{\"format_config\":{\"deck_size\":$shape}}" \
      | transform patched | gzip -9 -n > "$tmp/shape.json.gz"
    if deck_size_gate "$tmp/shape.json.gz" "$tmp/never" 2>/dev/null; then
      echo "SELFTEST DECK_SIZE_REFUSED=shape-blind — deck_size $shape was accepted; the" >&2
      echo "  shape clauses no longer discriminate, so a malformed tag reaches the fixture" >&2
      rm -rf "$tmp"; return 1
    fi
  done

  # (c‴) THE MIGRATING LEG. Runs ONLY when `--deck-size` was supplied, and the condition
  # IS the leg's premise: without the argument the stage is the identity, so demanding
  # ACCEPTANCE on this input would assert the exact opposite of (c) — which stays
  # unconditional and is this leg's negative control on the identical bytes — and
  # `selftests` runs before `regenerate`, so the script would exit 1 before the gate ever
  # spoke. Two ENVELOPE shapes, each driven through the same `transform` and the same
  # `deck_size_gate` the production path uses. They are WITNESSES that the walk is
  # depth-agnostic, never the corpus's depth set: both corpora carry a `deck_size` deeper
  # than either shape here, and `deck_size_gate` names the walk that regenerates them.
  if [ -n "$DECK_SIZE" ]; then
    for shape in '{"format_config":{"deck_size":100}}' \
                 '{"gameState":{"format_config":{"deck_size":100}}}'; do
      printf '%s\n' "$shape" | transform patched | gzip -9 -n > "$tmp/migrated.json.gz"
      if ! deck_size_gate "$tmp/migrated.json.gz" "$tmp/never"; then
        echo "SELFTEST DECK_SIZE_MIGRATED=false — the stage left $shape untagged, so the" >&2
        echo "  gate refused the very artifact this recipe would have emitted" >&2
        rm -rf "$tmp"; return 1
      fi
      rm -f "$tmp/migrated.json.gz"
    done
    echo "SELFTEST DECK_SIZE_MIGRATED=true (both envelope shapes, driven through deck_size_gate)"
  fi

  echo "SELFTEST ATOMIC_ON_FAILURE=true ENVELOPE_PRESERVED=true DECK_SIZE_REFUSED=true (each with a positive control)"
  rm -rf "$tmp"
}

selftests || exit 1

if [ "$CONTROL_MODE" -eq 1 ]; then
  # 5. Control mode: re-run the recipe from the read-only pristine dump and hold the
  #    result against what is committed. Runnable by anyone, at any time, with no
  #    engine build. Each arm below states its own subject and its own verdict; read
  #    the conclusion off the arms that RAN, never off their number.
  [ -f "$OUT" ] || { echo "control mode needs an existing committed fixture at $OUT" >&2; exit 1; }
  PATCHED="$(mktemp -t migrate-dump-patched-XXXXXX.json.gz)"
  UNPATCHED="$(mktemp -t migrate-dump-unpatched-XXXXXX.json.gz)"
  # COMPOSE, do not replace. `trap` is last-write-wins PER SIGNAL, so a bare
  # `trap '...' EXIT` here would silently disarm the `cleanup_stage_files` EXIT handler
  # installed above and leak a stage file on this path — the one path that regenerates
  # twice. (INT/TERM keep their handler either way, which is what made the omission easy
  # to miss: only the EXIT arm was disarmed.)
  trap 'cleanup_stage_files; rm -f "$PATCHED" "$UNPATCHED"' EXIT
  regenerate patched   "$PATCHED"
  regenerate unpatched "$UNPATCHED"

  echo "CONTROL pristine=$(basename "$PRISTINE") sha256=$ACTUAL_SHA"
  echo "CONTROL effect_kind=$EFFECT_KIND deck_size=${DECK_SIZE:-none} out=$OUT"
  echo "CONTROL jq=$JQ_VERSION gzip=$GZIP_VERSION"

  # ARM 1 — the patched regeneration reproduces the committed fixture.
  case "$JQ_VERSION:$GZIP_VERSION" in
    "$PINNED_JQ:$PINNED_GZIP"*)
      if cmp -s "$PATCHED" "$OUT"; then
        echo "CONTROL BYTE_IDENTICAL=true"
      else
        echo "CONTROL BYTE_IDENTICAL=false" >&2
        exit 1
      fi
      ;;
    *)
      # Toolchain drift: byte equality is not certifiable, but content equality
      # is, and it still catches a recipe that reads the wrong dump.
      echo "CONTROL toolchain differs from pinned ($PINNED_JQ / $PINNED_GZIP) — falling back to canonical content comparison"
      if [ "$(gzip -dc "$PATCHED" | jq -S -c .)" = "$(gzip -dc "$OUT" | jq -S -c .)" ]; then
        echo "CONTROL CANONICALLY_EQUAL=true BYTE_IDENTICAL=unknown"
      else
        echo "CONTROL CANONICALLY_EQUAL=false" >&2
        exit 1
      fi
      ;;
  esac

  # ARM 2 — the `effect_kind` patch reached `target_slots`.
  #
  # COMPARES THE `target_slots` PROJECTION, not the whole document.
  #
  # The struck form compared the two documents wholesale and required a difference.
  # That inference died when stage 2b landed: the patched filter also runs
  # `stamp_trigger_firing` and `stamp_delayed_allocators`, and the allocator stage
  # rewrites `next_delayed_trigger_token` / `..._instance` on EVERY `gameState`-shaped
  # dump in this corpus (measured: absent/0 -> 1 on each). The unpatched
  # filter runs neither stage. So the documents differed unconditionally, including on
  # the dumps that carry no target prompt at all — arm 2 reported `PATCHED_DIFFERS=true`
  # while the `effect_kind` filter had matched NOTHING. That is precisely the vacuous
  # pass this arm exists to prevent, so it was reporting the opposite of its claim.
  #
  # The no-prompt case is now NAMED rather than counted as a pass: it is a real and
  # expected shape here, but arm 2 cannot certify the `effect_kind` stage from it, and
  # saying so is the honest reading.
  SLOTS_P="$(gzip -dc "$PATCHED"   | jq -S -c '[.gameState.waiting_for.data.target_slots[]?]')"
  SLOTS_U="$(gzip -dc "$UNPATCHED" | jq -S -c '[.gameState.waiting_for.data.target_slots[]?]')"
  if [ "$SLOTS_P" = "[]" ] && [ "$SLOTS_U" = "[]" ]; then
    echo "CONTROL PATCHED_DIFFERS=n/a — this dump carries no target_slots, so the effect_kind stage is vacuously absent and arm 2 cannot certify it (arm 1 and the stage-2b arms still apply)"
  elif [ "$SLOTS_P" = "$SLOTS_U" ]; then
    echo "CONTROL PATCHED_DIFFERS=false — the effect_kind filter matched nothing; arm 1 above would pass vacuously" >&2
    exit 1
  else
    echo "CONTROL PATCHED_DIFFERS=true stamped=$(gzip -dc "$PATCHED" | jq -c '[.gameState.waiting_for.data.target_slots[]?.effect_kind]') unpatched=$(gzip -dc "$UNPATCHED" | jq -c '[.gameState.waiting_for.data.target_slots[]?.effect_kind]')"
  fi

  # ARM 3 — stage 2b landed: the firing carriers and the allocators are present and
  # canonical in the patched regeneration. This is what actually distinguishes patched
  # from unpatched on a no-prompt dump, and arm 2 above deliberately no longer claims it.
  #
  # MUST COMPARE AGAINST THE UNPATCHED REGENERATION. Asserting only that the patched
  # side is canonical is vacuous on any pristine dump that ALREADY carries allocators
  # >= 1 and its carriers: the arm would report `true` while stage 2b changed nothing.
  # That is the same vacuity arm 2 was corrected for — a control that cannot tell "the
  # stage landed" from "it was already there" certifies nothing. When the two sides
  # agree, this arm SKIPS LOUDLY as `n/a` rather than claiming a landing it cannot see.
  # The signature must be built from the fields stage 2b WRITES, not from the ones it
  # reads. `trigger_carrier_count` counts the dump's NEED (pending_trigger, triggered
  # stack entries, resolving_stack_entry) — inputs neither transform touches, so it is
  # identical on both sides by construction and contributes nothing to the comparison.
  # Keying on the STAMPED carriers is what makes the carrier half of this arm able to
  # move at all.
  #
  # RESIDUAL, stated so the arm is not read as more than it is: the comparison is one
  # equality over the COMBINED signature, so any differing term alone yields `true`. On
  # the common corpus shape — allocators repaired 0 -> 1 — a carrier-stamp regression is
  # still masked by the allocator half (measured: stamp disabled, allocators moving,
  # arm reports LANDED=true). What the printed signature gives you is self-disclosure:
  # `s:0` on both sides says the carrier half did not move, whatever the verdict.
  alloc_sig() {   # alloc_sig <gz> — the stage-2b observable: stamped carriers + allocators
    gzip -dc "$1" | jq -c '{p: .gameState.pending_trigger_firing,
                            s: (.gameState.stack_trigger_firings // {} | length),
                            r: .gameState.resolving_trigger_firing,
                            t: (.gameState.next_delayed_trigger_token // 0),
                            i: (.gameState.next_delayed_trigger_instance // 0)}'
  }
  STAGE2B_P="$(alloc_sig "$PATCHED")"
  STAGE2B_U="$(alloc_sig "$UNPATCHED")"
  if [ "$(gzip -dc "$PATCHED" | jq -c 'if (.gameState // null) == null then "n/a"
                                       elif (((.gameState.next_delayed_trigger_token // 0) >= 1)
                                         and ((.gameState.next_delayed_trigger_instance // 0) >= 1))
                                       then "true" else "false" end')" = '"false"' ]; then
    echo "CONTROL STAGE_2B_LANDED=false — the allocator repair did not reach the regeneration" >&2
    exit 1
  fi
  if [ "$STAGE2B_P" = "$STAGE2B_U" ]; then
    echo "CONTROL STAGE_2B_LANDED=n/a — the unpatched regeneration already carries $STAGE2B_U, so this arm cannot certify that stage 2b did anything (it is NOT evidence the stage ran)"
  else
    echo "CONTROL STAGE_2B_LANDED=true patched=$STAGE2B_P unpatched=$STAGE2B_U"
  fi
  exit 0
fi

regenerate patched "$OUT"

OUT_SHA="$(sha256sum "$OUT" | cut -d' ' -f1)"
SLOTS="$(gzip -dc "$OUT" | jq -c '[.gameState.waiting_for.data.target_slots[]?.effect_kind]')"

# 4. Record the provenance on stdout so a commit message can quote it. Both operator
#    arguments are named, because the recipe is not recoverable from the artifact:
#    `Minimum:100` and `Exactly:100` both satisfy `deck_size_gate` and produce
#    DIFFERENT bytes. `deck_size=none` records that no `--deck-size` was passed.
echo "MIGRATED pristine=$(basename "$PRISTINE") sha256=$ACTUAL_SHA"
echo "MIGRATED effect_kind=$EFFECT_KIND deck_size=${DECK_SIZE:-none} stamped_slots=$SLOTS"
echo "MIGRATED out=$OUT sha256=$OUT_SHA"
echo "MIGRATED jq=$JQ_VERSION gzip=$GZIP_VERSION"

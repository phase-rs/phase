#!/usr/bin/env bash
# Stamps the CR 603.7 `TriggerFiring` carriers that upstream #6842 (8121fd1c6)
# made mandatory onto an ALREADY-COMMITTED fixture, in place.
#
# WHY IN PLACE AND NOT A PRISTINE REGENERATION. `migrate-dump-fixture.sh`
# regenerates from the read-only pristine root, which is the stronger provenance
# and is preferred where it applies. It does NOT apply to every fixture in this
# corpus: measured, `dina_conqueror_4p` and `witherbloom_sprout_lumaret_simple_4p`
# differ from their pristine regeneration in exactly one object each (Priest of
# Forgotten Gods' `abilities` / `base_abilities` AST), because the committed
# fixture carries a LATER parser state than the 2026-07-22/25 capture. Rerunning
# those from pristine would silently REVERT that. Stamping in place is additive
# and cannot revert anything, and its arm-1 control is strictly stronger: the
# stamped artifact minus the five stamped keys (three firing carriers plus the
# two delayed-trigger allocators — the exact `del()` list below) must be
# BYTE-IDENTICAL to what was committed, which proves zero collateral change.
#
# The derivation is NOT re-spelled here — it is loaded from
# scripts/lib/trigger-firing.jq, the same single definition
# `migrate-dump-fixture.sh` uses. See that file for the CR 603.1 vs CR 603.7a
# discriminant and for why `UnknownLegacy` is not a legal persisted value.
#
# Usage:
#   scripts/stamp-fixture-firing.sh crates/engine/tests/fixtures/name.json.gz [...]
#   scripts/stamp-fixture-firing.sh --control crates/engine/tests/fixtures/name.json.gz
#
# It stamps TWO field classes, both made mandatory-or-repaired by #6842:
#   1. the CR 603.7 firing carriers themselves; and
#   2. the CR 603.7 delayed-trigger ALLOCATORS
#      (`next_delayed_trigger_token` / `..._instance`), which #6842 repairs at
#      load time on the PRODUCTION decode path only. Left unstamped, a legacy
#      dump restores 0 through a bare `GameState` decode and 1 through the
#      production decoder, so the two paths disagree — and 0 is the value the
#      engine's own coherence validator rejects. Stamping the repaired value on
#      disk keeps the decoders in agreement WITHOUT weakening any assertion.
#
# ALL FIVE control arms, and a partial check passes vacuously:
#   arm 1 => NO_COLLATERAL=true        stamped minus the 5 stamped keys is
#            byte-identical to the committed fixture, so nothing else moved.
#   arm 2 => CARRIERS_ADDED=true       every firing carrier the dump needs is
#            present (got == need). Keyed on CARRIER COUNT, not on byte
#            difference: gzip/jq re-serialization alone changes bytes without
#            stamping anything, which is the stale-artifact false pass.
#   arm 3 => ALLOCATORS_CANONICAL=true both allocators exist and are >= 1.
#   arm 4 => DEFINITION_SHAPES=true    a PRE-FLIGHT control, run once before any
#            fixture is touched: the shipped `_defs` must read BOTH serialized
#            definition shapes, and must still ABORT when a description is in
#            neither. See `definition_shape_control` below.
#   arm 5 => CARRIER_PRESERVED=true    a PRE-FLIGHT control: an existing canonical
#            carrier must SURVIVE the stamp (the "additive" claim above, which arm 1
#            structurally cannot check because it deletes those keys before
#            comparing), while an absent one is still derived. See
#            `carrier_preservation_control` below.

set -euo pipefail

CONTROL=0
[ "${1:-}" = "--control" ] && { CONTROL=1; shift; }
[ $# -gt 0 ] || { echo "usage: $0 [--control] <fixture.json.gz>..." >&2; exit 1; }

LIB="$(dirname "${BASH_SOURCE[0]}")/lib/trigger-firing.jq"
[ -f "$LIB" ] || { echo "missing $LIB" >&2; exit 1; }

for tool in jq gzip sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || { echo "required tool not found: $tool" >&2; exit 1; }
done

# Every key this script is allowed to add. Arm 1 deletes exactly these from both
# sides, so anything else that moved shows up as a collateral change.
CARRIERS='del(.gameState.pending_trigger_firing, .gameState.stack_trigger_firings, .gameState.resolving_trigger_firing,
              .gameState.next_delayed_trigger_token, .gameState.next_delayed_trigger_instance)'

# arm 4 — PRE-FLIGHT: does the shipped `_defs` actually read both serialized
# definition shapes?
#
# The two lists do NOT serialize alike. `trigger_definitions` is
# `Definitions<TriggerEntry>` and nests its text at `.definition.description`;
# `base_trigger_definitions` is `Vec<TriggerDefinition>` and exposes
# `.description` directly. A filter that reads one field name across both still
# resolves every carrier in THIS corpus — measured, 172 of 172 — because the base
# list happens to repeat the same descriptions. So "every carrier resolved" is NOT
# evidence that both shapes are read, and no fixture-level arm can supply that
# evidence. This one can: it asks the question directly, per shape.
#
# Three cases, and the NEGATIVE is what makes the positives non-vacuous — without
# it a `_defs` that returned every string it could find would score green.
definition_shape_control() {
  local nested direct absent
  # (a) description ONLY in the live list, nested under `.definition`.
  nested="$(jq -n -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" \
    '_firing({"7":{"trigger_definitions":[{"definition":{"description":"D"}}]}}; 7; "D")') 2>/dev/null || echo FAILED)"
  # (b) description ONLY in the base list, exposed directly.
  direct="$(jq -n -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" \
    '_firing({"7":{"base_trigger_definitions":[{"description":"D"}]}}; 7; "D")') 2>/dev/null || echo FAILED)"
  # (c) NEGATIVE CONTROL — present in neither shape MUST still abort by name.
  if jq -n -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" \
    '_firing({"7":{"trigger_definitions":[{"definition":{"description":"OTHER"}}]}}; 7; "D")') >/dev/null 2>&1
  then absent=RESOLVED; else absent=ABORTED; fi

  if [ "$nested" = '"Ordinary"' ] && [ "$direct" = '"Ordinary"' ] && [ "$absent" = ABORTED ]; then
    echo "CONTROL DEFINITION_SHAPES=true nested=$nested direct=$direct unknown=$absent"
    return 0
  fi
  echo "CONTROL DEFINITION_SHAPES=false nested=$nested direct=$direct unknown=$absent" >&2
  echo "  the derivation does not read both serialized definition shapes (or no longer" >&2
  echo "  aborts on an unknown description) — refusing to stamp anything" >&2
  return 1
}

# arm 5 — PRE-FLIGHT: stamping is genuinely ADDITIVE.
#
# The header above claims in-place stamping "cannot revert anything". Arm 1 cannot
# check that claim for the five stamped keys, because it DELETES exactly those keys
# from both sides before comparing — the one blind spot in an otherwise strong control.
# So a carrier that was already canonical (a modern capture, or an engine-side
# migration) could have been silently rewritten to "Ordinary", which is the CR 603.7a
# to CR 603.1 re-classification `lib/trigger-firing.jq` exists to refuse.
#
# The NEGATIVE half is what makes it non-vacuous: an ABSENT carrier must still be
# derived, or "preserved everything" would also describe a stamp that did nothing.
#
# VOCABULARY: these arms pin the LIVE `TriggerFiring` wire shapes
# (`types/identifiers.rs`): `"Ordinary"`, `"LegacyDelayed"`,
# `{"ReceiptEligible":{token,instance,source_id}}`, `"UnknownLegacy"`. They previously
# pinned `{"Delayed":null}`, a shape upstream #6933 removed. jq is untyped, so those arms
# stayed GREEN against a vocabulary the engine can no longer produce — a control passing
# on input the subject cannot emit is not evidence about the subject.
carrier_preservation_control() {
  local kept receipt derived
  local defs='"objects":{"7":{"base_trigger_definitions":[{"description":"D"}]}}'
  local pend='"pending_trigger":{"source_id":7,"description":"D"}'
  local firing='stamp_trigger_firing | .gameState.pending_trigger_firing'
  # (a) An existing delayed carrier survives, and is NOT rewritten to "Ordinary" —
  #     note its description IS present in the object's definitions, so the struck
  #     form would have overwritten it rather than aborting.
  kept="$(printf '%s' "{\"gameState\":{$defs,$pend,
                        \"pending_trigger_firing\":\"LegacyDelayed\"}}" \
    | jq -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" "$firing") \
      2>/dev/null || echo FAILED)"
  # (b) The PAYLOAD-CARRYING variant survives with its payload intact. A preservation
  #     rule written against the unit variants alone could drop `ReceiptEligible`'s
  #     origin and still satisfy (a) — this is the arm that says the value, not just
  #     the discriminant, comes through.
  receipt="$(printf '%s' "{\"gameState\":{$defs,$pend,
    \"pending_trigger_firing\":{\"ReceiptEligible\":{\"token\":3,\"instance\":4,\"source_id\":7}}}}" \
    | jq -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" "$firing") \
      2>/dev/null || echo FAILED)"
  # (c) NEGATIVE CONTROL — the SAME dump with the carrier removed must still derive one,
  #     or "preserved" would also describe a stamp that stopped working entirely.
  derived="$(printf '%s' "{\"gameState\":{$defs,$pend}}" \
    | jq -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" "$firing") \
      2>/dev/null || echo FAILED)"

  if [ "$kept" = '"LegacyDelayed"' ] \
     && [ "$receipt" = '{"ReceiptEligible":{"token":3,"instance":4,"source_id":7}}' ] \
     && [ "$derived" = '"Ordinary"' ]; then
    echo "CONTROL CARRIER_PRESERVED=true existing=$kept receipt=$receipt absent_is_derived=$derived"
    return 0
  fi
  echo "CONTROL CARRIER_PRESERVED=false existing=$kept receipt=$receipt absent_is_derived=$derived" >&2
  echo "  stamping is not additive (or stopped deriving absent carriers) — refusing to stamp" >&2
  return 1
}

# arm 6 — PRE-FLIGHT: a carrier whose stack entry has LEFT is pruned from what gets
# written, and the pruned map actually reaches the fixture.
#
# This arm exists because the prune shipped BROKEN in the previous round. The scoping to
# live ids was computed correctly into `$sf_existing`, but the write-back was gated on
# `($sf | length) > 0` — "did we derive any NEW carriers" — so in the one case the prune
# is FOR, a dump whose only change is that an entry left the stack, `$sf` was empty, the
# assignment was skipped, and the stale key survived into the written fixture. The prune
# was computed and then discarded.
#
# Every sub-arm below is keyed on a dump whose live carriers ALREADY exist, because that
# is what forces `$sf` empty and reaches the gate. Arm 1's key-blind comparison cannot
# see this and arm 5 does not construct a departed entry, which is how it got through.
stale_carrier_control() {
  local run pruned emptied untouched
  run() {   # run <dump-json> — stamp it, print the resulting stack_trigger_firings
    printf '%s' "$1" \
      | jq -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" \
          'stamp_trigger_firing | (.gameState.stack_trigger_firings // null)') \
        2>/dev/null || echo FAILED
  }
  local objs='"objects":{"7":{"base_trigger_definitions":[{"description":"D"}]}}'
  local live='"stack":[{"id":9,"kind":{"type":"TriggeredAbility","data":{"source_id":7,"description":"D"}}}]'

  # (a) THE REGRESSION ITSELF — entry 5 has left the stack, entry 9 is still on it and
  #     already carries a canonical marker. `$sf` is empty here, which is the gate the
  #     old form failed at. The stale key must be gone AND the live one must remain.
  pruned="$(run "{\"gameState\":{$objs,$live,
    \"stack_trigger_firings\":{\"9\":\"LegacyDelayed\",\"5\":\"Ordinary\"}}}")"
  # (b) EVERY carrier stale — the rebuilt map is `{}`, and `{}` must still be written.
  #     A prune that only ever shrinks a non-empty map would pass (a) and fail here.
  emptied="$(run "{\"gameState\":{$objs,\"stack\":[],
    \"stack_trigger_firings\":{\"5\":\"Ordinary\"}}}")"
  # (c) NEGATIVE CONTROL — nothing stale. The map must come through UNCHANGED, or (a)
  #     and (b) would also be satisfied by a stamp that simply wipes the slot. This is
  #     also the idempotence check: a re-run of an already-correct fixture writes nothing.
  untouched="$(run "{\"gameState\":{$objs,$live,
    \"stack_trigger_firings\":{\"9\":\"LegacyDelayed\"}}}")"

  if [ "$pruned" = '{"9":"LegacyDelayed"}' ] \
     && [ "$emptied" = '{}' ] \
     && [ "$untouched" = '{"9":"LegacyDelayed"}' ]; then
    echo "CONTROL STALE_CARRIER_PRUNED=true pruned=$pruned all_stale=$emptied unchanged=$untouched"
    return 0
  fi
  echo "CONTROL STALE_CARRIER_PRUNED=false pruned=$pruned all_stale=$emptied unchanged=$untouched" >&2
  echo "  expected pruned={\"9\":\"LegacyDelayed\"} all_stale={} unchanged={\"9\":\"LegacyDelayed\"}" >&2
  echo "  a departed stack entry's carrier is being carried forward — refusing to stamp" >&2
  return 1
}

# A valid envelope with NO `gameState` must reach the main loop's SKIP path.
# `stamp_trigger_firing` / `stamp_delayed_allocators` both gate on `.gameState` and pass
# such envelopes through untouched, so the stamper must ask nothing of them. The defect
# this pins lived in the SHELL's `ALLOC_NEED` / arm-3 reads, not in the derivation, so
# this control drives the REAL loop through a child invocation — a jq-only probe would
# have stayed green while the script refused an unchanged, valid fixture.
non_gamestate_control() {
  [ -z "${STAMP_SELFTEST_CHILD:-}" ] || return 0   # inside the child: do not recurse
  local d out crc
  d="$(mktemp -d)" || return 1
  printf '%s' '{"schemaVersion":3,"note":"a valid envelope carrying no gameState"}' \
    | gzip -9 -n > "$d/no-gamestate.json.gz"
  out="$(STAMP_SELFTEST_CHILD=1 "$0" "$d/no-gamestate.json.gz" 2>&1)"; crc=$?
  rm -rf "$d"
  # Assert the SKIP FIRED BY NAME. `rc=0` alone is not the claim: a loop that stamped
  # nothing and fell through silently would also exit 0, which is a different behaviour
  # wearing the same exit code.
  if [ "$crc" -eq 0 ] && printf '%s\n' "$out" | grep -q '^SKIP  no-gamestate\.json\.gz'; then
    echo "CONTROL NON_GAMESTATE_SKIPPED=true"
    return 0
  fi
  echo "CONTROL NON_GAMESTATE_SKIPPED=false rc=$crc" >&2
  printf '%s\n' "$out" | sed 's/^/    /' >&2
  echo "  a gameState-less envelope must reach the SKIP path — the jq passes it through," >&2
  echo "  so demanding an allocator repair here refuses an unchanged, valid fixture" >&2
  return 1
}

# The staged file MUST be minted in the destination's OWN directory. This script
# rewrites its fixtures IN PLACE, and those fixtures are TRACKED, so a stage on a
# different filesystem makes the final `mv` a copy-then-unlink and an interruption
# truncates a committed fixture — the worst failure mode either fixture script has.
# Measured here: `mktemp -t` resolves to /tmp (device 50) while the fixture directory
# is on /home (device 47), so the two genuinely differ. Same recipe, same fix, and the
# same helper shape as `migrate-dump-fixture.sh` — the sibling this was missed on.
stage_beside() {   # stage_beside <destination>
  mktemp "$(dirname "$1")/.stamp-firing-stage-XXXXXX.json.gz"
}
# Registered stage files are reaped on EXIT/INT/TERM: the explicit `rm -f "$TMP"` calls
# below cannot cover death before the next statement runs, and debris would land in the
# tracked fixture directory.
STAGE_FILES=""
cleanup_stage_files() {
  [ -n "$STAGE_FILES" ] || return 0
  # shellcheck disable=SC2086 # deliberate word-splitting over the staged-path list
  rm -f $STAGE_FILES
  STAGE_FILES=""
}
trap cleanup_stage_files EXIT INT TERM

# The staging mechanism itself: the stage file must be minted in the destination's OWN
# directory, or the in-place `mv` is not atomic and an interrupted run truncates a
# TRACKED fixture. Compares DIRECTORIES, not devices — device equality is the property
# that makes `mv` atomic but it does NOT discriminate here, because a `-t` revert puts
# the stage in /tmp and any test destination under /tmp shares its device. That trap
# already cost `migrate-dump-fixture.sh` a self-test that could not fail on its subject.
stage_locality_control() {
  local d probe
  d="$(mktemp -d)" || return 1
  : > "$d/dest.json.gz"
  probe="$(stage_beside "$d/dest.json.gz")"
  if [ "$(dirname "$probe")" != "$(dirname "$d/dest.json.gz")" ]; then
    echo "CONTROL STAGE_BESIDE_DEST=false — stage $(dirname "$probe") vs dest $(dirname "$d/dest.json.gz")" >&2
    echo "  a cross-directory stage makes the in-place mv non-atomic; an interrupted run" >&2
    echo "  would truncate a tracked fixture" >&2
    rm -f "$probe"; rm -rf "$d"; return 1
  fi
  rm -f "$probe"; rm -rf "$d"
  echo "CONTROL STAGE_BESIDE_DEST=true"
  return 0
}

definition_shape_control || exit 1
carrier_preservation_control || exit 1
stale_carrier_control || exit 1
non_gamestate_control || exit 1
stage_locality_control || exit 1

rc=0
for FIX in "$@"; do
  [ -f "$FIX" ] || { echo "no such fixture: $FIX" >&2; rc=1; continue; }
  TMP="$(stage_beside "$FIX")"
  STAGE_FILES="$STAGE_FILES $TMP"
  # CALL-SITE guard, and it is NOT redundant with `stage_locality_control` above. That
  # control proves the HELPER returns a beside-destination path; it says nothing about
  # whether this line still calls the helper. Measured: revert ONLY this binding to
  # `mktemp -t`, leaving `stage_beside` intact, and the control still prints
  # STAGE_BESIDE_DEST=true while the run writes with its stage in /tmp — the original
  # defect fully restored, past a green control. A property must be asserted where the
  # value is BOUND, not only where it is produced.
  if [ "$(dirname "$TMP")" != "$(dirname "$FIX")" ]; then
    echo "stage not beside destination: $TMP vs $FIX — mv would not be atomic" >&2
    rm -f "$TMP"; rc=1; continue
  fi
  # `-f` with the lib prepended keeps ONE definition of the derivation.
  if ! gzip -dc "$FIX" \
      | jq -c -f <(printf '%s\nstamp_trigger_firing | stamp_delayed_allocators\n' "$(cat "$LIB")") \
      | gzip -9 -n > "$TMP"; then
    echo "STAMP FAILED (fail-closed, nothing written): $FIX" >&2
    rm -f "$TMP"; rc=1; continue
  fi

  # How many carriers this dump NEEDS, read from the dump itself. A dump that
  # needs none is skipped outright: stamping it is a no-op, and a "the bytes
  # changed" arm over it would be reporting jq re-serialization rather than a
  # stamp — the stale-artifact false pass, inverted.
  NEED="$(gzip -dc "$FIX" | jq -c -f <(printf '%s\ntrigger_carrier_count\n' "$(cat "$LIB")"))"
  GOT="$(gzip -dc "$TMP" | jq -c '((if .gameState.pending_trigger_firing then 1 else 0 end)
                                   + (.gameState.stack_trigger_firings // {} | length)
                                   + (if .gameState.resolving_trigger_firing then 1 else 0 end))')"
  SUMMARY="$(gzip -dc "$TMP" | jq -c '{pending: .gameState.pending_trigger_firing,
                                       stack: (.gameState.stack_trigger_firings // {} | length),
                                       resolving: .gameState.resolving_trigger_firing}')"

  # The allocator repair is a SEPARATE need from the firing carriers: a dump can
  # want one and not the other, so a dump with no triggered record is only truly
  # a no-op when its allocators are already at or above 1.
  # A `.gameState`-less envelope is passed through untouched by `stamp_delayed_allocators`
  # (trigger-firing.jq gates on exactly this), so it NEEDS nothing. Without the guard
  # `.gameState.next_delayed_trigger_token` is null, `// 0` makes it 0, `< 1` is true, and
  # the script demands a repair the jq will never perform — which turned an unchanged,
  # valid fixture into an arm-3 failure and made this general-purpose script refuse it.
  ALLOC_NEED="$(gzip -dc "$FIX" | jq -c 'if (.gameState // null) == null then 0
                                         elif ((.gameState.next_delayed_trigger_token // 0) < 1)
                                            or ((.gameState.next_delayed_trigger_instance // 0) < 1)
                                         then 1 else 0 end')"
  ALLOC_GOT="$(gzip -dc "$TMP" | jq -c '{tok: .gameState.next_delayed_trigger_token,
                                         inst: .gameState.next_delayed_trigger_instance}')"

  if [ "$NEED" -eq 0 ] && [ "$ALLOC_NEED" -eq 0 ]; then
    echo "SKIP  $(basename "$FIX") needs=0 carriers, allocators already canonical — nothing to stamp"
    rm -f "$TMP"; continue
  fi

  # arm 1 — no collateral change: everything except the carrier keys is identical.
  A="$(gzip -dc "$TMP" | jq -S -c "$CARRIERS")"
  B="$(gzip -dc "$FIX" | jq -S -c "$CARRIERS")"
  if [ "$A" = "$B" ]; then ARM1=true; else ARM1=false; fi
  # arm 2 — the stamp had teeth: every carrier the dump needs is now present.
  # Keyed on CARRIER COUNT, not on byte difference, because gzip/jq
  # re-serialization alone can change bytes without stamping anything.
  if [ "$GOT" -eq "$NEED" ]; then ARM2=true; else ARM2=false; fi
  # arm 3 — the allocator repair landed. Both fields must exist and be >= 1;
  # 0 is the value the engine's own coherence validator rejects. Reports `n/a` on a
  # `.gameState`-less envelope rather than `false`: there is no allocator to make
  # canonical, so the arm has nothing to certify and must not read as a failure.
  ARM3="$(gzip -dc "$TMP" | jq -r 'if (.gameState // null) == null then "n/a"
                                   elif ((.gameState.next_delayed_trigger_token // 0) >= 1)
                                    and ((.gameState.next_delayed_trigger_instance // 0) >= 1)
                                   then "true" else "false" end')"

  echo "STAMP $(basename "$FIX") carriers=$SUMMARY needs=$NEED got=$GOT alloc_need=$ALLOC_NEED alloc=$ALLOC_GOT NO_COLLATERAL=$ARM1 CARRIERS_ADDED=$ARM2 ALLOCATORS_CANONICAL=$ARM3"

  # ARM3 is tri-valued (`true`/`false`/`n/a`); only an outright `false` is a failure.
  if [ "$ARM1" != true ] || [ "$ARM2" != true ] || [ "$ARM3" = false ]; then
    echo "  control arms failed for $FIX — not writing" >&2
    rm -f "$TMP"; rc=1; continue
  fi

  if [ "$CONTROL" -eq 1 ]; then
    rm -f "$TMP"
  else
    mv "$TMP" "$FIX"
    echo "  wrote $FIX sha256=$(sha256sum "$FIX" | cut -d' ' -f1)"
  fi
done
exit $rc

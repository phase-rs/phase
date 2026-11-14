#!/usr/bin/env bash
# Stamps `DecisionSlot.point` and `PinnedDecision::Order`'s slot onto ALREADY-COMMITTED
# dump fixtures, in place, in two passes, M1 (each slot gains its typed CR choice point) then
# M2 (`Order` gains the slot every other pin variant carries).
#
# WHY IN PLACE AND NOT A PRISTINE REGENERATION. Same ground `stamp-fixture-firing.sh`
# states: a committed fixture can carry a LATER parser state than any pristine capture, and
# rerunning from pristine would silently revert it, while stamping is additive and cannot
# revert anything. For THESE fixtures the pristine path is not merely weaker, it is
# unavailable — the read-only pristine root carries no archive for them (regenerate the
# root's contents with `ls ~/vibe-coding/combofb-dumps-pristine`, whose positive control is
# the several archives it does carry for other fixtures in this corpus).
#
# THE SCRIPT TAKES FIXTURE PATHS AND NOTHING ELSE. There is no operator-supplied variant,
# because a slot's shape is the same wherever it is stored and one variant per file would
# assert about every hit in it what is true of at most one. Each hit's point is derived
# from the neighbour that GOVERNS it — a published point's sibling `kind`, a decision's
# `PinnedDecision` variant tag, or the field's own documented contract — and any hit the
# derivation does not resolve ABORTS THE WHOLE RUN AND WRITES NOTHING.
#
# The derivation is NOT re-spelled here. It is loaded from
# scripts/lib/decision-slot-point.jq, the single definition the migration and every control
# below all run through, so no control can certify its own copy.
#
# Usage:
#   scripts/stamp-decision-slot-point.sh crates/engine/tests/fixtures/name.json.gz [...]
#   scripts/stamp-decision-slot-point.sh --control crates/engine/tests/fixtures/name.json.gz
#
# A PARTIAL CHECK CERTIFIES NOTHING. Each arm states its own subject and reports its own
# verdict, and a conclusion may only be drawn from the arms that actually RAN. The arms are
# labelled where they run rather than inventoried here:
#   grep -inE '^ *# arm [0-9]' scripts/stamp-decision-slot-point.sh
# The pre-flight controls that carry no arm label are reached by:
#   grep -n '_control || exit 1' scripts/stamp-decision-slot-point.sh

set -euo pipefail

CONTROL=0
[ "${1:-}" = "--control" ] && { CONTROL=1; shift; }
[ $# -gt 0 ] || { echo "usage: $0 [--control] <fixture.json.gz>..." >&2; exit 1; }

LIB="$(dirname "${BASH_SOURCE[0]}")/lib/decision-slot-point.jq"
[ -f "$LIB" ] || { echo "missing $LIB" >&2; exit 1; }

for tool in jq gzip sha256sum; do
  command -v "$tool" >/dev/null 2>&1 || { echo "required tool not found: $tool" >&2; exit 1; }
done

# `-f` with the lib prepended keeps ONE definition of the derivation.
jqlib() {   # jqlib <program> — run the lib plus <program> over stdin
  jq -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" "$1")
}
jqlib_n() { # jqlib_n <program> — same, with no input
  jq -n -c -f <(printf '%s\n%s\n' "$(cat "$LIB")" "$1")
}

# PRE-FLIGHT: each derivation is total, and the NEGATIVE leg is what makes the positives
# non-vacuous. Without it a derivation that returned a constant would score green — which
# is the failure mode the per-hit rule exists against.
derivation_totality_control() {
  local may tgt ord bad_ord bad_kind dup
  may="$(jqlib_n '_kind_point("MayChoice")' 2>/dev/null || echo FAILED)"
  tgt="$(jqlib_n '_kind_point({"Targets":{"ordered":false}})' 2>/dev/null || echo FAILED)"
  ord="$(jqlib_n '{"decisions":[{"Order":{"source":{"AllCopies":{"card_id":1}},"pos":3}}]}
                  | stamp_order_slots | .decisions[0].Order' 2>/dev/null || echo FAILED)"
  # (a) NEGATIVE — a `Targets` point at `ordered: true` names a choice the SCHEMA does not
  #     carry, so it must abort rather than be guessed at.
  if jqlib_n '_kind_point({"Targets":{"ordered":true}})' >/dev/null 2>&1
  then bad_ord=RESOLVED; else bad_ord=ABORTED; fi
  # (b) NEGATIVE — an unmapped governing neighbour must abort.
  if jqlib_n '_kind_point({"Bogus":{}})' >/dev/null 2>&1
  then bad_kind=RESOLVED; else bad_kind=ABORTED; fi
  # (c) NEGATIVE — two `Order` decisions of one template naming the SAME source must abort:
  #     which instance is which is not derivable from the serialized form.
  if jqlib_n '{"decisions":[{"Order":{"source":{"AllCopies":{"card_id":1}},"pos":0}},
                            {"Order":{"source":{"AllCopies":{"card_id":1}},"pos":1}}]}
              | stamp_order_slots' >/dev/null 2>&1
  then dup=RESOLVED; else dup=ABORTED; fi

  if [ "$may" = '"MayGate"' ] && [ "$tgt" = '"AnnouncedTarget"' ] \
     && [ "$ord" = '{"slot":{"source":{"AllCopies":{"card_id":1}},"point":"TriggerOrder","index":0},"pos":3}' ] \
     && [ "$bad_ord" = ABORTED ] && [ "$bad_kind" = ABORTED ] && [ "$dup" = ABORTED ]; then
    echo "CONTROL DERIVATION_TOTAL=true may=$may targets=$tgt order=$ord ordered_true=$bad_ord unmapped=$bad_kind dup_source=$dup"
    return 0
  fi
  echo "CONTROL DERIVATION_TOTAL=false may=$may targets=$tgt order=$ord ordered_true=$bad_ord unmapped=$bad_kind dup_source=$dup" >&2
  echo "  the derivation does not resolve the governing neighbours (or no longer aborts on" >&2
  echo "  an underivable one) — refusing to stamp anything" >&2
  return 1
}

# PRE-FLIGHT: a document carrying neither shape reaches the SKIP path, and the SKIP is
# asserted BY NAME. `rc=0` alone is not the claim: a loop that stamped nothing and fell
# through silently would also exit 0, which is a different behaviour wearing the same exit
# code. This is what makes the script safe to point at the whole fixture root, which holds
# a card map, a decklist bundle and census artifacts that are not persisted game states.
# The defect this pins would live in the SHELL's `NEED` reads rather than in the
# derivation, so it drives the REAL loop through a child invocation.
non_gamestate_control() {
  [ -z "${STAMP_SELFTEST_CHILD:-}" ] || return 0   # inside the child: do not recurse
  local d out crc
  d="$(mktemp -d)" || return 1
  printf '%s' '{"schemaVersion":3,"note":"a valid envelope carrying neither shape"}' \
    | gzip -9 -n > "$d/no-slots.json.gz"
  out="$(STAMP_SELFTEST_CHILD=1 "$0" "$d/no-slots.json.gz" 2>&1)"; crc=$?
  rm -rf "$d"
  if [ "$crc" -eq 0 ] && printf '%s\n' "$out" | grep -q '^SKIP  no-slots\.json\.gz'; then
    echo "CONTROL NO_SHAPE_SKIPPED=true"
    return 0
  fi
  echo "CONTROL NO_SHAPE_SKIPPED=false rc=$crc" >&2
  printf '%s\n' "$out" | sed 's/^/    /' >&2
  echo "  a document carrying neither shape must reach the SKIP path by name" >&2
  return 1
}

# The staged file MUST be minted in the destination's OWN directory. This script rewrites
# TRACKED fixtures in place, so a stage on a different filesystem makes the final `mv` a
# copy-then-unlink and an interruption truncates a committed fixture — the worst failure
# mode either fixture script has.
stage_beside() {   # stage_beside <destination>
  mktemp "$(dirname "$1")/.stamp-slot-point-stage-XXXXXX.json.gz"
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

# Compares DIRECTORIES, not devices — device equality is the property that makes `mv`
# atomic but it does NOT discriminate here, because a `-t` revert puts the stage in /tmp
# and any test destination under /tmp shares its device.
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

derivation_totality_control || exit 1
non_gamestate_control || exit 1
stage_locality_control || exit 1

rc=0
for FIX in "$@"; do
  [ -f "$FIX" ] || { echo "no such fixture: $FIX" >&2; rc=1; continue; }
  # GNU `gzip -dc` transparently reads a single-member deflate zip, so an unrefused `.zip`
  # would be decompressed, re-gzipped, and `mv`d over the archive with every arm green.
  case "$FIX" in *.json.gz) ;; *) echo "not a .json.gz fixture: $FIX" >&2; rc=1; continue ;; esac
  TMP="$(stage_beside "$FIX")"
  STAGE_FILES="$STAGE_FILES $TMP"
  # CALL-SITE guard, and it is NOT redundant with `stage_locality_control` above. That
  # control proves the HELPER returns a beside-destination path; it says nothing about
  # whether this line still calls the helper. A property must be asserted where the value
  # is BOUND, not only where it is produced.
  if [ "$(dirname "$TMP")" != "$(dirname "$FIX")" ]; then
    echo "stage not beside destination: $TMP vs $FIX — mv would not be atomic" >&2
    rm -f "$TMP"; rc=1; continue
  fi

  # How many carriers of each shape this dump NEEDS, read from the dump itself. A dump
  # that needs none is skipped outright: stamping it is a no-op, and a "the bytes changed"
  # arm over it would be reporting jq re-serialization rather than a stamp. That is also
  # what makes a re-run over an already-stamped fixture a no-op BY CONSTRUCTION — both
  # walks are key-set-exact and cannot see a stamped object.
  NEED_M1="$(gzip -dc "$FIX" | jqlib 'm1_need')"
  NEED_M2="$(gzip -dc "$FIX" | jqlib 'm2_need')"
  if [ "$NEED_M1" -eq 0 ] && [ "$NEED_M2" -eq 0 ]; then
    echo "SKIP  $(basename "$FIX") needs=0 slot points, 0 order slots — nothing to stamp"
    rm -f "$TMP"; continue
  fi

  if ! gzip -dc "$FIX" | jqlib 'stamp_decision_slot_points' | gzip -9 -n > "$TMP"; then
    echo "STAMP FAILED (fail-closed, nothing written): $FIX" >&2
    rm -f "$TMP"; rc=1; continue
  fi

  # arm 1 — NO_COLLATERAL, over the STAMPED output. The stamped artifact with exactly the
  # stamped keys blinded must be identical to the COMMITTED artifact through the same
  # blind. Strictly stronger than a difference check: it is the arm that sees a stage
  # rewriting something ELSE during stamping, which an arm run with the stamp disabled
  # cannot see. The blind is key-BLIND on both shapes, because these keys are added at
  # every object of a shape rather than at fixed top-level names.
  A="$(gzip -dc "$TMP" | jqlib 'unstamp_decision_slot_points' | jq -S -c .)"
  B="$(gzip -dc "$FIX" | jq -S -c .)"
  if [ "$A" = "$B" ]; then ARM1=true; else ARM1=false; fi
  # arm 1's PAIRED POSITIVE CONTROL, in the same invocation: the same comparison with the
  # blind replaced by `.` must report DIFFER. Two legs agreeing is a dead instrument and
  # its result is void, not confirming.
  C="$(gzip -dc "$TMP" | jq -S -c .)"
  if [ "$C" = "$B" ]; then ARM1CTL=SAME; else ARM1CTL=DIFFER; fi

  # arm 2 — SHAPES_STAMPED, keyed on COUNT per shape, because gzip/jq re-serialization
  # alone can change bytes without stamping anything. Both pre-move shapes are gone; every
  # slot in the output carries its point (including the one M2 mints inside each `Order`);
  # and every `Order` carries a slot.
  LEFT_M1="$(gzip -dc "$TMP" | jqlib 'm1_need')"
  LEFT_M2="$(gzip -dc "$TMP" | jqlib 'm2_need')"
  GOT_M1="$(gzip -dc "$TMP" | jqlib 'm1_got')"
  GOT_M2="$(gzip -dc "$TMP" | jqlib 'm2_got')"
  if [ "$LEFT_M1" -eq 0 ] && [ "$LEFT_M2" -eq 0 ] \
     && [ "$GOT_M1" -eq "$((NEED_M1 + NEED_M2))" ] && [ "$GOT_M2" -eq "$NEED_M2" ]; then
    ARM2=true; else ARM2=false; fi

  POINTS="$(gzip -dc "$TMP" | jqlib '[paths(type=="object") as $p | getpath($p)
             | select(type=="object" and ((keys_unsorted|sort)==["index","point","source"]))
             | .point] | group_by(.) | map({(.[0]): length}) | add')"

  echo "STAMP $(basename "$FIX") need_m1=$NEED_M1 need_m2=$NEED_M2 got_slots=$GOT_M1 got_orders=$GOT_M2 points=$POINTS NO_COLLATERAL=$ARM1 (control=$ARM1CTL) SHAPES_STAMPED=$ARM2"

  if [ "$ARM1" != true ] || [ "$ARM1CTL" != DIFFER ] || [ "$ARM2" != true ]; then
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

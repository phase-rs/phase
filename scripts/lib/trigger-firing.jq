# CR 603.7 firing carriers for a persisted game dump (upstream #6842, 8121fd1c6).
#
# SINGLE DEFINITION of the derivation. Both the pristine regeneration path and
# the in-place stamping path load THIS file, so neither can certify its own copy
# of the recipe.
#
# #6842 made a `TriggerFiring` carrier MANDATORY on every persisted triggered
# record and fails CLOSED without one, so a dump captured before that commit
# cannot load at all. The read-only pristine root predates it too (captured
# 2026-07-22/25), so the value cannot be recovered by re-reading the dump — it
# must be DERIVED, per record.
#
# `TriggerFiring::UnknownLegacy` is NOT an escape hatch: `validate_firing`
# rejects it for a live carrier ("has no canonical trigger firing
# discriminator") because it is the field-absent marker (`skip_serializing_if`)
# and the redaction default, never a legal persisted value.
#
# THE DISCRIMINANT, CR 603.1 vs CR 603.7a:
#   ORDINARY <= the fired trigger's definition is present on its SOURCE OBJECT's
#               own `trigger_definitions` / `base_trigger_definitions`, matched
#               by exact `description`. A printed or granted triggered ability of
#               a permanent is an ordinary triggered ability.
#   DELAYED  <= the trigger has an install receipt in `delayed_triggers`. Every
#               dump in this corpus records `delayed_triggers: []` and no install
#               journal, so `Delayed(Some(..))` could not validate regardless —
#               `validate_firing` demands a registered install root.
# Anything else ABORTS by name. There is deliberately no fallback stamp: a wrong
# carrier silently re-classifies a CR 603.7 firing identity, which is exactly the
# inference upstream refuses to make.
#
# `stack_trigger_firings` is keyed by the STACK ENTRY id — what
# `validate_trigger_firing_coherence` looks up — not by the source id.

# The two definition lists have DIFFERENT serialized shapes, and reading one field name
# across both silently drops a whole list:
#   `trigger_definitions`      is `Definitions<TriggerEntry>`, and `TriggerEntry` is
#                              `{occurrence, definition}` — the text lives at
#                              `.definition.description`
#                              (`crates/engine/src/types/ability.rs`).
#   `base_trigger_definitions` is `Arc<Vec<TriggerDefinition>>` — text at `.description`.
#
# MEASURED on the committed corpus: of the `trigger_definitions` entries, 0 expose a
# DIRECT `.description` and 100% (145 / 165 / 132 on dellian / dina / witherbloom) nest it
# under `.definition`. The struck form read `.description` on both lists, so every LIVE
# entry collapsed to the `// ""` fallback and that list contributed nothing — all 172
# carriers matched through `base_trigger_definitions` alone. "Every carrier resolved" was
# therefore true but not evidence that this read was right.
#
# The gap is REACHABLE, not theoretical: `dellian_emblem_conqueror_4p` carries a GRANTED
# trigger ("When ~ dies, you gain 1 life.") that is present in the live list and ABSENT
# from the base list. A firing whose description existed only there would have aborted the
# entire stamp with `UNDETERMINED firing carrier` on a fixture that is in fact classifiable.
#
# Read each entry by its OWN shape rather than assuming one: `.definition.description`
# first, then `.description`. Empty strings are dropped so a shape this does not
# understand cannot match a description that is itself empty — an unrecognised entry must
# reach the `UNDETERMINED` abort, never satisfy a lookup by accident.
def _defs($objs; $src):
  (($objs[($src|tostring)] // {})
   | (.trigger_definitions // []) + (.base_trigger_definitions // []))
  | map((.definition.description // .description) // "")
  | map(select(. != ""));

def _firing($objs; $src; $d):
  if ((_defs($objs; $src)) | index($d)) then "Ordinary"
  else error("UNDETERMINED firing carrier: source=\($src) description=\($d)")
  end;

# How many carriers this dump actually needs. 0 means the dump records no
# triggered pending/stack/resolving entry at all, so stamping it is a NO-OP and
# any "the bytes changed" control arm over it would be reporting jq
# re-serialization, not a stamp.
def trigger_carrier_count:
  (if ((.gameState.pending_trigger // null) != null) then 1 else 0 end)
  + ([ (.gameState.stack // [])[] | select(.kind.type == "TriggeredAbility") ] | length)
  + (if (((.gameState.resolving_stack_entry // .gameState.resolving_trigger).kind.type? // "")
         == "TriggeredAbility") then 1 else 0 end);

# Pass a dump that is not `gameState`-shaped straight through. Several fixtures
# in this corpus are stored in a different envelope (top level `turn_number`,
# not `gameState`); without this guard `.gameState |= ...` would CREATE a
# gameState key on them, i.e. corrupt them.
# CR 603.7 delayed-trigger ALLOCATORS — a second field class #6842 repairs at
# load time, and only on ONE of the two decode paths.
#
# `next_delayed_trigger_token` carries `#[serde(default)]`, so a pre-#6842 dump
# that omits it restores as 0 through a bare `GameState` decode. The production
# `PersistedGameState` path instead runs the load-time migration
#   next = max(existing // 1, (max used token) + 1)
# and restores 1. The two paths therefore disagree on a legacy dump, and 0 is
# invalid on its face: `validate_trigger_firing_coherence` rejects
# `next_delayed_trigger_token <= max_token`, and `max_token` is 0 when there are
# no install roots. Stamping the repaired value on disk makes the fixture look
# like a modern capture, which keeps the two decoders in agreement WITHOUT
# relaxing the assertion, and survives the eventual deletion of the shim.
#
# The GENERAL derivation of the used-token set is ENGINE logic — it walks
# `resolved_rules_journal` install commands and `delayed_triggers` provenance,
# with reuse and nonzero checks. Re-deriving that here would repeat exactly the
# mistake `migrate-dump-fixture.sh` refuses to make for `EffectKind`. So this
# stamps ONLY the case where the formula collapses to a constant — no install
# roots at all, so both used sets are empty and the result is
# `max(existing // 1, 1)` — and ABORTS BY NAME otherwise, leaving the general
# case to the engine.
def stamp_delayed_allocators:
  if (.gameState // null) == null then . else
    # `.command` is only indexable when it is an OBJECT. serde writes an externally
    # tagged UNIT variant as a bare JSON string, and `select(.command.X)` on a string
    # aborts jq with `Cannot index string with "DelayedTriggerInstall"`. That is not a
    # named abort: this file's contract is that an undetermined case aborts BY NAME, and
    # a raw type error leaves the operator unable to tell a shape it does not understand
    # from a real install root. Filter to objects first so the probe stays total.
    ([ (.gameState.resolved_rules_journal.entries // [])[]
       | select((.command | objects | has("DelayedTriggerInstall")) // false) ] | length) as $installs
  | ((.gameState.delayed_triggers // []) | length) as $delayed
  | if ($installs > 0 or $delayed > 0)
    then error("UNDETERMINED delayed-trigger allocators: \($installs) install command(s), \($delayed) delayed trigger(s) — deriving the used-token set is engine logic, not jq's")
    else .gameState.next_delayed_trigger_token
           = ([(.gameState.next_delayed_trigger_token // 1), 1] | max)
       | .gameState.next_delayed_trigger_instance
           = ([(.gameState.next_delayed_trigger_instance // 1), 1] | max)
    end
  end;

# DERIVES ONLY WHERE NO CARRIER EXISTS. Never rewrites one that is already there.
#
# The struck form assigned all three keys unconditionally, without reading them. That
# made `stamp-fixture-firing.sh`'s header claim — in-place stamping "is additive and
# cannot revert anything" — false for exactly these keys, and arm 1 cannot catch it
# because it deletes all five stamped keys from both sides before comparing.
#
# The damage is silent and is the one inference this file exists to refuse. A fixture
# already carrying a canonical `{"Delayed": {...}}` (a modern capture, or an
# engine-side migration) has two ways to lose it:
#   * the delayed trigger's description is absent from the source object's definitions,
#     so `_firing` ABORTS the whole stamp on a fixture that was already correct; or
#   * the description IS present there, so the carrier is silently rewritten to
#     "Ordinary" — a CR 603.7a delayed firing re-classified as a CR 603.1 ordinary one,
#     with no diagnostic. That is precisely the silent re-classification this file's
#     header forbids.
#
# Deriving only into an ABSENT slot makes the "additive" claim true, and makes the whole
# stamp idempotent: re-running it over already-stamped fixtures is now a no-op rather
# than a re-derivation that has to agree with itself.
def stamp_trigger_firing:
  if (.gameState // null) == null then . else
  .gameState.objects as $objs
  | (.gameState.resolving_stack_entry // .gameState.resolving_trigger) as $rt
  # Existing carriers are preserved, but ONLY for stack entries that are still there.
  # A key naming an entry that has left the stack is stale, and carrying it forward
  # would inflate `stack_trigger_firings` past the number of triggered records — which
  # is also the one shape that could defeat `stamp-fixture-firing.sh`'s arm 2, since
  # that arm compares carrier TOTALS and a surplus here could cancel a deficit
  # elsewhere. Scoping preservation to live entries keeps "preserve what is canonical"
  # from becoming "accumulate whatever was there".
  | ([ (.gameState.stack // [])[]
       | select(.kind.type == "TriggeredAbility") | (.id|tostring) ]) as $live_ids
  | ((.gameState.stack_trigger_firings // {})
     | with_entries(select(.key as $k | $live_ids | index($k)))) as $sf_existing
  | .gameState |= (
      (if ((.pending_trigger // null) != null
           and (.pending_trigger_firing // null) == null)
         then .pending_trigger_firing =
                _firing($objs; .pending_trigger.source_id; .pending_trigger.description)
         else . end)
    # Assign whenever the REBUILT map differs from what is on the fixture — not merely
    # when new carriers were derived. Gating on `($sf | length) > 0` dropped the prune
    # above on the floor in exactly the case the prune exists for: a fixture whose only
    # change is that an entry LEFT the stack derives no new carrier, so `$sf` is empty
    # and the stale key survived into the written fixture. Comparing against the current
    # map keeps this idempotent (a re-run writes nothing) and keeps it from touching
    # trigger-free fixtures (an absent slot and a rebuilt `{}` compare equal under
    # `// {}`), while still writing `{}` when every carrier the fixture had went stale.
    | (([ (.stack // [])[]
          | select(.kind.type == "TriggeredAbility")
          # BIND THE KEY FIRST. `$sf_existing | has((.id|tostring))` looks like it asks
          # "is this entry already carried?", but jq evaluates a function argument against
          # the INPUT of the pipe it sits in — here `$sf_existing`, not the stack entry.
          # `.id` is absent on that object, so the argument was the literal string "null",
          # `has` was ALWAYS false, and every live entry was re-derived and then allowed to
          # override the canonical marker through `$sf_existing + $sf`. That silently
          # rewrote `Delayed` to `Ordinary` — the CR 603.7a to CR 603.1 re-classification
          # this file's header exists to refuse.
          | (.id|tostring) as $k
          | select(($sf_existing | has($k)) | not)
          | {key: $k,
             value: _firing($objs; .kind.data.source_id; .kind.data.description)} ]
        | from_entries) as $sf
       | ($sf_existing + $sf) as $sf_rebuilt
       | if $sf_rebuilt != (.stack_trigger_firings // {})
           then .stack_trigger_firings = $sf_rebuilt
           else . end)
    | (if ($rt != null and ($rt.kind.type? // "") == "TriggeredAbility"
           and (.resolving_trigger_firing // null) == null)
         then .resolving_trigger_firing =
                _firing($objs; $rt.kind.data.source_id; $rt.kind.data.description)
         else . end)
    )
  end;

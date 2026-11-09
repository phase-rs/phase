# Legacy compatibility tripwires

This file indexes every deliberate legacy-compat shim in the engine: where the
shim lives, why it exists, and what deadline forces its removal. Each entry has
a grep token so an audit pass can find every site without manual recall.

Add a new entry whenever you introduce a `serde(alias)`, a `deserialize_with`
that handles a legacy on-disk shape, or any "accept both old and new" code path.
Removing a shim means deleting both the entry here AND the corresponding
tripwire const in the source.

## Format

Each entry MUST include:

- **Grep token** — a unique `LEGACY_*` constant name that appears verbatim both
  in the source tripwire const and in this index.
- **What it covers** — the on-disk shape being accepted in addition to the
  current one.
- **Added in** — workspace version when the shim landed.
- **Removal trigger** — the version boundary at which the tripwire `assert!`
  fires (usually `+14` patch releases as a soft window for downstream catch-up).
- **Source** — file:line of the tripwire const.

## Active shims

### `LEGACY_JOURNAL_SETTLED_PIP_INFERENCE_2026Q4` — ACTIVE

- **Covers:** a `SettlementNode` payload written before the `settled` field
  existed. `ResolvedRulesJournal::node_settled` answers from the node's recorded
  `produced_pips` when `settled` is absent (`None`), instead of requiring the
  settlement record this build always writes.
- **Added in:** 0.121.0.
- **Removal trigger:** the tripwire `assert!` fires once a release above 0.121.14
  ships (`+14` patch releases from the landing version). Removing the shim means
  answering `None` as "this payload states no settlement" rather than inferring one.
- **Source:** `crates/engine/src/types/resolved_commands.rs:4126` (tripwire const);
  the shimmed arm is at `crates/engine/src/types/resolved_commands.rs:2658`.

### `LEGACY_ASSIST_CONTRIBUTION_ABSENT_2026Q4` — ACTIVE

- **Covers:** a parked `PendingCast` payload written before
  `assist_contributed_units` existed. Such a save carries no record of the units its
  Assist helper spent, so the cast's payment snapshot
  (`GameObject::mana_spent_to_cast_amount`, `colors_spent_to_cast`,
  `mana_spent_source_snapshots`) is completed from the units the payload does have —
  the caster's own — and the helper's contribution is missing. That under-records
  rather than mis-records: the payment happened, and the only honest repair would be
  the pips themselves, which the payload does not carry.
- **Added in:** 0.121.0.
- **Removal trigger:** the tripwire `assert!` fires once a release above 0.121.14
  ships (`+14` patch releases from the landing version). Removing the shim means
  requiring the record on every parked Assist cast past its payment boundary, so a
  payload that omits it becomes a refused contradiction instead of the legacy shape.
- **Source:** `crates/engine/src/types/game_state.rs:528` (tripwire const); the
  shimmed reading is `PendingCast::assist_contributed_units`' empty serde default,
  consumed by `crates/engine/src/game/casting_costs.rs` (the capture at the spend
  boundary) and `crates/engine/src/game/casting.rs` (both payment finalizers).

### `LEGACY_PARKED_ACTIVATION_OCCURRENCE_ABSENT_2026Q4` — ACTIVE

- **Covers:** a parked `PendingManaAbility` payload written before
  `activated_mana_occurrence` existed. `PendingManaAbility::activation_producer_evidence`
  changes its evidence check when the occurrence is absent: the `None` arm asks the
  node's own settlement record (`SettledWithoutOccurrence`), where the `Some` arm can
  also compare the stated occurrence against the node's ordinal. Such a save's
  announcement epoch is still exact evidence, so it is admitted rather than refused.
- **Added in:** 0.121.0.
- **Removal trigger:** the tripwire `assert!` fires once a release above 0.121.14
  ships (`+14` patch releases from the landing version). Removing the shim means
  requiring the occurrence on every parked activation, so a payload that omits it
  becomes a refused contradiction instead of the legacy shape.
- **Source:** `crates/engine/src/types/game_state.rs:11891` (tripwire const); the
  shimmed arm is at `crates/engine/src/types/game_state.rs:11797`.

## Removed shims

### `LEGACY_DESER_ETB_CONTROLLER_2026Q2` — REMOVED in v0.1.54

- **Covers:** the pre-2026-Q2 `under_your_control: bool` shape at three layers
  (`Effect::ChangeZone.enters_under`, `PendingChangeZoneIteration.enters_under_player`,
  `WaitingFor::EffectZoneChoice.enters_under_player`).
- **Added in:** 0.1.39.
- **Removed in:** 0.1.54 — deserializers, serde aliases, and tripwire deleted.

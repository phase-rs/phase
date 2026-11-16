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

### `LEGACY_DESER_TRIGGER_CAUSE_CONTROLLED_CREATURE_2026Q4`

- **Covers:** the unit `TriggerCause::ControlledCreatureDealtDamage` (Wayta,
  Trainer Prodigy's doubler cause) written before the "creature you control"
  doubler causes were parameterized into `TriggerCause::ControlledCreature {
  event: ControlledCreatureEvent }`. `TriggerCauseWire` (the
  `#[serde(from)]` deserialize mirror of `TriggerCause`) maps it to
  `ControlledCreature { event: DealtDamage }`, so persisted games and older
  card exports still load.
- **Added in:** 0.126.0.
- **Removal trigger:** the tripwire fails to compile once the workspace
  version exceeds 0.140.x. Remove the legacy arm (or the whole
  `TriggerCauseWire` mirror and `#[serde(from)]`) and the tripwire.
- **Source:** `crates/phase-ast/src/types/statics.rs`
  (`_LEGACY_DESER_TRIGGER_CAUSE_CONTROLLED_CREATURE_2026Q4`).

## Removed shims

### `LEGACY_DESER_ETB_CONTROLLER_2026Q2` — REMOVED in v0.1.54

- **Covers:** the pre-2026-Q2 `under_your_control: bool` shape at three layers
  (`Effect::ChangeZone.enters_under`, `PendingChangeZoneIteration.enters_under_player`,
  `WaitingFor::EffectZoneChoice.enters_under_player`).
- **Added in:** 0.1.39.
- **Removed in:** 0.1.54 — deserializers, serde aliases, and tripwire deleted.

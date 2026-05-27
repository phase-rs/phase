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

### `LEGACY_DESER_ETB_CONTROLLER_2026Q2`

- **Covers:** `Effect::ChangeZone.under_your_control: bool` legacy shape.
  Modern shape is `enters_under: Option<ControllerRef>`. The compat
  deserializer maps `true` → `Some(ControllerRef::You)` and `false`/`null` →
  `None`. Routed via `#[serde(alias = "under_your_control")]`.
- **Added in:** 0.1.39 (engine variant lift, CR 110.2a).
- **Removal trigger:** workspace version > 0.1.53.
- **Source:** `crates/engine/src/types/ability.rs` — search the file for
  `_LEGACY_DESER_ETB_CONTROLLER_2026Q2`.

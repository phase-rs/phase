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

- **Covers:** the pre-2026-Q2 `under_your_control: bool` shape, lifted to
  typed shapes at three layers per CR 110.2a:
  - `Effect::ChangeZone.enters_under` (AST layer) — modern shape
    `Option<ControllerRef>`.
  - `PendingChangeZoneIteration.enters_under_player` (runtime carrier) —
    modern shape `Option<PlayerId>`.
  - `WaitingFor::EffectZoneChoice.enters_under_player` (interactive carrier) —
    modern shape `Option<PlayerId>`.
- **Compat deserializers:**
  - `deserialize_enters_under_compat` — full fidelity for the AST field
    (`Bool(true)` → `Some(ControllerRef::You)`, `false`/`null` → `None`).
  - `deserialize_enters_under_player_compat` — best-effort for the runtime
    carriers (`Bool(true)` → `None` + `tracing::warn`; `false`/`null` →
    `None`). Legacy `true` cannot be perfectly reconstructed because the
    PlayerId resolution requires the originating `AbilityDefinition` which
    is unavailable at deserialization time. Falling back to `None`
    matches the unshimmed behavior (owner control) and the warn provides
    an audit trail for mid-prompt resumes that crossed the boundary.
- All three fields use `#[serde(alias = "under_your_control")]` so legacy
  on-disk payloads (IndexedDB resume, phase-server SQLite restore, P2P resume)
  still parse.
- **Added in:** 0.1.39 (AST lift, CR 110.2a); runtime-carrier compat extended
  in the same release line before any version-bump past 0.1.39 ships.
- **Removal trigger:** workspace version > 0.1.53. **Removal procedure:**
  delete BOTH deserializers, all three `#[serde(alias = ..., deserialize_with
  = ...)]` attributes, and the tripwire const. Mark this entry **REMOVED in
  v\<X.Y.Z\>**.
- **Source:** `crates/engine/src/types/ability.rs` — search the file for
  `_LEGACY_DESER_ETB_CONTROLLER_2026Q2`. Field sites:
  `Effect::ChangeZone.enters_under` (ability.rs),
  `PendingChangeZoneIteration.enters_under_player` (game_state.rs),
  `WaitingFor::EffectZoneChoice.enters_under_player` (game_state.rs).

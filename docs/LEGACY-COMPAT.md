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
  fires (usually `+14` releases as a soft window for downstream catch-up).
  Releases here bump the MINOR component only (`v0.121.0`, `v0.122.0`, ...), so the
  window is counted in minor releases: a shim authored at 0.122.0 stays legal
  through 0.136.0 and its tripwire fires at 0.137.0. Counting the window in patch
  positions instead collapses it to the single next release and breaks the build for
  `main` and every open PR whenever a release lands.
- **Source** — file:line of the tripwire const.

## Active shims

None. The three paths this release introduced were withdrawn before shipping — see
[Withdrawn before shipping](#withdrawn-before-shipping).

## Withdrawn before shipping

A shim whose guarded shape never reached the wire is not a compatibility path at
all: no released build can write the older shape, so admitting it buys nothing and
the tripwire's deadline only breaks the build. These three were added and withdrawn
inside the same unreleased release (`0.122.0`). Each field was traced to its
introducing commit and no release tag contains one, which is what `git log -S
<field>` plus `git tag --contains <sha>` shows.

Note for the next shim: `docs/LEGACY-COMPAT.md` deadlines are counted in RELEASES,
and this workspace releases by bumping the minor version only, so a "+14" window is
`minor + 14`. Counting patch positions collapses the window to the next release and
fails the build for `main` and every open PR.

### `LEGACY_JOURNAL_SETTLED_PIP_INFERENCE_2026Q4` — WITHDRAWN

- **Covered:** a `SettlementNode` payload stating no `settled` record, from which
  `ResolvedRulesJournal::node_settled` inferred settlement off `produced_pips`.
- **Why withdrawn:** `SettlementNode::settled` was introduced on this branch and no
  released build omits it. The reading was also wrong on its own terms: a zero-output
  activation (Heronblade Elite at power 0, CR 605.2) settles with no pips, so the pip
  list answers a different question. `settled` is now written explicitly at both ends
  of a node's life — `Some(false)` at allocation, `Some(true)` in `settle_node` — and
  `validate_serialized_authority` refuses a node that states none.
- **Was:** `crates/engine/src/types/resolved_commands.rs:4183` (tripwire const);
  shimmed arm in `ResolvedRulesJournal::node_settled`.

### `LEGACY_PARKED_ACTIVATION_OCCURRENCE_ABSENT_2026Q4` — WITHDRAWN

- **Covered:** a parked `PendingManaAbility` payload stating no
  `activated_mana_occurrence`, which `PendingManaAbility::activation_producer_evidence`
  admitted by asking the retained node's own settlement record instead of comparing
  the occurrence against the node's ordinal.
- **Why withdrawn:** `activated_mana_occurrence` was introduced on this branch and no
  released build omits it. An occurrence-less save cannot say WHICH activation its
  retained node belongs to, so it is refused as a contradiction
  (`ParkedActivationConflict::AbsentOccurrence`) rather than read as an older shape.
- **Was:** `crates/engine/src/types/game_state.rs:11956` (tripwire const); shimmed
  `None` arm in `PendingManaAbility::activation_producer_evidence`.

### `LEGACY_ASSIST_CONTRIBUTION_ABSENT_2026Q4` — WITHDRAWN

- **Covered:** a parked `PendingCast` payload stating no `assist_contributed_units`,
  whose empty serde default recorded the helper's spend as "the helper spent nothing"
  in the cast's payment snapshot.
- **Why withdrawn:** the field was introduced on this branch and no released build
  omits it, so absence is a contradiction. The compat reading is replaced by binding
  a restored Assist to the spend it claims — recovering the journal-bound units where
  the record proves them and refusing an unprovable paid root — rather than
  under-recording the payment.
- **Was:** `crates/engine/src/types/game_state.rs:528` (tripwire const); the reading
  was `PendingCast::assist_contributed_units`' empty serde default, consumed by
  `crates/engine/src/game/casting_costs.rs` (the capture at the spend boundary) and
  `crates/engine/src/game/casting.rs` (both payment finalizers).

## Removed shims

### `LEGACY_DESER_ETB_CONTROLLER_2026Q2` — REMOVED in v0.1.54

- **Covers:** the pre-2026-Q2 `under_your_control: bool` shape at three layers
  (`Effect::ChangeZone.enters_under`, `PendingChangeZoneIteration.enters_under_player`,
  `WaitingFor::EffectZoneChoice.enters_under_player`).
- **Added in:** 0.1.39.
- **Removed in:** 0.1.54 — deserializers, serde aliases, and tripwire deleted.

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
  in the source const and in this index.
- **What it covers** — the on-disk shape being accepted in addition to the
  current one.
- **Added in** — workspace version when the shim landed.
- **Removal trigger** — the version boundary past which the shim must be removed
  (usually `+14` releases as a soft window for downstream catch-up). Releases here
  bump the MINOR component only (`v0.121.0`, `v0.122.0`, ...), so the window is
  counted in minor releases: a shim shipping in 0.123.0 must go once a release above
  0.137.0 ships. The boundary is DOCUMENTED, not compiled — the source const carries
  only the grep token. An earlier revision asserted it against
  `CARGO_PKG_VERSION_*` in a `const` block; that is a new environment-variable read
  the intake policy reserves for maintainer review, and it also turned every release
  bump into a compile failure for `main` and every open PR.
- **Source** — file:line of the grep-token const.

## Active shims

### `LEGACY_JOURNAL_SETTLED_PIP_INFERENCE_2026Q4` — ACTIVE

- **Covers:** a `SettlementNode` payload stating no `settled` record, from which
  `ResolvedRulesJournal::node_settled` infers settlement off `produced_pips`.
- **Scope:** the reading is admitted only for a payload that is UNIFORMLY older. The
  format is decided once per payload at the restore boundary
  (`GameState::bind_or_reject_settlement_custody`) from the markers only this build
  writes — whether any node states a settlement record, and whether any parked
  mana-ability activation states an `activated_mana_occurrence`. A payload carrying
  either marker cannot contain a settlement-recording node that states none: that is
  `PersistedRestoreError::ContradictorySettlementCustody`, not this shim. For a
  uniformly older payload the pip-derived answer is bound INTO the record
  (`ResolvedRulesJournal::bind_legacy_settlement_records`) at that boundary, so the
  save the resumed game writes next states its settlements and the mixed shape (old
  nodes without a record beside newly allocated nodes with one) cannot arise.
- **Why it is genuine:** `SettlementNode` has been persisted since 0.85
  (`d247cee56`, 2026-07-22) and every released build wrote it WITHOUT this field —
  `git tag --contains d247cee56` lists shipped tags, and `upstream/main`'s copy of
  the struct has no such field. The committed capture
  `crates/engine/tests/fixtures/dina_conqueror_4p.json.gz` is one such payload (zero
  `"settled"` and zero `activated_mana_occurrence` occurrences) and must keep
  restoring. What is new is the field, not the record. All 27 committed `.json.gz`
  captures are this uniform shape.
- **Spoofing:** deleting the settlement key from one node of a current save does not
  reach this shim — the payload's other settlement records (or the parked activation's
  occurrence) still mark it current, so the deletion is refused. Stripping EVERY
  current-format marker fabricates a payload this build never wrote, which is by
  construction the uniform older shape above; that residual ambiguity is the whole
  reason the shim exists.
- **Added in:** 0.123.0.
- **Removal trigger:** must be removed once a release above 0.137.0 ships (`+14`
  releases from the shipping version). Removing the shim means answering `None` as
  "this payload states no settlement" rather than inferring one.
- **Source:** `crates/engine/src/types/resolved_commands.rs:4223` (grep-token const);
  the shimmed arm is `ResolvedRulesJournal::node_settled`.

### `LEGACY_PARKED_ACTIVATION_OCCURRENCE_ABSENT_2026Q4` — ACTIVE

- **Covers:** a parked `PendingManaAbility` payload stating no
  `activated_mana_occurrence`. `PendingManaAbility::activation_producer_evidence`
  then asks the retained node's own settlement record instead of comparing the
  occurrence against the node's ordinal, and refuses a node that already settled
  (`ParkedActivationConflict::SettledWithoutOccurrence`). Such a save's announcement
  epoch is still exact evidence, so it is admitted rather than refused.
- **Why it is genuine:** `PendingManaAbility` is persisted by every released build
  and this field only by 0.123, so an occurrence-less save is an older save, not a
  contradiction.
- **Added in:** 0.123.0.
- **Removal trigger:** must be removed once a release above 0.137.0 ships (`+14`
  releases from the shipping version). Removing the shim means requiring the
  occurrence on every parked activation.
- **Source:** `crates/engine/src/types/game_state.rs:11930` (grep-token const); the
  shimmed `None` arm is `PendingManaAbility::activation_producer_evidence`.

### Not a shim: `PendingCast::assist_contributed_units` absence

A parked Assist cast written before the field existed decodes its unit list as the
serde default (empty). That shape is NOT admitted silently: the restore boundary
binds a restored contribution to the payment it claims
(`bind_or_reject_assist_contribution_provenance`) — recovering the units the journal
records for that helper on that spell, and refusing a paid-but-unprovable root and a
unit list with no paid helper alike. A POSITIVE contribution is admitted only when the
journal records the helper's spend on that spell: a correctly sized unit list that no
journal record backs is refused
(`AssistContributionConflict::JournalRecordsNoSpendForSavedUnits`) rather than read as
agreement, because CR 118.3a makes the journal's removal record the payment and the
save's list only a claim about it. The one gap that needs no journal is a `Paid`
helper who paid 0 generic: nothing was removed, so the empty list is the payment.
Removing the field's `#[serde(default)]` instead
would fail the whole payload at decode time and foreclose the journal recovery, which
is the behaviour the maintainer asked for.

## Withdrawn before shipping

None.

## Removed shims

### `LEGACY_DESER_ETB_CONTROLLER_2026Q2` — REMOVED in v0.1.54

- **Covers:** the pre-2026-Q2 `under_your_control: bool` shape at three layers
  (`Effect::ChangeZone.enters_under`, `PendingChangeZoneIteration.enters_under_player`,
  `WaitingFor::EffectZoneChoice.enters_under_player`).
- **Added in:** 0.1.39.
- **Removed in:** 0.1.54 — deserializers, serde aliases, and tripwire deleted.

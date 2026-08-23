---
phase: 58-custom-format-engine
doc: CONTEXT
subsystem: engine-formats
status: research-and-design (no implementation)
tags: [format, custom-format, old-school, middle-school, classic-magic, legacy-rules]
---

# Phase 58 — Custom ("Design Your Own") Format Engine — CONTEXT

## Why this matters

phase.rs today models formats as a **closed, compile-time `GameFormat` enum**
(`crates/engine/src/types/format.rs`). That is exactly the right shape for the
fixed roster of *official* formats (Standard, Commander, Premodern, …): typed,
exhaustively matched, audited, version-controlled. It is the *wrong* shape for
**player/operator-authored custom formats**, whose card pools, banned/restricted
lists, and alternate-rule toggles are open-ended data that must not require an
engine recompile and a new enum variant per format.

The goal of this phase is to design a **general, data-driven custom-format
layer** that coexists with the closed enum, and to validate that layer by
expressing **four Eternal Central (EC) retro formats as bundled preset data on
top of it**:

- Old School 93-94
- Old School 95
- Middle School
- Classic Magic

The four EC formats are the *proof*, not the *point*. The deliverable that
survives is the general engine; the four formats are its first four data
instances. Per this repo's CLAUDE.md "build for the class, not the card"
principle, we build the format *class*, then instantiate the four as data — we
do **not** hardcode four new `GameFormat` variants.

## Maintainer input — resolves the delivery-surface question (Open item 1)

A maintainer (matthewevans), given an informal, non-technical description of
this proposal, independently converged on: *"Something like 'FFA that's super
flexible and you can save a configuration as a custom format?'"* — i.e., start
from the existing flexible multiplayer format (Free-for-All), let a host tune
its knobs freely in the lobby they already use, and add a "save this as a
custom format" action, rather than picturing a from-scratch format-authoring
tool.

This does **not** replace the schema below — it resolves *which axis is the
primary entry point* and confirms delivery surface (c) over (b)-first. The
custom-format schema (§ below, PLAN.md §1) always had two axes; this input
corrects an emphasis mistake in how they were sequenced:

- **Axis A — structural/match config**: `starting_life`, `min_players`/
  `max_players`, `deck_size`, `singleton`, `range_of_influence`, `team_based`.
  These are **already `FormatConfig` fields today**
  (`crates/engine/src/types/format.rs:174-202`) and **already partially
  exposed** in the existing multiplayer lobby
  (`client/src/components/lobby/HostSetup.tsx` — starting life, player count,
  deck size are already host-adjustable there for a chosen built-in
  `GameFormat`). This is exactly what "FFA that's super flexible" names. The
  original PLAN.md draft treated this axis as an inherited afterthought
  ("structural params reuse `FormatConfig`'s existing fields (life/deck/etc.)")
  rather than a first-class part of `CustomFormatRules` — that was the gap.
- **Axis B — legality/era-rules config**: `legal_sets`, `banned`, `restricted`,
  `reprint_policy`, `LegacyRuleSet` (mana burn, damage-on-stack, pre-M10 Wish,
  legend-rule scope). This is genuinely new data with no existing UI surface,
  and it's what actually makes the four EC formats *rules-correct* — no amount
  of tuning Axis A knobs produces a faithful Old School 93-94. This axis keeps
  every finding already in RESEARCH.md/CONTEXT.md/PLAN.md; none of that work
  is discarded.

**Resolution of Open item 1**: ship (c) — one `CustomFormatDef`/
`CustomFormatRules` schema — but enter it through Axis A first, via the
*existing* lobby (`HostSetup.tsx` + "save as custom format"), not a new
designer screen and not (b)-first typed-preset-only delivery. The four EC
formats remain the validating case for Axis B and may still ship as audited
typed presets (a banned list should be curated, not free-typed by a player),
but they are no longer the thing that must land *before* anything else is
useful — the Axis A save-flow is smaller, ships independently, and is useful
to every casual multiplayer table, not just old-school players. See PLAN.md §1
and §7 for the schema and sequencing changes this implies.

## Further narrowing Axis B's MVP — Swedish Old School over the four EC formats

The follow-up ask: building `LegacyRuleSet` end-to-end (mana burn drop-site
hook, the damage-uses-the-stack combat rework, pre-M10 Wish exile access,
legend-rule scope) is real engine work with real risk, and doing all of it up
front makes the MVP harder to test, not easier. Two formats narrow Axis B's
first slice to something with **zero new legacy-rule engine work**:

- **Premodern needs no work at all.** It is already a native, working
  `GameFormat` (`FormatConfig::premodern()`) — fully modern rules, a
  set-window-restricted legal pool. It validates nothing new about this
  design; it's cited here only as an existing precedent for "old(er) card
  pool + entirely modern rules," the same shape Swedish Old School turns out
  to have.
- **Swedish Old School 93/94** — a *different, real ruleset* from the four EC
  formats already researched, independently verified this session via
  `oldschool-mtg.blogspot.com/p/banrestriction.html` (fetched directly, not
  from memory):
  - **Legal sets**: Alpha, Beta, Unlimited, Arabian Nights, Antiquities,
    Legends, The Dark, "Summer Magic" — the same era pool EC's 93-94 uses.
  - **Banned list**: empty — no card is fully banned under Swedish rules.
  - **Restricted list** (verbatim, 25 cards, one-copy maximum — corrected
    from an initial miscount of 23 flagged by review; recounted directly
    against the enumerated names below): Ancestral
    Recall, Balance, Black Lotus, Braingeyser, Channel, Chaos Orb, Contract
    from Below, Darkpact, Demonic Tutor, Library of Alexandria, Mana Drain,
    Mind Twist, Mishra's Workshop, Mox Emerald, Mox Jet, Mox Pearl, Mox Ruby,
    Mox Sapphire, Regrowth, Sol Ring, Strip Mine, Tempest Efreet, Time Walk,
    Timetwister, Wheel of Fortune — a **different list from EC's 93-94**, not
    a duplicate (confirms Axis B's `restricted: Vec<CardName>` shape needs to
    be a real per-format list, not a shared constant).
  - **Ante cards**: "must be removed before play unless the tournament is
    specifically played for ante" (Bronze Tablet, Contract from Below,
    Darkpact, Demonic Attorney, Jeweled Bird, Rebirth, Tempest Efreet) — a
    third list-shaped rule distinct from banned/restricted, **not yet modeled
    anywhere in this schema**. Flagged as a real gap to resolve at
    implementation time (likely a third named list, or folded into `banned`
    conditionally on an `ante_enabled` toggle this proposal does not yet
    have) — do not silently drop it.
  - **Reprint policy**: the primary source does **not** state one explicitly
    (only "Only English versions are allowed in Oldschool") — a secondary
    source (`mtgoldframe.com`) claims no Revised-or-later reprints are
    allowed, but that was **not** independently confirmed against the primary
    rules page this session. Do not encode `ReprintPolicy::OriginalPrintingsOnly`
    for this preset without re-verifying against the primary source or asking
    the community directly — flagged open, not resolved.
  - **Legacy rules**: the primary source makes **no mention** of mana burn,
    damage-on-the-stack, old Wish templating, or a modified legend rule —
    the plain reading is that Swedish Old School uses fully **modern** rules
    on the restricted-era pool. This is structurally the same shape as the
    already-documented **Classic Legacy** validation example (above): old
    card pool, modern rule engine, `LegacyRuleSet` at its all-`false`/
    `Modern` defaults. It is a *second* independent confirmation that
    card-pool era and legacy-rules toggles are properly decoupled axes, and
    the cheapest possible Axis B instance to build and test — no
    `LegacyRuleSet` engine wiring is exercised at all.

**Revised sequencing implication** (see PLAN.md §8): Phase 1 ships the general
engine + Axis A lobby-save + **Swedish Old School as the only new Axis B
preset**, with `LegacyRuleSet` fields present in the schema (so the type is
future-proof) but never exercised by anything Phase 1 actually ships. Phase 2
is the four EC formats (which genuinely need mana burn, and for Middle
School/Classic Magic, damage-on-the-stack) plus the `LegacyRuleSet` engine
wiring those require. This is a re-sequencing, not a scope cut — all four EC
formats and the full legacy-rules axis remain the target; they simply move to
a phase 2 that ships once the schema and Axis A/Axis B mechanics are already
proven end-to-end by something smaller and testable.

## Maintainer review round 2 — CHANGES_REQUESTED, addressed

matthewevans reviewed the previous head and requested changes on five
design-correctness grounds. All five are addressed in this revision; see the
cited PLAN.md sections for the actual schema/wiring changes.

1. **Axis-A unrestricted-legality bug (real, confirmed).** The evaluator design
   (PLAN.md §3) checked "legal iff a printing's set ∈ `legal_sets`" — an empty
   `Vec` (Axis A's default, since a lobby-saved format sets no restriction)
   makes every card fail, not none. Fixed: `legal_sets` is now
   `Option<Vec<SetCode>>` — `None` means unrestricted (every card passes the
   pool check), `Some(list)` means restricted to that list, matching this
   repo's own convention of using `Option<T>` over an ambiguous sentinel
   value (CLAUDE.md's `Option<ControllerRef>` example). See PLAN.md §1 and §3.
2. **`StructuralRules` was missing behavior-bearing `FormatConfig` fields
   (real, confirmed).** Re-read `FormatConfig`'s full field list
   (`types/format.rs:174-212`) directly rather than from memory. Fixed:
   `StructuralRules` now includes `command_zone`, `commander_damage_threshold`,
   and `archenemy_player` (all independently meaningful, not derived).
   `uses_commander` is derived as `commander_damage_threshold.is_some()`
   rather than stored redundantly. `supplies_fixed_deck` is always `false`
   for `Custom` (no custom-format use case for an auto-supplied deck exists;
   flagged, not silently dropped, if that need ever arises).
   `allow_debug_actions` is correctly excluded — its own doc comment says
   it is "orthogonal to format," a session capability flag, not format
   identity. **New finding**: `sideboard_policy` is not a `FormatConfig`
   field at all — it's a `GameFormat` *method*
   (`format.rs:270`, `fn sideboard_policy(self) -> SideboardPolicy`), so
   `GameFormat::Custom` has no derivation source for it the way built-in
   formats do. `StructuralRules` gains an explicit `sideboard_policy` field
   to fill that gap. See PLAN.md §1.
3. **No identity/persistence/transport contract for a lobby-saved format
   (real, confirmed — the largest gap).** The original design conflated two
   different identity concerns: (a) how a resolved ruleset is agreed on by
   both peers *in one active game*, and (b) how a *named, reusable* saved
   format persists for one player across many future games. (a) was already
   solved — `FormatConfig.custom_rules` carries the full resolved
   `CustomFormatRules` payload, not a lookup key, so a peer never needs to
   already know a format by ID to play it. (b) was never designed. Resolved
   by explicitly separating them: the engine's `CustomFormatId` stays a
   lightweight, `Copy`, per-`GameState` transport tag (stable/well-known only
   for the registry-backed EC/Swedish presets; an ad-hoc lobby save can use a
   fixed sentinel value, since the full payload — not the ID — is what
   travels and is interpreted). A player's "my saved formats" library is a
   **client-side-only** concern (local storage / profile-scoped, its own
   name+identity, never an engine or WASM type) that packages a
   `CustomFormatRules` value at game-start time. This also resolves the
   version-skew question: an older client that doesn't know the
   `GameFormat::Custom` enum variant at all can't be rescued by
   `serde(default)` on the payload (the failure is at the enum-variant level,
   not the payload level) — flagged as a real, explicit compatibility check
   the lobby-join flow needs (reject with a clear message, don't crash on
   deserialization), not something this schema revision can silently fix.
   See PLAN.md §1 (new subsection) and §7.
4. **Mana-burn behavioral/timing error (real, confirmed against this engine's
   own code and CR text, and against direct correction: mana burn keys off
   crossing a real MTG *phase* boundary, not the engine's finer-grained
   `Phase` enum, which flattens MTG's steps and phases into one 11-variant
   list — e.g. `DeclareAttackers` → `DeclareBlockers` is a step transition
   within the Combat phase, not a phase-end, even though the engine's own
   `Phase` enum treats every one of those as a variant transition).**
   Verified two things directly this session, not from memory:
   - `docs/MagicCompRules.txt:8278` (the obsolete-rules glossary): "unspent
     mana caused a player to **lose life**" — life loss, not damage. The
     original PLAN.md/RESEARCH.md draft's "deal that many damage" framing
     was wrong.
   - The engine already has a generic, existing mechanism for exactly this
     shape: `player_unspent_mana_loss_causes_life_loss`
     (`static_abilities.rs:1237`, backed by `StaticMode::UnspentManaLossCausesLifeLoss`)
     is checked inside `apply_empty_mana_pool_event` (`turns.rs`, doc comment:
     "CR 106.4 + CR 703.4q: Apply the final replacement-ordered mana
     dispositions as the step or phase ends, then apply one aggregate
     Yurlok-class life-loss event") — this fires on **every** `Phase` enum
     transition (both true steps and true phases, per modern CR 500.5),
     which is correct for the existing Yurlok-class static ability but too
     fine-grained for old-school mana burn, which must fire only when
     actually leaving one of the 5 real MTG phases (Beginning, Precombat
     Main, Combat, Postcombat Main, Ending) — not on every step within one.
   See PLAN.md §4 for the corrected wiring: a phase-group boundary check
   gating a *second*, independent contribution to the same life-loss event
   mechanism, alongside (not merged into) the existing Yurlok-class check.
5. **Preset data inconsistencies (real, confirmed and fixed).** Swedish Old
   School's restricted list was mislabeled "23 cards" when 25 are actually
   enumerated (fixed above); PLAN.md's `swedish_old_school()` sketch had
   dropped "Summer Magic" from the legal-sets list that this document already
   stated (fixed in PLAN.md §2). Classic Magic's restricted list was labeled
   "(37)" but enumerates 44 names in RESEARCH.md — recounted directly against
   the verbatim list and fixed to "(44)" in both RESEARCH.md and PLAN.md §2.

## Confirmed (verified against source this session)

- **`GameFormat` is a closed `Copy` enum** with ~23 variants, threaded through
  many exhaustive matches (`legality_format`, `sideboard_policy`,
  `grants_free_first_mulligan`, `uses_commander`, `supplies_fixed_deck`,
  `label`, `for_format`, plus `deck_validation.rs::format_compatibility_check`).
  Verified `format.rs` in full. Adding an *additive* variant is the established
  extension pattern (see the `GameFormat::Limited` prior art, phase 53).
- **Legality today is externally sourced per-card.** `card_db.rs:109` calls
  `normalize_legalities(&entry.legalities)`; the raw map comes from MTGJSON
  `AtomicCard.legalities` via the export binary (`bin/oracle_gen.rs:778`,
  `883`). `LegalityFormat::from_key` (`legality.rs:69`) **drops any key it does
  not recognize**. There is **no** `oldschool` / `middleschool` / `classic`
  variant in `LegalityFormat`, so even MTGJSON's `oldschool` key (if present) is
  silently discarded today. The existing test at `legality.rs:293-297`
  explicitly documents that unknown keys like `"oldschool"` are dropped. →
  **None of the four EC formats have any signal in the current pipeline.**
  Confirmed by reading `legality.rs`, `card_db.rs`, and `oracle_gen.rs`.
- **`set_gating.rs` does NOT fit** as a format-restriction mechanism. Confirmed
  by reading it in full: it is a `GATED_SETS`-env-var-driven, *generation-time*
  pre-release embargo tool that overrides a card's legalities to
  `all_formats_banned()`. It is narrow deployment tooling, not a runtime,
  general, per-format card-pool mechanism. Useful only as a *rough shape
  reference* for "how this codebase does deployment-level config", not an
  architecture to copy.
- **Set-membership data exists and is the right key.**
  `CardDatabase::printings_for(name) -> Option<&[String]>` (`card_db.rs:227`)
  returns the set codes a card was printed in. This is the building block a
  set-list-driven custom format needs.
- **`DeckCopyLimit::UpTo(1)` is the WRONG layer for a format restricted list**
  (see RESEARCH.md §4). The correct existing building block is the
  format-level restricted-list enforcer `restricted_copy_violations`
  (`deck_validation.rs:2462`), which already implements the CR 100.2b 1-copy
  ceiling generically.
- **Mana burn is small; "damage uses the stack" is large.** Verified hook
  points (RESEARCH.md §5). Mana burn is a localized addition at the mana-pool
  drop site. "Damage uses the stack" is a fundamental reversal of CR 510.2 and a
  deep combat/stack/priority rework.
- **Legend-rule scope change is REAL and the engine is hardcoded to modern; a
  legacy flag is SMALL — but no EC preset uses it.** Fully investigated
  (RESEARCH.md §10). The legend rule (introduced by *Legends* 1994, legal in
  93-94/95) was **global / any-controller** through 2013, then M14 (2013-07-13)
  made it **per-controller + choice** (current CR 704.5j,
  `MagicCompRules.txt:5510`). The engine's `check_legend_rule`
  (`sba.rs:902-956`) is hardcoded to the modern per-controller-with-choice form
  (loops per player, filters `controller == player_id`, pauses with
  `WaitingFor::ChooseLegend`). Re-adding the pre-M14 global-choiceless form is
  SMALL — it mirrors the existing `check_world_rule` (`sba.rs:1348`) shape and
  reuses the shared SBA departure mover. Modeled as a typed
  `LegendRuleScope { Modern, PreM14AnyController }` enum on `LegacyRuleSet` (not a
  bool). **HONEST CAVEAT:** EC's published rulesets do **not** list a legend-rule
  reversion (their only legacy exceptions are mana burn / damage-on-stack /
  wish), so all four EC presets default to `Modern`; the scope enum ships as a
  *general* historical-rules axis (like Block Constructed's `mana_burn`), not an
  EC-preset behavior. Planeswalker uniqueness needs no flag — the four pools end
  at Scourge (2003) and planeswalkers postdate that (Lorwyn 2007). The
  planeswalker-uniqueness → legend-rule fold happened at **Ixalan (2017-09-28)**,
  not Dominaria 2018.
- **Pre-M10 Wish templating is small and is a REAL functional difference.**
  Fully investigated (RESEARCH.md §9). It reverts the M10 change that made exile
  an in-game zone (CR 400.11/400.11a): pre-M10, Wishes could retrieve an owned
  card removed from the game, not just from the sideboard. The engine already
  implements the modern (post-M10) Wish cycle generically
  (`Effect::SearchOutsideGame`, `OutsideGameSourcePool::Sideboard`) AND already
  implements owned-face-up-exile retrieval for the Karn/Coax class
  (`SideboardAndFaceUpExile` + tested collector/mover in
  `search_outside_game.rs`). The legacy flag is therefore a one-line pool-widening
  at one existing resolver hook — SMALL, not the "Medium" the first pass guessed.
  Flag renamed `pre_m10_wish_templating` → `pre_m10_wish_reaches_exile` (it is a
  pool-scope toggle, not a wording change).

## Open (needs a human decision — do NOT resolve unilaterally)

1. ~~**Delivery surface**~~ — **RESOLVED**, see "Maintainer input" section
   above: (c), entered via Axis A (structural config) through the existing
   lobby first, Axis B (legality/legacy-rules) as audited presets validating
   the same schema. PLAN.md §1 and §7 updated accordingly.
2. **Reprint-policy fidelity — corrected/sharpened this round.** The original
   framing ("no frame/art metadata per printing, full stop") was too
   pessimistic. There are **two disjoint printing systems in this codebase
   today, and only one of them lacks frame data**:
   - **Engine/MTGJSON side** (`CardDatabase::printings_for`,
     `crates/engine/src/database/card_db.rs:227`): a bare `Vec<String>` of set
     codes, sourced from MTGJSON `AtomicCards.json`
     (`crates/engine/src/database/mtgjson.rs:78`). This format is
     structurally oracle-level, not printing-level — frame/border/full-art
     data **cannot** exist here; this is a schema-level absence, not a
     dropped-field ingestion gap. Confirmed empirically: `printings_for()`'s
     order is alphabetical-by-set-code (e.g. `10E` before `AKH`), not
     chronological — "pick the oldest legal printing" cannot use this order
     as-is.
   - **Frontend/Scryfall side** (`client/src/services/scryfall.ts`,
     `scryfall-printings.json` generated by
     `scripts/gen-scryfall-printings.sh`): a fully-built, **already-working**
     printing system with real per-printing `released_at`, `border_color`,
     `frame_effects`, `full_art`, `collector_number` — sourced from Scryfall's
     bulk data, not MTGJSON. `pickOldestPrinting()`
     (`client/src/services/scryfall.ts:136`) already implements "pick the
     oldest printing" as working code, and `preferencesStore.ts`'s
     `ArtChainEntry` (a typed union: `{type:"oldest"}` /
     `{type:"prefer_borderless"}` / etc., with a real v5→v6 migration
     upgrading a flat enum into this richer chain type) is a per-player
     cosmetic preference for exactly this.
   - **These two systems are completely disconnected** — they only share an
     oracle-id join key. The frontend's printing/frame system has no
     awareness of format legality; the engine's legality system has no
     printing/frame data. **The actual gap is not "no frame data exists,"
     it's "the frame data that exists has never been cross-referenced against
     format/legal-set data."**
   - Confirmed no per-user printing choice flows through the engine at all
     today: `printings_for()` has zero WASM/frontend call sites
     (`grep -rn "printings_for" crates/` → only an offline coverage tool and
     a test). `GameObject`/`PrintedCardRef`
     (`crates/engine/src/types/card.rs:76`) carries oracle_id + face_name
     only, never a set code.
   - Release-date data for a set-code→date join **already exists**
     (`SetCatalog`/`SetMeta.release_date`,
     `crates/engine/src/database/set_catalog.rs:78`, loaded from MTGJSON's
     `SetList.json`) and is even already projected to the frontend for an
     unrelated purpose (`client/public/set-list.json`, deck-builder set
     filter). So an "oldest legal printing" ordering is a **wiring problem**,
     not a new-data-acquisition problem — cheaper than true frame-level
     enforcement, which would need Scryfall's frame fields cross-referenced
     against the format's legal-set list, and a decision on whether that
     cross-reference is new engine-owned derived state (per "engine owns all
     logic") or an extension of the frontend's existing cosmetic
     `ArtChainEntry` preference system seeded by the format (a real
     architectural fork — see PLAN.md's new `PrintingDefault` note). Whether
     the approximation (set-code-list membership only) is acceptable for v1,
     or whether the Scryfall-engine cross-reference should be built, is a
     product decision.
3. **Classic Magic B&R cadence.** EC updates Classic's banlist twice yearly
   (Jan 1 / Jul 1). Bundled-preset data is version-controlled, so updates are
   ordinary edits — but whether we want a dated/versioned banlist history is
   open.
4. **Draw/tiebreaker resolution mechanics** (generalizing the Chaos Orb
   tiebreaker and the foreign-card-identification convention above): both are
   instances of a "how does this table resolve an otherwise-undecided
   outcome" hook, not an in-game rule. Confirmed via grep (`crates/draft-core`,
   `server-core`) that phase.rs has **no match-level concept at all today** —
   no best-of-N, no tournament round, no draw/tiebreak state machine; the
   engine models single games only. So this genuinely has two different
   shapes depending on a product decision this doc should not make
   unilaterally: (a) a **tournament-level policy** ("draws are not permitted;
   play resolves until decided," which needs no new state, since it's just a
   procedural rule about what happens after a game/match ends) or (b) a
   **per-format-configurable "how to decide a draw" hook**, which would
   require inventing a match/round concept above the single-game engine
   first — a materially larger scope than this custom-format engine. Given
   no match-level structure exists, this is flagged as **out of scope for
   the custom-format engine itself** and, if ever pursued, belongs to a
   separate, later "tournament/match structure" design, not bundled into
   `CustomFormatDef`/`LegacyRuleSet` here.
5. **Ante-card handling (new, from the Swedish Old School preset).** "Must be
   removed before play unless the tournament is specifically played for ante"
   is a *third* list-shaped rule, distinct from banned (illegal outright) and
   restricted (legal, max 1) — the schema has no slot for it today. Needs a
   decision: a third named list on `LegalityRules`, or folding it into
   `banned` gated by a new `ante_enabled: bool`/toggle. Low urgency (only 7
   cards; ante itself has no in-engine support and none is proposed here), but
   should not be silently dropped when the preset ships.
6. **Swedish Old School's reprint policy is unconfirmed.** Only "English
   versions are allowed" is stated by the primary source
   (`oldschool-mtg.blogspot.com/p/banrestriction.html`); a secondary source's
   "no Revised-or-later reprints" claim was not independently verified this
   session. Do not encode `ReprintPolicy::OriginalPrintingsOnly` for this
   preset without re-confirming against the primary source or the community
   directly.

## Source data

- **Authoritative EC ruleset**, re-verified this session (2026-07-07) against
  `https://github.com/northern-information/lordsofthepit.com/blob/main/src/pages/formats.md`
  (raw fetched). Full verbatim card lists captured in RESEARCH.md §1.
- **Prior art**: `GameFormat::Limited` planning cycle, phase 53. Retrieved via
  `git show 80404a98b:.planning/phases/53-limited-draft-core/53-01-SUMMARY.md`.
  Summarized in RESEARCH.md §7 (Analogous Trace).
- **Swedish Old School 93/94 ruleset**, fetched this session (2026-07-15) from
  `http://oldschool-mtg.blogspot.com/p/banrestriction.html` — the primary
  source for legal sets, the empty banned list, the 23-card restricted list,
  the ante-card carve-out, and the (absent) legacy-rules mentions used above.

## Relationship to adjacent work — scope boundaries (do NOT accommodate here)

- **Vanguard (CR 902) is orthogonal to this design — do not entangle them.**
  GitHub issue phase-rs/phase#5056 ("Implement the Vanguard variant (CR 902)")
  requests Vanguard support; verified **still OPEN and unaddressed**
  (`gh issue view 5056 --json state` → `"OPEN"`, 2026-07-07). Vanguard is
  genuinely unimplemented (no `CoreType::Vanguard`, no `GameFormat::Vanguard`, no
  Vanguard cards in `card-data.json`). Per the issue's own analysis, phase.rs
  already implements **three of the four** CR "nontraditional card lives in the
  command zone" casual variants — Planechase (CR 901, `game/planechase.rs`),
  Commander (CR 903, `game/commander.rs`), Archenemy (CR 904, `game/archenemy.rs`)
  — and Vanguard (CR 902) is "structurally the simplest of the four… a per-player,
  game-long static modifier plus a command-zone ability source, which is a strict
  subset of what archenemy.rs/planechase.rs already do." **Vanguard therefore
  belongs to the existing command-zone-supplemental-card family, NOT to the
  `CustomFormatDef`/`LegacyRuleSet` mechanism designed here.** The two are
  independent: this custom-format engine neither depends on nor blocks Vanguard,
  and this design does **not** need to accommodate it. Flagged so a reader does
  not mistakenly try to fold Vanguard into the custom-format layer.

- **Duplicate-check pass against all open upstream issues/branches (2026-07-07,
  before taking this to the maintainer).** Searched `phase-rs/phase` open
  issues/PRs and all `origin` branch names for: format, old school, block
  constructed, legendary rule, mana burn, damage stack, wish, vanguard, audio,
  video, voice chat, webrtc, spectator, watch game, replay, commentary,
  microphone/camera. Findings:
  - **No duplicate or conflicting open issue/PR/branch exists for this
    custom-format-engine design, the audio/video-chat design (phase 59), or
    the watch-game-mode design (phase 60).** Clear to proceed to the
    maintainer conversation on all three without a collision risk.
  - **phase-rs/phase#5056 (Vanguard)** — already covered above, confirmed
    still open, confirmed orthogonal.
  - **phase-rs/phase#5169 ("New Format Plan: Dandan"), OPEN, created
    2026-07-06 (one day before this check) — worth reading before the
    maintainer conversation, not a duplicate.** An unclaimed (no linked PR,
    no "token" claimed in comments), extremely detailed community-authored
    engine-planner-style implementation plan for a NEW `GameFormat::Dandan`
    shared-library format (both players draw from one shared 80-card library
    + graveyard; a real Wizards Secret Lair promo format). This is a
    **different actual format** from anything in this design (not an EC
    retro format, no legal-sets/banlist/legacy-rules axis) — but it is the
    **same class of upstream activity**: another contributor extending
    `GameFormat`/`FormatConfig` right now. Two things worth surfacing to the
    maintainer alongside this design:
    1. It's live precedent for how the maintainer currently accepts new
       formats: **one bespoke `GameFormat` enum variant + `FormatConfig`
       constructor per format**, not a generic data-driven mechanism. That's
       a legitimate approach for a format needing genuinely new engine
       *behavior* (Dandan needs new shared-zone/deal-order/ownership
       machinery no config toggle could express) — and is exactly why this
       design proposes `GameFormat::Custom` only for the *configuration*
       axis (legal sets, banned/restricted lists, era-rule toggles), not as
       a replacement for bespoke variants when a format needs new behavior.
       **Dandan's own plan already argues for `SharedZones` as a reusable
       building block** ("future shared-deck formats reuse the same
       `SharedZones` descriptor"), and that generality is not hypothetical:
       verified via WebSearch (`eternalcentral.com/type4`, plus a Riptide Lab
       forum thread grouping "Cubelets, Dandân, and other shared deck
       formats" together) that **Type 4** — a real, decades-old casual
       format (unlimited mana, no lands, one spell/turn, chaos targeting,
       last-player-standing) — has a documented variant where "all players
       use the Type 4 card pool as a shared library," structurally the same
       zone-sharing shape as Dandan. Type 4's *other* rules (infinite mana,
       no lands) are unrelated to this design and out of scope, but it's a
       second real, named format that would want the exact `SharedZones`/
       `library_of`/`graveyard_of` building block Dandan's plan proposes —
       reinforcing that it's worth building generally rather than
       Dandan-specifically, consistent with "build for the class."
       Worth confirming the maintainer sees these as complementary, not
       competing, approaches.
    2. No technical overlap or conflict either way: Dandan's shared-zone
       accessors and interleaved dealer touch `types/game_state.rs`/
       `mulligan.rs`/`zones.rs`; this design's touch points
       (`types/format.rs`, `game/deck_validation.rs`, a new
       `evaluate_custom_format`) are additive to the same `GameFormat` enum
       but do not share implementation surface.
  - **phase-rs/phase#4613 ("Add action-based game replay system"), OPEN,
    created 2026-06-29 — related-but-distinct from phase 60 (watch-game
    mode), not a duplicate.** Requests deterministic post-hoc replay
    export/scrub (record a finished/in-progress local game, load it later,
    step through in a read-only viewer) — a different use case from phase
    60's live spectating (phase 60 found spectator mode already fully
    implemented and live). Both land on a similar "read-only board view" UI
    surface, so worth a one-line cross-reference in phase 60's docs, but
    they solve different problems and neither blocks the other.
  - No open issue or branch name relates to audio/video/voice/webrtc chat,
    or to spectator/watch-game mode as a feature request (consistent with
    phase 60's finding that spectator mode already shipped without a
    tracking issue ever being filed for it).

## Additional validation example — Classic Legacy / "Lost Legacy" (community, non-EC)

A fifth real-world community format, independent of Eternal Central, that
further validates the schema's generality. Two distinct sources, both
user-provided this session — kept separate since they describe two different
things under overlapping branding:

- **Classic Legacy** (`https://classiclegacymtg.com/`, fetched 2026-07-07):
  Alpha through Rise of the Eldrazi (2010). **No restricted list at all** — a
  57-card banned list only (all five Moxes, Black Lotus, Ancestral Recall,
  Demonic Tutor, Yawgmoth's Will, Tinker, etc. are fully **banned**, not
  restricted-to-1 the way EC's 93-94 treats them). Explicitly states three
  rules deltas from current Comprehensive Rules: "The London Mulligan is
  used" (i.e. explicitly the *modern* default, not an old-style mulligan),
  "There is NO mana burn," and "Combat damage does not use the stack."
  **This is structurally the inverse of EC's Middle School/Classic Magic**:
  an old, broad card pool paired with entirely *modern* choices on every
  `LegacyRuleSet` axis. That is a stronger decoupling proof than Block
  Constructed (§ above) — it shows card-pool era and legacy-rules flags are
  not just independently *toggleable in principle*, a real, currently-run
  community format actually uses that independence (old pool + modern
  rules), not only the "old pool + old rules" combination the four EC
  presets happen to use. No new `LegacyRuleSet` field is needed to support
  it — it is fully expressible by *not* setting any of the flags already
  designed, on a broader `legal_sets` list.
- **"Lost Legacy 606"** (a dated tab titled "Lost Legacy" / ref. `606`,
  user-pasted content from a linked Google Sheet at
  `docs.google.com/spreadsheets/.../1rf9U9k93_.../htmlview#gid=0` — the sheet
  itself is a JS-rendered app and was **not** independently fetchable;
  content below is transcribed as pasted by the user, not independently
  re-verified against the live sheet). Labelled "(Legacy June 2006 - Pre
  Coldsnap)", sets "Alpha - Dissension/9th Ed., + Portals and Starter" — a
  **dated historical snapshot** of what tournament Legacy looked like at a
  specific point in time, distinct from the single fixed "Classic Legacy"
  ruleset above. This suggests the source project maintains *multiple*
  dated snapshots (naming pattern implies `606` = "2006, month 06"), which
  is an interesting further validation of "format as data" (a snapshot is
  just another `CustomFormatDef` instance with its own `legal_sets` cutoff
  and its own banned list) — **noted as a potential future direction, not
  investigated further and not in scope for the four-EC-preset MVP.**

  Rules deltas listed for this specific snapshot, and how each maps to what
  is already designed (or explicitly does not):
  - "Wishes can retrieve exiled cards" — **independently confirms**
    RESEARCH.md §9's pre-M10 Wish finding via a second, unrelated source.
  - "Mana Burn is in effect" + "Mana empties at end of phase" — **confirms**
    the mana-burn hook point already identified (RESEARCH.md §5): the
    "empties at end of phase" phrasing matches the engine's actual
    step/phase-boundary pool-emptying mechanism almost exactly.
  - "Damage uses the stack" — same as EC Middle School/Classic Magic,
    already modeled.
  - **"Same Legends are buried on resolution"** — this is the legend rule,
    described using **pre-2004 templating** ("bury" was a distinct keyword
    action for "destroy, cannot be regenerated" before being folded into
    plain "destroy" templating around 2004). The phrase "on resolution" is
    a potentially significant additional data point for RESEARCH.md §10's
    `LegendRuleScope::PreM14AnyController`: it may indicate the *original*
    legend rule was not a continuously-checked state-based action the way
    both the modern rule and the currently-designed `PreM14AnyController`
    variant are, but instead resolved as an immediate effect at the moment
    a legendary permanent entered (or a spell/ability finished resolving).
    **This needs a follow-up rules-history check before being treated as
    confirmed** — flagged here rather than silently folded into §10's
    existing SMALL-effort estimate, since a genuinely different resolution
    *timing* (immediate effect vs. SBA) could change that estimate.
  - **"CMC of split card in hand is X or X not the total"** — a real
    historical difference in how split cards' mana value was calculated
    outside the stack, but the precise mechanic and the years it applied
    are **not verified this session** (nothing in RESEARCH.md addresses
    this). Recorded as an **open flag candidate, not yet designed** —
    do not add a `LegacyRuleSet` field for this without first verifying the
    actual historical rule via an authoritative source, per this project's
    "verify before annotating" discipline.
  - **"Draws determined by foreign card identification"** — resolved via
    direct user explanation (not independently re-verified against an
    external source, per this doc's provenance discipline): each player
    shows their opponent an obscure card printed in a non-English language;
    the cards are shown back and forth, and the first player to fail to
    correctly identify/name the shown card loses. This reads as a
    paper-tournament social/anti-counterfeiting convention — a
    knowledge-based substitute for a coin flip or the Chaos Orb tiebreaker
    (see the EC "tied match" ruling above), not an in-game rule. It has no
    engine-relevant behavior: phase.rs already needs its own standard
    mechanism for resolving "who plays first" / breaking ties, and this is
    simply how humans did that at the table in an old-school paper-Magic
    setting. **No `LegacyRuleSet` flag needed for this.**
  - "Current Oracle on all cards" and "Reprints OK" — policy statements
    about which printing/wording to use, not engine-modelable rules deltas;
    same category as the already-noted reprint-fidelity open question
    (Open item 2, above).
  - "Proxies must be identifiable at arms length" — physical-tournament
    logistics with zero gameplay/engine relevance. **Explicitly out of
    engine scope**, noted only so a future reader does not wonder why it
    was dropped.

## Stretch goal (explicitly lower priority — do not design in depth)

- **Eternal Chaos** (a Lords-of-the-Pit variant on top of 93-94, NOT an
  EC-defined format): in-match booster-pack tutoring, a dynamically-built
  sideboard from opened packs, and a pre-match "Gentleman's Agreement" banlist.
  Depends on 93-94 already existing **plus** a genuinely new in-match
  pack-opening mechanic. Sequenced last; see PLAN.md §8.
  **Confirmed via WebSearch:** this in-match pack-opening mechanic is,
  structurally, exactly what the real card **Booster Tutor** (Unhinged;
  Oracle: "Open a sealed Magic booster pack, reveal the cards, and put one
  of them into your hand.") needs — open-a-pack, reveal, pick-one-to-hand.
  Whichever gets built first (the card's effect handler, generically, vs.
  the Eternal Chaos mechanic) is the reusable building block for the other;
  they should not be designed or implemented twice. Note the card's own
  effect is narrower (put one card into hand, not "build a sideboard from
  everything opened") — Eternal Chaos's dynamic-sideboard-from-packs piece
  is an additional layer on top of the same open-a-pack primitive, not a
  1:1 match.

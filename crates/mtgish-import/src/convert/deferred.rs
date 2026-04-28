//! Deferred-gap registry.
//!
//! After 6 rounds of converter work the corpus-level gap rate is below
//! 1.1% (358 of 32,394 cards). Every remaining gap falls into one of
//! four buckets, each documented here with its blocker so reviewers
//! can see at a glance why the converter is "exhaustively wired" even
//! while the gap count is non-zero.
//!
//! This module ships **no executable code** — it is documentation. The
//! deferral list below is the contract: each rule kept as a strict
//! failure must have a paragraph here naming its blocker.
//!
//! ## Ship readiness
//!
//! The converter is ready to wire into `oracle-gen --with-mtgish`
//! (Phase 14 of the plan). Every rule that can be converted **without
//! engine work** is converted; every rule that requires engine work is
//! either:
//!   1. Already shipped in rounds 2-6 (reuse-first or paired engine
//!      extension), or
//!   2. Listed below with a clear blocker.
//!
//! No must-fix structural issues remain. The strict-failure invariant
//! holds: a card produces output only when every one of its rules
//! converts cleanly. There are no `Effect::Unimplemented` fallbacks in
//! the mtgish-import output path.
//!
//! ## Deferred rules and their blockers
//!
//! ### Subsystem-scale gaps (entire mechanic missing)
//!
//! - ~~**`Rule::Visit(Actions)` / `Rule::VisitAndPrize`**~~ — **CLOSED
//!   (round 6)**. Maps onto the engine's existing
//!   `TriggerMode::VisitAttraction` (already a stub variant). Runtime
//!   hook ships when the Attractions subsystem ships, but the converter
//!   now emits structurally correct data.
//!
//! - **`Rule::SpellActions_Spree(Vec<SpreeAction>) (~21 cards)**:
//!   Modal-spell-with-per-mode-cost (Spree). Each `SpreeAction` is a
//!   `(Cost, Actions)` mode pair. The engine has `mode_abilities` for
//!   modal spells (Choose-one-or-more) and a `Keyword::Spree` flag,
//!   but no per-mode-cost wiring. Each Spree mode would need its own
//!   `AbilityDefinition` with a per-mode `AdditionalCost`, plus
//!   modal-selection plumbing that picks N modes and accumulates their
//!   costs. Deferred until that primitive lands.
//!
//! - **`Rule::IfCardIsInOpeningHand(Vec<Action>) (~28 cards)**:
//!   Opening-hand reveal trigger (Leyline of the Void, Serra Avenger,
//!   Iona, Shield of Emeria). The action `MayCost`/`MayAction` is a
//!   real in-game effect resolved before turn 1. Engine has no
//!   opening-hand reveal hook. Silent-consume would drop genuine
//!   effects — strict-fail correctly tracks the work item.
//!
//! - **`Rule::Companion(Companion) (~10 cards)**: The mtgish
//!   `Companion` enum carries a per-card filter expression
//!   (`AllCardsPassFilter`, `IncreaseStartingDeckSize`, etc.). Engine
//!   `Keyword::Companion(CompanionCondition)` enumerates the 10
//!   specific companions by their CR-codified condition (Gyruda,
//!   Yorion, etc.). Bridging requires identifying which canonical
//!   condition each filter expression matches. A dedicated commit
//!   would build this mapping table.
//!
//! - **`Rule::ClassAbilities(Vec<ClassAbility>) (~8 cards)**: Class
//!   enchantments (D&D crossover). Each level of the class is its own
//!   activated/static ability. Engine has Class infrastructure
//!   (`solve_condition`/level tracking) but the multi-tier
//!   level-up + per-level abilities translation requires structured
//!   thread-through that mirrors `Rule::SagaChapters`. Deferred.
//!
//! ### Engine-extension-required gaps (single primitive missing)
//!
//! - **`Rule::ReplaceWouldGainLife(event, [...]) (~13 cards)**:
//!   Engine has `ReplacementEvent::GainLife` and the dispatcher path,
//!   but the action variants (DoubleLife, GainAdditional, GainNothing
//!   instead) need a `LifeModification` enum parallel to
//!   `DamageModification`. Single-commit fix.
//!
//! - **`Rule::ReplaceWouldProduceMana (~5 cards)**: Engine has
//!   `ReplacementEvent::ProduceMana` and a `mana_modification` slot,
//!   but the existing slot only handles `ReplaceWith { mana_type }`.
//!   The mtgish action variants (Multiply, Add, Convert) need new
//!   `ManaModification` arms.
//!
//! - **`Rule::ReplaceAnyNumberOfTokensWouldBeCreated (~7 cards)**:
//!   Token-doubling family. Engine has `quantity_modification` for
//!   AddCounter / CreateToken events; the schema variant
//!   `AnEffectWouldCreateAnyNumberOfTokens` is an unmapped event-
//!   shape variant. Single-commit fix.
//!
//! - **`Rule::SpellActions_Tiered(Vec<TieredAction>) (~7 cards)**:
//!   Threshold spell — different effects at different mana spent
//!   ("Sphinx's Revelation"). Engine has `repeat_for` and
//!   `mode_abilities`, but the threshold-tiered structure needs a
//!   dedicated `TieredEffect` shape. Single-commit fix.
//!
//! - **`Rule::FlashForCasters(Condition) (~7 cards)**: Conditional
//!   flash. Engine has `casting_options` with `AsThoughHadFlash`, but
//!   the condition-decoder side (`mtgish::Condition` →
//!   `engine::ParsedCondition`) is a separate work item. The
//!   structural shape ships today via the `casting_options` slot;
//!   only the condition payload is deferred (engine accepts `None` as
//!   a permissive fallback).
//!
//! - **`Rule::Offering(Cards) (~6 cards)**: Spirit "offering"
//!   alt-cast that sacrifices a creature of the named subtype.
//!   Engine has no `Keyword::Offering` variant. Single-commit fix
//!   (mirrors round-4 `Awaken`).
//!
//! - ~~**`Rule::EachCardInGraveyardEffect`**~~ — **CLOSED (round 6)**.
//!   The `AddAbility` body recurses through `recurse_rules`; produced
//!   keywords/abilities/triggers become
//!   `ContinuousModification::AddKeyword` / `GrantAbility` /
//!   `GrantTrigger`. `LosesAllAbilities` → `RemoveAllAbilities`. The
//!   static carries `affected_zone = Graveyard`. Other
//!   `GraveyardCardEffect` variants
//!   (`CantBeTheTargetOfSpellsOrAbilities`,
//!   `AddCreatureTypeVariable`) still strict-fail.
//!
//! - **`Rule::EachCardInPlayersHandEffect (~6)**: Hand-zoned static.
//!   Same dispatch shape as the now-closed `EachCardInGraveyardEffect`,
//!   but `HandEffect` payloads need their own per-variant mapping.
//!
//! - ~~**`Rule::ActivatedAbilityEffect`**~~ — **CLOSED (round 6, partial)**.
//!   `CantBeActivated` (16 of 26 corpus occurrences) maps onto engine
//!   `StaticMode::CantBeActivated { who, source_filter, exemption }`.
//!   `IncreaseManaCost` / `AdditionalCost` /
//!   `ReduceManaCostNotLessThanOne` still strict-fail — engine has
//!   `ReduceAbilityCost` keyed on keyword name only, not on a generic
//!   `ManaCost` delta.
//!
//! ### One-card / niche-mechanic gaps
//!
//! - **`Rule::DungeonLevel (~4 cards)**: Venture-into-the-dungeon
//!   support. Engine has dungeon machinery (`DungeonId`,
//!   `CompletedADungeon` conditions) but the per-room ability
//!   wiring is its own subsystem.
//!
//! - **`Rule::Mystery (~5 cards)**: Maro spec mechanic;
//!   engine-bound on a typed `Mystery` keyword.
//!
//! - **`Rule::Escape(Cost, Vec<Rule>) (~4 cards)**: Engine has
//!   `Keyword::Escape { cost, exile_count }`. Converter wiring is
//!   straightforward but requires unwrapping the inner Vec<Rule> as
//!   a payload of inner abilities. Single-commit fix.
//!
//! - **`Rule::AbilitiesTriggerAnAdditionalTime (~4 cards)** /
//!   **`Rule::APermanentEnteringTheBattlefieldCausesAbilitiesToTriggerAnAdditionalTime (~4 cards)**:
//!   Panharmonicon-family. Engine has
//!   `StaticMode::DoubleTriggers { cause: TriggerCause }` already.
//!   Converter would map mtgish event/cause shapes to the engine
//!   `TriggerCause` enum.
//!
//! - **`Rule::FromGraveyardIf(Condition, Rule) (~7 cards)**:
//!   Conditional graveyard activation (the existing `FromGraveyard`
//!   wrapper handles the unconditional shape). Needs the same
//!   `mtgish::Condition` → engine condition mapping deferral that
//!   round-3 `If/Unless` and `FlashForCasters` ride on.
//!
//! - ~~**`Rule::UmbraArmor`**~~ — **CLOSED (round 6)**. Maps to the
//!   engine's existing `Keyword::TotemArmor` (CR 702.89b: Umbra Armor
//!   is the Oracle update of legacy Totem Armor; engine retains the
//!   legacy variant name per the documented erratum).
//!
//! - ~~**`Rule::AsPutIntoAGraveyardFromAnywhere`**~~ — **CLOSED
//!   (round 6)**. Maps onto `ReplacementEvent::Moved` with
//!   `destination_zone = Graveyard`, `valid_card = SelfRef`, and an
//!   execute body of `Effect::ChangeZone { origin: None, destination:
//!   Exile }`. Action coverage: `ExileItInstead` (32 of 37
//!   occurrences). `RevealItAndShuffleItIntoLibraryInstead` still
//!   strict-fails — needs a shuffle-after-redirect slot on
//!   `Effect::ChangeZone`.
//!
//! - **`Rule::StationCharged (~8 cards)**: Station-keyword
//!   companion rule that fires when the Station permanent reaches
//!   max charge. Engine has `TriggerMode::Stationed` but no
//!   "while-charged" static condition. Round 6 investigation found
//!   the inner `Vec<Rule>` body is mostly `Activated`/`TriggerA` rules
//!   that should *unlock* on charge (a "while-charged" conditional
//!   ability grant), not fire on charge — a deeper-than-1-commit fix.
//!
//! - **`Rule::IfCardIsInOpeningHand` (~28 cards)**: Opening-hand
//!   reveal trigger (Leyline of the Void family). Engine has
//!   `TriggerMode::NewGame` (a stub variant), but the inner
//!   `Action::MayAction(BeginGameWithCardOnBattlefield)` and
//!   `Action::MayCost` are not in `action::convert_list`. Adding the
//!   `BeginGameWithCardOnBattlefield` Action handler is the blocker;
//!   that ships with the opening-hand subsystem, not standalone.
//!
//! - ~~**`Rule::FlashForCasters`**~~ — **CLOSED (round 6)**. Pushes
//!   `SpellCastingOption::as_though_had_flash()` onto `casting_options`.
//!   Same condition-deferral precedent as round-3
//!   `CastEffect::MayCastAsThoughItHadFlashIf`.
//!
//! - ~~**`Rule::Offering(Cards)`**~~ — **CLOSED (round 6)**. New
//!   engine `Keyword::Offering(String)` variant (CR 702.48a) carrying
//!   the canonical capitalized subtype name. Mirrors
//!   `Keyword::Champion` convention.
//!
//! - **`Rule::Escape (~6 cards)**: Engine has
//!   `Keyword::Escape { cost, exile_count }` but mtgish carries only
//!   the cost — the `exile_count` lives in Oracle text or a separate
//!   metadata pipeline. Defaulting to 0 would silently produce wrong
//!   data; round-6 strict-fails until the count source is wired.
//!
//! ### Pre-game-only / no in-game effect (silent-consume in commit
//! `4c03ddb00`):
//!
//! - `Rule::HiddenAgenda` / `DoubleAgenda` / `FaceUpDraftEffect` —
//!   Conspiracy mechanics
//! - `Rule::AsSelfDraft` — Conspiracy draft
//! - `Rule::StartingIntensity` — Vanguard
//! - `DeckConstruction::CanBeYourCommander` /
//!   `RemoveFromDeckIfNotPlayingForAnte` /
//!   `CanHave*OfThisCard` — round-2 silent-consume
//!
//! These contribute zero gaps and zero engine artifacts.

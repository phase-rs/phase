//! Issue #8586: Runo Stromkirk // Krothuss, Lord of the Deep — verbatim Oracle
//! front face:
//!   "Flying
//!    When Runo enters, put up to one target creature card from your graveyard
//!    on top of your library.
//!    At the beginning of your upkeep, look at the top card of your library.
//!    You may reveal that card. If a creature card with mana value 6 or greater
//!    is revealed this way, transform Runo."
//!
//! THE DEFECT this file guards. `parse_if_revealed_card_type_conditional`
//! consumed `" card is revealed this way"` as a single `tag()`, leaving no slot
//! for the postnominal property. Runo's `"with mana value 6 or greater"` sits
//! exactly in that missing slot, so the head declined and the ENTIRE gate was
//! dropped (`condition: null` on the `Transform` node) — the upkeep trigger
//! turned Runo over regardless of what was on top of the library. CR 701.20a
//! (revealing) is the authorizing rule for the `"revealed this way"` anaphor;
//! CR 202.3 defines the mana value the gate bounds.
//!
//! MEASURED ROWS, at BASE (`3bc9531e7`) and after the parser fix:
//!
//! | row | shape | BASE | after the fix |
//! |---|---|---|---|
//! | `runo_declining_a_reveal_below_the_mana_value_gate_does_not_transform` | MV2 creature on top, DECLINE | `true` (wrong) | **`false`** |
//! | `runo_declining_a_reveal_of_a_noncreature_does_not_transform` | MV6 instant on top, DECLINE | `true` (wrong) | **`false`** |
//! | `runo_declining_a_qualifying_reveal_still_transforms_residual_defect` | MV6 creature on top, DECLINE | `true` | `true` (unchanged) |
//! | `runo_fixture_parses_to_a_transform_gated_on_the_mana_value_floor` | parse-only | condition `None` | `Some(Cmc { GE, 6 })` |
//!
//! REVERTING the parser change flips rows 1, 2 and 4. Row 3 is a labelled
//! characterization row and a live positive control — see its own doc comment.
//!
//! WHEN DW#5 LANDS — DW#5 being the deferred fix for the residual defect row 3
//! records: a DECLINED optional reveal still writes the reveal ledger, so the
//! `"…is revealed this way"` gate is satisfied by a card no player was ever
//! shown (CR 701.20a + CR 608.2d) — flipping row 3's `true` to `false` is NOT
//! the only edit the file needs. Once a DECLINED reveal stops writing the
//! reveal ledger, reach guard 2 (`last_revealed_ids.len() == 1`) fails on ALL
//! THREE runtime rows and rows 1 and 2 stop discriminating. That is deliberate
//! — the file goes loudly red rather than silently vacuous — but the DW#5 fixer
//! must re-anchor that guard on a signal the declined path still produces (or
//! move these rows to the ACCEPT side), not just edit row 3.
//!
//! `DEFERRED(DW#5, Appendix A + PR body)` is scoped to the DECLINE rows only.
//! The Phase-2 ACCEPT rows below are unaffected by it: the reveal ledger is
//! written on the accept path either way, so their reach guard 2 keeps holding.
//! After Phase 2 this file's Phase-1 discrimination is therefore carried by the
//! two ACCEPT negatives (MV2 creature, MV6 instant) as well as by the decline
//! rows — measured `TRUE` (wrong) with Phase 2 alone, `false` in the chartered
//! order.
//!
//! TWO MEASURED FOOT-GUNS, both closed here on purpose:
//!   1. The fixture's card NAME must be exactly `"Runo Stromkirk"` — see the
//!      warning on `RUNO_ORACLE`. A misnamed fixture reports
//!      `transformed=false, answered=true, last_revealed=1` on BOTH sides: it
//!      looks like a healthy negative and measures nothing.
//!   2. The harness shape must start at `Phase::PreCombatMain` with Runo under
//!      **P1** and then `advance_to_phase(Phase::Upkeep)`. The phase TRANSITION
//!      is what fires the trigger, and the trigger carries `OnlyDuringYourTurn`,
//!      so a Runo under P0 in this shape is never offered the reveal at all.
//!   3. The drive must STOP when the upkeep trigger's resolution is complete.
//!      `drive_upkeep` runs only while the trigger is on the stack (or the
//!      machine is parked in `OptionalEffectChoice`). The next priority pass
//!      after that ends the upkeep step and runs the draw (CR 503.1 ->
//!      CR 504.1), and a fixture whose accept branch just moved its only library
//!      card to hand then draws from an empty library and **loses the game**
//!      (CR 704.5b). MEASURED: with an unbounded drive the Sidequest ACCEPT row
//!      reads `transformed=false, answered=true, last_revealed=1` with
//!      `waiting_for = GameOver` — and ALL FOUR of the original reach guards
//!      still pass, so it looks exactly like a healthy negative while measuring
//!      nothing. Reach guard 5 (`phase == Phase::Upkeep`, game not over) exists
//!      to make that visible. Do not "simplify" `drive_upkeep` back to a bare
//!      loop budget, and do not paper over it by padding the library: padding
//!      leaves the drive running past the measurement and was measured still
//!      ending the game.
//!
//! Every runtime row asserts four reach guards in the same test (the optional
//! was genuinely offered and answered, exactly one card was looked at, a
//! `back_face` is installed, and the object's name), so no negative can pass
//! vacuously.
//!
//! PHASE 2 HAS LANDED, and the ACCEPT side is covered here now. `Effect::Transform`
//! resolved its `Single`-scope subject POSITIONALLY out of `ability.targets`, so
//! the object an earlier chain instruction bound displaced the printed
//! self-reference and the engine made an OFF-BATTLEFIELD card the subject — the
//! looked-at library card for Runo and Delver, and for Sidequest the card its
//! own `ChangeZone` parent had already moved to hand. `transform_permanent`
//! rejects an object that is not on the battlefield, so the printed transform
//! never happened. `crates/engine/src/game/effects/transform_effect.rs` now
//! resolves the subject through `targeting::resolved_targets` (CR 201.5).
//!
//! MEASURED ACCEPT-SIDE ROWS, taken BEFORE this PR's engine commit and after
//! it. "Before" means the state this PR's Phase 1 leaves behind, and is how the
//! discrimination is reproduced: revert
//! `crates/engine/src/game/effects/transform_effect.rs` alone to its pre-PR
//! state, keeping Phase 1's parser fix in the tree.
//!
//! | row | shape | before Phase 2 | after Phase 2 |
//! |---|---|---|---|
//! | `runo_transforms_when_top_card_is_a_creature_with_mana_value_six` | MV6 creature, ACCEPT | `false` | **`true`** |
//! | `runo_accepting_a_reveal_below_the_mana_value_gate_does_not_transform` | MV2 creature, ACCEPT | `false` | `false` |
//! | `runo_accepting_a_reveal_of_a_noncreature_does_not_transform` | MV6 instant, ACCEPT | `false` | `false` |
//! | `delver_of_secrets_transforms_when_top_card_is_an_instant` | Delver, instant, ACCEPT | `false` | **`true`** |
//! | `delver_of_secrets_does_not_transform_when_top_card_is_a_creature` | Delver, creature, ACCEPT | `false` | `false` |
//! | `sidequest_catch_a_fish_transforms_when_the_reveal_is_accepted` | Sidequest, creature, ACCEPT | `false` | **`true`** |
//! | `sidequest_catch_a_fish_does_not_transform_when_the_reveal_is_declined` | Sidequest, DECLINE | `false` | `false` |
//! | `a_bare_look_then_transform_chain_transforms_the_source` | synthetic bare `Dig -> Transform` | `false` | **`true`** |
//! | `a_transform_after_a_non_revealing_instruction_is_unaffected` | synthetic `PutCounter -> Transform` | `true` | `true` |
//!
//! REVERTING `crates/engine/src/game/effects/transform_effect.rs` ALONE flips
//! the four bolded rows red. The five unbolded rows pass on both sides and are
//! labelled in their own doc comments as negatives or controls, not as
//! discriminators.
//!
//! `setup`, `seed_library_top`, `drive_upkeep` and `assert_reach_guards` are
//! parameterized — on the printed permanent kind (`FixturePermanent`), the drive
//! shape (`UpkeepDrive`), the top card's core type and mana value, the expected
//! reveal-ledger size and the back face — so all three cards and both synthetics
//! share one harness rather than a cluster of near-duplicate helpers.

use engine::game::game_object::BackFaceData;
use engine::game::scenario::{GameRunner, GameScenario, P1};
use engine::game::zones::create_object;
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityCondition, AbilityDefinition, Comparator, Effect, EffectScope, FilterProp, QuantityExpr,
    TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Runo Stromkirk — verbatim Scryfall Oracle text of the FRONT face, including
/// the `Flying` and enters-the-battlefield lines.
///
/// ⚠ The card name passed to `from_oracle_text` MUST be exactly
/// `"Runo Stromkirk"`. `~` normalization runs once at the single parser entry
/// point (`parse_oracle_ir` calls `normalize_card_name_refs` before it splits
/// the text into lines), so any other name leaves `"transform Runo"`
/// unnormalized, **no `Effect::Transform` node enters the AST at all**, and
/// every row in this file reports `transformed=false, answered=true,
/// last_revealed=1` — it looks like a healthy negative and measures nothing.
/// The parse-level control
/// `runo_fixture_parses_to_a_transform_gated_on_the_mana_value_floor` exists to
/// catch exactly this.
const RUNO_ORACLE: &str = "Flying\nWhen Runo enters, put up to one target creature card from your graveyard on top of your library.\nAt the beginning of your upkeep, look at the top card of your library. You may reveal that card. If a creature card with mana value 6 or greater is revealed this way, transform Runo.";

/// Krothuss, Lord of the Deep — the back face, taken from the card corpus
/// (`card-data.json`: Legendary Creature — Kraken Horror, 3/5, Flying), built
/// on the `azors_gateway_transform_condition.rs::sanctum_of_the_sun_back_face`
/// template.
///
/// `layout_kind: Some(LayoutKind::Transform)` is set because that is what a
/// real double-faced card carries, NOT because any assertion depends on it:
/// the `TransformScope::Single` path does not consult it —
/// `is_double_faced_permanent` (`game/transform.rs`) gates only the `resolve_all`
/// scope. No test logic here may come to depend on that field.
fn krothuss_back_face() -> BackFaceData {
    BackFaceData {
        power: Some(3),
        toughness: Some(5),
        card_types: CardType {
            supertypes: vec![Supertype::Legendary],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Kraken".to_string(), "Horror".to_string()],
        },
        ..back_face_template("Krothuss, Lord of the Deep")
    }
}

/// The shared back-face template every fixture in this file builds on: an empty
/// transform-layout back face carrying only the name. Each builder overrides the
/// handful of fields its real card actually prints, with struct-update syntax, so
/// a new `BackFaceData` field does not have to be added in four places.
///
/// Extracted when Phase 2 added three more back faces; `krothuss_back_face`'s
/// values are unchanged, and the `layout_kind` note on it applies to all of them.
fn back_face_template(name: &str) -> BackFaceData {
    BackFaceData {
        is_swap_snapshot: false,
        name: name.to_string(),
        power: None,
        toughness: None,
        loyalty: None,
        printed_loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: Some(engine::types::card::LayoutKind::Transform),
        parse_warnings: vec![],
    }
}

/// Delver of Secrets — verbatim Scryfall / corpus Oracle text of the FRONT face.
///
/// Unlike Runo, the self-reference is `"transform this creature"`, which is
/// NAME-INDEPENDENT: no `~` normalization is involved, so this fixture does not
/// carry Runo's misnaming foot-gun. That independence is exactly why Delver is
/// the row that isolates Phase 2's subject-resolution fix from Phase 1's parser
/// fix — Delver's gate carries NO `additional_filter`, so it parses identically
/// before and after Phase 1.
const DELVER_ORACLE: &str = "At the beginning of your upkeep, look at the top card of your library. You may reveal that card. If an instant or sorcery card is revealed this way, transform this creature.";

/// Insectile Aberration — Delver's back face, taken from the card corpus
/// (`card-data.json`: Creature — Human Insect, 3/2, Flying).
fn insectile_aberration_back_face() -> BackFaceData {
    BackFaceData {
        power: Some(3),
        toughness: Some(2),
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Human".to_string(), "Insect".to_string()],
        },
        keywords: vec![Keyword::Flying],
        ..back_face_template("Insectile Aberration")
    }
}

/// Sidequest: Catch a Fish — verbatim Scryfall / corpus Oracle text of the FRONT
/// face. A THIRD card and a DIFFERENT chain shape from Runo and Delver:
/// `Dig -> RevealTop -> ChangeZone{ParentTarget} -> Token -> Transform{SelfRef}`,
/// with the transform gated on `ZoneChangedThisWay` rather than on
/// `RevealedHasCardType`. It is also the only fixture here whose permanent is an
/// ENCHANTMENT, which is why `setup` dispatches on `FixturePermanent`.
const SIDEQUEST_ORACLE: &str = "At the beginning of your upkeep, look at the top card of your library. If it's an artifact or creature card, you may reveal it and put it into your hand. If you put a card into your hand this way, create a Food token and transform this enchantment.";

/// Cooking Campsite — Sidequest's back face, taken from the card corpus
/// (`card-data.json`: Land, no P/T). A LAND back face is correct and deliberate:
/// `transform_permanent` does not require the back face to be a creature.
fn cooking_campsite_back_face() -> BackFaceData {
    BackFaceData {
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Land],
            subtypes: vec![],
        },
        ..back_face_template("Cooking Campsite")
    }
}

/// The back face for the two SYNTHETIC fixtures (the bare `Dig -> Transform`
/// bisect row and its `PutCounter -> Transform` control). No real card is
/// involved — the name exists only so the reach guard on the object's name can
/// discriminate a transform from a non-transform.
fn probe_back_face() -> BackFaceData {
    BackFaceData {
        power: Some(2),
        toughness: Some(2),
        ..back_face_template("Probe Back Face")
    }
}

/// The issue's own bisect row (row 2 of #8586): a chain whose FIRST instruction
/// writes the last-revealed ledger and whose second is a printed self-transform,
/// with NO optional in between. No card name appears in the text, so the `~`
/// foot-gun does not apply — but the PARSE shape does, which is why
/// `a_bare_look_then_transform_chain_transforms_the_source` asserts it before it
/// asserts any behaviour.
const BARE_DIG_ORACLE: &str = "At the beginning of your upkeep, look at the top card of your library. Transform this creature.";

/// The NEGATIVE CONTROL for the bisect row: the same two-instruction chain shape
/// with a first instruction that writes NO last-revealed ledger, so the chain
/// layer injects nothing and the transform was never displaced. Measured `true`
/// on BOTH sides of this change — the non-injecting path is untouched.
const PUT_COUNTER_ORACLE: &str = "At the beginning of your upkeep, put a +1/+1 counter on this creature. Transform this creature.";

/// Put a card of a known core type and mana value on top of `player`'s library,
/// so the upkeep "look at the top card" step has a real input for BOTH legs of
/// `AbilityCondition::RevealedHasCardType`: the type leg (`card_types` via
/// `object_has_core_type`) and the property leg (`additional_filter` via
/// `matches_target_filter`). Both `card_types` and `base_card_types` are set —
/// the two legs read different fields.
fn seed_library_top(
    state: &mut GameState,
    player: PlayerId,
    name: &str,
    core_type: CoreType,
    mana_value: u32,
) -> ObjectId {
    let id = create_object(
        state,
        CardId(state.next_object_id),
        player,
        name.to_string(),
        engine::types::zones::Zone::Library,
    );
    let obj = state.objects.get_mut(&id).unwrap();
    obj.card_types.core_types = vec![core_type];
    obj.base_card_types.core_types = vec![core_type];
    obj.mana_cost = ManaCost::generic(mana_value);
    let player_state = state.players.iter_mut().find(|p| p.id == player).unwrap();
    player_state.library.retain(|&oid| oid != id);
    player_state.library.insert(0, id);
    id
}

/// Which printed permanent the fixture card is, so `setup` can place it through
/// the right `GameScenario` builder. CR 205.2a: card type is a printed
/// characteristic — a `bool is_enchantment` would not survive the next fixture,
/// and would not carry the P/T a creature needs.
enum FixturePermanent {
    /// A legendary creature with printed P/T — Runo Stromkirk (1/4).
    LegendaryCreature { power: i32, toughness: i32 },
    /// A nonlegendary creature — Delver of Secrets (1/1).
    Creature { power: i32, toughness: i32 },
    /// An enchantment — Sidequest: Catch a Fish.
    Enchantment,
}

/// Build the board and advance into the permanent's controller's own upkeep.
///
/// The permanent goes under **P1** and the scenario starts at
/// `Phase::PreCombatMain` so that `advance_to_phase(Phase::Upkeep)` crosses a
/// turn boundary into P1's turn: the phase TRANSITION is what fires an
/// "at the beginning of your upkeep" trigger, and this one carries
/// `TriggerConstraint::OnlyDuringYourTurn`.
///
/// Parameterized on card name, oracle text, PRINTED PERMANENT KIND, top-card
/// core type and mana value (plus the back face) so a second and third fixture
/// card can reuse it without touching any row above.
fn setup(
    card_name: &str,
    oracle: &str,
    kind: FixturePermanent,
    top_core: CoreType,
    top_mana_value: u32,
    back_face: BackFaceData,
) -> (GameRunner, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // WILDCARD-FREE: a new `FixturePermanent` variant must be a compile error
    // here, not a silently mis-placed permanent.
    let subject = match kind {
        FixturePermanent::LegendaryCreature { power, toughness } => scenario
            .add_creature(P1, card_name, power, toughness)
            .from_oracle_text(oracle)
            .as_legendary()
            .id(),
        FixturePermanent::Creature { power, toughness } => scenario
            .add_creature(P1, card_name, power, toughness)
            .from_oracle_text(oracle)
            .id(),
        // `add_enchantment_from_oracle` parses the oracle text itself, places the
        // permanent on the battlefield with `CoreType::Enchantment`, syncs
        // `base_card_types` and clears summoning sickness — `.id()` is all this
        // caller needs.
        FixturePermanent::Enchantment => scenario
            .add_enchantment_from_oracle(P1, card_name, oracle)
            .id(),
    };
    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&subject)
        .unwrap()
        .back_face = Some(back_face);
    seed_library_top(runner.state_mut(), P1, "Top Card", top_core, top_mana_value);
    runner.advance_to_phase(Phase::Upkeep);
    assert_eq!(
        runner.state().active_player,
        P1,
        "reach guard: the trigger is OnlyDuringYourTurn, so this must be P1's own upkeep"
    );
    (runner, subject)
}

/// How a fixture's upkeep trigger is driven, and WHICH reach guard the row
/// then owes. CR 608.2d: a "you may" is a resolution-time choice, so a fixture
/// that PRINTS one and a fixture that prints none are different instruments and
/// must be reach-guarded differently. Typed rather than `Option<bool>` so a row
/// cannot silently assert the wrong guard.
enum UpkeepDrive {
    /// The card prints "You may reveal that card." — answer the offer.
    /// The returned bool is Phase 1's three-conjunct guard, unchanged.
    AnswerOptional { accept: bool },
    /// The card prints no optional (the bare `Dig -> Transform` bisect row and
    /// its `PutCounter -> Transform` control). The guard inverts: the returned
    /// bool is TRUE only if NO `OptionalEffectChoice` was ever presented.
    NoOptional,
}

/// Drive the upkeep trigger to the end of ITS OWN RESOLUTION, and stop there.
///
/// For an `AnswerOptional` drive this drains priority until the reveal offer
/// appears and answers it with `accept`, then returns whether the optional was
/// offered AND successfully answered AND the machine then left
/// `OptionalEffectChoice` — the reach guard every such row asserts, negatives
/// included (CR 608.2d: an effect's "you may" is a resolution-time choice, so
/// "did not transform" is only meaningful if the choice was genuinely presented
/// and answered).
///
/// ⚠ The three conjuncts are load-bearing; do NOT weaken this back to "was an
/// `OptionalEffectChoice` ever seen". If a later change makes
/// `DecideOptionalEffect` error at this seam, the machine stalls parked in
/// `OptionalEffectChoice` — and every OTHER reach guard still passes, because
/// `last_revealed_ids` was already written by the enclosing look step, the back
/// face is installed, the name is untouched and `transformed` is `false`. An
/// offered-only flag would let both negative rows go green while measuring
/// nothing at all. For a `NoOptional` drive the guard INVERTS: an offer that
/// appears at all means the fixture is not the shape the row claims.
///
/// CR 503.1: the upkeep step's whole content, for these fixtures, is the
/// triggered ability that was put on the stack by the phase transition. This
/// loop therefore runs while that ability is still on the stack (or is holding
/// the machine in `OptionalEffectChoice`) and stops the instant it is not.
///
/// ⚠ THE STOPPING CONDITION IS LOAD-BEARING — MEASURED FOOT-GUN #3. Do NOT
/// weaken this back to "pass priority until the loop budget runs out". The very
/// next priority pass after the trigger resolves ends the upkeep step and runs
/// CR 504.1's draw ("First, the active player draws a card"), which for a
/// fixture whose accept branch just moved its only library card to hand means
/// drawing from an empty library and LOSING THE GAME (CR 704.5b). Measured: the
/// Sidequest ACCEPT row then reads `transformed=false` with
/// `waiting_for = GameOver` — and every other reach guard in this file still
/// PASSES, because `last_revealed_ids` was already written, the back face is
/// installed and the front-face name is untouched. It is the same class as this
/// file's other two foot-guns: a healthy-looking negative that measures nothing.
/// `assert_reach_guards`' fifth guard (`phase == Phase::Upkeep`) is what makes
/// that class visible if anyone reintroduces it.
fn drive_upkeep(runner: &mut GameRunner, drive: UpkeepDrive) -> bool {
    assert!(
        !runner.state().stack.is_empty(),
        "reach guard: the upkeep trigger must already be on the stack when the drive \
         starts, or this loop returns immediately and the row measures nothing"
    );
    let mut answered = false;
    let mut offered = false;
    for _ in 0..20 {
        // Stop as soon as the trigger has left the stack and nothing is parked
        // in a mid-resolution choice: its resolution is complete, and the next
        // `PassPriority` would leave the upkeep step (CR 503.1 -> CR 504.1).
        if runner.state().stack.is_empty()
            && !matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            )
        {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::OptionalEffectChoice { .. } => {
                offered = true;
                match drive {
                    UpkeepDrive::AnswerOptional { accept } => {
                        answered = runner
                            .act(GameAction::DecideOptionalEffect { accept })
                            .is_ok();
                        if !answered {
                            break;
                        }
                    }
                    // A `NoOptional` row that IS offered a choice has already
                    // failed; stop and let the row's `assert!(drove)` say so.
                    UpkeepDrive::NoOptional => break,
                }
            }
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    break;
                }
            }
            _ => break,
        }
    }
    let complete = runner.state().stack.is_empty()
        && !matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        );
    match drive {
        UpkeepDrive::AnswerOptional { .. } => answered && complete,
        UpkeepDrive::NoOptional => !offered && complete,
    }
}

/// Walk an ability chain (effect + sub_ability + else_ability) for the first
/// `Effect::Transform` node.
fn find_transform_node(def: &AbilityDefinition) -> Option<&AbilityDefinition> {
    if matches!(def.effect.as_ref(), Effect::Transform { .. }) {
        return Some(def);
    }
    def.sub_ability
        .as_deref()
        .and_then(find_transform_node)
        .or_else(|| def.else_ability.as_deref().and_then(find_transform_node))
}

/// Assert the FIVE reach guards every runtime row shares. `expected_name` is
/// the name the object must carry AFTER the upkeep resolved — the front face
/// for a row that must not transform, the back face for one that must.
///
/// `expected_last_revealed` is the size of the reveal ledger the row's own chain
/// must have written. It is `1` for every fixture with a "look at the top card"
/// instruction (every Phase-1 row, and every Phase-2 row except one). `0` is
/// ONLY ever correct for a fixture that carries NO last-revealed writer at all —
/// the `PutCounter -> Transform` control, whose whole content is that absence. A
/// copy-pasted `0` on a reveal row would silently disarm this guard, so state the
/// row's reason when passing it.
fn assert_reach_guards(
    runner: &GameRunner,
    subject: ObjectId,
    answered: bool,
    expected_last_revealed: usize,
    expected_name: &str,
) {
    assert!(
        answered,
        "reach guard (CR 608.2d): the reveal's OptionalEffectChoice must have been \
         offered and answered (and left behind) — or, for a `NoOptional` row, never \
         offered at all — or this row measures nothing"
    );
    assert_eq!(
        runner.state().last_revealed_ids.len(),
        expected_last_revealed,
        "reach guard: the look step must have produced exactly {expected_last_revealed} \
         card(s) for the gate to read; got {:?}",
        runner.state().last_revealed_ids
    );
    assert!(
        runner.state().objects[&subject].back_face.is_some(),
        "reach guard (CR 701.27a): a permanent with no back face cannot transform at \
         all, so a 'did not transform' assertion would be vacuous without this"
    );
    assert_eq!(
        runner.state().objects[&subject].name,
        expected_name,
        "reach guard: the object's name must agree with the transform assertion"
    );
    // REACH GUARD 5 (B-1, MEASURED). The drive must have stopped inside the
    // upkeep step, with the game still live. Without this, a drive that runs on
    // past the trigger's resolution can deck the fixture's controller
    // (CR 504.1 draw + CR 704.5b) and every OTHER guard here still passes —
    // that is exactly how the Sidequest ACCEPT row was measured reading
    // `transformed=false` while looking perfectly healthy.
    assert_eq!(
        runner.state().phase,
        Phase::Upkeep,
        "reach guard: the drive must stop inside the upkeep step it is measuring"
    );
    assert!(
        !matches!(runner.state().waiting_for, WaitingFor::GameOver { .. }),
        "reach guard: the game must still be live; a fixture that lost the game measures nothing"
    );
}

/// DISCRIMINATOR #1. Top card is a **mana value 2 creature** — below Runo's
/// `mana value 6 or greater` floor — and the reveal is DECLINED.
///
/// Runo must NOT transform, and the assertion is permanently CR-correct: it is
/// over-determined by two independent reasons. Nothing was revealed at all
/// (CR 701.20a: to reveal a card is to show it to all players; a declined "you
/// may reveal" shows nothing), and independently the top card fails the gate on
/// its own terms (CR 202.3: mana value 2 is not 6 or greater).
///
/// Measured `true` (wrong) at BASE and `false` after the parser fix: at BASE
/// the postnominal property slot did not exist, the whole condition was dropped
/// to `null`, and the ungated `Transform` sub turned Runo over. This assertion
/// fails on revert.
#[test]
fn runo_declining_a_reveal_below_the_mana_value_gate_does_not_transform() {
    let (mut runner, runo) = setup(
        "Runo Stromkirk",
        RUNO_ORACLE,
        FixturePermanent::LegendaryCreature {
            power: 1,
            toughness: 4,
        },
        CoreType::Creature,
        2,
        krothuss_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: false });

    assert_reach_guards(&runner, runo, answered, 1, "Runo Stromkirk");
    assert!(
        !runner.state().objects[&runo].transformed,
        "a mana-value-2 creature does not satisfy 'mana value 6 or greater' (CR 202.3), \
         and a declined reveal shows no card at all (CR 701.20a) — Runo must stay front-face up"
    );
}

/// DISCRIMINATOR #2. Top card is a **mana value 6 instant** — it clears the
/// mana-value floor but is not a creature card — and the reveal is DECLINED.
///
/// This row exercises the OTHER leg of the evaluator: `RevealedHasCardType`
/// checks `card_types` and `additional_filter` independently, so row 1 fails
/// the property leg while this one fails the type leg. Permanently CR-correct
/// for the same two independent reasons: nothing was revealed (CR 701.20a), and
/// an instant is not a creature card.
///
/// Measured `true` (wrong) at BASE and `false` after the parser fix. Fails on
/// revert.
#[test]
fn runo_declining_a_reveal_of_a_noncreature_does_not_transform() {
    let (mut runner, runo) = setup(
        "Runo Stromkirk",
        RUNO_ORACLE,
        FixturePermanent::LegendaryCreature {
            power: 1,
            toughness: 4,
        },
        CoreType::Instant,
        6,
        krothuss_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: false });

    assert_reach_guards(&runner, runo, answered, 1, "Runo Stromkirk");
    assert!(
        !runner.state().objects[&runo].transformed,
        "an instant card is not a 'creature card' however large its mana value, and a \
         declined reveal shows no card at all (CR 701.20a) — Runo must stay front-face up"
    );
}

/// POSITIVE CONTROL (α) — **CHARACTERIZATION test. The outcome asserted below
/// is rules-INCORRECT and is asserted deliberately, as a record of current
/// behavior.**
///
/// Top card is a mana value 6 creature (it clears the gate on its own terms)
/// and the reveal is DECLINED. Current behavior transforms Runo anyway. Under
/// the rules it must not: CR 701.20a defines revealing as showing the card to
/// all players, and CR 608.2d makes the "you may reveal that card" a
/// resolution-time choice — a declined reveal shows no card, so the
/// `"…is revealed this way"` gate is unsatisfied and the transform should not
/// happen.
///
/// This assertion is EXPECTED TO BE FLIPPED by deferred work:
/// `DEFERRED(DW#5, Appendix A + PR body)` — DW#5 is the deferred fix for exactly
/// the residual this row records: a DECLINED optional reveal still writes the
/// reveal ledger, so the `"…is revealed this way"` gate is satisfied by a card
/// no player was ever shown. Chartered in the run charter, Appendix A; disclosed
/// in the PR body under `Deferred / known-remaining`. Whoever closes DW#5 should
/// change `true` to `false` here and delete this paragraph.
///
/// WHY IT IS HERE ANYWAY. It is the file's only positive control ON THE DECLINE
/// DRIVE PATH. Phase 2 landed in this same PR, so
/// `runo_transforms_when_top_card_is_a_creature_with_mana_value_six` is a live
/// accept-side positive control now — but that row drives
/// `UpkeepDrive::AnswerOptional { accept: true }`, and nothing else in this file
/// shows that an `accept: false` drive reaches the seam at all. Without this row
/// the two DECLINE discriminators above would be indistinguishable from a
/// fixture whose declined path never gets there. It runs through the same
/// `setup` and the same `drive_upkeep` as rows 1 and 2, differing only in the
/// top card's mana value, and so proves — live, on the DECLINED drive — that the
/// trigger fires, the optional is offered and answered, `last_revealed_ids` is
/// written, the `Transform` node is reachable, and this object can in fact turn
/// over. Note the name reach guard is INVERTED here (back face, not front): a
/// same-direction copy-paste from rows 1 and 2 would make this row vacuous.
#[test]
fn runo_declining_a_qualifying_reveal_still_transforms_residual_defect() {
    let (mut runner, runo) = setup(
        "Runo Stromkirk",
        RUNO_ORACLE,
        FixturePermanent::LegendaryCreature {
            power: 1,
            toughness: 4,
        },
        CoreType::Creature,
        6,
        krothuss_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: false });

    assert_reach_guards(&runner, runo, answered, 1, "Krothuss, Lord of the Deep");
    assert!(
        runner.state().objects[&runo].transformed,
        "CHARACTERIZATION (rules-INCORRECT, DW#5): current behavior transforms Runo on a \
         DECLINED reveal whose top card would have satisfied the gate. Per CR 701.20a + \
         CR 608.2d nothing was revealed, so this should be false"
    );
}

/// POSITIVE CONTROL (β) — parse-level fixture integrity, and a second
/// independent Phase-1 discriminator. Asserts nothing rules-incorrect, so
/// unlike (α) it is stable through every piece of deferred work.
///
/// It closes the measured fixture foot-gun by NAME: a fixture built under any
/// name other than `"Runo Stromkirk"` leaves `"transform Runo"` unnormalized
/// and drops the `Effect::Transform` node from the AST entirely, which the
/// runtime rows above would report as a healthy-looking negative.
///
/// THE `condition` ASSERTION BELOW IS LOAD-BEARING — DO NOT "SIMPLIFY" THIS
/// BACK TO A BARE `matches!(…, Effect::Transform { .. })`. That bare form is
/// `true` at BASE **and** `true` after the parser fix: it is not a
/// discriminator. Only the `Transform` node's `condition` moves, from `None` to
/// `RevealedHasCardType { [Creature], Cmc { GE, 6 } }` (CR 701.20a for the
/// `"revealed this way"` anaphor, CR 202.3 for the mana value it bounds).
#[test]
fn runo_fixture_parses_to_a_transform_gated_on_the_mana_value_floor() {
    let parsed = parse_oracle_text(RUNO_ORACLE, "Runo Stromkirk", &[], &[], &[]);
    let transform = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(find_transform_node)
        .expect(
            "the fixture's parsed triggers must contain an Effect::Transform node — if this \
             fails, check that the card name is exactly \"Runo Stromkirk\" so `~` normalization \
             rewrites \"transform Runo\"",
        );

    let Some(AbilityCondition::RevealedHasCardType {
        card_types,
        additional_filter,
        ..
    }) = transform.condition.as_ref()
    else {
        panic!(
            "the Transform node must be gated by the revealed-card type condition; got {:?}",
            transform.condition
        );
    };
    assert_eq!(card_types, &vec![CoreType::Creature]);
    assert_eq!(
        additional_filter,
        &Some(FilterProp::Cmc {
            comparator: Comparator::GE,
            value: QuantityExpr::Fixed { value: 6 },
        }),
        "the 'with mana value 6 or greater' postnominal property must reach the gate"
    );
}

/// Hand size for `player`, for the Sidequest rows' hand-delta reach guard.
fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

/// Assert a synthetic fixture parses to exactly the chain shape its row claims:
/// a trigger carrying an `Effect::Transform { target: SelfRef, scope: Single }`.
///
/// MANDATORY for the two synthetic rows. A synthetic that does not parse to the
/// intended shape is the same failure mode as this file's misnamed-Runo foot-gun:
/// it reports a healthy-looking result and measures nothing. If a synthetic ever
/// stops parsing to this shape, change the FIXTURE TEXT until it does — never
/// relax this assertion to match the parse.
fn assert_synthetic_parses_to_a_printed_self_transform(oracle: &str) {
    let parsed = parse_oracle_text(oracle, "Probe Permanent", &[], &[], &[]);
    let node = parsed
        .triggers
        .iter()
        .filter_map(|t| t.execute.as_deref())
        .find_map(find_transform_node)
        .expect("the synthetic fixture must parse to a trigger chain containing Effect::Transform");
    assert!(
        matches!(
            node.effect.as_ref(),
            Effect::Transform {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single
            }
        ),
        "the synthetic must carry a printed SELF-reference (CR 201.5); got {:?}",
        node.effect
    );
}

/// PHASE-2 DISCRIMINATOR #1 — **the issue's headline defect**. Top card is a
/// mana value 6 creature and the reveal is ACCEPTED: Runo must transform.
///
/// Measured `false` before this PR's engine commit — that is, with
/// `transform_effect.rs` reverted to its pre-PR state and Phase 1's parser fix
/// still in the tree — and `true` after the subject-resolution fix. REVERTING
/// that file alone flips this row red.
///
/// WHY IT WAS FALSE. `Dig{count:1,keep_count:0}` -> `Reveal{ParentTarget}`
/// propagates the looked-at LIBRARY card down the chain, the old positional
/// `ability.targets.as_slice()` read bound that card as the transform subject
/// instead of Runo, and `transform_permanent` rejects an object that is not on
/// the battlefield — so the printed `"transform Runo"` (CR 201.5) never
/// happened. `targeting::resolved_targets` resolves the printed self-reference
/// to the ability's own source, which is the permanent.
#[test]
fn runo_transforms_when_top_card_is_a_creature_with_mana_value_six() {
    let (mut runner, runo) = setup(
        "Runo Stromkirk",
        RUNO_ORACLE,
        FixturePermanent::LegendaryCreature {
            power: 1,
            toughness: 4,
        },
        CoreType::Creature,
        6,
        krothuss_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: true });

    assert_reach_guards(&runner, runo, answered, 1, "Krothuss, Lord of the Deep");
    assert!(
        runner.state().objects[&runo].transformed,
        "CR 201.5 + CR 701.27a: an accepted reveal of a mana-value-6 creature satisfies the \
         gate, and the printed self-reference binds Runo himself — he must turn over"
    );
}

/// NEGATIVE SIBLING of the row above, on the PROPERTY leg. Top card is a mana
/// value 2 creature and the reveal is ACCEPTED: the gate Phase 1 restored is
/// unsatisfied (CR 202.3), so nothing transforms.
///
/// Measured `false` with `transform_effect.rs` reverted to its pre-PR state and
/// `false` after this change — it does NOT flip, which is the point. It is also
/// the DURABLE PHASE-1 DISCRIMINATOR now that Phase 2 is in the tree: with the
/// subject fix but WITHOUT the parser gate, this row was measured `true`
/// (wrong).
#[test]
fn runo_accepting_a_reveal_below_the_mana_value_gate_does_not_transform() {
    let (mut runner, runo) = setup(
        "Runo Stromkirk",
        RUNO_ORACLE,
        FixturePermanent::LegendaryCreature {
            power: 1,
            toughness: 4,
        },
        CoreType::Creature,
        2,
        krothuss_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: true });

    assert_reach_guards(&runner, runo, answered, 1, "Runo Stromkirk");
    assert!(
        !runner.state().objects[&runo].transformed,
        "CR 202.3: mana value 2 is not 'mana value 6 or greater', so an ACCEPTED reveal of \
         this card leaves the gate unsatisfied"
    );
}

/// NEGATIVE SIBLING of the headline row, on the TYPE leg. Top card is a mana
/// value 6 instant and the reveal is ACCEPTED: it clears the mana-value floor
/// but is not a creature card.
///
/// Measured `false` with `transform_effect.rs` reverted to its pre-PR state and
/// `false` after this change. Same durable Phase-1 discriminator note as the
/// row above: `true` (wrong) with Phase 2 alone.
#[test]
fn runo_accepting_a_reveal_of_a_noncreature_does_not_transform() {
    let (mut runner, runo) = setup(
        "Runo Stromkirk",
        RUNO_ORACLE,
        FixturePermanent::LegendaryCreature {
            power: 1,
            toughness: 4,
        },
        CoreType::Instant,
        6,
        krothuss_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: true });

    assert_reach_guards(&runner, runo, answered, 1, "Runo Stromkirk");
    assert!(
        !runner.state().objects[&runo].transformed,
        "an instant card is not a 'creature card' however large its mana value — the gate \
         is unsatisfied even on an ACCEPTED reveal"
    );
}

/// PHASE-2 DISCRIMINATOR #2 — **a SECOND card, and the row that isolates Phase 2
/// from Phase 1.** Delver of Secrets with an instant on top, reveal ACCEPTED.
///
/// Delver's gate carries NO `additional_filter` ("If an instant or sorcery card
/// is revealed this way"), so Phase 1's postnominal-property slot is not
/// involved at all and this row can only move on the subject-resolution fix.
/// Measured `false` with `transform_effect.rs` reverted to its pre-PR state,
/// `true` after it. Delver's self-reference is `"transform this creature"`,
/// which is NAME-INDEPENDENT, so this row is also free of Runo's misnaming
/// foot-gun.
#[test]
fn delver_of_secrets_transforms_when_top_card_is_an_instant() {
    let (mut runner, delver) = setup(
        "Delver of Secrets",
        DELVER_ORACLE,
        FixturePermanent::Creature {
            power: 1,
            toughness: 1,
        },
        CoreType::Instant,
        1,
        insectile_aberration_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: true });

    assert_reach_guards(&runner, delver, answered, 1, "Insectile Aberration");
    assert!(
        runner.state().objects[&delver].transformed,
        "CR 201.5: 'transform this creature' names Delver himself, not the library card the \
         chain propagated"
    );
}

/// NEGATIVE SIBLING of the Delver row: a creature on top is neither an instant
/// nor a sorcery, so the gate is unsatisfied even on an accepted reveal.
/// Measured `false` on both sides.
#[test]
fn delver_of_secrets_does_not_transform_when_top_card_is_a_creature() {
    let (mut runner, delver) = setup(
        "Delver of Secrets",
        DELVER_ORACLE,
        FixturePermanent::Creature {
            power: 1,
            toughness: 1,
        },
        CoreType::Creature,
        3,
        insectile_aberration_back_face(),
    );
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: true });

    assert_reach_guards(&runner, delver, answered, 1, "Delver of Secrets");
    assert!(
        !runner.state().objects[&delver].transformed,
        "a creature card is not 'an instant or sorcery card' — Delver must stay front-face up"
    );
}

/// PHASE-2 DISCRIMINATOR #3 — **a THIRD card, and a DIFFERENT chain shape.**
/// Sidequest: Catch a Fish is an ENCHANTMENT whose chain is
/// `Dig -> RevealTop -> ChangeZone{ParentTarget} -> Token -> Transform{SelfRef}`,
/// gated on `ZoneChangedThisWay` rather than on `RevealedHasCardType`. Its
/// displaced subject is the card its own `ChangeZone` parent already moved to
/// HAND, not a library card — the same defect reached through a different
/// parent. Measured `false` with `transform_effect.rs` reverted to its pre-PR
/// state, `true` after this change.
///
/// THE HAND-DELTA GUARD IS A REACH GUARD, NOT A DISCRIMINATOR.
/// CR 400.7 / CR 701.20a: the accept branch actually MOVED the looked-at card,
/// which is what satisfies `ZoneChangedThisWay` and gates the transform.
/// Measured `0 -> 1` on BOTH sides (the `ChangeZone` fires either way; only the
/// `Transform` does not). Without it, this row could go green on a Sidequest
/// that transformed for the wrong reason. The ABSOLUTE values are safe to assert
/// ONLY because `drive_upkeep` stops at the end of the trigger's resolution.
/// Under an UNBOUNDED drive they depend on the library depth, and NEITHER
/// reading is 1 and 0:
///   * library padded below the seeded top card: 2 and 1 (CR 504.1's draw adds
///     one card to each row).
///   * library UNPADDED, which is this file's actual harness: the accept row
///     reads `0 -> 0` and `waiting_for = GameOver`, because P1 draws from the
///     library its own accept branch just emptied and LOSES (CR 704.5b) before
///     the post-count is taken.
#[test]
fn sidequest_catch_a_fish_transforms_when_the_reveal_is_accepted() {
    let (mut runner, sidequest) = setup(
        "Sidequest: Catch a Fish",
        SIDEQUEST_ORACLE,
        FixturePermanent::Enchantment,
        CoreType::Creature,
        3,
        cooking_campsite_back_face(),
    );
    let hand_before = hand_size(&runner, P1);
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: true });
    let hand_after = hand_size(&runner, P1);

    assert_reach_guards(&runner, sidequest, answered, 1, "Cooking Campsite");
    assert_eq!(
        hand_after - hand_before,
        1,
        "reach guard: the accepted branch must actually put the looked-at card into hand, \
         which is what satisfies `ZoneChangedThisWay`"
    );
    assert!(
        runner.state().objects[&sidequest].transformed,
        "CR 201.5: 'transform this enchantment' names Sidequest itself, not the card its own \
         ChangeZone parent moved to hand"
    );
}

/// NEGATIVE SIBLING of the Sidequest row. The reveal is DECLINED, so no card
/// moves, `ZoneChangedThisWay` never fires and nothing transforms. Measured
/// `false` on both sides. The hand delta `0 -> 0` is this row's extra reach
/// guard: it proves the negative comes from the card NOT moving, rather than
/// from a fixture that never ran.
#[test]
fn sidequest_catch_a_fish_does_not_transform_when_the_reveal_is_declined() {
    let (mut runner, sidequest) = setup(
        "Sidequest: Catch a Fish",
        SIDEQUEST_ORACLE,
        FixturePermanent::Enchantment,
        CoreType::Creature,
        3,
        cooking_campsite_back_face(),
    );
    let hand_before = hand_size(&runner, P1);
    let answered = drive_upkeep(&mut runner, UpkeepDrive::AnswerOptional { accept: false });
    let hand_after = hand_size(&runner, P1);

    assert_reach_guards(&runner, sidequest, answered, 1, "Sidequest: Catch a Fish");
    assert_eq!(
        hand_after, hand_before,
        "reach guard: a declined reveal moves no card, so `ZoneChangedThisWay` cannot fire"
    );
    assert!(
        !runner.state().objects[&sidequest].transformed,
        "no card was put into hand this way, so the transform is ungated and must not happen"
    );
}

/// PHASE-2 DISCRIMINATOR #4 — **the issue's own bisect row (row 2 of #8586).**
/// A bare `look at the top card` followed by a printed self-transform, with NO
/// optional and NO gate. Measured `false` with `transform_effect.rs` reverted to
/// its pre-PR state, `true` after this change.
///
/// Its job is to prove the fix belongs at the CONSUMER seam, not at the chain
/// site: the chain layer's propagation is legitimate (CR 608.2c) and this row
/// goes green without any chain-site patch. Paired with
/// `a_transform_after_a_non_revealing_instruction_is_unaffected`, which shows
/// the non-injecting path was already correct and stays correct.
#[test]
fn a_bare_look_then_transform_chain_transforms_the_source() {
    assert_synthetic_parses_to_a_printed_self_transform(BARE_DIG_ORACLE);

    let (mut runner, probe) = setup(
        "Probe Permanent",
        BARE_DIG_ORACLE,
        FixturePermanent::Creature {
            power: 2,
            toughness: 2,
        },
        CoreType::Creature,
        3,
        probe_back_face(),
    );
    let drove = drive_upkeep(&mut runner, UpkeepDrive::NoOptional);

    assert_reach_guards(&runner, probe, drove, 1, "Probe Back Face");
    assert!(
        runner.state().objects[&probe].transformed,
        "CR 201.5: the printed self-transform binds the source, not the looked-at library card"
    );
}

/// CONTROL for the bisect row — **not a discriminator, `true` on BOTH sides.**
/// The same two-instruction chain with a first instruction that writes NO
/// last-revealed ledger, so the chain layer injects nothing and the transform's
/// subject was never displaced.
///
/// TWO POSITIVE REACH GUARDS, both required. The `+1/+1` counter proves the
/// chain demonstrably RAN (otherwise "it transformed both ways" is
/// indistinguishable from a fixture whose trigger never fired), and
/// `expected_last_revealed == 0` proves the absence of a reveal writer that is
/// this row's whole content — the one place in this file where `0` is correct.
#[test]
fn a_transform_after_a_non_revealing_instruction_is_unaffected() {
    assert_synthetic_parses_to_a_printed_self_transform(PUT_COUNTER_ORACLE);

    let (mut runner, probe) = setup(
        "Probe Permanent",
        PUT_COUNTER_ORACLE,
        FixturePermanent::Creature {
            power: 2,
            toughness: 2,
        },
        CoreType::Creature,
        3,
        probe_back_face(),
    );
    let drove = drive_upkeep(&mut runner, UpkeepDrive::NoOptional);

    assert_reach_guards(&runner, probe, drove, 0, "Probe Back Face");
    assert_eq!(
        runner.state().objects[&probe]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "positive reach guard: the chain's first instruction must have run"
    );
    assert!(
        runner.state().objects[&probe].transformed,
        "no last-revealed writer means no chain injection — this path was correct before \
         this change and must stay correct"
    );
}

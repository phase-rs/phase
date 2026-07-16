//! Watched-red witness for phase.rs task #150 (DrawSequenceFrame origin/kind +
//! completion routing).
//!
//! CR 701.22a + CR 614.6 + CR 121.6b: a scry→draw substitution
//! (`ReplacementDefinition::new(ReplacementEvent::Scry).execute(Effect::Draw {
//! .. })`, mirroring `scry.rs`'s own `scry_replacement_to_draw_delivers_through_resolver`
//! unit test) currently delivers its substituted draw via
//! `apply_scry_after_replacement`'s `ProposedEvent::Draw` arm, which routes the
//! FULL substituted event (`count: 2`) into `replacement::replace_event` as one
//! atomic unit instead of decomposing it into individual `count: 1` draws
//! first (the `start_draw_sequence` decomposition every other draw producer
//! uses). Stinkweed Imp's Dredge is offered against the whole 2-card draw at
//! once: declining it resolves BOTH cards in a single choice, instead of CR
//! 121.6b's "if an effect replaces a draw within a sequence of card draws, the
//! replacement effect is completed before resuming the sequence" — Dredge must
//! be offered again, independently, for the second card. This test drives a
//! real scry→draw substitution over a real Dredge card (Stinkweed Imp, Dredge
//! 5), declines the first Dredge offer, and asserts a SECOND, independent
//! offer surfaces for the second card. It MUST FAIL (no second offer; the
//! whole draw completes on the first decline) before the DrawSequenceFrame
//! origin/kind routing lands.

use engine::game::effects::resolve_ability_chain;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, QuantityExpr, QuantityRef, ReplacementDefinition,
    ResolvedAbility, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::replacements::ReplacementEvent;

const STINKWEED_IMP_ORACLE: &str = "Flying\n\
Whenever this creature deals combat damage to a creature, destroy that creature.\n\
Dredge 5 (If you would draw a card, you may mill five cards instead. If you do, return this card from your graveyard to your hand.)";

/// CR 701.22a + CR 702.52a: scry 2, substituted into draw 2 by a mandatory
/// Scry→Draw replacement, must offer Stinkweed Imp's Dredge on the
/// substituted draw's first individual card.
#[test]
fn scry_substituted_draw_offers_dredge_per_individual_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    // Library >= Stinkweed Imp's Dredge 5, and >= the substituted draw 2 count.
    scenario.with_library_top(P0, &["Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6"]);
    scenario
        .add_creature_to_graveyard(P0, "Stinkweed Imp", 1, 2)
        .from_oracle_text(STINKWEED_IMP_ORACLE);
    let source = scenario.add_creature(P0, "Eligeth", 2, 2).id();

    let mut runner = scenario.build();

    // CR 614.6 + CR 121.6b: a mandatory "if you would scry, instead draw that
    // many cards" replacement (mirrors `scry.rs`'s
    // `scry_replacement_to_draw_delivers_through_resolver` unit test setup).
    let replacement =
        ReplacementDefinition::new(ReplacementEvent::Scry).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
        ));
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("Eligeth exists")
        .replacement_definitions
        .push(replacement);

    let ability = ResolvedAbility::new(
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("scry must propose its substituted draw");

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "Stinkweed Imp's Dredge must be offered for the substituted draw — got {:?}",
        runner.state().waiting_for
    );

    let hand_before_decline = runner.state().players[0].hand.len();

    // Decline Dredge for the first card (index 1 — matches the established
    // convention in replacement.rs's dredge tests).
    runner
        .act(GameAction::ChooseReplacement { index: 1 })
        .expect("decline Dredge for the first card");

    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before_decline + 1,
        "declining the first card's Dredge offer must draw exactly ONE card \
         (not the whole substituted count) before the second card is even \
         considered"
    );
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ),
        "CR 121.6b: Stinkweed Imp is still in the graveyard and still eligible, \
         so Dredge must be offered INDEPENDENTLY for the substituted draw's \
         second card — got {:?} (the whole 2-card draw resolved on a single \
         choice instead of being decomposed per individual draw)",
        runner.state().waiting_for
    );
}

const TEFERI_ORACLE: &str = "If you would draw a card except the first one you draw in each of your draw steps, draw two cards instead.";

/// CR 614.5: a replacement already applied to the substituted draw instruction
/// must not apply to it again when the scry resolver re-proposes the draw
/// through the sequence authority. Scry 2 → (mandatory Scry→Draw substitution)
/// draw 2 → Teferi's Ageless Insight doubles each individual card once → 4
/// cards, never 8. The event's `applied` set must be threaded into the frame,
/// not dropped.
#[test]
fn scry_substituted_draw_applies_draw_replacement_exactly_once_per_card() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(engine::types::phase::Phase::PreCombatMain);

    scenario.with_library_top(
        P0,
        &[
            "Lib 1", "Lib 2", "Lib 3", "Lib 4", "Lib 5", "Lib 6", "Lib 7", "Lib 8", "Lib 9",
            "Lib 10",
        ],
    );
    scenario
        .add_creature_from_oracle(P0, "Teferi's Ageless Insight", 0, 0, TEFERI_ORACLE)
        .as_enchantment();
    let source = scenario.add_creature(P0, "Eligeth", 2, 2).id();

    let mut runner = scenario.build();

    let replacement =
        ReplacementDefinition::new(ReplacementEvent::Scry).execute(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Ref {
                    qty: QuantityRef::EventContextAmount,
                },
                target: TargetFilter::Controller,
            },
        ));
    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .expect("Eligeth exists")
        .replacement_definitions
        .push(replacement);

    let ability = ResolvedAbility::new(
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 2 },
            target: TargetFilter::Controller,
        },
        vec![],
        source,
        P0,
    );

    let mut events = Vec::new();
    resolve_ability_chain(runner.state_mut(), &ability, &mut events, 0)
        .expect("scry must resolve through its substituted draw");

    assert_eq!(
        runner.state().players[0].hand.len(),
        4,
        "CR 614.5: the substituted 2-card draw doubled once per individual card \
         is exactly 4 cards — 8 means a replacement in the instruction's \
         `applied` set re-applied after the re-propose, 2 means the doubler \
         never saw the individual draws"
    );
}

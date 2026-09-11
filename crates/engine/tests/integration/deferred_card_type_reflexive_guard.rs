//! Runtime coverage for a reflexive trigger whose intervening-if condition is
//! owned by the ordered specialized card-type parser.
//!
//! CR 603.12 creates the separate "When you do" trigger. CR 603.4 checks its
//! intervening-if condition when the reveal finishes and again under CR 608.2a
//! when that trigger resolves. CR 701.20a keeps the card revealed for that
//! trigger, CR 608.2i preserves the reveal-time fact across the separate
//! resolution, and CR 701.20b leaves it on top of the library until a successful
//! draw moves that same card to hand.

use super::rules::{GameScenario, Phase, P0};
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::zones::Zone;

const ORACLE: &str = "At the beginning of your upkeep, reveal the top card of your library. When you do, if a creature card is revealed this way, draw a card.";

fn make_creature(state: &mut GameState, id: ObjectId) {
    let object = state.objects.get_mut(&id).unwrap();
    object.card_types.core_types.push(CoreType::Creature);
    object.base_card_types.core_types.push(CoreType::Creature);
}

#[test]
fn specialized_card_type_guard_draws_for_a_revealed_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    scenario.add_enchantment_from_oracle(P0, "Reflexive Reveal", ORACLE);
    let top = scenario.add_card_to_library_top(P0, "Top Creature");

    let mut runner = scenario.build();
    make_creature(runner.state_mut(), top);
    let hand_before = runner.state().players[P0.0 as usize].hand.len();

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Hand,
        "the specialized creature-card guard must permit the reflexive draw"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before + 1,
        "the guarded reflexive trigger must draw exactly the revealed top card"
    );
}

#[test]
fn specialized_card_type_guard_stops_the_draw_for_a_noncreature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::Untap);
    let source = scenario
        .add_enchantment_from_oracle(P0, "Reflexive Reveal", ORACLE)
        .id();
    let top = scenario.add_card_to_library_top(P0, "Top Noncreature");

    let mut runner = scenario.build();
    let hand_before = runner.state().players[P0.0 as usize].hand.len();
    assert!(
        !runner.state().objects[&top]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the negative fixture must put a noncreature on top"
    );
    assert!(
        !runner.state().objects[&source].has_unimplemented_mechanics(),
        "the negative runtime fixture must exercise the modeled specialized guard"
    );

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&top].zone,
        Zone::Library,
        "a noncreature must remain on top when the intervening-if guard fails"
    );
    assert_eq!(
        runner.state().players[P0.0 as usize].hand.len(),
        hand_before,
        "the failed specialized guard must prevent the reflexive draw"
    );
}

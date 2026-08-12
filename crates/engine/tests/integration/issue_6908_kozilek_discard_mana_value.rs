//! Kozilek, the Great Distortion — a discard cost's X must be announced before
//! target selection and bind both the target spell and the discarded card.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::game_state::{CastingVariant, StackEntry, StackEntryKind};
use engine::types::mana::ManaCost;
use engine::types::zones::Zone;

const KOZILEK_COUNTER_ABILITY: &str =
    "Discard a card with mana value X: Counter target spell with mana value X.";

/// CR 107.3a + CR 601.2b/c + CR 602.2b: X in an activation cost is announced
/// before selecting targets, then the same value restricts both the target and
/// the discarded card.
#[test]
fn kozilek_discards_a_card_matching_announced_x_to_counter_a_spell() {
    let mut scenario = GameScenario::new();
    let kozilek = scenario
        .add_creature_from_oracle(
            P0,
            "Kozilek, the Great Distortion",
            12,
            12,
            KOZILEK_COUNTER_ABILITY,
        )
        .id();
    let discard = scenario
        .add_spell_to_hand(P0, "Mana Value Three Discard", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let target = scenario
        .add_spell_to_hand(P1, "Mana Value Three Target", false)
        .with_mana_cost(ManaCost::generic(3))
        .id();
    let mut runner = scenario.build();

    {
        let state = runner.state_mut();
        let card_id = state.objects[&target].card_id;
        let mut events = Vec::new();
        move_to_zone(state, target, Zone::Stack, &mut events);
        state.stack.push_back(StackEntry {
            id: target,
            source_id: target,
            controller: P1,
            kind: StackEntryKind::Spell {
                card_id,
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
    }

    let outcome = runner
        .activate(kozilek, 0)
        .x(3)
        .target_object(target)
        .pay_with(&[discard])
        .resolve();

    outcome.assert_zone(&[discard, target], Zone::Graveyard);
}

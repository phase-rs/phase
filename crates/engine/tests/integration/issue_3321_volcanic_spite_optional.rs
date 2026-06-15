//! Issue #3321 — Volcanic Spite must prompt for the optional bottom-and-draw rider.

use engine::game::scenario::{GameScenario, P0};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;

const VOLCANIC_SPITE: &str = "Volcanic Spite deals 3 damage to target creature, planeswalker, or battle. You may put a card from your hand on the bottom of your library. If you do, draw a card.";

const RIGHTEOUS_VALKYRIE: &str = "Flying\nWhenever another Angel or Cleric you control enters, you gain life equal to that creature's toughness.\nAs long as you have at least 7 life more than your starting life total, creatures you control get +2/+2.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn volcanic_spite_prompts_optional_bottom_and_draw_after_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let valkyrie = scenario
        .add_creature_from_oracle(P0, "Righteous Valkyrie", 2, 4, RIGHTEOUS_VALKYRIE)
        .flying()
        .id();
    let spite = scenario
        .add_spell_to_hand_from_oracle(P0, "Volcanic Spite", true, VOLCANIC_SPITE)
        .id();
    scenario.add_spell_to_hand(P0, "Hand Filler", true);
    scenario.with_mana_pool(P0, floating_mana(3, ManaType::Red));

    let mut runner = scenario.build();
    let card_id = runner.state().objects[&spite].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: spite,
            card_id,
            targets: vec![valkyrie],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Volcanic Spite must be castable");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { player, .. } if player == P0
        ),
        "Volcanic Spite must prompt to bottom a card after dealing damage, got {:?}",
        runner.state().waiting_for
    );
}

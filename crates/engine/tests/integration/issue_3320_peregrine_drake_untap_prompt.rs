//! Issue #3320 — Peregrine Drake must prompt to untap lands, not sacrifice them.

use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaColor, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const PEREGRINE_DRAKE: &str = "Flying\nWhen this creature enters, untap up to five lands.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn peregrine_drake_etb_prompts_untap_lands_choice() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let mut land_ids = Vec::new();
    for _ in 0..3 {
        land_ids.push(scenario.add_basic_land(P0, ManaColor::Blue));
    }

    let drake = scenario
        .add_creature_to_hand_from_oracle(P0, "Peregrine Drake", 2, 3, PEREGRINE_DRAKE)
        .flying()
        .id();
    scenario.with_mana_pool(P0, floating_mana(4, ManaType::Blue));

    let mut runner = scenario.build();
    for land in land_ids {
        runner.state_mut().objects.get_mut(&land).unwrap().tapped = true;
    }
    let card_id = runner.state().objects[&drake].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: drake,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Peregrine Drake must be castable");

    runner.advance_until_stack_empty();

    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice {
                player,
                effect_kind: EffectKind::Untap,
                zone: Zone::Battlefield,
                up_to: true,
                ..
            } if player == P0
        ),
        "Peregrine Drake must prompt to untap up to five lands, got {:?}",
        runner.state().waiting_for
    );
}

//! Regression for issue #3874: declining Emet-Selch's optional graveyard cast
//! must not run the sequential exile-instead-of-graveyard rider.
//!
//! https://github.com/phase-rs/phase/issues/3874

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const EMET_SELCH_ORACLE: &str = "Spells you cast from your graveyard cost {2} less to cast.\n\
    Whenever one or more opponents lose life, you may cast target instant or sorcery card from your graveyard. If that spell would be put into your graveyard, exile it instead. Do this only once each turn.";

const SHOCK_ORACLE: &str = "Shock deals 2 damage to any target.";

fn floating_mana(n: usize, ty: ManaType) -> Vec<ManaUnit> {
    (0..n)
        .map(|_| ManaUnit::new(ty, ObjectId(0), false, vec![]))
        .collect()
}

#[test]
fn emet_selch_declining_graveyard_cast_does_not_exile_spell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Emet-Selch of the Third Seat", 2, 2, EMET_SELCH_ORACLE)
        .id();
    let p1_creature = scenario.add_creature(P1, "Target", 2, 2).id();
    let shock_in_graveyard = scenario
        .add_spell_to_graveyard(P0, "Shock", true)
        .from_oracle_text(SHOCK_ORACLE)
        .id();
    let shock_in_hand = scenario
        .add_spell_to_hand_from_oracle(P0, "Shock", true, SHOCK_ORACLE)
        .id();
    scenario.with_mana_pool(P0, floating_mana(1, ManaType::Red));

    let mut runner = scenario.build();
    runner
        .cast(shock_in_hand)
        .target_objects(&[p1_creature])
        .resolve();

    for _ in 0..20 {
        if matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ) {
            runner
                .act(GameAction::DecideOptionalEffect { accept: false })
                .expect("decline optional graveyard cast");
            break;
        }
        runner.advance_until_stack_empty();
    }

    runner.advance_until_stack_empty();

    assert_eq!(
        runner.state().objects[&shock_in_graveyard].zone,
        Zone::Graveyard,
        "declining the optional cast must leave the graveyard card in the graveyard"
    );
    assert!(
        !runner.state().exile.contains(&shock_in_graveyard),
        "declining must not exile the graveyard target via the cast rider"
    );
}

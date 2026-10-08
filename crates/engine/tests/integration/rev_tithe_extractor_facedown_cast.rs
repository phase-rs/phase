//! End-to-end regression for Rev, Tithe Extractor's face-down exile permission.
//!
//! CR 406.3a-b lets the permission holder inspect the face-down exiled card;
//! CR 601.2a lets that player cast it; CR 611.2a keeps the permission after Rev
//! leaves the battlefield, for as long as the card remains exiled.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::ability::{CastingPermission, Duration};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::run_combat;

const REV_DAMAGE_TRIGGER: &str = "Whenever one or more creatures you control deal combat damage to a player, create a Treasure token, then look at the top card of that player's library and exile it face down. You may cast that card for as long as it remains exiled.";

#[test]
fn rev_reveals_and_casts_the_opponents_facedown_exiled_card() {
    let mut scenario = GameScenario::new_n_player(2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let rev = scenario
        .add_creature_from_oracle(P0, "Rev, Tithe Extractor", 3, 3, REV_DAMAGE_TRIGGER)
        .id();
    let attacker = scenario.add_creature(P0, "Rev Test Attacker", 2, 2).id();
    let deep = scenario.add_card_to_library_top(P1, "Opponent Deep Card");
    let exiled = scenario
        .add_spell_to_library_top(P1, "Opponent Zero-Cost Instant", true)
        .with_mana_cost(ManaCost::zero())
        .id();
    let removal = scenario
        .add_spell_to_hand_from_oracle(P0, "Rev Test Removal", true, "Destroy target creature.")
        .with_mana_cost(ManaCost::zero())
        .id();
    let expected_name = "Opponent Zero-Cost Instant";

    let mut runner = scenario.build();
    run_combat(&mut runner, vec![attacker], vec![]);

    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("Rev's optional permission should be accepted");
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger ordering should succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().objects[&exiled].zone == Zone::Exile
                    && runner.state().stack.is_empty()
                {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority pass should advance Rev's trigger");
            }
            ref other => panic!("unexpected prompt while resolving Rev: {other:?}"),
        }
    }

    assert_eq!(runner.state().objects[&exiled].zone, Zone::Exile);
    assert!(runner.state().objects[&exiled].face_down);
    assert_eq!(runner.state().objects[&deep].zone, Zone::Library);
    assert!(runner.state().objects.values().any(|object| {
        object.zone == Zone::Battlefield
            && object.controller == P0
            && object
                .card_types
                .subtypes
                .iter()
                .any(|kind| kind == "Treasure")
    }));
    assert!(runner.state().objects[&exiled]
        .casting_permissions
        .iter()
        .any(|permission| matches!(
            permission,
            CastingPermission::PlayFromExile {
                granted_to: P0,
                duration: Duration::Permanent,
                ..
            }
        )));

    assert_eq!(
        filter_state_for_viewer(runner.state(), P0).objects[&exiled].name,
        expected_name
    );
    assert_eq!(
        filter_state_for_viewer(runner.state(), P1).objects[&exiled].name,
        "Hidden Card"
    );

    runner.cast(removal).target_object(rev).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(runner.state().objects[&rev].zone, Zone::Graveyard);
    assert_eq!(
        filter_state_for_viewer(runner.state(), P0).objects[&exiled].name,
        expected_name,
        "Rev may leave play without ending the exile-scoped permission"
    );

    let cast = engine::ai_support::legal_actions(runner.state())
        .into_iter()
        .find(|action| {
            matches!(
                action,
                GameAction::CastSpell { object_id, .. } if *object_id == exiled
            )
        })
        .expect("Rev's controller must be offered the exiled card as a cast action");
    runner
        .act(cast)
        .expect("casting Rev's exiled card should start");
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&exiled].zone,
        Zone::Graveyard,
        "the zero-cost instant should resolve after being cast from exile"
    );
}

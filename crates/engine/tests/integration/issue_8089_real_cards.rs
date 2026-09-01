//! Production-path regressions for #8089's two reported copy cards.

use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::turns::execute_cleanup;
use engine::game::zone_pipeline::{move_object_for_test, ZoneMoveRequest};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const FACETAKER_ORACLE: &str = "This creature can't be blocked.\nAt the beginning of combat on your turn, you may have this creature become a copy of another target creature until end of turn, except it's 1/4 and has \"This creature can't be blocked.\"";
const GLASSPOOL_ORACLE: &str = "You may have this creature enter as a copy of a creature you control, except it's a Shapeshifter Rogue in addition to its other types.";

fn resolve_facetaker_combat_trigger(runner: &mut GameRunner, target: ObjectId) {
    // Advance through the beginning-of-combat priority window so the printed
    // trigger is actually put on the stack; merely arriving at BeginCombat
    // leaves that window unopened.
    runner.advance_to_phase(Phase::DeclareAttackers);

    for _ in 0..64 {
        match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { .. } => runner.advance_until_stack_empty(),
            WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(target)),
                    })
                    .expect("Facetaker's trigger must accept its other-creature target");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept Facetaker's optional copy");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass priority while Facetaker's trigger resolves");
            }
            other => panic!("unexpected Facetaker trigger prompt: {other:?}"),
        }
    }
    panic!("Facetaker trigger did not settle");
}

#[test]
fn cephalid_facetaker_copy_expires_at_cleanup() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Facetaker Target", 5, 5).id();
    let facetaker = scenario
        .add_creature_from_oracle(P0, "Cephalid Facetaker", 1, 4, FACETAKER_ORACLE)
        .id();
    let mut runner = scenario.build();

    resolve_facetaker_combat_trigger(&mut runner, target);
    assert_eq!(runner.state().objects[&facetaker].name, "Facetaker Target");

    execute_cleanup(runner.state_mut(), &mut Vec::new());
    flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&facetaker].name,
        "Cephalid Facetaker",
        "the printed end-of-turn copy duration must expire during cleanup"
    );
}

#[test]
fn cephalid_facetaker_live_copy_does_not_follow_it_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Facetaker Target", 5, 5).id();
    let facetaker = scenario
        .add_creature_from_oracle(P0, "Cephalid Facetaker", 1, 4, FACETAKER_ORACLE)
        .id();
    let mut runner = scenario.build();

    resolve_facetaker_combat_trigger(&mut runner, target);
    assert_eq!(
        runner.state().objects[&facetaker].name,
        "Facetaker Target",
        "precondition: the until-end-of-turn copy must be live before it changes zones"
    );

    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(facetaker, Zone::Hand, target),
            &mut Vec::new(),
        ),
        "Facetaker's return to hand must complete without a replacement choice"
    );
    flush_layers(runner.state_mut());
    assert_eq!(runner.state().objects[&facetaker].zone, Zone::Hand);
    assert_eq!(
        runner.state().objects[&facetaker].name,
        "Cephalid Facetaker",
        "a new hand incarnation must retain its own identity, not the live battlefield copy"
    );
}

#[test]
fn glasspool_mimic_copy_target_choice_does_not_survive_a_return_to_hand() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let target = scenario.add_creature(P0, "Glasspool Target", 5, 5).id();
    let mimic = scenario
        .add_creature_to_hand_from_oracle(P0, "Glasspool Mimic", 0, 0, GLASSPOOL_ORACLE)
        .id();
    let mut runner = scenario.build();
    let card_id = runner.state().objects[&mimic].card_id;

    runner
        .act(GameAction::CastSpell {
            object_id: mimic,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Glasspool Mimic");
    runner.advance_until_stack_empty();
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::ReplacementChoice { .. }
    ));
    runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("accept Glasspool Mimic's enter-as-copy replacement");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CopyTargetChoice { .. }
    ));
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target)),
        })
        .expect("choose Glasspool Mimic's copy target");

    assert_eq!(runner.state().objects[&mimic].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&mimic].name, "Glasspool Target");

    assert!(
        !move_object_for_test(
            runner.state_mut(),
            ZoneMoveRequest::effect(mimic, Zone::Hand, target),
            &mut Vec::new(),
        ),
        "Glasspool Mimic's return to hand must complete without a replacement choice"
    );
    flush_layers(runner.state_mut());
    assert_eq!(
        runner.state().objects[&mimic].name,
        "Glasspool Mimic",
        "the permanent copy's recipient pin must not apply to the new hand incarnation"
    );
}

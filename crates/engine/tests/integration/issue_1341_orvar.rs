//! Issue #1341: Orvar, the All-Form must not trigger when an instant or sorcery
//! you cast targets only permanents an opponent controls (e.g. Pongify on their creature).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ORVAR: &str = "Changeling\n\
Whenever you cast an instant or sorcery spell, if it targets one or more other permanents you control, create a token that's a copy of one of those permanents.\n\
When a spell or ability an opponent controls causes you to discard this card, create a token that's a copy of target permanent.";

const PONGIFY: &str =
    "Destroy target creature. It can't be regenerated. Create a 3/3 green Ape creature token.";

fn add_blue_mana(runner: &mut GameRunner, n: usize) {
    for _ in 0..n {
        runner
            .state_mut()
            .players
            .iter_mut()
            .find(|p| p.id == P0)
            .unwrap()
            .mana_pool
            .add(ManaUnit::new(ManaType::Blue, ObjectId(0), false, vec![]));
    }
}

fn battlefield_creature_count(runner: &GameRunner, player: engine::types::PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| o.zone == Zone::Battlefield && o.controller == player)
        .filter(|o| o.card_types.core_types.contains(&CoreType::Creature))
        .count()
}

fn drive_cast_target(runner: &mut GameRunner, target: TargetRef) {
    for _ in 0..48 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(target.clone()),
                    })
                    .expect("choose cast target");
            }
            WaitingFor::TriggerTargetSelection {
                target_slots,
                selection,
                ..
            } => {
                let t = target_slots[selection.current_slot]
                    .legal_targets
                    .first()
                    .cloned();
                runner
                    .act(GameAction::ChooseTarget { target: t })
                    .expect("choose copy target");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            _ => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
        }
    }
}

#[test]
fn orvar_does_not_trigger_when_pongify_targets_opponent_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Orvar, the All-Form", 3, 3, ORVAR);
    let pongify = scenario
        .add_spell_to_hand_from_oracle(P0, "Pongify", true, PONGIFY)
        .id();
    let opponent_creature = scenario.add_vanilla(P1, 4, 4);

    let mut runner = scenario.build();
    add_blue_mana(&mut runner, 2);

    let card_id = runner.state().objects[&pongify].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: pongify,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Pongify cast");

    drive_cast_target(&mut runner, TargetRef::Object(opponent_creature));
    runner.advance_until_stack_empty();

    assert_eq!(
        battlefield_creature_count(&runner, P0),
        2,
        "Pongify should leave Orvar plus its Ape token only — no Orvar copy of the opponent's creature"
    );
}

#[test]
fn orvar_triggers_when_pongify_targets_own_other_creature() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature_from_oracle(P0, "Orvar, the All-Form", 3, 3, ORVAR);
    let own_creature = scenario.add_vanilla(P0, 4, 4);
    let pongify = scenario
        .add_spell_to_hand_from_oracle(P0, "Pongify", true, PONGIFY)
        .id();

    let mut runner = scenario.build();
    add_blue_mana(&mut runner, 2);

    let before = battlefield_creature_count(&runner, P0);

    let card_id = runner.state().objects[&pongify].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: pongify,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Pongify cast");

    drive_cast_target(&mut runner, TargetRef::Object(own_creature));
    runner.advance_until_stack_empty();

    assert!(
        battlefield_creature_count(&runner, P0) > before,
        "Orvar should create a token when Pongify targets another creature you control"
    );
}

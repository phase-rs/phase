//! CR 800.4b + CR 701.12a: an exchange of control whose half would go to a player who has left the game does nothing.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{ContinuousModification, EffectKind, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const AVARICE_TOTEM: &str = "{5}: Exchange control of this artifact and target nonland permanent.";
const RAY_OF_COMMAND: &str = "Untap target creature an opponent controls and gain control of it \
until end of turn. That creature gains haste until end of turn. When you lose control of the \
creature, tap it.";

fn control_effect_controllers(runner: &GameRunner, id: ObjectId) -> Vec<PlayerId> {
    runner
        .state()
        .transient_continuous_effects
        .iter()
        .filter(|effect| {
            effect.affected == TargetFilter::SpecificObject { id }
                && effect
                    .modifications
                    .iter()
                    .any(|m| matches!(m, ContinuousModification::ChangeController))
        })
        .map(|effect| effect.controller)
        .collect()
}

/// Bears owned by P3 under P1's base control are taken by P2 with Ray of Command; P1 optionally
/// concedes; on the next turn P0 exchanges the Totem for the Bears. Returns (Totem control-effect
/// controllers, Bears control-effect controllers, `ControllerChanged` count, the Totem's
/// `ExchangeControl` resolved).
fn exchange_after_temporary_control(
    p1_concedes: bool,
) -> (Vec<PlayerId>, Vec<PlayerId>, usize, bool) {
    let mut scenario = GameScenario::new_n_player(4, 7);
    scenario.at_phase(Phase::PreCombatMain);
    let totem = scenario
        .add_artifact_from_oracle(P0, "Avarice Totem", AVARICE_TOTEM)
        .id();
    let bears = scenario
        .add_creature(P3, "Grizzly Bears", 2, 2)
        .controlled_by(P1)
        .id();
    let ray = scenario
        .add_spell_to_hand_from_oracle(P2, "Ray of Command", true, RAY_OF_COMMAND)
        .id();
    let mut runner = scenario.build();
    let first_turn = runner.state().turn_number;

    for _ in 0..6 {
        if matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P2) {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    let _ = runner.cast(ray).target_objects(&[bears]).commit();
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().objects[&bears].controller, P2);

    if p1_concedes {
        runner.act(GameAction::Concede { player_id: P1 }).unwrap();
        assert!(runner.state().players[1].is_eliminated);
        assert_eq!(runner.state().objects[&bears].zone, Zone::Battlefield);
    }

    for _ in 0..80 {
        if runner.state().turn_number != first_turn
            && matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == P0)
        {
            break;
        }
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).unwrap();
            }
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_ne!(runner.state().turn_number, first_turn);
    assert_eq!(runner.state().objects[&bears].controller, P1);
    assert!(control_effect_controllers(&runner, bears).is_empty());

    for _ in 0..5 {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            ManaType::Colorless,
            ObjectId(0),
            false,
            vec![],
        ));
    }
    runner
        .act(GameAction::ActivateAbility {
            source_id: totem,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activation is accepted");
    for _ in 0..8 {
        match &runner.state().waiting_for {
            WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(bears)),
                    })
                    .expect("target accepted");
            }
            WaitingFor::Priority { .. } => break,
            other => panic!("unexpected prompt: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().stack.len(),
        1,
        "the exchange is on the stack"
    );

    let mut events = Vec::new();
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        events.extend(runner.act(GameAction::PassPriority).unwrap().events);
    }
    assert!(runner.state().stack.is_empty());
    (
        control_effect_controllers(&runner, totem),
        control_effect_controllers(&runner, bears),
        events
            .iter()
            .filter(|event| matches!(event, GameEvent::ControllerChanged { .. }))
            .count(),
        events.iter().any(|event| {
            matches!(
                event,
                GameEvent::EffectResolved { kind: EffectKind::ExchangeControl, source_id, .. }
                    if *source_id == totem
            )
        }),
    )
}

#[test]
fn avarice_totem_exchange_with_a_departed_recipient_does_nothing() {
    assert_eq!(
        exchange_after_temporary_control(true),
        (vec![], vec![], 0, true)
    );
}

#[test]
fn avarice_totem_exchange_between_living_recipients_completes() {
    assert_eq!(
        exchange_after_temporary_control(false),
        (vec![P1], vec![P0], 2, true)
    );
}

//! Issue #5269 — Zoraline's optional composite payment must queue its
//! reflexive return trigger.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{AbilityCondition, Effect, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const ZORALINE: &str = "Flying, vigilance\n\
Whenever a Bat you control attacks, you gain 1 life.\n\
Whenever Zoraline enters or attacks, you may pay {W}{B} and 2 life. When you do, return target nonland permanent card with mana value 3 or less from your graveyard to the battlefield with a finality counter on it.";

fn add_mana(runner: &mut engine::game::scenario::GameRunner, colors: &[ManaType]) {
    for &color in colors {
        runner.state_mut().players[0].mana_pool.add(ManaUnit::new(
            color,
            ObjectId(0),
            false,
            vec![],
        ));
    }
}

#[test]
fn zoraline_paid_attack_trigger_returns_graveyard_permanent_with_finality() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let zoraline = scenario
        .add_creature_from_oracle(P0, "Zoraline, Cosmos Caller", 3, 3, ZORALINE)
        .id();
    let returned = scenario
        .add_creature_to_graveyard(P0, "Returned Permanent", 2, 2)
        .with_mana_cost(ManaCost::generic(2))
        .id();

    let mut runner = scenario.build();

    let zoraline_trigger = runner.state().objects[&zoraline]
        .trigger_definitions
        .as_slice()
        .iter()
        .find(|trigger| {
            trigger
                .execute
                .as_ref()
                .is_some_and(|execute| matches!(execute.effect.as_ref(), Effect::PayCost { .. }))
        })
        .expect("Zoraline enters-or-attacks trigger");
    let execute = zoraline_trigger.execute.as_ref().expect("trigger execute");
    assert!(
        matches!(execute.effect.as_ref(), Effect::PayCost { .. }),
        "trigger must begin with the optional PayCost, got {:?}",
        execute.effect
    );
    let reflexive = execute
        .sub_ability
        .as_ref()
        .expect("PayCost must carry reflexive return sub-ability");
    assert_eq!(
        reflexive.condition,
        Some(AbilityCondition::WhenYouDo),
        "Zoraline's return clause must remain a CR 603.12 reflexive trigger"
    );
    let Effect::ChangeZone {
        origin: Some(Zone::Graveyard),
        destination: Zone::Battlefield,
        target,
        ..
    } = reflexive.effect.as_ref()
    else {
        panic!(
            "reflexive must target a graveyard permanent card to return, got {:?}",
            reflexive.effect
        );
    };
    assert!(
        matches!(target, TargetFilter::Typed(_)),
        "return target must be a typed graveyard filter, got {target:?}"
    );
    assert!(
        find_legal_targets(runner.state(), target, P0, zoraline)
            .contains(&TargetRef::Object(returned)),
        "staged graveyard permanent must be a legal Zoraline return target"
    );

    for step in 0..120 {
        if runner.state().objects[&returned].zone == Zone::Battlefield {
            break;
        }

        match runner.state().waiting_for.clone() {
            WaitingFor::DeclareAttackers { .. } => {
                runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![(zoraline, AttackTarget::Player(P1))],
                        bands: vec![],
                    })
                    .expect("declare Zoraline attacking");
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                add_mana(&mut runner, &[ManaType::White, ManaType::Black]);
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("pay Zoraline optional cost");
                assert!(
                    !runner.state().cost_payment_failed_flag,
                    "Zoraline's W/B plus 2 life payment must be payable"
                );
            }
            WaitingFor::TriggerTargetSelection { .. } | WaitingFor::TargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(TargetRef::Object(returned)),
                    })
                    .expect("target graveyard permanent");
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!(
                "unexpected waiting state before Zoraline returned target at step {step}: {other:?}; stack={:?}; pending_continuation={:?}; pending_optional={}; cost_failed={}; p0_life={}",
                runner.state().stack,
                runner.state().pending_continuation,
                runner.state().pending_optional_effect.is_some(),
                runner.state().cost_payment_failed_flag,
                runner.state().players[0].life
            ),
        }
    }

    assert_eq!(
        runner.state().objects[&returned].zone,
        Zone::Battlefield,
        "paid Zoraline reflexive trigger must return the target; waiting={:?}, stack={:?}",
        runner.state().waiting_for,
        runner.state().stack
    );
    assert_eq!(
        runner.state().objects[&returned]
            .counters
            .get(&CounterType::Finality)
            .copied(),
        Some(1),
        "returned permanent must enter with a finality counter"
    );
}

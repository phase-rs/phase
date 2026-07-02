//! Regression for issue #4566: Jocasta, Automaton Avenger — graveyard trigger
//! "return it to the battlefield tapped and attacking" must enter both tapped
//! and attacking (CR 508.4 + CR 614.1).

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const JOCASTA_ORACLE: &str = "Flying\n\
    Whenever your commander deals combat damage to a player, put a +1/+1 counter on Jocasta.\n\
    Whenever you attack with your commander, if this card is in your graveyard, you may return it to the battlefield tapped and attacking.";

fn accept_optional_return_and_resolve(runner: &mut GameRunner) {
    let mut saw_optional = false;
    for _ in 0..80 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice { .. } => {
                saw_optional = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept optional return");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    assert!(
                        saw_optional,
                        "must reach OptionalEffectChoice before stack drains; waiting={:?}",
                        runner.state().waiting_for
                    );
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("unexpected waiting state during Jocasta resolution: {other:?}"),
        }
    }
    panic!("Jocasta resolution did not settle");
}

#[test]
fn jocasta_returns_tapped_and_attacking_from_graveyard_on_commander_attack() {
    let parsed = parse_oracle_text(
        JOCASTA_ORACLE,
        "Jocasta, Automaton Avenger",
        &[],
        &["Creature".to_string()],
        &[],
    );
    let return_trigger = parsed
        .triggers
        .iter()
        .find(|t| {
            t.execute
                .as_ref()
                .is_some_and(|a| matches!(a.effect.as_ref(), Effect::ChangeZone { .. }))
        })
        .expect("parsed return trigger");
    match return_trigger.execute.as_ref().unwrap().effect.as_ref() {
        Effect::ChangeZone {
            origin,
            enter_tapped,
            enters_attacking,
            target,
            ..
        } => {
            assert_eq!(*origin, Some(Zone::Graveyard));
            assert!(enter_tapped.is_tapped());
            assert!(enters_attacking);
            assert!(matches!(
                target,
                engine::types::ability::TargetFilter::SelfRef
            ));
        }
        other => panic!("expected ChangeZone, got {other:?}"),
    }

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let jocasta = scenario
        .add_creature_to_graveyard(P0, "Jocasta, Automaton Avenger", 2, 2)
        .from_oracle_text(JOCASTA_ORACLE)
        .id();
    let commander = scenario.add_creature(P0, "Commander", 3, 3).id();

    let mut runner = scenario.build();
    runner
        .state_mut()
        .objects
        .get_mut(&commander)
        .expect("commander exists")
        .is_commander = true;

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(commander, AttackTarget::Player(P1))])
        .expect("declare commander attack");

    assert!(
        !runner.state().stack.is_empty() || matches!(
            runner.state().waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
        ),
        "commander attack must put Jocasta's optional trigger on the stack or prompt; stack={:?} waiting={:?}",
        runner.state().stack,
        runner.state().waiting_for
    );

    accept_optional_return_and_resolve(&mut runner);

    let jocasta_zone = runner.state().objects.get(&jocasta).map(|o| o.zone);
    assert_eq!(
        jocasta_zone,
        Some(Zone::Battlefield),
        "Jocasta must return to the battlefield; stack={:?} waiting={:?}",
        runner.state().stack,
        runner.state().waiting_for
    );

    let jocasta_obj = runner
        .state()
        .objects
        .get(&jocasta)
        .expect("Jocasta must exist");
    assert_eq!(jocasta_obj.zone, Zone::Battlefield);
    assert!(jocasta_obj.tapped, "Jocasta must enter tapped (CR 614.1)");

    let attackers: Vec<ObjectId> = runner
        .state()
        .combat
        .as_ref()
        .expect("combat must be active")
        .attackers
        .iter()
        .map(|a| a.object_id)
        .collect();
    assert!(
        attackers.contains(&jocasta),
        "Jocasta must enter attacking (CR 508.4); attackers: {attackers:?}"
    );
}

//! Runtime regression for Flamerush Rider's attack trigger: copy another
//! attacking creature, enter tapped and attacking, then exile the token at
//! end of combat.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use super::rules::AttackTarget;

const FLAMERUSH_RIDER_ORACLE: &str = "Whenever this creature attacks, create a token that's a copy of another target attacking creature and that's tapped and attacking. Exile the token at end of combat.\n\
    Dash {2}{R}{R} (You may cast this spell for its dash cost. If you do, it gains haste, and it's returned from the battlefield to its owner's hand at the beginning of the next end step.)";

fn rider_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.controller == P0
                && object.is_token
                && object.name == "Test Bear"
                && object.zone == Zone::Battlefield
        })
        .map(|object| object.id)
        .collect()
}

fn resolve_rider_trigger(
    runner: &mut GameRunner,
    rider: ObjectId,
    source: ObjectId,
    second_source: ObjectId,
) {
    let mut selected_target = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::TriggerTargetSelection { target_slots, .. } => {
                let legal = &target_slots[0].legal_targets;
                assert!(
                    legal.contains(&TargetRef::Object(source)),
                    "the other attacking creature must be a legal target; legal={legal:?}"
                );
                assert!(
                    legal.contains(&TargetRef::Object(second_source)),
                    "a second other attacking creature must be a legal target; legal={legal:?}"
                );
                assert!(
                    !legal.contains(&TargetRef::Object(rider)),
                    "Rider must not be a legal target for 'another' attacking creature"
                );
                selected_target = true;
                runner
                    .act(GameAction::SelectTargets {
                        targets: vec![TargetRef::Object(source)],
                    })
                    .expect("choose the other attacking creature to copy");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    assert!(selected_target, "Rider's target selection must occur");
                    return;
                }
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            WaitingFor::OrderTriggers { .. } => {
                runner
                    .act(GameAction::OrderTriggers { order: vec![] })
                    .expect("order attack triggers");
            }
            other => panic!("unexpected wait while resolving Flamerush Rider: {other:?}"),
        }
    }
    panic!("Flamerush Rider's attack trigger did not resolve");
}

#[test]
fn flamerush_rider_copies_an_attacker_tapped_attacking_until_end_of_combat() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    let rider = scenario
        .add_creature(P0, "Flamerush Rider", 3, 3)
        .from_oracle_text_with_keywords(&["Dash"], FLAMERUSH_RIDER_ORACLE)
        .id();
    let source = scenario
        .add_creature(P0, "Test Bear", 2, 2)
        .with_keyword(Keyword::Vigilance)
        .id();
    let second_source = scenario.add_creature(P0, "Second Bear", 2, 2).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (rider, AttackTarget::Player(P1)),
            (source, AttackTarget::Player(P1)),
            (second_source, AttackTarget::Player(P1)),
        ])
        .expect("declare Flamerush Rider and the copy source attacking P1");

    resolve_rider_trigger(&mut runner, rider, source, second_source);

    let tokens = rider_tokens(&runner);
    assert_eq!(tokens.len(), 1, "Rider must create one copy token");
    let token = tokens[0];
    assert_ne!(
        token, source,
        "the token must have an identity distinct from its copy source"
    );
    let copied = runner
        .state()
        .objects
        .get(&token)
        .expect("copy token exists");
    assert!(
        copied.tapped,
        "CR 110.5b: the token enters tapped as specified"
    );
    assert_eq!(copied.power, Some(2), "the token copies the source's power");
    assert_eq!(
        copied.toughness,
        Some(2),
        "the token copies the source's toughness"
    );
    assert_eq!(
        copied.name, "Test Bear",
        "the token copies the source's name"
    );

    let attacking: Vec<_> = runner
        .state()
        .combat
        .as_ref()
        .expect("combat remains active during the attack trigger")
        .attackers
        .iter()
        .map(|attacker| attacker.object_id)
        .collect();
    assert!(
        attacking.contains(&token),
        "CR 508.4: the copy token enters and remains attacking; attackers={attacking:?}"
    );

    // CR 603.7: resolving the attack trigger created a delayed triggered
    // ability; CR 511.3: the end-of-combat step ends before postcombat main.
    assert_eq!(runner.state().objects[&source].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&source].name, "Test Bear");
    assert!(
        !runner.state().objects[&source].tapped,
        "Vigilance leaves the selected copy source untapped after attacking"
    );
    let mut events = runner.combat_damage().events().to_vec();
    assert_eq!(runner.state().phase, Phase::EndCombat);
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() && runner.state().phase == Phase::PostCombatMain
                {
                    break;
                }
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("pass priority through end-combat trigger")
                        .events,
                );
            }
            WaitingFor::OrderTriggers { .. } => events.extend(
                runner
                    .act(GameAction::OrderTriggers { order: vec![] })
                    .expect("order end-combat triggers")
                    .events,
            ),
            other => panic!("unexpected wait while advancing through end of combat: {other:?}"),
        }
    }
    assert_eq!(runner.state().phase, Phase::PostCombatMain);

    assert!(
        events.iter().any(|event| matches!(
            event,
            GameEvent::ZoneChanged {
                object_id,
                from: Some(Zone::Battlefield),
                to: Zone::Exile,
                ..
            } if object_id == &token
        )),
        "the delayed end-of-combat ability must exile the token"
    );
    // CR 704.5d: once exiled, the token ceases to exist.
    assert!(!runner.state().objects.contains_key(&token));
    assert_eq!(
        runner.state().objects[&source].zone,
        Zone::Battlefield,
        "exiling the copy must leave its source on the battlefield"
    );
    assert!(
        !runner.state().objects[&source].tapped,
        "the vigilant copy source remains untapped through combat"
    );
}

//! Gut's attack trigger must offer each independently determined sacrifice type.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::EffectKind;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const GUT: &str = "Whenever you attack, you may sacrifice another creature or an artifact. If you do, create a 4/1 black Skeleton creature token with menace that's tapped and attacking. (It can't be blocked except by two or more creatures.)\nChoose a Background (You can have a Background as a second commander.)";

fn board(
    artifact_controllers: &[PlayerId],
) -> (GameRunner, ObjectId, ObjectId, ObjectId, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gut = scenario
        .add_creature_from_oracle(P0, "Gut, True Soul Zealot", 2, 2, GUT)
        .id();
    let attacker = scenario.add_creature(P0, "Ally", 2, 2).id();
    let fodder = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let artifacts = artifact_controllers
        .iter()
        .map(|&player| scenario.add_artifact_from_oracle(player, "Relic", "").id())
        .collect();
    (scenario.build(), gut, attacker, fodder, artifacts)
}

fn declare_attack(runner: &mut GameRunner, attacker: ObjectId) {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("attacker declared");
}

fn decide_gut(runner: &mut GameRunner, gut: ObjectId, accept: bool) {
    // CR 508.3d: another creature can declare the attack that triggers Gut.
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!(player, P0);
                assert_eq!(source_id, gut, "decision must be Gut's own trigger");
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("Gut's optional sacrifice decision accepted");
                return;
            }
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passed");
            }
            other => panic!("Gut trigger did not reach optional decision: {other:?}"),
        }
    }
    panic!("Gut trigger did not reach optional decision");
}

fn choice_cards(runner: &mut GameRunner) -> Vec<ObjectId> {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice {
                player,
                cards,
                effect_kind: EffectKind::Sacrifice,
                ..
            } => {
                assert_eq!(player, P0);
                return cards;
            }
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passed");
            }
            other => panic!("Gut never reached a sacrifice choice: {other:?}"),
        }
    }
    panic!("Gut sacrifice choice did not surface");
}

fn settle(runner: &mut GameRunner) {
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("resolve pending trigger");
            }
            other => panic!("Gut resolution did not settle: {other:?}"),
        }
    }
    panic!("Gut trigger did not finish resolving");
}

fn skeletons(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &runner.state().objects[id];
            object.is_token && object.name == "Skeleton" && object.controller == P0
        })
        .collect()
}

fn assert_printed_skeleton(runner: &GameRunner) {
    let tokens = skeletons(runner);
    assert_eq!(tokens.len(), 1, "the payment creates exactly one Skeleton");
    let token = &runner.state().objects[&tokens[0]];
    assert_eq!((token.power, token.toughness), (Some(4), Some(1)));
    assert!(token.color.contains(&ManaColor::Black));
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(token.card_types.subtypes.iter().any(|s| s == "Skeleton"));
    assert!(token.has_keyword(&Keyword::Menace));
    // CR 508.4: the created creature enters tapped and attacking without
    // itself being declared as an attacker.
    assert!(token.tapped);
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat remains active");
    let attack = combat
        .attackers
        .iter()
        .find(|attack| attack.object_id == tokens[0])
        .expect("Skeleton entered attacking");
    assert_eq!(attack.defending_player, P1);
}

#[test]
fn gut_attack_sacrifice_offers_noncreature_artifact() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0, P1]);
    let artifact = artifacts[0];
    let opponent_artifact = artifacts[1];
    assert!(
        !runner.state().objects[&artifact]
            .card_types
            .core_types
            .contains(&CoreType::Creature),
        "the artifact fixture must not qualify for the creature leg"
    );

    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, true);
    let cards = choice_cards(&mut runner);
    // CR 701.21a: a controller can sacrifice their noncreature artifact.
    assert!(cards.contains(&attacker), "creature leg must be live");
    assert!(cards.contains(&fodder), "second creature forces a choice");
    assert!(
        cards.contains(&artifact),
        "artifact leg must be live: {cards:?}"
    );
    assert!(
        !cards.contains(&opponent_artifact),
        "an opponent's artifact cannot be sacrificed: {cards:?}"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![artifact],
        })
        .expect("selecting artifact is legal");
    settle(&mut runner);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_printed_skeleton(&runner);
}

#[test]
fn gut_can_sacrifice_the_other_creature_instead() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0]);
    let artifact = artifacts[0];
    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, true);
    let cards = choice_cards(&mut runner);
    assert!(cards.contains(&fodder) && cards.contains(&artifact));
    assert!(
        !cards.contains(&gut),
        "ordinary Gut is not another creature"
    );
    runner
        .act(GameAction::SelectCards {
            cards: vec![fodder],
        })
        .expect("select another creature");
    settle(&mut runner);
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Graveyard);
    assert_eq!(runner.state().objects[&artifact].zone, Zone::Battlefield);
    assert_printed_skeleton(&runner);
}

#[test]
fn declining_gut_sacrifice_skips_its_token() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0]);
    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, false);
    settle(&mut runner);
    assert!(skeletons(&runner).is_empty(), "declining produces no token");
    assert_eq!(runner.state().objects[&fodder].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&artifacts[0]].zone,
        Zone::Battlefield
    );
}

#[test]
fn gut_without_eligible_payment_cannot_create_a_skeleton() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gut = scenario
        .add_creature_from_oracle(P0, "Gut, True Soul Zealot", 2, 2, GUT)
        .id();
    let mut runner = scenario.build();
    declare_attack(&mut runner, gut);
    decide_gut(&mut runner, gut, true);
    settle(&mut runner);
    assert!(skeletons(&runner).is_empty(), "no payment means no token");
    assert_eq!(runner.state().objects[&gut].zone, Zone::Battlefield);
}

#[test]
fn artifact_creature_gut_can_pay_with_itself_and_keep_its_trigger() {
    let (mut runner, gut, attacker, fodder, artifacts) = board(&[P0]);
    {
        let object = runner
            .state_mut()
            .objects
            .get_mut(&gut)
            .expect("Gut exists");
        object.card_types.core_types.push(CoreType::Artifact);
        object.base_card_types = object.card_types.clone();
    }
    declare_attack(&mut runner, attacker);
    decide_gut(&mut runner, gut, true);
    let cards = choice_cards(&mut runner);
    assert!(
        cards.contains(&gut),
        "Gut qualifies on unrestricted artifact leg"
    );
    assert!(
        cards.contains(&fodder),
        "other creature forces an explicit choice"
    );
    assert!(
        cards.contains(&artifacts[0]),
        "other artifact stays eligible"
    );
    runner
        .act(GameAction::SelectCards { cards: vec![gut] })
        .expect("Gut can sacrifice itself as an artifact");
    settle(&mut runner);
    // CR 113.7a: removing the source does not remove its triggered ability.
    assert_eq!(runner.state().objects[&gut].zone, Zone::Graveyard);
    assert_printed_skeleton(&runner);
}

#[test]
fn another_optional_attack_trigger_does_not_pay_guts_if_you_do() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gut = scenario
        .add_creature_from_oracle(P0, "Gut, True Soul Zealot", 2, 2, GUT)
        .id();
    let other = scenario
        .add_creature_from_oracle(
            P0,
            "Synthetic Optional Attack",
            1,
            1,
            "Whenever you attack, you may gain 1 life.",
        )
        .id();
    let attacker = scenario.add_creature(P0, "Ally", 2, 2).id();
    scenario.add_artifact_from_oracle(P0, "Relic", "");
    let mut runner = scenario.build();
    let life_before = runner.state().players[P0.0 as usize].life;
    declare_attack(&mut runner, attacker);
    let mut gut_decided = false;
    let mut sibling_decided = false;
    for _ in 0..40 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OrderTriggers { .. } => {
                engine::game::triggers::drain_order_triggers_with_identity(runner.state_mut());
            }
            WaitingFor::OptionalEffectChoice { source_id, .. } => {
                let accept = if source_id == gut {
                    gut_decided = true;
                    false
                } else {
                    assert_eq!(source_id, other);
                    sibling_decided = true;
                    true
                };
                runner
                    .act(GameAction::DecideOptionalEffect { accept })
                    .expect("optional decision accepted");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => break,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passed");
            }
            other_wait => panic!("unexpected optional attack trigger wait: {other_wait:?}"),
        }
    }
    assert!(
        gut_decided && sibling_decided,
        "both triggers must reach resolution"
    );
    assert_eq!(runner.state().players[P0.0 as usize].life, life_before + 1);
    assert!(
        skeletons(&runner).is_empty(),
        "Gut's decline gates only its token"
    );
}

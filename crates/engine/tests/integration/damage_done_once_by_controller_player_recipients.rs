//! Production coverage for one-or-more controller-batched damage triggers that
//! bind to each damaged player independently.

use engine::game::combat::AttackTarget;
use engine::game::functioning_abilities::active_trigger_definitions;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    ControllerRef, DamageKindFilter, FilterProp, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);

fn count_treasures(runner: &GameRunner) -> usize {
    runner
        .state()
        .battlefield
        .iter()
        .filter_map(|id| runner.state().objects.get(id))
        .filter(|object| {
            object.controller == P0
                && object.zone == Zone::Battlefield
                && object.is_token
                && object.name.eq_ignore_ascii_case("Treasure")
        })
        .count()
}

fn plus_one_counters(runner: &GameRunner, object_id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&object_id)
        .and_then(|object| object.counters.get(&CounterType::Plus1Plus1).copied())
        .unwrap_or(0)
}

fn drive_to_declare_attackers(runner: &mut GameRunner) {
    for _ in 0..64 {
        match runner.state().waiting_for {
            WaitingFor::DeclareAttackers { .. } => return,
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("passing priority should reach declare attackers");
            }
            ref other => panic!("expected priority or declare attackers, got {other:?}"),
        }
    }
    panic!("did not reach declare attackers");
}

fn drive_until_source_triggers_are_stacked(
    runner: &mut GameRunner,
    source: ObjectId,
    expected_count: usize,
) {
    for _ in 0..128 {
        let count = runner
            .state()
            .stack
            .iter()
            .filter(|entry| entry.source_id == source)
            .count();
        if count == expected_count {
            return;
        }

        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } | WaitingFor::DeclareBlockers { .. } => {
                step_unblocked_combat(runner);
            }
            WaitingFor::OrderTriggers { triggers, .. } => {
                runner
                    .act(GameAction::OrderTriggers {
                        order: (0..triggers.len()).collect(),
                    })
                    .expect("trigger order should be accepted");
            }
            other => panic!("unexpected wait state while stacking triggers: {other:?}"),
        }
    }
    panic!("source triggers did not reach the stack");
}

#[test]
fn professional_face_breaker_creates_one_treasure_for_each_combat_damage_player() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let face_breaker =
        scenario.add_real_card(P0, "Professional Face-Breaker", Zone::Battlefield, db);
    let attacker_p1 = scenario.add_creature(P0, "Attacker P1", 1, 1).id();
    let attacker_p2 = scenario.add_creature(P0, "Attacker P2", 1, 1).id();
    let attacker_p3 = scenario.add_creature(P0, "Attacker P3", 1, 1).id();
    let mut runner = scenario.build();
    let treasures_before = count_treasures(&runner);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (attacker_p1, AttackTarget::Player(P1)),
            (attacker_p2, AttackTarget::Player(P2)),
            (attacker_p3, AttackTarget::Player(P3)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, face_breaker, 3);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 19, "P1 took combat damage");
    assert_eq!(runner.life(P2), 19, "P2 took combat damage");
    assert_eq!(runner.life(P3), 19, "P3 took combat damage");
    assert_eq!(
        count_treasures(&runner),
        treasures_before + 3,
        "Professional Face-Breaker must fire once for every damaged player"
    );
}

#[test]
fn professional_face_breaker_batches_multiple_matching_sources_for_one_player() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let face_breaker =
        scenario.add_real_card(P0, "Professional Face-Breaker", Zone::Battlefield, db);
    let first = scenario.add_creature(P0, "First attacker", 1, 1).id();
    let second = scenario.add_creature(P0, "Second attacker", 1, 1).id();
    let mut runner = scenario.build();
    let treasures_before = count_treasures(&runner);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P1)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, face_breaker, 1);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 18, "both attackers dealt combat damage");
    assert_eq!(
        count_treasures(&runner),
        treasures_before + 1,
        "two matching creatures damaging one player remain one occurrence"
    );
}

#[test]
fn malcolm_still_creates_treasure_per_distinct_damaged_opponent() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let malcolm = scenario.add_real_card(P0, "Malcolm, Keen-Eyed Navigator", Zone::Battlefield, db);
    let pirate_p1 = scenario
        .add_creature(P0, "Pirate P1", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let pirate_p2_a = scenario
        .add_creature(P0, "Pirate P2 A", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let pirate_p2_b = scenario
        .add_creature(P0, "Pirate P2 B", 1, 1)
        .with_subtypes(vec!["Pirate"])
        .id();
    let mut runner = scenario.build();
    let treasures_before = count_treasures(&runner);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (pirate_p1, AttackTarget::Player(P1)),
            (pirate_p2_a, AttackTarget::Player(P2)),
            (pirate_p2_b, AttackTarget::Player(P2)),
        ])
        .expect("Pirates should be legal attackers");
    drive_until_source_triggers_are_stacked(&mut runner, malcolm, 1);

    runner.advance_until_stack_empty();
    assert_eq!(
        count_treasures(&runner),
        treasures_before + 2,
        "Malcolm's nonexact opponent scope must retain its aggregate behavior"
    );
}

#[test]
fn francisco_explores_once_for_each_noncombat_player_damage_recipient() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let francisco = scenario.add_real_card(P0, "Francisco, Fowl Marauder", Zone::Battlefield, db);
    let crew = scenario.add_real_card(P0, "Lightning-Rig Crew", Zone::Battlefield, db);
    for _ in 0..3 {
        scenario.add_real_card(P0, "Forest", Zone::Library, db);
    }
    let mut runner = scenario.build();
    let hand_before = runner.state().players[0].hand.len();

    runner
        .act(GameAction::ActivateAbility {
            source_id: crew,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("Lightning-Rig Crew activation should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, francisco, 3);

    runner.advance_until_stack_empty();
    assert_eq!(runner.life(P1), 19, "Crew damaged P1");
    assert_eq!(runner.life(P2), 19, "Crew damaged P2");
    assert_eq!(runner.life(P3), 19, "Crew damaged P3");
    assert_eq!(
        runner.state().players[0].hand.len(),
        hand_before + 3,
        "each Francisco trigger must resolve its own explore"
    );
    assert_eq!(
        plus_one_counters(&runner, francisco),
        0,
        "the three real Forest explores use the land branch rather than a synthetic counter"
    );
}

#[test]
fn the_thing_keeps_only_hero_damage_in_its_stored_event_context() {
    let db = shared_card_db().expect("integration card fixture must load");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let thing = scenario.add_real_card(P0, "The Thing, Ben Grimm", Zone::Battlefield, db);
    let hero = scenario
        .add_creature(P0, "Hero attacker", 1, 1)
        .with_subtypes(vec!["Hero"])
        .id();
    let non_hero = scenario.add_creature(P0, "Non-Hero attacker", 1, 1).id();
    let mut runner = scenario.build();

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (hero, AttackTarget::Player(P1)),
            (non_hero, AttackTarget::Player(P1)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, thing, 1);

    let entry = runner
        .state()
        .stack
        .iter()
        .find(|entry| entry.source_id == thing)
        .expect("The Thing trigger should be on the production stack");
    let StackEntryKind::TriggeredAbility {
        trigger_event:
            Some(GameEvent::CombatDamageDealtToPlayer {
                player_id,
                source_amounts,
                total_damage,
                ..
            }),
        ..
    } = &entry.kind
    else {
        panic!("The Thing must retain a normalized combat-damage event, got {entry:?}");
    };
    assert_eq!(*player_id, P1);
    assert_eq!(source_amounts, &vec![(hero, 1)]);
    assert_eq!(*total_damage, 1);

    runner.advance_until_stack_empty();
    assert_eq!(
        plus_one_counters(&runner, thing),
        2,
        "The Thing resolves once from the Hero-only context"
    );
}

const QUARTZWOOD_ORACLE: &str = "Trample\nWhenever one or more creatures you control with trample deal combat damage to a player, create an X/X green Dinosaur Beast creature token with trample, where X is the amount of damage those creatures dealt to that player.";

fn add_quartzwood(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_creature_from_oracle(P0, "Quartzwood Crasher", 6, 6, QUARTZWOOD_ORACLE)
        .id()
}

fn quartzwood_tokens(runner: &GameRunner) -> Vec<ObjectId> {
    runner
        .state()
        .battlefield
        .iter()
        .copied()
        .filter(|&id| {
            let object = &runner.state().objects[&id];
            object.is_token
                && object.card_types.subtypes.iter().any(|s| s == "Dinosaur")
                && object.card_types.subtypes.iter().any(|s| s == "Beast")
        })
        .collect()
}

fn assert_quartzwood_token(runner: &GameRunner, token: ObjectId, damage: i32) {
    let token = &runner.state().objects[&token];
    assert_eq!(token.controller, P0);
    assert_eq!(token.power, Some(damage));
    assert_eq!(token.toughness, Some(damage));
    assert_eq!(token.color, [ManaColor::Green]);
    assert!(token.card_types.core_types.contains(&CoreType::Creature));
    assert!(token.keywords.contains(&Keyword::Trample));
}

fn combat_damage_event(event: Option<&GameEvent>) -> (PlayerId, u32, &[(ObjectId, u32)]) {
    let Some(GameEvent::CombatDamageDealtToPlayer {
        player_id,
        total_damage,
        source_amounts,
        ..
    }) = event
    else {
        panic!("expected stored player combat damage event, got {event:?}");
    };
    (*player_id, *total_damage, source_amounts)
}

fn step_unblocked_combat(runner: &mut GameRunner) {
    let action = match &runner.state().waiting_for {
        WaitingFor::Priority { .. } => GameAction::PassPriority,
        WaitingFor::DeclareBlockers { .. } => GameAction::DeclareBlockers {
            assignments: vec![],
        },
        other => panic!("unexpected combat prompt: {other:?}"),
    };
    runner.act(action).expect("combat should advance");
}

#[test]
fn quartzwood_uses_only_matching_sources_in_its_token_stats() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quartzwood = add_quartzwood(&mut scenario);
    let trampler = scenario
        .add_creature_from_oracle(P0, "Argothian Swine", 3, 3, "Trample")
        .id();
    let excluded = scenario.add_creature(P0, "Nontrampler", 4, 4).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (quartzwood, AttackTarget::Player(P1)),
            (trampler, AttackTarget::Player(P1)),
            (excluded, AttackTarget::Player(P1)),
        ])
        .expect("attackers should be legal");
    drive_until_source_triggers_are_stacked(&mut runner, quartzwood, 1);

    assert_eq!(runner.life(P1), life_before - 13);
    assert_eq!(runner.state().stack.len(), 1);
    let entry = &runner.state().stack[0];
    assert_eq!(entry.source_id, quartzwood);
    let StackEntryKind::TriggeredAbility { trigger_event, .. } = &entry.kind else {
        panic!("expected Quartzwood trigger");
    };
    let (recipient, total, sources) = combat_damage_event(trigger_event.as_ref());
    assert_eq!((recipient, total), (P1, 9));
    assert_eq!(sources.len(), 2);
    assert!(sources.contains(&(quartzwood, 6)));
    assert!(sources.contains(&(trampler, 3)));

    runner.resolve_top();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    let [token] = quartzwood_tokens(&runner)
        .try_into()
        .expect("Quartzwood must create exactly one Dinosaur Beast token");
    assert_quartzwood_token(&runner, token, 9);
}

#[test]
fn quartzwood_resolves_each_damaged_players_total_in_chosen_order() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let quartzwood = add_quartzwood(&mut scenario);
    let first = scenario
        .add_creature_from_oracle(P0, "Argothian Swine", 3, 3, "Trample")
        .id();
    let second = scenario
        .add_creature_from_oracle(P0, "Satyr Rambler", 2, 1, "Trample")
        .id();
    let excluded = scenario.add_creature(P0, "Nontrampler", 3, 3).id();
    let mut runner = scenario.build();
    let life_before = (runner.life(P1), runner.life(P2));

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[
            (quartzwood, AttackTarget::Player(P1)),
            (first, AttackTarget::Player(P1)),
            (second, AttackTarget::Player(P2)),
            (excluded, AttackTarget::Player(P1)),
        ])
        .expect("attackers should be legal");
    for _ in 0..128 {
        if matches!(runner.state().waiting_for, WaitingFor::OrderTriggers { .. }) {
            break;
        }
        step_unblocked_combat(&mut runner);
    }

    assert_eq!(
        (runner.life(P1), runner.life(P2)),
        (life_before.0 - 12, life_before.1 - 2)
    );
    let WaitingFor::OrderTriggers { player, triggers } = &runner.state().waiting_for else {
        panic!("expected Quartzwood trigger-ordering prompt");
    };
    assert_eq!(*player, P0);
    assert_eq!(triggers.len(), 2);
    assert!(triggers
        .iter()
        .all(|trigger| trigger.source_id == quartzwood));

    let pending = runner
        .state()
        .pending_trigger_order
        .as_ref()
        .expect("pending order");
    let [group] = pending.groups.as_slice() else {
        panic!("expected one ordering group");
    };
    assert_eq!(group.controller, P0);
    let pending_pairs: Vec<_> = group
        .triggers
        .iter()
        .map(|trigger| {
            let (player, total, _) = combat_damage_event(trigger.pending.trigger_event.as_ref());
            (player, total)
        })
        .collect();
    assert!(
        pending_pairs == [(P1, 9), (P2, 2)] || pending_pairs == [(P2, 2), (P1, 9)],
        "unexpected occurrences: {pending_pairs:?}"
    );

    // CR 603.3b: The controller chooses the placement order.
    runner
        .act(GameAction::OrderTriggers { order: vec![1, 0] })
        .expect("reversed order should be legal");
    let stack_pairs: Vec<_> = runner
        .state()
        .stack
        .iter()
        .map(|entry| {
            assert_eq!(entry.source_id, quartzwood);
            let StackEntryKind::TriggeredAbility { trigger_event, .. } = &entry.kind else {
                panic!("expected Quartzwood trigger");
            };
            let (player, total, _) = combat_damage_event(trigger_event.as_ref());
            (player, total)
        })
        .collect();
    assert_eq!(stack_pairs, [pending_pairs[1], pending_pairs[0]]);

    assert!(quartzwood_tokens(&runner).is_empty());
    runner.resolve_top();
    assert_eq!(runner.state().stack.len(), 1);
    let [first_token] = quartzwood_tokens(&runner)
        .try_into()
        .expect("first firing must create one token");
    assert_quartzwood_token(&runner, first_token, pending_pairs[0].1 as i32);

    runner.resolve_top();
    let tokens = quartzwood_tokens(&runner);
    assert_eq!(tokens.len(), 2);
    assert!(tokens.contains(&first_token));
    let second_token = tokens
        .into_iter()
        .find(|&id| id != first_token)
        .expect("second new token");
    assert_quartzwood_token(&runner, second_token, pending_pairs[1].1 as i32);
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

#[test]
fn quartzwood_does_not_fire_for_an_empty_source_match() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let quartzwood = add_quartzwood(&mut scenario);
    let attacker = scenario.add_creature(P0, "Nontrampler", 4, 4).id();
    let mut runner = scenario.build();
    let life_before = runner.life(P1);

    let expected_source = Some(TargetFilter::Typed(
        TypedFilter::creature()
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::WithKeyword {
                value: Keyword::Trample,
            }]),
    ));
    let state = runner.state();
    assert!(
        active_trigger_definitions(state, &state.objects[&quartzwood]).any(|active| {
            let trigger = active.definition;
            trigger.mode == TriggerMode::DamageDoneOnceByController
                && trigger.batched
                && trigger.damage_kind == DamageKindFilter::CombatOnly
                && trigger.valid_source == expected_source
        }),
        "Quartzwood must have an active batched combat-damage trigger for creatures you control with trample"
    );

    drive_to_declare_attackers(&mut runner);
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("attacker should be legal");
    for _ in 0..128 {
        assert!(runner
            .state()
            .stack
            .iter()
            .all(|entry| entry.source_id != quartzwood));
        if runner.state().phase == Phase::EndCombat {
            break;
        }
        step_unblocked_combat(&mut runner);
    }

    assert_eq!(runner.life(P1), life_before - 4);
    assert_eq!(runner.state().phase, Phase::EndCombat);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(quartzwood_tokens(&runner).is_empty());
}

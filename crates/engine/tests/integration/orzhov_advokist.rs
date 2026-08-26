//! Orzhov Advokist: each player may accept two counters, and only an accepting
//! player's creatures are barred from attacking the trigger controller and that
//! controller's planeswalkers until the controller's next turn.

use engine::game::combat::{declare_attackers, AttackTarget};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::triggers::process_triggers;
use engine::game::turns::execute_untap;
use engine::game::zones::{create_object, move_to_zone};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{
    AbilityDefinition, ControllerRef, Duration, Effect, GameRestriction, PlayerFilter, PlayerScope,
    ProhibitedActivity, RestrictionExpiry, RestrictionPlayerScope, TargetFilter, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::triggers::AttackTargetFilter;
use engine::types::zones::Zone;

const ORZHOV_ADVOKIST_ORACLE: &str = "At the beginning of your upkeep, each player may put two +1/+1 counters on a creature they control. If a player does, creatures that player controls can't attack you or planeswalkers you control until your next turn.";
const PLANESWALKER_ONLY_ADVOKIST_ORACLE: &str = "At the beginning of your upkeep, each player may put two +1/+1 counters on a creature they control. If a player does, creatures that player controls can't attack planeswalkers you control until your next turn.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

fn find_attack_restriction(definition: &AbilityDefinition) -> Option<&AbilityDefinition> {
    if matches!(
        definition.effect.as_ref(),
        Effect::AddRestriction {
            restriction: GameRestriction::ProhibitActivity {
                activity: ProhibitedActivity::Attack { .. },
                ..
            },
        }
    ) {
        return Some(definition);
    }
    definition
        .sub_ability
        .as_deref()
        .and_then(find_attack_restriction)
}

#[test]
fn orzhov_advokist_parser_keeps_scoped_player_and_trigger_controller_distinct() {
    let parsed = parse_oracle_text(
        ORZHOV_ADVOKIST_ORACLE,
        "Orzhov Advokist",
        &[],
        &["Creature".to_string()],
        &["Human".to_string(), "Advisor".to_string()],
    );
    let trigger = parsed
        .triggers
        .iter()
        .find_map(|trigger| trigger.execute.as_deref())
        .expect("Orzhov Advokist has an upkeep trigger");

    assert!(trigger.optional, "each player may choose independently");
    assert_eq!(trigger.player_scope, Some(PlayerFilter::All));
    assert!(matches!(
        trigger.effect.as_ref(),
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            target: TargetFilter::Typed(TypedFilter {
                controller: Some(ControllerRef::ScopedPlayer),
                ..
            }),
            ..
        }
    ));

    let restriction = find_attack_restriction(trigger)
        .expect("the accepting-player rider must lower to an attack restriction");
    assert_eq!(
        restriction.duration,
        Some(Duration::UntilNextTurnOf {
            player: PlayerScope::Controller,
        })
    );
    assert!(matches!(
        restriction.effect.as_ref(),
        Effect::AddRestriction {
            restriction: GameRestriction::ProhibitActivity {
                source: ObjectId(0),
                affected_players: RestrictionPlayerScope::ScopedPlayer,
                expiry: RestrictionExpiry::EndOfTurn,
                activity: ProhibitedActivity::Attack {
                    defended: AttackTargetFilter::PlayerOrPlaneswalker,
                    protected_player: None,
                },
            },
        }
    ));
}

fn plus_counters(runner: &GameRunner, object: ObjectId) -> u32 {
    runner.state().objects[&object]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn add_creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
    let card = CardId(state.next_object_id);
    let object = create_object(state, card, controller, name.to_string(), Zone::Battlefield);
    let creature = state.objects.get_mut(&object).expect("created object");
    creature.card_types.core_types = vec![CoreType::Creature];
    creature.base_card_types = creature.card_types.clone();
    creature.power = Some(2);
    creature.toughness = Some(2);
    creature.base_power = Some(2);
    creature.base_toughness = Some(2);
    creature.summoning_sick = false;
    object
}

fn attack_is_legal(
    state: &GameState,
    controller: PlayerId,
    attacker: ObjectId,
    target: AttackTarget,
) -> bool {
    let mut state = state.clone();
    state.active_player = controller;
    let mut events = Vec::new();
    declare_attackers(&mut state, &[(attacker, target)], &mut events).is_ok()
}

/// Drive the real phase-trigger and optional-effect pipeline. P0 and P2 decline;
/// P1 accepts and selects their first creature for the two counters.
fn resolve_advokist_upkeep(
    runner: &mut GameRunner,
    source: ObjectId,
    p1_creature_a: ObjectId,
    p1_creature_b: ObjectId,
) {
    process_triggers(
        runner.state_mut(),
        &[GameEvent::PhaseChanged {
            phase: Phase::Upkeep,
        }],
    );

    let mut optional_players = Vec::new();
    for _ in 0..96 {
        match runner.state().waiting_for.clone() {
            WaitingFor::OptionalEffectChoice {
                player, source_id, ..
            } => {
                assert_eq!(source_id, source, "each offer comes from Advokist");
                optional_players.push(player);
                runner
                    .act(GameAction::DecideOptionalEffect {
                        accept: player == P1,
                    })
                    .expect("answering an Advokist optional offer succeeds");
            }
            WaitingFor::ChooseFromZoneChoice { player, cards, .. } => {
                assert_eq!(player, P1, "the accepting player chooses their creature");
                assert!(cards.contains(&p1_creature_a) && cards.contains(&p1_creature_b));
                runner
                    .act(GameAction::SelectCards {
                        cards: vec![p1_creature_a],
                    })
                    .expect("P1 can select their creature for the counters");
            }
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. } => {
                runner
                    .act(GameAction::ChooseTarget {
                        target: Some(engine::types::ability::TargetRef::Object(p1_creature_a)),
                    })
                    .expect("P1 can target their creature for the counters");
            }
            WaitingFor::Priority { .. } if runner.state().stack.is_empty() => {
                assert_eq!(optional_players, vec![P0, P1, P2]);
                return;
            }
            WaitingFor::Priority { .. } => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority progresses the upkeep trigger");
            }
            other => panic!("unexpected Advokist resolution state: {other:?}"),
        }
    }
    panic!("Advokist trigger did not finish within the resolution budget");
}

#[test]
fn orzhov_advokist_restriction_tracks_acceptance_controller_changes_and_expiry() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Upkeep);
    let advokist = scenario
        .add_creature_from_oracle(P0, "Orzhov Advokist", 1, 4, ORZHOV_ADVOKIST_ORACLE)
        .id();
    let _p0_creature = scenario.add_creature(P0, "P0 Bear", 2, 2).id();
    let p1_creature_a = scenario.add_creature(P1, "P1 Bear A", 2, 2).id();
    let p1_creature_b = scenario.add_creature(P1, "P1 Bear B", 2, 2).id();
    let p2_creature = scenario.add_creature(P2, "P2 Bear", 2, 2).id();
    let p0_planeswalker = scenario
        .add_creature(P0, "P0 Walker", 0, 0)
        .as_planeswalker_with_loyalty("Test", 4)
        .id();

    let mut runner = scenario.build();
    for object in [p1_creature_a, p1_creature_b, p2_creature] {
        runner
            .state_mut()
            .objects
            .get_mut(&object)
            .unwrap()
            .summoning_sick = false;
    }
    resolve_advokist_upkeep(&mut runner, advokist, p1_creature_a, p1_creature_b);

    assert_eq!(plus_counters(&runner, p1_creature_a), 2);
    assert_eq!(plus_counters(&runner, p1_creature_b), 0);
    assert_eq!(runner.state().restrictions.len(), 1);
    assert!(matches!(
        &runner.state().restrictions[0],
        GameRestriction::ProhibitActivity {
            source,
            affected_players: RestrictionPlayerScope::SpecificPlayer(P1),
            expiry: RestrictionExpiry::UntilPlayerNextTurn { player: P0 },
            activity: ProhibitedActivity::Attack {
                defended: AttackTargetFilter::PlayerOrPlaneswalker,
                protected_player: Some(P0),
            },
        } if *source == advokist
    ));

    // CR 508.1c / CR 508.1d: P1 alone is barred from attacking P0 and P0's
    // planeswalker; attacking P2 remains legal, as does P2 attacking P0.
    assert!(!attack_is_legal(
        runner.state(),
        P1,
        p1_creature_a,
        AttackTarget::Player(P0)
    ));
    assert!(!attack_is_legal(
        runner.state(),
        P1,
        p1_creature_a,
        AttackTarget::Planeswalker(p0_planeswalker)
    ));
    assert!(attack_is_legal(
        runner.state(),
        P1,
        p1_creature_a,
        AttackTarget::Player(P2)
    ));
    assert!(attack_is_legal(
        runner.state(),
        P2,
        p2_creature,
        AttackTarget::Player(P0)
    ));

    // CR 611.2c: later P1 creatures and a creature P2 gives to P1 are both
    // affected while the rule-modifying restriction lasts.
    let later_p1_creature = add_creature(runner.state_mut(), P1, "Later P1 Bear");
    assert!(!attack_is_legal(
        runner.state(),
        P1,
        later_p1_creature,
        AttackTarget::Player(P0)
    ));
    runner
        .state_mut()
        .objects
        .get_mut(&p2_creature)
        .unwrap()
        .controller = P1;
    assert!(!attack_is_legal(
        runner.state(),
        P1,
        p2_creature,
        AttackTarget::Player(P0)
    ));

    // The source changing controller and leaving cannot rewrite the trigger's
    // "you": the resolved restriction still protects P0.
    runner
        .state_mut()
        .objects
        .get_mut(&advokist)
        .unwrap()
        .controller = P2;
    let mut leave_events = Vec::new();
    move_to_zone(
        runner.state_mut(),
        advokist,
        Zone::Graveyard,
        &mut leave_events,
    );
    assert_eq!(runner.state().objects[&advokist].zone, Zone::Graveyard);
    assert!(!attack_is_legal(
        runner.state(),
        P1,
        p1_creature_a,
        AttackTarget::Player(P0)
    ));
    assert!(attack_is_legal(
        runner.state(),
        P1,
        p1_creature_a,
        AttackTarget::Player(P2)
    ));

    // CR 514.2: expiry occurs at P0's next turn, not P1's next turn.
    runner.state_mut().active_player = P0;
    runner.state_mut().turn_number += 3;
    let mut untap_events = Vec::new();
    execute_untap(runner.state_mut(), &mut untap_events);
    assert!(runner.state().restrictions.is_empty());
    assert!(attack_is_legal(
        runner.state(),
        P1,
        p1_creature_a,
        AttackTarget::Player(P0)
    ));
}

/// CR 608.2c + CR 611.2c: the scoped AddRestriction route snapshots the
/// accepting player, protected player, and controller-next-turn expiry when it
/// resolves. The planeswalker-only template makes player and unrelated-walker
/// attacks positive sibling cases rather than vacuous negatives.
#[test]
fn scoped_advokist_planeswalker_only_restriction_snapshots_controller_provenance() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::Upkeep);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Scoped Advokist Fixture",
            1,
            4,
            PLANESWALKER_ONLY_ADVOKIST_ORACLE,
        )
        .id();
    let p1_a = scenario.add_creature(P1, "P1 Bear A", 2, 2).id();
    let p1_b = scenario.add_creature(P1, "P1 Bear B", 2, 2).id();
    let p0_walker = scenario
        .add_creature(P0, "Protected Jace", 0, 0)
        .as_planeswalker_with_loyalty("Jace", 4)
        .id();
    let p2_walker = scenario
        .add_creature(P2, "Other Chandra", 0, 0)
        .as_planeswalker_with_loyalty("Chandra", 4)
        .id();
    let mut runner = scenario.build();
    for object in [p1_a, p1_b] {
        runner
            .state_mut()
            .objects
            .get_mut(&object)
            .unwrap()
            .summoning_sick = false;
    }
    resolve_advokist_upkeep(&mut runner, source, p1_a, p1_b);

    assert!(matches!(
        runner.state().restrictions.as_slice(),
        [GameRestriction::ProhibitActivity {
            source: stored_source,
            affected_players: RestrictionPlayerScope::SpecificPlayer(P1),
            expiry: RestrictionExpiry::UntilPlayerNextTurn { player: P0 },
            activity: ProhibitedActivity::Attack {
                defended: AttackTargetFilter::Planeswalker,
                protected_player: Some(P0),
            },
        }] if *stored_source == source
    ));
    assert!(!attack_is_legal(
        runner.state(),
        P1,
        p1_a,
        AttackTarget::Planeswalker(p0_walker)
    ));
    assert!(attack_is_legal(
        runner.state(),
        P1,
        p1_a,
        AttackTarget::Player(P0)
    ));
    assert!(attack_is_legal(
        runner.state(),
        P1,
        p1_a,
        AttackTarget::Planeswalker(p2_walker)
    ));

    runner
        .state_mut()
        .objects
        .get_mut(&source)
        .unwrap()
        .controller = P2;
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), source, Zone::Graveyard, &mut events);
    assert!(
        !attack_is_legal(
            runner.state(),
            P1,
            p1_a,
            AttackTarget::Planeswalker(p0_walker)
        ),
        "changing or removing the source cannot mutate the resolved snapshot"
    );
}

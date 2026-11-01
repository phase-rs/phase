//! Source-scoped one-shot replacements cover every recipient of one combat
//! damage instance, but not a later combat-damage step (CR 510.2 + CR 700.1).

use engine::game::combat::AttackTarget;
use engine::game::effects::create_damage_replacement;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ControllerRef, DamageModification, Effect, PreventionAmount, RedirectionLifetime,
    ResolvedAbility, ShieldKind, TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    ChosenDamageSource, CombatDamageAssignmentMode, GameRng, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

// Verbatim Oracle text from data/mtgjson/AtomicCards.json.
const IMPULSIVE_MANEUVERS: &str = "Whenever a creature attacks, flip a coin. If you win the flip, the next time that creature would deal combat damage this turn, it deals double that damage instead. If you lose the flip, the next time that creature would deal combat damage this turn, prevent that damage.";
const SHIELDMATES_BLESSING: &str =
    "Prevent the next 3 damage that would be dealt to any target this turn.";
const GOBLIN_PSYCHOPATH: &str = "Whenever this creature attacks or blocks, flip a coin. If you lose the flip, the next time it would deal combat damage this turn, it deals that damage to you instead.";

fn add_maneuvers(scenario: &mut GameScenario, controller: PlayerId) {
    scenario.add_enchantment_from_oracle(controller, "Impulsive Maneuvers", IMPULSIVE_MANEUVERS);
}

// These small action sequences retain every event and fail on unexpected prompts;
// the shared rules::run_combat intentionally discards the events needed here.
pub(super) fn declare_attacks(runner: &mut GameRunner, attackers: &[ObjectId]) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers {
                player,
                valid_attacker_ids,
                ..
            } => {
                assert_eq!(*player, P0);
                assert!(attackers.iter().all(|id| valid_attacker_ids.contains(id)));
                let attacks: Vec<_> = attackers
                    .iter()
                    .map(|&id| (id, AttackTarget::Player(P1)))
                    .collect();
                events.extend(
                    runner
                        .declare_attackers(&attacks)
                        .expect("declare the attackers")
                        .events,
                );
                return events;
            }
            WaitingFor::Priority { .. } => {
                assert!(runner.state().stack.is_empty());
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("advance to attack declaration")
                        .events,
                );
            }
            other => panic!("expected priority or attack declaration, got {other:?}"),
        }
    }
    panic!("attack declaration was never offered");
}

fn resolve_one_attack_flip(
    runner: &mut GameRunner,
    controller: PlayerId,
    won: bool,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..32 {
        let action = match &runner.state().waiting_for {
            WaitingFor::OrderTriggers { player, triggers } => {
                assert_eq!(*player, controller);
                assert!(
                    triggers.len() >= 2,
                    "ordering must offer the real attack triggers"
                );
                GameAction::OrderTriggers {
                    order: (0..triggers.len()).collect(),
                }
            }
            WaitingFor::Priority { .. } => {
                assert!(
                    !runner.state().stack.is_empty(),
                    "the attack trigger must still be on the stack"
                );
                // Pin immediately before resolution, independently for each trigger.
                runner.state_mut().rng = GameRng::seed_from_u64(if won { 0 } else { 1 });
                GameAction::PassPriority
            }
            other => panic!("unexpected attack-trigger prompt: {other:?}"),
        };
        let result = runner
            .act(action)
            .expect("resolve the actual attack trigger");
        let flips: Vec<_> = result
            .events
            .iter()
            .filter_map(|event| match event {
                GameEvent::CoinFlipped { player_id, won } => Some((*player_id, *won)),
                _ => None,
            })
            .collect();
        events.extend(result.events);
        if !flips.is_empty() {
            assert_eq!(
                flips,
                vec![(controller, won)],
                "observe the pinned flip and its actual actor"
            );
            assert!(matches!(
                runner.state().waiting_for,
                WaitingFor::Priority { .. }
            ));
            return events;
        }
    }
    panic!("the attack trigger never flipped its coin");
}

fn assert_attack_shields(runner: &GameRunner, attackers: &[ObjectId]) {
    assert!(
        runner.state().stack.is_empty(),
        "all attack triggers must have resolved"
    );
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for attacker in attackers {
        assert!(
            runner.state().objects[attacker]
                .replacement_definitions
                .as_slice()
                .iter()
                .any(|shield| shield.consume_on_apply && !shield.is_consumed),
            "the real trigger must install a live replacement on attacker {attacker:?}"
        );
    }
}

pub(super) fn reach_combat_damage(
    runner: &mut GameRunner,
    blocks: &[(ObjectId, ObjectId)],
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    let mut declared = false;
    for _ in 0..32 {
        if runner.state().phase == Phase::CombatDamage {
            assert!(
                declared || blocks.is_empty(),
                "the requested blockers must actually be declared"
            );
            return events;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                assert!(
                    runner.state().stack.is_empty(),
                    "resolve spells and attack triggers before blocking"
                );
                events.extend(
                    runner
                        .act(GameAction::PassPriority)
                        .expect("advance to combat damage")
                        .events,
                );
            }
            WaitingFor::DeclareBlockers { player, .. } => {
                assert_eq!(*player, P1);
                assert!(!declared, "block declaration must occur only once");
                declared = true;
                events.extend(
                    runner
                        .declare_blockers(blocks)
                        .expect("declare the requested blockers")
                        .events,
                );
            }
            other => panic!("unexpected prompt before damage: {other:?}"),
        }
    }
    panic!("combat never reached the damage step");
}

pub(super) fn assign_trample(
    runner: &mut GameRunner,
    attacker: ObjectId,
    assignments: &[(ObjectId, u32)],
    trample_damage: u32,
) -> Vec<GameEvent> {
    let WaitingFor::AssignCombatDamage {
        player,
        attacker_id,
        total_damage,
        blockers,
        trample,
        defending_player,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected explicit combat assignment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*attacker_id, attacker);
    assert_eq!(*defending_player, P1);
    assert!(trample.is_some());
    assert_eq!(
        *total_damage,
        assignments.iter().map(|(_, amount)| *amount).sum::<u32>() + trample_damage
    );
    assert_eq!(blockers.len(), assignments.len());
    for (blocker, amount) in assignments {
        let slot = blockers
            .iter()
            .find(|slot| slot.blocker_id == *blocker)
            .expect("each requested recipient must be in the assignment prompt");
        // CR 702.19b: assign lethal before trample; replacement effects do not
        // change the amount that must be assigned to a blocker.
        assert!(*amount >= slot.lethal_minimum);
    }
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: assignments.to_vec(),
            trample_damage,
            controller_damage: 0,
        })
        .expect("submit the explicit damage split")
        .events
}

fn assign_blocked_damage(
    runner: &mut GameRunner,
    attacker: ObjectId,
    assignments: &[(ObjectId, u32)],
) -> Vec<GameEvent> {
    let WaitingFor::AssignCombatDamage {
        player,
        attacker_id,
        total_damage,
        blockers,
        trample,
        defending_player,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "expected multiple-blocker assignment, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P0);
    assert_eq!(*attacker_id, attacker);
    assert_eq!(*defending_player, P1);
    assert!(trample.is_none());
    assert_eq!(
        *total_damage,
        assignments.iter().map(|(_, amount)| *amount).sum::<u32>()
    );
    assert_eq!(blockers.len(), assignments.len());
    for (blocker, _) in assignments {
        assert!(blockers.iter().any(|slot| slot.blocker_id == *blocker));
    }
    runner
        .act(GameAction::AssignCombatDamage {
            mode: CombatDamageAssignmentMode::Normal,
            assignments: assignments.to_vec(),
            trample_damage: 0,
            controller_damage: 0,
        })
        .expect("submit the multiple-blocker damage split")
        .events
}

pub(super) fn assert_damage_complete(runner: &GameRunner) {
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .expect("combat must still be present")
            .regular_damage_done
    );
    assert!(runner.state().stack.is_empty());
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "damage must finish, not suspend at an unanswered prompt"
    );
    assert!(!runner.state().has_post_replacement_drain());
}

pub(super) fn damage_amounts(
    events: &[GameEvent],
    source: ObjectId,
    recipient: TargetRef,
) -> Vec<u32> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                is_combat: true,
                ..
            } if *source_id == source && *target == recipient => Some(*amount),
            _ => None,
        })
        .collect()
}

fn trample_case(won: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Trampler", 5, 5).trample().id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    add_maneuvers(&mut scenario, P0);
    let mut runner = scenario.build();
    let mut events = declare_attacks(&mut runner, &[attacker]);
    events.extend(resolve_one_attack_flip(&mut runner, P0, won));
    assert_attack_shields(&runner, &[attacker]);
    events.extend(reach_combat_damage(&mut runner, &[(blocker, attacker)]));
    events.extend(assign_trample(&mut runner, attacker, &[(blocker, 2)], 3));
    assert_damage_complete(&runner);
    // CR 510.2 + CR 700.1 + CR 614.1a: 2/3 is one simultaneous source
    // instance, so doubling must change both portions to 4/6.
    // CR 615.8: losing instead prevents the entire next instance, not just 2.
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        2,
        "reach guard: the blocker's independent combat damage must occur"
    );
    assert_eq!(
        damage_amounts(&events, blocker, TargetRef::Object(attacker)),
        vec![2]
    );
    assert_eq!(runner.life(P1), if won { 14 } else { 20 });
    if won {
        assert_eq!(
            damage_amounts(&events, attacker, TargetRef::Object(blocker)),
            vec![4]
        );
        assert_eq!(
            damage_amounts(&events, attacker, TargetRef::Player(P1)),
            vec![6]
        );
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Graveyard);
    } else {
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&blocker].damage_marked, 0);
        assert!(damage_amounts(&events, attacker, TargetRef::Object(blocker)).is_empty());
        assert!(damage_amounts(&events, attacker, TargetRef::Player(P1)).is_empty());
    }
}

#[test]
fn impulsive_maneuvers_win_doubles_every_recipient_of_trample_damage() {
    trample_case(true);
}

#[test]
fn impulsive_maneuvers_loss_prevents_every_recipient_of_trample_damage() {
    trample_case(false);
}

/// DamageReplacementOneShot engine mechanism: CR 614.5 gives the replacement
/// one opportunity per event; CR 510.2 + CR 700.1 make this source's assigned
/// portions one simultaneous instance. CR 614.1a: doubling replaces both portions.
#[test]
fn chosen_source_one_shot_damage_replacement_doubles_every_trample_recipient() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature(P0, "Chosen trampler", 5, 5)
        .trample()
        .id();
    let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
    let mut runner = scenario.build();
    runner.state_mut().last_chosen_damage_source = Some(ChosenDamageSource {
        source_id: attacker,
        source_filter: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::You)),
    });
    let ability = ResolvedAbility::new(
        Effect::CreateDamageReplacement {
            redirect_lifetime: RedirectionLifetime::OneOpportunity,
            source_filter: Some(TargetFilter::ChosenDamageSource { filter: None }),
            combat_scope: None,
            target_filter: None,
            modification: Some(DamageModification::Double),
            redirect_to: None,
            redirect_amount: None,
            redirect_object_filter: None,
            recipient_object_filter: None,
        },
        vec![],
        attacker,
        P0,
    );
    let mut events = Vec::new();
    create_damage_replacement::resolve(runner.state_mut(), &ability, &mut events)
        .expect("resolve the chosen-source one-shot damage replacement");
    runner.state_mut().last_chosen_damage_source = None;
    assert!(
        runner.state().objects[&attacker]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(
                |shield| shield.shield_kind == ShieldKind::DamageReplacementOneShot
                    && shield.damage_modification == Some(DamageModification::Double)
                    && !shield.is_consumed
            ),
        "the chosen attacker must host a live one-shot doubling shield before combat"
    );

    events.extend(declare_attacks(&mut runner, &[attacker]));
    events.extend(reach_combat_damage(&mut runner, &[(blocker, attacker)]));
    events.extend(assign_trample(&mut runner, attacker, &[(blocker, 2)], 3));
    assert_damage_complete(&runner);
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        2,
        "reach guard: the blocker's independent combat damage must occur"
    );
    assert_eq!(
        damage_amounts(&events, blocker, TargetRef::Object(attacker)),
        vec![2]
    );
    assert_eq!(runner.life(P1), 14);
    assert_eq!(
        damage_amounts(&events, attacker, TargetRef::Object(blocker)),
        vec![4]
    );
    assert_eq!(
        damage_amounts(&events, attacker, TargetRef::Player(P1)),
        vec![6]
    );
    assert_eq!(runner.state().objects[&blocker].zone, Zone::Graveyard);
}

fn multiple_blockers_case(won: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_maneuvers(&mut scenario, P0);
    let attacker = scenario.add_creature(P0, "Trampler", 5, 5).trample().id();
    // CR 702.12b: indestructible keeps lethal recipients on the battlefield so
    // marked damage can be observed without relying on dead-object damage.
    let first = scenario
        .add_creature(P1, "First blocker", 2, 2)
        .with_keyword(Keyword::Indestructible)
        .id();
    let second = scenario
        .add_creature(P1, "Second blocker", 2, 2)
        .with_keyword(Keyword::Indestructible)
        .id();
    let mut runner = scenario.build();
    let mut events = declare_attacks(&mut runner, &[attacker]);
    events.extend(resolve_one_attack_flip(&mut runner, P0, won));
    assert_attack_shields(&runner, &[attacker]);
    events.extend(reach_combat_damage(
        &mut runner,
        &[(first, attacker), (second, attacker)],
    ));
    events.extend(assign_trample(
        &mut runner,
        attacker,
        &[(first, 2), (second, 2)],
        1,
    ));
    assert_damage_complete(&runner);
    // CR 510.2 + CR 700.1 + CR 615.8: all three portions share one instance.
    assert_eq!(runner.state().objects[&attacker].damage_marked, 4);
    for blocker in [first, second] {
        assert_eq!(
            damage_amounts(&events, blocker, TargetRef::Object(attacker)),
            vec![2]
        );
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
        assert_eq!(
            runner.state().objects[&blocker].damage_marked,
            if won { 4 } else { 0 }
        );
        assert_eq!(
            damage_amounts(&events, attacker, TargetRef::Object(blocker)),
            if won { vec![4] } else { vec![] }
        );
    }
    assert_eq!(runner.life(P1), if won { 18 } else { 20 });
    assert_eq!(
        damage_amounts(&events, attacker, TargetRef::Player(P1)),
        if won { vec![2] } else { vec![] }
    );
}

#[test]
fn impulsive_maneuvers_win_doubles_both_blockers_and_trample() {
    multiple_blockers_case(true);
}

#[test]
fn impulsive_maneuvers_loss_prevents_both_blockers_and_trample() {
    multiple_blockers_case(false);
}

#[test]
fn goblin_psychopath_loss_redirects_every_blocked_portion_to_its_controller() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(P0, "Goblin Psychopath", 5, 5, GOBLIN_PSYCHOPATH)
        .id();
    let first = scenario.add_creature(P1, "First blocker", 2, 2).id();
    let second = scenario.add_creature(P1, "Second blocker", 2, 2).id();
    let mut runner = scenario.build();
    let mut events = declare_attacks(&mut runner, &[attacker]);
    events.extend(resolve_one_attack_flip(&mut runner, P0, false));
    assert!(runner.state().stack.is_empty());
    assert!(
        runner.state().objects[&attacker]
            .replacement_definitions
            .as_slice()
            .iter()
            .any(|shield| {
                !shield.is_consumed
                    && matches!(
                        shield.shield_kind,
                        ShieldKind::Redirection {
                            amount: PreventionAmount::All,
                            lifetime: RedirectionLifetime::OneOpportunity,
                            ..
                        }
                    )
            }),
        "the actual lost flip must install a live one-shot redirection shield"
    );
    events.extend(reach_combat_damage(
        &mut runner,
        &[(first, attacker), (second, attacker)],
    ));
    events.extend(assign_blocked_damage(
        &mut runner,
        attacker,
        &[(first, 2), (second, 3)],
    ));
    assert_damage_complete(&runner);
    // CR 510.2 + CR 700.1 + CR 614.9: both portions of the source's next
    // simultaneous combat damage instance go to its controller instead.
    assert_eq!(runner.life(P0), 15);
    assert_eq!(runner.life(P1), 20);
    let mut redirected = damage_amounts(&events, attacker, TargetRef::Player(P0));
    redirected.sort_unstable();
    assert_eq!(redirected, vec![2, 3]);
    assert!(damage_amounts(&events, attacker, TargetRef::Player(P1)).is_empty());
    assert_eq!(runner.state().objects[&attacker].zone, Zone::Battlefield);
    assert_eq!(
        runner.state().objects[&attacker].damage_marked,
        4,
        "reach guard: both blockers must still deal their independent combat damage"
    );
    for blocker in [first, second] {
        assert_eq!(
            damage_amounts(&events, blocker, TargetRef::Object(attacker)),
            vec![2]
        );
        assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
        assert_eq!(runner.state().objects[&blocker].damage_marked, 0);
        assert!(damage_amounts(&events, attacker, TargetRef::Object(blocker)).is_empty());
    }
}

fn unblocked_case(won: bool, double_strike: bool) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_maneuvers(&mut scenario, P0);
    let mut creature = scenario.add_creature(P0, "Unblocked attacker", 5, 5);
    if double_strike {
        creature.double_strike();
    }
    let attacker = creature.id();
    let mut runner = scenario.build();
    let mut events = declare_attacks(&mut runner, &[attacker]);
    events.extend(resolve_one_attack_flip(&mut runner, P0, won));
    assert_attack_shields(&runner, &[attacker]);
    events.extend(reach_combat_damage(&mut runner, &[]));
    assert_damage_complete(&runner);
    // CR 702.4b + CR 615.8 + CR 700.1: the second double-strike step is a
    // different instance. Its 5 damage must not reuse the first-step shield.
    let expected = match (won, double_strike) {
        (true, true) => vec![10, 5],
        (false, true) => vec![5],
        (true, false) => vec![10],
        (false, false) => vec![],
    };
    assert_eq!(
        damage_amounts(&events, attacker, TargetRef::Player(P1)),
        expected
    );
    assert_eq!(runner.life(P1), 20 - expected.iter().sum::<u32>() as i32);
    if !won {
        let prevented: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamagePrevented {
                    source_id, amount, ..
                } if *source_id == attacker => Some(*amount),
                _ => None,
            })
            .collect();
        assert_eq!(
            prevented,
            vec![5],
            "positive reach: the first actual damage instance was prevented"
        );
    }
    if double_strike {
        assert!(
            runner
                .state()
                .combat
                .as_ref()
                .expect("combat exists")
                .first_strike_done
        );
        // Ordered life events observe the intermediate total even when both
        // unblocked steps complete in the same public action.
        let mut life = 20;
        let totals: Vec<_> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } if *player_id == P1 => {
                    life += *amount;
                    Some(life)
                }
                _ => None,
            })
            .collect();
        assert_eq!(totals, if won { vec![10, 5] } else { vec![15] });
        if !won {
            // CR 615.6: prevented first-step damage produces no life-change
            // event. Its prevention must precede the later positive 5 damage,
            // distinguishing 20 -> 20 -> 15 from 20 -> 15 -> 15.
            let prevented = events
                .iter()
                .position(|event| {
                    matches!(event,
                GameEvent::DamagePrevented { source_id, amount: 5, .. }
                if *source_id == attacker)
                })
                .expect("first-strike damage must be prevented");
            let dealt = events
                .iter()
                .position(|event| {
                    matches!(event,
                GameEvent::DamageDealt {
                    source_id, target: TargetRef::Player(player), amount: 5,
                    is_combat: true, ..
                } if *source_id == attacker && *player == P1)
                })
                .expect("regular-step damage must actually be dealt");
            assert!(prevented < dealt);
        }
    }
}

#[test]
fn impulsive_maneuvers_win_doubles_single_unblocked_recipient() {
    unblocked_case(true, false);
}

#[test]
fn impulsive_maneuvers_loss_prevents_single_unblocked_recipient() {
    unblocked_case(false, false);
}

#[test]
fn impulsive_maneuvers_win_does_not_double_the_second_double_strike_step() {
    unblocked_case(true, true);
}

#[test]
fn impulsive_maneuvers_loss_does_not_prevent_the_second_double_strike_step() {
    unblocked_case(false, true);
}

#[test]
fn impulsive_maneuvers_first_striker_does_not_consume_an_ordinary_attackers_shield() {
    for won in [true, false] {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        add_maneuvers(&mut scenario, P0);
        let first = scenario
            .add_creature(P0, "First strike only", 2, 2)
            .first_strike()
            .id();
        let ordinary = scenario.add_creature(P0, "Ordinary attacker", 5, 5).id();
        let blocker = scenario.add_creature(P1, "Blocker", 2, 2).id();
        let mut runner = scenario.build();
        let mut events = declare_attacks(&mut runner, &[first, ordinary]);
        events.extend(resolve_one_attack_flip(&mut runner, P0, won));
        events.extend(resolve_one_attack_flip(&mut runner, P0, won));
        assert_attack_shields(&runner, &[first, ordinary]);
        events.extend(reach_combat_damage(&mut runner, &[(blocker, ordinary)]));
        assert_damage_complete(&runner);
        // CR 702.7b: only the first striker participates in the first batch;
        // the ordinary attacker's one recipient receives its still-live shield
        // during regular damage. Neither row depends on multi-recipient fixes.
        assert!(
            runner
                .state()
                .combat
                .as_ref()
                .expect("combat exists")
                .first_strike_done
        );
        assert_eq!(runner.state().objects[&ordinary].damage_marked, 2);
        assert_eq!(
            damage_amounts(&events, blocker, TargetRef::Object(ordinary)),
            vec![2]
        );
        assert_eq!(runner.life(P1), if won { 16 } else { 20 });
        assert_eq!(
            damage_amounts(&events, ordinary, TargetRef::Object(blocker)),
            if won { vec![10] } else { vec![] }
        );
        assert_eq!(
            damage_amounts(&events, first, TargetRef::Player(P1)),
            if won { vec![4] } else { vec![] },
            "a first-strike-only creature must not deal regular-step damage"
        );
        if won {
            assert_eq!(runner.state().objects[&blocker].zone, Zone::Graveyard);
        } else {
            assert_eq!(runner.state().objects[&blocker].zone, Zone::Battlefield);
            assert_eq!(runner.state().objects[&blocker].damage_marked, 0);
            for (source, expected) in [(first, 2), (ordinary, 5)] {
                assert!(events.iter().any(|event| matches!(event,
                    GameEvent::DamagePrevented { source_id, amount, .. }
                    if *source_id == source && *amount == expected)));
            }
        }
    }
}

#[test]
fn impulsive_maneuvers_opponent_controller_keeps_opposite_flips_bound_to_their_sources() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_maneuvers(&mut scenario, P1);
    let first = scenario.add_creature(P0, "Two power", 2, 2).id();
    let second = scenario.add_creature(P0, "Three power", 3, 3).id();
    let mut runner = scenario.build();
    let mut events = declare_attacks(&mut runner, &[first, second]);
    events.extend(resolve_one_attack_flip(&mut runner, P1, true));
    let winners: Vec<_> = [first, second]
        .into_iter()
        .filter(|id| {
            runner.state().objects[id]
                .replacement_definitions
                .as_slice()
                .iter()
                .any(|shield| shield.consume_on_apply && !shield.is_consumed)
        })
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "the first resolved trigger must bind exactly one attacker"
    );
    let winner = winners[0];
    let loser = if winner == first { second } else { first };
    events.extend(resolve_one_attack_flip(&mut runner, P1, false));
    assert_attack_shields(&runner, &[first, second]);
    events.extend(reach_combat_damage(&mut runner, &[]));
    assert_damage_complete(&runner);
    // CR 700.1 + CR 615.8: the other source's simultaneous damage is not part
    // of this source's next instance. The enchantment controller flips, not P0.
    let doubled = if winner == first { 4 } else { 6 };
    assert_eq!(
        damage_amounts(&events, winner, TargetRef::Player(P1)),
        vec![doubled]
    );
    assert!(damage_amounts(&events, loser, TargetRef::Player(P1)).is_empty());
    assert_eq!(runner.life(P1), 20 - doubled as i32);
    assert!(events.iter().any(|event| matches!(event,
        GameEvent::DamagePrevented { source_id, amount, .. }
        if *source_id == loser && *amount == if loser == first { 2 } else { 3 })));
}

#[test]
fn finite_prevention_shield_depletes_after_three_total_simultaneous_damage() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let first = scenario.add_creature(P0, "Two power", 2, 2).id();
    let second = scenario.add_creature(P0, "Three power", 3, 3).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Shieldmate's Blessing", true, SHIELDMATES_BLESSING)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::White],
            generic: 0,
        })
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(ManaType::White, spell, false, vec![])],
    );
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).target_player(P1).resolve();
    assert!(matches!(
        outcome.final_waiting_for(),
        WaitingFor::Priority { .. }
    ));
    assert!(outcome.state().stack.is_empty());
    assert_eq!(outcome.state().objects[&spell].zone, Zone::Graveyard);
    assert!(
        outcome
            .state()
            .pending_damage_replacements
            .iter()
            .any(|shield| {
                shield.shield_kind
                    == ShieldKind::Prevention {
                        amount: PreventionAmount::Next(3),
                    }
                    && !shield.is_consumed
            }),
        "the real Oracle text must install a live finite three-point shield"
    );
    let mut events = declare_attacks(&mut runner, &[first, second]);
    events.extend(reach_combat_damage(&mut runner, &[]));
    assert_damage_complete(&runner);
    // CR 615.7: a finite shield prevents only three points, even though both
    // sources deal damage simultaneously (CR 510.2). Depletion must stay final.
    let prevented: u32 = events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented { amount, .. } => Some(*amount),
            _ => None,
        })
        .sum();
    assert_eq!(prevented, 3);
    let dealt: u32 = [first, second]
        .into_iter()
        .flat_map(|source| damage_amounts(&events, source, TargetRef::Player(P1)))
        .sum();
    assert_eq!(
        dealt, 2,
        "positive reach: the rest of the simultaneous damage must be delivered"
    );
    assert_eq!(runner.life(P1), 18);
}

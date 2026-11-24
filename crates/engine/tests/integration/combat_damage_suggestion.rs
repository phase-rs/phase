//! CR 510.1c + CR 702.19b: `DerivedViews::combat_damage_suggestion` — the engine's lethal-first
//! default for an open `WaitingFor::AssignCombatDamage` prompt. Every row reaches the prompt
//! through real combat, pins the published split, submits it verbatim and checks where the
//! damage lands, so the suggestion and the validator cannot drift apart. The AI rows pin the
//! candidate list against hard-coded actions.

use engine::ai_support::candidate_actions;
use engine::game::combat::AttackTarget;
use engine::game::derived_views::{derive_views, ClientGameStateRef};
use engine::game::engine::apply;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{ChosenAttribute, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::{CombatDamageAssignmentMode, CombatDamageSplit, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db;

const P2: PlayerId = PlayerId(2);
const THRASTA: &str = "Thrasta, Tempest's Roar";

fn scenario(players: u8) -> GameScenario {
    let mut scenario = GameScenario::new_n_player(players, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

fn card(scenario: &mut GameScenario, player: PlayerId, name: &str, zone: Zone) -> ObjectId {
    scenario.add_real_card(player, name, zone, shared_card_db().expect("card database"))
}

fn permanent(scenario: &mut GameScenario, player: PlayerId, name: &str) -> ObjectId {
    card(scenario, player, name, Zone::Battlefield)
}

fn attack(runner: &mut GameRunner, attacks: &[(ObjectId, AttackTarget)]) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareAttackers { player, .. } => {
                assert_eq!(*player, P0);
                runner
                    .declare_attackers(attacks)
                    .expect("declare attackers");
                return;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("expected declare attackers, got {other:?}"),
        }
    }
    panic!("declare attackers was never reached");
}

fn block(runner: &mut GameRunner, blocks: &[(ObjectId, ObjectId)]) {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::DeclareBlockers { player, .. } => {
                assert_eq!(*player, P1);
                runner.declare_blockers(blocks).expect("declare blockers");
                return;
            }
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("expected declare blockers, got {other:?}"),
        }
    }
    panic!("declare blockers was never reached");
}

fn chooser(runner: &mut GameRunner, attacker: ObjectId) -> PlayerId {
    for _ in 0..32 {
        match &runner.state().waiting_for {
            WaitingFor::AssignCombatDamage {
                player,
                attacker_id,
                ..
            } => {
                assert_eq!(*attacker_id, attacker);
                return *player;
            }
            WaitingFor::DeclareBlockers { player, .. } if *player == P2 => {
                runner
                    .declare_blockers(&[])
                    .expect("P2 declares no blockers");
            }
            WaitingFor::Priority { .. } if runner.state().phase != Phase::CombatDamage => {
                runner
                    .act(GameAction::PassPriority)
                    .expect("pass to combat damage");
            }
            other => panic!("expected combat damage assignment, got {other:?}"),
        }
    }
    panic!("combat damage assignment was never reached");
}

fn damage_action(assignments: Vec<(ObjectId, u32)>, trample: u32, controller: u32) -> GameAction {
    GameAction::AssignCombatDamage {
        mode: CombatDamageAssignmentMode::Normal,
        assignments,
        trample_damage: trample,
        controller_damage: controller,
    }
}

fn suggested_action(split: CombatDamageSplit) -> GameAction {
    damage_action(
        split.assignments,
        split.trample_damage,
        split.controller_damage,
    )
}

fn candidates(runner: &GameRunner) -> Vec<GameAction> {
    candidate_actions(runner.state())
        .into_iter()
        .map(|candidate| candidate.action)
        .collect()
}

fn wire(runner: &GameRunner, viewer: PlayerId) -> serde_json::Value {
    serde_json::to_value(ClientGameStateRef::wrap(runner.state(), Some(viewer)))
        .expect("serialize viewer snapshot")
}

fn submit(
    runner: &mut GameRunner,
    player: PlayerId,
    attacker: ObjectId,
    expected: GameAction,
    expected_damage: &[(TargetRef, u32)],
) {
    let suggestion = derive_views(runner.state(), Some(player))
        .combat_damage_suggestion
        .expect("prompt offers a suggestion");
    let WaitingFor::AssignCombatDamage {
        player: prompt_player,
        total_damage,
        blockers,
        trample,
        ..
    } = &runner.state().waiting_for
    else {
        panic!("suggestion must accompany an assignment prompt");
    };
    assert_eq!(*prompt_player, player);
    let expected_total = if blockers.is_empty() && trample.is_none() {
        0
    } else {
        *total_damage
    };
    assert_eq!(
        suggestion
            .assignments
            .iter()
            .map(|(_, amount)| amount)
            .sum::<u32>()
            + suggestion.trample_damage
            + suggestion.controller_damage,
        expected_total,
    );
    let action = suggested_action(suggestion);
    assert_eq!(action, expected);

    let result = apply(runner.state_mut(), player, action).expect("validator accepts suggestion");
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::AssignCombatDamage { .. }
    ));
    let damage = result
        .events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                is_combat: true,
                ..
            } if *source_id == attacker => Some((target.clone(), *amount)),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(
        damage.len(),
        expected_damage.len(),
        "combat damage packets: {damage:?}"
    );
    for packet in expected_damage {
        assert!(damage.contains(packet), "missing {packet:?} in {damage:?}");
    }
}

fn cast_removal(runner: &mut GameRunner, spell: ObjectId, target: ObjectId, mana: &[ManaType]) {
    let player = runner
        .state_mut()
        .players
        .iter_mut()
        .find(|p| p.id == P0)
        .expect("P0 exists");
    for &color in mana {
        player
            .mana_pool
            .add(ManaUnit::new(color, ObjectId(0), false, vec![]));
    }
    let outcome = runner.cast(spell).target_object(target).resolve();
    assert_eq!(
        outcome.zone_of(target),
        Zone::Graveyard,
        "the removal resolved"
    );
    assert!(outcome.state().stack.is_empty());
}

fn assign_protector(runner: &mut GameRunner, battle: ObjectId) {
    // Seed the protector of a pre-existing Siege if scenario setup did not choose P1.
    let object = runner
        .state_mut()
        .objects
        .get_mut(&battle)
        .expect("Siege exists");
    if object.protector() != Some(P1) {
        object
            .chosen_attributes
            .retain(|attr| !matches!(attr, ChosenAttribute::Player(_)));
        object.chosen_attributes.push(ChosenAttribute::Player(P1));
    }
    assert_eq!(object.protector(), Some(P1));
    assert_eq!(object.zone, Zone::Battlefield);
}

// Row 1. Siege Behemoth also gives the trampler a second, ordered AI candidate.
#[test]
fn ordinary_trample_offers_lethal_first_on_the_wire() {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Stampeding Rhino");
    let behemoth = permanent(&mut scenario, P0, "Siege Behemoth");
    let a = permanent(&mut scenario, P1, "Llanowar Elves");
    let b = permanent(&mut scenario, P1, "Llanowar Elves");
    let mut runner = scenario.build();
    attack(
        &mut runner,
        &[
            (attacker, AttackTarget::Player(P1)),
            (behemoth, AttackTarget::Player(P1)),
        ],
    );
    block(&mut runner, &[(a, attacker), (b, attacker)]);
    let player = chooser(&mut runner, attacker);
    let expected = damage_action(vec![(a, 1), (b, 1)], 2, 0);
    assert_eq!(
        candidates(&runner),
        vec![
            expected.clone(),
            GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::AsThoughUnblocked,
                assignments: vec![],
                trample_damage: 0,
                controller_damage: 0,
            }
        ]
    );
    let snapshot = serde_json::json!({
        "assignments": [[a.0, 1], [b.0, 1]], "trample_damage": 2, "controller_damage": 0,
    });
    for viewer in [P0, P1] {
        assert_eq!(
            wire(&runner, viewer)["derived"]["combat_damage_suggestion"],
            snapshot
        );
    }
    submit(
        &mut runner,
        player,
        attacker,
        expected,
        &[
            (TargetRef::Object(a), 1),
            (TargetRef::Object(b), 1),
            (TargetRef::Player(P1), 2),
        ],
    );
}

// Row 2.
#[test]
fn insufficient_damage_stays_with_the_blockers() {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Argothian Swine");
    let a = permanent(&mut scenario, P1, "Wall of Wood");
    let b = permanent(&mut scenario, P1, "Wall of Wood");
    let mut runner = scenario.build();
    attack(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut runner, &[(a, attacker), (b, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    submit(
        &mut runner,
        P0,
        attacker,
        damage_action(vec![(a, 3), (b, 0)], 0, 0),
        &[(TargetRef::Object(a), 3)],
    );
}

// Row 3.
#[test]
fn deathtouch_makes_one_damage_lethal_per_blocker() {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Stampeding Rhino");
    permanent(&mut scenario, P0, "Ohran Frostfang");
    let a = permanent(&mut scenario, P1, "Wall of Wood");
    let b = permanent(&mut scenario, P1, "Wall of Wood");
    let mut runner = scenario.build();
    attack(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut runner, &[(a, attacker), (b, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    submit(
        &mut runner,
        P0,
        attacker,
        damage_action(vec![(a, 1), (b, 1)], 2, 0),
        &[
            (TargetRef::Object(a), 1),
            (TargetRef::Object(b), 1),
            (TargetRef::Player(P1), 2),
        ],
    );
}

// Row 4.
#[test]
fn earlier_assignment_can_cover_all_of_a_blockers_lethal_minimum() {
    let mut scenario = scenario(2);
    let earlier = permanent(&mut scenario, P0, "Stampeding Rhino");
    let attacker = permanent(&mut scenario, P0, "Stampeding Rhino");
    let brigade = permanent(&mut scenario, P1, "Foriysian Brigade");
    let mut runner = scenario.build();
    attack(
        &mut runner,
        &[
            (earlier, AttackTarget::Player(P1)),
            (attacker, AttackTarget::Player(P1)),
        ],
    );
    block(&mut runner, &[(brigade, earlier), (brigade, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    submit(
        &mut runner,
        P0,
        attacker,
        damage_action(vec![(brigade, 0)], 4, 0),
        &[(TargetRef::Player(P1), 4)],
    );
    assert_eq!(runner.state().objects[&brigade].zone, Zone::Graveyard);
}

// Rows 5 and 9.
fn no_trample(banding_blocker: &str, chooser_player: PlayerId) {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Siege Behemoth");
    let a = permanent(&mut scenario, P1, banding_blocker);
    let b = permanent(&mut scenario, P1, "Wall of Wood");
    let mut runner = scenario.build();
    attack(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut runner, &[(a, attacker), (b, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), chooser_player);
    let expected = damage_action(vec![(a, 3), (b, 4)], 0, 0);
    assert_eq!(
        candidates(&runner),
        vec![
            expected.clone(),
            GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::AsThoughUnblocked,
                assignments: vec![],
                trample_damage: 0,
                controller_damage: 0,
            }
        ]
    );
    submit(
        &mut runner,
        chooser_player,
        attacker,
        expected,
        &[(TargetRef::Object(a), 3), (TargetRef::Object(b), 4)],
    );
}

#[test]
fn no_trample_puts_remaining_damage_on_the_last_blocker() {
    no_trample("Wall of Wood", P0);
}

#[test]
fn defender_submits_the_suggestion_when_a_blocker_has_banding() {
    no_trample("Shield Bearer", P1);
}

enum PlaneswalkerCase {
    Live,
    LoyaltyCovered,
    Removed,
}

// Rows 6 and 7.
fn trample_over_planeswalker(case: PlaneswalkerCase) {
    let mut scenario = scenario(2);
    let earlier = matches!(case, PlaneswalkerCase::LoyaltyCovered)
        .then(|| permanent(&mut scenario, P0, "Hill Giant"));
    let attacker = permanent(&mut scenario, P0, THRASTA);
    let blocker = permanent(&mut scenario, P1, "Grizzly Bears");
    let jace = permanent(&mut scenario, P1, "Jace Beleren");
    let bolt = card(&mut scenario, P0, "Lightning Bolt", Zone::Hand);
    let mut runner = scenario.build();
    let target = AttackTarget::Planeswalker(jace);
    let attacks = earlier
        .into_iter()
        .chain([attacker])
        .map(|id| (id, target))
        .collect::<Vec<_>>();
    attack(&mut runner, &attacks);
    if matches!(case, PlaneswalkerCase::Removed) {
        cast_removal(&mut runner, bolt, jace, &[ManaType::Red]);
    }
    block(&mut runner, &[(blocker, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    let mut damage = vec![(TargetRef::Object(blocker), 2)];
    let (to_walker, to_controller) = match case {
        PlaneswalkerCase::Live => (3, 2),
        PlaneswalkerCase::LoyaltyCovered | PlaneswalkerCase::Removed => (0, 5),
    };
    if to_walker > 0 {
        damage.push((TargetRef::Object(jace), to_walker));
    }
    damage.push((TargetRef::Player(P1), to_controller));
    let expected = damage_action(vec![(blocker, 2)], to_walker, to_controller);
    assert_eq!(candidates(&runner), vec![expected.clone()]);
    submit(&mut runner, P0, attacker, expected, &damage);
    assert_eq!(runner.state().objects[&jace].zone, Zone::Graveyard);
}

#[test]
fn trample_over_planeswalker_assigns_loyalty_before_controller_damage() {
    trample_over_planeswalker(PlaneswalkerCase::Live);
}

#[test]
fn already_committed_damage_can_cover_all_loyalty() {
    trample_over_planeswalker(PlaneswalkerCase::LoyaltyCovered);
}

#[test]
fn trample_over_departed_planeswalker_reaches_original_defender() {
    trample_over_planeswalker(PlaneswalkerCase::Removed);
}

// Row 8.
fn ordinary_trample_after_removal(
    target_to_build: fn(ObjectId) -> AttackTarget,
    target_card: &str,
    owner: PlayerId,
) {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Stampeding Rhino");
    let a = permanent(&mut scenario, P1, "Llanowar Elves");
    let b = permanent(&mut scenario, P1, "Darksteel Myr");
    let recipient = permanent(&mut scenario, owner, target_card);
    let removal = card(&mut scenario, P0, "Beast Within", Zone::Hand);
    let mut runner = scenario.build();
    if let AttackTarget::Battle(_) = target_to_build(recipient) {
        assign_protector(&mut runner, recipient);
    }
    attack(&mut runner, &[(attacker, target_to_build(recipient))]);
    cast_removal(&mut runner, removal, recipient, &[ManaType::Green; 3]);
    block(&mut runner, &[(a, attacker), (b, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    submit(
        &mut runner,
        P0,
        attacker,
        damage_action(vec![(a, 1), (b, 3)], 0, 0),
        &[(TargetRef::Object(a), 1), (TargetRef::Object(b), 3)],
    );
}

#[test]
fn ordinary_trample_after_planeswalker_removal_stays_on_blockers() {
    ordinary_trample_after_removal(AttackTarget::Planeswalker, "Jace Beleren", P1);
}

#[test]
fn ordinary_trample_after_battle_removal_stays_on_blockers() {
    ordinary_trample_after_removal(AttackTarget::Battle, "Invasion of Kaladesh", P0);
}

// Row 10.
#[test]
fn no_blockers_and_no_trample_offers_an_empty_normal_split() {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Lone Wolf");
    let blocker = permanent(&mut scenario, P1, "Grizzly Bears");
    let bolt = card(&mut scenario, P0, "Lightning Bolt", Zone::Hand);
    let mut runner = scenario.build();
    attack(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
    block(&mut runner, &[(blocker, attacker)]);
    cast_removal(&mut runner, bolt, blocker, &[ManaType::Red]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    submit(&mut runner, P0, attacker, damage_action(vec![], 0, 0), &[]);
}

// Row 11. P1 protects the Siege, so their blocker survives the Siege owner's concession.
#[test]
fn concession_changes_suggestion_without_replacing_the_prompt() {
    let mut scenario = scenario(3);
    let attacker = permanent(&mut scenario, P0, "Stampeding Rhino");
    let blocker = permanent(&mut scenario, P1, "Grizzly Bears");
    let siege = permanent(&mut scenario, P2, "Invasion of Kaladesh");
    // The pre-existing Siege setup abandons its as-enters choice under P2.
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    assign_protector(&mut runner, siege);
    attack(&mut runner, &[(attacker, AttackTarget::Battle(siege))]);
    block(&mut runner, &[(blocker, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    let prompt = serde_json::to_value(&runner.state().waiting_for).expect("prompt serializes");
    let before = wire(&runner, P0);
    let old: CombatDamageSplit =
        serde_json::from_value(before["derived"]["combat_damage_suggestion"].clone())
            .expect("initial wire suggestion");
    assert_eq!(
        old,
        CombatDamageSplit {
            assignments: vec![(blocker, 2)],
            trample_damage: 2,
            controller_damage: 0
        }
    );

    apply(
        runner.state_mut(),
        P2,
        GameAction::Concede { player_id: P2 },
    )
    .expect("P2 concedes");
    assert_eq!(
        serde_json::to_value(&runner.state().waiting_for).expect("prompt serializes"),
        prompt
    );
    let after = wire(&runner, P0);
    assert_eq!(
        after["derived"]["combat_damage_suggestion"],
        serde_json::json!({
            "assignments": [[blocker.0, 4]], "trample_damage": 0, "controller_damage": 0,
        })
    );
    let mut stale = runner.state().clone();
    let rejection =
        apply(&mut stale, P0, suggested_action(old)).expect_err("old excess has no recipient");
    assert!(rejection
        .to_string()
        .contains("No current recipient for combat damage excess"));
    submit(
        &mut runner,
        P0,
        attacker,
        damage_action(vec![(blocker, 4)], 0, 0),
        &[(TargetRef::Object(blocker), 4)],
    );
}

// Row 12.
#[test]
fn suggestion_is_absent_outside_damage_assignment() {
    let mut scenario = scenario(2);
    let attacker = permanent(&mut scenario, P0, "Stampeding Rhino");
    let blocker = permanent(&mut scenario, P1, "Grizzly Bears");
    let mut runner = scenario.build();
    attack(&mut runner, &[(attacker, AttackTarget::Player(P1))]);
    assert!(derive_views(runner.state(), Some(P1))
        .combat_damage_suggestion
        .is_none());
    assert!(wire(&runner, P1)["derived"]
        .get("combat_damage_suggestion")
        .is_none());
    block(&mut runner, &[(blocker, attacker)]);
    assert_eq!(chooser(&mut runner, attacker), P0);
    submit(
        &mut runner,
        P0,
        attacker,
        damage_action(vec![(blocker, 2)], 2, 0),
        &[(TargetRef::Object(blocker), 2), (TargetRef::Player(P1), 2)],
    );
    assert!(derive_views(runner.state(), Some(P1))
        .combat_damage_suggestion
        .is_none());
    assert!(wire(&runner, P1)["derived"]
        .get("combat_damage_suggestion")
        .is_none());
}

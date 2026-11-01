//! Last-known attack target on the combat-status record
//! (`ZoneChangeCombatStatus::attack_target`).
//!
//! CR 506.3 + CR 508.1b: an attacking creature is attacking one declared
//! target (a player, a planeswalker or a battle). CR 508.5 collapses that
//! target to a "defending player", which loses the planeswalker/battle
//! identity; the record keeps the declared target itself, beside
//! `defending_player`, so a source whose ability resolves after it left the
//! battlefield (CR 113.7a, CR 608.2h) can still be asked what it was
//! attacking.
//!
//! Every fixture drives the real pipeline: `GameRunner::declare_attackers`
//! places the attack trigger through `trigger_source_context_for_latch`, and
//! `zones::move_to_zone` / `move_to_library_at_index` build the zone-exit
//! record through the production capture. Reach guards pair every `None`
//! assertion with a `Some` observed in the same test, so a `None` is never
//! vacuous.
//!
//! This file covers the record; the resolver that reads it is tested with the referent.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::triggers::trigger_source_context_for_latch;
use engine::game::zones;
use engine::types::ability::{ChosenAttribute, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{StackEntryKind, WaitingFor, ZoneChangeCombatStatus};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const P2: PlayerId = PlayerId(2);

/// A neutral self-attack trigger: the asker is the attacker.
const ATTACKER_TEXT: &str = "Whenever this creature attacks, you gain 1 life.";
/// A two-clause chain, the shape of Myr Battlesphere's trigger.
const CHAIN_TEXT: &str = "Whenever this creature attacks, you gain 1 life. Then you draw a card.";
/// An observer: fires for any creature its controller attacks with.
const OBSERVER_TEXT: &str = "Whenever a creature you control attacks, you gain 1 life.";

/// Which kind of target the attacker is sent at.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Player,
    Planeswalker,
    Battle,
}

struct Board {
    runner: GameRunner,
    attacker: ObjectId,
    /// What the attacker is declared against.
    target: AttackTarget,
    /// The player the target collapses to (CR 508.5).
    defender: PlayerId,
}

/// Turn an existing battlefield creature into a Siege battle with the given
/// protector and printed defense (local copy of the helper in
/// `defending_player_controls_combat_anchor.rs`: it is private to that module).
fn make_into_siege(
    runner: &mut GameRunner,
    id: ObjectId,
    protector: PlayerId,
    printed_defense: u32,
) {
    let obj = runner.state_mut().objects.get_mut(&id).unwrap();
    obj.card_types.core_types.clear();
    obj.card_types.core_types.push(CoreType::Battle);
    obj.card_types.subtypes = vec!["Siege".to_string()];
    obj.base_card_types = obj.card_types.clone();
    obj.power = None;
    obj.toughness = None;
    obj.base_power = None;
    obj.base_toughness = None;
    obj.defense = Some(printed_defense);
    obj.base_defense = Some(printed_defense);
    obj.counters.insert(CounterType::Defense, printed_defense);
    obj.chosen_attributes
        .push(ChosenAttribute::Player(protector));
}

/// Three players. P0's creature (with `oracle`) is at declare attackers;
/// `observer` optionally adds a second, non-attacking P0 creature.
///
/// The planeswalker (Karn) is controlled by P2, so its controller differs from
/// the player a mis-collapsed target would name. The Siege is controlled by P0
/// and protected by P1 (CR 310.9b, CR 310.9d), so protector differs from
/// controller.
fn board(oracle: &str, kind: Kind, observer: Option<&str>) -> (Board, Option<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario
        .add_creature_from_oracle(P0, "Latch Attacker", 2, 2, oracle)
        .id();
    let observer_id = observer.map(|text| {
        scenario
            .add_creature_from_oracle(P0, "Latch Observer", 1, 1, text)
            .id()
    });
    let karn = scenario
        .add_planeswalker_from_oracle(P2, "Karn Stand-in", "Karn", 5, "+1: You gain 1 life.")
        .id();
    let siege = scenario.add_creature(P0, "Placeholder Siege", 0, 0).id();
    let mut runner = scenario.build();
    make_into_siege(&mut runner, siege, P1, 5);

    let (target, defender) = match kind {
        Kind::Player => (AttackTarget::Player(P1), P1),
        Kind::Planeswalker => (AttackTarget::Planeswalker(karn), P2),
        Kind::Battle => (AttackTarget::Battle(siege), P1),
    };
    (
        Board {
            runner,
            attacker,
            target,
            defender,
        },
        observer_id,
    )
}

fn order_triggers_if_needed(runner: &mut GameRunner) {
    while let WaitingFor::OrderTriggers { triggers, .. } = &runner.state().waiting_for {
        let order = (0..triggers.len()).collect();
        runner
            .act(GameAction::OrderTriggers { order })
            .expect("ordering attack triggers");
    }
}

fn declare(board: &mut Board) {
    board.runner.advance_to_combat();
    board
        .runner
        .declare_attackers(&[(board.attacker, board.target)])
        .expect("the attack is legal");
    order_triggers_if_needed(&mut board.runner);
}

/// The ability of the trigger on the stack from `source`.
fn stack_ability(runner: &GameRunner, source: ObjectId) -> &ResolvedAbility {
    let mut found = runner
        .state()
        .stack
        .iter()
        .filter(|entry| entry.source_id == source)
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability.as_ref()),
            _ => None,
        });
    let ability = found
        .next()
        .expect("one trigger from the source is on the stack");
    assert!(
        found.next().is_none(),
        "reach guard: exactly one trigger from the source"
    );
    ability
}

fn stack_latch(runner: &GameRunner, source: ObjectId) -> ZoneChangeCombatStatus {
    let status = stack_ability(runner, source)
        .trigger_source
        .as_ref()
        .expect("a placed trigger carries its source context")
        .combat_status;
    if let Some(target) = status.attack_target {
        assert_eq!(
            status.attack_target_identity,
            engine::game::combat::bind_attack_target(runner.state(), target)
        );
    }
    status
}

#[test]
fn recipient_retirement_captures_an_attacking_creature_with_no_destination() {
    for kind in [Kind::Planeswalker, Kind::Battle] {
        let (mut b, _) = board(ATTACKER_TEXT, kind, None);
        declare(&mut b);
        let placed = stack_latch(&b.runner, b.attacker);
        assert!(placed.attack_target_identity.is_some());
        let destination = match b.target {
            AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => id,
            _ => unreachable!(),
        };
        let snapshot = b.runner.state().combat.as_ref().unwrap().attackers.clone();
        engine::game::effects::remove_from_combat::remove_object_from_combat(
            b.runner.state_mut(),
            destination,
        );
        assert_eq!(
            b.runner.state().combat.as_ref().unwrap().attackers,
            snapshot
        );
        let live = trigger_source_context_for_latch(
            b.runner.state(),
            &b.runner.state().objects[&b.attacker],
        );
        assert!(live.combat_status.attacking);
        assert_eq!(live.combat_status.defending_player, Some(b.defender));
        assert_eq!(live.combat_status.attack_target, None);
        assert_eq!(live.combat_status.attack_target_identity, None);
        let mut events = Vec::new();
        zones::move_to_library_position(b.runner.state_mut(), b.attacker, true, &mut events);
        let row = zone_changed_record(&events, b.attacker, Zone::Library);
        assert_eq!(row.combat_status, live.combat_status);
        assert_eq!(
            stack_ability(&b.runner, b.attacker)
                .trigger_source
                .as_ref()
                .unwrap()
                .combat_status,
            placed,
            "past placement is immutable"
        );
    }
}

/// The `ZoneChanged` record emitted for `object` leaving to `to`.
fn zone_changed_record(
    events: &[GameEvent],
    object: ObjectId,
    to: Zone,
) -> engine::types::game_state::ZoneChangeRecord {
    events
        .iter()
        .find_map(|event| match event {
            GameEvent::ZoneChanged {
                object_id,
                to: dest,
                record,
                ..
            } if *object_id == object && *dest == to => Some(record.as_ref().clone()),
            _ => None,
        })
        .expect("the move emitted a ZoneChanged record")
}

/// A self-attack trigger placed against a player latches the declared
/// target (producer: `capture_combat_status` via `trigger_source_context_for_latch`).
#[test]
fn attack_trigger_latches_player_target() {
    let (mut b, _) = board(ATTACKER_TEXT, Kind::Player, None);
    declare(&mut b);
    let latch = stack_latch(&b.runner, b.attacker);
    // Reach guards: the producer saw a live attacker against P1.
    assert!(latch.attacking);
    assert_eq!(latch.defending_player, Some(P1));
    // The new datum.
    assert_eq!(latch.attack_target, Some(AttackTarget::Player(P1)));
}

/// Attacking a planeswalker latches the planeswalker itself, while
/// `defending_player` keeps collapsing to its controller (CR 508.5).
#[test]
fn attack_trigger_latches_planeswalker_target_not_its_controller() {
    let (mut b, _) = board(ATTACKER_TEXT, Kind::Planeswalker, None);
    declare(&mut b);
    let latch = stack_latch(&b.runner, b.attacker);
    assert!(latch.attacking);
    assert_eq!(latch.defending_player, Some(P2), "collapsed to controller");
    assert_eq!(latch.attack_target, Some(b.target));
    assert!(matches!(
        latch.attack_target,
        Some(AttackTarget::Planeswalker(_))
    ));
}

/// Attacking a battle latches the battle, while `defending_player` is the
/// protector, not the controller (CR 310.9b, CR 310.9d).
#[test]
fn attack_trigger_latches_battle_target_not_its_protector() {
    let (mut b, _) = board(ATTACKER_TEXT, Kind::Battle, None);
    declare(&mut b);
    let latch = stack_latch(&b.runner, b.attacker);
    assert!(latch.attacking);
    assert_eq!(latch.defending_player, Some(P1), "protector");
    assert_eq!(latch.attack_target, Some(b.target));
    assert!(matches!(latch.attack_target, Some(AttackTarget::Battle(_))));
}

/// A non-attacking observer's trigger latches nothing, paired with the
/// attacker's own trigger in the same fixture, which latches the target.
#[test]
fn non_attacking_observer_latches_no_target() {
    let (mut b, observer) = board(ATTACKER_TEXT, Kind::Player, Some(OBSERVER_TEXT));
    let observer = observer.unwrap();
    declare(&mut b);

    // Positive control, same fixture: the attacker's own trigger has the target.
    let asker = stack_latch(&b.runner, b.attacker);
    assert!(asker.attacking);
    assert_eq!(asker.attack_target, Some(AttackTarget::Player(P1)));

    // The observer is not in combat. (Its trigger event is the attacker's; the
    // latch is the observer's own combat role.)
    let latch = stack_latch(&b.runner, observer);
    assert!(!latch.attacking);
    assert_eq!(latch.defending_player, None);
    assert_eq!(latch.attack_target, None);
}

/// A trigger placed outside combat has no attack target,
/// (the sibling of the attack-trigger rows above, where the same field is `Some`).
#[test]
fn trigger_outside_combat_latches_no_target() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let source = scenario
        .add_creature_from_oracle(
            P0,
            "Combat-Step Source",
            2,
            2,
            "At the beginning of combat on your turn, you gain 1 life.",
        )
        .id();
    let mut runner = scenario.build();
    runner.advance_to_phase(Phase::BeginCombat);
    // Reach guard: a trigger from the source really is on the stack at the beginning of combat.
    assert_eq!(runner.state().phase, Phase::BeginCombat);
    let latch = stack_latch(&runner, source);
    assert!(!latch.attacking);
    assert_eq!(latch.defending_player, None);
    assert_eq!(latch.attack_target, None);
}

/// The latch is a snapshot. Retargeting the live `AttackerInfo` afterwards
/// (synthetic mutation) does not move the stored value.
#[test]
fn latch_is_a_snapshot_not_a_live_read() {
    let (mut b, _) = board(ATTACKER_TEXT, Kind::Player, None);
    let karn = {
        // Karn is the only planeswalker on the board.
        let (id, _) = b
            .runner
            .state()
            .objects
            .iter()
            .find(|(_, o)| o.card_types.core_types.contains(&CoreType::Planeswalker))
            .expect("fixture has a planeswalker");
        *id
    };
    declare(&mut b);

    // Synthetic reselection: the live record now names Karn.
    let attacker = b.attacker;
    let info = b
        .runner
        .state_mut()
        .combat
        .as_mut()
        .unwrap()
        .attackers
        .iter_mut()
        .find(|a| a.object_id == attacker)
        .unwrap();
    info.attack_target = AttackTarget::Planeswalker(karn);
    // Positive control: the live mutation is observable.
    let live = b.runner.state().combat.as_ref().unwrap().attackers[0].attack_target;
    assert_eq!(live, AttackTarget::Planeswalker(karn));

    // The placed trigger still holds the declared target.
    assert_eq!(
        stack_latch(&b.runner, b.attacker).attack_target,
        Some(AttackTarget::Player(P1))
    );
}

/// The stored latch survives its source leaving the battlefield, and is
/// not recomputed: a fresh latch of the departed object holds nothing.
#[test]
fn latch_survives_source_leaving_the_battlefield() {
    let (mut b, _) = board(ATTACKER_TEXT, Kind::Planeswalker, None);
    declare(&mut b);
    let attacker = b.attacker;
    let before = stack_latch(&b.runner, attacker);
    assert_eq!(before.attack_target, Some(b.target));

    zones::move_to_zone(
        b.runner.state_mut(),
        attacker,
        Zone::Graveyard,
        &mut Vec::new(),
    );

    // Positive control: the source is gone while its trigger remains.
    assert_eq!(b.runner.state().objects[&attacker].zone, Zone::Graveyard);
    assert_eq!(stack_latch(&b.runner, attacker), before);

    // A fresh latch of the departed object would hold nothing: the stored
    // value was captured, not recomputed.
    let fresh =
        trigger_source_context_for_latch(b.runner.state(), &b.runner.state().objects[&attacker]);
    assert_eq!(fresh.combat_status.attack_target, None);
}

/// The latch rides every clause of a chained trigger.
#[test]
fn latch_propagates_to_sub_abilities() {
    let (mut b, _) = board(CHAIN_TEXT, Kind::Planeswalker, None);
    declare(&mut b);
    let ability = stack_ability(&b.runner, b.attacker);
    let sub = ability
        .sub_ability
        .as_ref()
        .expect("the second clause is a sub-ability");
    let latch = sub
        .trigger_source
        .as_ref()
        .expect("the sub-ability carries the source context")
        .combat_status;
    assert_eq!(latch.attack_target, Some(b.target));
    assert_eq!(latch.defending_player, Some(b.defender));
}

/// The zone-exit record, its projected source context and the
/// `zone_changes_this_turn` row (the row a battlefield-departure look-back
/// reads) all hold the target, for each kind of attack target. A non-attacker
/// and a blocker leaving hold none.
#[test]
fn zone_exit_records_latch_attack_target() {
    for kind in [Kind::Player, Kind::Planeswalker, Kind::Battle] {
        let (mut b, _) = board(ATTACKER_TEXT, kind, None);
        declare(&mut b);
        let attacker = b.attacker;
        let mut events = Vec::new();
        zones::move_to_zone(b.runner.state_mut(), attacker, Zone::Graveyard, &mut events);

        let record = zone_changed_record(&events, attacker, Zone::Graveyard);
        let projected = record
            .trigger_source_context()
            .expect("the record owns a source context")
            .combat_status;
        let row = b
            .runner
            .state()
            .zone_changes_this_turn
            .iter()
            .rev()
            .find(|row| row.object_id == attacker)
            .expect("the move leaves a zone-change row")
            .combat_status;
        for (which, status) in [
            ("event record", record.combat_status),
            ("projected source context", projected),
            ("zone_changes_this_turn row", row),
        ] {
            // Reach guard.
            assert!(status.attacking, "{kind:?} {which}");
            assert_eq!(
                status.defending_player,
                Some(b.defender),
                "{kind:?} {which}"
            );
            assert_eq!(status.attack_target, Some(b.target), "{kind:?} {which}");
        }
    }

    // Siblings: a creature not in combat, in a fixture that has a live attack.
    let (mut b, observer) = board(ATTACKER_TEXT, Kind::Player, Some(OBSERVER_TEXT));
    let observer = observer.unwrap();
    declare(&mut b);
    let mut events = Vec::new();
    zones::move_to_zone(b.runner.state_mut(), observer, Zone::Graveyard, &mut events);
    let record = zone_changed_record(&events, observer, Zone::Graveyard);
    assert!(
        b.runner.state().combat.as_ref().unwrap().attackers.len() == 1,
        "reach guard: combat is live with an attacker"
    );
    assert!(!record.combat_status.attacking);
    assert_eq!(record.combat_status.attack_target, None);
}

/// A blocker leaving holds no attack target even while the
/// creature it blocks is attacking a target.
#[test]
fn blocker_zone_exit_holds_no_attack_target() {
    let (mut b, _) = board(ATTACKER_TEXT, Kind::Player, None);
    // Give P1 a blocker.
    let blocker = {
        let state = b.runner.state_mut();
        let id = engine::game::zones::create_object(
            state,
            engine::types::identifiers::CardId(900),
            P1,
            "Latch Blocker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.base_card_types = obj.card_types.clone();
        obj.power = Some(1);
        obj.toughness = Some(1);
        obj.base_power = Some(1);
        obj.base_toughness = Some(1);
        obj.summoning_sick = false;
        id
    };
    declare(&mut b);
    // Synthetic block record: the blocker is blocking the attacker.
    let attacker = b.attacker;
    b.runner
        .state_mut()
        .combat
        .as_mut()
        .unwrap()
        .blocker_to_attacker
        .insert(blocker, vec![attacker]);
    let mut events = Vec::new();
    zones::move_to_zone(b.runner.state_mut(), blocker, Zone::Graveyard, &mut events);
    let record = zone_changed_record(&events, blocker, Zone::Graveyard);
    // Reach guard: it was captured as a blocker.
    assert!(record.combat_status.blocking);
    assert_eq!(record.combat_status.attack_target, None);
    assert_eq!(record.combat_status.defending_player, None);
}

/// The library-move record holds the target too.
#[test]
fn library_move_record_latches_attack_target() {
    for kind in [Kind::Player, Kind::Planeswalker, Kind::Battle] {
        let (mut b, _) = board(ATTACKER_TEXT, kind, None);
        declare(&mut b);
        let attacker = b.attacker;
        let mut events = Vec::new();
        zones::move_to_library_at_index(b.runner.state_mut(), attacker, Some(0), &mut events);
        let record = zone_changed_record(&events, attacker, Zone::Library);
        assert!(record.combat_status.attacking, "{kind:?}");
        assert_eq!(
            record.combat_status.attack_target,
            Some(b.target),
            "{kind:?}"
        );
    }
}

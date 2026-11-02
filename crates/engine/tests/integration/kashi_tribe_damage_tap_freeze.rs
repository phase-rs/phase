//! CR 608.2k + CR 701.26a: "Whenever this creature deals combat
//! damage to a creature, tap that creature and it doesn't untap during its
//! controller's next untap step." (Kashi-Tribe Warriors, Kashi-Tribe Elite).
//!
//! "That creature" is the damage recipient, an untargeted event referent
//! (`TargetFilter::EventTarget`), so the trigger has no chosen target. The tap
//! resolver read only `ability.targets`, so the trigger resolved and tapped
//! nothing. It now reads the event-subject anaphors from the trigger event, the
//! way Destroy and Prepare already do.

use super::rules::{GameScenario, Phase, P0, P1};
use engine::game::combat::AttackTarget;
use engine::game::scenario::GameRunner;
use engine::game::zones::move_to_zone;
use engine::types::ability::{ContinuousModification, TargetFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

const KASHI_TRIBE_WARRIORS: &str = "Whenever this creature deals combat damage to a creature, tap that creature and it doesn't untap during its controller's next untap step.";

fn pass_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
}

/// Advance turn by turn to `player`'s next upkeep, declaring no attackers when
/// a declare-attackers step asks for them.
fn advance_to_upkeep_of(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..8 {
        runner.advance_to_upkeep();
        if runner.state().phase == Phase::Upkeep && runner.state().active_player == player {
            return;
        }
        if runner.waiting_for_kind() == "DeclareAttackers" {
            runner.declare_attackers(&[]).expect("declare no attackers");
        } else {
            runner.advance_to_end_step();
        }
    }
    panic!("did not reach {player:?}'s upkeep");
}

fn is_tapped(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].tapped
}

/// CR 701.26a + CR 502.3: Kashi-Tribe Warriors attacks and is blocked. The
/// blocker it damaged becomes tapped, stays tapped through its controller's
/// next untap step, and untaps normally in the untap step after that.
#[test]
fn damaged_blocker_is_tapped_and_skips_its_controllers_next_untap() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island"]);
    let warriors = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Wood", 0, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(warriors, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, warriors)])
        .expect("declare blockers");
    runner.combat_damage();

    assert!(
        is_tapped(&runner, wall),
        "CR 701.26a: the creature dealt combat damage must be tapped"
    );

    // CR 502.3: P1's next untap step — the wall must stay tapped.
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        is_tapped(&runner, wall),
        "the damaged creature must not untap during its controller's next untap step"
    );

    // The restriction covers only that one untap step.
    runner.advance_to_end_step();
    runner.advance_to_upkeep();
    assert_eq!(runner.state().active_player, P0);
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        !is_tapped(&runner, wall),
        "the creature untaps normally in its controller's following untap step"
    );
}

/// CR 701.26a + CR 702.20b: blocking side. A vigilant attacker isn't tapped by
/// attacking, so being tapped after combat can only come from the trigger.
#[test]
fn damaged_attacker_is_tapped_when_warriors_blocks() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island"]);
    let warriors = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.id()
    };
    let attacker = {
        let mut b = scenario.add_creature(P1, "Vigilant Giant", 2, 5);
        b.vigilance();
        b.id()
    };

    let mut runner = scenario.build();
    runner.state_mut().active_player = P1;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P0))])
        .expect("declare attackers");
    assert!(
        !is_tapped(&runner, attacker),
        "CR 702.20b: vigilance keeps the attacker untapped"
    );
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(warriors, attacker)])
        .expect("declare blockers");
    runner.combat_damage();

    assert!(
        is_tapped(&runner, attacker),
        "CR 701.26a: the attacker dealt combat damage must be tapped"
    );
    assert!(
        !is_tapped(&runner, warriors),
        "the trigger source is not its own referent"
    );

    // CR 502.3: the freeze is keyed to the controller of the affected creature
    // (P1), not the controller of the ability (P0). It is P1's turn, so P1's
    // untap step has already passed; P0's untap step does not untap P1's
    // permanents.
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P0);
    assert!(
        is_tapped(&runner, attacker),
        "P0's untap step does not untap P1's permanent"
    );

    // P1's next untap step: the attacker must stay tapped.
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        is_tapped(&runner, attacker),
        "the damaged attacker must not untap during its controller's next untap step"
    );

    // P1's following untap step (after P0's turn): it untaps normally.
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P0);
    runner.advance_to_end_step();
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        !is_tapped(&runner, attacker),
        "the attacker untaps normally in its controller's following untap step"
    );
}

/// CR 701.26a: only a permanent can be tapped. In one combat two Warriors each
/// deal combat damage to a different blocker: the survivor is tapped, the one
/// that died from the damage is no longer on the battlefield and is not
/// tapped, and neither trigger falls back to its source.
#[test]
fn recipient_that_died_is_not_tapped_while_survivor_in_same_combat_is() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let warriors_a = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.vigilance();
        b.id()
    };
    let warriors_b = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.vigilance();
        b.id()
    };
    let bear = scenario.add_creature(P1, "Grizzly Bears", 2, 2).id();
    let wall = scenario.add_creature(P1, "Wall of Wood", 0, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[
            (warriors_a, AttackTarget::Player(P1)),
            (warriors_b, AttackTarget::Player(P1)),
        ])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(bear, warriors_a), (wall, warriors_b)])
        .expect("declare blockers");
    let outcome = runner.combat_damage();

    // Positive reach-guards: both damage triggers resolved in this combat and
    // the surviving recipient was tapped exactly once.
    assert!(
        is_tapped(&runner, wall),
        "CR 701.26a: the surviving creature dealt combat damage must be tapped"
    );
    let tapped_wall = outcome
        .events()
        .iter()
        .filter(|e| matches!(e, GameEvent::PermanentTapped { object_id, .. } if *object_id == wall))
        .count();
    assert_eq!(tapped_wall, 1, "events: {:?}", outcome.events());
    let resolved = outcome
        .events()
        .iter()
        .filter(|e| matches!(e, GameEvent::StackResolved { .. }))
        .count();
    assert!(
        resolved >= 2,
        "both damage triggers must resolve; events: {:?}",
        outcome.events()
    );

    // Negatives.
    assert_eq!(outcome.zone_of(bear), Zone::Graveyard);
    assert!(
        !runner.state().objects[&bear].tapped,
        "a creature card in the graveyard is not a permanent and is not tapped"
    );
    assert!(
        !outcome.events().iter().any(
            |e| matches!(e, GameEvent::PermanentTapped { object_id, .. } if *object_id == bear)
        ),
        "no tap event for the dead recipient"
    );
    assert!(
        !is_tapped(&runner, warriors_a) && !is_tapped(&runner, warriors_b),
        "the trigger must not fall back to tapping its source"
    );
}

/// CR 400.7 + CR 506.4: the damage recipient leaves the battlefield and
/// returns while the trigger is on the stack. The returned permanent is a new
/// object: the trigger neither taps it nor stops it from untapping.
#[test]
fn blinked_recipient_is_neither_tapped_nor_frozen() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Forest", "Forest", "Forest"]);
    scenario.with_library_top(P1, &["Island", "Island", "Island"]);
    let warriors = {
        let mut b = scenario.add_creature(P0, "Kashi-Tribe Warriors", 2, 4);
        b.from_oracle_text(KASHI_TRIBE_WARRIORS);
        b.id()
    };
    let wall = scenario.add_creature(P1, "Wall of Wood", 0, 5).id();

    let mut runner = scenario.build();
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(warriors, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(wall, warriors)])
        .expect("declare blockers");
    for _ in 0..8 {
        if !runner.state().stack.is_empty() {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass into combat damage");
    }

    // CR 510.2 + CR 510.3a: combat damage was dealt and the trigger was put on
    // the stack.
    assert_eq!(runner.state().stack.len(), 1);
    assert_eq!(runner.state().phase, Phase::CombatDamage);
    assert!(
        runner
            .state()
            .damage_dealt_this_turn
            .iter()
            .any(|record| record.target == TargetRef::Object(wall) && record.is_combat),
        "the wall must have been dealt combat damage"
    );

    // Blink the wall: CR 400.7 + CR 506.4, a new object removed from combat.
    let mut events = Vec::new();
    move_to_zone(runner.state_mut(), wall, Zone::Exile, &mut events);
    move_to_zone(runner.state_mut(), wall, Zone::Battlefield, &mut events);
    assert_eq!(runner.state().objects[&wall].zone, Zone::Battlefield);

    runner.resolve_top();
    assert!(
        runner.state().stack.is_empty(),
        "the trigger must have resolved"
    );
    assert!(
        !is_tapped(&runner, wall),
        "CR 400.7: the returned permanent is a new object and is not tapped"
    );
    assert!(
        !runner
            .state()
            .transient_continuous_effects
            .iter()
            .any(|tce| {
                tce.affected == TargetFilter::SpecificObject { id: wall }
                    && tce
                        .modifications
                        .contains(&ContinuousModification::AddStaticMode {
                            mode: StaticMode::CantUntap,
                        })
            }),
        "CR 400.7: the returned permanent gets no untap restriction; TCEs: {:?}",
        runner.state().transient_continuous_effects
    );

    // Stronger check: tapped by other means, the new object untaps in its
    // controller's next untap step (the paired positive is
    // `damaged_blocker_is_tapped_and_skips_its_controllers_next_untap`).
    runner.state_mut().objects.get_mut(&wall).unwrap().tapped = true;
    advance_to_upkeep_of(&mut runner, P1);
    assert!(
        !is_tapped(&runner, wall),
        "the returned permanent untaps during its controller's next untap step"
    );
}

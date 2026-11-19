//! Marauding Raptor — "Whenever another creature you control enters, this
//! creature deals 2 damage to it. If a Dinosaur is dealt damage this way, this
//! creature gets +2/+0 until end of turn."
//!
//! CR 603.2 + CR 608.2c: the "If a Dinosaur is dealt damage this way" clause is
//! a conditional rider on the same triggered ability — the sub-ability is
//! checked on resolution against the damage the first instruction dealt, and
//! the pump must land whenever a Dinosaur received that damage, even when the
//! Dinosaur survives it (Colossal Dreadmaw, 6/6).
//!
//! Field report (issue #9012): the Raptor dealt its 2 damage to entering
//! Dinosaurs but the +2/+0 never applied.

use engine::database::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

/// Colossal Dreadmaw: a 6/6 Dinosaur, so the 2 damage does not kill it — the
/// damage-dealt rider is tested independently of lethality.
const ENTERING_DINOSAUR: &str = "Colossal Dreadmaw";
/// A Dragon, satisfying the trigger's "another creature you control enters"
/// while missing the "Dinosaur" rider filter — the non-matching control.
const ENTERING_NON_DINOSAUR: &str = "Shivan Dragon";

/// A pool of `colored` mana of one color plus enough colorless to cover a
/// {N}+{C}{C} casting cost.
fn cast_pool(color: ManaType, colored: usize, total: usize) -> Vec<ManaUnit> {
    (0..total)
        .map(|i| {
            if i < colored {
                ManaUnit::new(color, ObjectId(0), false, vec![])
            } else {
                ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])
            }
        })
        .collect()
}

#[test]
fn raptor_pumps_when_a_surviving_dinosaur_enters() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Green, 2, 7));
    let mut runner = scenario.build();

    let dino = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == ENTERING_DINOSAUR)
        .map(|(id, _)| *id)
        .expect("Dinosaur should be in hand");

    runner.cast(dino).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: the trigger really dealt 2 damage to the Dinosaur
    // (CR 120.3: damage is marked, not subtracted from base power).
    assert_eq!(
        runner.state().objects[&dino].damage_marked,
        2,
        "Raptor's 2 damage must be marked on the entering Dinosaur"
    );
    assert_eq!(runner.state().objects[&dino].zone, Zone::Battlefield);
    // The rider: +2/+0 until end of turn. 2 base -> 4 effective power.
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "a Dinosaur took the damage, so Marauding Raptor must be +2/+0"
    );
}

#[test]
fn raptor_does_not_pump_for_a_non_dinosaur() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, ENTERING_NON_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Red, 2, 7));
    let mut runner = scenario.build();

    let dragon = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == ENTERING_NON_DINOSAUR)
        .map(|(id, _)| *id)
        .expect("Dragon should be in hand");

    runner.cast(dragon).resolve();
    runner.advance_until_stack_empty();

    // Reach guard: the trigger really fired — the Dragon took the 2 damage too.
    assert_eq!(
        runner.state().objects[&dragon].damage_marked,
        2,
        "the Dragon must take the same 2 damage the Dinosaur case deals"
    );
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "a non-Dinosaur took the damage, so no pump"
    );
}

/// Pass priority until the stack is empty, appending every event emitted on the
/// way so a test can prove the Raptor's damage instruction actually resolved.
fn drain_stack_collecting_events(runner: &mut GameRunner, events: &mut Vec<GameEvent>) {
    for _ in 0..20 {
        if runner.state().stack.is_empty() {
            break;
        }
        let result = runner
            .act(GameAction::PassPriority)
            .expect("passing priority must be legal while the trigger is on the stack");
        events.extend(result.events);
    }
    assert!(runner.state().stack.is_empty(), "the stack must drain");
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
        "resolution must end at a priority window"
    );
}

/// Cast the entering Dinosaur with `shield` already on the battlefield, then
/// resolve the creature spell and the Raptor's trigger.
fn cast_dinosaur_under_shield(
    db: &CardDatabase,
    shield: &str,
) -> (GameRunner, ObjectId, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let raptor = scenario.add_real_card(P0, "Marauding Raptor", Zone::Battlefield, db);
    scenario.add_real_card(P0, shield, Zone::Battlefield, db);
    scenario.add_real_card(P0, ENTERING_DINOSAUR, Zone::Hand, db);
    scenario.with_mana_pool(P0, cast_pool(ManaType::Green, 2, 7));
    let mut runner = scenario.build();

    let dino = runner
        .state()
        .objects
        .iter()
        .find(|(_, o)| o.name == ENTERING_DINOSAUR)
        .map(|(id, _)| *id)
        .expect("Dinosaur should be in hand");

    // The cast harness drives the creature spell and the Raptor's trigger;
    // drain anything it left on the stack so the event log is complete.
    let mut events = runner.cast(dino).resolve().events().to_vec();
    drain_stack_collecting_events(&mut runner, &mut events);
    (runner, raptor, dino, events)
}

/// CR 615.1a + CR 608.2c: prevented damage is damage that is not dealt, so a
/// Dinosaur whose 2 damage is fully prevented (Inner Sanctum: "Prevent all
/// damage that would be dealt to creatures you control.") was not "dealt damage
/// this way" and the Raptor must not get +2/+0. The rider's
/// `PreviousEffectAmount > 0` conjunct reads the post-prevention amount the
/// damage instruction actually dealt, not the entering creature's identity.
#[test]
fn raptor_does_not_pump_when_the_damage_to_the_dinosaur_is_prevented() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, events) = cast_dinosaur_under_shield(db, "Inner Sanctum");

    // Reach guard: the trigger resolved and its 2 damage to the Dinosaur was
    // prevented, not skipped.
    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 2 }
                if *source_id == raptor && *target == dino
        )),
        "the Raptor's 2 damage to the Dinosaur must be prevented, got {events:?}"
    );
    assert_eq!(runner.state().objects[&dino].zone, Zone::Battlefield);
    assert_eq!(runner.state().objects[&dino].damage_marked, 0);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(2),
        "no damage was dealt to the Dinosaur, so Marauding Raptor must not be pumped"
    );
}

/// CR 615.1a + CR 608.2c: a partial prevention still leaves damage dealt.
/// Temple Altisaur ("If a source would deal damage to another Dinosaur you
/// control, prevent all but 1 of that damage.") cuts the 2 damage to 1; the
/// Dinosaur was still dealt damage this way, so the pump applies.
#[test]
fn raptor_pumps_when_the_damage_to_the_dinosaur_is_partially_prevented() {
    let Some(db) = load_db() else {
        eprintln!("skipping: card database absent");
        return;
    };
    let (runner, raptor, dino, events) = cast_dinosaur_under_shield(db, "Temple Altisaur");

    assert!(
        events.iter().any(|e| matches!(
            e,
            GameEvent::DamagePrevented { source_id, target: TargetRef::Object(target), amount: 1 }
                if *source_id == raptor && *target == dino
        )),
        "Temple Altisaur must prevent all but 1 of the Raptor's damage, got {events:?}"
    );
    assert_eq!(runner.state().objects[&dino].damage_marked, 1);
    assert_eq!(
        runner.state().objects[&raptor].power,
        Some(4),
        "the Dinosaur was still dealt 1 damage, so Marauding Raptor must be +2/+0"
    );
}

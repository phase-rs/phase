//! Regression for issue #11111 — Vile Bile went straight to the graveyard.
//!
//! Unhinged prints half-point power and toughness (Vile Bile 2½/2½, Little Girl
//! ½/½). Card data carried them as unresolved `PtValue::Variable` strings, so
//! the object resolved as a 0/0 and died to CR 704.5f the moment it entered.
//! They are now typed `PtValue::HalfPoints` and played as the integers that
//! decide every whole-damage outcome the way the printed value does
//! (CR 107.1a): toughness `t + ½` rounds up, power `p + ½` rounds down.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::PtValue;
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

#[test]
fn printed_half_points_load_as_typed_values() {
    let Some(db) = load_db() else {
        return;
    };
    let bile = db.get_face_by_name("Vile Bile").expect("Vile Bile");
    assert_eq!(bile.power, Some(PtValue::HalfPoints(5)), "printed 2½");
    assert_eq!(bile.toughness, Some(PtValue::HalfPoints(5)), "printed 2½");
    let girl = db.get_face_by_name("Little Girl").expect("Little Girl");
    assert_eq!(girl.power, Some(PtValue::HalfPoints(1)), "printed ½");
    assert_eq!(girl.toughness, Some(PtValue::HalfPoints(1)), "printed ½");
}

/// The reported bug: casting Vile Bile ({1}{B}) must leave it on the
/// battlefield, not put it into the graveyard as a 0/0.
#[test]
fn vile_bile_cast_from_hand_stays_on_the_battlefield() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Hand, db);
    let swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let mountain = scenario.add_basic_land(P0, ManaColor::Red);
    let mut runner = scenario.build();

    let outcome = runner.cast(bile).resolve();

    assert!(
        outcome.events().iter().any(|event| matches!(
            event,
            GameEvent::SpellCast { object_id, .. } if *object_id == bile
        )),
        "Vile Bile was cast"
    );
    assert!(
        [swamp, mountain]
            .iter()
            .all(|land| runner.state().objects[land].tapped),
        "both lands paid Vile Bile's {{1}}{{B}}"
    );
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "the cast resolves to a priority window: {:?}",
        outcome.final_waiting_for()
    );
    assert!(runner.state().stack.is_empty());
    outcome.assert_zone(&[bile], Zone::Battlefield);
    let obj = &runner.state().objects[&bile];
    assert_eq!(
        (obj.power, obj.toughness),
        (Some(2), Some(3)),
        "2½/2½ plays as power 2, toughness 3"
    );
}

/// Vile Bile attacks and is blocked by a vanilla `blocker_power`/
/// `blocker_toughness` creature. Returns where Vile Bile and the blocker end
/// up, then the combat damage Vile Bile dealt the blocker and the blocker
/// dealt Vile Bile.
fn vile_bile_blocked_by(blocker_power: i32, blocker_toughness: i32) -> (Zone, Zone, u32, u32) {
    let db = load_db().expect("card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let blocker = scenario
        .add_creature(P1, "Blocker", blocker_power, blocker_toughness)
        .id();
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bile, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(blocker, bile)])
        .expect("declare blockers");
    let outcome = runner.combat_damage();

    let dealt_to = |source: ObjectId, recipient: ObjectId| -> u32 {
        outcome
            .events()
            .iter()
            .filter_map(|event| match event {
                GameEvent::DamageDealt {
                    source_id,
                    target: engine::types::ability::TargetRef::Object(target),
                    amount,
                    is_combat: true,
                    ..
                } if *source_id == source && *target == recipient => Some(*amount),
                _ => None,
            })
            .sum()
    };
    (
        outcome.zone_of(bile),
        outcome.zone_of(blocker),
        dealt_to(bile, blocker),
        dealt_to(blocker, bile),
    )
}

fn pass_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers" {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    panic!("never reached declare blockers");
}

/// CR 704.5g: a 2/2 blocker's 2 damage is short of toughness 2½, while
/// Vile Bile's 2½ damage is lethal to the 2/2.
#[test]
fn vile_bile_survives_two_damage_and_kills_a_two_two() {
    if load_db().is_none() {
        return;
    }
    let (bile, blocker, bile_dealt, blocker_dealt) = vile_bile_blocked_by(2, 2);
    assert_eq!(
        (bile_dealt, blocker_dealt),
        (2, 2),
        "both creatures dealt combat damage"
    );
    assert_eq!(bile, Zone::Battlefield, "2 damage is not lethal to 2½");
    assert_eq!(blocker, Zone::Graveyard, "2½ damage is lethal to a 2/2");
}

/// CR 704.5g: a 3/3 blocker's 3 damage is lethal to toughness 2½, while
/// Vile Bile's 2½ damage is short of toughness 3.
#[test]
fn vile_bile_dies_to_three_damage_and_a_three_three_survives_it() {
    if load_db().is_none() {
        return;
    }
    let (bile, blocker, bile_dealt, blocker_dealt) = vile_bile_blocked_by(3, 3);
    assert_eq!(
        (bile_dealt, blocker_dealt),
        (2, 3),
        "both creatures dealt combat damage"
    );
    assert_eq!(bile, Zone::Graveyard, "3 damage is lethal to 2½");
    assert_eq!(blocker, Zone::Battlefield, "2½ damage is short of 3");
}

/// CR 704.5f: Little Girl's toughness ½ is above 0, so it survives the
/// state-based-action check that puts a 0-toughness creature into the
/// graveyard in the same pass.
#[test]
fn little_girl_survives_the_zero_toughness_check() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let girl = scenario.add_real_card(P0, "Little Girl", Zone::Battlefield, db);
    let zero = scenario.add_creature(P0, "Zero Toughness", 0, 0).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();

    assert_eq!(
        runner.state().objects[&zero].zone,
        Zone::Graveyard,
        "state-based actions ran: the 0/0 control is gone"
    );
    let obj = &runner.state().objects[&girl];
    assert_eq!(obj.zone, Zone::Battlefield, "toughness ½ is above 0");
    assert_eq!((obj.power, obj.toughness), (Some(0), Some(1)));
}

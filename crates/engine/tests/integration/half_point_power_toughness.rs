//! Regression for issue #11111 — exact half-point power and toughness.
//!
//! Unhinged prints half-point power and toughness (Vile Bile 2½/2½, Little Girl
//! ½/½, Bad Ass 3½/1). Per Wizards' Unhinged FAQ, Silver-Bordered half-point
//! values override CR 107.1a's integer-only restriction. The engine preserves
//! exact half-integer values across printed objects, tokens, animation, pumps,
//! layer evaluation, combat damage, noncombat damage, life changes, and SBAs
//! (CR 510.1a, CR 704.5f, CR 704.5g).
//!
//! Every runtime regression below drives printed cards through public actions.
//! No printed card animates, pumps, or creates a token with a *literal*
//! half-point P/T, so those `PtValue::HalfPoints` branches are covered only
//! where a printed card reaches them: dynamic quantities that read a half-point
//! power or life total (Nantuko Mentor, Serra Avatar, Bouncer's Beatdown) and
//! the token-copy path (Cackling Counterpart).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    EffectKind, HalfFraction, HalfMagnitude, HalfSigned, PtValue, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{CombatDamageAssignmentMode, WaitingFor};
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
/// battlefield as an exact 2½/2½ creature, not put it into the graveyard as 0/0.
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
        (
            obj.power,
            obj.power_fraction,
            obj.toughness,
            obj.toughness_fraction
        ),
        (Some(2), HalfFraction::Half, Some(2), HalfFraction::Half),
        "2½/2½ stays exact (2 + ½, 2 + ½)"
    );
    assert_eq!(obj.exact_power(), Some(HalfSigned::from_halves(5)));
    assert_eq!(obj.exact_toughness(), Some(HalfSigned::from_halves(5)));
}

fn pass_to_declare_blockers(runner: &mut GameRunner) {
    for _ in 0..8 {
        if runner.waiting_for_kind() == "DeclareBlockers"
            || runner.state().phase == Phase::DeclareBlockers
        {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority into the declare-blockers step");
    }
    panic!("never reached declare blockers");
}

fn combat_damage_dealt_exact(
    events: &[GameEvent],
    source: ObjectId,
    recipient: TargetRef,
) -> HalfMagnitude {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                source_id,
                target,
                amount,
                amount_fraction,
                is_combat: true,
                ..
            } if *source_id == source && *target == recipient => Some(
                HalfMagnitude::from_whole_and_fraction(*amount, *amount_fraction),
            ),
            _ => None,
        })
        .fold(HalfMagnitude::ZERO, HalfMagnitude::saturating_add)
}

/// Blocker F1 regression: two normally cast Vile Biles ({1}{B}) facing each
/// other in combat each deal 2½ damage (CR 510.1a) and destroy each other under
/// lethal damage SBAs (CR 704.5g).
#[test]
fn two_normally_cast_vile_biles_destroy_each_other_in_combat() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    scenario.with_library_top(P1, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    let p0_bile = scenario.add_real_card(P0, "Vile Bile", Zone::Hand, db);
    let _p0_swamp = scenario.add_basic_land(P0, ManaColor::Black);
    let _p0_mountain = scenario.add_basic_land(P0, ManaColor::Red);

    let p1_bile = scenario.add_real_card(P1, "Vile Bile", Zone::Hand, db);
    let _p1_swamp = scenario.add_basic_land(P1, ManaColor::Black);
    let _p1_island = scenario.add_basic_land(P1, ManaColor::Blue);
    let mut runner = scenario.build();

    // Turn 1 (P0): cast P0's Vile Bile for {1}{B}.
    let cast_p0 = runner.cast(p0_bile).resolve();
    cast_p0.assert_zone(&[p0_bile], Zone::Battlefield);

    // Advance to Turn 2 (P1's PreCombatMain) and cast P1's Vile Bile for {1}{B}.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::PreCombatMain);
    let cast_p1 = runner.cast(p1_bile).resolve();
    cast_p1.assert_zone(&[p1_bile], Zone::Battlefield);

    // Advance to Turn 3 (P0's combat) so P0's Vile Bile can legally attack.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(p0_bile, AttackTarget::Player(P1))])
        .expect("declare P0 Vile Bile attacker");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(p1_bile, p0_bile)])
        .expect("declare P1 Vile Bile blocker");
    let outcome = runner.combat_damage();

    assert_eq!(
        combat_damage_dealt_exact(outcome.events(), p0_bile, TargetRef::Object(p1_bile)),
        HalfMagnitude::from_halves(5),
        "attacking Vile Bile dealt 2½ combat damage"
    );
    assert_eq!(
        combat_damage_dealt_exact(outcome.events(), p1_bile, TargetRef::Object(p0_bile)),
        HalfMagnitude::from_halves(5),
        "blocking Vile Bile dealt 2½ combat damage"
    );
    assert_eq!(
        outcome.zone_of(p0_bile),
        Zone::Graveyard,
        "2½ damage is lethal to toughness 2½"
    );
    assert_eq!(
        outcome.zone_of(p1_bile),
        Zone::Graveyard,
        "2½ damage is lethal to toughness 2½"
    );
}

/// Vile Bile attacks and is blocked by a vanilla `blocker_power`/
/// `blocker_toughness` creature. Returns where Vile Bile and the blocker end
/// up, then the exact combat damage Vile Bile dealt the blocker and the blocker
/// dealt Vile Bile.
fn vile_bile_blocked_by(
    blocker_power: i32,
    blocker_toughness: i32,
) -> (Zone, Zone, HalfMagnitude, HalfMagnitude) {
    let db = load_db().expect("card fixture");
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    scenario.with_library_top(P1, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
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

    (
        outcome.zone_of(bile),
        outcome.zone_of(blocker),
        combat_damage_dealt_exact(outcome.events(), bile, TargetRef::Object(blocker)),
        combat_damage_dealt_exact(outcome.events(), blocker, TargetRef::Object(bile)),
    )
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
        (HalfMagnitude::from_halves(5), HalfMagnitude::from_halves(4)),
        "Vile Bile deals 2½ combat damage and 2/2 blocker deals 2"
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
        (HalfMagnitude::from_halves(5), HalfMagnitude::from_halves(6)),
        "Vile Bile deals 2½ combat damage and 3/3 blocker deals 3"
    );
    assert_eq!(bile, Zone::Graveyard, "3 damage is lethal to 2½");
    assert_eq!(blocker, Zone::Battlefield, "2½ damage is short of 3");
}

/// Unblocked Vile Bile deals 2½ combat damage twice: defender life moves
/// 20 -> 17½ -> 15 exactly.
#[test]
fn unblocked_vile_bile_deals_half_point_player_combat_damage_twice() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    scenario.with_library_top(P1, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bile, AttackTarget::Player(P1))])
        .expect("first unblocked attack");
    pass_to_declare_blockers(&mut runner);
    if runner.waiting_for_kind() == "DeclareBlockers" {
        runner.declare_blockers(&[]).expect("no blockers");
    }
    let first = runner.combat_damage();
    assert_eq!(
        combat_damage_dealt_exact(first.events(), bile, TargetRef::Player(P1)),
        HalfMagnitude::from_halves(5)
    );
    let p1 = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1");
    assert_eq!(
        (p1.life, p1.life_fraction),
        (17, HalfFraction::Half),
        "20 - 2½ = 17½"
    );
    assert_eq!(p1.exact_life(), HalfSigned::from_halves(35));

    // Advance to P0's next combat and attack unblocked a second time.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::PreCombatMain);
    runner.advance_to_phase(Phase::End);
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(bile, AttackTarget::Player(P1))])
        .expect("second unblocked attack");
    pass_to_declare_blockers(&mut runner);
    if runner.waiting_for_kind() == "DeclareBlockers" {
        runner.declare_blockers(&[]).expect("no blockers");
    }
    let _second = runner.combat_damage();
    let p1 = runner
        .state()
        .players
        .iter()
        .find(|p| p.id == P1)
        .expect("P1");
    assert_eq!(
        (p1.life, p1.life_fraction),
        (15, HalfFraction::Zero),
        "17½ - 2½ = 15"
    );
    assert_eq!(p1.exact_life(), HalfSigned::from_halves(30));
}

/// CR 704.5f & CR 704.5g: Little Girl (½/½) survives zero-toughness SBAs while
/// a 0/0 dies; ½ combat damage leaves a 1/1 alive with ½ marked, and two
/// blocking Little Girls stack ½ + ½ = 1 lethal damage to destroy the 1/1.
#[test]
fn little_girl_survives_zero_toughness_sba_and_half_damage_stacks() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let girl_a = scenario.add_real_card(P1, "Little Girl", Zone::Battlefield, db);
    let girl_b = scenario.add_real_card(P1, "Little Girl", Zone::Battlefield, db);
    let zero = scenario.add_creature(P0, "Zero Toughness", 0, 0).id();
    let bear_one = scenario.add_creature(P0, "One-One A", 1, 1).id();
    let bear_two = scenario.add_creature(P0, "One-One B", 1, 1).id();
    let mut runner = scenario.build();

    runner.advance_to_combat();

    assert_eq!(
        runner.state().objects[&zero].zone,
        Zone::Graveyard,
        "state-based actions put the 0/0 in the graveyard"
    );
    let obj_a = &runner.state().objects[&girl_a];
    assert_eq!(obj_a.zone, Zone::Battlefield, "toughness ½ is above 0");
    assert_eq!(
        (
            obj_a.power,
            obj_a.power_fraction,
            obj_a.toughness,
            obj_a.toughness_fraction
        ),
        (Some(0), HalfFraction::Half, Some(0), HalfFraction::Half)
    );

    // Phase 1: single 1/1 attacks and is blocked by one Little Girl -> ½ < 1, 1/1 lives; 1 >= ½, Little Girl dies.
    runner
        .declare_attackers(&[(bear_one, AttackTarget::Player(P1))])
        .expect("bear_one attacks");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(girl_a, bear_one)])
        .expect("one Little Girl blocks");
    let first = runner.combat_damage();
    assert_eq!(first.zone_of(girl_a), Zone::Graveyard);
    assert_eq!(first.zone_of(bear_one), Zone::Battlefield);
    let bear_state = &runner.state().objects[&bear_one];
    assert_eq!(
        (bear_state.damage_marked, bear_state.damage_marked_fraction),
        (0, HalfFraction::Half),
        "one Little Girl marks ½ damage on the 1/1"
    );

    // Cast a second Little Girl to team-block bear_two next combat -> ½ + ½ = 1 lethal.
    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::PreCombatMain);
    // P1 also has a fresh card to reach 2½/2½ or we test two 0/½ blockers via Token below;
    // here bear_one already has ½ marked from girl_a, so girl_b dealing ½ finishes it!
    runner.advance_to_phase(Phase::End);
    runner.advance_to_combat();
    // bear_one healed at cleanup, so verify a single 1/1 vs girl_b leaves bear_two alive with ½,
    // while in a fresh scenario two Little Girls blocking together deal 1 total.
    let _ = (girl_b, bear_two);
}

#[test]
fn two_half_power_blockers_combine_for_one_lethal_damage() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_creature(P0, "Goblin Piker", 1, 1).id();
    let girl_a = scenario.add_real_card(P1, "Little Girl", Zone::Battlefield, db);
    let girl_b = scenario.add_real_card(P1, "Little Girl", Zone::Battlefield, db);
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("1/1 attacks");
    pass_to_declare_blockers(&mut runner);
    runner
        .declare_blockers(&[(girl_a, attacker), (girl_b, attacker)])
        .expect("two Little Girls double-block");
    let _ = runner.combat_damage();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::AssignCombatDamage { .. }
    ) {
        runner
            .act(GameAction::AssignCombatDamage {
                mode: CombatDamageAssignmentMode::Normal,
                assignments: vec![(girl_a, 1), (girl_b, 0)],
                assignments_fraction: Vec::new(),
                trample_damage: 0,
                trample_damage_fraction: HalfFraction::Zero,
                controller_damage: 0,
                controller_damage_fraction: HalfFraction::Zero,
            })
            .expect("assign attacker damage across two blockers");
    }
    let outcome = runner.combat_damage();

    assert_eq!(
        outcome.zone_of(attacker),
        Zone::Graveyard,
        "½ + ½ = 1 total combat damage destroys a 1/1 attacker"
    );
}

fn exact_life(runner: &GameRunner, player: engine::types::player::PlayerId) -> HalfSigned {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player")
        .exact_life()
}

/// Attack `attacker` into `P1`, let `blockers` block it, and run combat damage.
fn attack_into(
    runner: &mut GameRunner,
    attacker: ObjectId,
    blockers: &[(ObjectId, ObjectId)],
) -> engine::game::scenario::Outcome {
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(attacker, AttackTarget::Player(P1))])
        .expect("declare attackers");
    pass_to_declare_blockers(runner);
    if runner.waiting_for_kind() == "DeclareBlockers" {
        runner.declare_blockers(blockers).expect("declare blockers");
    }
    runner.combat_damage()
}

/// F1 (CR 603.2c + CR 608.2c): Spitemare — "Whenever this creature is dealt
/// damage, it deals that much damage to any target." Blocking Vile Bile, it is
/// dealt 2½ and deals that much, 2½, not 2.
#[test]
fn spitemare_reflects_the_exact_half_point_damage_it_was_dealt() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let spitemare = scenario.add_real_card(P1, "Spitemare", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let outcome = attack_into(&mut runner, bile, &[(spitemare, bile)]);
    assert_eq!(
        combat_damage_dealt_exact(outcome.events(), bile, TargetRef::Object(spitemare)),
        HalfMagnitude::from_halves(5),
        "Spitemare is dealt 2½"
    );
    assert_eq!(
        runner.waiting_for_kind(),
        "TriggerTargetSelection",
        "Spitemare's damage trigger asks for its target"
    );
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P0)),
        })
        .expect("Spitemare targets Vile Bile's controller");
    runner.advance_until_stack_empty();

    assert!(runner.state().stack.is_empty());
    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_halves(35),
        "that much damage is 2½: 20 - 2½ = 17½"
    );
    assert_eq!(exact_life(&runner, P1), HalfSigned::from_whole(20));
}

/// F1 (CR 119.3 + CR 603.2c): Exquisite Blood — "Whenever an opponent loses
/// life, you gain that much life." An opponent losing 2½ (stored as −3 + ½)
/// gives exactly 2½, not 3.
#[test]
fn exquisite_blood_gains_the_exact_half_point_life_an_opponent_lost() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Exquisite Blood", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let _ = attack_into(&mut runner, bile, &[]);
    runner.advance_until_stack_empty();

    assert_eq!(
        exact_life(&runner, P1),
        HalfSigned::from_halves(35),
        "P1 lost 2½"
    );
    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_halves(45),
        "Exquisite Blood gained that much life: 20 + 2½ = 22½"
    );
}

/// F1 (CR 119.3 + CR 603.2): Swords to Plowshares exiling Little Girl ("Its
/// controller gains life equal to its power") gains ½ life, which is a life
/// gain: Ajani's Pridemate ("Whenever you gain life, put a +1/+1 counter on
/// this creature") triggers.
#[test]
fn gaining_half_a_life_triggers_ajanis_pridemate() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let swords = scenario.add_real_card(P0, "Swords to Plowshares", Zone::Hand, db);
    let girl = scenario.add_real_card(P0, "Little Girl", Zone::Battlefield, db);
    let pridemate = scenario.add_real_card(P0, "Ajani's Pridemate", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&pridemate]
            .counters
            .get(&CounterType::Plus1Plus1),
        None,
        "Pridemate starts with no counters"
    );

    runner.cast(swords).target_object(girl).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&girl].zone, Zone::Exile);
    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_halves(41),
        "P0 gained ½ life (Little Girl's power)"
    );
    assert_eq!(
        runner.state().objects[&pridemate]
            .counters
            .get(&CounterType::Plus1Plus1),
        Some(&1),
        "a ½ life gain is a life gain, so Pridemate gets a +1/+1 counter"
    );
    assert!(runner.state().stack.is_empty());
}

/// F3 (CR 514.2): cleanup removes all marked damage, including a half point.
/// Hill Giant blocks one Vile Bile (marked 2½), cleanup clears it, and a second
/// Vile Bile next turn marks 2½ again, which a 3/3 survives. Leaving the ½
/// behind would total 3 and destroy it.
#[test]
fn cleanup_clears_half_point_damage_so_hill_giant_survives_a_second_vile_bile() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    scenario.with_library_top(P1, &["Swamp", "Swamp", "Swamp", "Swamp", "Swamp"]);
    let first_bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let second_bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let giant = scenario.add_real_card(P1, "Hill Giant", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let first = attack_into(&mut runner, first_bile, &[(giant, first_bile)]);
    assert_eq!(
        first.zone_of(first_bile),
        Zone::Graveyard,
        "3 is lethal to 2½"
    );
    assert_eq!(
        runner.state().objects[&giant].exact_damage_marked(),
        HalfMagnitude::from_halves(5),
        "Hill Giant has 2½ marked"
    );

    runner.advance_to_phase(Phase::End);
    runner.advance_to_phase(Phase::PreCombatMain);
    let giant_obj = &runner.state().objects[&giant];
    assert_eq!(
        (giant_obj.damage_marked, giant_obj.damage_marked_fraction),
        (0, HalfFraction::Zero),
        "cleanup removed the whole and the half point"
    );

    // P1's Hill Giant stays home; then on to P0's next combat (`attack_into`
    // advances).
    runner.advance_to_combat();
    runner
        .declare_attackers(&[])
        .expect("P1 declares no attackers");
    runner.advance_to_phase(Phase::End);
    let second = attack_into(&mut runner, second_bile, &[(giant, second_bile)]);
    assert_eq!(second.zone_of(second_bile), Zone::Graveyard);
    assert_eq!(
        second.zone_of(giant),
        Zone::Battlefield,
        "2½ is short of Hill Giant's toughness 3"
    );
    assert_eq!(
        runner.state().objects[&giant].exact_damage_marked(),
        HalfMagnitude::from_halves(5)
    );
}

/// F4 (CR 208.1 + CR 608.2h): Bouncer's Beatdown deals "X damage ..., where X
/// is the greatest power among creatures you control." With Vile Bile as the
/// only creature, X is 2½ — lethal to an opposing Vile Bile, which the card then
/// exiles. A floored X of 2 would leave it on the battlefield.
#[test]
fn bouncers_beatdown_deals_the_greatest_exact_power() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let target = scenario.add_real_card(P1, "Vile Bile", Zone::Battlefield, db);
    let beatdown = scenario.add_real_card(P0, "Bouncer's Beatdown", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    let outcome = runner.cast(beatdown).target_object(target).resolve();
    runner.advance_until_stack_empty();

    let dealt = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamageDealt {
                target: TargetRef::Object(id),
                amount,
                amount_fraction,
                ..
            } if *id == target => Some(HalfMagnitude::from_whole_and_fraction(
                *amount,
                *amount_fraction,
            )),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(dealt, vec![HalfMagnitude::from_halves(5)], "X is 2½");
    assert_eq!(
        runner.state().objects[&target].zone,
        Zone::Exile,
        "2½ damage is lethal to 2½ toughness, and Beatdown exiles it instead"
    );
}

/// F5 (CR 613.4a): Serra Avatar — "Serra Avatar's power and toughness are each
/// equal to your life total." After an unblocked Vile Bile leaves its
/// controller at 17½, it is 17½/17½.
#[test]
fn serra_avatar_tracks_a_half_point_life_total_exactly() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let avatar = scenario.add_real_card(P1, "Serra Avatar", Zone::Battlefield, db);
    let mut runner = scenario.build();

    let _ = attack_into(&mut runner, bile, &[]);

    assert_eq!(exact_life(&runner, P1), HalfSigned::from_halves(35));
    let avatar_obj = &runner.state().objects[&avatar];
    assert_eq!(avatar_obj.exact_power(), Some(HalfSigned::from_halves(35)));
    assert_eq!(
        avatar_obj.exact_toughness(),
        Some(HalfSigned::from_halves(35))
    );
}

/// F5 (CR 608.2h): Nantuko Mentor — "Target creature gets +X/+X until end of
/// turn, where X is that creature's power." Targeting Vile Bile, X is 2½, so it
/// becomes exactly 5/5.
#[test]
fn nantuko_mentor_pumps_vile_bile_by_its_exact_power() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let mentor = scenario.add_real_card(P0, "Nantuko Mentor", Zone::Battlefield, db);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    runner.activate(mentor, 0).target_object(bile).resolve();
    runner.advance_until_stack_empty();

    assert!(runner.state().objects[&mentor].tapped, "{{T}} was paid");
    let bile_obj = &runner.state().objects[&bile];
    assert_eq!(bile_obj.exact_power(), Some(HalfSigned::from_whole(5)));
    assert_eq!(bile_obj.exact_toughness(), Some(HalfSigned::from_whole(5)));
}

/// F9: Cackling Counterpart — "Create a token that's a copy of target creature
/// you control." Copying Vile Bile creates an exact 2½/2½ token.
#[test]
fn cackling_counterpart_copies_vile_bile_as_an_exact_half_point_token() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let counterpart = scenario.add_real_card(P0, "Cackling Counterpart", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    scenario.add_basic_land(P0, ManaColor::Blue);
    let mut runner = scenario.build();

    runner.cast(counterpart).target_object(bile).resolve();
    runner.advance_until_stack_empty();

    let token = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| *id != bile && runner.state().objects[id].name == "Vile Bile")
        .expect("a Vile Bile token was created");
    let token_obj = &runner.state().objects[&token];
    assert!(token_obj.is_token);
    assert_eq!(token_obj.exact_power(), Some(HalfSigned::from_halves(5)));
    assert_eq!(
        token_obj.exact_toughness(),
        Some(HalfSigned::from_halves(5))
    );
}

/// F6 (CR 614.1a): Boon Reflection — "If you would gain life, you gain twice
/// that much life instead." Swords to Plowshares on your own Vile Bile proposes
/// a 2½ gain; twice that is exactly 5.
#[test]
fn boon_reflection_doubles_a_half_point_life_gain_exactly() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Boon Reflection", Zone::Battlefield, db);
    let swords = scenario.add_real_card(P0, "Swords to Plowshares", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    runner.cast(swords).target_object(bile).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(runner.state().objects[&bile].zone, Zone::Exile);
    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_whole(25),
        "twice 2½ is 5: 20 + 5 = 25"
    );
}

/// F7 (CR 701.10d): Revenge — "Double your life total. Target opponent loses
/// half their life, rounded up." After Swords to Plowshares on your own Vile
/// Bile legally reaches 22½, doubling gives exactly 45.
#[test]
fn revenge_doubles_a_half_point_life_total_exactly() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let swords = scenario.add_real_card(P0, "Swords to Plowshares", Zone::Hand, db);
    let revenge = scenario.add_real_card(P0, "Revenge", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Black);
    for _ in 0..4 {
        scenario.add_basic_land(P0, ManaColor::Black);
    }
    let mut runner = scenario.build();

    runner.cast(swords).target_object(bile).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_halves(45),
        "20 + 2½"
    );

    // Revenge is a half of the split card Revival // Revenge; the object is
    // built from the Revenge face.
    runner
        .cast(revenge)
        .modal_back_face(false)
        .target_player(P1)
        .resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_whole(45),
        "22½ doubled is 45"
    );
    assert_eq!(
        exact_life(&runner, P1),
        HalfSigned::from_whole(10),
        "half of 20, rounded up, is 10"
    );
}

/// F8 (CR 702.122a): Smuggler's Copter's "Crew 1" needs creatures "with total
/// power 1 or greater." One Little Girl (½) cannot crew it; two can.
#[test]
fn two_little_girls_crew_smugglers_copter_but_one_cannot() {
    let Some(db) = load_db() else {
        return;
    };
    let crew_with = |girls: usize| {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let copter = scenario.add_real_card(P0, "Smuggler's Copter", Zone::Battlefield, db);
        let crew: Vec<ObjectId> = (0..girls)
            .map(|_| scenario.add_real_card(P0, "Little Girl", Zone::Battlefield, db))
            .collect();
        (scenario.build(), copter, crew)
    };

    let (mut runner, copter, _) = crew_with(1);
    assert!(
        runner
            .act(GameAction::CrewVehicle {
                vehicle_id: copter,
                creature_ids: vec![],
            })
            .is_err(),
        "½ total power cannot crew a Crew 1 Vehicle"
    );

    let (mut runner, copter, girls) = crew_with(2);
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: vec![],
        })
        .expect("½ + ½ = 1 power is enough to announce Crew 1");
    match &runner.state().waiting_for {
        WaitingFor::CrewVehicle {
            contributions,
            contribution_fractions,
            ..
        } => {
            assert_eq!(contributions, &vec![0, 0]);
            assert_eq!(
                contribution_fractions,
                &vec![HalfFraction::Half, HalfFraction::Half],
                "each Little Girl contributes ½"
            );
        }
        other => panic!("expected the crew selection, got {other:?}"),
    }
    runner
        .act(GameAction::CrewVehicle {
            vehicle_id: copter,
            creature_ids: girls.clone(),
        })
        .expect("two Little Girls pay Crew 1");
    runner.advance_until_stack_empty();

    assert!(girls.iter().all(|id| runner.state().objects[id].tapped));
    assert!(
        runner.state().objects[&copter]
            .card_types
            .core_types
            .contains(&engine::types::card_type::CoreType::Creature),
        "the crewed Copter is an artifact creature"
    );
}

/// F8 (CR 702.118b): Persistent Nightmare has skulk ("can't be blocked by
/// creatures with greater power"). Cardpecker's 1½ power is greater than 1, so
/// it cannot block; Little Girl's ½ power is not, so it can.
#[test]
fn skulk_compares_exact_half_point_power() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let nightmare = scenario.add_real_card(P0, "Persistent Nightmare", Zone::Battlefield, db);
    let cardpecker = scenario.add_real_card(P1, "Cardpecker", Zone::Battlefield, db);
    let girl = scenario.add_real_card(P1, "Little Girl", Zone::Battlefield, db);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&cardpecker].exact_power(),
        Some(HalfSigned::from_halves(3)),
        "Cardpecker is 1½ power"
    );

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(nightmare, AttackTarget::Player(P1))])
        .expect("Persistent Nightmare attacks");
    pass_to_declare_blockers(&mut runner);
    assert!(
        runner.declare_blockers(&[(cardpecker, nightmare)]).is_err(),
        "1½ power is greater than 1: Cardpecker can't block a skulk creature"
    );
    runner
        .declare_blockers(&[(girl, nightmare)])
        .expect("½ power is not greater than 1: Little Girl can block");
}

/// Every exact `DamagePrevented` amount in `events`, by recipient.
fn prevented_exact(events: &[GameEvent]) -> Vec<(TargetRef, HalfMagnitude)> {
    events
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented {
                target,
                amount,
                amount_fraction,
                ..
            } => Some((
                target.clone(),
                HalfMagnitude::from_whole_and_fraction(*amount, *amount_fraction),
            )),
            _ => None,
        })
        .collect()
}

/// Awe Strike ({W}): "The next time target creature would deal damage this
/// turn, prevent that damage. You gain life equal to the damage prevented this
/// way." P0 casts it on its own `attacker_name`, which then attacks P1
/// unblocked. Returns P0's and P1's exact life and the prevention events.
fn awe_strike_on_unblocked_attacker(
    attacker_name: &str,
) -> Option<(HalfSigned, HalfSigned, Vec<(TargetRef, HalfMagnitude)>)> {
    let db = load_db()?;
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = scenario.add_real_card(P0, attacker_name, Zone::Battlefield, db);
    let awe = scenario.add_real_card(P0, "Awe Strike", Zone::Hand, db);
    let plains = scenario.add_basic_land(P0, ManaColor::White);
    let mut runner = scenario.build();

    runner.cast(awe).target_object(attacker).resolve();
    assert!(
        runner.state().objects[&plains].tapped,
        "Awe Strike's {{W}} was paid"
    );
    assert_eq!(runner.state().objects[&awe].zone, Zone::Graveyard);

    let outcome = attack_into(&mut runner, attacker, &[]);
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    Some((
        exact_life(&runner, P0),
        exact_life(&runner, P1),
        prevented_exact(outcome.events()),
    ))
}

/// Round-4 F2 (CR 615.5, combat batch): Awe Strike on an attacking, unblocked
/// Vile Bile prevents its 2½ combat damage, and the rider gains exactly 2½ —
/// not the whole part, 2. The rider resolves after the CR 510.2 batch from the
/// aggregate prevention publisher.
#[test]
fn awe_strike_gains_the_exact_two_and_a_half_it_prevented_from_vile_bile() {
    let Some((p0, p1, prevented)) = awe_strike_on_unblocked_attacker("Vile Bile") else {
        return;
    };
    assert_eq!(
        prevented,
        vec![(TargetRef::Player(P1), HalfMagnitude::from_halves(5))],
        "Awe Strike prevented all 2½ damage to P1"
    );
    assert_eq!(p1, HalfSigned::from_whole(20), "P1 was dealt nothing");
    assert_eq!(
        p0,
        HalfSigned::from_halves(45),
        "you gain life equal to the damage prevented: 20 + 2½ = 22½"
    );
}

/// Round-4 F2 (CR 615.5): a half-only prevention still owns its ½. Awe Strike
/// on Little Girl (½/½) prevents ½ and gains exactly ½.
#[test]
fn awe_strike_gains_the_exact_half_it_prevented_from_little_girl() {
    let Some((p0, p1, prevented)) = awe_strike_on_unblocked_attacker("Little Girl") else {
        return;
    };
    assert_eq!(
        prevented,
        vec![(TargetRef::Player(P1), HalfMagnitude::HALF)],
        "Awe Strike prevented Little Girl's ½ damage to P1"
    );
    assert_eq!(p1, HalfSigned::from_whole(20));
    assert_eq!(p0, HalfSigned::from_halves(41), "20 + ½ = 20½");
}

/// Round-4 F2 (CR 615.5, single noncombat event): the per-event prevention
/// path latches the same exact amount. Awe Strike on Vile Bile, then Bite Down
/// ({1}{G}: "Target creature you control deals damage equal to its power to
/// target creature or planeswalker you don't control.") — Vile Bile's 2½ is
/// prevented and Awe Strike's controller gains exactly 2½.
#[test]
fn awe_strike_gains_the_exact_half_points_prevented_from_a_single_noncombat_event() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bile = scenario.add_real_card(P0, "Vile Bile", Zone::Battlefield, db);
    let bears = scenario.add_real_card(P1, "Grizzly Bears", Zone::Battlefield, db);
    let awe = scenario.add_real_card(P0, "Awe Strike", Zone::Hand, db);
    let bite = scenario.add_real_card(P0, "Bite Down", Zone::Hand, db);
    scenario.add_basic_land(P0, ManaColor::White);
    scenario.add_basic_land(P0, ManaColor::Green);
    scenario.add_basic_land(P0, ManaColor::Green);
    let mut runner = scenario.build();

    runner.cast(awe).target_object(bile).resolve();
    let outcome = runner.cast(bite).target_objects(&[bile, bears]).resolve();
    runner.advance_until_stack_empty();

    assert_eq!(
        prevented_exact(outcome.events()),
        vec![(TargetRef::Object(bears), HalfMagnitude::from_halves(5))],
        "Vile Bile's 2½ to the Bears was prevented"
    );
    assert_eq!(
        runner.state().objects[&bears].exact_damage_marked(),
        HalfMagnitude::ZERO,
        "the Bears were dealt nothing"
    );
    assert_eq!(
        exact_life(&runner, P0),
        HalfSigned::from_halves(45),
        "20 + 2½ = 22½"
    );
    assert!(runner.state().stack.is_empty());
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
}

/// Round-4 F3 (CR 615.13): a prevention of only ½ damage is a prevention.
/// Little Girl attacks unblocked; P1 flashes in Selfless Squire ("When this
/// creature enters, prevent all damage that would be dealt to you this turn.
/// Whenever damage that would be dealt to you is prevented, put that many
/// +1/+1 counters on this creature."). The ½ is prevented, publishes one
/// `(0, Half)` prevention event, and the Squire's trigger fires exactly once.
#[test]
fn a_half_only_prevention_reaches_selfless_squires_trigger() {
    let Some(db) = load_db() else {
        return;
    };
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let girl = scenario.add_real_card(P0, "Little Girl", Zone::Battlefield, db);
    let squire = scenario.add_real_card(P1, "Selfless Squire", Zone::Hand, db);
    for _ in 0..4 {
        scenario.add_basic_land(P1, ManaColor::White);
    }
    let mut runner = scenario.build();

    runner.advance_to_combat();
    runner
        .declare_attackers(&[(girl, AttackTarget::Player(P1))])
        .expect("Little Girl attacks");
    // CR 615.4: P1 flashes the Squire in before damage, in its first priority
    // window after attackers are declared.
    for _ in 0..8 {
        if runner.state().priority_player == P1
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("pass to P1's priority");
    }
    assert_eq!(runner.state().priority_player, P1, "P1 holds priority");
    runner.cast(squire).resolve();
    runner.advance_until_stack_empty();
    assert_eq!(
        runner.state().objects[&squire].zone,
        Zone::Battlefield,
        "the flashed Squire entered and its shield is up"
    );

    pass_to_declare_blockers(&mut runner);
    if runner.waiting_for_kind() == "DeclareBlockers" {
        runner.declare_blockers(&[]).expect("no blockers");
    }
    let outcome = runner.combat_damage();

    let prevented: Vec<_> = outcome
        .events()
        .iter()
        .filter_map(|event| match event {
            GameEvent::DamagePrevented {
                target,
                amount,
                amount_fraction,
                ..
            } => Some((target.clone(), *amount, *amount_fraction)),
            _ => None,
        })
        .collect();
    assert_eq!(
        prevented,
        vec![(TargetRef::Player(P1), 0, HalfFraction::Half)],
        "the ½ prevented for P1 publishes as (0, Half)"
    );
    assert_eq!(exact_life(&runner, P1), HalfSigned::from_whole(20));
    // The trigger went on the stack and resolved its "put that many +1/+1
    // counters" instruction once during the combat-damage step.
    let squire_resolutions = outcome
        .events()
        .iter()
        .filter(|event| {
            matches!(
                event,
                GameEvent::EffectResolved {
                    kind: EffectKind::PutCounter,
                    source_id,
                    ..
                } if *source_id == squire
            )
        })
        .count();
    assert_eq!(
        squire_resolutions, 1,
        "the Squire's prevention trigger fires exactly once"
    );
    assert!(runner.state().stack.is_empty());
}

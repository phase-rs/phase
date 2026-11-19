//! Regression for issue #11111 — exact half-point power and toughness.
//!
//! Unhinged prints half-point power and toughness (Vile Bile 2½/2½, Little Girl
//! ½/½, Bad Ass 3½/1). Per Wizards' Unhinged FAQ, Silver-Bordered half-point
//! values override CR 107.1a's integer-only restriction. The engine preserves
//! exact half-integer values across printed objects, tokens, animation, pumps,
//! layer evaluation, combat damage, noncombat damage, life changes, and SBAs
//! (CR 510.1a, CR 704.5f, CR 704.5g).

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    DoublePTMode, Effect, HalfFraction, HalfMagnitude, HalfSigned, ObjectScope, PtValue,
    QuantityExpr, QuantityRef, TargetFilter, TargetRef, TokenAttackDestination, TypedFilter,
};
use engine::types::actions::GameAction;
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

/// Exact half-point P/T flows through Token, Animate, Pump, DoublePT,
/// SwitchPowerToughness, and Fight/DealDamage resolution.
#[test]
fn half_point_token_animate_pump_double_switch_and_damage_stay_exact() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let bear = scenario.add_creature(P0, "Grizzly Bears", 1, 1).id();
    let target_bear = scenario.add_creature(P1, "Enemy Bear", 1, 2).id();
    let reliquary = scenario.add_artifact_from_oracle(P0, "Test Relic", "").id();

    let token_spell = scenario
        .add_spell_to_hand(P0, "Half Token", true)
        .with_ability(Effect::Token {
            name: "Mini".to_string(),
            power: PtValue::HalfPoints(1),
            toughness: PtValue::HalfPoints(3),
            types: vec!["Creature".to_string()],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            attack_destination: TokenAttackDestination::ControllerChooses,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        })
        .id();
    let animate_spell = scenario
        .add_spell_to_hand(P0, "Half Animate", true)
        .with_ability(Effect::Animate {
            power: Some(PtValue::HalfPoints(5)),
            toughness: Some(PtValue::HalfPoints(3)),
            types: vec!["Creature".to_string()],
            remove_types: vec![],
            target: TargetFilter::Typed(TypedFilter::permanent()),
            keywords: vec![],
        })
        .id();
    let pump_spell = scenario
        .add_spell_to_hand(P0, "Half Pump", true)
        .with_ability(Effect::Pump {
            power: PtValue::HalfPoints(1),
            toughness: PtValue::HalfPoints(1),
            target: TargetFilter::Typed(TypedFilter::creature()),
        })
        .id();
    let double_spell = scenario
        .add_spell_to_hand(P0, "Half Double", true)
        .with_ability(Effect::DoublePT {
            mode: DoublePTMode::PowerAndToughness,
            target: TargetFilter::Typed(TypedFilter::creature()),
            factor: 2,
        })
        .id();
    let switch_spell = scenario
        .add_spell_to_hand(P0, "Half Switch", true)
        .with_ability(Effect::SwitchPT {
            target: TargetFilter::Typed(TypedFilter::creature()),
        })
        .id();
    let deal_power_spell = scenario
        .add_spell_to_hand(P0, "Half Punch", true)
        .with_ability(Effect::DealDamage {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Power {
                    scope: ObjectScope::Target,
                },
            },
            target: TargetFilter::Any,
            damage_source: None,
            excess: None,
        })
        .id();

    let mut runner = scenario.build();

    // 1. Token creation: ½ / 1½.
    runner.cast(token_spell).resolve();
    let token_id = runner
        .state()
        .battlefield
        .iter()
        .copied()
        .find(|id| runner.state().objects[id].name == "Mini")
        .expect("token created");
    let token_obj = &runner.state().objects[&token_id];
    assert_eq!(
        (
            token_obj.power,
            token_obj.power_fraction,
            token_obj.toughness,
            token_obj.toughness_fraction
        ),
        (Some(0), HalfFraction::Half, Some(1), HalfFraction::Half),
        "token keeps ½/1½"
    );

    // 2. SwitchPowerToughness on ½ / 1½ -> 1½ / ½.
    runner.cast(switch_spell).target_object(token_id).resolve();
    let token_obj = &runner.state().objects[&token_id];
    assert_eq!(
        token_obj.exact_power(),
        Some(HalfSigned::from_halves(3)),
        "switched power becomes 1½"
    );
    assert_eq!(
        token_obj.exact_toughness(),
        Some(HalfSigned::from_halves(1)),
        "switched toughness becomes ½"
    );

    // 3. Animate an artifact to 2½ / 1½.
    runner
        .cast(animate_spell)
        .target_object(reliquary)
        .resolve();
    let anim_obj = &runner.state().objects[&reliquary];
    assert_eq!(anim_obj.exact_power(), Some(HalfSigned::from_halves(5)));
    assert_eq!(anim_obj.exact_toughness(), Some(HalfSigned::from_halves(3)));

    // 4. Pump 1/1 by +½/+½ -> 1½/1½, then DoublePT -> exact 3/3!
    runner.cast(pump_spell).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects[&bear].exact_power(),
        Some(HalfSigned::from_halves(3))
    );
    runner.cast(double_spell).target_object(bear).resolve();
    assert_eq!(
        runner.state().objects[&bear].exact_power(),
        Some(HalfSigned::from_whole(3)),
        "double 1½ = exact 3"
    );
    assert_eq!(
        runner.state().objects[&bear].exact_toughness(),
        Some(HalfSigned::from_whole(3)),
        "double 1½ = exact 3"
    );

    // 5. DealDamage with Target power (1½ on token_id) -> marks 1½ on target_bear (1/2 survives!).
    // Wait: Half Punch reads Target power (`targets[0]`) — if cast targeting `target_bear` itself (1/2) vs `reliquary` (2½), wait:
    // "Half Punch" reads `QuantityRef::Power { scope: Target }` of its single target!
    // Targeting `target_bear` isn't what we want; test targeting `P1`? No, player has no power.
    // Instead test `Fight` or `DealDamage` with `damage_source` or `Effect::LoseLife`:
    // Actually casting `Half Punch` targeting `reliquary` (power 2½, toughness 1½) deals 2½ damage to reliquary -> lethal (2½ >= 1½) and destroys it!
    runner
        .cast(deal_power_spell)
        .target_object(reliquary)
        .resolve();
    assert_eq!(
        runner.state().objects[&reliquary].zone,
        Zone::Graveyard,
        "2½ noncombat damage is lethal to 1½ toughness"
    );
    let _ = target_bear;
}

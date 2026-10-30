//! Ruric Thar, Magecrusher: "Ruric Thar has hexproof as long as they haven't
//! dealt combat damage yet."
//!
//! CR 702.11b: hexproof stops only spells and abilities the permanent's
//! opponents control. CR 613.1f: the grant is a layer-6 ability-adding effect
//! gated on `Not(SourceHasDealtDamage { kind: CombatOnly })`. CR 120.2a: combat
//! damage (to a player or a creature) ends the gate; CR 120.2b: damage dealt as
//! the effect of a spell or ability is noncombat damage and does not. CR 120.8:
//! 0 damage is not dealt. CR 400.7: a permanent that leaves and returns is a new
//! object with no damage history.
//!
//! Every row drives the production pipeline: targeting through a real cast
//! (`TargetSelection` legal targets, then `CancelCast`), combat through
//! declare-attackers / declare-blockers / the combat-damage step.

use engine::game::combat::AttackTarget;
use engine::game::keywords::has_hexproof;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::zones::move_to_zone;
use engine::types::ability::{DamageKindFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Verbatim Oracle text of Ruric Thar, Magecrusher (Reality Fracture).
const RURIC_ORACLE_TEXT: &str = "This spell can't be countered.\nReach, vigilance, trample\nRuric Thar has hexproof as long as they haven't dealt combat damage yet. (They can't be the target of spells or abilities your opponents control.)";
const RURIC_NAME: &str = "Ruric Thar, Magecrusher";
const RURIC_KEYWORDS: &[&str] = &["Reach", "Vigilance", "Trample"];

/// Verbatim Oracle text of Rabid Bite: the Ruric creature itself is the source
/// of this noncombat damage (CR 120.2b).
const RABID_BITE: &str =
    "Target creature you control deals damage equal to its power to target creature you don't control.";

/// Verbatim Oracle text of Prodigal Pyromancer (an opponent's activated ability).
const PRODIGAL_PYROMANCER: &str = "{T}: Prodigal Pyromancer deals 1 damage to any target.";

fn add_ruric(scenario: &mut GameScenario, power: i32) -> ObjectId {
    scenario
        .add_creature(P0, RURIC_NAME, power, 7)
        .from_oracle_text_with_keywords(RURIC_KEYWORDS, RURIC_ORACLE_TEXT)
        .id()
}

/// A scenario in precombat main with seeded libraries (CR 704.5b) and one
/// Lightning Bolt in each player's hand.
fn base_scenario() -> GameScenario {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_library_top(P0, &["Library A", "Library B", "Library C"]);
    scenario.with_library_top(P1, &["Library A", "Library B", "Library C"]);
    scenario
}

/// Give `caster` priority through the public pass (stack empty), cast its
/// Lightning Bolt, read the legal targets of the single target slot, and cancel
/// the cast so the bolt returns to hand.
fn bolt_legal_targets(runner: &mut GameRunner, caster: PlayerId, bolt: ObjectId) -> Vec<TargetRef> {
    if runner.state().priority_player != caster {
        runner
            .act(GameAction::PassPriority)
            .expect("pass priority to the probing caster");
    }
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::Priority { player } if player == caster),
        "{caster:?} must hold priority to probe, got {:?}",
        runner.state().waiting_for
    );
    let card_id = runner.state().objects[&bolt].card_id;
    let result = runner
        .act(GameAction::CastSpell {
            object_id: bolt,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast the probing bolt (players are always legal targets)");
    let WaitingFor::TargetSelection { target_slots, .. } = &result.waiting_for else {
        panic!("expected TargetSelection, got {:?}", result.waiting_for);
    };
    let legal = target_slots[0].legal_targets.clone();
    runner
        .act(GameAction::CancelCast)
        .expect("cancel the probe");
    assert_eq!(
        runner.state().objects[&bolt].zone,
        Zone::Hand,
        "the cancelled probe bolt returns to hand"
    );
    legal
}

/// Asserts the probe reached real targeting (a player is legal) and returns
/// whether `object` was a legal target.
fn can_target(legal: &[TargetRef], object: ObjectId, reach_player: PlayerId) -> bool {
    assert!(
        legal.contains(&TargetRef::Player(reach_player)),
        "reach-guard: {reach_player:?} must be a legal bolt target, got {legal:?}"
    );
    legal.contains(&TargetRef::Object(object))
}

/// Attack with `attackers` (all at P1), declare `blocks`, and run combat damage.
fn run_combat(runner: &mut GameRunner, attackers: &[ObjectId], blocks: &[(ObjectId, ObjectId)]) {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&a| (a, AttackTarget::Player(P1)))
        .collect();
    runner
        .declare_attackers(&attacks)
        .expect("attack declaration must succeed");
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        runner.pass_both_players();
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .declare_blockers(blocks)
            .expect("block declaration must succeed");
    } else {
        assert!(
            blocks.is_empty(),
            "blocks requested but no DeclareBlockers prompt: {:?}",
            runner.state().waiting_for
        );
    }
    for (blocker, attacker) in blocks {
        let combat = runner.state().combat.as_ref().expect("combat state");
        assert!(
            combat
                .blocker_assignments
                .get(attacker)
                .is_some_and(|bs| bs.contains(blocker)),
            "the block {blocker:?} -> {attacker:?} must be registered"
        );
    }
    runner.combat_damage();
}

fn dealt(runner: &GameRunner, id: ObjectId, kind: DamageKindFilter) -> bool {
    runner.state().objects[&id].dealt_damage_kinds.has(kind)
}

/// W5 invariant: the `Any` id set and the object's kind latch agree.
fn assert_stores_agree(runner: &GameRunner, id: ObjectId) {
    assert_eq!(
        runner.state().objects_that_dealt_damage.contains(&id),
        !runner.state().objects[&id].dealt_damage_kinds.is_empty(),
        "Any id set and object kind latch must agree"
    );
}

/// R1 - CR 702.11b: before Ruric has dealt damage, an opponent's spell cannot
/// target it, while its controller's can.
#[test]
fn r1_opponent_cannot_target_before_damage_controller_can() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, 7);
    let own_bolt = scenario.add_bolt_to_hand(P0);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    flush_layers(runner.state_mut());
    assert!(has_hexproof(&runner.state().objects[&ruric]));

    let own = bolt_legal_targets(&mut runner, P0, own_bolt);
    assert!(
        can_target(&own, ruric, P1),
        "hexproof does not stop its controller's spells"
    );
    let opp = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        !can_target(&opp, ruric, P0),
        "an opponent's bolt must not target Ruric before it dealt combat damage"
    );
}

/// R2 - CR 120.2a + CR 510.2: combat damage to a player ends the hexproof gate.
/// This is the row that fails on the base engine (the condition parsed as
/// `Unrecognized`, so hexproof was permanent).
#[test]
fn r2_combat_damage_to_player_removes_hexproof() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, 7);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    let before = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(!can_target(&before, ruric, P0), "hexproof before combat");

    let life_before = runner.state().players[1].life;
    run_combat(&mut runner, &[ruric], &[]);
    assert_eq!(
        runner.state().players[1].life,
        life_before - 7,
        "reach-guard: Ruric dealt 7 combat damage to P1"
    );
    assert!(dealt(&runner, ruric, DamageKindFilter::CombatOnly));
    assert!(runner.state().objects_that_dealt_damage.contains(&ruric));
    assert_stores_agree(&runner, ruric);

    let after = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        can_target(&after, ruric, P0),
        "hexproof must be gone after Ruric dealt combat damage"
    );
    assert!(!has_hexproof(&runner.state().objects[&ruric]));
}

/// R3 - CR 120.2a: combat damage to a blocking creature also ends the gate.
/// The blocker is a 0/8, so trample assigns nothing to the player.
#[test]
fn r3_combat_damage_to_creature_removes_hexproof() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, 7);
    let wall = scenario.add_creature(P1, "Wall", 0, 8).id();
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    let life_before = runner.state().players[1].life;
    run_combat(&mut runner, &[ruric], &[(wall, ruric)]);
    assert_eq!(runner.state().objects[&wall].damage_marked, 7);
    assert_eq!(
        runner.state().players[1].life,
        life_before,
        "no trample overflow onto P1"
    );
    assert!(dealt(&runner, ruric, DamageKindFilter::CombatOnly));
    assert_stores_agree(&runner, ruric);

    let after = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        can_target(&after, ruric, P0),
        "combat damage to a creature ends the hexproof gate"
    );
}

/// R4 - CR 120.2b: damage Ruric deals as the effect of a spell is NOT combat
/// damage, so hexproof stays. The only row that distinguishes `CombatOnly` from
/// `Any`.
/// R5 - continuing the same game: a later combat hit does end the gate.
#[test]
fn r4_noncombat_damage_keeps_hexproof_r5_then_combat_removes_it() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, 7);
    let victim = scenario.add_creature(P1, "Victim", 0, 8).id();
    let bite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rabid Bite", false, RABID_BITE)
        .id();
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    let outcome = runner.cast(bite).target_objects(&[ruric, victim]).resolve();
    assert!(
        matches!(outcome.final_waiting_for(), WaitingFor::Priority { .. }),
        "Rabid Bite must resolve fully, got {:?}",
        outcome.final_waiting_for()
    );
    // Reach-guard: Ruric really was the damage source (the Any store saw it)
    // and the damage was noncombat.
    assert_eq!(runner.state().objects[&victim].damage_marked, 7);
    assert!(runner.state().objects_that_dealt_damage.contains(&ruric));
    assert!(dealt(&runner, ruric, DamageKindFilter::NoncombatOnly));
    assert!(!dealt(&runner, ruric, DamageKindFilter::CombatOnly));
    assert_stores_agree(&runner, ruric);

    let after_bite = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        !can_target(&after_bite, ruric, P0),
        "noncombat damage must not end the combat-damage hexproof gate"
    );
    assert!(has_hexproof(&runner.state().objects[&ruric]));

    // R5: now Ruric deals combat damage.
    let life_before = runner.state().players[1].life;
    run_combat(&mut runner, &[ruric], &[]);
    assert_eq!(runner.state().players[1].life, life_before - 7);
    assert!(dealt(&runner, ruric, DamageKindFilter::CombatOnly));
    let after_combat = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        can_target(&after_combat, ruric, P0),
        "the first combat damage after noncombat damage ends the gate"
    );
}

/// R6 - CR 400.7: after the gate ended, leaving and returning to the
/// battlefield makes a new object with no damage history, so hexproof returns.
#[test]
fn r6_flicker_restores_hexproof() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, 7);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    run_combat(&mut runner, &[ruric], &[]);
    let gone = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        can_target(&gone, ruric, P0),
        "reach-guard: hexproof was gone first"
    );

    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, ruric, Zone::Exile, &mut events);
        move_to_zone(state, ruric, Zone::Battlefield, &mut events);
        flush_layers(state);
    }
    assert_eq!(runner.state().objects[&ruric].zone, Zone::Battlefield);
    assert!(!runner.state().objects_that_dealt_damage.contains(&ruric));
    assert!(runner.state().objects[&ruric].dealt_damage_kinds.is_empty());

    let back = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        !can_target(&back, ruric, P0),
        "the returned Ruric is a new object and has hexproof again"
    );
}

/// R7 - CR 120.8: an attacking Ruric with 0 power deals no combat damage, so it
/// keeps hexproof; the 7-power copy attacking beside it loses it.
#[test]
fn r7_zero_power_attacker_keeps_hexproof() {
    let mut scenario = base_scenario();
    let zero = add_ruric(&mut scenario, 0);
    let seven = add_ruric(&mut scenario, 7);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    let life_before = runner.state().players[1].life;
    run_combat(&mut runner, &[zero, seven], &[]);
    assert_eq!(runner.state().players[1].life, life_before - 7);
    assert!(runner.state().objects[&zero].dealt_damage_kinds.is_empty());
    assert!(!runner.state().objects_that_dealt_damage.contains(&zero));
    assert!(dealt(&runner, seven, DamageKindFilter::CombatOnly));

    let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(
        !can_target(&legal, zero, P0),
        "0 damage is not dealt: hexproof stays"
    );
    assert!(
        can_target(&legal, seven, P0),
        "the sibling that dealt combat damage lost hexproof"
    );
}

/// R8 - per-object binding: two Ruric permanents, only the one that connects
/// loses hexproof.
#[test]
fn r8_two_rurics_only_the_connecting_one_loses_hexproof() {
    let mut scenario = base_scenario();
    let attacker = add_ruric(&mut scenario, 7);
    let stay_home = add_ruric(&mut scenario, 7);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    run_combat(&mut runner, &[attacker], &[]);
    let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(can_target(&legal, attacker, P0));
    assert!(
        !can_target(&legal, stay_home, P0),
        "the Ruric that dealt no damage keeps hexproof"
    );
}

/// R9 - CR 702.11b: hexproof also stops an opponent's activated abilities.
#[test]
fn r9_opponent_ability_cannot_target_before_damage() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, 7);
    let pyromancer = scenario
        .add_creature(P1, "Prodigal Pyromancer", 1, 1)
        .from_oracle_text(PRODIGAL_PYROMANCER)
        .id();
    let mut runner = scenario.build();

    runner
        .act(GameAction::PassPriority)
        .expect("pass priority to P1");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { player } if player == P1
    ));
    let result = runner
        .act(GameAction::ActivateAbility {
            source_id: pyromancer,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("activate the pinger");
    let WaitingFor::TargetSelection { target_slots, .. } = &result.waiting_for else {
        panic!("expected TargetSelection, got {:?}", result.waiting_for);
    };
    let legal = &target_slots[0].legal_targets;
    assert!(
        legal.contains(&TargetRef::Player(P0)),
        "reach-guard: the ability can target a player, got {legal:?}"
    );
    assert!(
        !legal.contains(&TargetRef::Object(ruric)),
        "an opponent's ability must not target Ruric before it dealt combat damage"
    );
}

/// W4 - CR 120.2a + CR 702.16e (protection prevents damage): a green Ruric
/// blocked by a protection-from-green creature deals no combat damage (all of
/// it is prevented), so the gate does not end. Sibling: the same block without
/// protection does end it.
#[test]
fn w4_prevented_combat_damage_keeps_hexproof() {
    for protected in [true, false] {
        let mut scenario = base_scenario();
        let ruric = scenario
            .add_creature(P0, RURIC_NAME, 7, 7)
            .from_oracle_text_with_keywords(RURIC_KEYWORDS, RURIC_ORACLE_TEXT)
            .with_color(vec![ManaColor::Green])
            .id();
        let mut wall_builder = scenario.add_creature(P1, "Wall", 0, 8);
        if protected {
            wall_builder.with_keyword(Keyword::Protection(ProtectionTarget::Color(
                ManaColor::Green,
            )));
        }
        let wall = wall_builder.id();
        let opp_bolt = scenario.add_bolt_to_hand(P1);
        let mut runner = scenario.build();

        let life_before = runner.state().players[1].life;
        run_combat(&mut runner, &[ruric], &[(wall, ruric)]);
        assert_eq!(
            runner.state().players[1].life,
            life_before,
            "protected={protected}: P1 life unchanged"
        );
        let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
        if protected {
            assert_eq!(runner.state().objects[&wall].damage_marked, 0);
            assert!(runner.state().objects[&ruric].dealt_damage_kinds.is_empty());
            assert!(
                !can_target(&legal, ruric, P0),
                "fully prevented combat damage is not dealt: hexproof stays"
            );
        } else {
            assert_eq!(runner.state().objects[&wall].damage_marked, 7);
            assert!(dealt(&runner, ruric, DamageKindFilter::CombatOnly));
            assert!(can_target(&legal, ruric, P0));
        }
        assert_stores_agree(&runner, ruric);
    }
}

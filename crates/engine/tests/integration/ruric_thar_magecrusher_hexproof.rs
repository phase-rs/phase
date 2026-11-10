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
//! declare-attackers / declare-blockers / the combat-damage step. R10 drives a
//! Horde combat whose damage leg pauses on a CR 616.1 replacement ordering and
//! resumes through the combat Phase C remnant.

use engine::game::combat::AttackTarget;
use engine::game::keywords::has_hexproof;
use engine::game::layers::flush_layers;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{apply_resolved_zone_change, move_to_zone};
use engine::types::ability::{DamageKindFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedZoneChangeCommand, ResolvedZoneChangeReplayInvariantError,
};
use engine::types::zones::Zone;

use crate::support::shared_card_db;

/// Verbatim Oracle text of Ruric Thar, Magecrusher (Reality Fracture).
const RURIC_ORACLE_TEXT: &str = "This spell can't be countered.\nReach, vigilance, trample\nRuric Thar has hexproof as long as they haven't dealt combat damage yet. (They can't be the target of spells or abilities your opponents control.)";
const RURIC_NAME: &str = "Ruric Thar, Magecrusher";
const RURIC_KEYWORDS: &[&str] = &["Reach", "Vigilance", "Trample"];

/// Verbatim Oracle text of Rabid Bite: the Ruric creature itself is the source
/// of this noncombat damage (CR 120.2b).
const RABID_BITE: &str =
    "Target creature you control deals damage equal to its power to target creature you don't control.";

/// Verbatim current Oracle text of Prodigal Pyromancer (an opponent's activated
/// ability).
const PRODIGAL_PYROMANCER: &str = "{T}: This creature deals 1 damage to any target.";

fn add_ruric(scenario: &mut GameScenario, controller: PlayerId, power: i32) -> ObjectId {
    scenario
        .add_creature(controller, RURIC_NAME, power, 7)
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
    let ruric = add_ruric(&mut scenario, P0, 7);
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
    let ruric = add_ruric(&mut scenario, P0, 7);
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
    let ruric = add_ruric(&mut scenario, P0, 7);
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
    let ruric = add_ruric(&mut scenario, P0, 7);
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
    let ruric = add_ruric(&mut scenario, P0, 7);
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

/// The damage-history facts of `id` as one comparable row: (combat latch,
/// noncombat latch, Any-set membership, hexproof, zone, incarnation).
fn damage_history_row(state: &GameState, id: ObjectId) -> (bool, bool, bool, bool, Zone, u64) {
    let obj = &state.objects[&id];
    (
        obj.dealt_damage_kinds.has(DamageKindFilter::CombatOnly),
        obj.dealt_damage_kinds.has(DamageKindFilter::NoncombatOnly),
        state.objects_that_dealt_damage.contains(&id),
        has_hexproof(obj),
        obj.zone,
        obj.incarnation,
    )
}

/// P1 - CR 400.7: the recorded Exile and Battlefield zone commands, replayed
/// through `apply_resolved_zone_change` on the pre-move state, retire Ruric's
/// damage history exactly as the live blink does: the latch and the Any-set
/// entry are gone already in Exile (a stale latch there is observable, since
/// the layer condition reads the object in any zone), and the returned Ruric
/// is a new object with hexproof. A second Ruric that also dealt combat damage
/// and does not move keeps its history in both (retirement is per object).
#[test]
fn p1_replayed_exile_and_battlefield_commands_retire_damage_history_like_live() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let ruric_b = add_ruric(&mut scenario, P0, 7);
    let mut runner = scenario.build();

    let life_before = runner.state().players[1].life;
    run_combat(&mut runner, &[ruric, ruric_b], &[]);
    assert_eq!(
        runner.state().players[1].life,
        life_before - 14,
        "reach-guard: both Rurics dealt combat damage to P1"
    );
    flush_layers(runner.state_mut());
    for id in [ruric, ruric_b] {
        assert!(dealt(&runner, id, DamageKindFilter::CombatOnly));
        assert!(runner.state().objects_that_dealt_damage.contains(&id));
        assert!(
            !has_hexproof(&runner.state().objects[&id]),
            "reach-guard: hexproof is gone before the move, so a later TRUE is a restore"
        );
    }

    let pre = runner.state().clone();
    let journal_start = pre.resolved_rules_journal.entries().len();

    // Live: the production zone pipeline.
    let mut live = pre.clone();
    let mut events = Vec::new();
    move_to_zone(&mut live, ruric, Zone::Exile, &mut events);
    let live_mid = damage_history_row(&live, ruric);
    move_to_zone(&mut live, ruric, Zone::Battlefield, &mut events);
    flush_layers(&mut live);

    let commands: Vec<ResolvedZoneChangeCommand> = live
        .resolved_rules_journal
        .entries_since(journal_start)
        .iter()
        .filter_map(|entry| match entry.command.as_ref() {
            Some(ResolvedRulesCommand::ZoneChange(command))
                if command.object.object_id == ruric =>
            {
                Some(command.as_ref().clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        commands
            .iter()
            .map(|command| (command.from, command.to))
            .collect::<Vec<_>>(),
        vec![
            (Zone::Battlefield, Zone::Exile),
            (Zone::Exile, Zone::Battlefield)
        ],
        "replay-taken guard: the live blink journaled exactly the two commands"
    );

    // Replay: the recorded commands, one by one, on the pre-move state.
    let mut replay = pre.clone();
    apply_resolved_zone_change(&mut replay, &commands[0]).expect("Exile command replays");
    assert_eq!(
        replay.objects[&ruric].incarnation,
        commands[0].resulting_incarnation
    );
    let replay_mid = damage_history_row(&replay, ruric);
    // Mid-point: the old incarnation's history is retired on EXIT, not on a
    // later entry.
    for (label, mid) in [("live", live_mid), ("replay", replay_mid)] {
        assert_eq!(mid.4, Zone::Exile, "{label}: Ruric is in exile");
        assert!(
            !mid.0 && !mid.1,
            "{label}: the exiled Ruric carries no damage-kind latch"
        );
        assert!(!mid.2, "{label}: the exiled Ruric is out of the Any set");
    }
    apply_resolved_zone_change(&mut replay, &commands[1]).expect("Battlefield command replays");
    assert_eq!(
        replay.objects[&ruric].incarnation,
        commands[1].resulting_incarnation
    );
    assert_eq!(
        replay.zone_changes_this_turn.len(),
        pre.zone_changes_this_turn.len() + 2,
        "replay-taken guard: both commands recorded their zone changes"
    );
    assert!(
        matches!(
            apply_resolved_zone_change(&mut replay, &commands[0]),
            Err(ResolvedZoneChangeReplayInvariantError::OccurrenceMismatch { .. })
        ),
        "replay-taken guard: the Exile command was consumed"
    );
    flush_layers(&mut replay);

    let live_end = damage_history_row(&live, ruric);
    let replay_end = damage_history_row(&replay, ruric);
    // Derived from the card and CR 400.7, not copied from live.
    assert_eq!(
        replay_end,
        (
            false,
            false,
            false,
            true,
            Zone::Battlefield,
            commands[1].resulting_incarnation
        ),
        "the replayed Ruric is a new object with no damage history and hexproof"
    );
    assert_eq!(live_end, replay_end, "live and replay agree");

    for (label, state) in [("live", &live), ("replay", &replay)] {
        assert!(
            state.objects[&ruric_b]
                .dealt_damage_kinds
                .has(DamageKindFilter::CombatOnly),
            "{label}: the unmoved Ruric keeps its combat latch"
        );
        assert!(
            state.objects_that_dealt_damage.contains(&ruric_b),
            "{label}: the unmoved Ruric keeps its Any-set entry"
        );
        assert!(
            !has_hexproof(&state.objects[&ruric_b]),
            "{label}: the unmoved Ruric stays without hexproof"
        );
    }
}

/// R7 - CR 120.8: an attacking Ruric with 0 power deals no combat damage, so it
/// keeps hexproof; the 7-power copy attacking beside it loses it.
#[test]
fn r7_zero_power_attacker_keeps_hexproof() {
    let mut scenario = base_scenario();
    let zero = add_ruric(&mut scenario, P0, 0);
    let seven = add_ruric(&mut scenario, P0, 7);
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
    let attacker = add_ruric(&mut scenario, P0, 7);
    let stay_home = add_ruric(&mut scenario, P0, 7);
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
    let ruric = add_ruric(&mut scenario, P0, 7);
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

/// In a Horde game the Horde is P0 and the survivor is P1.
const HORDE: PlayerId = PlayerId(0);
const SURVIVOR: PlayerId = PlayerId(1);

/// A 2-player Horde game parked at its pre-combat main (local copy of the
/// `horde_advanced_deployment.rs` scenario, default legendary rule).
fn horde_scenario() -> GameScenario {
    let ruleset = ChallengeDeck::CybermanHorde.default_ruleset();
    let mut scenario = GameScenario::new_with_format(FormatConfig::horde(ruleset), 2, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario
}

/// Park the game at SURVIVOR's priority on SURVIVOR's pre-combat main (local
/// copy of the `horde_advanced_deployment.rs` helper): a survivor's main fires
/// no Horde wave.
fn park_survivor_priority(runner: &mut GameRunner) {
    let state = runner.state_mut();
    state.turn_number = 4;
    state.active_player = SURVIVOR;
    state.phase = Phase::PreCombatMain;
    state.priority_player = SURVIVOR;
    state.waiting_for = WaitingFor::Priority { player: SURVIVOR };
}

/// R10 - CR 120.2a + CR 616.1: Ruric's combat damage to the Horde is turned
/// into a mill whose graveyard move collides Rest in Peace with Leyline of the
/// Void, so the combat damage leg pauses on the ordering prompt and completes
/// through the combat Phase C remnant after the answer. That resumed leg never
/// re-enters `apply_damage_after_replacement`, so it must still latch `combat`
/// through the shared damage-history authority and end the hexproof gate.
#[test]
fn r10_combat_damage_resumed_after_replacement_pause_removes_hexproof() {
    let db = shared_card_db().expect("integration card fixture must be available");

    let mut scenario = horde_scenario();
    scenario.add_real_card(SURVIVOR, "Rest in Peace", Zone::Battlefield, db);
    scenario.add_real_card(SURVIVOR, "Leyline of the Void", Zone::Battlefield, db);
    let ruric = add_ruric(&mut scenario, SURVIVOR, 1);
    for _ in 0..10 {
        scenario.add_real_card(HORDE, "Lightning Bolt", Zone::Library, db);
    }
    let mut runner = scenario.build();
    engine::game::rehydrate_game_from_card_db(runner.state_mut(), db);
    park_survivor_priority(&mut runner);

    flush_layers(runner.state_mut());
    assert!(
        has_hexproof(&runner.state().objects[&ruric]),
        "premise: Ruric has hexproof before any combat damage"
    );

    runner.pass_both_players();
    runner
        .declare_attackers(&[(ruric, AttackTarget::Player(HORDE))])
        .expect("Ruric must be able to attack the Horde");
    let mut mill_prompts = 0;
    for _ in 0..40 {
        if runner.state().phase == Phase::PostCombatMain {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => runner.pass_both_players(),
            WaitingFor::DeclareBlockers { .. } => {
                runner
                    .declare_blockers(&[])
                    .expect("declaring no blockers should succeed");
            }
            WaitingFor::ReplacementChoice { .. } => {
                mill_prompts += 1;
                // Reach-guard: the combat leg really paused with its post-loss
                // work stashed for the remnant resume.
                let stash = runner
                    .state()
                    .pending_damage_post_loss
                    .as_ref()
                    .expect("the paused combat leg must stash its post-loss work");
                assert!(stash.is_combat, "the stashed leg must be the combat leg");
                assert_eq!(stash.source_id, ruric);
                // Engine-artifact pin: nothing is recorded until the resume.
                assert!(runner.state().objects[&ruric].dealt_damage_kinds.is_empty());
                runner
                    .act(GameAction::ChooseReplacement { index: 0 })
                    .expect("answer the CR 616.1 ordering prompt");
            }
            other => panic!("unexpected window during combat: {other:?}"),
        }
    }
    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "combat must complete, stuck at {:?} with {:?}",
        runner.state().phase,
        runner.state().waiting_for
    );
    assert_eq!(
        mill_prompts, 1,
        "the redirected mill-1 must pause on its ordering - zero means the \
         board stopped pausing and the test is vacuous"
    );

    assert!(dealt(&runner, ruric, DamageKindFilter::CombatOnly));
    assert!(!dealt(&runner, ruric, DamageKindFilter::NoncombatOnly));
    assert!(runner.state().objects_that_dealt_damage.contains(&ruric));
    assert_stores_agree(&runner, ruric);
    flush_layers(runner.state_mut());
    assert!(
        !has_hexproof(&runner.state().objects[&ruric]),
        "the resumed combat damage must end the hexproof gate"
    );
}

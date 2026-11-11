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
//! resumes through the combat Phase C remnant. R11-R19 drive infect / toxic
//! damage whose poison-counter RESULT (CR 120.4c) pauses on a Vorinclex
//! ordering choice: the damage is already dealt (CR 120.4b), so its fact is
//! recorded once at the pause and the combat batch parks on the prompt.

use engine::database::card_db::CardDatabase;
use engine::database::mtgjson::AtomicCard;
use engine::database::synthesis::build_oracle_face;
use engine::game::combat::AttackTarget;
use engine::game::effects::attach::attach_to;
use engine::game::game_object::AttachTarget;
use engine::game::keywords::has_hexproof;
use engine::game::layers::flush_layers;
use engine::game::printed_cards::{
    rehydrate_game_from_card_db_with_finalization, CardDbRehydrationFinalization,
};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::game::zones::{apply_resolved_zone_change, move_to_zone};
use engine::types::ability::{ContinuousModification, StaticCondition, StaticDefinition};
use engine::types::ability::{DamageKindFilter, TargetRef};
use engine::types::actions::GameAction;
use engine::types::card::PrintedCardRef;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::format::{ChallengeDeck, FormatConfig};
use engine::types::game_state::{CastPaymentMode, GameState, WaitingFor};
use engine::types::game_state::{DamageRecord, PersistedGameState, PersistedRestoreFinalization};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, ProtectionTarget};
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::proposed_event::{CounterPlacement, ProposedEvent};
use engine::types::resolved_commands::{
    ResolvedRulesCommand, ResolvedZoneChangeCommand, ResolvedZoneChangeReplayInvariantError,
};
use engine::types::zones::Zone;
use serde_json::{json, Map, Value};

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

// ---------------------------------------------------------------------------
// R11-R19: poison-counter result replacement choices (CR 120.4b / CR 120.4c).
// ---------------------------------------------------------------------------

/// Verbatim Oracle text of Phyresis (Aura).
const PHYRESIS_ORACLE_TEXT: &str = "Enchant creature\nEnchanted creature has infect. (It deals damage to creatures in the form of -1/-1 counters and to players in the form of poison counters.)";
const VORINCLEX: &str = "Vorinclex, Monstrous Raider";

/// Ruric (verbatim Oracle) plus extra keywords pushed after the parse.
fn add_ruric_with(
    scenario: &mut GameScenario,
    controller: PlayerId,
    power: i32,
    extra: &[Keyword],
) -> ObjectId {
    let mut builder = scenario.add_creature(controller, RURIC_NAME, power, 7);
    builder.from_oracle_text_with_keywords(RURIC_KEYWORDS, RURIC_ORACLE_TEXT);
    for kw in extra {
        builder.with_keyword(kw.clone());
    }
    builder.id()
}

/// One Vorinclex per player from the shared card database (real parse):
/// P0's doubles the counters P0 puts (CR 614.1a), P1's halves the counters
/// P1's opponent (P0) puts. Returns `(doubler, halver)`.
fn add_two_vorinclex(scenario: &mut GameScenario) -> (ObjectId, ObjectId) {
    let db = shared_card_db().expect("integration card fixture must be available");
    let doubler = scenario.add_real_card(P0, VORINCLEX, Zone::Battlefield, db);
    let halver = scenario.add_real_card(P1, VORINCLEX, Zone::Battlefield, db);
    (doubler, halver)
}

/// A printed Phyresis Aura for P0 (re-parsed with the Aura subtype and the
/// MTGJSON `Enchant` keyword hint, as the card-data pipeline does).
fn add_phyresis(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_enchantment_from_oracle(P0, "Phyresis", PHYRESIS_ORACLE_TEXT)
        .with_subtypes(vec!["Aura"])
        .from_oracle_text_with_keywords(&["Enchant"], PHYRESIS_ORACLE_TEXT)
        .id()
}

/// Attach the printed Aura through the production attach authority and prove
/// the host really gained infect (CR 613.1f) before any damage is dealt.
fn attach_phyresis(runner: &mut GameRunner, aura: ObjectId, host: ObjectId) {
    attach_to(runner.state_mut(), aura, host);
    assert_eq!(
        runner.state().objects[&aura].attached_to,
        Some(AttachTarget::Object(host)),
        "the production attach authority must wire Phyresis to its host"
    );
    flush_layers(runner.state_mut());
    assert!(
        runner.state().objects[&host]
            .keywords
            .contains(&Keyword::Infect),
        "reach-guard: Phyresis grants infect to the enchanted creature"
    );
}

/// Events and CR 616.1 prompts collected while driving one combat.
struct CombatDrive {
    events: Vec<GameEvent>,
    prompts: usize,
}

/// Attack P1 with `attackers`, declare `blocks`, and drive the combat through
/// `act` (keeping every action's events) to the post-combat main phase. At each
/// `ReplacementChoice` the callback receives the runner and the 1-based prompt
/// number and returns the candidate index to choose.
fn drive_combat(
    runner: &mut GameRunner,
    attackers: &[ObjectId],
    blocks: &[(ObjectId, ObjectId)],
    mut on_prompt: impl FnMut(&mut GameRunner, usize) -> usize,
) -> CombatDrive {
    runner.advance_to_combat();
    let attacks: Vec<_> = attackers
        .iter()
        .map(|&a| (a, AttackTarget::Player(P1)))
        .collect();
    let mut events = runner
        .declare_attackers(&attacks)
        .expect("attack declaration must succeed")
        .events;
    let mut prompts = 0;
    for _ in 0..80 {
        if runner.state().phase == Phase::PostCombatMain
            && runner.state().stack.is_empty()
            && matches!(runner.state().waiting_for, WaitingFor::Priority { .. })
        {
            break;
        }
        let result = match runner.state().waiting_for.clone() {
            WaitingFor::Priority { .. } => runner.act(GameAction::PassPriority),
            WaitingFor::DeclareBlockers { .. } => runner.declare_blockers(blocks),
            WaitingFor::ReplacementChoice { .. } => {
                prompts += 1;
                let index = on_prompt(runner, prompts);
                runner.act(GameAction::ChooseReplacement { index })
            }
            other => panic!("unexpected window during combat: {other:?}"),
        }
        .expect("combat action must be accepted");
        events.extend(result.events);
    }
    assert_eq!(
        runner.state().phase,
        Phase::PostCombatMain,
        "combat must complete, stuck with {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().pending_replacement.is_none(),
        "no replacement choice may be left orphaned after combat"
    );
    assert!(runner.state().pending_combat_lifelink.is_none());
    CombatDrive { events, prompts }
}

/// The index of the pending replacement candidate whose source is `source`.
fn candidate_from(runner: &GameRunner, source: ObjectId) -> usize {
    runner
        .state()
        .pending_replacement
        .as_ref()
        .expect("a replacement choice is pending")
        .candidates
        .iter()
        .position(|candidate| candidate.source == source)
        .unwrap_or_else(|| panic!("no pending candidate from {source:?}"))
}

/// `DamageDealt` events from `source`, filtered by kind.
fn damage_events(events: &[GameEvent], source: ObjectId, combat: bool) -> usize {
    events
        .iter()
        .filter(|event| {
            matches!(event, GameEvent::DamageDealt { source_id, is_combat, .. }
                if *source_id == source && *is_combat == combat)
        })
        .count()
}

/// Damage records written for `source`.
fn records_from(state: &GameState, source: ObjectId) -> usize {
    state
        .damage_dealt_this_turn
        .iter()
        .filter(|record| record.source_id == source)
        .count()
}

/// Hexproof as the layer system would report it now, read on a clone so the
/// probe does not flush the live state mid-choice.
fn hexproof_now(state: &GameState, id: ObjectId) -> bool {
    let mut probe = state.clone();
    flush_layers(&mut probe);
    has_hexproof(&probe.objects[&id])
}

/// The damage-history facts compared between the paused and unpaused runs:
/// (combat DamageDealt events, records, Any, combat latch, noncombat latch,
/// hexproof).
fn fact_snapshot(
    runner: &GameRunner,
    drive: &CombatDrive,
    id: ObjectId,
) -> (usize, usize, bool, bool, bool, bool) {
    let state = runner.state();
    (
        damage_events(&drive.events, id, true),
        records_from(state, id),
        state.objects_that_dealt_damage.contains(&id),
        state.objects[&id]
            .dealt_damage_kinds
            .has(DamageKindFilter::CombatOnly),
        state.objects[&id]
            .dealt_damage_kinds
            .has(DamageKindFilter::NoncombatOnly),
        hexproof_now(state, id),
    )
}

/// Asserts the pending choice is the poison-counter ordering for P1, with the
/// two Vorinclex replacements as its candidates.
fn assert_poison_ordering_prompt(runner: &GameRunner, doubler: ObjectId, halver: ObjectId) {
    let state = runner.state();
    assert!(
        matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { player, candidate_count: 2, .. } if player == P1
        ),
        "expected P1's 2-candidate ReplacementChoice, got {:?}",
        state.waiting_for
    );
    let pending = state
        .pending_replacement
        .as_ref()
        .expect("the ordering choice is pending");
    assert!(
        matches!(
            pending.proposed,
            ProposedEvent::AddCounter {
                placement: CounterPlacement::Player {
                    actor,
                    player_id,
                    counter_kind: PlayerCounterKind::Poison,
                },
                ..
            } if actor == P0 && player_id == P1
        ),
        "the paused event must be P0 giving P1 poison counters, got {:?}",
        pending.proposed
    );
    let mut sources: Vec<_> = pending.candidates.iter().map(|c| c.source).collect();
    sources.sort_by_key(|id| id.0);
    let mut expected = vec![doubler, halver];
    expected.sort_by_key(|id| id.0);
    assert_eq!(sources, expected, "the candidates are the two Vorinclex");
}

/// R11 - the maintainer's test. Phyresis-infect Ruric (power 3) attacks P1 with
/// one Vorinclex per player: the poison result needs a CR 616.1 order. The
/// combat batch parks on that prompt; the damage was already dealt (CR 120.4b),
/// so the combat fact is recorded exactly once at the pause and Ruric loses
/// hexproof; after the answer the counters follow the chosen order
/// (double-then-half 3, half-then-double floor(3/2)*2 = 2) and an opponent can
/// target Ruric.
#[test]
fn r11_infect_poison_replacement_choice_records_combat_damage_once() {
    for double_first in [true, false] {
        let mut scenario = base_scenario();
        let ruric = add_ruric(&mut scenario, P0, 3);
        let phyresis = add_phyresis(&mut scenario);
        let (doubler, halver) = add_two_vorinclex(&mut scenario);
        let opp_bolt = scenario.add_bolt_to_hand(P1);
        let mut runner = scenario.build();
        attach_phyresis(&mut runner, phyresis, ruric);

        let before = bolt_legal_targets(&mut runner, P1, opp_bolt);
        assert!(
            !can_target(&before, ruric, P0),
            "control: hexproof before combat"
        );

        let drive = drive_combat(&mut runner, &[ruric], &[], |runner, _| {
            // Reach-guard: the combat batch parked on the poison ordering.
            assert_poison_ordering_prompt(runner, doubler, halver);
            let state = runner.state();
            assert!(
                state.pending_combat_lifelink.is_some(),
                "the combat batch must be parked on the prompt"
            );
            assert!(
                state.pending_damage_post_loss.is_none(),
                "a counter pause stashes no post-loss work"
            );
            assert_eq!(state.players[1].poison_counters, 0);
            assert_eq!(state.players[1].life, 20, "infect causes no life loss");
            // CR 120.4b + CR 120.4c: dealt before its result is processed.
            assert!(state.objects[&ruric]
                .dealt_damage_kinds
                .has(DamageKindFilter::CombatOnly));
            assert!(!state.objects[&ruric]
                .dealt_damage_kinds
                .has(DamageKindFilter::NoncombatOnly));
            assert!(state.objects_that_dealt_damage.contains(&ruric));
            assert_stores_agree(runner, ruric);
            assert_eq!(state.damage_dealt_this_turn.len(), 1);
            let record = &state.damage_dealt_this_turn[0];
            assert_eq!(record.source_id, ruric);
            assert_eq!(record.target, TargetRef::Player(P1));
            assert_eq!(record.amount, 3);
            assert!(record.is_combat);
            assert!(
                !hexproof_now(state, ruric),
                "the damage is dealt at the pause, so the gate is already closed"
            );
            candidate_from(runner, if double_first { doubler } else { halver })
        });

        assert_eq!(drive.prompts, 1, "exactly one poison ordering prompt");
        assert_eq!(
            runner.state().players[1].poison_counters,
            if double_first { 3 } else { 2 },
            "double_first={double_first}: the chosen order applies (CR 616.1)"
        );
        assert_eq!(runner.state().players[1].life, 20);
        assert_eq!(runner.state().damage_dealt_this_turn.len(), 1);
        assert_eq!(
            damage_events(&drive.events, ruric, true),
            1,
            "exactly one combat DamageDealt across the pause and the answer"
        );
        assert_stores_agree(&runner, ruric);
        assert_eq!(
            fact_snapshot(&runner, &drive, ruric),
            (1, 1, true, true, false, false)
        );

        let after = bolt_legal_targets(&mut runner, P1, opp_bolt);
        assert!(
            can_target(&after, ruric, P0),
            "double_first={double_first}: an opponent can target Ruric after the answer"
        );
    }
}

/// R12 - parity baseline: the same board without Vorinclex never pauses and
/// yields the same damage-history facts as R11's settled run.
#[test]
fn r12_unpaused_infect_matches_paused_facts() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 3);
    let phyresis = add_phyresis(&mut scenario);
    let mut runner = scenario.build();
    attach_phyresis(&mut runner, phyresis, ruric);

    let drive = drive_combat(&mut runner, &[ruric], &[], |_, _| {
        panic!("no replacement choice without Vorinclex")
    });
    assert_eq!(drive.prompts, 0);
    assert_eq!(runner.state().players[1].poison_counters, 3);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(
        fact_snapshot(&runner, &drive, ruric),
        (1, 1, true, true, false, false),
        "the unpaused leg records exactly what the paused leg records"
    );
}

/// R13 - CR 120.3a + CR 120.3g: toxic adds poison IN ADDITION to the life loss.
/// The loss runs before the toxic counters pause, so it is applied once and the
/// damage fact is recorded once at the pause. Toxic 3 is order-discriminating:
/// double-then-half 3, half-then-double 2.
#[test]
fn r13_toxic_poison_replacement_choice_records_once() {
    for double_first in [true, false] {
        let mut scenario = base_scenario();
        let ruric = add_ruric_with(&mut scenario, P0, 3, &[Keyword::Toxic(3)]);
        let (doubler, halver) = add_two_vorinclex(&mut scenario);
        let mut runner = scenario.build();

        let drive = drive_combat(&mut runner, &[ruric], &[], |runner, _| {
            assert_poison_ordering_prompt(runner, doubler, halver);
            let state = runner.state();
            assert_eq!(state.players[1].life, 17, "the loss already applied");
            assert_eq!(state.players[1].poison_counters, 0);
            assert!(state.pending_damage_post_loss.is_none());
            assert_eq!(records_from(state, ruric), 1);
            assert!(state.objects[&ruric]
                .dealt_damage_kinds
                .has(DamageKindFilter::CombatOnly));
            assert!(state.objects_that_dealt_damage.contains(&ruric));
            candidate_from(runner, if double_first { doubler } else { halver })
        });
        assert_eq!(drive.prompts, 1);
        assert_eq!(
            runner.state().players[1].poison_counters,
            if double_first { 3 } else { 2 },
            "double_first={double_first}"
        );
        assert_eq!(runner.state().players[1].life, 17, "life loss applied once");
        assert_eq!(
            fact_snapshot(&runner, &drive, ruric),
            (1, 1, true, true, false, false)
        );
    }
}

/// R14 - CR 120.2b: a NONcombat infect ping whose poison result pauses records
/// the noncombat fact once at the pause; the combat gate stays open.
#[test]
fn r14_noncombat_infect_pause_records_noncombat_fact_once() {
    let mut scenario = base_scenario();
    let pinger = {
        let mut builder = scenario.add_creature(P0, "Infect Pinger", 1, 1);
        builder.from_oracle_text(PRODIGAL_PYROMANCER);
        builder.with_keyword(Keyword::Infect);
        builder.id()
    };
    let (doubler, halver) = add_two_vorinclex(&mut scenario);
    let mut runner = scenario.build();

    let outcome = runner.activate(pinger, 0).target_player(P1).resolve();
    let mut events = outcome.events().to_vec();
    assert_poison_ordering_prompt(&runner, doubler, halver);
    {
        let state = runner.state();
        assert_eq!(state.players[1].poison_counters, 0);
        assert_eq!(records_from(state, pinger), 1);
        let kinds = &state.objects[&pinger].dealt_damage_kinds;
        assert!(kinds.has(DamageKindFilter::NoncombatOnly));
        assert!(!kinds.has(DamageKindFilter::CombatOnly));
        assert!(state.objects_that_dealt_damage.contains(&pinger));
    }
    let index = candidate_from(&runner, doubler);
    let result = runner
        .act(GameAction::ChooseReplacement { index })
        .expect("answer the poison ordering");
    events.extend(result.events);
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    assert!(runner.state().pending_replacement.is_none());
    assert_eq!(
        runner.state().players[1].poison_counters,
        1,
        "1 doubled to 2, then halved to 1"
    );
    assert_eq!(records_from(runner.state(), pinger), 1);
    assert_eq!(damage_events(&events, pinger, false), 1);
    assert_eq!(damage_events(&events, pinger, true), 0);
    assert!(!runner.state().objects[&pinger]
        .dealt_damage_kinds
        .has(DamageKindFilter::CombatOnly));
}

/// R15 - negative: the same Vorinclex board without infect deals life loss, so
/// no counter is put and no prompt appears (the R11 prompt comes from the
/// poison result).
#[test]
fn r15_no_infect_no_prompt() {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 3);
    add_two_vorinclex(&mut scenario);
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[ruric], &[], |_, _| {
        panic!("life loss is not a counter: no ordering prompt")
    });
    assert_eq!(drive.prompts, 0);
    assert_eq!(runner.state().players[1].life, 17);
    assert_eq!(runner.state().players[1].poison_counters, 0);
    assert_eq!(
        fact_snapshot(&runner, &drive, ruric),
        (1, 1, true, true, false, false)
    );
}

/// R16a - multi-leg park/resume: an infect+lifelink Ruric (pauses) and a plain
/// lifelink attacker in one batch. The paused leg's amount still folds into
/// its lifelink (CR 702.15b), the other leg applies exactly once, and a second
/// Ruric that stayed home keeps hexproof (per-object binding).
#[test]
fn r16a_infect_and_lifelink_attackers_one_prompt() {
    let mut scenario = base_scenario();
    let ruric = add_ruric_with(&mut scenario, P0, 2, &[Keyword::Infect, Keyword::Lifelink]);
    let home = add_ruric(&mut scenario, P0, 2);
    let linker = {
        let mut builder = scenario.add_creature(P0, "Lifelinker", 2, 2);
        builder.with_keyword(Keyword::Lifelink);
        builder.id()
    };
    let (doubler, halver) = add_two_vorinclex(&mut scenario);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[ruric, linker], &[], |runner, _| {
        assert_poison_ordering_prompt(runner, doubler, halver);
        0
    });
    assert_eq!(drive.prompts, 1);
    let state = runner.state();
    assert_eq!(state.damage_dealt_this_turn.len(), 2);
    assert_eq!(records_from(state, ruric), 1);
    assert_eq!(records_from(state, linker), 1);
    assert_eq!(
        state.players[1].poison_counters, 2,
        "2 -> 4 -> 2 or 2 -> 1 -> 2"
    );
    assert_eq!(
        state.players[1].life, 18,
        "only the plain attacker causes loss"
    );
    assert_eq!(
        state.players[0].life, 24,
        "each lifelink attacker gains its power once (the paused leg too)"
    );
    assert_eq!(damage_events(&drive.events, ruric, true), 1);
    assert_eq!(damage_events(&drive.events, linker, true), 1);

    let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(can_target(&legal, ruric, P0));
    assert!(
        !can_target(&legal, home, P0),
        "the Ruric that stayed home keeps hexproof"
    );
}

/// R16b - two infect attackers each pause: the batch re-parks after the first
/// answer (remnant `Repaused`) and each leg is recorded once.
#[test]
fn r16b_two_infect_attackers_two_prompts() {
    let mut scenario = base_scenario();
    let a = add_ruric_with(&mut scenario, P0, 2, &[Keyword::Infect]);
    let b = add_ruric_with(&mut scenario, P0, 2, &[Keyword::Infect]);
    let (doubler, halver) = add_two_vorinclex(&mut scenario);
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[a, b], &[], |runner, _| {
        assert_poison_ordering_prompt(runner, doubler, halver);
        0
    });
    assert_eq!(drive.prompts, 2);
    let state = runner.state();
    assert_eq!(state.damage_dealt_this_turn.len(), 2);
    assert_eq!(records_from(state, a), 1);
    assert_eq!(records_from(state, b), 1);
    assert_eq!(state.players[1].poison_counters, 4);
    assert_eq!(damage_events(&drive.events, a, true), 1);
    assert_eq!(damage_events(&drive.events, b, true), 1);
}

/// R16c - CR 702.4b: a double-strike infect attacker pauses in both combat
/// damage steps; each step's damage is recorded once.
#[test]
fn r16c_double_strike_infect_two_prompts() {
    let mut scenario = base_scenario();
    let ruric = add_ruric_with(
        &mut scenario,
        P0,
        2,
        &[Keyword::Infect, Keyword::DoubleStrike],
    );
    let (doubler, halver) = add_two_vorinclex(&mut scenario);
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[ruric], &[], |runner, _| {
        assert_poison_ordering_prompt(runner, doubler, halver);
        0
    });
    assert_eq!(drive.prompts, 2);
    assert_eq!(records_from(runner.state(), ruric), 2);
    assert_eq!(runner.state().players[1].poison_counters, 4);
    assert_eq!(damage_events(&drive.events, ruric, true), 2);
}

/// R16d - CR 702.7b: a first-strike infect attacker pauses in the first-strike
/// step only; the regular step deals nothing more.
#[test]
fn r16d_first_strike_infect_one_prompt() {
    let mut scenario = base_scenario();
    let ruric = add_ruric_with(
        &mut scenario,
        P0,
        3,
        &[Keyword::Infect, Keyword::FirstStrike],
    );
    let (doubler, halver) = add_two_vorinclex(&mut scenario);
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[ruric], &[], |runner, _| {
        assert_poison_ordering_prompt(runner, doubler, halver);
        candidate_from(runner, doubler)
    });
    assert_eq!(drive.prompts, 1);
    assert_eq!(records_from(runner.state(), ruric), 1);
    assert_eq!(runner.state().players[1].poison_counters, 3);
    assert_eq!(damage_events(&drive.events, ruric, true), 1);
}

/// R17 - Solemnity's "Players can't get counters." prevents the poison result
/// synchronously (no choice), but the damage is still dealt (Solemnity ruling
/// 2017-07-14), so the fact is recorded once and the gate closes.
#[test]
fn r17_prevented_poison_still_records_the_damage() {
    let db = shared_card_db().expect("integration card fixture must be available");
    let mut scenario = base_scenario();
    let ruric = add_ruric_with(&mut scenario, P0, 3, &[Keyword::Infect]);
    scenario.add_real_card(P1, "Solemnity", Zone::Battlefield, db);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[ruric], &[], |_, _| {
        panic!("a mandatory prevention needs no ordering choice")
    });
    assert_eq!(drive.prompts, 0);
    assert_eq!(runner.state().players[1].poison_counters, 0);
    assert_eq!(runner.state().players[1].life, 20);
    assert_eq!(
        fact_snapshot(&runner, &drive, ruric),
        (1, 1, true, true, false, false)
    );
    let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
    assert!(can_target(&legal, ruric, P0));
}

/// R18 - CR 120.3d + CR 702.90b: infect combat damage to a blocking creature is
/// -1/-1 counters; it is recorded once and closes the gate.
#[test]
fn r18_infect_damage_to_blocker_records_once() {
    let mut scenario = base_scenario();
    let ruric = add_ruric_with(&mut scenario, P0, 3, &[Keyword::Infect]);
    let wall = scenario.add_creature(P1, "Wall", 0, 8).id();
    let mut runner = scenario.build();

    let drive = drive_combat(&mut runner, &[ruric], &[(wall, ruric)], |_, _| {
        panic!("creature -1/-1 counters do not pause here")
    });
    assert_eq!(drive.prompts, 0);
    assert_eq!(
        runner.state().objects[&wall]
            .counters
            .get(&CounterType::Minus1Minus1)
            .copied(),
        Some(3)
    );
    assert_eq!(runner.state().objects[&wall].damage_marked, 0);
    assert_eq!(runner.state().players[1].poison_counters, 0);
    assert_eq!(
        fact_snapshot(&runner, &drive, ruric),
        (1, 1, true, true, false, false)
    );
}

/// R19 - CR 120.4b + CR 603.2: "deals combat damage to a player" triggers fire
/// once across the poison pause (the DamageDealt event is emitted at the pause,
/// never again on the answer). Paired positive: the same board without
/// Vorinclex.
#[test]
fn r19_combat_damage_trigger_fires_once_across_the_pause() {
    const TRIGGER: &str =
        "Whenever this creature deals combat damage to a player, you gain 1 life.";
    for with_vorinclex in [true, false] {
        let mut scenario = base_scenario();
        let raider = {
            let mut builder = scenario.add_creature(P0, "Infect Raider", 2, 2);
            builder.from_oracle_text(TRIGGER);
            builder.with_keyword(Keyword::Infect);
            builder.id()
        };
        let vorinclex = with_vorinclex.then(|| add_two_vorinclex(&mut scenario));
        let mut runner = scenario.build();

        let drive = drive_combat(&mut runner, &[raider], &[], |runner, _| {
            let (doubler, halver) = vorinclex.expect("prompts only with Vorinclex");
            assert_poison_ordering_prompt(runner, doubler, halver);
            0
        });
        assert_eq!(drive.prompts, usize::from(with_vorinclex));
        assert_eq!(damage_events(&drive.events, raider, true), 1);
        assert_eq!(
            runner.state().players[0].life,
            21,
            "with_vorinclex={with_vorinclex}: the trigger resolves exactly once"
        );
        assert!(runner.state().stack.is_empty());
        assert_eq!(runner.state().players[1].poison_counters, 2);
    }
}

// ---------------------------------------------------------------------------
// S1-S10 + S-guard: restoring saves written before `dealt_damage_kinds`
// existed (CR 400.7 + CR 120.2a / CR 120.2b). Every row restores through the
// real chokepoint: `PersistedGameState` JSON -> `prepare_for_restore` ->
// `finalize_after_rehydration` with the card-DB rehydration (printed-face
// reapply) inside it. An old save is the current capture with the latch key
// stripped from every object (the field serializes only when non-empty and
// deserializes with `#[serde(default)]`).
// ---------------------------------------------------------------------------

/// Oracle id of the one-face Ruric database built from the real card-data
/// pipeline (`build_oracle_face`).
const RURIC_ORACLE_ID: &str = "ruric-thar-test-oracle";
/// The condition the pre-PR parser wrote for Ruric's hexproof static
/// (`layers.rs` evaluates `Unrecognized` as true: permanent hexproof).
const OLD_RURIC_CONDITION_TEXT: &str = "they haven't dealt combat damage yet";

fn current_ruric_condition() -> StaticCondition {
    StaticCondition::Not {
        condition: Box::new(StaticCondition::SourceHasDealtDamage {
            kind: DamageKindFilter::CombatOnly,
        }),
    }
}

/// The conditions of every static that grants hexproof.
fn hexproof_conditions<'a>(
    defs: impl IntoIterator<Item = &'a StaticDefinition>,
) -> Vec<Option<StaticCondition>> {
    defs.into_iter()
        .filter(|def| {
            def.modifications.iter().any(|m| {
                matches!(
                    m,
                    ContinuousModification::AddKeyword {
                        keyword: Keyword::Hexproof
                    }
                )
            })
        })
        .map(|def| def.condition.clone())
        .collect()
}

/// F1: a one-face database whose Ruric face comes from the real card-data
/// pipeline (MTGJSON-shaped `AtomicCard` -> `build_oracle_face`), so its
/// statics are the CURRENT parse. Face guard: exactly one hexproof static,
/// gated on `Not(SourceHasDealtDamage { CombatOnly })`, and no `Unrecognized`.
fn ruric_face_db() -> CardDatabase {
    let atomic: AtomicCard = serde_json::from_value(json!({
        "name": RURIC_NAME,
        "manaCost": "{5}{G}{G}",
        "colors": ["G"],
        "colorIdentity": ["G"],
        "power": "7",
        "toughness": "7",
        "text": RURIC_ORACLE_TEXT,
        "layout": "normal",
        "type": "Legendary Creature — Ogre Warrior",
        "types": ["Creature"],
        "subtypes": ["Ogre", "Warrior"],
        "supertypes": ["Legendary"],
        "keywords": ["Reach", "Trample", "Vigilance"],
        "manaValue": 7.0,
        "identifiers": {
            "scryfallOracleId": RURIC_ORACLE_ID,
            "scryfallId": "ruric-thar-test-face"
        }
    }))
    .expect("MTGJSON-shaped Ruric deserializes");
    let face = build_oracle_face(&atomic, Some(RURIC_ORACLE_ID.to_string()));
    assert_eq!(
        hexproof_conditions(&face.static_abilities),
        vec![Some(current_ruric_condition())],
        "face guard: the parser emits the current Ruric static"
    );
    let face_json = serde_json::to_value(&face).expect("CardFace serializes");
    assert!(
        !face_json.to_string().contains("\"Unrecognized\""),
        "face guard: no Unrecognized condition in the current face"
    );
    let mut export = Map::new();
    export.insert(face.name.to_lowercase(), face_json);
    CardDatabase::from_json_str(&Value::Object(export).to_string()).expect("one-face export parses")
}

/// Removes every `key` from every JSON object in `value`; returns the count.
fn strip_key(value: &mut Value, key: &str) -> usize {
    match value {
        Value::Object(map) => {
            let mut removed = usize::from(map.remove(key).is_some());
            for child in map.values_mut() {
                removed += strip_key(child, key);
            }
            removed
        }
        Value::Array(items) => items.iter_mut().map(|item| strip_key(item, key)).sum(),
        _ => 0,
    }
}

/// Every serialized object map whose `id` is `id` and that carries statics.
fn object_jsons<'a>(value: &'a Value, id: ObjectId, out: &mut Vec<&'a Map<String, Value>>) {
    match value {
        Value::Object(map) => {
            if map.get("id") == Some(&json!(id)) && map.contains_key("static_definitions") {
                out.push(map);
            }
            for child in map.values() {
                object_jsons(child, id, out);
            }
        }
        Value::Array(items) => items.iter().for_each(|item| object_jsons(item, id, out)),
        _ => {}
    }
}

/// F3: rewrites the object's hexproof static (both `static_definitions` and
/// `base_static_definitions`) to the OLD `Unrecognized` condition. Returns the
/// number of patched definitions.
fn patch_old_static(value: &mut Value, id: ObjectId) -> usize {
    match value {
        Value::Object(map) => {
            let mut patched = 0;
            if map.get("id") == Some(&json!(id)) && map.contains_key("static_definitions") {
                for key in ["static_definitions", "base_static_definitions"] {
                    if let Some(Value::Array(defs)) = map.get_mut(key) {
                        for def in defs.iter_mut() {
                            let gated = def
                                .get("condition")
                                .is_some_and(|c| c.to_string().contains("SourceHasDealtDamage"));
                            if gated {
                                def["condition"] = json!({"type": "Unrecognized", "text": OLD_RURIC_CONDITION_TEXT});
                                patched += 1;
                            }
                        }
                    }
                }
            }
            for child in map.values_mut() {
                patched += patch_old_static(child, id);
            }
            patched
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| patch_old_static(item, id))
            .sum(),
        _ => 0,
    }
}

/// Removes `source_incarnation` from every damage record of `source` (a save
/// written before the field existed: it serializes only when `Some`).
fn strip_record_incarnation(value: &mut Value, source: ObjectId) -> usize {
    match value {
        Value::Object(map) => {
            let mut removed = 0;
            if let Some(Value::Array(records)) = map.get_mut("damage_dealt_this_turn") {
                for record in records.iter_mut() {
                    if record.get("source_id") == Some(&json!(source)) {
                        if let Some(record) = record.as_object_mut() {
                            removed += usize::from(record.remove("source_incarnation").is_some());
                        }
                    }
                }
            }
            for child in map.values_mut() {
                removed += strip_record_incarnation(child, source);
            }
            removed
        }
        Value::Array(items) => items
            .iter_mut()
            .map(|item| strip_record_incarnation(item, source))
            .sum(),
        _ => 0,
    }
}

/// The current persisted capture of `state` as JSON.
fn current_save(state: &GameState) -> Value {
    serde_json::to_value(PersistedGameState::capture(state.clone()))
        .expect("persisted state serializes")
}

/// An old-format save of `state`: the current capture with `dealt_damage_kinds`
/// stripped from every object. Returns the JSON and the strip count.
fn old_format_save(state: &GameState) -> (Value, usize) {
    let mut json = current_save(state);
    let stripped = strip_key(&mut json, "dealt_damage_kinds");
    assert!(
        !json.to_string().contains("dealt_damage_kinds"),
        "old-format guard: no latch key remains"
    );
    (json, stripped)
}

/// The real restore chokepoint shared by server-core and engine-wasm.
fn restore(json: Value, db: &CardDatabase) -> GameState {
    let decoded: PersistedGameState = serde_json::from_value(json).expect("persisted JSON decodes");
    decoded
        .prepare_for_restore(PersistedRestoreFinalization::DeferUntilRehydrated)
        .expect("prepare restore")
        .finalize_after_rehydration(|state| {
            rehydrate_game_from_card_db_with_finalization(
                state,
                db,
                CardDbRehydrationFinalization::Defer,
            );
            Ok(())
        })
        .expect("rehydrate and finalize restore")
}

/// Reach-guard: the restore really ran on the saved history (Any-set entry
/// and records carried over) and finalized with layers flushed (no manual
/// `flush_layers` in these rows).
fn assert_restore_ran(pre: &GameState, restored: &GameState, id: ObjectId) {
    assert_eq!(
        restored.objects_that_dealt_damage.contains(&id),
        pre.objects_that_dealt_damage.contains(&id),
        "the Any-set entry survives restore"
    );
    assert_eq!(
        restored.damage_dealt_this_turn.len(),
        pre.damage_dealt_this_turn.len(),
        "the this-turn damage records survive restore"
    );
    assert!(
        !restored.layers_dirty.is_dirty(),
        "the restore finalized with layers flushed"
    );
}

/// (combat latch, noncombat latch).
fn kinds(state: &GameState, id: ObjectId) -> (bool, bool) {
    let latch = &state.objects[&id].dealt_damage_kinds;
    (
        latch.has(DamageKindFilter::CombatOnly),
        latch.has(DamageKindFilter::NoncombatOnly),
    )
}

/// Records written for `source`.
fn records_of(state: &GameState, source: ObjectId) -> Vec<DamageRecord> {
    state
        .damage_dealt_this_turn
        .iter()
        .filter(|record| record.source_id == source)
        .cloned()
        .collect()
}

/// Whether an opponent's bolt can target `id` on the restored state.
fn opponent_can_target(restored: GameState, id: ObjectId, opp_bolt: ObjectId) -> bool {
    let mut runner = GameRunner::from_state(restored);
    let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
    can_target(&legal, id, P0)
}

/// Pass priority until the phase is `phase` (same turn).
fn pass_to_phase(runner: &mut GameRunner, phase: Phase) {
    for _ in 0..40 {
        if runner.state().phase == phase {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected window: {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("never reached {phase:?}");
}

/// Pass priority until the next turn begins (`start_next_turn` clears
/// `damage_dealt_this_turn`).
fn pass_to_next_turn(runner: &mut GameRunner) {
    let start = runner.state().turn_number;
    for _ in 0..200 {
        if runner.state().turn_number != start {
            return;
        }
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::Priority { .. }),
            "unexpected window while passing to the next turn: {:?}",
            runner.state().waiting_for
        );
        runner.act(GameAction::PassPriority).expect("pass priority");
    }
    panic!("the next turn never began");
}

/// Ruric (verbatim Oracle) attacks P1 for 7 combat damage this turn; it entered
/// a previous turn. Returns the pre-save state (layers flushed), Ruric, and the
/// opponent's bolt.
fn same_turn_combat_fixture() -> (GameState, ObjectId, ObjectId) {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    let life_before = runner.state().players[1].life;
    run_combat(&mut runner, &[ruric], &[]);
    assert_eq!(runner.state().players[1].life, life_before - 7);
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    assert_ne!(
        pre.objects[&ruric].entered_battlefield_turn,
        Some(pre.turn_number),
        "fixture: Ruric entered a previous turn"
    );
    assert_eq!(kinds(&pre, ruric), (true, false));
    assert!(
        !has_hexproof(&pre.objects[&ruric]),
        "pre-save control: hexproof is gone"
    );
    (pre, ruric, opp_bolt)
}

/// Ruric entered THIS turn (typed mutation of `entered_battlefield_turn`, the
/// field is `pub`) and dealt only noncombat damage (Rabid Bite, verbatim).
fn same_turn_noncombat_entrant_fixture() -> (GameState, ObjectId, ObjectId) {
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let victim = scenario.add_creature(P1, "Victim", 0, 8).id();
    let bite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rabid Bite", false, RABID_BITE)
        .id();
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let turn = state.turn_number;
        state
            .objects
            .get_mut(&ruric)
            .unwrap()
            .entered_battlefield_turn = Some(turn);
    }
    runner.cast(bite).target_objects(&[ruric, victim]).resolve();
    assert_eq!(runner.state().objects[&victim].damage_marked, 7);
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    let obj = &pre.objects[&ruric];
    assert_eq!(obj.entered_battlefield_turn, Some(pre.turn_number));
    let records = records_of(&pre, ruric);
    assert_eq!(records.len(), 1);
    assert!(!records[0].is_combat);
    assert_eq!(records[0].source_incarnation, Some(obj.incarnation));
    assert_eq!(kinds(&pre, ruric), (false, true));
    assert!(has_hexproof(obj), "pre-save control: hexproof stays");
    (pre, ruric, opp_bolt)
}

/// S-guard: the record the adapter rows install by typed mutation (decoded
/// from old-format JSON) matches, on its six damage-history fields, the record
/// real combat writes, so the adapter-built history cannot drift from the
/// writer.
#[test]
fn s_guard_adapter_record_shape_matches_real_combat_record() {
    let (pre, ruric, _) = same_turn_combat_fixture();
    let written = records_of(&pre, ruric);
    assert_eq!(written.len(), 1);
    let written = &written[0];
    let incarnation = pre.objects[&ruric].incarnation;
    let typed: DamageRecord = serde_json::from_value(json!({
        "source_id": ruric,
        "source_controller": P0,
        "target": TargetRef::Player(P1),
        "amount": 7,
        "is_combat": true,
        "source_incarnation": incarnation,
    }))
    .expect("old-format damage record decodes");
    assert_eq!(typed.source_id, written.source_id);
    assert_eq!(typed.source_controller, written.source_controller);
    assert_eq!(typed.target, written.target);
    assert_eq!(typed.amount, written.amount);
    assert_eq!(typed.is_combat, written.is_combat);
    assert_eq!(typed.source_incarnation, written.source_incarnation);
}

/// S1 - CR 120.2a + CR 400.7: an old save of a Ruric that dealt combat damage
/// this turn restores WITHOUT hexproof: the this-turn combat record of this
/// incarnation rebuilds the latch. Revert (no recovery): hexproof TRUE.
#[test]
fn s1_old_save_same_turn_combat_restores_without_hexproof() {
    let db = ruric_face_db();
    let (pre, ruric, opp_bolt) = same_turn_combat_fixture();
    let records = records_of(&pre, ruric);
    assert_eq!(records.len(), 1);
    assert!(records[0].is_combat);

    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1, "the old format lacks exactly Ruric's latch");
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    assert!(restored.objects_that_dealt_damage.contains(&ruric));
    assert_eq!(kinds(&restored, ruric), (true, false));
    assert!(
        !has_hexproof(&restored.objects[&ruric]),
        "a Ruric that dealt combat damage before the save must not regain hexproof"
    );
    assert!(opponent_can_target(restored, ruric, opp_bolt));

    // New-format control: the current save restores identically.
    let current = restore(current_save(&pre), &db);
    assert_eq!(
        current.objects[&ruric].dealt_damage_kinds,
        pre.objects[&ruric].dealt_damage_kinds
    );
    assert!(!has_hexproof(&current.objects[&ruric]));
}

/// S2 - CR 120.2b: Ruric entered this turn and dealt only noncombat damage;
/// its trusted this-turn records are its complete history, so the latch is
/// exactly {noncombat} and hexproof stays.
#[test]
fn s2_old_save_same_turn_noncombat_entrant_keeps_hexproof() {
    let db = ruric_face_db();
    let (pre, ruric, opp_bolt) = same_turn_noncombat_entrant_fixture();
    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    assert_eq!(
        restored.objects[&ruric].entered_battlefield_turn,
        Some(restored.turn_number)
    );
    assert_eq!(kinds(&restored, ruric), (false, true));
    assert!(has_hexproof(&restored.objects[&ruric]));
    assert!(
        !opponent_can_target(restored, ruric, opp_bolt),
        "complete noncombat-only history keeps hexproof"
    );
}

/// S3 - policy A: an Any-set entry whose kind the save cannot prove (Ruric
/// entered before this turn; the records of earlier turns are gone) is
/// attributed to combat damage, so hexproof drops.
#[test]
fn s3_old_save_unprovable_history_follows_policy_a() {
    let db = ruric_face_db();

    // (a) Combat in an earlier turn: the records were cleared at the start of
    // this turn.
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    run_combat(&mut runner, &[ruric], &[]);
    let combat_turn = runner.state().turn_number;
    pass_to_next_turn(&mut runner);
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    assert_eq!(pre.turn_number, combat_turn + 1);
    assert!(records_of(&pre, ruric).is_empty(), "no this-turn record");
    assert!(pre.objects_that_dealt_damage.contains(&ruric));
    assert!(!has_hexproof(&pre.objects[&ruric]));
    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    assert_eq!(
        kinds(&restored, ruric),
        (true, false),
        "policy A: an Any-set entry with no provable kind is attributed to combat damage"
    );
    assert!(!has_hexproof(&restored.objects[&ruric]));
    assert!(opponent_can_target(restored, ruric, opp_bolt));

    // (b) Policy A over-approximation: Ruric entered before this turn and its
    // only this-turn record is noncombat; an earlier turn's history could have
    // been combat, so the save cannot prove "no combat damage" and hexproof
    // drops although the live Ruric kept it.
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let victim = scenario.add_creature(P1, "Victim", 0, 8).id();
    let bite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rabid Bite", false, RABID_BITE)
        .id();
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    runner.cast(bite).target_objects(&[ruric, victim]).resolve();
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    assert_ne!(
        pre.objects[&ruric].entered_battlefield_turn,
        Some(pre.turn_number)
    );
    assert_eq!(records_of(&pre, ruric).len(), 1);
    assert_eq!(kinds(&pre, ruric), (false, true));
    assert!(has_hexproof(&pre.objects[&ruric]), "live: noncombat only");
    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    assert_eq!(
        kinds(&restored, ruric),
        (true, true),
        "policy A over-approximation: unprovable earlier history is attributed to combat"
    );
    assert!(!has_hexproof(&restored.objects[&ruric]));
    assert!(opponent_can_target(restored, ruric, opp_bolt));
}

/// S4 - a current-format save restores unchanged (the recovery's trigger is an
/// EMPTY latch on an Any-set member), and restoring a migrated old save a
/// second time is a no-op. A Ruric with no damage history is untouched.
#[test]
fn s4_current_format_save_round_trips_unchanged_and_migration_is_idempotent() {
    let db = ruric_face_db();
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let home = add_ruric(&mut scenario, P0, 7);
    let mut runner = scenario.build();
    run_combat(&mut runner, &[ruric], &[]);
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();

    let json = current_save(&pre);
    assert!(
        json.to_string().contains("dealt_damage_kinds"),
        "current format"
    );
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    for id in [ruric, home] {
        assert_eq!(
            serde_json::to_value(restored.objects[&id].dealt_damage_kinds).unwrap(),
            serde_json::to_value(pre.objects[&id].dealt_damage_kinds).unwrap()
        );
    }
    assert!(!restored.objects_that_dealt_damage.contains(&home));
    assert!(restored.objects[&home].dealt_damage_kinds.is_empty());
    assert!(has_hexproof(&restored.objects[&home]));
    let again = restore(current_save(&restored), &db);
    assert_eq!(
        again.objects[&ruric].dealt_damage_kinds,
        restored.objects[&ruric].dealt_damage_kinds
    );

    // Idempotence of the migration itself.
    let (old, _) = old_format_save(&pre);
    let migrated = restore(old, &db);
    assert_eq!(kinds(&migrated, ruric), (true, false));
    let second = restore(current_save(&migrated), &db);
    assert_eq!(
        second.objects[&ruric].dealt_damage_kinds,
        migrated.objects[&ruric].dealt_damage_kinds
    );
    assert_eq!(
        second.objects_that_dealt_damage,
        migrated.objects_that_dealt_damage
    );
}

/// Verbatim Oracle texts of the four "hasn't dealt damage yet" (Any-kind)
/// siblings: (name, keywords, Oracle text, power, toughness).
const ANY_SIBLINGS: &[(&str, &[&str], &str, i32, i32)] = &[
    (
        "Karakyk Guardian",
        &["Flying", "Vigilance", "Trample"],
        "Flying, vigilance, trample\nThis creature has hexproof as long as it hasn't dealt damage yet. (It can't be the target of spells or abilities your opponents control.)",
        6,
        5,
    ),
    (
        "Palladia-Mors, the Ruiner",
        &["Flying", "Vigilance", "Trample"],
        "Flying, vigilance, trample\nPalladia-Mors has hexproof as long as it hasn't dealt damage yet.",
        6,
        6,
    ),
    (
        "Oyaminartok, Polar Werebear",
        &[],
        "Oyaminartok, Polar Werebear has hexproof if it hasn't dealt damage yet.\nWhenever Oyaminartok deals combat damage to a player, create a Food token.\n{U}, Sacrifice a Food: Draft a card from Oyaminartok's spellbook. When you do, add {U}{U}{U}. Spend this mana only to cast blue creature spells.",
        4,
        5,
    ),
    (
        "Ratonhnhaké꞉ton",
        &[],
        "As long as Ratonhnhaké꞉ton hasn't dealt damage yet, it has hexproof and can't be blocked.\nWhenever Ratonhnhaké꞉ton deals combat damage to a player, create a 1/1 black Assassin creature token with menace. When you do, return target Equipment card from your graveyard to the battlefield, then attach it to that token.",
        3,
        3,
    ),
];

/// S5 - `DamageKindFilter::Any` readers consult only the Any set: each of the
/// four siblings that dealt damage has no hexproof before the save and after
/// the old-format restore, and a non-dealing copy of each keeps hexproof.
#[test]
fn s5_any_kind_siblings_unaffected_by_restore() {
    let db = ruric_face_db();
    let any_condition = StaticCondition::Not {
        condition: Box::new(StaticCondition::SourceHasDealtDamage {
            kind: DamageKindFilter::Any,
        }),
    };
    let mut scenario = base_scenario();
    let mut pairs = Vec::new();
    for (name, keywords, text, power, toughness) in ANY_SIBLINGS {
        let mut ids = [ObjectId(0); 2];
        for slot in &mut ids {
            *slot = scenario
                .add_creature(P0, name, *power, *toughness)
                .from_oracle_text_with_keywords(keywords, text)
                .id();
        }
        pairs.push((*name, ids[0], ids[1]));
    }
    let victim = scenario.add_creature(P1, "Victim", 0, 40).id();
    let bites: Vec<ObjectId> = pairs
        .iter()
        .map(|_| {
            scenario
                .add_spell_to_hand_from_oracle(P0, "Rabid Bite", false, RABID_BITE)
                .id()
        })
        .collect();
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    for ((_, dealer, _), bite) in pairs.iter().zip(&bites) {
        runner
            .cast(*bite)
            .target_objects(&[*dealer, victim])
            .resolve();
    }
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    for (name, dealer, control) in &pairs {
        for id in [dealer, control] {
            let conditions = hexproof_conditions(pre.objects[id].static_definitions.0.iter());
            assert!(
                !conditions.is_empty()
                    && conditions
                        .iter()
                        .all(|c| c.as_ref() == Some(&any_condition)),
                "{name}: parsed Not(SourceHasDealtDamage {{ Any }}), got {conditions:?}"
            );
        }
        assert!(pre.objects_that_dealt_damage.contains(dealer), "{name}");
        assert!(
            !has_hexproof(&pre.objects[dealer]),
            "{name}: dealer, pre-save"
        );
        assert!(
            has_hexproof(&pre.objects[control]),
            "{name}: control, pre-save"
        );
    }

    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, pairs.len());
    let restored = restore(json, &db);
    assert_eq!(
        restored.objects_that_dealt_damage, pre.objects_that_dealt_damage,
        "Any-set membership unchanged"
    );
    assert!(!restored.layers_dirty.is_dirty());
    for (name, dealer, control) in &pairs {
        assert!(!has_hexproof(&restored.objects[dealer]), "{name}: dealer");
        assert!(has_hexproof(&restored.objects[control]), "{name}: control");
    }
    let mut runner = GameRunner::from_state(restored);
    let legal = bolt_legal_targets(&mut runner, P1, opp_bolt);
    for (name, dealer, control) in &pairs {
        assert!(can_target(&legal, *dealer, P0), "{name}: dealer targetable");
        assert!(
            !can_target(&legal, *control, P0),
            "{name}: control hexproof"
        );
    }
}

/// S6 - CR 400.7: Ruric dealt combat damage, was blinked (a new object), then
/// dealt only noncombat damage. The old incarnation's combat record must not
/// leak into the new object's latch. Revert (drop the incarnation comparison):
/// hexproof FALSE.
#[test]
fn s6_old_save_stale_incarnation_record_is_ignored() {
    let db = ruric_face_db();
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let victim = scenario.add_creature(P1, "Victim", 0, 8).id();
    let bite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rabid Bite", false, RABID_BITE)
        .id();
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    run_combat(&mut runner, &[ruric], &[]);
    let old_incarnation = runner.state().objects[&ruric].incarnation;
    pass_to_phase(&mut runner, Phase::PostCombatMain);
    {
        let state = runner.state_mut();
        let mut events = Vec::new();
        move_to_zone(state, ruric, Zone::Exile, &mut events);
        move_to_zone(state, ruric, Zone::Battlefield, &mut events);
        flush_layers(state);
    }
    assert!(!runner.state().objects_that_dealt_damage.contains(&ruric));
    runner.cast(bite).target_objects(&[ruric, victim]).resolve();
    assert_eq!(runner.state().objects[&victim].damage_marked, 7);
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    let obj = &pre.objects[&ruric];
    assert_ne!(obj.incarnation, old_incarnation);
    assert_eq!(obj.entered_battlefield_turn, Some(pre.turn_number));
    let records = records_of(&pre, ruric);
    assert!(
        records
            .iter()
            .any(|r| r.is_combat && r.source_incarnation == Some(old_incarnation)),
        "the old incarnation's combat record is still in this turn's records"
    );
    assert!(records
        .iter()
        .any(|r| !r.is_combat && r.source_incarnation == Some(obj.incarnation)));
    assert!(pre.objects_that_dealt_damage.contains(&ruric));
    assert_eq!(kinds(&pre, ruric), (false, true));

    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    assert_eq!(kinds(&restored, ruric), (false, true));
    assert!(has_hexproof(&restored.objects[&ruric]));
    assert!(
        !opponent_can_target(restored, ruric, opp_bolt),
        "the new object never dealt combat damage: hexproof stays"
    );
}

/// S7 - per object, battlefield only: only the Any-set Ruric on the
/// battlefield changes; a second Ruric with no history and an exiled Any-set
/// id (inconsistent data) stay latch-empty.
#[test]
fn s7_old_save_recovery_is_per_object_and_battlefield_only() {
    let db = ruric_face_db();
    let mut scenario = base_scenario();
    let attacker = add_ruric(&mut scenario, P0, 7);
    let home = add_ruric(&mut scenario, P0, 7);
    let exiled = add_ruric(&mut scenario, P0, 7);
    let mut runner = scenario.build();
    {
        let mut events = Vec::new();
        move_to_zone(runner.state_mut(), exiled, Zone::Exile, &mut events);
    }
    run_combat(&mut runner, &[attacker], &[]);
    runner.state_mut().objects_that_dealt_damage.insert(exiled);
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    assert_eq!(pre.objects[&exiled].zone, Zone::Exile);
    let mut expected: Vec<_> = vec![attacker, exiled];
    expected.sort_by_key(|id| id.0);
    let mut any: Vec<_> = pre.objects_that_dealt_damage.iter().copied().collect();
    any.sort_by_key(|id| id.0);
    assert_eq!(any, expected);

    let (json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, attacker);
    assert_eq!(kinds(&restored, attacker), (true, false));
    assert!(!has_hexproof(&restored.objects[&attacker]));
    assert_eq!(kinds(&restored, home), (false, false));
    assert!(has_hexproof(&restored.objects[&home]));
    assert_eq!(restored.objects[&exiled].zone, Zone::Exile);
    assert_eq!(kinds(&restored, exiled), (false, false));
}

/// S8 - a non-empty latch is never overwritten, even when a this-turn record
/// disagrees with it (a state the current build never produces).
#[test]
fn s8_present_latch_is_never_overwritten() {
    let db = ruric_face_db();
    let mut scenario = base_scenario();
    let ruric = add_ruric(&mut scenario, P0, 7);
    let victim = scenario.add_creature(P1, "Victim", 0, 8).id();
    let bite = scenario
        .add_spell_to_hand_from_oracle(P0, "Rabid Bite", false, RABID_BITE)
        .id();
    let mut runner = scenario.build();
    runner.cast(bite).target_objects(&[ruric, victim]).resolve();
    {
        let state = runner.state_mut();
        let mut combat = records_of(state, ruric)
            .pop()
            .expect("the noncombat record");
        combat.is_combat = true;
        state.damage_dealt_this_turn.push_back(combat);
    }
    flush_layers(runner.state_mut());
    let pre = runner.state().clone();
    assert_eq!(kinds(&pre, ruric), (false, true));
    let json = current_save(&pre);
    assert!(json.to_string().contains("dealt_damage_kinds"));
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    assert_eq!(kinds(&restored, ruric), (false, true));
    assert!(has_hexproof(&restored.objects[&ruric]));
}

/// S9 - a record without `source_incarnation` (a save written before the
/// field existed) cannot be proven to be this incarnation's and is not
/// trusted: the same-turn noncombat entrant of S2 falls to policy A. The
/// difference from S2 is the absent key alone.
#[test]
fn s9_pre_field_record_without_incarnation_is_not_trusted() {
    let db = ruric_face_db();
    let (pre, ruric, opp_bolt) = same_turn_noncombat_entrant_fixture();
    let (mut json, stripped) = old_format_save(&pre);
    assert_eq!(stripped, 1);
    assert_eq!(strip_record_incarnation(&mut json, ruric), 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    let records = records_of(&restored, ruric);
    assert_eq!(records.len(), 1);
    assert!(!records[0].is_combat);
    assert!(records[0].source_incarnation.is_none());
    assert_eq!(
        restored.objects[&ruric].entered_battlefield_turn,
        Some(restored.turn_number)
    );
    assert_eq!(
        kinds(&restored, ruric),
        (true, false),
        "policy A: an untrusted record leaves no provable history"
    );
    assert!(!has_hexproof(&restored.objects[&ruric]));
    assert!(opponent_can_target(restored, ruric, opp_bolt));
}

/// Builds the S10 fixture: a printed Ruric (`add_real_card`, carrying
/// `printed_ref`) from the one-face database, optionally attacking P1 this
/// turn. Returns the pre-save state, Ruric and the opponent's bolt.
fn printed_ruric_fixture(db: &CardDatabase, attack: bool) -> (GameState, ObjectId, ObjectId) {
    let mut scenario = base_scenario();
    let ruric = scenario.add_real_card(P0, RURIC_NAME, Zone::Battlefield, db);
    let opp_bolt = scenario.add_bolt_to_hand(P1);
    let mut runner = scenario.build();
    assert_eq!(
        runner.state().objects[&ruric].printed_ref,
        Some(PrintedCardRef {
            oracle_id: RURIC_ORACLE_ID.to_string(),
            face_name: RURIC_NAME.to_string(),
        })
    );
    assert_eq!(
        hexproof_conditions(runner.state().objects[&ruric].static_definitions.0.iter()),
        vec![Some(current_ruric_condition())]
    );
    if attack {
        let life_before = runner.state().players[1].life;
        run_combat(&mut runner, &[ruric], &[]);
        assert_eq!(runner.state().players[1].life, life_before - 7);
    }
    flush_layers(runner.state_mut());
    (runner.state().clone(), ruric, opp_bolt)
}

/// F3 on a save: strip the latch and patch the old `Unrecognized` static into
/// both static lists of `id`, with the pre-restore JSON guards.
fn old_static_save(pre: &GameState, id: ObjectId) -> (Value, usize) {
    let (mut json, stripped) = old_format_save(pre);
    assert_eq!(
        patch_old_static(&mut json, id),
        2,
        "both static lists patched"
    );
    let mut objects = Vec::new();
    object_jsons(&json, id, &mut objects);
    assert!(!objects.is_empty());
    for object in objects {
        let text = Value::Object(object.clone()).to_string();
        assert!(
            !text.contains("SourceHasDealtDamage"),
            "only the old static remains"
        );
        assert!(text.contains(OLD_RURIC_CONDITION_TEXT));
        assert!(object.contains_key("printed_ref"), "printed_ref is saved");
    }
    (json, stripped)
}

/// S10 - the reapply half end to end: an old save carries the OLD
/// `Unrecognized` static (evaluated true) and no latch. Restore replaces the
/// static with the current printed one and reads it against the recovered
/// latch: hexproof FALSE. Reverting either the recovery or the reapply leaves
/// hexproof TRUE. Control: with no damage history the current static is still
/// installed and hexproof is TRUE.
#[test]
fn s10_old_static_is_reapplied_and_read_against_the_recovered_latch() {
    let db = ruric_face_db();
    let (pre, ruric, opp_bolt) = printed_ruric_fixture(&db, true);
    assert!(!has_hexproof(&pre.objects[&ruric]));
    let (json, stripped) = old_static_save(&pre, ruric);
    assert_eq!(stripped, 1);
    let restored = restore(json, &db);
    assert_restore_ran(&pre, &restored, ruric);
    let obj = &restored.objects[&ruric];
    // Checked first: reverting the recovery (empty latch) or the reapply (the
    // saved `Unrecognized` evaluates true) each leaves hexproof TRUE here.
    assert!(
        !has_hexproof(obj),
        "the current static read against the recovered combat latch ends hexproof"
    );
    assert_eq!(
        hexproof_conditions(obj.static_definitions.0.iter()),
        vec![Some(current_ruric_condition())],
        "reapply replaced the saved Unrecognized static"
    );
    assert_eq!(
        hexproof_conditions(obj.base_static_definitions.iter()),
        vec![Some(current_ruric_condition())]
    );
    assert_eq!(kinds(&restored, ruric), (true, false));
    assert!(opponent_can_target(restored, ruric, opp_bolt));

    // Control: no damage history.
    let (pre, ruric, opp_bolt) = printed_ruric_fixture(&db, false);
    let (json, stripped) = old_static_save(&pre, ruric);
    assert_eq!(stripped, 0);
    let restored = restore(json, &db);
    assert!(!restored.layers_dirty.is_dirty());
    let obj = &restored.objects[&ruric];
    assert_eq!(
        hexproof_conditions(obj.static_definitions.0.iter()),
        vec![Some(current_ruric_condition())]
    );
    assert!(obj.dealt_damage_kinds.is_empty());
    assert!(restored.objects_that_dealt_damage.is_empty());
    assert!(has_hexproof(obj));
    assert!(!opponent_can_target(restored, ruric, opp_bolt));
}

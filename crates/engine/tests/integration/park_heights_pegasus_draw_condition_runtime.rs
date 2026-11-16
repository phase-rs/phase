//! Regression test for a dropped effect-gate on **Park Heights Pegasus**:
//!
//! > Whenever this creature deals combat damage to a player, draw a card if
//! > you had two or more creatures enter the battlefield under your control
//! > this turn.
//!
//! The "draw a card **if you had two or more creatures enter the battlefield
//! under your control this turn**" clause is an effect-level conditional
//! (CR 608.2c) gated on a CR 403.3 this-turn battlefield-entry threshold.
//! Before the fix the condition combinator's "you had" branch hardcoded a
//! count of 1 and required an article ("a"/"an"/"another"), so the counted
//! "two or more" surface failed to parse and the whole trailing `if` was
//! DROPPED — the card drew a card on every combat-damage trigger,
//! unconditionally.
//!
//! After the fix, `parse_entered_this_turn` recognizes the counted "you had N
//! or more [type] enter ... this turn" form and emits `QuantityComparison {
//! BattlefieldEntriesThisTurn{Controller, creature} >= N }`, which the effect
//! pipeline bridges onto the draw as `AbilityCondition::QuantityCheck`.
//!
//! The count reads the `battlefield_entries_this_turn` snapshot (CR 608.2h:
//! game information is read once, at resolution), NOT the live board — so a
//! creature that entered under your control this turn still counts after it has
//! left (died / bounced / was sacrificed). All three tests populate that
//! snapshot directly via `BattlefieldEntryRecord`, mirroring the Smuggler's
//! Share integration harness.
//!
//! CR 403.3 / CR 608.2h: this-turn battlefield-entry tally (persists after the
//! object leaves).
//! CR 608.2c: a conditional ("if") clause on an effect.
//! CR 109.5 / CR 205: "under your control" scopes the count to the controller.

use engine::game::ability_utils::build_resolved_from_def;
use engine::game::effects::resolve_ability_chain;
use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::AbilityKind;
use engine::types::actions::AlternativeCastDecision;
use engine::types::card_type::CoreType;
use engine::types::game_state::{BattlefieldEntryRecord, FaceState, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ast::types::counter::CounterType;

fn green_mana() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Green, ObjectId(0), false, vec![])]
}

fn colorless_mana() -> Vec<ManaUnit> {
    vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![])]
}

const DRAW_CLAUSE: &str =
    "Draw a card if you had two or more creatures enter the battlefield under your control this turn.";

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .unwrap_or(0)
}

/// Record a creature battlefield-entry this turn under `controller`, independent
/// of whether the object is still on the battlefield — exactly the snapshot the
/// "you had N creatures enter this turn" condition reads.
fn record_creature_entry(state: &mut GameState, object_id: u64, controller: PlayerId) {
    state
        .battlefield_entries_this_turn
        .push_back(BattlefieldEntryRecord {
            object_id: ObjectId(object_id),
            name: format!("Entered Creature {object_id}"),
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
            controller,
            face_state: FaceState::FaceUp,
        });
}

fn resolve_draw(runner_state: &mut GameState, source: ObjectId) {
    let def = engine::parser::oracle_effect::parse_effect_chain(DRAW_CLAUSE, AbilityKind::Spell);
    let ability = build_resolved_from_def(&def, source, P0);
    let mut events = Vec::new();
    resolve_ability_chain(runner_state, &ability, &mut events, 0)
        .expect("draw effect must resolve");
}

/// DISCRIMINATOR (revert-probe): only ONE creature entered under P0's control
/// this turn, so the `>= 2` gate is FALSE and no card is drawn. With the fix
/// reverted the dropped condition makes the draw unconditional, P0 draws, and
/// this assertion fails.
#[test]
fn pegasus_does_not_draw_when_fewer_than_two_creatures_entered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let pegasus = scenario.add_creature(P0, "Park Heights Pegasus", 2, 2).id();
    scenario.with_library_top(P0, &["Reward Card"]);
    let mut runner = scenario.build();

    // Exactly ONE creature entered under P0's control this turn.
    record_creature_entry(runner.state_mut(), 9001, P0);

    let before = hand_size(runner.state(), P0);
    resolve_draw(runner.state_mut(), pegasus);
    let after = hand_size(runner.state(), P0);

    assert_eq!(
        after,
        before,
        "P0 drew {} card(s) but only one creature entered this turn; the >= 2 \
         gate must suppress the draw (dropped condition would draw unconditionally)",
        after as i64 - before as i64
    );
}

/// POSITIVE case: TWO creatures entered under P0's control this turn, so the
/// `>= 2` gate is satisfied and exactly one card is drawn.
#[test]
fn pegasus_draws_when_two_creatures_entered() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let pegasus = scenario.add_creature(P0, "Park Heights Pegasus", 2, 2).id();
    scenario.with_library_top(P0, &["Reward Card"]);
    let mut runner = scenario.build();

    record_creature_entry(runner.state_mut(), 9001, P0);
    record_creature_entry(runner.state_mut(), 9002, P0);

    let before = hand_size(runner.state(), P0);
    resolve_draw(runner.state_mut(), pegasus);
    let after = hand_size(runner.state(), P0);

    assert_eq!(
        after,
        before + 1,
        "P0 should draw exactly one card when two creatures entered this turn"
    );
}

/// AUTHORITY DISCRIMINATOR (per review): two creatures entered under P0's
/// control this turn but NEITHER is on the battlefield now — they entered and
/// then left (died / bounced / sacrificed) before the draw condition resolves.
/// The battlefield-entry snapshot still counts both, so the draw fires. A
/// live-board `EnteredThisTurn` count would see zero surviving creatures and
/// wrongly suppress the draw — this test fails under that (incorrect) authority.
#[test]
fn pegasus_draws_when_entered_creatures_have_since_left() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);
    let pegasus = scenario.add_creature(P0, "Park Heights Pegasus", 2, 2).id();
    scenario.with_library_top(P0, &["Reward Card"]);
    let mut runner = scenario.build();

    // Two entries recorded this turn; the objects are NOT on the battlefield.
    record_creature_entry(runner.state_mut(), 9001, P0);
    record_creature_entry(runner.state_mut(), 9002, P0);

    let before = hand_size(runner.state(), P0);
    resolve_draw(runner.state_mut(), pegasus);
    let after = hand_size(runner.state(), P0);

    assert_eq!(
        after,
        before + 1,
        "entries that already left the battlefield must still count toward the \
         'you had two or more creatures enter this turn' gate"
    );
}

/// Tunnel Tipster end-step trigger — CR 603.4 + CR 708.2: "if a face-down
/// creature entered the battlefield under your control this turn" must count
/// face-down entries (printed morph creatures like Sagu Archer) recorded at
/// battlefield entry. Before the FaceState fix, the FaceDown filter was not
/// evaluable from the entry snapshot and the trigger never fired.
#[test]
fn tunnel_tipster_triggers_on_face_down_creature_entry() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Tunnel Tipster: "At the beginning of your end step, if a face-down
    // creature entered the battlefield under your control this turn, put a
    // +1/+1 counter on this creature."
    let tipster = scenario
        .add_creature(P0, "Tunnel Tipster", 1, 2)
        .from_oracle_text("At the beginning of your end step, if a face-down creature entered the battlefield under your control this turn, put a +1/+1 counter on this creature.")
        .id();

    // Sagu Archer — a printed morph creature (face-down when cast face-down)
    // "Morph {2}{G} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)"
    let archer = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Sagu Archer",
            2,
            3,
            "Morph {2}{G} (You may cast this card face down as a 2/2 creature for {3}. Turn it face up any time for its morph cost.)",
        )
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Green, ManaCostShard::Green],
            generic: 1,
        })
        .id();

    // Give P0 enough mana for both normal cast ({1}{G}{G}) and face-down cast ({3})
    // Face-down morph cost is {3} generic, so provide 3 colorless + 2 green for flexibility
    scenario.with_mana_pool(P0, colorless_mana());
    scenario.with_mana_pool(P0, colorless_mana());
    scenario.with_mana_pool(P0, colorless_mana());
    scenario.with_mana_pool(P0, green_mana());
    scenario.with_mana_pool(P0, green_mana());

    let mut runner = scenario.build();

    // DEBUG: Check tipster's abilities and trigger_definitions after creation
    let tipster_obj = runner.state().objects.get(&tipster).expect("tipster on bf");
    eprintln!("DEBUG: Tipster abilities: {:?}", tipster_obj.abilities);
    eprintln!("DEBUG: Tipster trigger_definitions: {:?}", tipster_obj.trigger_definitions);
    for (i, ability) in tipster_obj.abilities.iter().enumerate() {
        eprintln!("DEBUG: Ability {}: kind={:?}, condition={:?}", i, ability.kind, ability.condition);
    }
    for (i, trigger) in tipster_obj.trigger_definitions.0.iter().enumerate() {
        eprintln!("DEBUG: Trigger {}: {:?}", i, trigger);
    }

    // Cast Sagu Archer face-down (morph) — this records a face-down entry
    runner.cast(archer).alternative_cast(AlternativeCastDecision::Alternative).resolve();
    // After morph resolves, it's a 2/2 face-down creature
    evaluate_layers(runner.state_mut());

    // DEBUG: Check if Archer entered face-down
    let archer_obj = runner.state().objects.get(&archer).expect("archer on bf");
    eprintln!("DEBUG: Archer face_down = {}, power = {:?}, toughness = {:?}", archer_obj.face_down, archer_obj.power, archer_obj.toughness);

    // DEBUG: Check battlefield entries
    eprintln!("DEBUG: Battlefield entries this turn: {:?}", runner.state().battlefield_entries_this_turn.len());
    for entry in &runner.state().battlefield_entries_this_turn {
        eprintln!("DEBUG: Entry: id={:?}, name={}, face_state={:?}, core_types={:?}", entry.object_id, entry.name, entry.face_state, entry.core_types);
    }

    // Advance to end step — Tunnel Tipster's trigger should fire
    runner.advance_to_end_step();

    // DEBUG: Check stack after advancing to end step
    eprintln!("DEBUG: Stack after advance_to_end_step: {:?}", runner.state().stack.len());
    for (i, item) in runner.state().stack.iter().enumerate() {
        eprintln!("DEBUG: Stack[{}] = {:?}", i, item);
    }

    runner.advance_until_stack_empty();

    // DEBUG: Check stack after resolving
    eprintln!("DEBUG: Stack after advance_until_stack_empty: {:?}", runner.state().stack.len());

    // Check that Tunnel Tipster got a +1/+1 counter
    let tipster_obj = runner.state().objects.get(&tipster).expect("tipster on bf");
    assert_eq!(tipster_obj.power, Some(2), "Tunnel Tipster should be 2/3 after trigger");
    assert_eq!(tipster_obj.toughness, Some(3), "Tunnel Tipster should be 2/3 after trigger");
    assert!(tipster_obj.counters.get(&CounterType::Plus1Plus1).unwrap_or(&0) >= &1,
        "Tunnel Tipster should have at least one +1/+1 counter");
}

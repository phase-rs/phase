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
use engine::game::scenario::{GameScenario, P0};
use engine::types::ability::AbilityKind;
use engine::types::card_type::CoreType;
use engine::types::game_state::{BattlefieldEntryRecord, GameState};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

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
        .push(BattlefieldEntryRecord {
            object_id: ObjectId(object_id),
            name: format!("Entered Creature {object_id}"),
            core_types: vec![CoreType::Creature],
            subtypes: vec![],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
            controller,
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

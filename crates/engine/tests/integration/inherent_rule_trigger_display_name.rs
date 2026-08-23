//! CR 113.7 defines an ability's source; these inherent triggered abilities have
//! no source object by their own rules.
//!
//! CR 725.2 (monarch), CR 726.2 (initiative), CR 728.1 (rad counters) and
//! CR 702.179d (speed) each say so in the same words — "these triggered
//! abilities have no source". CR 113.8 instead defines an ability's controller;
//! CR 901.8 separately gives Planechase's planeswalking ability no source. The
//! engine models these four constructed triggers with `ObjectId(0)`, which
//! resolves to no `GameObject`.
//!
//! The consequence is a display hole, not a rules hole: `StackEntryKind::
//! TriggeredAbility::source_name` is filled by looking `source_id` up in the
//! objects map, so these four entries reach the client with an EMPTY name. The
//! client has nothing to render and substitutes a name of its own — which is the
//! display layer deriving game-facing content, the one thing it must never do.
//!
//! Reported from a real game: increasing speed off combat damage briefly showed a
//! blank card on the stack.
//!
//! These rows assert the wire contract the client depends on: a stack entry
//! always carries a name for its own source, whether or not that source is an
//! object.

use engine::game::scenario::{GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::StackEntryKind;
use engine::types::phase::Phase;

/// The `source_name` of the single triggered ability on the stack.
///
/// Panics rather than returning `Option` so a row that fails to produce its
/// trigger at all reports "no triggered ability on the stack" instead of
/// silently passing an emptiness check it never reached.
fn only_trigger_source_name(runner: &engine::game::scenario::GameRunner) -> String {
    let names: Vec<String> = runner
        .state()
        .stack
        .iter()
        .filter_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility { source_name, .. } => Some(source_name.clone()),
            StackEntryKind::Spell { .. }
            | StackEntryKind::ActivatedAbility { .. }
            | StackEntryKind::KeywordAction { .. } => None,
        })
        .collect();
    assert_eq!(
        names.len(),
        1,
        "expected exactly one triggered ability on the stack, found {names:?}"
    );
    names.into_iter().next().expect("length was asserted")
}

/// CR 725.2: "At the beginning of the monarch's end step, that player draws a
/// card." No source.
#[test]
fn the_monarch_draw_trigger_names_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let mut runner = scenario.build();
    runner.state_mut().monarch = Some(P0);
    runner.advance_to_end_step();

    // Asserted by VALUE, not by `!is_empty()`: an emptiness check passes on any
    // placeholder the engine might grow later, which is the very thing this row
    // exists to forbid.
    assert_eq!(
        only_trigger_source_name(&runner),
        "The Monarch",
        "CR 725.2's ability has no source object, so it names itself — otherwise \
         the client has to invent a name"
    );
}

/// CR 702.179d: "Whenever one or more opponents lose life during your turn, if
/// your speed is less than 4, your speed increases by 1." No source.
///
/// This is the row the player reported. It differs from the monarch row only in
/// which rule mints the trigger, which is the point: the hole is in the class of
/// sourceless rule triggers, not in one designation.
#[test]
fn the_speed_increase_trigger_names_its_own_source() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let drain = scenario
        .add_spell_to_hand_from_oracle(P0, "Drain", true, "Target player loses 1 life.")
        .id();
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Black);
    let mut runner = scenario.build();
    // CR 702.179b: speed exists only once a rule or effect sets it. Starting at
    // 1 keeps the trigger available (CR 702.179d gates on "less than 4") without
    // needing a `Start your engines!` permanent, which would add a second
    // ability to the board and blur which trigger the assertion reads.
    for player in runner.state_mut().players.iter_mut() {
        if player.id == P0 {
            player.speed = Some(1);
        }
    }
    // `.resolve()` drives the stack to empty, which would resolve the speed
    // trigger too and leave nothing to read. Commit the spell, then pass
    // priority just far enough for the drain to resolve and its trigger to land.
    drop(runner.cast(drain).target_player(P1).commit());
    for _ in 0..8 {
        if runner
            .state()
            .stack
            .iter()
            .any(|entry| matches!(entry.kind, StackEntryKind::TriggeredAbility { .. }))
        {
            break;
        }
        if runner.act(GameAction::PassPriority).is_err() {
            break;
        }
    }

    assert_eq!(
        only_trigger_source_name(&runner),
        "Start your engines!",
        "CR 702.179d's ability has no source object, so it names itself — \
         otherwise the client has to invent a name"
    );
}

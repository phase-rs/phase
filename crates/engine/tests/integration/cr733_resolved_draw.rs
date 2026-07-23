//! CR733 P2 coverage for replay-exact draw bookkeeping composed with zone changes.

use engine::game::scenario::{GameScenario, P0};
use engine::types::game_state::GameState;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::resolved_commands::{
    ResolvedLedgerEdit, ResolvedRulesCommand, ResolvedRulesJournal,
};
use engine::types::zones::Zone;

const DIVINATION_ORACLE: &str = "Draw two cards.";

fn semantic_commands_after(
    state: &GameState,
    first_resolution_entry: usize,
) -> Vec<ResolvedRulesCommand> {
    state
        .resolved_rules_journal
        .entries()
        .iter()
        .skip(first_resolution_entry)
        .filter_map(|entry| entry.command.clone())
        .collect()
}

fn apply_semantic_command(state: &mut GameState, command: &ResolvedRulesCommand) {
    match command {
        ResolvedRulesCommand::ManaInsert(command) => {
            state.apply_resolved_mana_insert(command).unwrap();
        }
        ResolvedRulesCommand::ManaSpend(command) => {
            state.apply_resolved_mana_spend(command).unwrap();
        }
        ResolvedRulesCommand::PlayerEdit(command) => {
            state.apply_resolved_player_edit(command).unwrap();
        }
        ResolvedRulesCommand::ObjectStatus(command) => {
            engine::game::object_state::apply_resolved_object_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::ObjectCounter(command) => {
            engine::game::effects::counters::apply_resolved_counter_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::LedgerEdit(command) => {
            engine::game::ledger::apply_resolved_ledger_edit(state, command).unwrap();
        }
        ResolvedRulesCommand::LibraryShuffle(command) => {
            engine::game::library::apply_resolved_library_shuffle(state, command, &mut Vec::new())
                .unwrap();
        }
        ResolvedRulesCommand::ZoneChange(command) => {
            engine::game::zones::apply_resolved_zone_change(state, command).unwrap();
        }
        ResolvedRulesCommand::Information(command) => {
            state.apply_resolved_information(command).unwrap();
        }
        ResolvedRulesCommand::FrameTransition(command) => {
            state
                .apply_resolved_frame_transition(command.as_ref())
                .unwrap();
        }
        ResolvedRulesCommand::TriggerCollection(command) => {
            engine::game::triggers::apply_resolved_trigger_collection(state, command).unwrap();
        }
    }
}

/// CR 121.1 + CR 121.2: A real Divination-class draw spell records every
/// Library → Hand move through the zone-change hub and each settled draw's
/// bookkeeping through the ledger. Replaying those recorded commands from the
/// pre-resolution stack state must not inspect the library or select cards.
#[test]
fn divination_draw_replays_zone_changes_and_ledger_bookkeeping_exactly() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Divination", false, DIVINATION_ORACLE)
        .with_mana_cost(ManaCost::zero())
        .id();
    scenario.with_library_top(P0, &["First", "Second", "Third"]);
    let mut runner = scenario.build();

    let committed = runner.cast(spell).commit();
    let pre_resolution_state = committed.state().clone();
    let first_resolution_entry = pre_resolution_state.resolved_rules_journal.entries().len();
    let outcome = committed.resolve();
    outcome.assert_hand_drawn(P0, 2);
    let ordinary_state = runner.state().clone();
    let commands = semantic_commands_after(&ordinary_state, first_resolution_entry);

    let draw_zone_changes: Vec<_> = commands
        .iter()
        .filter_map(|command| match command {
            ResolvedRulesCommand::ZoneChange(command)
                if command.from == Zone::Library && command.to == Zone::Hand =>
            {
                Some((command.object.object_id, command.resulting_incarnation))
            }
            _ => None,
        })
        .collect();
    let drawn_edits: Vec<_> = commands
        .iter()
        .filter_map(|command| match command {
            ResolvedRulesCommand::LedgerEdit(command) => match &command.edit {
                ResolvedLedgerEdit::CardsDrawn {
                    drawn_object: Some(object),
                    ..
                } => Some((object.object_id, object.incarnation)),
                _ => None,
            },
            _ => None,
        })
        .collect();
    assert_eq!(
        draw_zone_changes.len(),
        2,
        "CR 121.2: the two-card instruction settles one Library → Hand command per card"
    );
    assert_eq!(
        drawn_edits.len(),
        2,
        "each settled draw must record one CardsDrawn ledger edit after its zone command"
    );
    assert_eq!(
        drawn_edits, draw_zone_changes,
        "the ledger records the exact post-zone-change object occurrence, not a replay-time selection"
    );

    let journal_wire = serde_json::to_value(&ordinary_state.resolved_rules_journal)
        .expect("draw journal serializes");
    assert_eq!(
        serde_json::from_value::<ResolvedRulesJournal>(journal_wire)
            .expect("draw journal validates and deserializes"),
        ordinary_state.resolved_rules_journal
    );

    let mut replay = pre_resolution_state;
    replay.resolved_rules_journal = ordinary_state.resolved_rules_journal.clone();
    for command in &commands {
        apply_semantic_command(&mut replay, command);
    }

    let replay_player = replay
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("replay keeps the drawing player");
    let ordinary_player = ordinary_state
        .players
        .iter()
        .find(|player| player.id == P0)
        .expect("ordinary resolution keeps the drawing player");
    assert_eq!(
        replay_player.cards_drawn_this_turn, ordinary_player.cards_drawn_this_turn,
        "replay installs the player turn counter"
    );
    assert_eq!(
        replay_player.cards_drawn_this_step, ordinary_player.cards_drawn_this_step,
        "replay installs the player step counter"
    );
    assert_eq!(
        replay_player.has_drawn_this_turn, ordinary_player.has_drawn_this_turn,
        "replay installs the player draw flag"
    );
    assert_eq!(
        replay_player.drew_from_empty_library, ordinary_player.drew_from_empty_library,
        "replay installs the player empty-library fact"
    );
    assert_eq!(
        replay.cards_drawn_this_turn, ordinary_state.cards_drawn_this_turn,
        "replay appends the GameState drawn-card ledger in order"
    );
    assert_eq!(
        replay.first_card_drawn_this_turn, ordinary_state.first_card_drawn_this_turn,
        "replay installs the first-draw ledger fact"
    );
    assert_eq!(
        replay_player.hand, ordinary_player.hand,
        "the zone-change commands install the exact hand contents"
    );
    assert_eq!(
        replay_player.library, ordinary_player.library,
        "the zone-change commands preserve the exact remaining library order"
    );
}

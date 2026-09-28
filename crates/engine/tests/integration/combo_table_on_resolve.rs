//! Combo-table floor carriage through deck resolution into runtime deck pools.

use std::collections::BTreeSet;
use std::sync::Arc;

use engine::database::{
    ComboEntry, ComboFilterCounts, ComboOmission, ComboOutcome, ComboPiece, ComboPieceZone,
    ComboProvenance, ComboRelevance, ComboResource, ComboSetup, ComboTable, ComboTableDoc,
};
use engine::game::{
    load_deck_into_state, reconstruct_initial_state, resolve_deck_list, CommanderBracketTier,
    DeckList, PlayerDeckList,
};
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::match_config::MatchConfig;
use engine::types::replay::ReplayHeader;

const COMMANDER: &str = "Abdel Adrian, Gorion's Ward";
const FIRST: &str = "Grizzly Bears";
const SECOND: &str = "Runeclaw Bear";

fn shared_db() -> Option<&'static engine::database::CardDatabase> {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: committed integration card fixture is unavailable");
        return None;
    };
    Some(db)
}

fn combo_piece(name: &str) -> ComboPiece {
    ComboPiece {
        key: name.to_lowercase(),
        display: name.to_string(),
        zone: ComboPieceZone::Anywhere,
    }
}

fn measured_table() -> ComboTable {
    ComboTable::from_doc(ComboTableDoc {
        provenance: ComboProvenance {
            snapshot_date: "2026-09-27".to_string(),
            table_version: "test-v1".to_string(),
            attribution: "test fixture".to_string(),
            card_pool_version: "integration-fixture".to_string(),
            filtered: ComboFilterCounts::default(),
            omitted: vec![
                ComboOmission::PrerequisiteText,
                ComboOmission::ResultText,
                ComboOmission::UnmodeledResultClasses,
            ],
        },
        entries: vec![ComboEntry {
            pieces: [combo_piece(FIRST), combo_piece(SECOND)],
            relevance: ComboRelevance::Standalone,
            setup: ComboSetup::AsPrinted,
            mana_value_needed: 0,
            assemble_cost: 7,
            popularity: 1,
            outcomes: BTreeSet::from([ComboOutcome::Unbounded(ComboResource::Mana)]),
            axes: BTreeSet::new(),
        }],
    })
}

fn deck() -> DeckList {
    DeckList {
        player: PlayerDeckList {
            commander: vec![COMMANDER.to_string()],
            main_deck: vec![FIRST.to_string(), SECOND.to_string()],
            bracket_tier: CommanderBracketTier::Exhibition,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn resolved_pool(combos: &ComboTable) -> Option<engine::types::game_state::PlayerDeckPool> {
    let db = shared_db()?;
    let payload = resolve_deck_list(db, combos, &deck());
    let mut state = GameState::new_two_player(42);
    load_deck_into_state(&mut state, &payload);
    Some(state.deck_pools[0].clone())
}

#[test]
fn resolve_deck_list_carries_combo_floors_into_the_pool() {
    let Some(pool) = resolved_pool(&measured_table()) else {
        return;
    };
    assert_eq!(
        pool.estimated_bracket_tier,
        Some(CommanderBracketTier::Upgraded)
    );
    assert_eq!(
        pool.effective_bracket_tier().tier(),
        CommanderBracketTier::Upgraded
    );
}

#[test]
fn resolve_deck_list_with_a_default_table_is_unchanged() {
    let Some(pool) = resolved_pool(&ComboTable::default()) else {
        return;
    };
    assert_eq!(
        pool.estimated_bracket_tier,
        Some(CommanderBracketTier::Core)
    );
}

#[test]
fn replay_resolves_with_the_table_it_is_given() {
    // Replay reconstruction needs an owned database handle for the state's
    // resolution-time registry. Combo matching itself is name-based, so an
    // empty database keeps this test focused on forwarding the supplied table.
    let db = Arc::new(engine::database::CardDatabase::default());
    let header = ReplayHeader {
        format_config: FormatConfig::commander(),
        match_config: MatchConfig::default(),
        player_count: 2,
        first_player: Some(0),
        seed: 42,
        deck_data: Some(deck()),
    };

    let measured = reconstruct_initial_state(&header, Some(&db), &measured_table())
        .expect("measured replay reconstructs");
    assert_eq!(
        measured.deck_pools[0].estimated_bracket_tier,
        Some(CommanderBracketTier::Upgraded)
    );

    let unmeasured = reconstruct_initial_state(&header, Some(&db), &ComboTable::default())
        .expect("unmeasured replay reconstructs");
    assert_eq!(
        unmeasured.deck_pools[0].estimated_bracket_tier,
        Some(CommanderBracketTier::Core)
    );
}

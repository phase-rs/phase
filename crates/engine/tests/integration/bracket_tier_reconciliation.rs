//! End-to-end carriage of Commander bracket estimates into game-state deck pools.

use engine::database::ComboTable;
use engine::game::bracket_estimate::CommanderBracketTier;
use engine::game::deck_loading::{
    load_deck_into_state, resolve_deck_list, DeckList, PlayerDeckList,
};
use engine::types::game_state::GameState;

fn shared_db() -> Option<&'static engine::database::CardDatabase> {
    let Some(db) = crate::support::shared_card_db() else {
        eprintln!("skipping: committed integration card fixture is unavailable");
        return None;
    };
    Some(db)
}

fn list_with_declaration(declared: CommanderBracketTier) -> DeckList {
    DeckList {
        player: PlayerDeckList {
            commander: vec!["Braids, Cabal Minion".to_string()],
            main_deck: [
                "Ad Nauseam",
                "Ancient Tomb",
                "Bolas's Citadel",
                "Chrome Mox",
            ]
            .into_iter()
            .map(str::to_string)
            .collect(),
            bracket_tier: declared,
            ..Default::default()
        },
        ..Default::default()
    }
}

fn resolved_pool(list: &DeckList) -> Option<engine::types::game_state::PlayerDeckPool> {
    let db = shared_db()?;
    let payload = resolve_deck_list(db, &ComboTable::default(), list);
    let mut state = GameState::new_two_player(42);
    load_deck_into_state(&mut state, &payload);
    Some(state.deck_pools[0].clone())
}

#[test]
fn bracket_estimate_is_carried_and_reconciled_through_deck_loading() {
    let Some(optimized) = resolved_pool(&list_with_declaration(CommanderBracketTier::Exhibition))
    else {
        return;
    };
    assert_eq!(
        optimized.estimated_bracket_tier,
        Some(CommanderBracketTier::Optimized)
    );
    assert_eq!(
        optimized.effective_bracket_tier().tier(),
        CommanderBracketTier::Optimized
    );

    let cedh = resolved_pool(&list_with_declaration(CommanderBracketTier::Cedh))
        .expect("fixture was available for the first resolve");
    assert_eq!(
        cedh.estimated_bracket_tier,
        Some(CommanderBracketTier::Optimized)
    );
    assert_eq!(
        cedh.effective_bracket_tier().tier(),
        CommanderBracketTier::Cedh
    );

    let no_commander = resolved_pool(&DeckList {
        player: PlayerDeckList {
            main_deck: vec!["Ancient Tomb".to_string()],
            bracket_tier: CommanderBracketTier::Upgraded,
            ..Default::default()
        },
        ..Default::default()
    })
    .expect("fixture was available for the first resolve");
    assert_eq!(no_commander.estimated_bracket_tier, None);
    assert_eq!(
        no_commander.effective_bracket_tier().tier(),
        CommanderBracketTier::Upgraded
    );
}

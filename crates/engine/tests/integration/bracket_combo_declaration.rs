use engine::database::{BracketLists, CardDatabase};
use engine::game::{
    estimate_bracket_for_request, Barometer, BarometerAuthority, BracketEstimateRequest,
    ComboDeclaration, ComboWindow, CommanderBracketTier, PlayerDeckList,
};
use engine::starter_decks::DeckData;

#[test]
fn legacy_deck_json_without_the_field_deserializes_as_unanswered() {
    let player_deck: PlayerDeckList =
        serde_json::from_str(r#"{"main_deck":[],"commander":["Cmdr"]}"#).unwrap();
    let deck_data: DeckData =
        serde_json::from_str(r#"{"main_deck":[],"commander":["Cmdr"]}"#).unwrap();

    assert_eq!(player_deck.combo_declaration, ComboDeclaration::Undeclared);
    assert_ne!(
        player_deck.combo_declaration,
        ComboDeclaration::NoneIntended
    );
    assert_eq!(deck_data.combo_declaration, ComboDeclaration::Undeclared);
    assert_ne!(deck_data.combo_declaration, ComboDeclaration::NoneIntended);
}

#[test]
fn declaration_survives_deck_data_to_player_deck_list() {
    let deck_data = DeckData {
        commander: vec!["Cmdr".to_string()],
        combo_declaration: ComboDeclaration::Intended {
            window: Some(ComboWindow::EarlyGame),
        },
        ..Default::default()
    };
    let json = serde_json::to_string(&deck_data).unwrap();
    let player_deck: PlayerDeckList = serde_json::from_str(&json).unwrap();

    assert_eq!(player_deck.combo_declaration, deck_data.combo_declaration);

    let request = BracketEstimateRequest {
        deck: player_deck,
        declared_tier: None,
    };
    let db = CardDatabase::default().with_bracket_lists(BracketLists::from_pairs("t", &[]));
    let estimate = estimate_bracket_for_request(&request, &db).unwrap();

    assert_eq!(estimate.tier, CommanderBracketTier::Optimized);
    assert_eq!(
        estimate.barometers[&Barometer::TwoCardCombos],
        BarometerAuthority::DeckOwner
    );
}

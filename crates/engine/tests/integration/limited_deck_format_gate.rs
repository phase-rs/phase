use engine::game::deck_validation::{
    evaluate_deck_compatibility, evaluate_deck_format_gate, validate_name_deck_for_format_full,
    DeckCompatibilityRequest,
};
use engine::types::format::{FormatConfig, GameFormat, SelectedFormat};
use engine::types::match_config::MatchType;

fn request(main_deck: Vec<String>, sideboard: Vec<String>) -> DeckCompatibilityRequest {
    DeckCompatibilityRequest {
        main_deck,
        sideboard,
        selected_format: Some(SelectedFormat::Tag(GameFormat::Limited)),
        selected_match_type: Some(MatchType::Bo1),
        player_count: 2,
        ..Default::default()
    }
}

fn forests(count: usize) -> Vec<String> {
    std::iter::repeat_n("Forest".to_string(), count).collect()
}

#[test]
fn limited_gate_full_summary_and_game_entry_agree_on_size_and_resolution() {
    let db = crate::support::shared_card_db().expect("curated card export is required");
    assert!(db.get_face_by_name("Forest").is_some());

    let mut unknown_main = forests(39);
    unknown_main.push("Unknown Limited Probe".to_string());
    let cases = [
        ("resolved floor", request(forests(40), vec![]), None),
        ("short", request(forests(39), vec![]), Some("at least 40")),
        (
            "unknown main",
            request(unknown_main, vec![]),
            Some("Unknown cards"),
        ),
        (
            "unknown sideboard",
            request(forests(40), vec!["Unknown Limited Probe".to_string()]),
            Some("Unknown cards"),
        ),
    ];

    for (label, full_request, expected_reason) in cases {
        let gate = evaluate_deck_format_gate(db, &full_request);
        let full = evaluate_deck_compatibility(db, &full_request);
        let mut summary_request = full_request.clone();
        summary_request.summary_only = true;
        let summary = evaluate_deck_compatibility(db, &summary_request);
        let game_entry = validate_name_deck_for_format_full(
            db,
            &full_request.main_deck,
            &full_request.sideboard,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::limited(),
            Some(MatchType::Bo1),
            2,
        );
        match expected_reason {
            None => {
                assert!(gate.compatible, "{label}: {:?}", gate.reasons);
                assert_eq!(full.selected_format_compatible, Some(true), "{label}");
                assert_eq!(summary.selected_format_compatible, Some(true), "{label}");
                assert_eq!(game_entry, Ok(()), "{label}");
            }
            Some(reason) => {
                assert!(!gate.compatible, "{label}: gate must refuse");
                assert!(
                    gate.reasons.iter().any(|r| r.contains(reason)),
                    "{label}: {:?}",
                    gate.reasons
                );
                assert_eq!(full.selected_format_compatible, Some(false), "{label}");
                assert_eq!(summary.selected_format_compatible, Some(false), "{label}");
                assert!(
                    summary
                        .selected_format_reasons
                        .iter()
                        .any(|r| r.contains(reason)),
                    "{label}: {:?}",
                    summary.selected_format_reasons
                );
                assert!(game_entry.is_err(), "{label}: game entry must refuse");
                if reason == "Unknown cards" {
                    assert_eq!(
                        summary.unknown_cards,
                        vec!["Unknown Limited Probe"],
                        "{label}"
                    );
                }
            }
        }
    }
}

#[test]
fn limited_gate_keeps_the_cross_format_ante_refusal() {
    let db = crate::support::shared_card_db().expect("curated card export is required");
    assert!(db.get_face_by_name("Contract from Below").is_some());
    let mut main_deck = forests(39);
    main_deck.push("Contract from Below".to_string());
    let gate = evaluate_deck_format_gate(db, &request(main_deck, vec![]));
    assert!(!gate.compatible);
    assert!(
        gate.reasons
            .iter()
            .any(|reason| reason.contains("Contract from Below") && reason.contains("ante")),
        "{:?}",
        gate.reasons
    );
}

//! `GameFormat::Freeform`, through the production entry points
//! `evaluate_deck_compatibility` and `validate_name_deck_for_format_full` (the
//! latter is what `engine-wasm::validate_deck_list_seats` and
//! `phase-server/src/main.rs` both call at the real game-creation boundary).
//!
//! Freeform's rules are fixed by the user, not derived: unrestricted card
//! pool (every set, including unreleased/preview, no ban list), no main-deck
//! minimum, no copy limit, and CR 100.4a's fifteen-card sideboard cap. See
//! `GameFormat::Freeform`'s doc comment in `crates/engine/src/types/format.rs`.
//!
//! Every card name below is verified against the real card export,
//! not from memory: `Craterclaw Colossus` (printed only in FRA, no
//! legality row), `Black Lotus` (`legacy: banned`, `vintage: restricted`),
//! `Contract from Below` ("Remove this card from your deck before playing if
//! you're not playing for ante"), `Vazal, the Compleat` ("Your deck can have
//! only one copy of this card"), `Lightning Bolt` (`legacy: legal`, no
//! `standard` key), `Forest` (a basic, exempt from every copy limit).

use engine::database::CardDatabase;
use engine::game::{
    evaluate_deck_compatibility, validate_name_deck_for_format_full, DeckCompatibilityRequest,
};
use engine::types::format::{FormatConfig, FormatGroup, GameFormat, SelectedFormat};

fn repeat(name: &str, n: usize) -> Vec<String> {
    std::iter::repeat_n(name.to_string(), n).collect()
}

fn db() -> Option<&'static CardDatabase> {
    crate::support::shared_card_db()
}

/// Degenerate end. `DeckSizeRule::Minimum(0).accepts(n)` holds for
/// every `usize`, so the deck-size refusal path is unreachable for this
/// format — including at the smallest input the request type admits.
#[test]
fn freeform_accepts_an_empty_main_deck() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Ok(()),
        "Freeform must accept an empty main deck"
    );
}

/// No main-deck size floor. Legacy is the
/// contrast because its pool never shrinks, so the refusal stays attributable
/// to the deck-size subject rather than to a rotated card; the same-format
/// control shows Freeform's own validator reached this deck and refused it on
/// a DIFFERENT authority, so the acceptance above is not a short-circuit.
#[test]
fn freeform_has_no_main_deck_size_floor() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let ten_forest = repeat("Forest", 10);

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &ten_forest,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Ok(()),
        "Freeform must accept a 10-card main deck"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &ten_forest,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::legacy(),
            None,
            2,
        ),
        Err(vec![
            "Legacy deck must have at least 60 cards (found 10)".to_string()
        ]),
        "Legacy's own deck-size authority must still reach this deck"
    );

    let sixteen_sideboard = repeat("Forest", 16);
    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &ten_forest,
            &sixteen_sideboard,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Err(vec!["Sideboard has 16 cards (maximum 15)".to_string()]),
        "same-format control: Freeform's own validator reached this deck and \
         refused it on a different authority, so the accept above is not a \
         short-circuit before the size subject was evaluated"
    );
}

/// No copy limit. Same contrast/control
/// discipline as the floor test.
#[test]
fn freeform_has_no_copy_limit() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let mut deck = repeat("Lightning Bolt", 8);
    deck.extend(repeat("Forest", 52));

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Ok(()),
        "Freeform must accept eight copies of a non-basic card"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::legacy(),
            None,
            2,
        ),
        Err(vec![
            "More than 4 copies (main + sideboard combined): Lightning Bolt (8 copies)".to_string()
        ]),
        "Legacy's own copy-limit authority must still reach this deck"
    );

    let sixteen_sideboard = repeat("Forest", 16);
    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &sixteen_sideboard,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Err(vec!["Sideboard has 16 cards (maximum 15)".to_string()]),
        "same-format control"
    );
}

/// CR 100.4a's fifteen-card sideboard, the one
/// constructed-play rule Freeform keeps. A bound straddles,
/// so no contrast/control pair is needed on either side.
#[test]
fn freeform_caps_the_sideboard_at_fifteen() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let mut deck = repeat("Lightning Bolt", 8);
    deck.extend(repeat("Forest", 52));
    let fifteen_sideboard = repeat("Forest", 15);
    let sixteen_sideboard = repeat("Forest", 16);

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &fifteen_sideboard,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Ok(()),
        "CR 100.4a: a 15-card sideboard is legal"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &sixteen_sideboard,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Err(vec!["Sideboard has 16 cards (maximum 15)".to_string()]),
        "CR 100.4a: a 16-card sideboard is refused"
    );
}

/// Freeform's unrestricted pool admits a card with no legality
/// row at all. Premodern is the durable contrast: its pool closed in 2003, so
/// an FRA card will never become Premodern-legal, unlike a Standard contrast
/// that would invert on the set's release.
#[test]
fn freeform_admits_a_card_printed_only_in_reality_fracture() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let mut deck = repeat("Craterclaw Colossus", 4);
    deck.extend(repeat("Forest", 56));

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Ok(()),
        "Freeform's unrestricted pool admits a card with no legality row"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::premodern(),
            None,
            2,
        ),
        Err(vec![
            "Not Premodern legal: Craterclaw Colossus (not legal in Premodern)".to_string()
        ]),
    );
}

/// Freeform's `AdmitsEveryCard` authority never consults a ban or
/// restricted list; those are a distinct authority from the absence of a
/// legality table. Vintage's `restricted` verdict demonstrates the second
/// half of "banned or restricted" through a different authority
/// (`restricted_copy_violations`).
#[test]
fn freeform_accepts_a_card_the_legacy_ban_list_refuses() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let mut deck = repeat("Black Lotus", 4);
    deck.extend(repeat("Forest", 56));

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform(),
            None,
            2,
        ),
        Ok(()),
        "Freeform's card pool never consults a ban or restricted list"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::legacy(),
            None,
            2,
        ),
        Err(vec!["Not Legacy legal: Black Lotus (banned)".to_string()]),
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &deck,
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::vintage(),
            None,
            2,
        ),
        Err(vec![
            "More than 1 copy of a restricted card: Black Lotus (4 copies)".to_string()
        ]),
        "the same card is restricted rather than banned in Vintage, a different authority"
    );
}

/// Out-of-pool refusal classes that DO NOT consult the card-pool
/// axis, so Freeform's unrestricted pool does not suppress them: CR 407.3's
/// ante-card refusal and a card's own PRINTED deck-construction limit
/// (`effective_copy_limit`'s `deck_copy_limit_for(..).unwrap_or(format_default)`,
/// which lets a printed override replace Freeform's `Unlimited` default).
///
/// Both classes reach each dispatcher through DIFFERENT code: the full leg's
/// accumulating `copy_limit_violations` in `evaluate_constructed` vs. the
/// summary leg's early-returning one in `quick_constructed_check`, and
/// `ante_deck_violations`, which neither dispatcher's per-format arms call —
/// `evaluate_selected_format_summary` and `evaluate_selected_format` each
/// call it once, from their own post-`match` block.
/// Looped over `summary_only` like this file's other dispatcher-agreement
/// tests, so a future edit that makes the two legs disagree reds here instead
/// of only surfacing through the deck-builder hint accepting what game
/// creation refuses.
#[test]
fn freeform_still_refuses_an_ante_card_and_a_printed_copy_limit_overrun() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let mut ante_deck = vec!["Contract from Below".to_string()];
        ante_deck.extend(repeat("Forest", 59));
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: ante_deck,
                selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "Can't be in a deck or sideboard unless the game is played for ante: Contract from Below"
                    .to_string()
            ],
            "summary_only={summary_only}: CR 407.3 applies to every format whose AntePolicy is \
             not Enabled, and ante::policy_of's own doc records that no built-in plays for ante"
        );

        let mut over_limit = repeat("Vazal, the Compleat", 2);
        over_limit.extend(repeat("Forest", 58));
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: over_limit,
                selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "More than the allowed copies (main + sideboard combined): Vazal, the Compleat (2 copies)"
                    .to_string()
            ],
            "summary_only={summary_only}: a card's own printed deck-construction limit still \
             binds under Freeform's Unlimited default"
        );

        let mut at_limit = vec!["Vazal, the Compleat".to_string()];
        at_limit.extend(repeat("Forest", 59));
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: at_limit,
                selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: one copy is within the card's own printed limit: {:?}",
            result.selected_format_reasons
        );
    }
}

/// The boundary pair `FormatConfig::freeform().validate_for_player_count`
/// draws.
#[test]
fn freeform_admits_exactly_two_seats() {
    let config = FormatConfig::freeform();
    assert!(
        config.validate_for_player_count(1).is_err(),
        "one seat is below the floor"
    );
    assert!(
        config.validate_for_player_count(2).is_ok(),
        "two seats is Freeform's only admitted count"
    );
    assert!(
        config.validate_for_player_count(3).is_err(),
        "three seats is above the ceiling"
    );
}

/// Freeform's registry entry keeps it inside
/// `registry_constructed_formats_declare_a_deck_construction_pool`'s clause 1:
/// a Constructed-group format must declare `LegalityTable(_)` or
/// `Unrestricted`, never merely lack an authority.
#[test]
fn freeform_is_in_the_constructed_group() {
    let registry = GameFormat::registry();
    let entry = registry
        .iter()
        .find(|m| m.format == GameFormat::Freeform)
        .expect("Freeform must be in registry");
    assert_eq!(entry.group, FormatGroup::Constructed);
    assert_eq!(entry.short_label, "FRF");
}

/// The wrong-LABEL discriminator, on BOTH dispatches.
/// `Forest` occupies the commander slot deliberately: it is in the curated
/// fixture, so `collect_unknown_cards` adds no second reason, and
/// `CommanderPairing::NoCommander` refuses on the SLOT COUNT, never on
/// eligibility. Equality (not `contains`) on the whole reason vector is also
/// the assertion that the two dispatchers agree on the reason SET for this
/// deck, which `evaluate_constructed`'s accumulating shape and
/// `quick_constructed_check`'s early-return shape could otherwise diverge on.
#[test]
fn freeform_refuses_a_populated_commander_slot_on_both_dispatches() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let main_deck = repeat("Forest", 60);

    for summary_only in [false, true] {
        let with_commander = DeckCompatibilityRequest {
            main_deck: main_deck.clone(),
            commander: vec!["Forest".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &with_commander);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Freeform decks do not use a commander slot".to_string()],
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert_eq!(
            result.selected_format_compatible,
            Some(false),
            "summary_only={summary_only}"
        );

        // Reach guard, inside the same loop: the identical request with an
        // empty commander slot is compatible with an empty reason list.
        let without_commander = DeckCompatibilityRequest {
            main_deck: main_deck.clone(),
            commander: Vec::new(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &without_commander);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
    }
}

/// The wrong-EVALUATOR discriminator: the summary dispatcher's
/// `QuickCheckResult::compatible()` arm (`FreeForAll | TwoHeadedGiant |
/// Limited`) is a live sibling of Freeform's membership, and a membership
/// landing there would make the summary leg accept decks the full leg
/// refuses. The absent floor, the absent copy limit and the sideboard bound
/// are each asserted on both legs of the same loop, plus a Legacy reach guard
/// proving the summary leg evaluates rather than answering unconditionally.
#[test]
fn freeform_axes_give_the_same_verdict_on_both_dispatches() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let ten_forest = repeat("Forest", 10);
    let mut eight_bolt = repeat("Lightning Bolt", 8);
    eight_bolt.extend(repeat("Forest", 52));
    let sixty_forest = repeat("Forest", 60);
    let sixteen_sideboard = repeat("Forest", 16);

    for summary_only in [false, true] {
        let floor = DeckCompatibilityRequest {
            main_deck: ten_forest.clone(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &floor);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only} floor: {:?}",
            result.selected_format_reasons
        );
        assert!(result.selected_format_reasons.is_empty());

        let copy_limit = DeckCompatibilityRequest {
            main_deck: eight_bolt.clone(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &copy_limit);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only} copy limit: {:?}",
            result.selected_format_reasons
        );
        assert!(result.selected_format_reasons.is_empty());

        let bound = DeckCompatibilityRequest {
            main_deck: sixty_forest.clone(),
            sideboard: sixteen_sideboard.clone(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &bound);
        assert_eq!(
            result.selected_format_compatible,
            Some(false),
            "summary_only={summary_only} sideboard bound: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("Sideboard has 16 cards (maximum 15)")),
            "summary_only={summary_only} sideboard bound: {:?}",
            result.selected_format_reasons
        );

        // Reach guard: Legacy, already a member of the same class arm, is
        // refused on both legs — showing the summary leg evaluates rather
        // than answering unconditionally.
        let legacy = DeckCompatibilityRequest {
            main_deck: ten_forest.clone(),
            selected_format: Some(SelectedFormat::Tag(GameFormat::Legacy)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &legacy);
        assert_eq!(
            result.selected_format_compatible,
            Some(false),
            "summary_only={summary_only} Legacy reach guard: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("Legacy deck must have at least 60 cards (found 10)")),
            "summary_only={summary_only} Legacy reach guard: {:?}",
            result.selected_format_reasons
        );
    }
}

/// The multi-authority hostile fixture. `evaluate_standard`
/// reads `&FormatConfig::standard()` regardless of what the request selected,
/// so a Freeform deck's `standard` reference column is computed against
/// Standard's own rules and can disagree with `selected_format_compatible`.
/// `summary_only: false` is load-bearing, unlike the tests above that loop
/// `summary_only`: the summary dispatcher never calls `evaluate_standard` at
/// all — it synthesizes `standard` from a `matches!` on the tag with an empty
/// `reasons` — so a fixture that looped `summary_only` would assert an empty
/// reason list on one leg and the deck-size string on the other.
#[test]
fn freeform_reference_column_is_computed_against_standards_own_rules() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let request = DeckCompatibilityRequest {
        main_deck: repeat("Forest", 10),
        selected_format: Some(SelectedFormat::Tag(GameFormat::Freeform)),
        summary_only: false,
        ..DeckCompatibilityRequest::default()
    };
    let result = evaluate_deck_compatibility(db, &request);
    assert_eq!(
        result.selected_format_compatible,
        Some(true),
        "{:?}",
        result.selected_format_reasons
    );
    assert!(result.selected_format_reasons.is_empty());

    // Reach guard: the reference column's reasons are non-empty, showing it
    // ran rather than defaulting.
    assert!(
        !result.standard.compatible,
        "the reference column must disagree for a deck Freeform accepts and Standard refuses"
    );
    assert_eq!(
        result.standard.reasons,
        vec!["Standard deck must have at least 60 cards (found 10)".to_string()],
    );
}

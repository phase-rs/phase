//! `GameFormat::FreeformCommander`, through the production entry
//! points `evaluate_deck_compatibility` and `validate_name_deck_for_format_full`
//! (the latter is what `engine-wasm::validate_deck_list_seats` and
//! `phase-server/src/main.rs` both call at the real game-creation boundary).
//!
//! Freeform Commander's rules are not derived:
//! any card that can be CAST may be its commander (a departure from CR 903.3),
//! a land may not (CR 305.1 + CR 305.9 + CR 903.8), every set is in the pool,
//! there is no ban list, no copy limit, no main-deck minimum, and no
//! sideboard. See
//! `GameFormat::FreeformCommander`'s doc comment in
//! `crates/engine/src/types/format.rs`.
//!
//! `Wastes` is used as a main-deck filler for Commander-family deck-size
//! padding without introducing a color-identity or copy-limit reason of its
//! own: it is a Basic land (exempt from every singleton rule), its own color
//! identity is empty (a subset of every commander's identity, so it can never
//! be "outside" one), and it carries a `"commander": "legal"` row in the
//! fixture. Every card name below is verified against the real card export.

use engine::database::legality::LegalityFormat;
use engine::game::deck_loading::{load_deck_into_state, DeckEntry, DeckPayload, PlayerDeckPayload};
use engine::game::match_flow::handle_submit_sideboard;
use engine::game::{
    evaluate_deck_compatibility, validate_name_deck_for_format_full, DeckCompatibilityRequest,
};
use engine::types::card::CardFace;
use engine::types::format::{
    validate_starting_life_bounds, CardPool, CommanderPairing, DeckCopyLimit, DeckSizeRule,
    DeckSizeSubject, FormatConfig, FormatGroup, GameFormat, SelectedFormat, SideboardPolicy,
};
use engine::types::game_state::{GameState, PlayerDeckPool};
use engine::types::match_config::{DeckCardCount, MatchPhase};
use engine::types::player::PlayerId;
use strum::IntoEnumIterator;

use super::format_axis_census;
use super::source_census;
use super::support;

fn repeat(name: &str, n: usize) -> Vec<String> {
    std::iter::repeat_n(name.to_string(), n).collect()
}

fn db() -> Option<&'static engine::database::CardDatabase> {
    support::shared_card_db()
}

/// A card that is not a legendary creature is
/// admitted, on both legs. The Commander contrast pads its main deck to
/// exactly 100 `Wastes` so the deck-size check (which the summary dispatcher
/// consults BEFORE eligibility) does not displace the eligibility refusal —
/// without the padding, an empty main deck would make the summary leg
/// early-return on deck size and never reach eligibility at all.
#[test]
fn freeform_commander_admits_a_card_that_is_not_a_legendary_creature() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let wastes_99 = repeat("Wastes", 99);

    for summary_only in [false, true] {
        for card in [
            "Sol Ring",
            "Lightning Bolt",
            "Llanowar Elves",
            "The One Ring",
        ] {
            let ffc = DeckCompatibilityRequest {
                commander: vec![card.to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &ffc);
            assert_eq!(
                result.selected_format_compatible,
                Some(true),
                "summary_only={summary_only} {card}: {:?}",
                result.selected_format_reasons
            );
            assert!(
                result.selected_format_reasons.is_empty(),
                "summary_only={summary_only} {card}"
            );

            let commander = DeckCompatibilityRequest {
                main_deck: wastes_99.clone(),
                commander: vec![card.to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &commander);
            assert_eq!(
                result.selected_format_reasons,
                vec![format!(
                    "Commander cards must be legendary creatures or explicitly allow being a \
                     commander: {card}"
                )],
                "summary_only={summary_only} {card}"
            );
        }
    }
}

/// A land may not be this format's commander, including
/// the land-plus-another-type boundary case (CR 305.9) and a transforming
/// DFC's land front face.
#[test]
fn freeform_commander_refuses_a_land_as_commander() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        for card in [
            "Forest",
            "Dryad Arbor",
            "Barracks of the Thousand",
            "Westvale Abbey",
        ] {
            let request = DeckCompatibilityRequest {
                commander: vec![card.to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &request);
            assert_eq!(
                result.selected_format_reasons,
                vec![format!(
                    "Freeform Commander commanders must be cards that can be cast: {card}"
                )],
                "summary_only={summary_only} {card}"
            );
            assert_eq!(
                result.selected_format_compatible,
                Some(false),
                "summary_only={summary_only} {card}"
            );
        }
    }
}

/// The widened predicate's own coverage
/// boundary. A nontraditional command-zone card
/// type (CR 108.2a) is refused the same way a land is — CR 311.2 (Plane),
/// CR 314.2 (Scheme), CR 312.2 (Phenomenon), and CR 315.3 (Conspiracy) each
/// say explicitly "They... can't be cast". The positive control in the same
/// loop is an ordinary castable, non-legendary, non-creature card: without
/// it, a predicate that refused every commander would also satisfy the
/// refusals above.
#[test]
fn freeform_commander_refuses_a_nontraditional_card_as_commander() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        for card in [
            "Bad Wolf Bay",
            "A Premonition of Your Demise",
            "Caught in a Parallel Universe",
            "Power Play",
        ] {
            let request = DeckCompatibilityRequest {
                commander: vec![card.to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &request);
            assert_eq!(
                result.selected_format_reasons,
                vec![format!(
                    "Freeform Commander commanders must be cards that can be cast: {card}"
                )],
                "summary_only={summary_only} {card}"
            );
            assert_eq!(
                result.selected_format_compatible,
                Some(false),
                "summary_only={summary_only} {card}"
            );
        }

        let control = DeckCompatibilityRequest {
            commander: vec!["Sol Ring".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &control);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );
    }
}

/// `is_freeform_commander_eligible`'s
/// `!core_types.is_empty()` guard. `Iterator::all` is vacuously `true` on an
/// empty iterator, so without the guard a face with no recognized `CoreType`
/// would be admitted rather than refused. `Ashnod` is a Vanguard face
/// (CR 313.1 + CR 313.2: "can't be cast"), a CR 313 card type this engine's
/// `CoreType` enum has no variant for, so MTGJSON's card-type extraction
/// leaves `core_types` empty rather than populating a `Vanguard` arm. The
/// positive control is the same admitted, castable, non-legendary card used
/// above.
#[test]
fn freeform_commander_refuses_a_card_with_no_recognized_core_type() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let request = DeckCompatibilityRequest {
            commander: vec!["Ashnod".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &request);
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "Freeform Commander commanders must be cards that can be cast: Ashnod".to_string()
            ],
            "summary_only={summary_only}"
        );
        assert_eq!(
            result.selected_format_compatible,
            Some(false),
            "summary_only={summary_only}"
        );

        let control = DeckCompatibilityRequest {
            commander: vec!["Sol Ring".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &control);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );
    }
}

/// The predicate judges the
/// face the decklist NAMES. `Kazandu Mammoth` (the non-land face of a modal
/// DFC) is accepted and `Kazandu Valley` (the land face) is refused; the same
/// pair is asserted for Commander's own `Ormendahl, Profane Prince` /
/// `Westvale Abbey` (a transforming DFC) to record that this face-per-name
/// resolution is pre-existing.
#[test]
fn freeform_commander_judges_the_double_faced_card_face_the_decklist_names() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let wastes_99 = repeat("Wastes", 99);

    for summary_only in [false, true] {
        let mammoth = DeckCompatibilityRequest {
            commander: vec!["Kazandu Mammoth".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &mammoth);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );

        let valley = DeckCompatibilityRequest {
            commander: vec!["Kazandu Valley".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &valley);
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "Freeform Commander commanders must be cards that can be cast: Kazandu Valley"
                    .to_string()
            ],
            "summary_only={summary_only}"
        );

        let ormendahl = DeckCompatibilityRequest {
            main_deck: wastes_99.clone(),
            commander: vec!["Ormendahl, Profane Prince".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &ormendahl);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );

        let westvale = DeckCompatibilityRequest {
            main_deck: wastes_99.clone(),
            commander: vec!["Westvale Abbey".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &westvale);
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "Commander cards must be legendary creatures or explicitly allow being a \
                 commander: Westvale Abbey"
                    .to_string()
            ],
            "summary_only={summary_only}"
        );
    }
}

/// Eligibility governs EACH commander slot, not only the first: both slot
/// orders of `[Tymna the Weaver, Forest]` refuse naming `Forest`. The full
/// leg also carries the pairing reason (Tymna's Partner keyword does not
/// pair with a card that carries none); the summary leg's eligibility loop
/// returns at the first ineligible commander, before the pairing check runs.
#[test]
fn freeform_commander_eligibility_governs_each_commander_slot() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        for commander in [
            vec!["Tymna the Weaver".to_string(), "Forest".to_string()],
            vec!["Forest".to_string(), "Tymna the Weaver".to_string()],
        ] {
            let request = DeckCompatibilityRequest {
                commander: commander.clone(),
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &request);
            let eligibility =
                "Freeform Commander commanders must be cards that can be cast: Forest".to_string();
            let expected = if summary_only {
                vec![eligibility]
            } else {
                vec![
                    eligibility,
                    format!(
                        "Invalid partner pairing: {} and {} do not have compatible partner \
                         keywords",
                        commander[0], commander[1]
                    ),
                ]
            };
            assert_eq!(
                result.selected_format_reasons, expected,
                "summary_only={summary_only} commander={commander:?}"
            );
        }
    }
}

/// How many of `text`'s own lines contain `needle` on the CODE half — this
/// file's own route through the comment-stripping authority
/// (`super::source_census::code`), mirroring `format_axis_census`'s private
/// `count_needle`. Not that sibling's function itself: this file independently
/// `include_str!`s a `.rs` path (`ENGINE_WASM_LIB_RS` below) and counts a
/// needle in it, so it is itself in `source_census`'s producer population
/// (see `source_census::tests::no_source_reading_file_carries_a_private_comment_policy`)
/// and must route directly rather than through a peer's private routing.
fn count_needle(text: &str, needle: &str) -> usize {
    text.lines()
        .filter(|line| source_census::code(line).contains(needle))
        .count()
}

/// Establishes separately that the verdict is reached through
/// the surface the client actually calls — the OTHER client-called surface,
/// `engine-wasm::is_card_commander_eligible_for_format`. This export takes
/// `JsValue` and is reachable only from a wasm target, so unlike
/// `validate_deck_list_seats` (driven natively — see
/// `crates/engine-wasm/src/lib.rs`'s `deck_list_seat_validation_tests`) this
/// stays a SOURCE CENSUS: a positive control on the pre-existing
/// `TinyLeaders` arm proves the extraction reads the real function body
/// rather than an empty or truncated span. What would establish this more
/// strongly: a client integration test calling
/// `isCardCommanderEligibleForFormat` against a freshly built `.wasm`.
#[test]
fn freeform_commander_eligibility_is_answered_by_the_surface_the_client_calls() {
    const ENGINE_WASM_LIB_RS: &str = include_str!("../../../engine-wasm/src/lib.rs");
    let span = format_axis_census::fn_span(
        ENGINE_WASM_LIB_RS,
        "pub fn is_card_commander_eligible_for_format(",
    );
    // `count_needle` (not `str::contains`) so a needle written inside a
    // comment in this span cannot satisfy the assertion — `fn_span` strips
    // comments only to find the span's BOUNDARIES; the slice it returns is
    // raw source. `== 1` rather than `>= 1`: each needle names a single
    // match arm, so a duplicate arm is itself a defect this assertion
    // should catch, not tolerate.
    assert_eq!(
        count_needle(
            span,
            "GameFormat::FreeformCommander => is_freeform_commander_eligible(face)"
        ),
        1,
        "{span}"
    );
    assert_eq!(
        count_needle(
            span,
            "GameFormat::TinyLeaders => is_tiny_leader_eligible(face)"
        ),
        1,
        "{span}"
    );
}

/// The count boundary: exactly one or two commanders are admitted, three are
/// refused, on both legs.
#[test]
fn freeform_commander_admits_one_commander_and_refuses_three() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let one = DeckCompatibilityRequest {
            commander: vec!["Tymna the Weaver".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &one);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );

        let three = DeckCompatibilityRequest {
            commander: vec![
                "Tymna the Weaver".to_string(),
                "Reyhan, Last of the Abzan".to_string(),
                "Sol Ring".to_string(),
            ],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &three);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Freeform Commander decks require 1 or 2 commanders (found 3)".to_string()],
            "summary_only={summary_only}"
        );
    }
}

/// Two admitted where the ordinary CR 702.124 partner rule admits (both carry
/// generic Partner); two refused where it does not (neither carries any
/// partner ability, even though this format's widened eligibility admits
/// both individually).
#[test]
fn freeform_commander_admits_a_pair_the_ordinary_partner_rule_admits() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let generic_partners = DeckCompatibilityRequest {
            commander: vec![
                "Tymna the Weaver".to_string(),
                "Reyhan, Last of the Abzan".to_string(),
            ],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &generic_partners);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );

        let no_partner_ability = DeckCompatibilityRequest {
            commander: vec!["Sol Ring".to_string(), "Lightning Bolt".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &no_partner_ability);
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "Invalid partner pairing: Sol Ring and Lightning Bolt do not have compatible \
                 partner keywords"
                    .to_string()
            ],
            "summary_only={summary_only}"
        );
    }
}

/// An implementation gating on "both members carry a partner
/// keyword" refuses this pair while being wrong. `Veteran Soldier` carries no partner keyword
/// at all — it pairs with `Abdel Adrian, Gorion's Ward`'s "Choose a
/// Background" (CR 702.124k) through `subtype_partner_match`, on its
/// Background subtype. The SAME pair is asserted `Ok` under
/// `FormatConfig::commander()` too: that pairing already held, so this pair alone establishes nothing this format
/// created.
#[test]
fn freeform_commander_admits_an_asymmetric_partner_family_pair() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let wastes_98 = repeat("Wastes", 98);

    for summary_only in [false, true] {
        for pair in [
            vec![
                "Abdel Adrian, Gorion's Ward".to_string(),
                "Veteran Soldier".to_string(),
            ],
            vec![
                "Veteran Soldier".to_string(),
                "Abdel Adrian, Gorion's Ward".to_string(),
            ],
        ] {
            let ffc = DeckCompatibilityRequest {
                commander: pair.clone(),
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &ffc);
            assert_eq!(
                result.selected_format_compatible,
                Some(true),
                "summary_only={summary_only} {pair:?}: {:?}",
                result.selected_format_reasons
            );
            assert!(
                result.selected_format_reasons.is_empty(),
                "summary_only={summary_only} {pair:?}"
            );

            let commander = DeckCompatibilityRequest {
                main_deck: wastes_98.clone(),
                commander: pair.clone(),
                selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            };
            let result = evaluate_deck_compatibility(db, &commander);
            assert_eq!(
                result.selected_format_compatible,
                Some(true),
                "summary_only={summary_only} {pair:?} (Commander baseline): {:?}",
                result.selected_format_reasons
            );
        }
    }
}

/// Two sub-classes, each pinned with its measured answer, on the
/// FULL leg (`validate_name_deck_for_format_full`): a card offered as one of
/// a pair, and a legendary non-creature that is NOT a Background. Both are
/// refused for pairing under Freeform Commander (this
/// format's widened eligibility admits the card itself), and for BOTH
/// eligibility and pairing under Commander. The Commander contrast pads to
/// exactly 100 `Wastes` so the isolated reason is the pairing/eligibility
/// pair alone.
#[test]
fn freeform_commander_pairing_still_refuses_a_card_its_eligibility_admits() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let wastes_98 = repeat("Wastes", 98);

    // Sub-class A: no partner ability at all.
    let ffc_a = validate_name_deck_for_format_full(
        db,
        &[],
        &[],
        &[
            "Abdel Adrian, Gorion's Ward".to_string(),
            "Llanowar Elves".to_string(),
        ],
        &[],
        &[],
        &[],
        &[],
        &[],
        &FormatConfig::freeform_commander(),
        None,
        2,
    );
    assert_eq!(
        ffc_a,
        Err(vec![
            "Invalid partner pairing: Abdel Adrian, Gorion's Ward and Llanowar Elves do not \
             have compatible partner keywords"
                .to_string()
        ])
    );

    let commander_a = validate_name_deck_for_format_full(
        db,
        &wastes_98,
        &[],
        &[
            "Abdel Adrian, Gorion's Ward".to_string(),
            "Llanowar Elves".to_string(),
        ],
        &[],
        &[],
        &[],
        &[],
        &[],
        &FormatConfig::commander(),
        None,
        2,
    );
    assert_eq!(
        commander_a,
        Err(vec![
            "Commander cards must be legendary creatures or explicitly allow being a \
             commander: Llanowar Elves"
                .to_string(),
            "Invalid partner pairing: Abdel Adrian, Gorion's Ward and Llanowar Elves do not \
             have compatible partner keywords"
                .to_string(),
        ])
    );

    // Sub-class B: legendary non-creature, not a Background.
    let ffc_b = validate_name_deck_for_format_full(
        db,
        &[],
        &[],
        &[
            "Abdel Adrian, Gorion's Ward".to_string(),
            "The One Ring".to_string(),
        ],
        &[],
        &[],
        &[],
        &[],
        &[],
        &FormatConfig::freeform_commander(),
        None,
        2,
    );
    assert_eq!(
        ffc_b,
        Err(vec![
            "Invalid partner pairing: Abdel Adrian, Gorion's Ward and The One Ring do not have \
             compatible partner keywords"
                .to_string()
        ])
    );

    let commander_b = validate_name_deck_for_format_full(
        db,
        &wastes_98,
        &[],
        &[
            "Abdel Adrian, Gorion's Ward".to_string(),
            "The One Ring".to_string(),
        ],
        &[],
        &[],
        &[],
        &[],
        &[],
        &FormatConfig::commander(),
        None,
        2,
    );
    assert_eq!(
        commander_b,
        Err(vec![
            "Commander cards must be legendary creatures or explicitly allow being a \
             commander: The One Ring"
                .to_string(),
            "Invalid partner pairing: Abdel Adrian, Gorion's Ward and The One Ring do not have \
             compatible partner keywords"
                .to_string(),
        ])
    );
}

/// CONTRAST pair and SAME-FORMAT control. The control is
/// `Forest` added as a second commander to the deck under test: its refusal
/// is `CommanderVariantRules::freeform_commander()`'s own `eligibility_error`
/// field, which differs from Commander's when the wrong variant rules are
/// handed to the shared evaluator — MEASURED by mutation to
/// discriminate the mis-dispatch.
#[test]
fn freeform_commander_has_no_main_deck_size_floor() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let ten_swamp = repeat("Swamp", 10);

    for summary_only in [false, true] {
        let accepted = DeckCompatibilityRequest {
            main_deck: ten_swamp.clone(),
            commander: vec!["Tymna the Weaver".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &accepted);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );

        let commander_contrast = DeckCompatibilityRequest {
            main_deck: ten_swamp.clone(),
            commander: vec!["Tymna the Weaver".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &commander_contrast);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Commander deck must have exactly 100 cards (found 11)".to_string()],
            "summary_only={summary_only}"
        );

        let control = DeckCompatibilityRequest {
            main_deck: ten_swamp.clone(),
            commander: vec!["Tymna the Weaver".to_string(), "Forest".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &control);
        let eligibility =
            "Freeform Commander commanders must be cards that can be cast: Forest".to_string();
        let expected = if summary_only {
            vec![eligibility]
        } else {
            vec![
                eligibility,
                "Invalid partner pairing: Tymna the Weaver and Forest do not have compatible \
                 partner keywords"
                    .to_string(),
            ]
        };
        assert_eq!(
            result.selected_format_reasons, expected,
            "summary_only={summary_only}"
        );
    }
}

/// The degenerate end: an empty main deck, a 1-card main deck,
/// and a main deck that names the commander itself are all accepted.
#[test]
fn freeform_commander_accepts_an_empty_main_deck() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &[],
            &[],
            &["Tymna the Weaver".to_string()],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform_commander(),
            None,
            2,
        ),
        Ok(()),
        "an empty main deck plus a commander must be accepted"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &repeat("Swamp", 1),
            &[],
            &["Tymna the Weaver".to_string()],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform_commander(),
            None,
            2,
        ),
        Ok(()),
        "a 1-card main deck plus a commander must be accepted"
    );

    assert_eq!(
        validate_name_deck_for_format_full(
            db,
            &["Sol Ring".to_string()],
            &[],
            &["Sol Ring".to_string()],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::freeform_commander(),
            None,
            2,
        ),
        Ok(()),
        "the commander named in the main deck too must be accepted"
    );
}

/// The deck-size subject is declared, not
/// fallen through. `GameFormat::FreeformCommander.deck_size_subject()` is
/// `MainDeckAndCommanders` — measurably inert for this format's verdicts either way, since
/// `DeckSizeRule::Minimum(0).accepts(n)` holds for every count — and every
/// other command-zone format still answers its own subject from the
/// property that generates the set, never a hand-copied list.
#[test]
fn freeform_commander_declares_its_deck_size_subject() {
    assert_eq!(
        GameFormat::FreeformCommander.deck_size_subject(),
        DeckSizeSubject::MainDeckAndCommanders
    );

    let expected: &[(GameFormat, DeckSizeSubject)] = &[
        (
            GameFormat::Commander,
            DeckSizeSubject::MainDeckAndCommanders,
        ),
        (
            GameFormat::PauperCommander,
            DeckSizeSubject::MainDeckAndCommanders,
        ),
        (
            GameFormat::DuelCommander,
            DeckSizeSubject::MainDeckAndCommanders,
        ),
        (
            GameFormat::TinyLeaders,
            DeckSizeSubject::MainDeckAndCommanders,
        ),
        (
            GameFormat::Oathbreaker,
            DeckSizeSubject::MainDeckAndCommandZone,
        ),
        (GameFormat::Brawl, DeckSizeSubject::MainDeckAndCommanders),
        (
            GameFormat::HistoricBrawl,
            DeckSizeSubject::MainDeckAndCommanders,
        ),
        (
            GameFormat::CommanderDraft,
            DeckSizeSubject::MainDeckAndCommanders,
        ),
    ];
    let others: Vec<GameFormat> = GameFormat::iter()
        .filter(|f| {
            f.command_zone_holds_decklist_commander() == Ok(true)
                && *f != GameFormat::FreeformCommander
        })
        .collect();
    let expected_formats: Vec<GameFormat> = expected.iter().map(|(f, _)| *f).collect();
    assert_eq!(
        others, expected_formats,
        "the filter over command_zone_holds_decklist_commander() is what generates this \
         table's membership, not a hand-copied list"
    );
    for (format, subject) in expected {
        assert_eq!(format.deck_size_subject(), *subject, "{format:?}");
    }
}

/// No copy limit, with a Commander contrast, a positive control
/// showing the Commander refusal is the copy rule and nothing else, and the
/// SAME-FORMAT control: the same 8-copy deck and commander slots
/// under `FormatConfig::commander()` DO carry a copy-limit reason in the
/// same accumulating vector, so its absence under Freeform Commander is
/// informative rather than a silent gap.
#[test]
fn freeform_commander_has_no_copy_limit() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let mut eight_sol_ring = repeat("Sol Ring", 8);
    eight_sol_ring.extend(repeat("Swamp", 91));
    let mut one_sol_ring = vec!["Sol Ring".to_string()];
    one_sol_ring.extend(repeat("Swamp", 98));

    for summary_only in [false, true] {
        let accepted = DeckCompatibilityRequest {
            main_deck: eight_sol_ring.clone(),
            commander: vec!["Tymna the Weaver".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &accepted);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );

        let commander_contrast = DeckCompatibilityRequest {
            main_deck: eight_sol_ring.clone(),
            commander: vec!["Tymna the Weaver".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &commander_contrast);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Singleton violations: Sol Ring (8 copies)".to_string()],
            "summary_only={summary_only}"
        );

        // Positive control: the identical shape with only ONE Sol Ring is
        // legal under Commander, showing the refusal above is the copy rule
        // and nothing else.
        let commander_one_copy = DeckCompatibilityRequest {
            main_deck: one_sol_ring.clone(),
            commander: vec!["Tymna the Weaver".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::Commander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &commander_one_copy);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );

        let control = DeckCompatibilityRequest {
            main_deck: eight_sol_ring.clone(),
            commander: vec!["Tymna the Weaver".to_string(), "Forest".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &control);
        let eligibility =
            "Freeform Commander commanders must be cards that can be cast: Forest".to_string();
        let expected = if summary_only {
            vec![eligibility]
        } else {
            vec![
                eligibility,
                "Invalid partner pairing: Tymna the Weaver and Forest do not have compatible \
                 partner keywords"
                    .to_string(),
            ]
        };
        assert_eq!(
            result.selected_format_reasons, expected,
            "summary_only={summary_only}: no copy-limit reason must appear, unlike the \
             Commander contrast above on the identical deck and slots"
        );
    }
}

/// Colour identity comes from the commander(s), including the case:
/// a sole commander that is neither
/// legendary nor a creature still contributes its own identity. The
/// sole-commander shape is used deliberately so the pairing answer is
/// not presumed.
#[test]
fn freeform_commander_colour_identity_comes_from_the_commanders() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let plains_ok = DeckCompatibilityRequest {
            main_deck: repeat("Plains", 1),
            commander: vec![
                "Abdel Adrian, Gorion's Ward".to_string(),
                "Veteran Soldier".to_string(),
            ],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &plains_ok);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );

        let swamp_violation = DeckCompatibilityRequest {
            main_deck: repeat("Swamp", 1),
            commander: vec![
                "Abdel Adrian, Gorion's Ward".to_string(),
                "Veteran Soldier".to_string(),
            ],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &swamp_violation);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Cards outside commander's color identity: Swamp".to_string()],
            "summary_only={summary_only}"
        );

        let mountain_ok = DeckCompatibilityRequest {
            main_deck: repeat("Mountain", 1),
            commander: vec!["Lightning Bolt".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &mountain_ok);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );

        let elves_violation = DeckCompatibilityRequest {
            main_deck: repeat("Llanowar Elves", 1),
            commander: vec!["Lightning Bolt".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &elves_violation);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Cards outside commander's color identity: Llanowar Elves".to_string()],
            "summary_only={summary_only}"
        );
    }
}

/// A colourless sole
/// commander admits only colourless cards — a BASIC LAND is not exempt from
/// CR 903.4's subset test.
#[test]
fn freeform_commander_empty_identity_commander_admits_only_colourless_cards() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let ornithopter_ok = DeckCompatibilityRequest {
            main_deck: repeat("Ornithopter", 1),
            commander: vec!["Sol Ring".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &ornithopter_ok);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: {:?}",
            result.selected_format_reasons
        );

        let bolt_violation = DeckCompatibilityRequest {
            main_deck: repeat("Lightning Bolt", 1),
            commander: vec!["Sol Ring".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &bolt_violation);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Cards outside commander's color identity: Lightning Bolt".to_string()],
            "summary_only={summary_only}"
        );

        let swamp_violation = DeckCompatibilityRequest {
            main_deck: repeat("Swamp", 1),
            commander: vec!["Sol Ring".to_string()],
            selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
            summary_only,
            ..DeckCompatibilityRequest::default()
        };
        let result = evaluate_deck_compatibility(db, &swamp_violation);
        assert_eq!(
            result.selected_format_reasons,
            vec!["Cards outside commander's color identity: Swamp".to_string()],
            "summary_only={summary_only}: a basic land is not exempt from CR 903.4"
        );
    }
}

/// 2 to 4 seats.
#[test]
fn freeform_commander_admits_two_to_four_seats() {
    let config = FormatConfig::freeform_commander();
    assert_eq!(
        config.validate_for_player_count(1),
        Err("player_count 1 is outside FreeformCommander's seat range 2-4".to_string())
    );
    assert!(config.validate_for_player_count(2).is_ok());
    assert!(config.validate_for_player_count(3).is_ok());
    assert!(config.validate_for_player_count(4).is_ok());
    assert_eq!(
        config.validate_for_player_count(5),
        Err("player_count 5 is outside FreeformCommander's seat range 2-4".to_string())
    );
}

/// The registry entry's group and short label, pinned.
#[test]
fn freeform_commander_is_in_the_commander_group() {
    let registry = GameFormat::registry();
    let entry = registry
        .iter()
        .find(|m| m.format == GameFormat::FreeformCommander)
        .expect("FreeformCommander must be in registry");
    assert_eq!(entry.group, FormatGroup::Commander);
    assert_eq!(entry.short_label, "FFC");
}

/// CR 903.7's 40 starting life, declared and pinned; the host can
/// adjust it through the existing `FormatConfig` admission gate (the paired
/// control is that a widened `max_players` is refused by that SAME gate,
/// showing it is actually running).
#[test]
fn freeform_commander_declares_forty_starting_life_and_the_host_can_change_it() {
    let config = FormatConfig::freeform_commander();
    assert_eq!(config.starting_life, 40);
    assert_eq!(config.starting_life_for_seat(), 40);
    assert!(validate_starting_life_bounds(&config).is_ok());

    let mut adjusted = config.clone();
    adjusted.starting_life = 20;
    let json = serde_json::to_value(&adjusted).unwrap();
    let round_tripped: FormatConfig = serde_json::from_value(json)
        .expect("starting_life is a HostChoiceWithin row: a host-adjusted value must round-trip");
    assert_eq!(round_tripped.starting_life, 20);

    let mut widened_seats = config;
    widened_seats.max_players = 8;
    let json = serde_json::to_value(&widened_seats).unwrap();
    let err = serde_json::from_value::<FormatConfig>(json)
        .expect_err("max_players outside the registry range must be refused");
    assert!(
        err.to_string().contains("FreeformCommander seats 2-4"),
        "{err}"
    );
}

/// The main deck and the commander slot both admit a card printed
/// only in an unreleased/preview set (FRA — Reality Fracture, no legality
/// row); Premodern refuses the identical 60 with the legality
/// reason alone. Both legs.
#[test]
fn freeform_commander_admits_a_card_printed_only_in_reality_fracture() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };
    let mut deck = repeat("Craterclaw Colossus", 4);
    deck.extend(repeat("Mountain", 56));

    for summary_only in [false, true] {
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: deck.clone(),
                commander: vec!["Craterclaw Colossus".to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: Freeform Commander's unrestricted pool admits a \
             card with no legality row, in both the main deck and the commander slot: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}"
        );

        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                commander: vec!["Craterclaw Colossus".to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "summary_only={summary_only}: the commander slot alone, with an empty main deck, \
             must also be accepted: {:?}",
            result.selected_format_reasons
        );
        assert!(
            result.selected_format_reasons.is_empty(),
            "summary_only={summary_only}, empty main deck"
        );

        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: deck.clone(),
                selected_format: Some(SelectedFormat::Tag(GameFormat::Premodern)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_reasons,
            vec!["Not Premodern legal: Craterclaw Colossus (not legal in Premodern)".to_string()],
            "summary_only={summary_only}"
        );
    }
}

/// Established at the axis: both formats declare the SAME `CardPool`
/// variant.
#[test]
fn freeform_commander_admits_the_same_pool_as_freeform() {
    assert_eq!(
        GameFormat::FreeformCommander.card_pool(),
        GameFormat::Freeform.card_pool()
    );
    assert_eq!(
        GameFormat::FreeformCommander.card_pool(),
        CardPool::Unrestricted
    );
}

/// Out-of-pool refusal classes that do NOT consult the card-pool
/// axis survive unchanged: CR 407.3's ante-card refusal, and a card's own
/// PRINTED deck-construction limit still binds under this format's
/// `Unlimited` default. No revert: this test pins UNCHANGED behaviour.
#[test]
fn freeform_commander_still_refuses_an_ante_card_and_a_printed_copy_limit_overrun() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    for summary_only in [false, true] {
        let mut ante_deck = vec!["Contract from Below".to_string()];
        ante_deck.extend(repeat("Swamp", 9));
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: ante_deck,
                commander: vec!["Tymna the Weaver".to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_reasons,
            vec![
                "Can't be in a deck or sideboard unless the game is played for ante: Contract \
                 from Below"
                    .to_string()
            ],
            "summary_only={summary_only}"
        );

        let over_limit = repeat("Vazal, the Compleat", 2);
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: over_limit,
                commander: vec!["Vazal, the Compleat".to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
                summary_only,
                ..DeckCompatibilityRequest::default()
            },
        );
        assert_eq!(
            result.selected_format_reasons,
            vec!["Singleton violations: Vazal, the Compleat (2 copies)".to_string()],
            "summary_only={summary_only}: a card's own printed deck-construction limit still \
             binds under this format's Unlimited default"
        );

        let at_limit = repeat("Vazal, the Compleat", 1);
        let result = evaluate_deck_compatibility(
            db,
            &DeckCompatibilityRequest {
                main_deck: at_limit,
                commander: vec!["Vazal, the Compleat".to_string()],
                selected_format: Some(SelectedFormat::Tag(GameFormat::FreeformCommander)),
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

/// This format declares `Forbidden`, pinned; the test reds if the
/// value changes.
#[test]
fn freeform_commander_declares_no_sideboard() {
    assert_eq!(
        GameFormat::FreeformCommander.sideboard_policy(),
        SideboardPolicy::Forbidden
    );

    let mut widened = FormatConfig::freeform_commander();
    widened.sideboard_policy = SideboardPolicy::Limited(15);
    let json = serde_json::to_value(&widened).unwrap();
    let err = serde_json::from_value::<FormatConfig>(json)
        .expect_err("a looser sideboard policy than Forbidden must be refused");
    assert!(err.to_string().contains("more permissive than"), "{err}");

    let unchanged = FormatConfig::freeform_commander();
    let json = serde_json::to_value(&unchanged).unwrap();
    assert!(serde_json::from_value::<FormatConfig>(json).is_ok());
}

/// The declaration is a BOUND AT ZERO, and the
/// pair straddles it at the surface that enforces it. STATED PLAINLY: no
/// deck-compatibility verdict distinguishes this format's sideboard value at
/// all, because `request_without_sideboard` strips the sideboard before
/// every commander check runs (MEASURED) — so the straddle below is
/// NOT a deck-level pair, and a reader must not take it for one. The pair is
/// taken instead at `match_flow::handle_submit_sideboard` (the function
/// `engine.rs::apply_non_priority_pass_action` dispatches a player's
/// `GameAction::SubmitSideboard` into) and at `load_deck_into_state`.
#[test]
fn freeform_commanders_sideboard_bound_is_behavioural() {
    fn card_entry(name: &str) -> DeckEntry {
        DeckEntry {
            card: CardFace {
                name: name.to_string(),
                ..Default::default()
            },
            count: 1,
        }
    }

    fn between_games_state(
        format_config: FormatConfig,
        registered_sideboard: Vec<DeckEntry>,
    ) -> GameState {
        let mut state = GameState::new(format_config, 2, 7);
        state.match_phase = MatchPhase::BetweenGames;
        state.deck_pools = vec![PlayerDeckPool {
            player: PlayerId(0),
            registered_main: std::sync::Arc::new(vec![card_entry("Plains")]),
            registered_sideboard: std::sync::Arc::new(registered_sideboard),
            ..Default::default()
        }];
        state
    }

    for (format_label, format_config, expect_sideboard_rejected) in [
        (
            "FreeformCommander",
            FormatConfig::freeform_commander(),
            true,
        ),
        ("Freeform", FormatConfig::freeform(), false),
    ] {
        // An EMPTY submission is always accepted.
        let mut state = between_games_state(format_config.clone(), Vec::new());
        let mut events = Vec::new();
        let empty_submission = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![DeckCardCount {
                name: "Plains".to_string(),
                count: 1,
            }],
            Vec::new(),
            &mut events,
        );
        assert!(
            empty_submission.is_ok(),
            "{format_label}: {empty_submission:?}"
        );

        // A ONE-CARD submission is where the two formats diverge.
        let mut state = between_games_state(format_config, vec![card_entry("Forest")]);
        let mut events = Vec::new();
        let one_card_submission = handle_submit_sideboard(
            &mut state,
            PlayerId(0),
            vec![DeckCardCount {
                name: "Plains".to_string(),
                count: 1,
            }],
            vec![DeckCardCount {
                name: "Forest".to_string(),
                count: 1,
            }],
            &mut events,
        );
        if expect_sideboard_rejected {
            assert_eq!(
                one_card_submission,
                Err("Sideboard has 1 cards (maximum 0)".to_string()),
                "{format_label}"
            );
        } else {
            assert!(
                one_card_submission.is_ok(),
                "{format_label}: {one_card_submission:?}"
            );
        }
    }

    // The same bound at the loader: `load_deck_into_state` leaves
    // `current_sideboard` empty for this format and non-empty for Freeform.
    for (format_label, format_config, expected_len) in [
        (
            "FreeformCommander",
            FormatConfig::freeform_commander(),
            0usize,
        ),
        ("Freeform", FormatConfig::freeform(), 1usize),
    ] {
        let mut state = GameState::new(format_config, 2, 7);
        let sideboard_card = DeckEntry {
            card: CardFace {
                name: "Test Sideboard Card".to_string(),
                ..Default::default()
            },
            count: 1,
        };
        let payload = DeckPayload {
            player: PlayerDeckPayload {
                sideboard: vec![sideboard_card.clone()],
                ..Default::default()
            },
            opponent: PlayerDeckPayload {
                sideboard: vec![sideboard_card],
                ..Default::default()
            },
            ..Default::default()
        };
        load_deck_into_state(&mut state, &payload);
        let p0 = state
            .deck_pools
            .iter()
            .find(|pool| pool.player == PlayerId(0))
            .expect("player 0 deck pool must exist after loading");
        assert_eq!(p0.current_sideboard.len(), expected_len, "{format_label}");
    }
}

/// The class sweep: no OTHER command-zone format's axes
/// moved. The membership is generated by the same filter
/// (`command_zone_holds_decklist_commander() == Ok(true)`), never a
/// hand-copied list, so a format silently dropped from the filter would be
/// caught by the equality assertion against that filtered list rather than
/// by an empty loop.
#[test]
fn no_other_command_zone_format_s_rules_moved() {
    struct Expected {
        format: GameFormat,
        pairing: CommanderPairing,
        sideboard: SideboardPolicy,
        copy_limit: DeckCopyLimit,
        deck_size_subject: DeckSizeSubject,
        pool: CardPool,
        deck_size: DeckSizeRule,
        min_players: u8,
        max_players: u8,
    }

    let expected = [
        Expected {
            format: GameFormat::Commander,
            pairing: CommanderPairing::PartnerFamilies,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::LegalityTable(LegalityFormat::Commander),
            deck_size: DeckSizeRule::Exactly(100),
            min_players: 2,
            max_players: 6,
        },
        Expected {
            format: GameFormat::PauperCommander,
            pairing: CommanderPairing::PartnerFamilies,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::LegalityTable(LegalityFormat::PauperCommander),
            deck_size: DeckSizeRule::Exactly(100),
            min_players: 2,
            max_players: 6,
        },
        Expected {
            format: GameFormat::DuelCommander,
            pairing: CommanderPairing::PartnerFamilies,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::LegalityTable(LegalityFormat::DuelCommander),
            deck_size: DeckSizeRule::Exactly(100),
            min_players: 2,
            max_players: 2,
        },
        Expected {
            format: GameFormat::TinyLeaders,
            pairing: CommanderPairing::PartnerFamilies,
            sideboard: SideboardPolicy::Limited(10),
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::NoEngineAuthority,
            deck_size: DeckSizeRule::Exactly(50),
            min_players: 2,
            max_players: 2,
        },
        Expected {
            format: GameFormat::Oathbreaker,
            pairing: CommanderPairing::Solo,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommandZone,
            pool: CardPool::NoEngineAuthority,
            deck_size: DeckSizeRule::Exactly(60),
            min_players: 2,
            max_players: 4,
        },
        Expected {
            format: GameFormat::Brawl,
            pairing: CommanderPairing::Solo,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::LegalityTable(LegalityFormat::StandardBrawl),
            deck_size: DeckSizeRule::Exactly(60),
            min_players: 2,
            max_players: 2,
        },
        Expected {
            format: GameFormat::HistoricBrawl,
            pairing: CommanderPairing::Solo,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::UpTo(1),
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::LegalityTable(LegalityFormat::Brawl),
            deck_size: DeckSizeRule::Exactly(100),
            min_players: 2,
            max_players: 2,
        },
        Expected {
            format: GameFormat::CommanderDraft,
            pairing: CommanderPairing::PartnerFamilies,
            sideboard: SideboardPolicy::Forbidden,
            copy_limit: DeckCopyLimit::Unlimited,
            deck_size_subject: DeckSizeSubject::MainDeckAndCommanders,
            pool: CardPool::NoEngineAuthority,
            deck_size: DeckSizeRule::Minimum(60),
            min_players: 3,
            max_players: 8,
        },
    ];

    let others: Vec<GameFormat> = GameFormat::iter()
        .filter(|f| {
            f.command_zone_holds_decklist_commander() == Ok(true)
                && *f != GameFormat::FreeformCommander
        })
        .collect();
    let expected_formats: Vec<GameFormat> = expected.iter().map(|e| e.format).collect();
    assert_eq!(
        others, expected_formats,
        "the filter is what generates this table's membership, not a hand-copied list"
    );

    let registry = GameFormat::registry();
    for e in &expected {
        assert_eq!(e.format.commander_pairing(), e.pairing, "{:?}", e.format);
        assert_eq!(e.format.sideboard_policy(), e.sideboard, "{:?}", e.format);
        assert_eq!(
            e.format.default_deck_copy_limit(),
            e.copy_limit,
            "{:?}",
            e.format
        );
        assert_eq!(
            e.format.deck_size_subject(),
            e.deck_size_subject,
            "{:?}",
            e.format
        );
        assert_eq!(e.format.card_pool(), e.pool, "{:?}", e.format);

        let config = FormatConfig::for_format(e.format).unwrap();
        assert_eq!(config.deck_size, e.deck_size, "{:?}", e.format);
        assert_eq!(config.min_players, e.min_players, "{:?}", e.format);
        assert_eq!(config.max_players, e.max_players, "{:?}", e.format);

        let meta = registry
            .iter()
            .find(|m| m.format == e.format)
            .unwrap_or_else(|| panic!("{:?} must be in the registry", e.format));
        assert_eq!(meta.group, FormatGroup::Commander, "{:?}", e.format);
    }
}

/// The eligibility half of the class sweep:
/// `commander = [Sol Ring]` is refused by every OTHER
/// command-zone format, each naming its own eligibility message. Membership
/// (`.any`), not the file's usual full-vector equality: with an empty main
/// deck each format also emits its own deck-size reason (and Oathbreaker
/// wants a signature spell), so a full-vector form would need a bespoke
/// vector per format.
#[test]
fn no_other_command_zone_format_admits_a_card_this_format_now_admits() {
    let Some(db) = db() else {
        eprintln!("skipping: card database not available");
        return;
    };

    let cases: &[(GameFormat, &str)] = &[
        (
            GameFormat::Commander,
            "Commander cards must be legendary creatures or explicitly allow being a \
             commander: Sol Ring",
        ),
        (
            GameFormat::DuelCommander,
            "Duel Commander cards must be legendary creatures or explicitly allow being a \
             commander: Sol Ring",
        ),
        (
            GameFormat::PauperCommander,
            "Pauper Commander commander must be an uncommon creature, Vehicle, or Spacecraft: \
             Sol Ring",
        ),
        (
            GameFormat::Brawl,
            "Brawl commander must be a legendary creature or legendary planeswalker: Sol Ring",
        ),
        (
            GameFormat::HistoricBrawl,
            "Historic Brawl commander must be a legendary creature or legendary planeswalker: \
             Sol Ring",
        ),
        (
            GameFormat::TinyLeaders,
            "Tiny Leader must be a legendary creature, Vehicle, Spacecraft, planeswalker, or \
             explicitly allow being a commander: Sol Ring",
        ),
        (
            GameFormat::Oathbreaker,
            "Sol Ring: Oathbreaker must be a legendary Planeswalker",
        ),
        (
            GameFormat::CommanderDraft,
            "Commander Draft cards must be legendary creatures or explicitly allow being a \
             commander: Sol Ring",
        ),
    ];

    for (format, message) in cases {
        let result = validate_name_deck_for_format_full(
            db,
            &[],
            &[],
            &["Sol Ring".to_string()],
            &[],
            &[],
            &[],
            &[],
            &[],
            &FormatConfig::for_format(*format).unwrap(),
            None,
            2,
        );
        let reasons =
            result.expect_err("Sol Ring must be refused by every other command-zone format");
        assert!(
            reasons.iter().any(|r| r.as_str() == *message),
            "{format:?}: expected {message:?} among {reasons:?}"
        );
    }
}

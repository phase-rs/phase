//! CR 903.13f Commander Draft deck construction (U9), through the production
//! entry point `evaluate_deck_compatibility`.
//!
//! FIXTURE CONVENTION, stated because it is load-bearing. The validator
//! computes the UNION `main_deck.len() + (commander.len() - represented_in_main)`,
//! so it is convention-agnostic per logical deck but NOT agnostic about what a
//! fixture author means by "a 59-card deck". Every fixture here is
//! commanders-INSIDE: `main_deck` carries the complete list and the designated
//! commander is a member of it (CR 903.5a, "including its commander", which
//! CR 903.13f(1) rescales to "at least 60" without disturbing the "including").
//!
//! Writing these the way the existing commander fixtures in `deck_validation.rs`
//! are written — N main cards plus a SEPARATE commander absent from main —
//! would give `total_cards = N + 1` and turn the 59-card rejection into a
//! 60-card acceptance: a green-looking test asserting nothing.

use std::collections::BTreeMap;

use engine::database::CardDatabase;
use engine::game::{
    evaluate_deck_compatibility, is_commander_eligible, validate_name_deck_for_format_full,
    DeckCompatibilityRequest,
};
use engine::types::card::CardFace;
use engine::types::card_type::{CardType, CoreType, Supertype};
use engine::types::format::{FormatConfig, GameFormat};
use engine::types::keywords::{Keyword, PartnerType};
use engine::types::mana::ManaColor;

const COMMANDER: &str = "Legal Commander";
const OFF_COLOR: &str = "Off Color Card";

fn creature(name: &str, legendary: bool, colors: Vec<ManaColor>) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: if legendary {
                vec![Supertype::Legendary]
            } else {
                Vec::new()
            },
            core_types: vec![CoreType::Creature],
            subtypes: Vec::new(),
        },
        color_identity: colors,
        ..CardFace::default()
    }
}

fn basic_land(name: &str, colors: Vec<ManaColor>) -> CardFace {
    CardFace {
        name: name.to_string(),
        card_type: CardType {
            supertypes: vec![Supertype::Basic],
            core_types: vec![CoreType::Land],
            subtypes: vec![name.to_string()],
        },
        color_identity: colors,
        ..CardFace::default()
    }
}

/// Every fixture card is legal in Commander EXCEPT `Draft Only Card`, which
/// carries no Commander legality entry at all — that is the axis row 5 uses.
fn test_db() -> CardDatabase {
    let faces = vec![
        creature(COMMANDER, true, vec![ManaColor::White]),
        creature("In Color Card", false, vec![ManaColor::White]),
        creature(OFF_COLOR, false, vec![ManaColor::Blue]),
        creature("Draft Only Card", false, vec![ManaColor::White]),
        basic_land("Plains", vec![ManaColor::White]),
        creature("Unpairable Legend", true, vec![ManaColor::White]),
        // CR 903.13f(3)'s bound is "one or fewer colors", so a TWO-colour
        // legend is outside the grant no matter which draft produced it. Row 7
        // uses it to prove the grant WIDENS the pairing rule rather than
        // disabling it.
        creature(
            "Two Color Legend",
            true,
            vec![ManaColor::White, ManaColor::Blue],
        ),
        CardFace {
            keywords: vec![Keyword::Partner(PartnerType::Generic)],
            ..creature("Partner Legend A", true, vec![ManaColor::White])
        },
        CardFace {
            keywords: vec![Keyword::Partner(PartnerType::Generic)],
            ..creature("Partner Legend B", true, vec![ManaColor::White])
        },
    ];

    let mut entries = BTreeMap::new();
    for f in faces {
        let mut obj = serde_json::to_value(&f).unwrap();
        let legalities = if f.name == "Draft Only Card" {
            serde_json::json!({})
        } else {
            serde_json::json!({ "commander": "legal" })
        };
        obj.as_object_mut()
            .unwrap()
            .insert("legalities".to_string(), legalities);
        entries.insert(f.name.to_lowercase(), obj);
    }
    CardDatabase::from_json_str(&serde_json::to_string(&entries).unwrap()).unwrap()
}

/// A commanders-INSIDE Commander Draft request: `main_deck` is the complete
/// list and already contains `COMMANDER`.
fn request(main_deck: Vec<String>, commander: Vec<String>) -> DeckCompatibilityRequest {
    DeckCompatibilityRequest {
        main_deck,
        commander,
        selected_format: Some(GameFormat::CommanderDraft),
        ..DeckCompatibilityRequest::default()
    }
}

/// `n` cards: one `COMMANDER` plus `n - 1` in-colour cards, `repeats` of which
/// share a name (CR 903.13f(2) makes that legal).
fn deck(n: usize, repeats: usize) -> Vec<String> {
    let mut d = vec![COMMANDER.to_string()];
    d.extend(std::iter::repeat_n("In Color Card".to_string(), repeats));
    while d.len() < n {
        d.push("Plains".to_string());
    }
    assert_eq!(d.len(), n, "fixture must be a {n}-card logical deck");
    d
}

fn verdict(db: &CardDatabase, req: &DeckCompatibilityRequest) -> (bool, Vec<String>) {
    let result = evaluate_deck_compatibility(db, req);
    (
        result.selected_format_compatible.unwrap(),
        result.selected_format_reasons,
    )
}

/// U9 row 1 — positive reach guard. A 60-card Commander Draft deck with a legal
/// commander and one card repeated FOUR times is compatible.
///
/// NOT the discriminating assertion: on the pre-change tree the Commander Draft
/// arm returned `compatible()` unconditionally, so this passed vacuously. It
/// exists to pair the negatives below, which the pre-change tree fails because
/// it never rejected anything.
#[test]
fn commander_draft_deck_allows_duplicates_but_enforces_color_identity() {
    let db = test_db();

    let (compatible, reasons) = verdict(&db, &request(deck(60, 4), vec![COMMANDER.to_string()]));
    assert!(
        compatible,
        "expected a legal 60-card deck to pass: {reasons:?}"
    );

    // U9 row 2 — CR 903.5c via CR 903.13f: colour identity IS enforced. Same
    // deck, one card swapped for one outside the commander's identity.
    let mut off_color = deck(60, 4);
    off_color.pop();
    off_color.push(OFF_COLOR.to_string());
    let (compatible, reasons) = verdict(&db, &request(off_color, vec![COMMANDER.to_string()]));
    assert!(
        !compatible,
        "a card outside the commander's colour identity must be rejected"
    );
    assert_eq!(
        reasons,
        vec!["Cards outside commander's color identity: Off Color Card"],
        "the engine must retain the exact named colour-identity reason"
    );
}

/// U9 row 3 — CR 903.13f(1): "A player's deck must contain at least 60 cards.
/// There is no maximum deck size."
///
/// The 61-card half is the one an `Exactly` rule gets wrong, and it is why the
/// hard-coded `total_cards != 100` had to go.
#[test]
fn commander_draft_enforces_a_minimum_deck_size_and_no_maximum() {
    let db = test_db();

    let (compatible, _) = verdict(&db, &request(deck(59, 4), vec![COMMANDER.to_string()]));
    assert!(!compatible, "CR 903.13f(1): 59 cards is below the minimum");

    let (compatible, reasons) = verdict(&db, &request(deck(61, 4), vec![COMMANDER.to_string()]));
    assert!(
        compatible,
        "CR 903.13f(1) sets NO maximum deck size: {reasons:?}"
    );
}

/// U9 row 4 — CR 903.13f(2): "A player's deck may include any number of cards
/// from that player's card pool with the same name", so the CR 903.5b singleton
/// rule must NOT fire.
///
/// The negative is paired with its sibling positive: the SAME deck shape under
/// `GameFormat::Commander` DOES report a singleton violation. Without that,
/// "no singleton reason" would be satisfied vacuously by the check
/// short-circuiting somewhere earlier.
///
/// Both halves run under BOTH dispatches. `evaluate_deck_compatibility` splits
/// on `summary_only` into two independent commander validators
/// (`evaluate_commander_with_format` and `quick_commander_check`), each holding
/// its own copy-limit call site, and the deck builder reaches the summary one.
/// Fixtures that leave `summary_only` at its `false` default exercise one of
/// the two and assert an agreement they never checked. The deck is 100 cards so
/// the constructed run clears CR 903.5a and reaches the copy-limit check on the
/// early-returning summary path; CR 903.13f(1) sets no maximum, so the same
/// list is a legal draft deck.
#[test]
fn commander_draft_suppresses_the_singleton_rule() {
    let db = test_db();
    let draft_main = deck(60, 4);
    let constructed_main = deck(100, 4);

    for summary_only in [false, true] {
        let draft = DeckCompatibilityRequest {
            summary_only,
            ..request(draft_main.clone(), vec![COMMANDER.to_string()])
        };
        let (compatible, reasons) = verdict(&db, &draft);
        assert!(
            !reasons.iter().any(|r| r.contains("Singleton")),
            "CR 903.13f(2) disapplies the singleton rule (summary_only={summary_only}): {reasons:?}"
        );
        assert!(
            compatible,
            "a four-of Commander Draft deck is legal outright (summary_only={summary_only}): {reasons:?}"
        );

        let constructed = DeckCompatibilityRequest {
            selected_format: Some(GameFormat::Commander),
            summary_only,
            ..request(constructed_main.clone(), vec![COMMANDER.to_string()])
        };
        let (_, reasons) = verdict(&db, &constructed);
        assert!(
            reasons.iter().any(|r| r.contains("Singleton")),
            "reach guard: constructed Commander DOES enforce CR 903.5b (summary_only={summary_only}), got {reasons:?}"
        );
    }
}

/// U9 row 5 — CR 903.13e: "the cards a player drafted become that player's card
/// pool", so there is no constructed legality table to consult. A card with no
/// Commander legality entry is accepted under Commander Draft and rejected
/// under Commander — one card, two formats, opposite verdicts.
#[test]
fn commander_draft_consults_no_constructed_legality_table() {
    let db = test_db();
    let mut main = deck(60, 0);
    main.pop();
    main.push("Draft Only Card".to_string());

    let (compatible, reasons) = verdict(&db, &request(main.clone(), vec![COMMANDER.to_string()]));
    assert!(
        compatible,
        "CR 903.13e: the drafted cards ARE the pool: {reasons:?}"
    );

    let constructed = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Commander),
        ..request(main, vec![COMMANDER.to_string()])
    };
    let (compatible, _) = verdict(&db, &constructed);
    assert!(
        !compatible,
        "reach guard: constructed Commander DOES consult its legality table"
    );
}

/// U9 row 6, runtime half — the eligibility predicate the Commander Draft arm
/// delegates to (CR 903.3, unchanged by CR 903.13f).
///
/// The wasm export `is_card_commander_eligible_for_format` cannot be called
/// from any `cargo test` — `JsValue` is not constructible off-wasm — so the
/// COMPILE half of this row is the `E0004` that removing its `_ => false`
/// produces. That is a stronger assertion than a test: Commander Draft cannot
/// silently fall through, now or when the next format lands.
#[test]
fn commander_draft_eligibility_is_cr_903_3() {
    let legendary = creature(COMMANDER, true, vec![ManaColor::White]);
    let ordinary = creature("In Color Card", false, vec![ManaColor::White]);
    assert!(is_commander_eligible(&legendary));
    assert!(!is_commander_eligible(&ordinary));
}

/// U9 row 7 — the hostile fixture: pairing runs on the SUBMITTED pair, not on
/// pool contents. Two legends that cannot legally pair under CR 702.124 are
/// rejected; a legally pairing pair is accepted.
///
/// This lives on the engine side because that is where the `CardDatabase` is:
/// `session::apply` / `apply_submit_deck` carry none, and `DraftCardInstance`
/// stores no keywords, so partner data is unreachable at the draft-core seam
/// even heuristically.
#[test]
fn commander_draft_pairing_runs_on_the_submitted_pair() {
    let db = test_db();

    let mut main = deck(60, 0);
    main.pop();
    main.push("Unpairable Legend".to_string());
    let (compatible, reasons) = verdict(
        &db,
        &request(
            main,
            vec![COMMANDER.to_string(), "Unpairable Legend".to_string()],
        ),
    );
    assert!(!compatible, "neither legend has a partner ability");
    assert!(
        reasons.iter().any(|r| r.contains("partner")),
        "expected a pairing reason, got {reasons:?}"
    );

    let mut main = deck(60, 0);
    main.pop();
    main.pop();
    main.push("Partner Legend A".to_string());
    main.push("Partner Legend B".to_string());
    let (compatible, reasons) = verdict(
        &db,
        &request(
            main,
            vec![
                "Partner Legend A".to_string(),
                "Partner Legend B".to_string(),
            ],
        ),
    );
    assert!(
        compatible,
        "CR 702.124h: two printed generic Partner legends pair: {reasons:?}"
    );
}

/// Constructed Commander is BYTE-IDENTICAL across this phase's parameter
/// changes. `evaluate_commander_with_format` stopped taking `legality_format`,
/// `format_label` and a literal `100`, and now derives all three from
/// `GameFormat` — this asserts the derivation reproduces Commander's own
/// values, which is what makes the refactor safe for the six shipped
/// commander-family formats.
#[test]
fn constructed_commander_keeps_its_own_axes() {
    let db = test_db();

    // CR 903.5a: exactly 100, so a 60-card deck is rejected — the same deck
    // Commander Draft accepts above.
    let sixty = DeckCompatibilityRequest {
        selected_format: Some(GameFormat::Commander),
        ..request(deck(60, 0), vec![COMMANDER.to_string()])
    };
    let (compatible, reasons) = verdict(&db, &sixty);
    assert!(!compatible);
    assert!(
        reasons.iter().any(|r| r.contains("100")),
        "expected Commander's exactly-100 rule, got {reasons:?}"
    );
}

// ── PF2 / U22 — CR 903.13f(3): the draft's set code reaches the LAUNCH
// validator ────────────────────────────────────────────────────────────────
//
// Everything above drives `evaluate_deck_compatibility` through `verdict()`.
// The rows below call `validate_name_deck_for_format_full` DIRECTLY, because
// that — not `evaluate_deck_compatibility` — is the function `engine-wasm` and
// `phase-server` call to decide whether the pod's game may begin, and before
// U22 it hardcoded `draft_set_codes: none` so the grant could never be in force
// at game start. `request()` is not on this path at all: it builds a
// `DeckCompatibilityRequest`, and this function takes twelve positional
// parameters.

/// The launch validator, with the draft's contained set codes the only value
/// that varies. Takes a slice because CR 903.13's conditions are about set
/// CONTAINMENT: an empty slice is constructed play, one code is a single-set
/// draft, and several are a mixed one.
///
/// Parameter order mirrors the signature: db, main_deck, sideboard, commander,
/// companion, planar_deck, scheme_deck, signature_spell, draft_set_codes,
/// format_config, selected_match_type, player_count.
fn launch_verdict(
    db: &CardDatabase,
    main_deck: &[String],
    commanders: &[String],
    draft_set_codes: &[&str],
) -> Result<(), Vec<String>> {
    let draft_set_codes: Vec<String> = draft_set_codes.iter().map(|c| (*c).to_string()).collect();
    validate_name_deck_for_format_full(
        db,
        main_deck,
        &[],
        commanders,
        &[],
        &[],
        &[],
        &[],
        &draft_set_codes,
        &FormatConfig::commander_draft(),
        None,
        4,
    )
}

/// A commanders-INSIDE 60-card deck whose designated pair is `a` and `b`.
fn paired_deck(a: &str, b: &str) -> Vec<String> {
    let mut main = deck(60, 0);
    main.pop();
    main.pop();
    main.push(a.to_string());
    main.push(b.to_string());
    main
}

/// PF2 row 6 — the CR 903.13f(3) grant, in both directions, at the launch
/// validator.
///
/// CR 903.13f(3): "If the draft contained draft boosters from Commander
/// Masters, any card which can be a player's commander by itself and whose
/// color identity includes one or fewer colors is considered to have the
/// partner ability for the purposes of deckbuilding."
///
/// `Legal Commander` and `Unpairable Legend` satisfy all three predicates
/// `partner_types_for` reads: both are legendary creatures (so
/// `CommanderQualification::ByItself`), both are mono-white (so
/// `card_color_identity(face).len() <= grant.max_colors`, which is 1 under
/// CMM), and neither carries a PRINTED `PartnerType::Generic` — only
/// `Partner Legend A`/`B` do. Neither name is a real card: this suite's card
/// database is fully synthetic.
///
/// REVERT-PROBE, and it must keep the test COMPILING: restore
/// `draft_set_codes: Vec::new()` in the BODY of `validate_name_deck_for_format_full`
/// (`deck_validation.rs`) while KEEPING the new parameter. The grant is then
/// empty in both directions and the accepted half below reds. (A revert that
/// also removes the parameter is an `E0061` — a compile failure, which is
/// correct but exercises no assertion.)
#[test]
fn commander_masters_draft_set_code_grants_the_partner_ability_at_launch() {
    let db = test_db();
    let main = paired_deck(COMMANDER, "Unpairable Legend");
    let pair = vec![COMMANDER.to_string(), "Unpairable Legend".to_string()];

    // REACH GUARD, and the whole point of asserting on the REASON rather than
    // on `is_err()`: without the set code this pair is refused BY THE PAIRING
    // CHECK specifically. A bare `is_err()` cannot tell "refused for the
    // partner rule" from "refused for deck size / colour identity / an
    // unresolvable name", and a fixture refused by an earlier gate would be
    // refused-then-accepted for reasons unrelated to the grant.
    let reasons = launch_verdict(&db, &main, &pair, &[])
        .expect_err("no grant: two ordinary legends do not pair (CR 702.124)");
    assert!(
        reasons.iter().any(|r| r.contains("partner")),
        "expected the pairing reason, got {reasons:?}"
    );

    // REVERT-FAILING: refused (with that same pairing reason) at base.
    // Asserting `Ok(())` — i.e. ZERO reasons — rather than "no partner reason",
    // so the accepted half cannot pass while some other gate quietly fails.
    assert_eq!(
        launch_verdict(&db, &main, &pair, &["CMM"]),
        Ok(()),
        "CR 903.13f(3): a CMM draft grants both mono-colour legends Partner"
    );
}

/// PF2 row 6, third hostile fixture — the set code is MATCHED, not merely
/// present. `draft_set_concessions` returns `DraftSetConcessions::default()`
/// for any code outside `DRAFT_SET_CONCESSIONS`' three rows, so this separates
/// "a set code is present" from "THIS set code concedes".
///
/// First production branch it reaches: the
/// `.find(|(code, _, _)| code.eq_ignore_ascii_case(set_code))` lookup.
#[test]
fn a_non_conceding_draft_set_code_grants_nothing() {
    let db = test_db();
    let main = paired_deck(COMMANDER, "Unpairable Legend");
    let pair = vec![COMMANDER.to_string(), "Unpairable Legend".to_string()];

    let reasons = launch_verdict(&db, &main, &pair, &["NOT_A_SET"])
        .expect_err("an unknown set code concedes nothing");
    assert!(
        reasons.iter().any(|r| r.contains("partner")),
        "expected the pairing reason, got {reasons:?}"
    );

    // Fourth: the lookup is `eq_ignore_ascii_case`, and the client forwards
    // `draft_set_codes` verbatim off the view, so casing must not decide a
    // rules question.
    assert_eq!(launch_verdict(&db, &main, &pair, &["cmm"]), Ok(()));
}

/// PF2 row 6, mixed-set half — CR 903.13f(3) conditions the grant on whether
/// the draft CONTAINED Commander Masters boosters, not on whether it contained
/// ONLY them. A draft that opened CMM and CLB boosters contained CMM, so the
/// grant is in force at launch.
///
/// This is the launch-validator end of the concession union: the draft
/// publishes every set it contained and this function takes the union over
/// them. Collapsing the list to a single representative — or refusing to answer
/// because the sets disagree — reds the second assertion, which is exactly the
/// silent drop this row exists to catch.
///
/// Paired with a non-granting neighbour so "any second set at all" cannot pass:
/// NEO concedes nothing, and CMM+NEO must still grant.
#[test]
fn a_mixed_draft_that_contained_commander_masters_still_grants_the_partner_ability() {
    let db = test_db();
    let main = paired_deck(COMMANDER, "Unpairable Legend");
    let pair = vec![COMMANDER.to_string(), "Unpairable Legend".to_string()];

    // Reach guard: neither companion set grants on its own, so an accepted
    // pairing below can only have come from CMM's presence in the union.
    for lone in [&["CLB"][..], &["NEO"][..]] {
        let reasons = launch_verdict(&db, &main, &pair, lone)
            .expect_err("CR 903.13f(3) names Commander Masters and nothing else");
        assert!(
            reasons.iter().any(|r| r.contains("partner")),
            "expected the pairing reason, got {reasons:?}"
        );
    }

    for mixed in [
        &["CMM", "CLB"][..],
        &["CLB", "CMM"][..],
        &["CMM", "NEO"][..],
    ] {
        assert_eq!(
            launch_verdict(&db, &main, &pair, mixed),
            Ok(()),
            "CR 903.13f(3): the draft contained CMM boosters, so the grant stands for {mixed:?}"
        );
    }
}

/// PF2 row 7 — hostile: the grant WIDENS the pairing rule, it does not disable
/// it.
///
/// NOT a discriminating test, and not claimed as one: the asserted value is
/// "refused" on both the fixed and the unfixed tree. It guards against an
/// over-broad implementation that reads "a set code is present" as "skip the
/// pairing check", which would turn CR 903.13f(3) from a widening into a
/// disabling.
///
/// `Two Color Legend`'s colour identity is two colours, so
/// `within_color_bound` fails (CR 903.13f(3)'s "one or fewer colors") and no
/// synthetic `Generic` is pushed for it — that is the first production branch
/// this fixture reaches.
#[test]
fn the_grant_does_not_legalise_a_two_colour_legend() {
    let db = test_db();
    let main = paired_deck(COMMANDER, "Two Color Legend");
    let pair = vec![COMMANDER.to_string(), "Two Color Legend".to_string()];

    let reasons = launch_verdict(&db, &main, &pair, &["CMM"])
        .expect_err("CR 903.13f(3) bounds the grant at one or fewer colours");
    // Reach guard: it is refused BY THE PAIRING CHECK, not by deck size or by
    // an unresolvable name.
    assert!(
        reasons.iter().any(|r| r.contains("partner")),
        "expected the pairing reason, got {reasons:?}"
    );
}

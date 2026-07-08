use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::database::legality::{LegalityFormat, LegalityStatus};
use crate::database::CardDatabase;
use crate::parser::oracle::{compute_deck_copy_limit_from_text, oracle_text_allows_commander};
use crate::types::card::{CardFace, CardRules, PrintedCardRef};
use crate::types::card_type::{CoreType, Supertype};
use crate::types::format::{DeckCopyLimit, GameFormat, SideboardPolicy};
use crate::types::keywords::Keyword;
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::match_config::MatchType;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DeckCompatibilityRequest {
    #[serde(default)]
    pub main_deck: Vec<String>,
    #[serde(default)]
    pub sideboard: Vec<String>,
    #[serde(default)]
    pub commander: Vec<String>,
    #[serde(default)]
    pub planar_deck: Vec<String>,
    #[serde(default)]
    pub scheme_deck: Vec<String>,
    /// Oathbreaker RC: the signature spell card name. Empty for all non-Oathbreaker
    /// formats. Included in `all_deck_cards` so copy-count and identity checks are
    /// accurate regardless of which validation path is active.
    #[serde(default)]
    pub signature_spell: Vec<String>,
    #[serde(default)]
    pub selected_format: Option<GameFormat>,
    #[serde(default)]
    pub selected_match_type: Option<MatchType>,
    #[serde(default = "default_player_count")]
    pub player_count: usize,
    #[serde(default)]
    pub summary_only: bool,
}

fn default_player_count() -> usize {
    2
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompatibilityCheck {
    pub compatible: bool,
    #[serde(default)]
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckCompatibilityResult {
    pub standard: CompatibilityCheck,
    pub commander: CompatibilityCheck,
    pub bo3_ready: bool,
    #[serde(default)]
    pub unknown_cards: Vec<String>,
    #[serde(default)]
    pub selected_format_compatible: Option<bool>,
    #[serde(default)]
    pub selected_format_reasons: Vec<String>,
    /// Combined color identity of all cards in the deck, in WUBRG order.
    /// Each entry is a single-letter color code: "W", "U", "B", "R", or "G".
    #[serde(default)]
    pub color_identity: Vec<String>,
    /// Engine coverage summary for the deck's unique cards.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage: Option<DeckCoverage>,
    /// Per-format legality: maps format key (e.g. "standard", "modern") to the
    /// deck's aggregate status ("legal", "not_legal", or "banned").
    /// A deck is "legal" only if every card is legal in that format.
    #[serde(default)]
    pub format_legality: BTreeMap<String, String>,
}

/// Per-card engine coverage gap info with detailed parse breakdown.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UnsupportedCard {
    pub name: String,
    pub gaps: Vec<String>,
    /// Number of copies of this card in the deck (main + sideboard + commander).
    #[serde(default = "default_one")]
    pub copies: usize,
    /// Original Oracle text for the card face.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub oracle_text: Option<String>,
    /// Hierarchical parse tree — same structure used by the coverage dashboard.
    #[serde(skip_serializing_if = "Vec::is_empty", default)]
    pub parse_details: Vec<crate::game::coverage::ParsedItem>,
}

fn default_one() -> usize {
    1
}

/// Engine coverage summary for a deck: how many unique cards are fully supported.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckCoverage {
    pub total_unique: usize,
    pub supported_unique: usize,
    pub unsupported_cards: Vec<UnsupportedCard>,
}

pub fn evaluate_deck_compatibility(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> DeckCompatibilityResult {
    if request.summary_only && request.selected_format.is_some() {
        return evaluate_deck_compatibility_summary(db, request);
    }

    let unknown_cards = collect_unknown_cards(db, request);
    let standard = evaluate_standard(db, request, &unknown_cards);
    let commander = evaluate_commander(db, request, &unknown_cards);
    // CR 100.4a / CR 903.5e: A "BO3-ready" deck is one with a real sideboard
    // the format actually uses. Decks that declare a commander are
    // Commander-style (CR 903) — their submitted sideboard slot is Phase's
    // builder-only Maybeboard staging area and the engine drops it at load
    // time, so they are never BO3-ready regardless of slot occupancy.
    let bo3_ready = !request.sideboard.is_empty() && request.commander.is_empty();
    let color_identity = collect_color_identity(db, request);

    let (selected_format_compatible, selected_format_reasons) = evaluate_selected_format(
        db,
        request,
        &unknown_cards,
        &standard,
        &commander,
        bo3_ready,
    );

    let coverage = evaluate_deck_coverage(db, request);
    let format_legality = evaluate_format_legality(db, request);

    DeckCompatibilityResult {
        standard,
        commander,
        bo3_ready,
        unknown_cards: unknown_cards.into_iter().collect(),
        selected_format_compatible,
        selected_format_reasons,
        color_identity,
        coverage: Some(coverage),
        format_legality,
    }
}

fn evaluate_deck_compatibility_summary(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> DeckCompatibilityResult {
    // CR 100.4a / CR 903.5e: A "BO3-ready" deck is one with a real sideboard
    // the format actually uses. Decks that declare a commander are
    // Commander-style (CR 903) — their submitted sideboard slot is Phase's
    // builder-only Maybeboard staging area and the engine drops it at load
    // time, so they are never BO3-ready regardless of slot occupancy.
    let bo3_ready = !request.sideboard.is_empty() && request.commander.is_empty();
    let (mut selected_format_compatible, mut selected_format_reasons, unknown_cards) =
        evaluate_selected_format_summary(db, request);

    if matches!(request.selected_match_type, Some(MatchType::Bo3)) && !bo3_ready {
        selected_format_compatible = Some(false);
        selected_format_reasons.push("BO3 requires a sideboard".to_string());
    }

    DeckCompatibilityResult {
        standard: CompatibilityCheck {
            compatible: matches!(
                (request.selected_format, selected_format_compatible),
                (Some(GameFormat::Standard), Some(true))
            ),
            reasons: Vec::new(),
        },
        commander: CompatibilityCheck {
            compatible: matches!(
                (request.selected_format, selected_format_compatible),
                (Some(GameFormat::Commander), Some(true))
            ),
            reasons: Vec::new(),
        },
        bo3_ready,
        unknown_cards: unknown_cards.into_iter().collect(),
        selected_format_compatible,
        selected_format_reasons,
        color_identity: collect_color_identity(db, request),
        coverage: None,
        format_legality: BTreeMap::new(),
    }
}

/// Validate a deck against its selected format, returning `Ok(())` if legal or
/// `Err` with human-readable reasons if not. Delegates to the same validation
/// chain used by `evaluate_deck_compatibility`.
///
/// Returns `Ok(())` when no format is selected, or for formats without card-pool
/// restrictions (FreeForAll, TwoHeadedGiant).
pub fn validate_deck_for_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> Result<(), Vec<String>> {
    if request.selected_format.is_none() {
        return Ok(());
    }
    let unknown_cards = collect_unknown_cards(db, request);
    let standard = evaluate_standard(db, request, &unknown_cards);
    let commander = evaluate_commander(db, request, &unknown_cards);
    // CR 100.4a / CR 903.5e: A "BO3-ready" deck is one with a real sideboard
    // the format actually uses. Decks that declare a commander are
    // Commander-style (CR 903) — their submitted sideboard slot is Phase's
    // builder-only Maybeboard staging area and the engine drops it at load
    // time, so they are never BO3-ready regardless of slot occupancy.
    let bo3_ready = !request.sideboard.is_empty() && request.commander.is_empty();
    let (compatible, reasons) = evaluate_selected_format(
        db,
        request,
        &unknown_cards,
        &standard,
        &commander,
        bo3_ready,
    );
    match compatible {
        Some(false) => Err(reasons),
        _ => Ok(()),
    }
}

pub fn validate_name_deck_for_format(
    db: &CardDatabase,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
    selected_format: GameFormat,
    selected_match_type: Option<MatchType>,
) -> Result<(), Vec<String>> {
    validate_name_deck_for_format_with_sig(
        db,
        main_deck,
        sideboard,
        commander,
        &[],
        selected_format,
        selected_match_type,
    )
}

/// Extended variant of `validate_name_deck_for_format` that accepts a
/// signature spell slot for Oathbreaker validation. All other callers
/// continue to use `validate_name_deck_for_format` with an implicit empty slice.
pub fn validate_name_deck_for_format_with_sig(
    db: &CardDatabase,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
    signature_spell: &[String],
    selected_format: GameFormat,
    selected_match_type: Option<MatchType>,
) -> Result<(), Vec<String>> {
    validate_name_deck_for_format_full(
        db,
        main_deck,
        sideboard,
        commander,
        &[],
        &[],
        signature_spell,
        selected_format,
        selected_match_type,
        default_player_count(),
    )
}

#[allow(clippy::too_many_arguments)]
pub fn validate_name_deck_for_format_full(
    db: &CardDatabase,
    main_deck: &[String],
    sideboard: &[String],
    commander: &[String],
    planar_deck: &[String],
    scheme_deck: &[String],
    signature_spell: &[String],
    selected_format: GameFormat,
    selected_match_type: Option<MatchType>,
    player_count: usize,
) -> Result<(), Vec<String>> {
    let request = DeckCompatibilityRequest {
        main_deck: main_deck.to_vec(),
        sideboard: sideboard.to_vec(),
        commander: commander.to_vec(),
        planar_deck: planar_deck.to_vec(),
        scheme_deck: scheme_deck.to_vec(),
        signature_spell: signature_spell.to_vec(),
        selected_format: Some(selected_format),
        selected_match_type,
        player_count,
        summary_only: false,
    };
    validate_deck_for_format(db, &request)
}

fn evaluate_standard(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    evaluate_constructed(
        db,
        request,
        unknown_cards,
        LegalityFormat::Standard,
        "Standard",
        GameFormat::Standard.sideboard_policy(),
    )
}

/// Shared validation for constructed formats (Standard, Pioneer, Pauper, etc.):
/// checks unknown cards, no commander slot, minimum 60 cards, sideboard size,
/// combined main+sideboard 4-per-name limit, and legality against the given
/// `LegalityFormat`.
///
/// CR 100.2a + CR 100.4a: The 4-card-per-name limit applies to the combined
/// deck and sideboard, with basic lands and "A deck can have any number"
/// cards exempt.
fn evaluate_constructed(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    legality_format: LegalityFormat,
    format_label: &str,
    sideboard_policy: SideboardPolicy,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if !request.commander.is_empty() {
        reasons.push(format!("{format_label} decks do not use a commander slot"));
    }

    if request.main_deck.len() < 60 {
        reasons.push(format!(
            "Main deck has {} cards (minimum 60)",
            request.main_deck.len()
        ));
    }

    // CR 100.4a: In constructed play, the sideboard may contain at most 15 cards.
    if let SideboardPolicy::Limited(max) = sideboard_policy {
        if request.sideboard.len() as u32 > max {
            reasons.push(format!(
                "Sideboard has {} cards (maximum {})",
                request.sideboard.len(),
                max
            ));
        }
    }

    // CR 100.2a + CR 100.4a: The 4-card limit applies to main + sideboard combined.
    let counts = combined_copy_counts(db, request);
    let over_limit = copy_limit_violations(db, &counts, 4);
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(
            "More than 4 copies (main + sideboard combined)",
            &over_limit,
            6,
        ));
    }

    let mut illegal_cards = BTreeSet::new();
    let mut restricted_canonical: HashSet<String> = HashSet::new();
    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        let resolved = resolve_card_name(db, name);
        match db.legality_status(resolved, legality_format) {
            Some(LegalityStatus::Legal) => {}
            // CR 100.2b: A card on a format's restricted list is legal but
            // a deck may contain at most one copy of it. Vintage is the
            // canonical user, but the rule is format-general — any format
            // whose legality table marks a card `Restricted` follows the
            // same 1-copy ceiling, enforced below. The card itself is not
            // "illegal" — that was the bug that flagged Power 9 as banned
            // in Vintage.
            Some(LegalityStatus::Restricted) => {
                restricted_canonical.insert(canonical_deck_count_key(db, name));
            }
            Some(status) => {
                illegal_cards.insert(format!("{name} ({})", status_label(status)));
            }
            None => {
                illegal_cards.insert(format!("{name} (not legal in {format_label})"));
            }
        }
    }

    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards(
            &format!("Not {format_label} legal"),
            &illegal_cards,
            6,
        ));
    }

    // CR 100.2b: Restricted cards may appear at most once in a deck.
    let restricted_violations = restricted_copy_violations(db, &counts, &restricted_canonical);
    if !restricted_violations.is_empty() {
        reasons.push(summarize_cards(
            "More than 1 copy of a restricted card",
            &restricted_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn evaluate_planechase(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }
    if !(2..=4).contains(&request.player_count) {
        reasons.push(format!(
            "Planechase requires 2 to 4 players (found {})",
            request.player_count
        ));
    }
    if !request.commander.is_empty() {
        reasons.push("Planechase decks do not use a commander slot".to_string());
    }
    if request.main_deck.len() < 60 {
        reasons.push(format!(
            "Main deck has {} cards (minimum 60)",
            request.main_deck.len()
        ));
    }

    let counts = combined_copy_counts(db, request);
    let over_limit = copy_limit_violations(db, &counts, 4);
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(
            "More than 4 copies (main + sideboard combined)",
            &over_limit,
            6,
        ));
    }

    if request.planar_deck.is_empty() {
        return CompatibilityCheck {
            compatible: reasons.is_empty(),
            reasons,
        };
    }

    // CR 901.15a: a shared planar deck must contain at least 40 cards, or at
    // least ten cards per player if there are fewer than four players.
    let min_planar_cards = 40usize.min(request.player_count.saturating_mul(10));
    if request.planar_deck.len() < min_planar_cards {
        reasons.push(format!(
            "Planar deck has {} cards (minimum {min_planar_cards})",
            request.planar_deck.len()
        ));
    }

    let mut seen_names = HashSet::new();
    let mut duplicates = BTreeSet::new();
    let mut non_planar = BTreeSet::new();
    let mut planes = 0usize;
    let mut phenomena = 0usize;
    for name in &request.planar_deck {
        let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) else {
            continue;
        };
        let canonical = face.name.to_lowercase();
        if !seen_names.insert(canonical) {
            duplicates.insert(face.name.clone());
        }
        let is_plane = face.card_type.core_types.contains(&CoreType::Plane);
        let is_phenomenon = face.card_type.core_types.contains(&CoreType::Phenomenon);
        if is_plane {
            planes += 1;
        }
        if is_phenomenon {
            phenomena += 1;
        }
        if !is_plane && !is_phenomenon {
            non_planar.insert(face.name.clone());
        }
    }
    if !duplicates.is_empty() {
        reasons.push(summarize_cards(
            "Planar deck singleton violations",
            &duplicates,
            6,
        ));
    }
    if !non_planar.is_empty() {
        reasons.push(summarize_cards(
            "Planar deck cards must be Plane or Phenomenon",
            &non_planar,
            6,
        ));
    }
    if planes == 0 {
        reasons.push("Planar deck must contain at least one Plane".to_string());
    }
    let max_phenomena = request.player_count.saturating_mul(2);
    if phenomena > max_phenomena {
        reasons.push(format!(
            "Planar deck has {phenomena} phenomena (maximum {max_phenomena})"
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn evaluate_archenemy(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }
    if !(2..=6).contains(&request.player_count) {
        reasons.push(format!(
            "Archenemy requires 2 to 6 players (found {})",
            request.player_count
        ));
    }
    if !request.commander.is_empty() {
        reasons.push("Archenemy decks do not use a commander slot".to_string());
    }
    if request.main_deck.len() < 60 {
        reasons.push(format!(
            "Main deck has {} cards (minimum 60)",
            request.main_deck.len()
        ));
    }

    let counts = combined_copy_counts(db, request);
    let over_limit = copy_limit_violations(db, &counts, 4);
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(
            "More than 4 copies (main + sideboard combined)",
            &over_limit,
            6,
        ));
    }

    if !request.scheme_deck.is_empty() {
        validate_scheme_deck(db, &request.scheme_deck, &mut reasons);
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn validate_scheme_deck(db: &CardDatabase, scheme_deck: &[String], reasons: &mut Vec<String>) {
    // CR 904.3: A scheme deck must contain at least twenty scheme cards and
    // can't contain more than two copies of any card by English name.
    if scheme_deck.len() < 20 {
        reasons.push(format!(
            "Scheme deck has {} cards (minimum 20)",
            scheme_deck.len()
        ));
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut non_scheme = BTreeSet::new();
    let mut unsupported = BTreeSet::new();
    for name in scheme_deck {
        let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) else {
            continue;
        };
        *counts.entry(face.name.to_lowercase()).or_insert(0) += 1;
        if !face.card_type.core_types.contains(&CoreType::Scheme) {
            non_scheme.insert(face.name.clone());
        }
        if !crate::game::coverage::card_face_gaps(face).is_empty() {
            unsupported.insert(face.name.clone());
        }
    }

    let over_limit: BTreeSet<String> = counts
        .into_iter()
        .filter(|(_, count)| *count > 2)
        .filter_map(|(name, count)| {
            db.get_face_by_name(&name)
                .map(|face| format!("{} ({count} copies)", face.name))
        })
        .collect();
    if !over_limit.is_empty() {
        reasons.push(summarize_cards(
            "Scheme deck copy-limit violations",
            &over_limit,
            6,
        ));
    }
    if !non_scheme.is_empty() {
        reasons.push(summarize_cards(
            "Scheme deck cards must be Scheme cards",
            &non_scheme,
            6,
        ));
    }
    if !unsupported.is_empty() {
        reasons.push(summarize_cards("Unsupported scheme cards", &unsupported, 6));
    }
}

fn evaluate_commander(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    evaluate_commander_with_format(
        db,
        request,
        unknown_cards,
        LegalityFormat::Commander,
        "Commander",
        CommanderVariantRules::commander(),
    )
}

struct CommanderVariantRules {
    eligible: fn(&CardFace) -> bool,
    eligibility_error: &'static str,
    skip_commander_legality: bool,
}

impl CommanderVariantRules {
    fn commander() -> Self {
        Self {
            eligible: is_commander_eligible,
            eligibility_error:
                "Commander cards must be legendary creatures or explicitly allow being a commander",
            skip_commander_legality: false,
        }
    }

    fn duel_commander() -> Self {
        Self {
            eligible: is_commander_eligible,
            eligibility_error:
                "Duel Commander cards must be legendary creatures or explicitly allow being a commander",
            skip_commander_legality: false,
        }
    }

    fn pauper_commander() -> Self {
        Self {
            eligible: is_pauper_commander_eligible,
            eligibility_error:
                "Pauper Commander commander must be an uncommon creature, Vehicle, or Spacecraft",
            skip_commander_legality: true,
        }
    }
}

/// Strip the sideboard slot from a request before running the validators
/// that share `all_deck_cards`/`combined_copy_counts`. Commander, Brawl, and
/// their variants drop the sideboard at game load (CR 903.5e), so its
/// contents must not contribute to singleton, color-identity, or legality
/// rules. The original request is preserved for callers that still need it
/// (e.g. unknown-card collection, which is computed once upstream).
fn request_without_sideboard(request: &DeckCompatibilityRequest) -> DeckCompatibilityRequest {
    DeckCompatibilityRequest {
        sideboard: Vec::new(),
        ..request.clone()
    }
}

/// Shared commander-variant validator. Commander, Duel Commander, and Pauper
/// Commander all use 100-card-singleton deck shape with a command zone; only
/// the legality table, commander eligibility, and display label differ.
/// DuelCommander's 30-life / 1v1-only rules are expressed in `FormatConfig`,
/// not deck validation.
fn evaluate_commander_with_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    legality_format: LegalityFormat,
    format_label: &str,
    rules: CommanderVariantRules,
) -> CompatibilityCheck {
    // CR 903.5e: the sideboard is dropped at game load for Commander-style
    // formats. Re-scope the request so singleton, color-identity, and legality
    // checks operate on the actual game deck.
    let stripped = request_without_sideboard(request);
    let request = &stripped;
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.commander.is_empty() || request.commander.len() > 2 {
        reasons.push(format!(
            "{format_label} decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }

    if !request.commander.is_empty() && request.commander.len() <= 2 {
        let mut ineligible_commanders = BTreeSet::new();

        for name in &request.commander {
            let Some(face) = db.get_face_by_name(name) else {
                continue;
            };

            if !(rules.eligible)(face) {
                ineligible_commanders.insert(name.clone());
            }
        }

        if !ineligible_commanders.is_empty() {
            reasons.push(summarize_cards(
                rules.eligibility_error,
                &ineligible_commanders,
                6,
            ));
        }

        // CR 702.124: Validate partner pairing for two-commander setups
        if request.commander.len() == 2 {
            let face_a = db.get_face_by_name(&request.commander[0]);
            let face_b = db.get_face_by_name(&request.commander[1]);
            if let (Some(a), Some(b)) = (face_a, face_b) {
                if !are_valid_partners(a, b) {
                    reasons.push(format!(
                        "Invalid partner pairing: {} and {} do not have compatible partner keywords",
                        request.commander[0], request.commander[1]
                    ));
                }
            }
        }
    }

    // CR 903.5e (+ variant rules): Commander-style formats do not start the
    // game with a sideboard. We accept extra entries in the submitted list
    // (Phase's deck builder uses that slot as a builder-only "Maybeboard"
    // staging area) and enforce CR 903.5e at game load by dropping them —
    // see `load_deck_into_state` in `deck_loading.rs`.

    let represented_in_main = request
        .commander
        .iter()
        .filter(|name| {
            request
                .main_deck
                .iter()
                .any(|card| card.eq_ignore_ascii_case(name))
        })
        .count();
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if total_cards != 100 {
        reasons.push(format!(
            "{format_label} deck must have exactly 100 cards (found {total_cards})"
        ));
    }

    // CR 903.5b: Other than basic lands, each card in a Commander deck must have
    // a different English name. Canonicalization (CR 201.3) is handled inside
    // the shared helper.
    let counts = combined_copy_counts(db, request);
    let singleton_violations = copy_limit_violations(db, &counts, 1);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    let mut illegal_cards = BTreeSet::new();
    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        if rules.skip_commander_legality
            && request
                .commander
                .iter()
                .any(|commander| commander.eq_ignore_ascii_case(name))
        {
            continue;
        }
        match db.legality_status(resolve_card_name(db, name), legality_format) {
            Some(status) if status.is_legal() => {}
            Some(status) => {
                illegal_cards.insert(format!("{name} ({})", status_label(status)));
            }
            None => {
                illegal_cards.insert(format!("{name} (not legal in {format_label})"));
            }
        }
    }
    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards(
            &format!("Not {format_label} legal"),
            &illegal_cards,
            6,
        ));
    }

    // CR 903.4: Each non-commander card's color identity must be a subset of
    // the commander(s)' combined color identity.
    let mut commander_identity = HashSet::new();
    for name in &request.commander {
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) {
            commander_identity.extend(card_color_identity(face));
        }
    }
    let identity_violations = color_identity_violations(
        db,
        &request.main_deck,
        &commander_identity,
        unknown_cards,
        |name| {
            request
                .commander
                .iter()
                .any(|c| c.eq_ignore_ascii_case(name))
        },
    );
    if !identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside commander's color identity",
            &identity_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// Brawl variant of CR 903.3: a legendary planeswalker is also eligible as a Brawl commander.
/// Uses the pre-computed `brawl_commander` field (union of MTGJSON leadershipSkills
/// and type-line analysis). Falls back to type-line check for cards loaded from
/// test fixtures that may not have the field set.
pub fn is_brawl_commander_eligible(face: &CardFace) -> bool {
    if face.brawl_commander {
        return true;
    }
    // Fallback: type-line check for cards without pre-computed field (e.g. test DB)
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));

    (is_legendary && (is_creature || is_planeswalker)) || explicitly_allowed
}

/// Shared validation for Brawl and Historic Brawl: 60-card singleton with a commander,
/// legendary creature or planeswalker as commander, no partner, no sideboard.
fn evaluate_brawl(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    legality_format: LegalityFormat,
    format_label: &str,
) -> CompatibilityCheck {
    // CR 903.5e (Brawl variant): drop the sideboard slot before shape /
    // singleton / identity checks — it is not part of the loaded deck.
    let stripped = request_without_sideboard(request);
    let request = &stripped;
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    // Brawl requires exactly 1 commander (no partner)
    if request.commander.len() != 1 {
        reasons.push(format!(
            "{format_label} decks require exactly 1 commander (found {})",
            request.commander.len()
        ));
    }

    // Validate commander eligibility: legendary creature OR legendary planeswalker
    if request.commander.len() == 1 {
        let name = &request.commander[0];
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) {
            if !is_brawl_commander_eligible(face) {
                reasons.push(format!(
                    "{format_label} commander must be a legendary creature or legendary planeswalker: {name}"
                ));
            }
        }
    }

    // CR 903.5e (via Brawl variant): Brawl formats do not start the game with
    // a sideboard. Extra entries in the submitted list are silently ignored at
    // load time — see `load_deck_into_state` in `deck_loading.rs`.

    // Exactly 60 total cards (main + commander, accounting for commander listed in main)
    let represented_in_main = request
        .commander
        .iter()
        .filter(|name| {
            request
                .main_deck
                .iter()
                .any(|card| card.eq_ignore_ascii_case(name))
        })
        .count();
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if total_cards != 60 {
        reasons.push(format!(
            "{format_label} deck must have exactly 60 cards (found {total_cards})"
        ));
    }

    // CR 903.5b (Brawl variant): singleton rule, basic lands exempt, canonicalized
    // via CR 201.3 in the shared helper.
    let counts = combined_copy_counts(db, request);
    let singleton_violations = copy_limit_violations(db, &counts, 1);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    // Legality check
    let mut illegal_cards = BTreeSet::new();
    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        match db.legality_status(resolve_card_name(db, name), legality_format) {
            Some(status) if status.is_legal() => {}
            Some(status) => {
                illegal_cards.insert(format!("{name} ({})", status_label(status)));
            }
            None => {
                illegal_cards.insert(format!("{name} (not legal in {format_label})"));
            }
        }
    }
    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards(
            &format!("Not {format_label} legal"),
            &illegal_cards,
            6,
        ));
    }

    // CR 903.4: Each non-commander card's color identity must be a subset of
    // the commander's color identity.
    if request.commander.len() == 1 {
        let cmd_name = &request.commander[0];
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, cmd_name)) {
            let commander_identity = card_color_identity(face);
            let mut identity_violations = BTreeSet::new();
            for name in &request.main_deck {
                if name.eq_ignore_ascii_case(cmd_name) {
                    continue;
                }
                if unknown_cards.contains(name.as_str()) {
                    continue;
                }
                if let Some(card_face) = db.get_face_by_name(resolve_card_name(db, name)) {
                    let card_colors = card_color_identity(card_face);
                    for color in &card_colors {
                        if !commander_identity.contains(color) {
                            identity_violations.insert(name.clone());
                            break;
                        }
                    }
                }
            }
            if !identity_violations.is_empty() {
                reasons.push(summarize_cards(
                    &format!("Cards outside {format_label} commander's color identity"),
                    &identity_violations,
                    6,
                ));
            }
        }
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// Official Tiny Leaders: Reborn banlist snapshot.
/// Source: https://official-tlr.com/banlist/ — latest update 2026-04-29.
const TINY_LEADERS_DECK_BANNED: &[&str] = &[
    "Ancestral Recall",
    "Black Lotus",
    "Balance",
    "Channel",
    "Chaos Orb",
    "Chrome Mox",
    "Counterbalance",
    "Court of Cunning",
    "Deflecting Swat",
    "Demonic Tutor",
    "Earthcraft",
    "Falling Star",
    "Fastbond",
    "Fierce Guardianship",
    "Forth Eorlingas!",
    "Gaea's Cradle",
    "Grindstone",
    "Hermit Druid",
    "High Tide",
    "Imperial Seal",
    "Jeweled Lotus",
    "Karakas",
    "Library of Alexandria",
    "Lion's Eye Diamond",
    "Maddening Hex",
    "Mana Crypt",
    "Mana Vault",
    "Mind Twist",
    "Mishra's Workshop",
    "Mox Amber",
    "Mox Diamond",
    "Mox Emerald",
    "Mox Jet",
    "Mox Opal",
    "Mox Pearl",
    "Mox Ruby",
    "Mox Sapphire",
    "Mystical Tutor",
    "Necropotence",
    "Oko, Thief of Crowns",
    "Price of Progress",
    "Shahrazad",
    "Skullclamp",
    "Sol Ring",
    "Strip Mine",
    "Survival of the Fittest",
    "Tasha's Hideous Laughter",
    "Teferi, Time Raveler",
    "Thassa's Oracle",
    "The Tabernacle at Pendrell Vale",
    "Time Vault",
    "Time Walk",
    "Timetwister",
    "Tolarian Academy",
    "True-Name Nemesis",
    "Umezawa's Jitte",
    "Vampiric Tutor",
    "Wheel of Fortune",
    "White Plume Adventurer",
    "Yawgmoth's Will",
];

const TINY_LEADERS_COMMANDER_BANNED: &[&str] = &[
    "Ajani, Nacatl Pariah",
    "Ashiok, Dream Render",
    "Derevi, Imperial Tactician",
    "Erayo, Soratami Ascendant",
    "Jeska, Thrice Reborn",
    "Ketramose, the New Dawn",
    "Nadu, Winged Wisdom",
    "Rofellos, Llanowar Emissary",
    "Uro, Titan of Nature's Wrath",
    "Wrenn and Six",
];

const TINY_LEADERS_COMPANION_BANNED: &[&str] = &["Lutri, the Spellchaser"];

pub(crate) fn tiny_leaders_companion_banned(name: &str) -> bool {
    name_in_list(name, TINY_LEADERS_COMPANION_BANNED)
}

fn evaluate_tiny_leaders(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.commander.is_empty() || request.commander.len() > 2 {
        reasons.push(format!(
            "Tiny Leaders: Reborn decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }

    if request.commander.len() <= 2 {
        let mut ineligible_commanders = BTreeSet::new();
        let mut commander_bans = BTreeSet::new();
        for name in &request.commander {
            let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) else {
                continue;
            };
            if !is_tiny_leader_eligible(face) {
                ineligible_commanders.insert(name.clone());
            }
            if name_in_list(&face.name, TINY_LEADERS_COMMANDER_BANNED) {
                commander_bans.insert(face.name.clone());
            }
        }
        if !ineligible_commanders.is_empty() {
            reasons.push(summarize_cards(
                "Tiny Leader must be a legendary creature, Vehicle, Spacecraft, planeswalker, or explicitly allow being a commander",
                &ineligible_commanders,
                6,
            ));
        }
        if !commander_bans.is_empty() {
            reasons.push(summarize_cards("Banned as Tiny Leader", &commander_bans, 6));
        }

        if request.commander.len() == 2 {
            let face_a = db.get_face_by_name(resolve_card_name(db, &request.commander[0]));
            let face_b = db.get_face_by_name(resolve_card_name(db, &request.commander[1]));
            if let (Some(a), Some(b)) = (face_a, face_b) {
                if !are_valid_partners(a, b) {
                    reasons.push(format!(
                        "Invalid partner pairing: {} and {} do not have compatible partner keywords",
                        request.commander[0], request.commander[1]
                    ));
                }
            }
        }
    }

    if request.sideboard.len() > 10 {
        reasons.push(format!(
            "Sideboard has {} cards (maximum 10)",
            request.sideboard.len()
        ));
    }

    let represented_in_main = request
        .commander
        .iter()
        .filter(|name| {
            request
                .main_deck
                .iter()
                .any(|card| card.eq_ignore_ascii_case(name))
        })
        .count();
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if total_cards != 50 {
        reasons.push(format!(
            "Tiny Leaders: Reborn deck must have exactly 50 main+commander cards (found {total_cards})"
        ));
    }

    let counts = combined_copy_counts(db, request);
    let singleton_violations = copy_limit_violations(db, &counts, 1);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    let mut commander_identity = HashSet::new();
    for name in &request.commander {
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) {
            commander_identity.extend(card_color_identity(face));
        }
    }

    let mut identity_violations = BTreeSet::new();
    let mut basic_land_type_violations = BTreeSet::new();
    let mut tiny_identity_violations = BTreeSet::new();
    let mut deck_bans = BTreeSet::new();
    let mut category_bans = BTreeSet::new();

    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        let resolved = resolve_card_name(db, name);
        let Some(face) = db.get_face_by_name(resolved) else {
            continue;
        };

        if name_in_list(&face.name, TINY_LEADERS_DECK_BANNED) {
            deck_bans.insert(face.name.clone());
        }
        if tiny_leaders_category_banned(face) {
            category_bans.insert(face.name.clone());
        }

        if !request
            .commander
            .iter()
            .any(|commander| commander.eq_ignore_ascii_case(name))
        {
            for color in card_color_identity(face) {
                if !commander_identity.contains(&color) {
                    identity_violations.insert(name.to_string());
                    break;
                }
            }
        }

        for color in basic_land_type_colors(face) {
            if !commander_identity.contains(&color) {
                basic_land_type_violations.insert(face.name.clone());
                break;
            }
        }

        if !tiny_leaders_cost_identity_ok(db, resolved) {
            tiny_identity_violations.insert(face.name.clone());
        }
    }

    if !deck_bans.is_empty() {
        reasons.push(summarize_cards(
            "Banned in Tiny Leaders: Reborn deck construction",
            &deck_bans,
            6,
        ));
    }
    if !category_bans.is_empty() {
        reasons.push(summarize_cards(
            "Categorically excluded in Tiny Leaders: Reborn",
            &category_bans,
            6,
        ));
    }
    if !identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside Tiny Leader color identity",
            &identity_violations,
            6,
        ));
    }
    if !basic_land_type_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards with off-identity basic land types",
            &basic_land_type_violations,
            6,
        ));
    }
    if !tiny_identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside Tiny cost identity",
            &tiny_identity_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn quick_tiny_leaders_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_tiny_leaders(db, request, &unknown_cards);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

pub fn is_tiny_leader_eligible(face: &CardFace) -> bool {
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let subtypes = &face.card_type.subtypes;
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let is_planeswalker = face.card_type.core_types.contains(&CoreType::Planeswalker);
    let is_vehicle = subtypes.iter().any(|s| s.eq_ignore_ascii_case("Vehicle"));
    let is_spacecraft_with_pt = subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Spacecraft"))
        && face.power.is_some()
        && face.toughness.is_some();
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));

    explicitly_allowed
        || (is_legendary && (is_creature || is_vehicle || is_spacecraft_with_pt || is_planeswalker))
}

fn basic_land_type_colors(face: &CardFace) -> Vec<ManaColor> {
    let mut colors = Vec::new();
    for subtype in &face.card_type.subtypes {
        let color = match subtype.as_str() {
            "Plains" => ManaColor::White,
            "Island" => ManaColor::Blue,
            "Swamp" => ManaColor::Black,
            "Mountain" => ManaColor::Red,
            "Forest" => ManaColor::Green,
            _ => continue,
        };
        if !colors.contains(&color) {
            colors.push(color);
        }
    }
    colors
}

fn tiny_leaders_cost_identity_ok(db: &CardDatabase, name: &str) -> bool {
    tiny_leaders_cost_faces(db, name)
        .into_iter()
        .all(|face| tiny_leaders_face_cost_identity_ok(db, face))
}

fn tiny_leaders_face_cost_identity_ok(db: &CardDatabase, face: &CardFace) -> bool {
    // CR 202.3d + CR 709.4b: off the stack a split card's mana value is the COMBINED
    // value of both halves, so the Tiny Leaders MV <= 3 cap must use the combined
    // value (a Fire // Ice-style card is MV 4, not 2). `off_stack_mana_value_for_face`
    // combines for split cards and is the face's own value for every other layout,
    // preserving the per-face check for DFC/MDFC/Adventure cards.
    db.off_stack_mana_value_for_face(face) <= 3
        && face.keywords.iter().all(|keyword| match keyword {
            Keyword::Prototype { cost, .. } => cost.mana_value() <= 3,
            _ => true,
        })
}

fn tiny_leaders_cost_faces<'a>(db: &'a CardDatabase, name: &str) -> Vec<&'a CardFace> {
    if let Some(rules) = db.get_by_name(name) {
        return card_rules_faces(rules);
    }

    let Some(face) = db.get_face_by_name(name) else {
        return Vec::new();
    };
    let mut faces = vec![face];
    if let Some(oracle_id) = &face.scryfall_oracle_id {
        let printed_ref = PrintedCardRef {
            oracle_id: oracle_id.clone(),
            face_name: face.name.clone(),
        };
        if let Some(other) = db.get_other_face_by_printed_ref(&printed_ref) {
            faces.push(other);
        }
    }
    faces
}

fn card_rules_faces(rules: &CardRules) -> Vec<&CardFace> {
    crate::database::synthesis::layout_faces(&rules.layout)
}

fn tiny_leaders_category_banned(face: &CardFace) -> bool {
    let text = face
        .oracle_text
        .as_deref()
        .unwrap_or("")
        .to_ascii_lowercase();
    face.card_type.subtypes.iter().any(|subtype| {
        subtype.eq_ignore_ascii_case("Conspiracy") || subtype.eq_ignore_ascii_case("Attraction")
    }) || text.contains("playing for ante")
        || text.contains("sticker")
        || text.contains("attraction")
}

fn name_in_list(name: &str, list: &[&str]) -> bool {
    list.iter().any(|banned| names_match(name, banned))
}

fn names_match(a: &str, b: &str) -> bool {
    fn normalize(raw: &str) -> String {
        raw.chars()
            .map(|c| match c {
                '\u{2019}' => '\'',
                _ => c,
            })
            .flat_map(|c| c.to_lowercase())
            .collect()
    }
    normalize(a) == normalize(b)
}

/// Oathbreaker RC: returns `true` if `face` is an instant or sorcery.
fn is_instant_or_sorcery(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Instant)
        || face.card_type.core_types.contains(&CoreType::Sorcery)
}

/// Oathbreaker RC: full deck compatibility check.
fn evaluate_oathbreaker(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    // Oathbreaker RC: exactly one Oathbreaker (legendary Planeswalker).
    if request.commander.len() != 1 {
        reasons.push(format!(
            "Oathbreaker decks require exactly 1 Oathbreaker (found {})",
            request.commander.len()
        ));
    } else {
        let name = &request.commander[0];
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) {
            if !face.is_oathbreaker {
                reasons.push(format!(
                    "{name}: Oathbreaker must be a legendary Planeswalker"
                ));
            }
        }
    }

    let oathbreaker_identity = request.commander.first().and_then(|ob_name| {
        db.get_face_by_name(resolve_card_name(db, ob_name))
            .filter(|face| face.is_oathbreaker)
            .map(|face| {
                card_color_identity(face)
                    .into_iter()
                    .collect::<HashSet<_>>()
            })
    });

    // Oathbreaker RC: exactly one signature spell (instant or sorcery within color identity).
    if request.signature_spell.len() != 1 {
        reasons.push(format!(
            "Oathbreaker decks require exactly 1 signature spell (found {})",
            request.signature_spell.len()
        ));
    } else {
        let sig_name = &request.signature_spell[0];
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, sig_name)) {
            if !is_instant_or_sorcery(face) {
                reasons.push(format!(
                    "{sig_name}: signature spell must be an instant or sorcery"
                ));
            }
            // Signature spell must be within the Oathbreaker's color identity.
            if let Some(identity) = &oathbreaker_identity {
                for color in card_color_identity(face) {
                    if !identity.contains(&color) {
                        reasons.push(format!(
                            "{sig_name}: signature spell is outside the Oathbreaker's color identity"
                        ));
                        break;
                    }
                }
            }
        }
    }

    // Oathbreaker RC: exactly 60 cards total (main + commander + signature spell,
    // de-duplicating any that appear in both main and a command-zone slot).
    let commander_represented = request
        .commander
        .iter()
        .filter(|n| request.main_deck.iter().any(|c| names_match(c, n)))
        .count();
    let sig_represented = request
        .signature_spell
        .iter()
        .filter(|n| request.main_deck.iter().any(|c| names_match(c, n)))
        .count();
    let total_cards = request.main_deck.len()
        + (request
            .commander
            .len()
            .saturating_sub(commander_represented))
        + (request
            .signature_spell
            .len()
            .saturating_sub(sig_represented));
    if total_cards != 60 {
        reasons.push(format!(
            "Oathbreaker deck must have exactly 60 cards (found {total_cards})"
        ));
    }

    // Oathbreaker RC: singleton (basic lands exempt, consistent with other
    // singleton command-zone formats). `all_deck_cards` now includes `signature_spell`
    // so a card in both the main deck and signature-spell slot is caught here.
    let counts = combined_copy_counts(db, request);
    let singleton_violations = copy_limit_violations(db, &counts, 1);
    if !singleton_violations.is_empty() {
        reasons.push(summarize_cards(
            "Singleton violations",
            &singleton_violations,
            6,
        ));
    }

    // Oathbreaker RC: every main-deck card must be within the Oathbreaker's
    // color identity. CR 903.5c (color identity) is shared with the other
    // command-zone formats via `color_identity_violations`; CR 903.5d (off-
    // identity basic land types) is reported in its own bucket alongside it.
    let mut identity_violations = BTreeSet::new();
    let mut basic_type_violations = BTreeSet::new();
    if let Some(identity) = &oathbreaker_identity {
        identity_violations =
            color_identity_violations(db, &request.main_deck, identity, unknown_cards, |_| false);
        for name in request.main_deck.iter().map(String::as_str) {
            if unknown_cards.contains(name) {
                continue;
            }
            let resolved = resolve_card_name(db, name);
            let Some(face) = db.get_face_by_name(resolved) else {
                continue;
            };
            for color in basic_land_type_colors(face) {
                if !identity.contains(&color) {
                    basic_type_violations.insert(face.name.clone());
                    break;
                }
            }
        }
    }
    if !identity_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards outside Oathbreaker color identity",
            &identity_violations,
            6,
        ));
    }
    if !basic_type_violations.is_empty() {
        reasons.push(summarize_cards(
            "Cards with off-identity basic land types",
            &basic_type_violations,
            6,
        ));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

/// CR 305.6: the five basic land types (Plains/Island/Swamp/Mountain/Forest).
/// "Wastes" is the basic colorless land but is NOT a basic land type, so
/// Snow-Covered Wastes is naturally excluded from the Momir's Madness deck by
/// requiring a subtype in this set.
const BASIC_LAND_TYPES: [&str; 5] = ["Plains", "Island", "Swamp", "Mountain", "Forest"];

/// Momir's Madness format deck rule: the deck is fixed at exactly 12 copies of
/// each of the five snow basic lands (Snow-Covered Plains/Island/Swamp/Mountain/
/// Forest), totaling 60, with nothing else. Players cannot adjust this ratio.
///
/// A "snow basic land" is identified by typed checks (CR 205.4a Snow + Basic
/// supertypes, CR 305 Land core type, and a CR 305.6 basic land type subtype) —
/// never by matching printed card names. Snow-Covered Wastes is excluded because
/// its subtype is "Wastes", which is not a basic land type (CR 305.6). This is a
/// format-construction rule, not a Comprehensive Rule; CR 100.2a's basic-land
/// copy exception is what makes the 12-of-each copies legal despite the
/// four-copy default.
fn evaluate_momir(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    const EXPECTED_PER_TYPE: usize = 12;

    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.main_deck.len() != 60 {
        reasons.push(format!(
            "Momir's Madness decks must have exactly 60 cards (found {})",
            request.main_deck.len()
        ));
    }

    // Tally snow-basic copies per basic land type; collect anything that is not a
    // snow basic land of a CR 305.6 basic land type.
    let mut per_type: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut non_snow_basic = BTreeSet::new();
    for name in request.main_deck.iter().map(String::as_str) {
        if unknown_cards.contains(name) {
            continue;
        }
        let resolved = resolve_card_name(db, name);
        let Some(face) = db.get_face_by_name(resolved) else {
            continue;
        };
        // CR 205.4a + CR 305: Snow + Basic supertypes on a Land.
        let is_snow_basic_land = face.card_type.supertypes.contains(&Supertype::Snow)
            && face.card_type.supertypes.contains(&Supertype::Basic)
            && face.card_type.core_types.contains(&CoreType::Land);
        // CR 305.6: must carry one of the five basic land type subtypes
        // (excludes Snow-Covered Wastes, whose subtype is "Wastes").
        let basic_type = is_snow_basic_land
            .then(|| {
                BASIC_LAND_TYPES.into_iter().find(|bt| {
                    face.card_type
                        .subtypes
                        .iter()
                        .any(|s| s.eq_ignore_ascii_case(bt))
                })
            })
            .flatten();
        match basic_type {
            Some(bt) => *per_type.entry(bt).or_insert(0) += 1,
            None => {
                non_snow_basic.insert(face.name.clone());
            }
        }
    }
    if !non_snow_basic.is_empty() {
        reasons.push(summarize_cards(
            "Momir's Madness decks may only contain the five snow basic lands \
             (Snow-Covered Plains/Island/Swamp/Mountain/Forest)",
            &non_snow_basic,
            6,
        ));
    }

    // The ratio is fixed: exactly 12 of each of the five snow basic types.
    for bt in BASIC_LAND_TYPES {
        let count = per_type.get(bt).copied().unwrap_or(0);
        if count != EXPECTED_PER_TYPE {
            reasons.push(format!(
                "Momir's Madness decks must contain exactly {EXPECTED_PER_TYPE} \
                 copies of Snow-Covered {bt} (found {count})"
            ));
        }
    }

    if !request.sideboard.is_empty() {
        reasons.push("Momir's Madness does not use a sideboard".to_string());
    }
    if !request.commander.is_empty() || !request.signature_spell.is_empty() {
        reasons.push("Momir's Madness does not use command-zone cards".to_string());
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn quick_momir_check(db: &CardDatabase, request: &DeckCompatibilityRequest) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_momir(db, request, &unknown_cards);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn quick_oathbreaker_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_oathbreaker(db, request, &unknown_cards);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn quick_planechase_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_planechase(db, request, &unknown_cards);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn quick_archenemy_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> QuickCheckResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let check = evaluate_archenemy(db, request, &unknown_cards);
    QuickCheckResult {
        reason: check.reasons.into_iter().next(),
        unknown_cards,
    }
}

fn evaluate_selected_format_summary(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> (Option<bool>, Vec<String>, BTreeSet<String>) {
    let Some(format) = request.selected_format else {
        return (None, Vec::new(), BTreeSet::new());
    };

    let result = match format {
        GameFormat::Standard => quick_constructed_check(
            db,
            request,
            LegalityFormat::Standard,
            "Standard",
            GameFormat::Standard.sideboard_policy(),
        ),
        GameFormat::Pioneer
        | GameFormat::Modern
        | GameFormat::Premodern
        | GameFormat::Legacy
        | GameFormat::Vintage
        | GameFormat::Historic
        | GameFormat::Timeless
        | GameFormat::Pauper => quick_constructed_check(
            db,
            request,
            format.legality_format().unwrap(),
            format.label(),
            format.sideboard_policy(),
        ),
        GameFormat::Commander => quick_commander_check(
            db,
            request,
            LegalityFormat::Commander,
            "Commander",
            CommanderVariantRules::commander(),
            100,
        ),
        GameFormat::PauperCommander | GameFormat::DuelCommander => quick_commander_check(
            db,
            request,
            format.legality_format().unwrap(),
            format.label(),
            match format {
                GameFormat::PauperCommander => CommanderVariantRules::pauper_commander(),
                GameFormat::DuelCommander => CommanderVariantRules::duel_commander(),
                _ => unreachable!("commander variant branch only handles PDH and Duel"),
            },
            100,
        ),
        GameFormat::TinyLeaders => quick_tiny_leaders_check(db, request),
        GameFormat::Oathbreaker => quick_oathbreaker_check(db, request),
        GameFormat::Momir => quick_momir_check(db, request),
        GameFormat::Planechase => quick_planechase_check(db, request),
        GameFormat::Archenemy => quick_archenemy_check(db, request),
        GameFormat::Brawl | GameFormat::HistoricBrawl => quick_brawl_check(
            db,
            request,
            format.legality_format().unwrap(),
            format.label(),
        ),
        GameFormat::FreeForAll | GameFormat::TwoHeadedGiant | GameFormat::Limited => {
            QuickCheckResult::compatible()
        }
    };

    (
        Some(result.reason.is_none()),
        result.reason.into_iter().collect(),
        result.unknown_cards,
    )
}

struct QuickCheckResult {
    reason: Option<String>,
    unknown_cards: BTreeSet<String>,
}

impl QuickCheckResult {
    fn compatible() -> Self {
        Self {
            reason: None,
            unknown_cards: BTreeSet::new(),
        }
    }

    fn incompatible(reason: String) -> Self {
        Self {
            reason: Some(reason),
            unknown_cards: BTreeSet::new(),
        }
    }

    fn unknown(name: &str) -> Self {
        Self {
            reason: Some(format!("Unknown cards: {name}")),
            unknown_cards: BTreeSet::from([name.to_string()]),
        }
    }
}

fn quick_constructed_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    legality_format: LegalityFormat,
    format_label: &str,
    sideboard_policy: SideboardPolicy,
) -> QuickCheckResult {
    if !request.commander.is_empty() {
        return QuickCheckResult::incompatible(format!(
            "{format_label} decks do not use a commander slot"
        ));
    }
    if request.main_deck.len() < 60 {
        return QuickCheckResult::incompatible(format!(
            "Main deck has {} cards (minimum 60)",
            request.main_deck.len()
        ));
    }
    if let SideboardPolicy::Limited(max) = sideboard_policy {
        if request.sideboard.len() as u32 > max {
            return QuickCheckResult::incompatible(format!(
                "Sideboard has {} cards (maximum {})",
                request.sideboard.len(),
                max
            ));
        }
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    let mut restricted = HashSet::new();
    for name in all_deck_cards(request) {
        let resolved = resolve_card_name(db, name);
        if db.get_face_by_name(resolved).is_none() {
            return QuickCheckResult::unknown(name);
        }
        let canonical = canonical_deck_count_key(db, name);
        *counts.entry(canonical.clone()).or_insert(0) += 1;
        match db.legality_status(resolved, legality_format) {
            Some(LegalityStatus::Legal) => {}
            Some(LegalityStatus::Restricted) => {
                restricted.insert(canonical);
            }
            Some(status) => {
                return QuickCheckResult::incompatible(format!(
                    "Not {format_label} legal: {name} ({})",
                    status_label(status)
                ));
            }
            None => {
                return QuickCheckResult::incompatible(format!(
                    "Not {format_label} legal: {name} (not legal in {format_label})"
                ));
            }
        }
    }

    if let Some(reason) = copy_limit_violations(db, &counts, 4).into_iter().next() {
        return QuickCheckResult::incompatible(format!(
            "More than 4 copies (main + sideboard combined): {reason}"
        ));
    }
    if let Some(reason) = restricted_copy_violations(db, &counts, &restricted)
        .into_iter()
        .next()
    {
        return QuickCheckResult::incompatible(format!(
            "More than 1 copy of a restricted card: {reason}"
        ));
    }

    QuickCheckResult::compatible()
}

fn quick_commander_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    legality_format: LegalityFormat,
    format_label: &str,
    rules: CommanderVariantRules,
    expected_total: usize,
) -> QuickCheckResult {
    if request.commander.is_empty() || request.commander.len() > 2 {
        return QuickCheckResult::incompatible(format!(
            "{format_label} decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }
    // CR 903.5e: Commander-style formats do not start with a sideboard. Extra
    // entries in the submitted list are silently ignored at game load (see
    // `load_deck_into_state` in `deck_loading.rs`) — strip them here so the
    // shape, singleton, and color-identity checks below operate on the actual
    // loaded deck.
    let stripped = request_without_sideboard(request);
    let request = &stripped;

    let represented_in_main = request
        .commander
        .iter()
        .filter(|name| {
            request
                .main_deck
                .iter()
                .any(|card| card.eq_ignore_ascii_case(name))
        })
        .count();
    let total_cards = request.main_deck.len() + (request.commander.len() - represented_in_main);
    if total_cards != expected_total {
        return QuickCheckResult::incompatible(format!(
            "{format_label} deck must have exactly {expected_total} cards (found {total_cards})"
        ));
    }

    let mut commander_identity = HashSet::new();
    for name in &request.commander {
        let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) else {
            return QuickCheckResult::unknown(name);
        };
        if !(rules.eligible)(face) {
            return QuickCheckResult::incompatible(format!("{}: {name}", rules.eligibility_error));
        }
        commander_identity.extend(card_color_identity(face));
    }
    if request.commander.len() == 2 {
        let face_a = db.get_face_by_name(resolve_card_name(db, &request.commander[0]));
        let face_b = db.get_face_by_name(resolve_card_name(db, &request.commander[1]));
        if let (Some(a), Some(b)) = (face_a, face_b) {
            if !are_valid_partners(a, b) {
                return QuickCheckResult::incompatible(format!(
                    "Invalid partner pairing: {} and {} do not have compatible partner keywords",
                    request.commander[0], request.commander[1]
                ));
            }
        }
    }

    let mut counts: HashMap<String, u32> = HashMap::new();
    for name in all_deck_cards(request) {
        let resolved = resolve_card_name(db, name);
        let Some(face) = db.get_face_by_name(resolved) else {
            return QuickCheckResult::unknown(name);
        };
        *counts
            .entry(canonical_deck_count_key(db, name))
            .or_insert(0) += 1;
        if !rules.skip_commander_legality
            || !request
                .commander
                .iter()
                .any(|commander| commander.eq_ignore_ascii_case(name))
        {
            match db.legality_status(resolved, legality_format) {
                Some(status) if status.is_legal() => {}
                Some(status) => {
                    return QuickCheckResult::incompatible(format!(
                        "Not {format_label} legal: {name} ({})",
                        status_label(status)
                    ));
                }
                None => {
                    return QuickCheckResult::incompatible(format!(
                        "Not {format_label} legal: {name} (not legal in {format_label})"
                    ));
                }
            }
        }
        if request
            .commander
            .iter()
            .any(|commander| commander.eq_ignore_ascii_case(name))
        {
            continue;
        }
        for color in card_color_identity(face) {
            if !commander_identity.contains(&color) {
                return QuickCheckResult::incompatible(format!(
                    "Cards outside commander's color identity: {name}"
                ));
            }
        }
    }

    if let Some(reason) = copy_limit_violations(db, &counts, 1).into_iter().next() {
        return QuickCheckResult::incompatible(format!("Singleton violations: {reason}"));
    }

    QuickCheckResult::compatible()
}

fn quick_brawl_check(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    legality_format: LegalityFormat,
    format_label: &str,
) -> QuickCheckResult {
    if request.commander.len() != 1 {
        return QuickCheckResult::incompatible(format!(
            "{format_label} decks require exactly 1 commander (found {})",
            request.commander.len()
        ));
    }
    let name = &request.commander[0];
    let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) else {
        return QuickCheckResult::unknown(name);
    };
    if !is_brawl_commander_eligible(face) {
        return QuickCheckResult::incompatible(format!(
            "{format_label} commander must be a legendary creature or legendary planeswalker: {name}"
        ));
    }

    quick_commander_check(
        db,
        request,
        legality_format,
        format_label,
        CommanderVariantRules {
            eligible: is_brawl_commander_eligible,
            eligibility_error:
                "Brawl commander must be a legendary creature or legendary planeswalker",
            skip_commander_legality: false,
        },
        60,
    )
}

fn evaluate_selected_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    standard: &CompatibilityCheck,
    commander: &CompatibilityCheck,
    bo3_ready: bool,
) -> (Option<bool>, Vec<String>) {
    let Some(format) = request.selected_format else {
        return (None, Vec::new());
    };

    let mut reasons = Vec::new();
    let mut compatible = match format {
        GameFormat::Standard => {
            if !standard.compatible {
                reasons.extend(standard.reasons.clone());
            }
            standard.compatible
        }
        GameFormat::Commander => {
            if !commander.compatible {
                reasons.extend(commander.reasons.clone());
            }
            commander.compatible
        }
        GameFormat::Pioneer
        | GameFormat::Modern
        | GameFormat::Premodern
        | GameFormat::Legacy
        | GameFormat::Vintage
        | GameFormat::Historic
        | GameFormat::Timeless
        | GameFormat::Pauper => {
            let check = evaluate_constructed(
                db,
                request,
                unknown_cards,
                format.legality_format().unwrap(),
                format.label(),
                format.sideboard_policy(),
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::PauperCommander | GameFormat::DuelCommander => {
            // Both variants share Commander's structural rules (100-card
            // singleton, command zone). We route them through the existing
            // Commander check against the format's own legality table — the
            // card pool differs from Commander but the deck shape is identical.
            let check = evaluate_commander_with_format(
                db,
                request,
                unknown_cards,
                format.legality_format().unwrap(),
                format.label(),
                match format {
                    GameFormat::PauperCommander => CommanderVariantRules::pauper_commander(),
                    GameFormat::DuelCommander => CommanderVariantRules::duel_commander(),
                    _ => unreachable!("commander variant branch only handles PDH and Duel"),
                },
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Brawl | GameFormat::HistoricBrawl => {
            let check = evaluate_brawl(
                db,
                request,
                unknown_cards,
                format.legality_format().unwrap(),
                format.label(),
            );
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::TinyLeaders => {
            let check = evaluate_tiny_leaders(db, request, unknown_cards);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Oathbreaker => {
            let check = evaluate_oathbreaker(db, request, unknown_cards);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Momir => {
            let check = evaluate_momir(db, request, unknown_cards);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Planechase => {
            let check = evaluate_planechase(db, request, unknown_cards);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::Archenemy => {
            let check = evaluate_archenemy(db, request, unknown_cards);
            if !check.compatible {
                reasons.extend(check.reasons);
            }
            check.compatible
        }
        GameFormat::FreeForAll | GameFormat::TwoHeadedGiant | GameFormat::Limited => true,
    };

    // CR 100.4 × MatchType::Bo3: BO3 requires a sideboard regardless of format.
    // `SideboardPolicy::Unlimited` formats (FreeForAll, TwoHeadedGiant) impose
    // no size cap, so the only cross-cutting requirement is non-empty. The
    // constructed-policy branches above enforce the 15-card upper bound.
    if matches!(request.selected_match_type, Some(MatchType::Bo3)) && !bo3_ready {
        compatible = false;
        reasons.push("BO3 requires a sideboard".to_string());
    }

    (Some(compatible), reasons)
}

fn evaluate_deck_coverage(db: &CardDatabase, request: &DeckCompatibilityRequest) -> DeckCoverage {
    // Count copies per card name for the tooltip severity indicator
    let mut copy_counts: HashMap<String, usize> = HashMap::new();
    for name in all_deck_cards(request) {
        let resolved = resolve_card_name(db, name);
        *copy_counts.entry(resolved.to_lowercase()).or_insert(0) += 1;
    }

    let unique_names: HashSet<&str> = all_deck_cards(request).collect();
    let mut unsupported_cards = Vec::new();
    let mut supported_count = 0usize;

    for name in &unique_names {
        let resolved = resolve_card_name(db, name);
        if let Some(face) = db.get_face_by_name(resolved) {
            let gaps = crate::game::coverage::card_face_gaps(face);
            if gaps.is_empty() {
                supported_count += 1;
            } else {
                let copies = copy_counts
                    .get(&face.name.to_lowercase())
                    .copied()
                    .unwrap_or(1);
                let parse_details = crate::game::coverage::build_parse_details_for_face(face);
                unsupported_cards.push(UnsupportedCard {
                    name: face.name.clone(),
                    gaps,
                    oracle_text: face.oracle_text.clone(),
                    parse_details,
                    copies,
                });
            }
        }
        // Unknown cards are already tracked separately; skip them here.
    }

    unsupported_cards.sort_by(|a, b| a.name.cmp(&b.name));

    DeckCoverage {
        total_unique: unique_names.len(),
        supported_unique: supported_count,
        unsupported_cards,
    }
}

/// Check deck legality across all known formats. A deck is "legal" in a format
/// only if every card is legal there. If any card is banned, the deck is "banned".
/// Otherwise if any card is not legal, the deck is "not_legal".
fn evaluate_format_legality(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> BTreeMap<String, String> {
    let unique_names: HashSet<&str> = all_deck_cards(request).collect();
    let mut result = BTreeMap::new();

    for format in LegalityFormat::ALL {
        let mut worst = LegalityStatus::Legal;
        for name in &unique_names {
            let resolved = resolve_card_name(db, name);
            let status = db
                .legality_status(resolved, format)
                .unwrap_or(LegalityStatus::NotLegal);
            match status {
                LegalityStatus::Banned => {
                    worst = LegalityStatus::Banned;
                    break; // Can't get worse
                }
                LegalityStatus::NotLegal => {
                    worst = LegalityStatus::NotLegal;
                    break; // Deck is already illegal — no need to scan further
                }
                LegalityStatus::Restricted | LegalityStatus::Legal => {}
            }
        }
        result.insert(
            format.as_key().to_string(),
            worst.as_export_str().to_string(),
        );
    }

    result
}

fn collect_unknown_cards(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> BTreeSet<String> {
    let mut unknown = BTreeSet::new();
    for name in all_deck_cards(request) {
        if !card_is_known(db, name) {
            unknown.insert(name.to_string());
        }
    }
    for name in &request.planar_deck {
        if !card_is_known(db, name) {
            unknown.insert(name.to_string());
        }
    }
    for name in &request.scheme_deck {
        if !card_is_known(db, name) {
            unknown.insert(name.to_string());
        }
    }
    unknown
}

/// CR 903.4: Compute color identity of a single card from mana cost + color indicator.
/// CR 903.5c: collect every main-deck card whose color identity is not a
/// subset of `identity`. Shared by the command-zone formats so the
/// color-identity-subset loop lives in one place instead of being copied per
/// format. `is_command_zone_card` skips cards that occupy the command zone
/// (e.g. a commander also listed in the main deck); unknown cards are skipped
/// so they are reported only once under "Unknown cards".
fn color_identity_violations(
    db: &CardDatabase,
    main_deck: &[String],
    identity: &HashSet<ManaColor>,
    unknown_cards: &BTreeSet<String>,
    is_command_zone_card: impl Fn(&str) -> bool,
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for name in main_deck {
        if is_command_zone_card(name.as_str()) || unknown_cards.contains(name.as_str()) {
            continue;
        }
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) {
            if card_color_identity(face)
                .iter()
                .any(|color| !identity.contains(color))
            {
                violations.insert(name.clone());
            }
        }
    }
    violations
}

fn card_color_identity(face: &CardFace) -> HashSet<ManaColor> {
    if !face.color_identity.is_empty() {
        return face.color_identity.iter().copied().collect();
    }

    let mut colors = HashSet::new();
    if let ManaCost::Cost { shards, .. } = &face.mana_cost {
        for shard in shards {
            for color in ManaColor::ALL {
                if shard.contributes_to(color) {
                    colors.insert(color);
                }
            }
        }
    }
    if let Some(overrides) = &face.color_override {
        for color in overrides {
            colors.insert(*color);
        }
    }
    colors
}

/// Collects the combined color identity of all cards in the deck from their mana costs
/// and color overrides, returned as single-letter codes in WUBRG order.
fn collect_color_identity(db: &CardDatabase, request: &DeckCompatibilityRequest) -> Vec<String> {
    let mut colors = HashSet::new();

    // Deduplicate card names — we only need each unique card once
    let unique_names: HashSet<&str> = all_deck_cards(request).collect();

    for name in unique_names {
        let resolved = resolve_card_name(db, name);
        if let Some(face) = db.get_face_by_name(resolved) {
            colors.extend(card_color_identity(face));
        }
    }

    // Return in canonical WUBRG order
    ManaColor::ALL
        .iter()
        .filter(|c| colors.contains(c))
        .map(mana_color_letter)
        .collect()
}

fn mana_color_letter(color: &ManaColor) -> String {
    match color {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
    }
    .to_string()
}

/// Returns true if the card is in the database, handling DFC names like "Front // Back"
/// by also trying just the front face name.
fn card_is_known(db: &CardDatabase, name: &str) -> bool {
    db.get_face_by_name(resolve_card_name(db, name)).is_some()
}

/// Combined copy counts across main deck + sideboard + commander, keyed by the
/// canonical (DFC-resolved, lowercased) card name so `"Plains"`/`"plains"` and
/// `"Delver of Secrets // Insectile Aberration"`/`"Delver of Secrets"` are
/// counted as the same card.
///
/// CR 201.3 + CR 100.2a: Canonical key for aggregating deck copy counts.
/// Uses the indexed face name when the card resolves so alias spellings
/// ("Nazgul" vs "Nazgûl") merge into one bucket for copy-limit checks.
fn canonical_deck_count_key(db: &CardDatabase, name: &str) -> String {
    let resolved = resolve_card_name(db, name);
    db.get_face_by_name(resolved)
        .map(|face| face.name.to_lowercase())
        .unwrap_or_else(|| resolved.to_lowercase())
}

fn combined_copy_counts(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for name in all_deck_cards(request) {
        let canonical = canonical_deck_count_key(db, name);
        *counts.entry(canonical).or_insert(0) += 1;
    }
    counts
}

/// CR 100.2a: Flag card names whose combined count exceeds `max_copies`,
/// excluding basic lands and cards whose Oracle text grants a per-card deck-limit
/// override (e.g. Relentless Rats — "any number"; Seven Dwarves → 7, Nazgûl → 9
/// via "up to N"; Vazal singleton → 1). The typed override is resolved from
/// `face.deck_copy_limit`, falling back to a live Oracle-text parse for faces
/// loaded without synthesis (test fixtures, `from_json_str`).
///
/// Input counts must be keyed by canonical (DFC-resolved, lowercased) names —
/// use `combined_copy_counts`.
fn copy_limit_violations(
    db: &CardDatabase,
    counts: &HashMap<String, u32>,
    max_copies: u32,
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for (canonical_name, count) in counts {
        // CR 100.2a + CR 205.4c: Basic lands are exempt from copy limits
        // regardless of any other override. "Basic" is a supertype (covering
        // Plains/Island/Swamp/Mountain/Forest, Snow-Covered variants, Wastes,
        // and any future basic), not a fixed name allowlist — trust the
        // MTGJSON-populated supertype field. Checked FIRST so basics never flag.
        if db
            .get_face_by_name(canonical_name)
            .is_some_and(|face| face.card_type.supertypes.contains(&Supertype::Basic))
        {
            continue;
        }
        // CR 100.2a / CR 903.5b: apply the per-card override when present,
        // otherwise the format-default `max_copies` (4 constructed, 1 singleton).
        match deck_copy_limit_for(db, canonical_name) {
            Some(DeckCopyLimit::Unlimited) => continue,
            Some(DeckCopyLimit::UpTo(n)) if *count <= n => continue,
            Some(DeckCopyLimit::UpTo(_)) => {} // override cap exceeded — flag
            None if *count <= max_copies => continue,
            None => {} // default limit exceeded — flag
        }
        // Prefer the database's canonical display casing for error messages;
        // fall back to the lowercased key if the face is missing (e.g. for
        // tests with unresolved names).
        let display = db
            .get_face_by_name(canonical_name)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| canonical_name.clone());
        violations.insert(format!("{display} ({count} copies)"));
    }
    violations
}

/// CR 100.2b: Flag any card the active format marks as `Restricted` whose
/// combined main+sideboard count exceeds 1. The 1-copy ceiling is
/// format-general — Vintage is the canonical consumer, but the rule applies
/// to any format whose legality table uses `Restricted`.
/// `restricted_canonical` is the set of canonical (DFC-resolved, lowercased)
/// names that the legality table marks as `Restricted` for the active format;
/// `counts` is the combined main+sideboard map produced by `combined_copy_counts`.
///
/// Note: this hardcodes the `<= 1` Restricted ceiling and does NOT consult any
/// per-card `DeckCopyLimit` override — no override card is currently
/// Vintage-Restricted, so the interaction is out of scope.
fn restricted_copy_violations(
    db: &CardDatabase,
    counts: &HashMap<String, u32>,
    restricted_canonical: &HashSet<String>,
) -> BTreeSet<String> {
    let mut violations = BTreeSet::new();
    for canonical in restricted_canonical {
        let Some(count) = counts.get(canonical) else {
            continue;
        };
        if *count <= 1 {
            continue;
        }
        let display = db
            .get_face_by_name(canonical)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| canonical.clone());
        violations.insert(format!("{display} ({count} copies)"));
    }
    violations
}

/// CR 100.2a / CR 903.5b: Resolve a card's deck-construction copy-limit override.
/// Reads the precomputed `face.deck_copy_limit` field; falls back to a live
/// Oracle-text parse for faces loaded without synthesis (test fixtures and
/// `CardDatabase::from_json_str` / `from_export_entries`, which skip synthesis).
/// Mirrors `is_commander_eligible`'s synthesized-field-with-live-fallback shape.
pub fn deck_copy_limit_for(db: &CardDatabase, canonical_name: &str) -> Option<DeckCopyLimit> {
    let face = db.get_face_by_name(canonical_name)?;
    if let Some(limit) = face.deck_copy_limit {
        return Some(limit);
    }
    compute_deck_copy_limit_from_text(face.oracle_text.as_deref()?)
}

/// Resolves a card name to the key used in the database. For DFC names like "Front // Back",
/// returns the front face name if that's how it's indexed.
fn resolve_card_name<'a>(db: &CardDatabase, name: &'a str) -> &'a str {
    if let Some((front, _)) = name.split_once("//") {
        let front = front.trim();
        if db.get_face_by_name(front).is_some() {
            return front;
        }
    }
    if db.get_face_by_name(name).is_some() {
        return name;
    }
    name
}

fn all_deck_cards(request: &DeckCompatibilityRequest) -> impl Iterator<Item = &str> {
    request
        .main_deck
        .iter()
        .chain(request.sideboard.iter())
        .chain(request.commander.iter())
        .chain(request.signature_spell.iter())
        .map(String::as_str)
}

fn status_label(status: LegalityStatus) -> &'static str {
    match status {
        LegalityStatus::Legal => "legal",
        LegalityStatus::NotLegal => "not legal",
        LegalityStatus::Banned => "banned",
        LegalityStatus::Restricted => "restricted",
    }
}

fn summarize_cards(prefix: &str, cards: &BTreeSet<String>, max_names: usize) -> String {
    let mut listed = cards.iter().take(max_names).cloned().collect::<Vec<_>>();
    if cards.len() > max_names {
        listed.push(format!("+{} more", cards.len() - max_names));
    }
    format!("{prefix}: {}", listed.join(", "))
}

/// CR 903.3: A card is eligible to be a commander if it is a legendary creature
/// (903.3a), a legendary Vehicle (903.3b), a legendary Spacecraft with one or more
/// power/toughness boxes (903.3c), a legendary Background enchantment (CR 702.124),
/// or has "can be your commander" in its rules text (903.3a override).
///
/// Reads the pre-computed `face.is_commander` field (union of MTGJSON
/// `leadershipSkills.commander` and our own type-line analysis, synthesized at
/// card-data build time). Falls back to a live type-line check for cards loaded
/// from test fixtures that may not have the field set — mirrors
/// `is_brawl_commander_eligible`.
pub fn is_commander_eligible(face: &CardFace) -> bool {
    if face.is_commander {
        return true;
    }
    crate::database::synthesis::type_line_commander_eligible(face)
}

fn is_pauper_commander_eligible(face: &CardFace) -> bool {
    use crate::types::card::Rarity;

    let is_creature_or_vehicle = face.card_type.core_types.contains(&CoreType::Creature)
        || face.card_type.subtypes.iter().any(|subtype| {
            subtype.eq_ignore_ascii_case("Vehicle") || subtype.eq_ignore_ascii_case("Spacecraft")
        });
    let has_uncommon_printing = face.rarities.contains(&Rarity::Uncommon);
    is_creature_or_vehicle && has_uncommon_printing
}

/// CR 702.124: Public entry point — can these two named cards form a legal
/// co-commander pair? Resolves both faces in the database and applies the full
/// partner-family rules. Returns false if either name is unknown.
///
/// This is the single authority for partner-pairing legality. Deck-builder UIs
/// consume it through the WASM bridge rather than re-implementing the rules, so
/// the engine and frontend can never disagree about a pairing.
pub fn can_pair_commanders(db: &CardDatabase, name_a: &str, name_b: &str) -> bool {
    match (db.get_face_by_name(name_a), db.get_face_by_name(name_b)) {
        (Some(a), Some(b)) => are_valid_partners(a, b),
        _ => false,
    }
}

/// CR 702.124: Check if two cards form a valid partner pair for co-commanders.
/// Handles the full partner family: Generic Partner, Partner with [Name],
/// Friends Forever, Character Select, Doctor's Companion, and Choose a Background.
fn are_valid_partners(face_a: &CardFace, face_b: &CardFace) -> bool {
    use crate::types::keywords::PartnerType;

    let partners_a: Vec<&PartnerType> = face_a
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Partner(pt) => Some(pt),
            _ => None,
        })
        .collect();
    let partners_b: Vec<&PartnerType> = face_b
        .keywords
        .iter()
        .filter_map(|kw| match kw {
            Keyword::Partner(pt) => Some(pt),
            _ => None,
        })
        .collect();

    // Any compatible combination across both cards' partner keywords is valid
    partners_a
        .iter()
        .any(|a| partners_b.iter().any(|b| partner_types_compatible(a, b, face_a, face_b)))
        // Also check asymmetric cases: one card has ChooseABackground/DoctorsCompanion
        // and the other has the matching subtype but no partner keyword
        || partners_a
            .iter()
            .any(|a| subtype_partner_match(a, face_b))
        || partners_b
            .iter()
            .any(|b| subtype_partner_match(b, face_a))
}

/// CR 702.124: Check if two partner types are compatible with each other.
fn partner_types_compatible(
    a: &crate::types::keywords::PartnerType,
    b: &crate::types::keywords::PartnerType,
    face_a: &CardFace,
    face_b: &CardFace,
) -> bool {
    use crate::types::keywords::PartnerType;

    match (a, b) {
        (PartnerType::Generic, PartnerType::Generic) => true,
        (PartnerType::With(x), PartnerType::With(y)) => {
            x.eq_ignore_ascii_case(&face_b.name) && y.eq_ignore_ascii_case(&face_a.name)
        }
        (PartnerType::FriendsForever, PartnerType::FriendsForever) => true,
        (PartnerType::CharacterSelect, PartnerType::CharacterSelect) => true,
        _ => false,
    }
}

/// CR 702.124m: Doctor's companion pairs with a legendary Time Lord Doctor
/// creature card that has no other creature types.
fn is_time_lord_doctor_commander(face: &CardFace) -> bool {
    if !face.card_type.supertypes.contains(&Supertype::Legendary)
        || !face.card_type.core_types.contains(&CoreType::Creature)
    {
        return false;
    }
    if !is_commander_eligible(face) {
        return false;
    }
    let subtypes = &face.card_type.subtypes;
    // MTGJSON may emit the single two-word subtype or the split pair.
    if subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Time Lord Doctor"))
    {
        return subtypes.len() == 1;
    }
    subtypes.len() == 2
        && subtypes.iter().any(|s| s.eq_ignore_ascii_case("Doctor"))
        && subtypes.iter().any(|s| s.eq_ignore_ascii_case("Time Lord"))
        && subtypes
            .iter()
            .all(|s| s.eq_ignore_ascii_case("Doctor") || s.eq_ignore_ascii_case("Time Lord"))
}

/// CR 702.124k + CR 702.124m: Check if a partner type matches the other face by subtype.
/// Doctor's Companion pairs with a Time Lord Doctor commander; Choose a Background
/// pairs with any Background.
fn subtype_partner_match(
    partner_type: &crate::types::keywords::PartnerType,
    other_face: &CardFace,
) -> bool {
    use crate::types::keywords::PartnerType;

    match partner_type {
        PartnerType::DoctorsCompanion => is_time_lord_doctor_commander(other_face),
        PartnerType::ChooseABackground => other_face
            .card_type
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Background")),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::types::keywords::PartnerType;
    use serde_json::{Map, Value};

    fn test_db_json() -> String {
        serde_json::json!({
            "legal standard": {
                "name": "Legal Standard",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "premodern": "legal",
                    "pioneer": "legal",
                    "pauper": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "premodern": "legal",
                    "pioneer": "legal",
                    "pauper": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "not standard": {
                "name": "Not Standard",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "not_legal",
                    "commander": "legal",
                    "premodern": "not_legal",
                    "pioneer": "legal",
                    "pauper": "not_legal",
                    "standardbrawl": "not_legal",
                    "brawl": "legal"
                }
            },
            "pioneer only": {
                "name": "Pioneer Only",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "not_legal",
                    "commander": "legal",
                    "pioneer": "legal",
                    "pauper": "not_legal"
                }
            },
            "premodern banned": {
                "name": "Premodern Banned",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "premodern": "banned"
                }
            },
            "commander banned": {
                "name": "Commander Banned",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "banned"
                }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "legendary planeswalker": {
                "name": "Legendary Planeswalker",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Planeswalker"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "standardbrawl": "legal",
                    "brawl": "legal"
                }
            },
            "partner commander": {
                "name": "Partner Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": "Partner",
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [{ "Partner": { "type": "Generic" } }],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal"
                }
            },
            "grub commander": {
                "name": "Grub Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["Black", "Red"],
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal"
                }
            },
            "red card": {
                "name": "Red Card",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["Red"],
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal"
                }
            },
            "mountain": {
                "name": "Mountain",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Mountain"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Red"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "relentless rats": {
                "name": "Relentless Rats",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Rat"] },
                "power": "2", "toughness": "2", "loyalty": null, "defense": null,
                "oracle_text": "This creature gets +1/+1 for each other creature on the battlefield named Relentless Rats.\nA deck can have any number of cards named Relentless Rats.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Black"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "seven dwarves": {
                "name": "Seven Dwarves",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Dwarf"] },
                "power": "3", "toughness": "3", "loyalty": null, "defense": null,
                "oracle_text": "This creature gets +1/+1 for each other creature named Seven Dwarves you control.\nA deck can have up to seven cards named Seven Dwarves.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Red"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "nazgûl": {
                "name": "Nazgûl",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Wraith"] },
                "power": "3", "toughness": "3", "loyalty": null, "defense": null,
                "oracle_text": "Deathtouch\nA deck can have up to nine cards named Nazgûl.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "color_identity": ["Black"], "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered plains": {
                "name": "Snow-Covered Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Plains"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered island": {
                "name": "Snow-Covered Island",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Island"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered swamp": {
                "name": "Snow-Covered Swamp",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Swamp"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered mountain": {
                "name": "Snow-Covered Mountain",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Mountain"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered forest": {
                "name": "Snow-Covered Forest",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Forest"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            },
            "snow-covered wastes": {
                "name": "Snow-Covered Wastes",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic", "Snow"], "core_types": ["Land"], "subtypes": ["Wastes"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "standard": "legal", "commander": "legal" }
            }
        })
        .to_string()
    }

    fn expand(name: &str, count: usize) -> Vec<String> {
        (0..count).map(|_| name.to_string()).collect()
    }

    /// Build a 60-card main deck with 4x `name` plus 56x Plains, respecting the
    /// 4-per-name rule (CR 100.2a) while keeping the target card in the deck.
    fn legal_60_main(name: &str) -> Vec<String> {
        let mut deck = expand(name, 4);
        deck.extend(expand("Plains", 56));
        deck
    }

    fn planechase_card_json(name: &str, supertypes: &[&str], core_types: &[&str]) -> Value {
        serde_json::json!({
            "name": name,
            "mana_cost": { "type": "NoCost" },
            "card_type": {
                "supertypes": supertypes,
                "core_types": core_types,
                "subtypes": []
            },
            "power": null,
            "toughness": null,
            "loyalty": null,
            "defense": null,
            "oracle_text": null,
            "non_ability_text": null,
            "flavor_name": null,
            "keywords": [],
            "abilities": [],
            "triggers": [],
            "static_abilities": [],
            "replacements": [],
            "color_override": null,
            "scryfall_oracle_id": null,
            "legalities": {
                "standard": "legal",
                "commander": "legal"
            }
        })
    }

    fn insert_planechase_card(
        cards: &mut Map<String, Value>,
        name: &str,
        supertypes: &[&str],
        core_types: &[&str],
    ) {
        cards.insert(
            name.to_lowercase(),
            planechase_card_json(name, supertypes, core_types),
        );
    }

    fn planechase_test_db() -> CardDatabase {
        let mut cards = Map::new();
        insert_planechase_card(&mut cards, "Legal Standard", &[], &[]);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        for index in 1..=40 {
            insert_planechase_card(&mut cards, &format!("Plane {index}"), &[], &["Plane"]);
        }
        for index in 1..=5 {
            insert_planechase_card(
                &mut cards,
                &format!("Phenomenon {index}"),
                &[],
                &["Phenomenon"],
            );
        }
        CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap()
    }

    fn plane_names(count: usize) -> Vec<String> {
        (1..=count).map(|index| format!("Plane {index}")).collect()
    }

    fn phenomenon_names(count: usize) -> Vec<String> {
        (1..=count)
            .map(|index| format!("Phenomenon {index}"))
            .collect()
    }

    fn planechase_request(
        player_count: usize,
        planar_deck: Vec<String>,
    ) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck,
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Planechase),
            selected_match_type: None,
            player_count,
            summary_only: false,
        }
    }

    fn insert_scheme_card(cards: &mut Map<String, Value>, name: &str) {
        cards.insert(
            name.to_lowercase(),
            planechase_card_json(name, &[], &["Scheme"]),
        );
    }

    fn insert_unsupported_scheme_card(cards: &mut Map<String, Value>, name: &str) {
        let mut card = planechase_card_json(name, &[], &["Scheme"]);
        card["abilities"] = serde_json::to_value(vec![crate::types::AbilityDefinition::new(
            crate::types::AbilityKind::Spell,
            crate::types::Effect::unimplemented("scheme_test", "unsupported scheme test"),
        )])
        .unwrap();
        cards.insert(name.to_lowercase(), card);
    }

    fn archenemy_test_db() -> CardDatabase {
        let mut cards = Map::new();
        insert_planechase_card(&mut cards, "Legal Standard", &[], &[]);
        insert_planechase_card(&mut cards, "Plains", &["Basic"], &["Land"]);
        for index in 1..=20 {
            insert_scheme_card(&mut cards, &format!("Scheme {index}"));
        }
        insert_unsupported_scheme_card(&mut cards, "Unsupported Scheme");
        CardDatabase::from_json_str(&Value::Object(cards).to_string()).unwrap()
    }

    fn scheme_names(count: usize) -> Vec<String> {
        (1..=count).map(|index| format!("Scheme {index}")).collect()
    }

    fn archenemy_request(scheme_deck: Vec<String>) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck,
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Archenemy),
            selected_match_type: None,
            player_count: 4,
            summary_only: false,
        }
    }

    #[test]
    fn archenemy_accepts_valid_twenty_card_scheme_deck() {
        let db = archenemy_test_db();
        let request = archenemy_request(scheme_names(20));
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new());

        assert!(check.compatible, "reasons: {:?}", check.reasons);
    }

    #[test]
    fn archenemy_rejects_short_scheme_deck() {
        let db = archenemy_test_db();
        let request = archenemy_request(scheme_names(19));
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("minimum 20")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn archenemy_rejects_third_scheme_copy() {
        let db = archenemy_test_db();
        let mut scheme_deck = scheme_names(18);
        scheme_deck.extend(["Scheme 1".to_string(), "Scheme 1".to_string()]);
        let request = archenemy_request(scheme_deck);
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("copy-limit")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn archenemy_rejects_non_scheme_in_scheme_deck() {
        let db = archenemy_test_db();
        let mut scheme_deck = scheme_names(19);
        scheme_deck.push("Legal Standard".to_string());
        let request = archenemy_request(scheme_deck);
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("must be Scheme")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn archenemy_rejects_unsupported_scheme() {
        let db = archenemy_test_db();
        let mut scheme_deck = scheme_names(19);
        scheme_deck.push("Unsupported Scheme".to_string());
        let request = archenemy_request(scheme_deck);
        let check = evaluate_archenemy(&db, &request, &BTreeSet::new());

        assert!(!check.compatible);
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("Unsupported scheme cards")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_planar_minimum_scales_with_actual_player_count() {
        let db = planechase_test_db();

        for (player_count, minimum) in [(2, 20), (3, 30), (4, 40)] {
            let short = planechase_request(player_count, plane_names(minimum - 1));
            let check = evaluate_planechase(&db, &short, &BTreeSet::new());
            assert!(
                !check.compatible,
                "{player_count}-player Planechase must reject a {minimum_minus_one}-card planar deck",
                minimum_minus_one = minimum - 1
            );
            assert!(
                check
                    .reasons
                    .iter()
                    .any(|reason| reason.contains(&format!("minimum {minimum}"))),
                "reasons: {:?}",
                check.reasons
            );

            let exact = planechase_request(player_count, plane_names(minimum));
            let check = evaluate_planechase(&db, &exact, &BTreeSet::new());
            assert!(
                check.compatible,
                "{player_count}-player Planechase must accept exactly {minimum} planar cards, reasons: {:?}",
                check.reasons
            );
        }
    }

    #[test]
    fn planechase_phenomenon_cap_is_player_count_scaled() {
        let db = planechase_test_db();
        let mut planar_deck = plane_names(15);
        planar_deck.extend(phenomenon_names(5));

        let check = evaluate_planechase(&db, &planechase_request(2, planar_deck), &BTreeSet::new());

        assert!(!check.compatible, "five phenomena exceeds the 2-player cap");
        assert!(
            check
                .reasons
                .iter()
                .any(|reason| reason.contains("5 phenomena (maximum 4)")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_planar_deck_is_singleton_by_english_name() {
        let db = planechase_test_db();
        let mut planar_deck = plane_names(19);
        planar_deck.push("Plane 1".to_string());

        let check = evaluate_planechase(&db, &planechase_request(2, planar_deck), &BTreeSet::new());

        assert!(
            !check.compatible,
            "duplicate English names must be rejected"
        );
        assert!(
            check.reasons.iter().any(|reason| {
                reason.contains("Planar deck singleton violations") && reason.contains("Plane 1")
            }),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_planar_deck_rejects_non_planar_cards() {
        let db = planechase_test_db();
        let mut planar_deck = plane_names(19);
        planar_deck.push("Legal Standard".to_string());

        let check = evaluate_planechase(&db, &planechase_request(2, planar_deck), &BTreeSet::new());

        assert!(
            !check.compatible,
            "main-deck cards cannot appear in the planar deck"
        );
        assert!(
            check.reasons.iter().any(|reason| {
                reason.contains("Planar deck cards must be Plane or Phenomenon")
                    && reason.contains("Legal Standard")
            }),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn planechase_empty_custom_planar_deck_is_allowed() {
        let db = planechase_test_db();
        let check = evaluate_planechase(&db, &planechase_request(2, Vec::new()), &BTreeSet::new());

        assert!(
            check.compatible,
            "empty custom planar deck should pass validation so loading can use the default deck, reasons: {:?}",
            check.reasons
        );
    }

    fn tiny_leaders_test_db_json() -> String {
        serde_json::json!({
            "white tiny leader": {
                "name": "White Tiny Leader",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 2 },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["White"],
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "ajani, nacatl pariah": {
                "name": "Ajani, Nacatl Pariah",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 2 },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Planeswalker"], "subtypes": ["Ajani"] },
                "power": null,
                "toughness": null,
                "loyalty": "3",
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "color_identity": ["White"],
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Plains"] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "big spell": {
                "name": "Big Spell",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 4 },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "sol ring": {
                "name": "Sol Ring",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 1 },
                "card_type": { "supertypes": [], "core_types": ["Artifact"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string()
    }

    #[test]
    fn standard_legal_deck_passes() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.standard.compatible,
            "expected legal deck to pass, reasons: {:?}",
            result.standard.reasons
        );
    }

    #[test]
    fn standard_illegal_deck_reports_reasons() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut deck = expand("Legal Standard", 59);
        deck.push("Not Standard".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: deck,
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.standard.compatible);
        assert!(result
            .standard
            .reasons
            .iter()
            .any(|r| r.contains("Standard decks do not use a commander slot")));
        assert!(result
            .standard
            .reasons
            .iter()
            .any(|r| r.contains("Not Standard")));
    }

    // CR 100.2a / CR 903.5b: per-card copy-limit overrides drive
    // `copy_limit_violations`. Faces loaded via `from_json_str` skip synthesis,
    // so the limit is resolved through the live Oracle-text fallback in
    // `deck_copy_limit_for`. Helpers below build the canonical count map the way
    // the production callers do.
    fn counts_of(pairs: &[(&str, u32)]) -> HashMap<String, u32> {
        pairs
            .iter()
            .map(|(name, n)| (name.to_ascii_lowercase(), *n))
            .collect()
    }

    #[test]
    fn copy_limit_respects_typed_overrides_constructed() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        // Seven Dwarves: UpTo(7) — 7 legal, 8 illegal.
        assert!(copy_limit_violations(&db, &counts_of(&[("Seven Dwarves", 7)]), 4).is_empty());
        assert!(!copy_limit_violations(&db, &counts_of(&[("Seven Dwarves", 8)]), 4).is_empty());

        // Nazgûl: UpTo(9) — 8 legal, 10 illegal.
        assert!(copy_limit_violations(&db, &counts_of(&[("Nazgûl", 8)]), 4).is_empty());
        assert!(!copy_limit_violations(&db, &counts_of(&[("Nazgûl", 10)]), 4).is_empty());

        // Relentless Rats: Unlimited — 5 legal.
        assert!(copy_limit_violations(&db, &counts_of(&[("Relentless Rats", 5)]), 4).is_empty());

        // Mountain: basic-land exemption — 30 legal.
        assert!(copy_limit_violations(&db, &counts_of(&[("Mountain", 30)]), 4).is_empty());

        // A normal card with no override is still flagged at 5.
        let violations = copy_limit_violations(&db, &counts_of(&[("Red Card", 5)]), 4);
        assert!(violations.iter().any(|v| v.contains("Red Card")));
    }

    #[test]
    fn copy_limit_override_fires_before_commander_singleton() {
        // CR 903.5b: in a singleton (Commander) context max_copies = 1, but
        // Nazgûl's UpTo(9) override must raise the cap so 9 copies are legal.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        assert!(copy_limit_violations(&db, &counts_of(&[("Nazgûl", 9)]), 1).is_empty());
        // A normal card is still singleton-restricted to 1.
        assert!(!copy_limit_violations(&db, &counts_of(&[("Red Card", 2)]), 1).is_empty());
    }

    #[test]
    fn combined_copy_counts_merge_nazgul_spelling_variants() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Nazgul", 5);
        main.extend(expand("Nazgûl", 5));
        main.extend(expand("Plains", 80));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let counts = combined_copy_counts(&db, &request);
        assert_eq!(counts.get("nazgûl"), Some(&10));
        assert!(!copy_limit_violations(&db, &counts, 1).is_empty());
    }

    #[test]
    fn commander_accepts_nine_nazgul_copies() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Nazgul", 9);
        main.extend(expand("Mountain", 90));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Grub Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible commander deck, got: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn summary_commander_accepts_nine_nazgul_copies() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Nazgul", 9);
        main.extend(expand("Mountain", 90));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Grub Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: true,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn commander_rules_detect_size_singleton_and_legality_failures() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 97);
        main.push("Commander Banned".to_string());
        main.push("Commander Banned".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            // CR 903.5e: a Commander deck's sideboard slot is Phase's
            // builder-only Maybeboard — extra entries are accepted by the
            // validator and dropped at game load. They must not contribute
            // to the singleton count below.
            sideboard: vec!["Legal Standard".to_string()],
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Singleton violations")));
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Commander Banned")));
    }

    #[test]
    fn bo3_ready_depends_on_sideboard() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let no_sideboard = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: Some(MatchType::Bo3),
            player_count: default_player_count(),
            summary_only: false,
        };
        let with_sideboard = DeckCompatibilityRequest {
            sideboard: vec!["Legal Standard".to_string()],
            ..no_sideboard.clone()
        };

        let no_sb_result = evaluate_deck_compatibility(&db, &no_sideboard);
        assert!(!no_sb_result.bo3_ready);
        assert_eq!(no_sb_result.selected_format_compatible, Some(false));
        assert!(no_sb_result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("BO3 requires a sideboard")));

        let with_sb_result = evaluate_deck_compatibility(&db, &with_sideboard);
        assert!(with_sb_result.bo3_ready);
    }

    #[test]
    fn unknown_cards_are_reported() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Mystery Card".to_string()],
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.unknown_cards, vec!["Mystery Card".to_string()]);
        assert!(!result.standard.compatible);
        assert!(!result.commander.compatible);
        assert!(result
            .standard
            .reasons
            .iter()
            .any(|reason| reason.contains("Unknown cards")));
    }

    #[test]
    fn commander_requires_eligible_commander_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|reason| reason.contains("must be legendary creatures")));
    }

    #[test]
    fn pauper_commander_allows_nonlegendary_creature_commander_slot() {
        let db_json = serde_json::json!({
            "pdh commander": {
                "name": "PDH Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "rarities": ["uncommon"],
                "legalities": {}
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "paupercommander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["PDH Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::PauperCommander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(true));
        assert!(
            result.selected_format_reasons.is_empty(),
            "{:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn pauper_commander_rejects_rare_only_creature() {
        let db_json = serde_json::json!({
            "rare creature": {
                "name": "Rare Creature",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "rarities": ["rare"],
                "legalities": {}
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "paupercommander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Rare Creature".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::PauperCommander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("uncommon creature")));
    }

    #[test]
    fn pauper_commander_rejects_uncommon_noncreature() {
        let db_json = serde_json::json!({
            "uncommon sorcery": {
                "name": "Uncommon Sorcery",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "rarities": ["uncommon"],
                "legalities": { "paupercommander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": { "paupercommander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Uncommon Sorcery".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::PauperCommander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("uncommon creature")));
    }

    #[test]
    fn commander_partners_require_partner_keyword() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 98),
            sideboard: Vec::new(),
            commander: vec![
                "Partner Commander".to_string(),
                "Legal Commander".to_string(),
            ],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|reason| reason.contains("Invalid partner pairing")));
    }

    #[test]
    fn selected_format_defaults_to_true_for_ffa_and_two_headed_giant() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: Vec::new(),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::FreeForAll),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let thg_request = DeckCompatibilityRequest {
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::TwoHeadedGiant),
            ..request.clone()
        };

        assert_eq!(
            evaluate_deck_compatibility(&db, &request).selected_format_compatible,
            Some(true)
        );
        assert_eq!(
            evaluate_deck_compatibility(&db, &thg_request).selected_format_compatible,
            Some(true)
        );
    }

    #[test]
    fn selected_standard_and_commander_use_corresponding_checks() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let standard_request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: Some(MatchType::Bo1),
            player_count: default_player_count(),
            summary_only: false,
        };
        let commander_request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: Some(MatchType::Bo1),
            player_count: default_player_count(),
            summary_only: false,
        };

        let standard_result = evaluate_deck_compatibility(&db, &standard_request);
        let commander_result = evaluate_deck_compatibility(&db, &commander_request);

        assert!(standard_result.standard.compatible);
        assert_eq!(standard_result.selected_format_compatible, Some(true));
        assert_eq!(
            commander_result.selected_format_compatible,
            Some(commander_result.commander.compatible)
        );
    }

    #[test]
    fn summarize_cards_limits_output() {
        let cards = (0..10)
            .map(|i| format!("Card {i}"))
            .collect::<BTreeSet<String>>();
        let text = summarize_cards("Example", &cards, 3);
        assert!(text.contains("+7 more"));
    }

    #[test]
    fn pioneer_selected_format_validates_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Legal deck: all cards are pioneer-legal
        let legal_request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Pioneer Only"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Pioneer),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &legal_request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn premodern_selected_format_validates_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Premodern),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn premodern_selected_format_rejects_banned_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Premodern Banned"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Premodern),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Not Premodern legal") && r.contains("banned")));
    }

    #[test]
    fn premodern_selected_format_rejects_missing_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Pioneer Only"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Premodern),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Pioneer Only") && r.contains("not legal in Premodern")));
    }

    #[test]
    fn premodern_selected_format_enforces_constructed_deck_shape() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        let commander_request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Premodern),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &commander_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Premodern decks do not use a commander slot")));

        let oversize_sideboard = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Premodern),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &oversize_sideboard);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Sideboard has 16") && r.contains("maximum 15")));

        let mut main = expand("Legal Standard", 5);
        main.extend(expand("Plains", 55));
        let copy_limit = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Premodern),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &copy_limit);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("More than 4 copies")));
    }

    #[test]
    fn validate_name_deck_for_format_rejects_non_premodern_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let main_deck = legal_60_main("Pioneer Only");
        let result =
            validate_name_deck_for_format(&db, &main_deck, &[], &[], GameFormat::Premodern, None);

        let reasons = result.expect_err("Premodern validation must reject missing legality");
        assert!(reasons.iter().any(|r| r.contains("Not Premodern legal")));
    }

    #[test]
    fn pauper_selected_format_rejects_illegal_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Pioneer Only card is not pauper-legal
        let illegal_request = DeckCompatibilityRequest {
            main_deck: expand("Pioneer Only", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Pauper),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &illegal_request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Not Pauper legal")));
    }

    #[test]
    fn evaluate_constructed_checks_deck_size_and_commander_slot() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let unknown_cards = BTreeSet::new();
        // Too few cards + has commander slot
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 30),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let check = evaluate_constructed(
            &db,
            &request,
            &unknown_cards,
            LegalityFormat::Pioneer,
            "Pioneer",
            GameFormat::Pioneer.sideboard_policy(),
        );
        assert!(!check.compatible);
        assert!(check.reasons.iter().any(|r| r.contains("minimum 60")));
        assert!(check
            .reasons
            .iter()
            .any(|r| r.contains("Pioneer decks do not use a commander slot")));
    }

    #[test]
    fn brawl_valid_deck_passes() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn brawl_planeswalker_commander_is_valid() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legendary Planeswalker".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn brawl_rejects_non_legendary_commander() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 59),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("legendary creature or legendary planeswalker")));
    }

    #[test]
    fn brawl_rejects_partner() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 58),
            sideboard: Vec::new(),
            commander: vec![
                "Legal Commander".to_string(),
                "Partner Commander".to_string(),
            ],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 1 commander")));
    }

    #[test]
    fn brawl_rejects_wrong_deck_size() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 60 cards")));
    }

    #[test]
    fn tiny_leaders_valid_deck_passes() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 49),
            sideboard: expand("Plains", 10),
            commander: vec!["White Tiny Leader".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::TinyLeaders),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "{:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn tiny_leaders_allows_legendary_planeswalker_leaders() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let face = db
            .get_face_by_name("Ajani, Nacatl Pariah")
            .expect("fixture planeswalker exists");

        assert!(is_tiny_leader_eligible(face));
    }

    #[test]
    fn tiny_leaders_rejects_cost_identity_and_deck_ban() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let mut main = expand("Plains", 47);
        main.push("Big Spell".to_string());
        main.push("Sol Ring".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["White Tiny Leader".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::TinyLeaders),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Tiny cost identity") && r.contains("Big Spell")));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Banned in Tiny Leaders") && r.contains("Sol Ring")));
    }

    #[test]
    fn tiny_leaders_rejects_commander_only_ban() {
        let db = CardDatabase::from_json_str(&tiny_leaders_test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 49),
            sideboard: Vec::new(),
            commander: vec!["Ajani, Nacatl Pariah".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::TinyLeaders),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Banned as Tiny Leader") && r.contains("Ajani")));
    }

    #[test]
    fn historic_brawl_uses_brawl_legality() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // "Not Standard" has brawl: legal but standardbrawl: not_legal
        // Use basic lands to avoid singleton violations, plus one non-basic to test legality
        let mut main = expand("Plains", 58);
        main.push("Not Standard".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::HistoricBrawl),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));

        // Same deck should fail Standard Brawl
        let brawl_request = DeckCompatibilityRequest {
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Brawl),
            ..request
        };
        let brawl_result = evaluate_deck_compatibility(&db, &brawl_request);
        assert_eq!(brawl_result.selected_format_compatible, Some(false));
        assert!(brawl_result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Not Brawl legal")));
    }

    // --- Partner family validation tests ---

    /// Build a minimal CardFace with specific partner keywords for unit testing.
    fn partner_face(name: &str, keywords: Vec<Keyword>, subtypes: Vec<&str>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Creature],
                subtypes: subtypes.into_iter().map(String::from).collect(),
            },
            keywords,
            ..CardFace::default()
        }
    }

    #[test]
    fn partner_generic_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face("A", vec![Keyword::Partner(PartnerType::Generic)], vec![]);
        let b = partner_face("B", vec![Keyword::Partner(PartnerType::Generic)], vec![]);
        assert!(are_valid_partners(&a, &b));
    }

    #[test]
    fn partner_with_matched_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "Brallin, Skyshark Rider",
            vec![Keyword::Partner(PartnerType::With(
                "Shabraz, the Skyshark".to_string(),
            ))],
            vec![],
        );
        let b = partner_face(
            "Shabraz, the Skyshark",
            vec![Keyword::Partner(PartnerType::With(
                "Brallin, Skyshark Rider".to_string(),
            ))],
            vec![],
        );
        assert!(are_valid_partners(&a, &b));
    }

    #[test]
    fn partner_with_mismatched_names_rejected() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "A",
            vec![Keyword::Partner(PartnerType::With("C".to_string()))],
            vec![],
        );
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::With("D".to_string()))],
            vec![],
        );
        assert!(!are_valid_partners(&a, &b));
    }

    #[test]
    fn friends_forever_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "A",
            vec![Keyword::Partner(PartnerType::FriendsForever)],
            vec![],
        );
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::FriendsForever)],
            vec![],
        );
        assert!(are_valid_partners(&a, &b));
    }

    #[test]
    fn character_select_pair_is_valid() {
        use crate::types::keywords::PartnerType;
        let a = partner_face(
            "A",
            vec![Keyword::Partner(PartnerType::CharacterSelect)],
            vec![],
        );
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::CharacterSelect)],
            vec![],
        );
        assert!(are_valid_partners(&a, &b));
    }

    #[test]
    fn doctors_companion_pairs_with_doctor_subtype() {
        use crate::types::keywords::PartnerType;
        let companion = partner_face(
            "Amy Pond",
            vec![Keyword::Partner(PartnerType::DoctorsCompanion)],
            vec![],
        );
        let doctor = partner_face("The Thirteenth Doctor", vec![], vec!["Doctor", "Time Lord"]);
        assert!(are_valid_partners(&companion, &doctor));
        // Reversed order also works
        assert!(are_valid_partners(&doctor, &companion));
    }

    #[test]
    fn choose_a_background_pairs_with_background_subtype() {
        use crate::types::keywords::PartnerType;
        let commander = partner_face(
            "Wilson, Refined Grizzly",
            vec![Keyword::Partner(PartnerType::ChooseABackground)],
            vec![],
        );
        // Background enchantment (not a creature)
        let mut bg = CardFace {
            name: "Criminal Past".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Background".to_string()],
            },
            ..CardFace::default()
        };
        assert!(are_valid_partners(&commander, &bg));
        // Background enchantment is commander-eligible
        assert!(is_commander_eligible(&bg));

        // Non-Background enchantment is not a valid partner
        bg.card_type.subtypes = vec!["Aura".to_string()];
        assert!(!are_valid_partners(&commander, &bg));
    }

    #[test]
    fn commander_eligibility_uses_parsed_permission_text() {
        let mut face = CardFace {
            name: "Teferi, Temporal Archmage".to_string(),
            oracle_text: Some("Teferi, Temporal Archmage can be your commander.".to_string()),
            ..CardFace::default()
        };
        face.card_type.supertypes.push(Supertype::Legendary);
        face.card_type.core_types.push(CoreType::Planeswalker);

        assert!(is_commander_eligible(&face));

        face.oracle_text = Some("Teferi, Temporal Archmage can't be your commander.".to_string());
        assert!(!is_commander_eligible(&face));
    }

    /// CR 903.3(c): A legendary Spacecraft with one or more power/toughness boxes
    /// is commander-eligible. Hearthhull, the Worldseed is the motivating case
    /// (Legendary Artifact — Spacecraft with printed P/T 6/7).
    #[test]
    fn commander_eligibility_accepts_legendary_spacecraft_with_pt() {
        use crate::types::ability::PtValue;

        let face = CardFace {
            name: "Hearthhull, the Worldseed".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Spacecraft".to_string()],
            },
            power: Some(PtValue::Fixed(6)),
            toughness: Some(PtValue::Fixed(7)),
            ..CardFace::default()
        };
        assert!(is_commander_eligible(&face));

        // CR 903.3(c) explicitly requires a power/toughness box. A Spacecraft
        // without P/T is *not* eligible (no such card exists today; the guard is
        // load-bearing for forward compatibility).
        let face_no_pt = CardFace {
            power: None,
            toughness: None,
            ..face.clone()
        };
        assert!(!is_commander_eligible(&face_no_pt));
    }

    /// CR 903.3(b): A legendary Vehicle is commander-eligible regardless of
    /// whether it is currently a creature. Verifies the type-line path catches
    /// legendary Vehicles independent of MTGJSON's leadershipSkills bit.
    #[test]
    fn commander_eligibility_accepts_legendary_vehicle() {
        let face = CardFace {
            name: "Parnesse, the Subtle Brush".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Vehicle".to_string()],
            },
            ..CardFace::default()
        };
        assert!(is_commander_eligible(&face));
    }

    /// Non-legendary Spacecraft / Vehicle must NOT be commander-eligible —
    /// the legendary supertype is required by every clause of CR 903.3.
    #[test]
    fn commander_eligibility_rejects_non_legendary_spacecraft_or_vehicle() {
        use crate::types::ability::PtValue;

        let spacecraft = CardFace {
            name: "Hypothetical Common Spacecraft".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Spacecraft".to_string()],
            },
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            ..CardFace::default()
        };
        assert!(!is_commander_eligible(&spacecraft));

        let vehicle = CardFace {
            name: "Smuggler's Copter".to_string(),
            card_type: crate::types::card_type::CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Vehicle".to_string()],
            },
            ..CardFace::default()
        };
        assert!(!is_commander_eligible(&vehicle));
    }

    /// `face.is_commander` precomputed by synthesis (MTGJSON `leadershipSkills.commander`)
    /// must short-circuit the type-line analysis — catches cards MTGJSON has
    /// blessed that our type-line check might not yet recognize.
    #[test]
    fn commander_eligibility_honors_precomputed_field() {
        let face = CardFace {
            name: "Future Commander With No Type-Line Match".to_string(),
            is_commander: true,
            // No legendary supertype, no creature/vehicle/spacecraft, no permission text:
            // type-line analysis would reject this, but the synthesized field overrides.
            ..CardFace::default()
        };
        assert!(is_commander_eligible(&face));
    }

    #[test]
    fn cross_group_pairings_rejected() {
        use crate::types::keywords::PartnerType;
        // Generic + FriendsForever = invalid
        let a = partner_face("A", vec![Keyword::Partner(PartnerType::Generic)], vec![]);
        let b = partner_face(
            "B",
            vec![Keyword::Partner(PartnerType::FriendsForever)],
            vec![],
        );
        assert!(!are_valid_partners(&a, &b));

        // Generic + CharacterSelect = invalid
        let c = partner_face(
            "C",
            vec![Keyword::Partner(PartnerType::CharacterSelect)],
            vec![],
        );
        assert!(!are_valid_partners(&a, &c));

        // FriendsForever + CharacterSelect = invalid
        assert!(!are_valid_partners(&b, &c));
    }

    #[test]
    fn amy_pond_multi_keyword_pairing() {
        // Amy Pond has Doctor's Companion AND Partner with Rory Williams
        use crate::types::keywords::PartnerType;
        let amy = partner_face(
            "Amy Pond",
            vec![
                Keyword::Partner(PartnerType::DoctorsCompanion),
                Keyword::Partner(PartnerType::With("Rory Williams".to_string())),
            ],
            vec![],
        );
        // Can pair with a Doctor
        let doctor = partner_face("The Thirteenth Doctor", vec![], vec!["Time Lord", "Doctor"]);
        assert!(are_valid_partners(&amy, &doctor));

        // Can pair with Rory Williams
        let rory = partner_face(
            "Rory Williams",
            vec![Keyword::Partner(PartnerType::With("Amy Pond".to_string()))],
            vec![],
        );
        assert!(are_valid_partners(&amy, &rory));

        // Cannot pair with a random generic partner
        let random = partner_face(
            "Random",
            vec![Keyword::Partner(PartnerType::Generic)],
            vec![],
        );
        assert!(!are_valid_partners(&amy, &random));
    }

    // CR 702.124: the public `can_pair_commanders` seam (consumed by the WASM
    // deck-builder bridge) must resolve both names through the database and apply
    // the asymmetric Doctor's Companion rule in either selection order.
    #[test]
    fn can_pair_commanders_resolves_doctor_pairing_through_db() {
        fn card_json(
            name: &str,
            subtypes: &[&str],
            keywords: serde_json::Value,
        ) -> serde_json::Value {
            serde_json::json!({
                "name": name,
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": subtypes },
                "power": "2", "toughness": "2",
                "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": keywords,
                "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null, "legalities": {}
            })
        }
        let db_json = serde_json::json!({
            "amy pond": card_json("Amy Pond", &["Human"], serde_json::json!([{ "Partner": { "type": "DoctorsCompanion" } }])),
            "the eleventh doctor": card_json("The Eleventh Doctor", &["Time Lord", "Doctor"], serde_json::json!([])),
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();

        assert!(can_pair_commanders(&db, "Amy Pond", "The Eleventh Doctor"));
        assert!(can_pair_commanders(&db, "The Eleventh Doctor", "Amy Pond"));
        // Unknown names resolve to no pairing rather than panicking.
        assert!(!can_pair_commanders(&db, "Amy Pond", "Nonexistent Card"));
    }

    #[test]
    fn doctors_companion_rejects_non_doctor_subtypes() {
        let companion = partner_face(
            "Amy Pond",
            vec![Keyword::Partner(PartnerType::DoctorsCompanion)],
            vec![],
        );
        let human_doctor = partner_face("Not A Real Doctor", vec![], vec!["Human", "Doctor"]);
        assert!(!are_valid_partners(&companion, &human_doctor));

        let non_creature_doctor = CardFace {
            name: "Noncreature Doctor".to_string(),
            is_commander: true,
            card_type: crate::types::card_type::CardType {
                supertypes: vec![Supertype::Legendary],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Time Lord".to_string(), "Doctor".to_string()],
            },
            ..CardFace::default()
        };
        assert!(!are_valid_partners(&companion, &non_creature_doctor));

        let unified = partner_face("The Eleventh Doctor", vec![], vec!["Time Lord Doctor"]);
        assert!(are_valid_partners(&companion, &unified));
    }

    #[test]
    fn no_partner_keywords_rejected() {
        let a = partner_face("A", vec![], vec![]);
        let b = partner_face("B", vec![], vec![]);
        assert!(!are_valid_partners(&a, &b));
    }

    // --- validate_deck_for_format tests ---

    #[test]
    fn validate_standard_rejects_non_standard_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = validate_deck_for_format(&db, &request);
        assert!(result.is_err());
        let reasons = result.unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("Not Standard legal")));
    }

    #[test]
    fn validate_name_deck_for_format_rejects_non_standard_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let main_deck = vec!["Not Standard".to_string(); 60];
        let result =
            validate_name_deck_for_format(&db, &main_deck, &[], &[], GameFormat::Standard, None);

        let reasons = result.expect_err("name-list validation must reject illegal AI decks");
        assert!(reasons.iter().any(|r| r.contains("Not Standard legal")));
    }

    #[test]
    fn validate_standard_accepts_legal_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: vec![],
            commander: vec![],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    #[test]
    fn validate_ffa_accepts_any_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::FreeForAll),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    #[test]
    fn oathbreaker_missing_commander_does_not_spam_color_identity_errors() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Red Card", 58),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: vec!["Big Spell".to_string()],
            selected_format: Some(GameFormat::Oathbreaker),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 1 Oathbreaker")));
        assert!(!result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("outside Oathbreaker color identity")));
        assert!(!result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("signature spell is outside")));
    }

    #[test]
    fn validate_no_format_accepts_any_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: None,
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    // --- Sideboard size + combined copy-limit tests (CR 100.2a, CR 100.4a, CR 201.3) ---

    #[test]
    fn constructed_sideboard_of_15_is_accepted() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: expand("Plains", 15),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "reasons: {:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn constructed_sideboard_of_16_is_rejected() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("Sideboard has 16") && r.contains("maximum 15")));
    }

    #[test]
    fn combined_copies_over_four_rejected() {
        // 3 copies of "Legal Standard" in main + 2 in sideboard = 5 combined.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 3);
        main.extend(expand("Plains", 57));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: expand("Legal Standard", 2),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("More than 4 copies")));
    }

    #[test]
    fn combined_copies_basic_lands_exempt() {
        // 60 Plains in main + 15 Plains in sideboard — exempt.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            sideboard: expand("Plains", 15),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn combined_copies_case_insensitive() {
        // Regression for B1: "Legal Standard" + "legal standard" must count as
        // the same card (CR 201.3 / CR 100.2a canonicalization).
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 3);
        main.extend(expand("legal standard", 2)); // lowercase
        main.extend(expand("Plains", 55));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("More than 4 copies")));
    }

    #[test]
    fn relentless_rats_allows_more_than_four() {
        // B2: cards whose Oracle text grants "A deck can have any number of
        // cards named X" are exempt from the 4-per-name rule.
        let db_json = serde_json::json!({
            "relentless rats": {
                "name": "Relentless Rats",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Rat"] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": "Relentless Rats gets +1/+1 for each other creature named Relentless Rats you control. A deck can have any number of cards named Relentless Rats.",
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "legal",
                    "commander": "legal",
                    "pioneer": "legal"
                }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Relentless Rats", 60),
            sideboard: expand("Relentless Rats", 15),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn commander_singleton_now_case_insensitive() {
        // Regression for B1 applied retroactively to commander: 2x "Legal
        // Standard" with different casing used to slip past the singleton
        // check because the HashMap was keyed by raw string.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 1);
        main.extend(expand("legal standard", 1));
        main.extend(expand("Plains", 97));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Singleton violations")));
    }

    #[test]
    fn commander_color_identity_uses_explicit_card_face_identity() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Plains", 98);
        main.push("Red Card".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Grub Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };

        let result = evaluate_deck_compatibility(&db, &request);

        assert!(
            result.commander.compatible,
            "{:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_singleton_exempts_deck_limit_override() {
        // A commander deck running 5x Relentless Rats passes the singleton
        // check because the card grants its own deck-limit override.
        let db_json = serde_json::json!({
            "relentless rats": {
                "name": "Relentless Rats",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": ["Rat"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": "A deck can have any number of cards named Relentless Rats.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Plains"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let mut main = expand("Relentless Rats", 5);
        main.extend(expand("Plains", 94));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible, got reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_accepts_ten_slime_against_humanity_copies() {
        // Issue #1138: "A deck can have any number of cards named Slime Against
        // Humanity" must override the Commander singleton default.
        let db_json = serde_json::json!({
            "slime against humanity": {
                "name": "Slime Against Humanity",
                "mana_cost": { "type": "Cost", "shards": ["Green"], "generic": 2 },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": "Create a 0/0 green Ooze creature token with trample. Put X +1/+1 counters on it, where X is two plus the total number of cards you own in exile and in your graveyard that are Oozes or are named Slime Against Humanity.\nA deck can have any number of cards named Slime Against Humanity.",
                "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": ["Green"], "scryfall_oracle_id": null,
                "deck_copy_limit": { "type": "Unlimited" },
                "legalities": { "commander": "legal" }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": ["Green"], "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "forest": {
                "name": "Forest",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Forest"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string();
        let db = CardDatabase::from_json_str(&db_json).unwrap();
        let mut main = expand("Slime Against Humanity", 10);
        main.extend(expand("Forest", 89));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible, got reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_sideboard_policy_accepts_maybeboard_entries() {
        // CR 903.5e: Phase's deck builder reuses the sideboard slot as a
        // builder-only "Maybeboard" for Commander-style formats. The
        // validator must accept extra entries (the engine drops them at game
        // load) and the deck must not be flagged as BO3-ready.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: vec!["Plains".to_string()],
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "reasons: {:?}",
            result.selected_format_reasons
        );
        assert!(result.commander.compatible);
        assert!(!result.bo3_ready, "commander decks are never BO3-ready");
    }

    #[test]
    fn validate_deck_for_format_rejects_oversize_sideboard() {
        // S8: registration gate must reject a 16-card sideboard for Standard.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let err = validate_deck_for_format(&db, &request)
            .expect_err("16-card sideboard must be rejected at registration");
        assert!(err.iter().any(|r| r.contains("Sideboard has 16")));
    }

    #[test]
    fn free_for_all_bo3_requires_sideboard_but_no_size_cap() {
        // S2: Unlimited policy formats allow BO3 with arbitrarily large
        // sideboards — only the non-empty requirement applies.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();

        let no_sideboard = DeckCompatibilityRequest {
            main_deck: expand("Plains", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::FreeForAll),
            selected_match_type: Some(MatchType::Bo3),
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &no_sideboard);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("BO3 requires a sideboard")));

        let huge_sideboard = DeckCompatibilityRequest {
            sideboard: expand("Plains", 30),
            ..no_sideboard
        };
        let result = evaluate_deck_compatibility(&db, &huge_sideboard);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn validate_commander_rejects_non_singleton() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Legal Standard".to_string(); 99],
            sideboard: vec![],
            commander: vec!["Test Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = validate_deck_for_format(&db, &request);
        assert!(result.is_err());
        let reasons = result.unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("Singleton violations")));
    }

    /// CR 100.2a + CR 205.3i: Basic-lands exemption from singleton is driven
    /// by the Basic *supertype*, not a fixed name allowlist. Snow-Covered
    /// Plains and Wastes both carry the Basic supertype; Llanowar Elves does
    /// not.
    fn basic_supertype_test_db() -> String {
        serde_json::json!({
            "snow-covered plains": {
                "name": "Snow-Covered Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic", "Snow"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "plains": {
                "name": "Plains",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": ["Plains"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "wastes": {
                "name": "Wastes",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"],
                    "core_types": ["Land"],
                    "subtypes": []
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "llanowar elves": {
                "name": "Llanowar Elves",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": [],
                    "core_types": ["Creature"],
                    "subtypes": ["Elf", "Druid"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            },
            "legal commander": {
                "name": "Legal Commander",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Legendary"],
                    "core_types": ["Creature"],
                    "subtypes": []
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "commander": "legal" }
            }
        })
        .to_string()
    }

    #[test]
    fn commander_singleton_permits_snow_covered_basic_duplicates() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = expand("Snow-Covered Plains", 10);
        main.extend(expand("Plains", 89));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "Snow-Covered Plains must be treated as basic; reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_singleton_permits_wastes_duplicates() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = expand("Wastes", 10);
        main.extend(expand("Plains", 89));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "Wastes (Basic supertype) must be treated as basic; reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_singleton_rejects_non_basic_duplicates() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = expand("Llanowar Elves", 2);
        main.extend(expand("Plains", 97));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            !result.commander.compatible,
            "duplicate non-basic must still fail singleton"
        );
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("Llanowar Elves")));
    }

    #[test]
    fn commander_singleton_permits_mixed_basic_variants() {
        let db = CardDatabase::from_json_str(&basic_supertype_test_db()).unwrap();
        let mut main = vec!["Plains".to_string(), "Snow-Covered Plains".to_string()];
        main.extend(expand("Wastes", 97));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: vec!["Legal Commander".to_string()],
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "1x Plains + 1x Snow-Covered Plains must pass; reasons: {:?}",
            result.commander.reasons
        );
    }

    fn vintage_test_db() -> String {
        // Power-9-shaped fixture: a "restricted" Vintage card and a generic
        // legal Vintage filler so we can build a 60-card deck.
        serde_json::json!({
            "black lotus": {
                "name": "Black Lotus",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Artifact"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "vintage": "restricted" }
            },
            "island": {
                "name": "Island",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": ["Basic"], "core_types": ["Land"], "subtypes": ["Island"]
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null,
                "legalities": { "vintage": "legal" }
            }
        })
        .to_string()
    }

    #[test]
    fn vintage_one_copy_of_restricted_card_is_legal() {
        // CR 100.2b: A restricted card is legal in Vintage at no more than
        // one copy. Regression for a bug where `is_legal()` rejected the
        // `Restricted` status, marking Power 9 as illegal in Vintage decks.
        let db = CardDatabase::from_json_str(&vintage_test_db()).unwrap();
        let mut main = vec!["Black Lotus".to_string()];
        main.extend(expand("Island", 59));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Vintage),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(
            result.selected_format_compatible,
            Some(true),
            "1x restricted card must be legal in Vintage; reasons: {:?}",
            result.selected_format_reasons
        );
    }

    #[test]
    fn vintage_two_copies_of_restricted_card_violate_one_copy_limit() {
        // CR 100.2b: Two copies of a restricted card violate the 1-copy
        // ceiling — the deck must be flagged, but the message is
        // "More than 1 copy of a restricted card", not "banned".
        let db = CardDatabase::from_json_str(&vintage_test_db()).unwrap();
        let mut main = expand("Black Lotus", 2);
        main.extend(expand("Island", 58));
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Vintage),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(
            result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("More than 1 copy of a restricted card")
                    && r.contains("Black Lotus")),
            "expected restricted-copy violation; reasons: {:?}",
            result.selected_format_reasons
        );
        assert!(
            !result
                .selected_format_reasons
                .iter()
                .any(|r| r.contains("Not Vintage legal")),
            "restricted card must not be flagged as illegal; reasons: {:?}",
            result.selected_format_reasons
        );
    }

    fn momir_request(main: Vec<String>) -> DeckCompatibilityRequest {
        DeckCompatibilityRequest {
            main_deck: main,
            sideboard: Vec::new(),
            commander: Vec::new(),
            planar_deck: Vec::new(),
            scheme_deck: Vec::new(),
            signature_spell: Vec::new(),
            selected_format: Some(GameFormat::Momir),
            selected_match_type: None,
            player_count: default_player_count(),
            summary_only: false,
        }
    }

    /// The fixed Momir's Madness deck: 12 copies of each of the five snow basic
    /// lands (no Snow-Covered Wastes), totaling 60. Delegates to the engine's
    /// canonical `momir_fixed_deck_names()` so the auto-supplied deck and this
    /// validator are exercised against the same single source of truth — if they
    /// ever drift, `momir_madness_snow_basics_pass` below catches it.
    fn momir_madness_deck() -> Vec<String> {
        crate::game::deck_loading::momir_fixed_deck_names()
    }

    #[test]
    fn momir_madness_snow_basics_pass() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = momir_request(momir_madness_deck());
        let check = evaluate_momir(&db, &request, &BTreeSet::new());
        assert!(
            check.compatible,
            "12x each of the five snow basics must be a legal Momir's Madness deck, reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_regular_basics_fail() {
        // 60 regular (non-snow) Plains must be rejected — Momir's Madness
        // requires snow basics, not ordinary basics.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let check = evaluate_momir(&db, &momir_request(expand("Plains", 60)), &BTreeSet::new());
        assert!(
            !check.compatible,
            "60 regular (non-snow) basics must be rejected"
        );
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("only contain the five snow basic lands")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_wrong_per_type_count_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // 11 Plains + 13 Island + 12 each of the other three = 60 total, but the
        // fixed 12-per-type ratio is broken.
        let mut deck = expand("Snow-Covered Plains", 11);
        deck.extend(expand("Snow-Covered Island", 13));
        deck.extend(expand("Snow-Covered Swamp", 12));
        deck.extend(expand("Snow-Covered Mountain", 12));
        deck.extend(expand("Snow-Covered Forest", 12));
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(!check.compatible, "an off-ratio deck must be rejected");
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("exactly 12") && r.contains("Plains")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_missing_type_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // 15 each of four types = 60 total, but Forest is entirely absent. This is
        // the "iterate over expected types, not present types" guard: a naive
        // per-present-type check would pass this (every present type is off-ratio
        // too, but the danger is a 12-each-of-four + 12-extra shape; here the rule
        // must reject because Forest's count resolves to 0 != 12.
        let mut deck = expand("Snow-Covered Plains", 15);
        deck.extend(expand("Snow-Covered Island", 15));
        deck.extend(expand("Snow-Covered Swamp", 15));
        deck.extend(expand("Snow-Covered Mountain", 15));
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(
            !check.compatible,
            "a deck missing one of the five snow basic types must be rejected"
        );
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("exactly 12") && r.contains("Forest")),
            "reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_count_off_total_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        for delta in [-1i32, 1] {
            let mut deck = momir_madness_deck();
            if delta > 0 {
                deck.push("Snow-Covered Plains".to_string());
            } else {
                deck.pop();
            }
            let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
            assert!(
                !check.compatible,
                "a deck off the 60-card total (delta {delta}) must be rejected"
            );
            assert!(check.reasons.iter().any(|r| r.contains("exactly 60")));
        }
    }

    #[test]
    fn momir_madness_snow_covered_wastes_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Swap one Snow-Covered Plains for a Snow-Covered Wastes: still snow +
        // basic + land, but "Wastes" is not a basic land type (CR 305.6).
        let mut deck = expand("Snow-Covered Wastes", 1);
        deck.extend(expand("Snow-Covered Plains", 11));
        deck.extend(expand("Snow-Covered Island", 12));
        deck.extend(expand("Snow-Covered Swamp", 12));
        deck.extend(expand("Snow-Covered Mountain", 12));
        deck.extend(expand("Snow-Covered Forest", 12));
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(!check.compatible, "Snow-Covered Wastes must be rejected");
        assert!(
            check
                .reasons
                .iter()
                .any(|r| r.contains("only contain the five snow basic lands")),
            "Snow-Covered Wastes must be flagged as a non-snow-basic-type card; reasons: {:?}",
            check.reasons
        );
    }

    #[test]
    fn momir_madness_non_basic_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut deck = expand("Snow-Covered Plains", 11);
        deck.extend(expand("Snow-Covered Island", 12));
        deck.extend(expand("Snow-Covered Swamp", 12));
        deck.extend(expand("Snow-Covered Mountain", 12));
        deck.extend(expand("Snow-Covered Forest", 12));
        deck.push("Legal Standard".to_string()); // non-basic, total 60
        assert_eq!(deck.len(), 60);
        let check = evaluate_momir(&db, &momir_request(deck), &BTreeSet::new());
        assert!(!check.compatible, "a non-basic card must be rejected");
        assert!(check
            .reasons
            .iter()
            .any(|r| r.contains("only contain the five snow basic lands")));
    }

    #[test]
    fn momir_madness_non_empty_sideboard_fails() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut request = momir_request(momir_madness_deck());
        request.sideboard = vec!["Snow-Covered Plains".to_string()];
        let check = evaluate_momir(&db, &request, &BTreeSet::new());
        assert!(!check.compatible, "Momir's Madness has no sideboard");
        assert!(check.reasons.iter().any(|r| r.contains("sideboard")));
    }
}

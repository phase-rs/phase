use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::database::legality::{LegalityFormat, LegalityStatus};
use crate::database::CardDatabase;
use crate::parser::oracle::oracle_text_allows_commander;
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::format::{GameFormat, SideboardPolicy};
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
    pub selected_format: Option<GameFormat>,
    #[serde(default)]
    pub selected_match_type: Option<MatchType>,
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
    let unknown_cards = collect_unknown_cards(db, request);
    let standard = evaluate_standard(db, request, &unknown_cards);
    let commander = evaluate_commander(db, request, &unknown_cards);
    let bo3_ready = !request.sideboard.is_empty();
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
    let bo3_ready = !request.sideboard.is_empty();
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

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
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
    )
}

/// Shared commander-variant validator. Commander, Duel Commander, and Pauper
/// Commander all use 100-card-singleton deck shape with a command zone; only
/// the legality table and display label differ. DuelCommander's 30-life /
/// 1v1-only rules are expressed in `FormatConfig`, not deck validation.
///
/// Known gap for Pauper Commander (PDH): the PDH community rule that the
/// commander must be an **uncommon** creature/planeswalker is not yet
/// structurally enforced — `is_commander_eligible` is rarity-agnostic, and
/// the card-pool check relies solely on `LegalityFormat::PauperCommander`
/// status for non-commander slots. Pool legality works; commander rarity
/// validation needs a future rarity-aware commander-eligibility predicate.
fn evaluate_commander_with_format(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
    legality_format: LegalityFormat,
    format_label: &str,
) -> CompatibilityCheck {
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

            if !is_commander_eligible(face) {
                ineligible_commanders.insert(name.clone());
            }
        }

        if !ineligible_commanders.is_empty() {
            reasons.push(summarize_cards(
                "Commander cards must be legendary creatures or explicitly allow being a commander",
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

    // CR 903.5e (+ variant rules): Commander-style formats do not use sideboards.
    if !request.sideboard.is_empty() {
        reasons.push(format!(
            "{format_label} decks should not include a sideboard"
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
    let mut identity_violations = BTreeSet::new();
    for name in &request.main_deck {
        if request
            .commander
            .iter()
            .any(|c| c.eq_ignore_ascii_case(name))
        {
            continue;
        }
        if unknown_cards.contains(name.as_str()) {
            continue;
        }
        if let Some(face) = db.get_face_by_name(resolve_card_name(db, name)) {
            let card_colors = card_color_identity(face);
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
fn is_brawl_commander_eligible(face: &CardFace) -> bool {
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

    // CR 903.5e (via Brawl variant): Brawl formats do not use sideboards.
    if matches!(
        GameFormat::Brawl.sideboard_policy(),
        SideboardPolicy::Forbidden
    ) && !request.sideboard.is_empty()
    {
        reasons.push(format!(
            "{format_label} decks should not include a sideboard"
        ));
    }

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
        GameFormat::FreeForAll | GameFormat::TwoHeadedGiant => true,
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
    unknown
}

/// CR 903.4: Compute color identity of a single card from mana cost + color indicator.
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
/// CR 201.3 + CR 903.5b: For deck construction, cards with interchangeable
/// names have the same name.
fn combined_copy_counts(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> HashMap<String, u32> {
    let mut counts: HashMap<String, u32> = HashMap::new();
    for name in all_deck_cards(request) {
        let canonical = resolve_card_name(db, name).to_ascii_lowercase();
        *counts.entry(canonical).or_insert(0) += 1;
    }
    counts
}

/// CR 100.2a: Flag card names whose combined count exceeds `max_copies`,
/// excluding basic lands and cards whose Oracle text grants a per-card deck-limit
/// override (e.g. Relentless Rats, Shadowborn Apostle, Rat Colony, Persistent
/// Petitioners — all printed with "A deck can have any number of cards named ...").
///
/// Seven Dwarves / Nazgûl have finite caps printed on the card (7 and 9
/// respectively) via "A deck can have up to <N> cards named ..."; their phrasing
/// does not match the "any number" override, so they currently fall through to
/// the default 4-per-name limit. That's a known gap — supporting arbitrary N-caps
/// requires parsing the printed number, which is out of scope for this pass.
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
        if *count <= max_copies {
            continue;
        }
        // CR 100.2a + CR 205.3i: Basic lands are exempt from copy limits.
        // "Basic" is a supertype (covering Plains/Island/Swamp/Mountain/Forest,
        // Snow-Covered variants, Wastes, and any future basic), not a fixed
        // name allowlist — trust the MTGJSON-populated supertype field.
        if db
            .get_face_by_name(canonical_name)
            .is_some_and(|face| face.card_type.supertypes.contains(&Supertype::Basic))
        {
            continue;
        }
        if has_deck_limit_override(db, canonical_name) {
            continue;
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

/// CR 100.2a exception: a card's Oracle text may read
/// "A deck can have any number of cards named <Name>." When present, the
/// 4-per-name constructed limit and the 1-per-name singleton limit do not
/// apply to that card. Class-level Oracle text detection — covers Relentless
/// Rats, Shadowborn Apostle, Rat Colony, Persistent Petitioners, and any
/// future card printed with the same phrasing.
fn has_deck_limit_override(db: &CardDatabase, canonical_name: &str) -> bool {
    db.get_face_by_name(canonical_name)
        .and_then(|face| face.oracle_text.as_deref())
        .is_some_and(|text| {
            text.to_ascii_lowercase()
                .contains("a deck can have any number of cards named")
        })
}

/// Resolves a card name to the key used in the database. For DFC names like "Front // Back",
/// returns the front face name if that's how it's indexed.
fn resolve_card_name<'a>(db: &CardDatabase, name: &'a str) -> &'a str {
    if db.get_face_by_name(name).is_some() {
        return name;
    }
    if let Some(front) = name.split(" // ").next() {
        if db.get_face_by_name(front).is_some() {
            return front;
        }
    }
    name
}

fn all_deck_cards(request: &DeckCompatibilityRequest) -> impl Iterator<Item = &str> {
    request
        .main_deck
        .iter()
        .chain(request.sideboard.iter())
        .chain(request.commander.iter())
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

/// CR 903.3: A card is eligible to be a commander if it is a legendary creature,
/// a legendary background enchantment, or has "can be your commander" in its rules text.
pub fn is_commander_eligible(face: &CardFace) -> bool {
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| oracle_text_allows_commander(text, &face.name));
    // CR 702.124: Background enchantments are eligible as commanders
    // (pairing validation is handled separately by are_valid_partners)
    let is_background = face
        .card_type
        .subtypes
        .iter()
        .any(|s| s.eq_ignore_ascii_case("Background"));

    (is_legendary && is_creature) || explicitly_allowed || (is_legendary && is_background)
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

/// CR 702.124: Check if a partner type matches the other face by subtype.
/// Doctor's Companion pairs with any Doctor; Choose a Background pairs with any Background.
fn subtype_partner_match(
    partner_type: &crate::types::keywords::PartnerType,
    other_face: &CardFace,
) -> bool {
    use crate::types::keywords::PartnerType;

    match partner_type {
        PartnerType::DoctorsCompanion => other_face
            .card_type
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Doctor")),
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

    #[test]
    fn standard_legal_deck_passes() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: Vec::new(),
            commander: Vec::new(),
            selected_format: None,
            selected_match_type: None,
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
            selected_format: None,
            selected_match_type: None,
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

    #[test]
    fn commander_rules_detect_size_singleton_and_legality_failures() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let mut main = expand("Legal Standard", 97);
        main.push("Commander Banned".to_string());
        main.push("Commander Banned".to_string());
        let request = DeckCompatibilityRequest {
            main_deck: main,
            sideboard: vec!["Legal Standard".to_string()],
            commander: vec!["Legal Standard".to_string()],
            selected_format: None,
            selected_match_type: None,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(!result.commander.compatible);
        assert!(result
            .commander
            .reasons
            .iter()
            .any(|r| r.contains("should not include a sideboard")));
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: Some(MatchType::Bo3),
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
            selected_format: None,
            selected_match_type: None,
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
            selected_format: None,
            selected_match_type: None,
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
    fn commander_partners_require_partner_keyword() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 98),
            sideboard: Vec::new(),
            commander: vec![
                "Partner Commander".to_string(),
                "Legal Commander".to_string(),
            ],
            selected_format: None,
            selected_match_type: None,
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
            selected_format: Some(GameFormat::FreeForAll),
            selected_match_type: None,
        };
        let thg_request = DeckCompatibilityRequest {
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: Some(MatchType::Bo1),
        };
        let commander_request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 99),
            sideboard: Vec::new(),
            commander: vec!["Legal Standard".to_string()],
            selected_format: Some(GameFormat::Commander),
            selected_match_type: Some(MatchType::Bo1),
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
            selected_format: Some(GameFormat::Pioneer),
            selected_match_type: None,
        };
        let result = evaluate_deck_compatibility(&db, &legal_request);
        assert_eq!(result.selected_format_compatible, Some(true));
    }

    #[test]
    fn pauper_selected_format_rejects_illegal_cards() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        // Pioneer Only card is not pauper-legal
        let illegal_request = DeckCompatibilityRequest {
            main_deck: expand("Pioneer Only", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            selected_format: Some(GameFormat::Pauper),
            selected_match_type: None,
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
            selected_format: None,
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Brawl),
            selected_match_type: None,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("exactly 60 cards")));
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
            selected_format: Some(GameFormat::HistoricBrawl),
            selected_match_type: None,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(true));

        // Same deck should fail Standard Brawl
        let brawl_request = DeckCompatibilityRequest {
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
        let doctor = partner_face("The Thirteenth Doctor", vec![], vec!["Doctor"]);
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
        };
        let result = validate_deck_for_format(&db, &request);
        assert!(result.is_err());
        let reasons = result.unwrap_err();
        assert!(reasons.iter().any(|r| r.contains("Not Standard legal")));
    }

    #[test]
    fn validate_standard_accepts_legal_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: legal_60_main("Legal Standard"),
            sideboard: vec![],
            commander: vec![],
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::FreeForAll),
            selected_match_type: None,
        };
        assert!(validate_deck_for_format(&db, &request).is_ok());
    }

    #[test]
    fn validate_no_format_accepts_any_deck() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: vec!["Not Standard".to_string(); 60],
            sideboard: vec![],
            commander: vec![],
            selected_format: None,
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "expected compatible, got reasons: {:?}",
            result.commander.reasons
        );
    }

    #[test]
    fn commander_sideboard_policy_forbidden_rejects_any_sideboard() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Plains", 99),
            sideboard: vec!["Plains".to_string()],
            commander: vec!["Legal Commander".to_string()],
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert_eq!(result.selected_format_compatible, Some(false));
        assert!(result
            .selected_format_reasons
            .iter()
            .any(|r| r.contains("should not include a sideboard")));
    }

    #[test]
    fn validate_deck_for_format_rejects_oversize_sideboard() {
        // S8: registration gate must reject a 16-card sideboard for Standard.
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: expand("Plains", 16),
            commander: Vec::new(),
            selected_format: Some(GameFormat::Standard),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::FreeForAll),
            selected_match_type: Some(MatchType::Bo3),
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
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
            selected_format: Some(GameFormat::Commander),
            selected_match_type: None,
        };
        let result = evaluate_deck_compatibility(&db, &request);
        assert!(
            result.commander.compatible,
            "1x Plains + 1x Snow-Covered Plains must pass; reasons: {:?}",
            result.commander.reasons
        );
    }
}

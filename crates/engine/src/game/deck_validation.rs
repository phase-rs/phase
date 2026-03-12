use std::collections::{BTreeSet, HashMap};

use serde::{Deserialize, Serialize};

use crate::database::legality::{LegalityFormat, LegalityStatus};
use crate::database::CardDatabase;
use crate::types::card::CardFace;
use crate::types::card_type::{CoreType, Supertype};
use crate::types::format::GameFormat;
use crate::types::keywords::Keyword;
use crate::types::match_config::MatchType;

const BASIC_LANDS: [&str; 5] = ["Plains", "Island", "Swamp", "Mountain", "Forest"];

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
}

pub fn evaluate_deck_compatibility(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> DeckCompatibilityResult {
    let unknown_cards = collect_unknown_cards(db, request);
    let standard = evaluate_standard(db, request, &unknown_cards);
    let commander = evaluate_commander(db, request, &unknown_cards);
    let bo3_ready = !request.sideboard.is_empty();

    let (selected_format_compatible, selected_format_reasons) =
        evaluate_selected_format(request, &standard, &commander, bo3_ready);

    DeckCompatibilityResult {
        standard,
        commander,
        bo3_ready,
        unknown_cards: unknown_cards.into_iter().collect(),
        selected_format_compatible,
        selected_format_reasons,
    }
}

fn evaluate_standard(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
    unknown_cards: &BTreeSet<String>,
) -> CompatibilityCheck {
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if !request.commander.is_empty() {
        reasons.push("Standard decks do not use a commander slot".to_string());
    }

    if request.main_deck.len() < 60 {
        reasons.push(format!(
            "Main deck has {} cards (minimum 60)",
            request.main_deck.len()
        ));
    }

    let mut illegal_cards = BTreeSet::new();
    for name in all_deck_cards(request) {
        if unknown_cards.contains(name) {
            continue;
        }
        match db.legality_status(name, LegalityFormat::Standard) {
            Some(status) if status.is_legal() => {}
            Some(status) => {
                illegal_cards.insert(format!("{name} ({})", status_label(status)));
            }
            None => {
                illegal_cards.insert(format!("{name} (missing legality data)"));
            }
        }
    }

    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards("Not Standard legal", &illegal_cards, 6));
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
    let mut reasons = Vec::new();

    if !unknown_cards.is_empty() {
        reasons.push(summarize_cards("Unknown cards", unknown_cards, 6));
    }

    if request.commander.is_empty() || request.commander.len() > 2 {
        reasons.push(format!(
            "Commander decks require 1 or 2 commanders (found {})",
            request.commander.len()
        ));
    }

    if !request.commander.is_empty() && request.commander.len() <= 2 {
        let mut ineligible_commanders = BTreeSet::new();
        let mut partner_missing = BTreeSet::new();

        for name in &request.commander {
            let Some(face) = db.get_face_by_name(name) else {
                continue;
            };

            if !is_commander_eligible(face) {
                ineligible_commanders.insert(name.clone());
            }

            if request.commander.len() == 2 && !has_partner(face) {
                partner_missing.insert(name.clone());
            }
        }

        if !ineligible_commanders.is_empty() {
            reasons.push(summarize_cards(
                "Commander cards must be legendary creatures or explicitly allow being a commander",
                &ineligible_commanders,
                6,
            ));
        }

        if !partner_missing.is_empty() {
            reasons.push(summarize_cards(
                "Two commanders require Partner on both commanders",
                &partner_missing,
                6,
            ));
        }
    }

    if !request.sideboard.is_empty() {
        reasons.push("Commander decks should not include a sideboard".to_string());
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
            "Commander deck must have exactly 100 cards (found {total_cards})"
        ));
    }

    let mut combined_counts: HashMap<String, u32> = HashMap::new();
    for name in &request.main_deck {
        *combined_counts.entry(name.to_string()).or_insert(0) += 1;
    }
    for name in &request.commander {
        if !request
            .main_deck
            .iter()
            .any(|card| card.eq_ignore_ascii_case(name))
        {
            *combined_counts.entry(name.to_string()).or_insert(0) += 1;
        }
    }

    let mut singleton_violations = BTreeSet::new();
    for (name, count) in combined_counts {
        if count <= 1 {
            continue;
        }
        if BASIC_LANDS
            .iter()
            .any(|basic| basic.eq_ignore_ascii_case(&name))
        {
            continue;
        }
        singleton_violations.insert(format!("{name} ({count} copies)"));
    }
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
        match db.legality_status(name, LegalityFormat::Commander) {
            Some(status) if status.is_legal() => {}
            Some(status) => {
                illegal_cards.insert(format!("{name} ({})", status_label(status)));
            }
            None => {
                illegal_cards.insert(format!("{name} (missing legality data)"));
            }
        }
    }
    if !illegal_cards.is_empty() {
        reasons.push(summarize_cards("Not Commander legal", &illegal_cards, 6));
    }

    CompatibilityCheck {
        compatible: reasons.is_empty(),
        reasons,
    }
}

fn evaluate_selected_format(
    request: &DeckCompatibilityRequest,
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
        GameFormat::FreeForAll | GameFormat::TwoHeadedGiant => true,
    };

    if matches!(request.selected_match_type, Some(MatchType::Bo3)) && !bo3_ready {
        compatible = false;
        reasons.push("BO3 requires a sideboard".to_string());
    }

    (Some(compatible), reasons)
}

fn collect_unknown_cards(
    db: &CardDatabase,
    request: &DeckCompatibilityRequest,
) -> BTreeSet<String> {
    let mut unknown = BTreeSet::new();
    for name in all_deck_cards(request) {
        if db.get_face_by_name(name).is_none() {
            unknown.insert(name.to_string());
        }
    }
    unknown
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

fn is_commander_eligible(face: &CardFace) -> bool {
    let is_legendary = face.card_type.supertypes.contains(&Supertype::Legendary);
    let is_creature = face.card_type.core_types.contains(&CoreType::Creature);
    let explicitly_allowed = face
        .oracle_text
        .as_ref()
        .is_some_and(|text| text.to_ascii_lowercase().contains("can be your commander"));

    (is_legendary && is_creature) || explicitly_allowed
}

fn has_partner(face: &CardFace) -> bool {
    face.keywords
        .iter()
        .any(|keyword| matches!(keyword, Keyword::Partner(_)))
        || face
            .oracle_text
            .as_ref()
            .is_some_and(|text| text.to_ascii_lowercase().contains("partner"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_db_json() -> String {
        serde_json::json!({
            "legal standard": {
                "name": "Legal Standard",
                "mana_cost": "NoCost",
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
                    "commander": "legal"
                }
            },
            "not standard": {
                "name": "Not Standard",
                "mana_cost": "NoCost",
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
                    "commander": "legal"
                }
            },
            "commander banned": {
                "name": "Commander Banned",
                "mana_cost": "NoCost",
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
                "mana_cost": "NoCost",
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
                    "commander": "legal"
                }
            },
            "partner commander": {
                "name": "Partner Commander",
                "mana_cost": "NoCost",
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
            }
        })
        .to_string()
    }

    fn expand(name: &str, count: usize) -> Vec<String> {
        (0..count).map(|_| name.to_string()).collect()
    }

    #[test]
    fn standard_legal_deck_passes() {
        let db = CardDatabase::from_json_str(&test_db_json()).unwrap();
        let request = DeckCompatibilityRequest {
            main_deck: expand("Legal Standard", 60),
            sideboard: Vec::new(),
            commander: Vec::new(),
            selected_format: None,
            selected_match_type: None,
        };

        let result = evaluate_deck_compatibility(&db, &request);
        assert!(result.standard.compatible);
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
            .any(|reason| reason.contains("require Partner")));
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
            main_deck: expand("Legal Standard", 60),
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
}

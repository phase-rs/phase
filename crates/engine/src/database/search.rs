//! Local card search over the loaded `CardDatabase`.
//!
//! This is the single authority for deck-builder card search. The engine owns
//! the rules data the search filters on — format legality (banned-list data),
//! set membership, card types (CR 205), mana value (CR 202.3), and a card's
//! colors (CR 105.2, derived from its mana symbols and color indicator) — so
//! search lives here rather than as a third-party HTTP call from the display
//! layer.
//!
//! Results carry only rules data (name, oracle id, mana value, color identity,
//! legalities). Presentation data (artwork, printed type line) is hydrated by
//! the frontend from its local Scryfall image map, keyed by the returned
//! `oracle_id`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use super::card_db::CardDatabase;
use super::legality::LegalityFormat;
use crate::types::card::CardFace;
use crate::types::card_type::CardType;
use crate::types::mana::{ManaColor, ManaCost};

/// Default page size, mirroring Scryfall's search page size so the grid renders
/// a comparable result count. `total` still reports the full match count.
const DEFAULT_LIMIT: usize = 175;

/// A card-search request from the deck builder. All fields are optional; an
/// all-empty query matches every card (each filter is skipped when empty), so
/// callers gate on "has criteria" before searching.
#[derive(Debug, Default, Deserialize)]
pub struct CardSearchQuery {
    /// Free text, matched word-by-word (AND) against name + oracle text + type.
    #[serde(default)]
    pub text: String,
    /// WUBRG color letters the card's colors must include (superset match,
    /// mirroring Scryfall's `c:` operator — CR 105.2).
    #[serde(default)]
    pub colors: Vec<String>,
    /// A type word (core type, supertype, or subtype), matched case-insensitively.
    #[serde(default)]
    pub type_line: String,
    /// Inclusive upper bound on mana value (CR 202.3).
    #[serde(default)]
    pub cmc_max: Option<u32>,
    /// Set codes; the card must have a printing in at least one. Format
    /// legality is a separate filter (`legal_format`).
    #[serde(default)]
    pub sets: Vec<String>,
    /// A legality-format key (e.g. `"modern"`); the card must be `legal` in it.
    #[serde(default)]
    pub legal_format: Option<String>,
    /// Max results returned (defaults to [`DEFAULT_LIMIT`]); `total` is unbounded.
    #[serde(default)]
    pub limit: Option<usize>,
}

/// One matching card. Rules data only — the frontend hydrates artwork and the
/// printed type line from its local image map using `oracle_id`.
#[derive(Debug, Serialize)]
pub struct CardSearchResult {
    pub name: String,
    pub oracle_id: Option<String>,
    pub mana_value: u32,
    pub color_identity: Vec<&'static str>,
    pub legalities: BTreeMap<String, String>,
}

/// A page of results plus the full match count (which may exceed `results.len()`
/// when the page limit truncates).
#[derive(Debug, Serialize)]
pub struct CardSearchResults {
    pub results: Vec<CardSearchResult>,
    pub total: usize,
}

impl CardDatabase {
    /// Filter the loaded cards by the query, deduplicating multi-face cards by
    /// oracle id. Name/text matches sort ahead of incidental oracle-text hits,
    /// then alphabetically.
    pub fn search(&self, query: &CardSearchQuery) -> CardSearchResults {
        let needle = query.text.trim().to_lowercase();
        // Drop the bare "//" combined-name separator: it is punctuation, never a
        // searchable term. Searching "Commit // Memory" tokenizes to
        // ["commit", "//", "memory"]; the "//" would otherwise fail every
        // face's haystack (split/DFC/Adventure faces are indexed individually).
        let words: Vec<&str> = needle.split_whitespace().filter(|w| *w != "//").collect();
        let requested_colors: Vec<ManaColor> = query
            .colors
            .iter()
            .filter_map(|c| parse_color_letter(c))
            .collect();
        let type_needle = query.type_line.trim().to_lowercase();
        let legal_format = query
            .legal_format
            .as_deref()
            .and_then(LegalityFormat::from_key);
        let requested_sets: Vec<String> = query.sets.iter().map(|s| s.to_uppercase()).collect();

        let mut seen_oracle: HashSet<&str> = HashSet::new();
        // (relevance rank, lowercased name for tiebreak, result)
        let mut matched: Vec<(u8, String, CardSearchResult)> = Vec::new();

        for (key, face) in self.face_index.iter() {
            let name_lower = face.name.to_lowercase();

            if !words.is_empty() && !self.text_matches(&name_lower, face, &words) {
                continue;
            }
            if !requested_colors.is_empty() {
                let colors = face_colors(face);
                if !requested_colors.iter().all(|c| colors.contains(c)) {
                    continue;
                }
            }
            if !type_needle.is_empty() && !type_matches(&face.card_type, &type_needle) {
                continue;
            }
            if let Some(max) = query.cmc_max {
                if face.mana_cost.mana_value() > max {
                    continue;
                }
            }
            if let Some(format) = legal_format {
                let legal = self
                    .legalities
                    .get(key)
                    .and_then(|m| m.get(&format))
                    .is_some_and(|status| status.is_legal());
                if !legal {
                    continue;
                }
            }
            if !requested_sets.is_empty() {
                let in_set = self
                    .printings_index
                    .get(key)
                    .is_some_and(|sets| sets.iter().any(|s| requested_sets.contains(s)));
                if !in_set {
                    continue;
                }
            }

            // Deduplicate multi-face cards: keep the first matching face per
            // oracle id. The frontend re-derives the combined display name from
            // the image map, so which face won here doesn't affect display.
            if let Some(oracle_id) = face.scryfall_oracle_id.as_deref() {
                if !seen_oracle.insert(oracle_id) {
                    continue;
                }
            }

            let rank = if needle.is_empty() || name_lower.contains(&needle) {
                0
            } else {
                1
            };
            matched.push((rank, name_lower, self.build_result(key, face)));
        }

        let total = matched.len();
        matched.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));

        let limit = query.limit.unwrap_or(DEFAULT_LIMIT);
        let results = matched
            .into_iter()
            .take(limit)
            .map(|(_, _, result)| result)
            .collect();

        CardSearchResults { results, total }
    }

    fn build_result(&self, key: &str, face: &CardFace) -> CardSearchResult {
        let legalities = self
            .legalities
            .get(key)
            .map(|m| {
                m.iter()
                    .map(|(format, status)| {
                        (
                            format.as_key().to_string(),
                            status.as_export_str().to_string(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default();

        CardSearchResult {
            name: face.name.clone(),
            oracle_id: face.scryfall_oracle_id.clone(),
            mana_value: face.mana_cost.mana_value(),
            color_identity: face
                .color_identity
                .iter()
                .copied()
                .map(color_letter)
                .collect(),
            legalities,
        }
    }

    /// Word-AND match across the card's name, its sibling faces' names, and its
    /// oracle text. Sibling names are folded in so a combined `A // B` query
    /// matches each individual face: split/Aftermath/Fuse cards plus DFC,
    /// Adventure, and MDFC are all indexed per-face, so without the sibling
    /// names no single face's haystack contains every word of its combined
    /// name. Siblings are enumerated layout-agnostically via
    /// `scryfall_oracle_id` → `oracle_id_index` → `face_index`.
    fn text_matches(&self, name_lower: &str, face: &CardFace, words: &[&str]) -> bool {
        let mut haystack = name_lower.to_string();
        if let Some(oid) = face.scryfall_oracle_id.as_deref() {
            if let Some(keys) = self.oracle_id_index.get(oid) {
                for key in keys {
                    if let Some(sibling) = self.face_index.get(key) {
                        if !sibling.name.eq_ignore_ascii_case(&face.name) {
                            haystack.push(' ');
                            haystack.push_str(&sibling.name.to_lowercase());
                        }
                    }
                }
            }
        }
        if let Some(text) = &face.oracle_text {
            haystack.push(' ');
            haystack.push_str(&text.to_lowercase());
        }
        words.iter().all(|w| haystack.contains(w))
    }
}

/// A card's colors per CR 105.2: the colors of its mana-cost symbols, plus any
/// color indicator (`color_override`). Hybrid symbols count for each color.
fn face_colors(face: &CardFace) -> Vec<ManaColor> {
    let mut colors: Vec<ManaColor> = Vec::new();
    if let ManaCost::Cost { shards, .. } = &face.mana_cost {
        for &color in &ManaColor::ALL {
            if shards.iter().any(|s| s.contributes_to(color)) {
                colors.push(color);
            }
        }
    }
    if let Some(indicator) = &face.color_override {
        for &color in indicator {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }
    }
    colors
}

/// Case-insensitive type match against core types, supertypes, and subtypes.
fn type_matches(card_type: &CardType, needle: &str) -> bool {
    card_type
        .core_types
        .iter()
        .any(|t| t.to_string().to_lowercase().contains(needle))
        || card_type
            .supertypes
            .iter()
            .any(|t| t.to_string().to_lowercase().contains(needle))
        || card_type
            .subtypes
            .iter()
            .any(|s| s.to_lowercase().contains(needle))
}

fn parse_color_letter(letter: &str) -> Option<ManaColor> {
    match letter.trim().to_uppercase().as_str() {
        "W" => Some(ManaColor::White),
        "U" => Some(ManaColor::Blue),
        "B" => Some(ManaColor::Black),
        "R" => Some(ManaColor::Red),
        "G" => Some(ManaColor::Green),
        _ => None,
    }
}

fn color_letter(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};

    /// Build one export entry. `shards`/`generic` form the mana cost (shard and
    /// `color_identity` names use the engine's full color spelling, e.g.
    /// `"Red"`, matching `card-data.json`), and `legalities`/`printings` drive
    /// the legality and set filters.
    #[allow(clippy::too_many_arguments)]
    fn card(
        name: &str,
        oracle_id: &str,
        shards: &[&str],
        generic: u32,
        core_type: &str,
        color_identity: &[&str],
        oracle_text: &str,
        legalities: Value,
        printings: &[&str],
    ) -> Value {
        json!({
            "name": name,
            "mana_cost": { "type": "Cost", "shards": shards, "generic": generic },
            "card_type": { "supertypes": [], "core_types": [core_type], "subtypes": [] },
            "power": null, "toughness": null, "loyalty": null, "defense": null,
            "oracle_text": oracle_text,
            "non_ability_text": null, "flavor_name": null,
            "keywords": [], "abilities": [], "triggers": [],
            "static_abilities": [], "replacements": [],
            "color_override": null,
            "color_identity": color_identity,
            "scryfall_oracle_id": oracle_id,
            "legalities": legalities,
            "printings": printings,
        })
    }

    fn db_from(cards: &[(&str, Value)]) -> CardDatabase {
        let map: serde_json::Map<String, Value> = cards
            .iter()
            .map(|(key, value)| (key.to_string(), value.clone()))
            .collect();
        CardDatabase::from_json_str(&Value::Object(map).to_string()).unwrap()
    }

    fn result_names(results: &CardSearchResults) -> Vec<String> {
        results.results.iter().map(|r| r.name.clone()).collect()
    }

    fn sample_db() -> CardDatabase {
        db_from(&[
            (
                "lightning bolt",
                card(
                    "Lightning Bolt",
                    "o-bolt",
                    &["Red"],
                    0,
                    "Instant",
                    &["Red"],
                    "Lightning Bolt deals 3 damage to any target.",
                    json!({ "modern": "legal" }),
                    &["LEA", "M10"],
                ),
            ),
            (
                "grizzly bears",
                card(
                    "Grizzly Bears",
                    "o-bears",
                    &["Green"],
                    1,
                    "Creature",
                    &["Green"],
                    "",
                    json!({ "modern": "legal" }),
                    &["LEA"],
                ),
            ),
            (
                "shock",
                card(
                    "Shock",
                    "o-shock",
                    &["Red"],
                    0,
                    "Instant",
                    &["Red"],
                    "Shock deals 2 damage to any target.",
                    json!({ "modern": "banned" }),
                    &["M10"],
                ),
            ),
        ])
    }

    #[test]
    fn text_matches_name_only() {
        let res = sample_db().search(&CardSearchQuery {
            text: "bolt".into(),
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Lightning Bolt"]);
    }

    #[test]
    fn text_matches_oracle_text_and_ranks_name_hits_first() {
        // "damage" hits the oracle text of both Bolt and Shock (neither name),
        // so both return, ordered alphabetically.
        let res = sample_db().search(&CardSearchQuery {
            text: "damage".into(),
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Lightning Bolt", "Shock"]);
    }

    #[test]
    fn color_filter_is_superset_match() {
        let db = db_from(&[
            (
                "azorius",
                card(
                    "Azorius Card",
                    "o-az",
                    &["White", "Blue"],
                    0,
                    "Creature",
                    &["White", "Blue"],
                    "",
                    json!({}),
                    &[],
                ),
            ),
            (
                "white",
                card(
                    "White Card",
                    "o-w",
                    &["White"],
                    0,
                    "Creature",
                    &["White"],
                    "",
                    json!({}),
                    &[],
                ),
            ),
        ]);
        // {W} matches both the mono-white card and the WU card (superset).
        let res = db.search(&CardSearchQuery {
            colors: vec!["W".into()],
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Azorius Card", "White Card"]);
        // {W}{U} matches only the WU card; mono-white lacks blue.
        let res = db.search(&CardSearchQuery {
            colors: vec!["W".into(), "U".into()],
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Azorius Card"]);
    }

    #[test]
    fn type_filter_matches_core_type() {
        let res = sample_db().search(&CardSearchQuery {
            type_line: "creature".into(),
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Grizzly Bears"]);
    }

    #[test]
    fn cmc_max_is_inclusive_upper_bound() {
        // Bolt/Shock are mana value 1; Grizzly Bears is {1}{G} = 2.
        let res = sample_db().search(&CardSearchQuery {
            cmc_max: Some(1),
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Lightning Bolt", "Shock"]);
    }

    #[test]
    fn legal_format_excludes_banned_and_unknown() {
        // Shock is banned in modern; Bolt and Bears are legal.
        let res = sample_db().search(&CardSearchQuery {
            legal_format: Some("modern".into()),
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Grizzly Bears", "Lightning Bolt"]);
    }

    #[test]
    fn sets_filter_requires_a_printing_in_set() {
        let res = sample_db().search(&CardSearchQuery {
            sets: vec!["m10".into()], // case-insensitive
            ..Default::default()
        });
        assert_eq!(result_names(&res), vec!["Lightning Bolt", "Shock"]);
    }

    #[test]
    fn multi_face_cards_dedupe_by_oracle_id() {
        let db = db_from(&[
            (
                "front face",
                card(
                    "Front Face",
                    "o-dfc",
                    &["Blue"],
                    1,
                    "Creature",
                    &["Blue"],
                    "",
                    json!({}),
                    &[],
                ),
            ),
            (
                "back face",
                card(
                    "Back Face",
                    "o-dfc",
                    &[],
                    0,
                    "Land",
                    &[],
                    "",
                    json!({}),
                    &[],
                ),
            ),
        ]);
        let res = db.search(&CardSearchQuery {
            text: "face".into(),
            ..Default::default()
        });
        assert_eq!(res.results.len(), 1, "both faces share an oracle id");
        assert_eq!(res.total, 1);
    }

    #[test]
    fn limit_truncates_results_but_total_is_full_count() {
        let res = sample_db().search(&CardSearchQuery {
            limit: Some(1),
            ..Default::default()
        });
        assert_eq!(res.results.len(), 1);
        assert_eq!(res.total, 3, "total reflects all matches, not the page");
    }

    #[test]
    fn result_carries_engine_authoritative_fields() {
        let res = sample_db().search(&CardSearchQuery {
            text: "lightning bolt".into(),
            ..Default::default()
        });
        let bolt = &res.results[0];
        assert_eq!(bolt.oracle_id.as_deref(), Some("o-bolt"));
        assert_eq!(bolt.mana_value, 1);
        assert_eq!(bolt.color_identity, vec!["R"]);
        assert_eq!(
            bolt.legalities.get("modern").map(String::as_str),
            Some("legal")
        );
    }

    /// A two-faced card (split/Aftermath/DFC) is indexed per-face, so searching
    /// its combined `A // B` name must still match (and dedupe to one result)
    /// via the sibling-name fold. Single-face searches still work, and the fold
    /// does not produce cross-card false positives.
    fn split_db() -> CardDatabase {
        db_from(&[
            (
                "commit",
                card(
                    "Commit",
                    "o-commit-memory",
                    &["Blue"],
                    3,
                    "Instant",
                    &["Blue"],
                    "Put target spell or permanent into its owner's library second from the top.",
                    json!({}),
                    &["AKH"],
                ),
            ),
            (
                "memory",
                card(
                    "Memory",
                    "o-commit-memory",
                    &["Blue"],
                    4,
                    "Sorcery",
                    &["Blue"],
                    "Each player shuffles their hand and graveyard into their library, then draws seven cards.",
                    json!({}),
                    &["AKH"],
                ),
            ),
            (
                "lightning bolt",
                card(
                    "Lightning Bolt",
                    "o-bolt",
                    &["Red"],
                    0,
                    "Instant",
                    &["Red"],
                    "Lightning Bolt deals 3 damage to any target.",
                    json!({}),
                    &["LEA"],
                ),
            ),
        ])
    }

    #[test]
    fn split_card_matches_combined_name_and_dedupes() {
        let res = split_db().search(&CardSearchQuery {
            text: "Commit // Memory".into(),
            ..Default::default()
        });
        assert_eq!(
            res.results.len(),
            1,
            "combined name matches via sibling fold and dedupes by oracle id"
        );
    }

    #[test]
    fn split_card_single_face_search_still_matches() {
        for face in ["commit", "memory"] {
            let res = split_db().search(&CardSearchQuery {
                text: face.into(),
                ..Default::default()
            });
            assert_eq!(res.results.len(), 1, "single-face search '{face}' matches");
        }
    }

    #[test]
    fn sibling_fold_does_not_cross_match_unrelated_cards() {
        // "commit" (a split face) + "lightning" (a different card) share no
        // single card, so the sibling fold must not produce a false positive.
        let res = split_db().search(&CardSearchQuery {
            text: "commit lightning".into(),
            ..Default::default()
        });
        assert!(
            res.results.is_empty(),
            "sibling fold only joins same-oracle faces, not arbitrary cards"
        );
    }
}

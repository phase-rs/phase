use std::collections::HashMap;
use std::error::Error;
use std::path::Path;

use serde::Deserialize;

use crate::types::mana::{ManaCost, ManaCostShard};

/// Root structure of an MTGJSON AtomicCards.json file.
#[derive(Deserialize)]
pub struct AtomicCardsFile {
    pub data: HashMap<String, Vec<AtomicCard>>,
}

/// A single card face from MTGJSON's atomic card data.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AtomicCard {
    pub name: String,
    #[serde(default)]
    pub mana_cost: Option<String>,
    pub colors: Vec<String>,
    pub color_identity: Vec<String>,
    #[serde(default)]
    pub power: Option<String>,
    #[serde(default)]
    pub toughness: Option<String>,
    #[serde(default)]
    pub loyalty: Option<String>,
    #[serde(default)]
    pub defense: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    pub layout: String,
    #[serde(rename = "type")]
    pub type_line: Option<String>,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub subtypes: Vec<String>,
    #[serde(default)]
    pub supertypes: Vec<String>,
    #[serde(default)]
    pub keywords: Option<Vec<String>>,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub face_name: Option<String>,
    pub mana_value: f64,
    #[serde(default)]
    pub legalities: HashMap<String, String>,
    pub identifiers: AtomicIdentifiers,
}

/// Card identifiers from MTGJSON.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AtomicIdentifiers {
    #[serde(default)]
    pub scryfall_oracle_id: Option<String>,
}

/// Load and deserialize an AtomicCards.json file.
pub fn load_atomic_cards(path: &Path) -> Result<AtomicCardsFile, Box<dyn Error>> {
    let contents = std::fs::read_to_string(path)?;
    let file: AtomicCardsFile = serde_json::from_str(&contents)?;
    Ok(file)
}

/// Look up a card by name, returning the first face (index 0).
pub fn find_card<'a>(data: &'a AtomicCardsFile, name: &str) -> Option<&'a AtomicCard> {
    data.data.get(name).and_then(|faces| faces.first())
}

/// Parse an MTGJSON mana cost string (e.g. "{2}{W}{U}") into the engine's ManaCost type.
pub fn parse_mtgjson_mana_cost(s: &str) -> ManaCost {
    let s = s.trim();
    if s.is_empty() {
        return ManaCost::NoCost;
    }

    let mut generic: u32 = 0;
    let mut shards = Vec::new();

    // Extract symbols between braces
    for segment in s.split('{').filter(|seg| !seg.is_empty()) {
        let symbol = segment.trim_end_matches('}');
        let symbol = symbol.to_ascii_uppercase();
        match symbol.as_str() {
            "W" => shards.push(ManaCostShard::White),
            "U" => shards.push(ManaCostShard::Blue),
            "B" => shards.push(ManaCostShard::Black),
            "R" => shards.push(ManaCostShard::Red),
            "G" => shards.push(ManaCostShard::Green),
            "C" => shards.push(ManaCostShard::Colorless),
            "S" => shards.push(ManaCostShard::Snow),
            "X" => shards.push(ManaCostShard::X),
            // Hybrid and phyrexian symbols
            "W/U" => shards.push(ManaCostShard::WhiteBlue),
            "W/B" => shards.push(ManaCostShard::WhiteBlack),
            "U/B" => shards.push(ManaCostShard::BlueBlack),
            "U/R" => shards.push(ManaCostShard::BlueRed),
            "B/R" => shards.push(ManaCostShard::BlackRed),
            "B/G" => shards.push(ManaCostShard::BlackGreen),
            "R/W" => shards.push(ManaCostShard::RedWhite),
            "R/G" => shards.push(ManaCostShard::RedGreen),
            "G/W" => shards.push(ManaCostShard::GreenWhite),
            "G/U" => shards.push(ManaCostShard::GreenBlue),
            "2/W" => shards.push(ManaCostShard::TwoWhite),
            "2/U" => shards.push(ManaCostShard::TwoBlue),
            "2/B" => shards.push(ManaCostShard::TwoBlack),
            "2/R" => shards.push(ManaCostShard::TwoRed),
            "2/G" => shards.push(ManaCostShard::TwoGreen),
            "W/P" => shards.push(ManaCostShard::PhyrexianWhite),
            "U/P" => shards.push(ManaCostShard::PhyrexianBlue),
            "B/P" => shards.push(ManaCostShard::PhyrexianBlack),
            "R/P" => shards.push(ManaCostShard::PhyrexianRed),
            "G/P" => shards.push(ManaCostShard::PhyrexianGreen),
            other => {
                // Try to parse as a number (generic mana)
                if let Ok(n) = other.parse::<u32>() {
                    generic += n;
                }
                // Ignore unrecognized symbols
            }
        }
    }

    ManaCost::Cost { shards, generic }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TEST_FIXTURE: &str = include_str!("../../../../data/mtgjson/test_fixture.json");

    fn load_fixture() -> AtomicCardsFile {
        serde_json::from_str(TEST_FIXTURE).expect("Test fixture should deserialize")
    }

    #[test]
    fn deserializes_test_fixture() {
        let data = load_fixture();
        assert!(
            data.data.len() >= 5,
            "Fixture should contain at least 5 cards"
        );
    }

    #[test]
    fn find_lightning_bolt() {
        let data = load_fixture();
        let card = find_card(&data, "Lightning Bolt").expect("Lightning Bolt should exist");
        assert_eq!(card.name, "Lightning Bolt");
        assert_eq!(card.mana_cost.as_deref(), Some("{R}"));
        assert_eq!(card.types, vec!["Instant"]);
        assert_eq!(card.colors, vec!["R"]);
        assert!(card.text.as_ref().unwrap().contains("3 damage"));
        assert!(card.identifiers.scryfall_oracle_id.is_some());
    }

    #[test]
    fn find_creature_with_power_toughness() {
        let data = load_fixture();
        let card = find_card(&data, "Grizzly Bears").expect("Grizzly Bears should exist");
        assert_eq!(card.power.as_deref(), Some("2"));
        assert_eq!(card.toughness.as_deref(), Some("2"));
        assert_eq!(card.types, vec!["Creature"]);
        assert_eq!(card.subtypes, vec!["Bear"]);
    }

    #[test]
    fn find_unknown_card_returns_none() {
        let data = load_fixture();
        assert!(find_card(&data, "Nonexistent Card Name").is_none());
    }

    #[test]
    fn multi_face_card_has_both_faces() {
        let data = load_fixture();
        let faces = data
            .data
            .get("Delver of Secrets // Insectile Aberration")
            .expect("Delver should exist");
        assert_eq!(faces.len(), 2);
        assert_eq!(faces[0].side.as_deref(), Some("a"));
        assert_eq!(faces[0].face_name.as_deref(), Some("Delver of Secrets"));
        assert_eq!(faces[1].side.as_deref(), Some("b"));
        assert_eq!(faces[1].face_name.as_deref(), Some("Insectile Aberration"));
    }

    #[test]
    fn parse_mana_cost_single_red() {
        assert_eq!(
            parse_mtgjson_mana_cost("{R}"),
            ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Red],
            }
        );
    }

    #[test]
    fn parse_mana_cost_generic_and_colored() {
        assert_eq!(
            parse_mtgjson_mana_cost("{2}{W}{U}"),
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White, ManaCostShard::Blue],
            }
        );
    }

    #[test]
    fn parse_mana_cost_empty_is_no_cost() {
        assert_eq!(parse_mtgjson_mana_cost(""), ManaCost::NoCost);
    }

    #[test]
    fn parse_mana_cost_zero_generic() {
        assert_eq!(
            parse_mtgjson_mana_cost("{0}"),
            ManaCost::Cost {
                generic: 0,
                shards: vec![],
            }
        );
    }

    #[test]
    fn parse_mana_cost_multicolor() {
        assert_eq!(
            parse_mtgjson_mana_cost("{5}{W}{U}{B}"),
            ManaCost::Cost {
                generic: 5,
                shards: vec![
                    ManaCostShard::White,
                    ManaCostShard::Blue,
                    ManaCostShard::Black,
                ],
            }
        );
    }

    #[test]
    fn parse_mana_cost_x_spell() {
        assert_eq!(
            parse_mtgjson_mana_cost("{X}{R}"),
            ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
            }
        );
    }

    #[test]
    fn parse_mana_cost_hybrid() {
        assert_eq!(
            parse_mtgjson_mana_cost("{W/U}"),
            ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::WhiteBlue],
            }
        );
    }

    #[test]
    fn load_from_file_path() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/mtgjson/test_fixture.json");
        let data = load_atomic_cards(&path).expect("Should load test fixture from file");
        let card = find_card(&data, "Lightning Bolt").expect("Lightning Bolt should exist");
        assert_eq!(card.name, "Lightning Bolt");
    }
}

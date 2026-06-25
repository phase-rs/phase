use std::collections::HashMap;
use std::error::Error;
use std::path::Path;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::types::mana::{ManaCost, ManaCostShard};

/// Root structure of an MTGJSON AtomicCards.json file.
#[derive(Deserialize)]
pub struct AtomicCardsFile {
    pub data: HashMap<String, Vec<AtomicCard>>,
}

/// Root structure of an MTGJSON CardTypes.json file (CR 205.3 canonical lists).
#[derive(Deserialize, Debug, Clone)]
pub struct CardTypesFile {
    pub data: CardTypesData,
}

/// CR 205.3: per-category subtype and supertype lists from MTGJSON CardTypes.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CardTypesData {
    pub creature: CardTypeEntry,
}

/// Subtype/supertype entry for one card category in CardTypes.json.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct CardTypeEntry {
    pub sub_types: Vec<String>,
    #[serde(default)]
    pub super_types: Vec<String>,
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
    #[serde(default)]
    pub leadership_skills: Option<LeadershipSkills>,
    #[serde(default)]
    pub printings: Vec<String>,
    #[serde(default)]
    pub rulings: Vec<Ruling>,
    #[serde(default)]
    pub is_game_changer: bool,
    pub identifiers: AtomicIdentifiers,
    /// Localized printings of this card from MTGJSON. Only display fields
    /// (name/text/type) are captured — used to emit per-language card-data
    /// sidecars for content i18n. The engine itself stays English-only.
    #[serde(default)]
    pub foreign_data: Vec<ForeignData>,
}

/// A localized printing of a card from MTGJSON's `foreignData` array. `language`
/// is the full English language name (e.g. "German", "Portuguese (Brazil)").
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ForeignData {
    pub language: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(rename = "type", default)]
    pub type_line: Option<String>,
}

/// An official WotC ruling attached to a card. MTGJSON mirrors these from Gatherer.
/// Note: MTGJSON duplicates the same rulings across every face of a multi-face
/// card (DFC, adventure, split, etc.); dedup happens at export time in
/// `oracle_gen` by attaching rulings to the front face only.
#[derive(Deserialize, Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct Ruling {
    pub date: String,
    pub text: String,
}

/// Leadership skills from MTGJSON — indicates whether a card can serve as a
/// commander in various formats.
#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct LeadershipSkills {
    #[serde(default)]
    pub brawl: bool,
    #[serde(default)]
    pub commander: bool,
    #[serde(default)]
    pub oathbreaker: bool,
}

/// Card identifiers from MTGJSON.
#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AtomicIdentifiers {
    #[serde(default)]
    pub scryfall_id: Option<String>,
    #[serde(default)]
    pub scryfall_oracle_id: Option<String>,
}

#[derive(Deserialize, Debug, Clone)]
pub struct SetFile {
    pub data: SetData,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SetData {
    pub code: String,
    pub name: String,
    #[serde(default)]
    pub release_date: Option<String>,
    #[serde(default)]
    pub cards: Vec<SetCard>,
    #[serde(default)]
    pub tokens: Vec<SetToken>,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SetCard {
    pub uuid: String,
    pub name: String,
    #[serde(default)]
    pub face_name: Option<String>,
    #[serde(default)]
    pub rarity: String,
    #[serde(default)]
    pub identifiers: SetIdentifiers,
    #[serde(default)]
    pub related_cards: SetRelatedCards,
}

#[derive(Deserialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SetToken {
    pub uuid: String,
    pub name: String,
    pub layout: String,
    #[serde(default)]
    pub side: Option<String>,
    #[serde(default)]
    pub face_name: Option<String>,
    #[serde(default)]
    pub number: Option<String>,
    #[serde(rename = "type")]
    pub type_line: String,
    #[serde(default)]
    pub types: Vec<String>,
    #[serde(default)]
    pub subtypes: Vec<String>,
    #[serde(default)]
    pub supertypes: Vec<String>,
    #[serde(default)]
    pub text: Option<String>,
    #[serde(default)]
    pub power: Option<String>,
    #[serde(default)]
    pub toughness: Option<String>,
    #[serde(default)]
    pub colors: Vec<String>,
    #[serde(default)]
    pub keywords: Vec<String>,
    #[serde(default)]
    pub identifiers: SetIdentifiers,
    #[serde(default)]
    pub related_cards: SetRelatedCards,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetIdentifiers {
    #[serde(default)]
    pub scryfall_id: Option<String>,
    #[serde(default)]
    pub scryfall_oracle_id: Option<String>,
}

#[derive(Deserialize, Debug, Clone, Default)]
#[serde(rename_all = "camelCase")]
pub struct SetRelatedCards {
    #[serde(default)]
    pub tokens: Vec<String>,
    #[serde(default)]
    pub reverse_related: Vec<String>,
    /// Alchemy spellbook — the fixed list of card names this card can draft from.
    #[serde(default)]
    pub spellbook: Vec<String>,
}

/// Load and deserialize an AtomicCards.json file.
pub fn load_atomic_cards(path: &Path) -> Result<AtomicCardsFile, Box<dyn Error>> {
    let contents = std::fs::read_to_string(path)?;
    let file: AtomicCardsFile = serde_json::from_str(&contents)?;
    Ok(file)
}

/// Load and deserialize a CardTypes.json file.
pub fn load_card_types(path: &Path) -> Result<CardTypesFile, Box<dyn Error>> {
    let contents = std::fs::read_to_string(path)?;
    let file: CardTypesFile = serde_json::from_str(&contents)?;
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

    // CR 107.4: `ManaCostShard::from_str` is the single authority for the
    // symbol→shard mapping. Routing every symbol through it keeps the loader
    // from drifting out of sync with the rest of the engine — previously this
    // function kept its own hand-maintained list that recognized mono-Phyrexian
    // (`{W/P}`) but not Phyrexian-hybrid (`{G/U/P}`) symbols, silently dropping
    // the pip from compleated planeswalkers and undercounting their cost by 1
    // (issue #1416, same class as #493's `{C/W}` drop). Anything `from_str`
    // doesn't recognize is either a bare number (generic mana) or ignorable.
    for segment in s.split('{').filter(|seg| !seg.is_empty()) {
        let symbol = segment.trim_end_matches('}');
        let symbol = symbol.to_ascii_uppercase();
        match ManaCostShard::from_str(&symbol) {
            Ok(shard) => shards.push(shard),
            Err(_) => {
                // A bare number is generic mana; any other unknown symbol is ignored.
                if let Ok(n) = symbol.parse::<u32>() {
                    generic += n;
                }
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
    fn rulings_deserialize_from_fixture() {
        let data = load_fixture();
        let card = find_card(&data, "Augur of Bolas").expect("Augur of Bolas should exist");
        assert!(
            !card.rulings.is_empty(),
            "Augur of Bolas has published rulings in the fixture"
        );
        let first = &card.rulings[0];
        assert!(!first.date.is_empty(), "ruling date should be populated");
        assert!(!first.text.is_empty(), "ruling text should be populated");
    }

    #[test]
    fn deserializes_is_game_changer() {
        let data: AtomicCardsFile = serde_json::from_str(
            r#"{
                "data": {
                    "Sol Ring": [{
                        "name": "Sol Ring",
                        "colors": [],
                        "colorIdentity": [],
                        "layout": "normal",
                        "manaValue": 1.0,
                        "isGameChanger": true,
                        "identifiers": {}
                    }]
                }
            }"#,
        )
        .expect("inline fixture should deserialize");

        let card = find_card(&data, "Sol Ring").expect("Sol Ring should exist");
        assert!(card.is_game_changer);
    }

    #[test]
    fn rulings_duplicated_across_multi_face_cards() {
        // This test proves the premise behind our export-time dedup: MTGJSON
        // duplicates rulings on every face of a multi-face card. We rely on
        // this invariant when we attach rulings to the front face only.
        let data = load_fixture();
        let faces = data
            .data
            .get("Lovestruck Beast // Heart's Desire")
            .expect("Lovestruck Beast should exist");
        assert_eq!(faces.len(), 2);
        assert!(!faces[0].rulings.is_empty());
        assert_eq!(
            faces[0].rulings, faces[1].rulings,
            "MTGJSON mirrors rulings across every face; export-time dedup relies on this"
        );
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

    /// CR 107.4e regression for #493 — Eldrazi colorless-hybrid `{C/X}`
    /// symbols (Ulalek Fused Atrocity, BFZ/OGW) must populate the cost
    /// shards rather than fall through to the silent "ignore unrecognized
    /// symbols" branch that left mana_cost empty and the AI casting for
    /// free.
    #[test]
    fn parse_mana_cost_colorless_hybrid_ulalek() {
        assert_eq!(
            parse_mtgjson_mana_cost("{C/W}{C/U}{C/B}{C/R}{C/G}"),
            ManaCost::Cost {
                generic: 0,
                shards: vec![
                    ManaCostShard::ColorlessWhite,
                    ManaCostShard::ColorlessBlue,
                    ManaCostShard::ColorlessBlack,
                    ManaCostShard::ColorlessRed,
                    ManaCostShard::ColorlessGreen,
                ],
            }
        );
    }

    /// CR 107.4f + CR 202.3g regression for #1416 — Phyrexian-hybrid
    /// `{C1/C2/P}` symbols on the compleated planeswalkers must populate the
    /// cost shards rather than fall through to the "ignore unrecognized
    /// symbols" branch. The dropped pip undercounted mana value by 1, letting
    /// the cards be cast a mana too cheaply. Each printed cost below carries
    /// exactly one Phyrexian-hybrid pip; the assertions pin both the shard
    /// list and the resulting mana value.
    #[test]
    fn parse_mana_cost_phyrexian_hybrid_compleated_walkers() {
        // Tamiyo, Compleated Sage — {2}{G}{G/U/P}{U} → MV 5.
        let tamiyo = parse_mtgjson_mana_cost("{2}{G}{G/U/P}{U}");
        assert_eq!(
            tamiyo,
            ManaCost::Cost {
                generic: 2,
                shards: vec![
                    ManaCostShard::Green,
                    ManaCostShard::PhyrexianGreenBlue,
                    ManaCostShard::Blue,
                ],
            }
        );
        assert_eq!(tamiyo.mana_value(), 5);

        // Ajani, Sleeper Agent — {1}{G}{G/W/P}{W} → MV 4.
        let ajani = parse_mtgjson_mana_cost("{1}{G}{G/W/P}{W}");
        assert_eq!(
            ajani,
            ManaCost::Cost {
                generic: 1,
                shards: vec![
                    ManaCostShard::Green,
                    ManaCostShard::PhyrexianGreenWhite,
                    ManaCostShard::White,
                ],
            }
        );
        assert_eq!(ajani.mana_value(), 4);

        // Lukka, Bound to Ruin — {2}{R}{R/G/P}{G} → MV 5.
        let lukka = parse_mtgjson_mana_cost("{2}{R}{R/G/P}{G}");
        assert_eq!(
            lukka,
            ManaCost::Cost {
                generic: 2,
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::PhyrexianRedGreen,
                    ManaCostShard::Green,
                ],
            }
        );
        assert_eq!(lukka.mana_value(), 5);

        // Nahiri, the Unforgiving — {1}{R}{R/W/P}{W} → MV 4.
        let nahiri = parse_mtgjson_mana_cost("{1}{R}{R/W/P}{W}");
        assert_eq!(
            nahiri,
            ManaCost::Cost {
                generic: 1,
                shards: vec![
                    ManaCostShard::Red,
                    ManaCostShard::PhyrexianRedWhite,
                    ManaCostShard::White,
                ],
            }
        );
        assert_eq!(nahiri.mana_value(), 4);
    }

    /// CR 107.4: The symbol→shard mapping lives in three places —
    /// `ManaCostShard::from_str` (the authority), the MTGJSON loader (this
    /// module), and the nom combinator `parse_mana_symbol`. They drifted once
    /// (issue #493 colorless-hybrid, issue #1416 Phyrexian-hybrid). This parity
    /// test pins every `from_str` symbol to parse identically through the loader
    /// and the nom path so the three can never silently diverge again.
    #[test]
    fn every_shard_symbol_round_trips_through_loader_and_nom() {
        use crate::parser::oracle_nom::primitives::parse_mana_symbol;

        // Every symbol string `from_str` accepts that denotes a single shard.
        // Bare-number generic mana is excluded — it is accumulated as `generic`,
        // not pushed as a shard, in both the loader and `parse_mana_cost`.
        const SYMBOLS: &[&str] = &[
            "W", "U", "B", "R", "G", "C", "S", "X", "Z", // basic + special
            "W/U", "W/B", "U/B", "U/R", "B/R", "B/G", "R/W", "R/G", "G/W", "G/U", // hybrid
            "2/W", "2/U", "2/B", "2/R", "2/G", // two-generic hybrid
            "W/P", "U/P", "B/P", "R/P", "G/P", // Phyrexian
            "W/U/P", "W/B/P", "U/B/P", "U/R/P", "B/R/P", "B/G/P", "R/W/P", "R/G/P", "G/W/P",
            "G/U/P", // Phyrexian-hybrid
            "C/W", "C/U", "C/B", "C/R", "C/G", // colorless hybrid
        ];

        for symbol in SYMBOLS {
            let expected = ManaCostShard::from_str(symbol)
                .unwrap_or_else(|_| panic!("from_str must accept {symbol}"));

            // Loader path: `{SYMBOL}` parses to exactly that one shard.
            assert_eq!(
                parse_mtgjson_mana_cost(&format!("{{{symbol}}}")),
                ManaCost::Cost {
                    generic: 0,
                    shards: vec![expected],
                },
                "loader disagrees with from_str for {symbol}"
            );

            // Nom path: `{SYMBOL}` parses to the same shard with no remainder.
            // Bind the braced string to a local so `rest` (which borrows from
            // the input) outlives the `format!` temporary.
            let braced = format!("{{{symbol}}}");
            let (rest, shard) = parse_mana_symbol(&braced)
                .unwrap_or_else(|_| panic!("nom parser must accept {braced}"));
            assert!(rest.is_empty(), "nom left remainder {rest:?} for {symbol}");
            assert_eq!(shard, expected, "nom disagrees with from_str for {symbol}");
        }
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

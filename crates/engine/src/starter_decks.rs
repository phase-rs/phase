use serde::{Deserialize, Serialize};

use crate::game::bracket_estimate::{ComboDeclaration, CommanderBracketTier};
use crate::game::deck_loading::PlayerDeckList;

/// A deck specified as card name strings — the wire format used by clients
/// and the starter deck module. Distinct from `PlayerDeckPayload` which
/// contains fully-parsed `CardFace` data resolved against the card database.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct DeckData {
    pub main_deck: Vec<String>,
    #[serde(default)]
    pub sideboard: Vec<String>,
    #[serde(default)]
    pub commander: Vec<String>,
    /// Commander-family companion kept outside the 100-card deck.
    #[serde(default)]
    pub companion: Vec<String>,
    /// CR 717.2: Supplementary Attraction deck (Unfinity) as card names.
    #[serde(default)]
    pub attraction_deck: Vec<String>,
    /// CR 901.15a: Supplementary Planechase planar deck as card names.
    #[serde(default)]
    pub planar_deck: Vec<String>,
    /// CR 904.3: Supplementary Archenemy scheme deck as card names.
    #[serde(default)]
    pub scheme_deck: Vec<String>,
    /// Unstable Contraptions: supplementary Contraption deck as card names.
    #[serde(default)]
    pub contraption_deck: Vec<String>,
    /// CR 123.2c: The sticker sheets selected for this deck/game.
    #[serde(default)]
    pub sticker_sheets: Vec<String>,
    /// Oathbreaker RC: the signature spell card name. Empty for all non-Oathbreaker formats.
    #[serde(default)]
    pub signature_spell: Vec<String>,
    /// Declared bracket tier for this deck. Defaults to `Core` when omitted,
    /// preserving backward compatibility with older wire payloads.
    #[serde(default)]
    pub bracket_tier: CommanderBracketTier,
    /// The deck owner's answer to the two-card-infinite-combo barometer. Defaults to
    /// `Undeclared` when omitted — every payload that predates this field is unanswered,
    /// which is deliberately NOT the same as a declared absence.
    #[serde(default)]
    pub combo_declaration: ComboDeclaration,
}

impl From<&DeckData> for PlayerDeckList {
    fn from(deck: &DeckData) -> Self {
        Self {
            main_deck: deck.main_deck.clone(),
            sideboard: deck.sideboard.clone(),
            commander: deck.commander.clone(),
            companion: deck.companion.clone(),
            attraction_deck: deck.attraction_deck.clone(),
            planar_deck: deck.planar_deck.clone(),
            scheme_deck: deck.scheme_deck.clone(),
            contraption_deck: deck.contraption_deck.clone(),
            sticker_sheets: deck.sticker_sheets.clone(),
            signature_spell: deck.signature_spell.clone(),
            bracket_tier: deck.bracket_tier,
            combo_declaration: deck.combo_declaration,
        }
    }
}

/// A named starter deck with its card list.
pub struct StarterDeck {
    pub name: &'static str,
    pub main_deck: &'static [(&'static str, u32)],
}

/// All available starter decks for AI opponents.
pub static STARTER_DECKS: &[StarterDeck] = &[
    StarterDeck {
        name: "Red Deck Wins",
        main_deck: &[
            ("Lightning Bolt", 4),
            ("Shock", 4),
            ("Goblin Guide", 4),
            ("Monastery Swiftspear", 4),
            ("Jackal Pup", 4),
            ("Raging Goblin", 4),
            ("Searing Spear", 4),
            ("Volcanic Hammer", 4),
            ("Firebolt", 4),
            ("Mountain", 24),
        ],
    },
    StarterDeck {
        name: "White Weenie",
        main_deck: &[
            ("Savannah Lions", 4),
            ("Elite Vanguard", 4),
            ("Raise the Alarm", 4),
            ("Precinct Captain", 4),
            ("Swords to Plowshares", 4),
            ("Glorious Anthem", 4),
            ("Benalish Marshal", 4),
            ("Soldier of the Pantheon", 4),
            ("Honor of the Pure", 4),
            ("Plains", 24),
        ],
    },
    StarterDeck {
        name: "Blue Control",
        main_deck: &[
            ("Counterspell", 4),
            ("Cancel", 4),
            ("Air Elemental", 4),
            ("Unsummon", 4),
            ("Divination", 4),
            ("Essence Scatter", 4),
            ("Wind Drake", 2),
            ("Opt", 4),
            ("Negate", 4),
            ("Island", 26),
        ],
    },
    StarterDeck {
        name: "Green Stompy",
        main_deck: &[
            ("Llanowar Elves", 4),
            ("Giant Growth", 4),
            ("Grizzly Bears", 4),
            ("Elvish Mystic", 4),
            ("Leatherback Baloth", 4),
            ("Rancor", 4),
            ("Garruk's Companion", 4),
            ("Kalonian Tusker", 4),
            ("Giant Spider", 4),
            ("Forest", 24),
        ],
    },
    StarterDeck {
        name: "Azorius Flyers",
        main_deck: &[
            ("Suntail Hawk", 4),
            ("Wind Drake", 4),
            ("Serra Angel", 4),
            ("Favorable Winds", 4),
            ("Swords to Plowshares", 4),
            ("Counterspell", 4),
            ("Opt", 4),
            ("Warden of Evos Isle", 4),
            ("Unsummon", 4),
            ("Plains", 12),
            ("Island", 12),
        ],
    },
];

/// Find a starter deck by name (case-insensitive).
pub fn find_starter_deck(name: &str) -> Option<DeckData> {
    let lower = name.to_lowercase();
    STARTER_DECKS
        .iter()
        .find(|d| d.name.to_lowercase() == lower)
        .map(starter_to_deck_data)
}

/// Pick a random starter deck.
pub fn random_starter_deck() -> DeckData {
    let idx = rand::random_range(0..STARTER_DECKS.len());
    starter_to_deck_data(&STARTER_DECKS[idx])
}

/// Return all available starter deck names.
pub fn starter_deck_names() -> Vec<&'static str> {
    STARTER_DECKS.iter().map(|d| d.name).collect()
}

fn starter_to_deck_data(deck: &StarterDeck) -> DeckData {
    let main_deck: Vec<String> = deck
        .main_deck
        .iter()
        .flat_map(|(name, count)| std::iter::repeat_n(name.to_string(), *count as usize))
        .collect();
    DeckData {
        main_deck,
        ..Default::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn player_deck_list_from_deck_data_round_trips_every_field() {
        let source = DeckData {
            main_deck: vec!["Main".to_string()],
            sideboard: vec!["Side".to_string()],
            commander: vec!["Commander".to_string()],
            companion: vec!["Companion".to_string()],
            attraction_deck: vec!["Attraction".to_string()],
            planar_deck: vec!["Plane".to_string()],
            scheme_deck: vec!["Scheme".to_string()],
            contraption_deck: vec!["Contraption".to_string()],
            sticker_sheets: vec!["Stickers".to_string()],
            signature_spell: vec!["Signature".to_string()],
            bracket_tier: CommanderBracketTier::Optimized,
            combo_declaration: ComboDeclaration::Intended {
                window: Some(crate::game::bracket_estimate::ComboWindow::EarlyGame),
            },
        };

        let converted = PlayerDeckList::from(&source);

        assert_eq!(converted.main_deck, source.main_deck);
        assert_eq!(converted.sideboard, source.sideboard);
        assert_eq!(converted.commander, source.commander);
        assert_eq!(converted.companion, source.companion);
        assert_eq!(converted.attraction_deck, source.attraction_deck);
        assert_eq!(converted.planar_deck, source.planar_deck);
        assert_eq!(converted.scheme_deck, source.scheme_deck);
        assert_eq!(converted.contraption_deck, source.contraption_deck);
        assert_eq!(converted.sticker_sheets, source.sticker_sheets);
        assert_eq!(converted.signature_spell, source.signature_spell);
        assert_eq!(converted.bracket_tier, source.bracket_tier);
        assert_eq!(converted.combo_declaration, source.combo_declaration);
    }

    #[test]
    fn all_starter_decks_have_60_cards() {
        for deck in STARTER_DECKS {
            let total: u32 = deck.main_deck.iter().map(|(_, c)| c).sum();
            assert_eq!(total, 60, "deck '{}' has {} cards", deck.name, total);
        }
    }

    #[test]
    fn find_starter_deck_case_insensitive() {
        let deck = find_starter_deck("red deck wins");
        assert!(deck.is_some());
        let data = deck.unwrap();
        assert_eq!(data.main_deck.len(), 60);
    }

    #[test]
    fn find_starter_deck_not_found() {
        assert!(find_starter_deck("nonexistent").is_none());
    }

    #[test]
    fn random_starter_deck_returns_60_cards() {
        let data = random_starter_deck();
        assert_eq!(data.main_deck.len(), 60);
    }

    #[test]
    fn starter_deck_names_returns_all() {
        let names = starter_deck_names();
        assert_eq!(names.len(), STARTER_DECKS.len());
        assert!(names.contains(&"Red Deck Wins"));
        assert!(names.contains(&"Green Stompy"));
    }
}

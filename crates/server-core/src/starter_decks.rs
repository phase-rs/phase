use crate::protocol::DeckData;

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
        sideboard: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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

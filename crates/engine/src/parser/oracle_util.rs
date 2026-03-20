use crate::types::ability::TargetFilter;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

/// Strip reminder text (parenthesized) from a line.
pub fn strip_reminder_text(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut depth = 0u32;
    for ch in text.chars() {
        match ch {
            '(' => depth += 1,
            ')' => {
                depth = depth.saturating_sub(1);
            }
            _ if depth == 0 => result.push(ch),
            _ => {}
        }
    }
    result.trim().to_string()
}

/// Replace "~" and "CARDNAME" with the actual card name, then lowercase for matching.
pub fn self_ref(text: &str, card_name: &str) -> String {
    text.replace('~', card_name).replace("CARDNAME", card_name)
}

/// Parse an English number word or digit at the start of text.
/// Returns (value, remaining_text) or None.
pub fn parse_number(text: &str) -> Option<(u32, &str)> {
    let text = text.trim_start();
    // Try digit(s) first
    let digit_end = text
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(text.len());
    if digit_end > 0 {
        if let Ok(n) = text[..digit_end].parse::<u32>() {
            return Some((n, text[digit_end..].trim_start()));
        }
    }
    // English words
    let words: &[(&str, u32)] = &[
        ("twenty", 20),
        ("nineteen", 19),
        ("eighteen", 18),
        ("seventeen", 17),
        ("sixteen", 16),
        ("fifteen", 15),
        ("fourteen", 14),
        ("thirteen", 13),
        ("twelve", 12),
        ("eleven", 11),
        ("ten", 10),
        ("nine", 9),
        ("eight", 8),
        ("seven", 7),
        ("six", 6),
        ("five", 5),
        ("four", 4),
        ("three", 3),
        ("two", 2),
        ("one", 1),
        ("an", 1),
        ("a", 1),
    ];
    let lower = text.to_lowercase();
    for &(word, val) in words {
        if lower.starts_with(word) {
            let rest = &text[word.len()..];
            // "a" and "an" must be followed by space or end
            if word.len() <= 2 && !rest.starts_with(|c: char| c.is_whitespace()) && !rest.is_empty()
            {
                continue;
            }
            return Some((val, rest.trim_start()));
        }
    }
    // "X" → 0 (caller should check for "X" and use QuantityRef::Variable where applicable)
    if lower.starts_with('x') {
        let rest = &text[1..];
        if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) {
            return Some((0, rest.trim_start()));
        }
    }
    None
}

/// Parse an English ordinal number word at the start of text.
/// Returns (value, remaining_text) or None.
/// Handles "second" = 2, "third" = 3, "fourth" = 4, etc.
pub fn parse_ordinal(text: &str) -> Option<(u32, &str)> {
    let text = text.trim_start();
    let ordinals: &[(&str, u32)] = &[
        ("twentieth", 20),
        ("nineteenth", 19),
        ("eighteenth", 18),
        ("seventeenth", 17),
        ("sixteenth", 16),
        ("fifteenth", 15),
        ("fourteenth", 14),
        ("thirteenth", 13),
        ("twelfth", 12),
        ("eleventh", 11),
        ("tenth", 10),
        ("ninth", 9),
        ("eighth", 8),
        ("seventh", 7),
        ("sixth", 6),
        ("fifth", 5),
        ("fourth", 4),
        ("third", 3),
        ("second", 2),
        ("first", 1),
    ];
    let lower = text.to_lowercase();
    for &(word, val) in ordinals {
        if lower.starts_with(word) {
            let rest = &text[word.len()..];
            return Some((val, rest.trim_start()));
        }
    }
    None
}

/// Parse mana symbols like `{2}{W}{U}` at the start of text.
/// Returns (ManaCost, remaining_text) or None.
pub fn parse_mana_symbols(text: &str) -> Option<(ManaCost, &str)> {
    let text = text.trim_start();
    if !text.starts_with('{') {
        return None;
    }

    let mut generic: u32 = 0;
    let mut shards = Vec::new();
    let mut pos = 0;
    let mut parsed_any = false;

    while pos < text.len() && text[pos..].starts_with('{') {
        let end = text[pos..].find('}')? + pos;
        let symbol = &text[pos + 1..end];
        pos = end + 1;
        parsed_any = true;

        match symbol {
            "W" => shards.push(ManaCostShard::White),
            "U" => shards.push(ManaCostShard::Blue),
            "B" => shards.push(ManaCostShard::Black),
            "R" => shards.push(ManaCostShard::Red),
            "G" => shards.push(ManaCostShard::Green),
            "C" => shards.push(ManaCostShard::Colorless),
            "S" => shards.push(ManaCostShard::Snow),
            "X" => shards.push(ManaCostShard::X),
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
                if let Ok(n) = other.parse::<u32>() {
                    generic += n;
                } else {
                    // Unknown symbol — stop parsing
                    pos = pos - symbol.len() - 2; // rewind
                    break;
                }
            }
        }
    }

    if !parsed_any {
        return None;
    }

    let cost = ManaCost::Cost { shards, generic };
    Some((cost, &text[pos..]))
}

/// Possessive variants used in MTG Oracle text ("your library", "their hand", etc.).
const POSSESSIVES: &[&str] = &["your", "their", "its owner's", "that player's"];

/// Object pronouns in MTG Oracle text that refer to previously-mentioned objects.
/// Used in anaphoric references like "shuffle it into", "put them onto", "exile that card".
pub const OBJECT_PRONOUNS: &[&str] = &["it", "them", "that card", "those cards"];

/// Test whether `text` matches `"{prefix} {word} {suffix}"` for any word in `variants`,
/// using the given match strategy.
fn match_phrase_variants(
    text: &str,
    prefix: &str,
    suffix: &str,
    variants: &[&str],
    strategy: fn(&str, &str) -> bool,
) -> bool {
    variants.iter().any(|word| {
        let mut needle = String::with_capacity(prefix.len() + word.len() + suffix.len() + 2);
        needle.push_str(prefix);
        needle.push(' ');
        needle.push_str(word);
        needle.push(' ');
        needle.push_str(suffix);
        strategy(text, &needle)
    })
}

/// Check if `text` contains `"{prefix} {possessive} {suffix}"` for any possessive variant.
///
/// Useful for matching zone references like "into your hand" / "into their hand" without
/// enumerating every possessive form at each call site.
pub fn contains_possessive(text: &str, prefix: &str, suffix: &str) -> bool {
    match_phrase_variants(text, prefix, suffix, POSSESSIVES, |hay, needle| {
        hay.contains(needle)
    })
}

/// Like `contains_possessive`, but checks if `text` starts with the phrase.
pub fn starts_with_possessive(text: &str, prefix: &str, suffix: &str) -> bool {
    match_phrase_variants(text, prefix, suffix, POSSESSIVES, |hay, needle| {
        hay.starts_with(needle)
    })
}

/// Check if `text` contains `"{prefix} {pronoun} {suffix}"` for any object pronoun variant.
///
/// Matches anaphoric references like "shuffle it into", "put them onto", "exile that card from".
pub fn contains_object_pronoun(text: &str, prefix: &str, suffix: &str) -> bool {
    match_phrase_variants(text, prefix, suffix, OBJECT_PRONOUNS, |hay, needle| {
        hay.contains(needle)
    })
}

/// Parse mana production symbols like `{G}` into Vec<ManaColor>.
pub fn parse_mana_production(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let text = text.trim_start();
    if !text.starts_with('{') {
        return None;
    }

    let mut colors = Vec::new();
    let mut pos = 0;

    while pos < text.len() && text[pos..].starts_with('{') {
        let end = match text[pos..].find('}') {
            Some(e) => e + pos,
            None => break,
        };
        let symbol = &text[pos + 1..end];
        pos = end + 1;

        match symbol {
            "W" => colors.push(ManaColor::White),
            "U" => colors.push(ManaColor::Blue),
            "B" => colors.push(ManaColor::Black),
            "R" => colors.push(ManaColor::Red),
            "G" => colors.push(ManaColor::Green),
            _ => {
                pos = pos - symbol.len() - 2;
                break;
            }
        }
    }

    if colors.is_empty() {
        return None;
    }
    Some((colors, &text[pos..]))
}

/// Capitalize the first letter of each word in a subtype name.
/// "human soldier" → "Human Soldier"
pub fn canonicalize_subtype_name(text: &str) -> String {
    text.split_whitespace()
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => {
                    let mut capitalized = first.to_uppercase().collect::<String>();
                    capitalized.push_str(chars.as_str());
                    capitalized
                }
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Irregular plural → singular mappings for MTG creature subtypes.
/// Only entries that cannot be resolved by stripping "-s" or "-es".
const SUBTYPE_PLURALS: &[(&str, &str)] = &[
    ("elves", "Elf"),
    ("dwarves", "Dwarf"),
    ("wolves", "Wolf"),
    ("halves", "Half"),
    ("fungi", "Fungus"),
    ("loci", "Locus"),
    ("djinn", "Djinn"),
    ("sphinxes", "Sphinx"),
    ("foxes", "Fox"),
    ("octopi", "Octopus"),
    ("mice", "Mouse"),
    ("oxen", "Ox"),
    ("allies", "Ally"),
    ("armies", "Army"),
    ("faeries", "Faerie"),
    ("zombies", "Zombie"),
    ("sorceries", "Sorcery"),
    ("ponies", "Pony"),
    ("harpies", "Harpy"),
    ("berserkers", "Berserker"),
];

/// Comprehensive list of MTG subtypes (creature types, land types, spell types, etc.).
/// Case-insensitive matching is done by lowercasing the input.
/// This covers the standard MTGJSON subtype list plus common Oracle text usage.
const SUBTYPES: &[&str] = &[
    // ── Creature types (alphabetical) ──
    "Advisor",
    "Aetherborn",
    "Alien",
    "Ally",
    "Angel",
    "Antelope",
    "Ape",
    "Archer",
    "Archon",
    "Armadillo",
    "Army",
    "Artificer",
    "Assassin",
    "Assembly-Worker",
    "Astartes",
    "Atog",
    "Aurochs",
    "Avatar",
    "Azra",
    "Badger",
    "Balloon",
    "Barbarian",
    "Bard",
    "Basilisk",
    "Bat",
    "Bear",
    "Beast",
    "Beeble",
    "Beholder",
    "Berserker",
    "Bird",
    "Blinkmoth",
    "Boar",
    "Bringer",
    "Brushwagg",
    "Bureaucrat",
    "Camarid",
    "Camel",
    "Capybara",
    "Caribou",
    "Carrier",
    "Cat",
    "Centaur",
    "Cephalid",
    "Chimera",
    "Citizen",
    "Cleric",
    "Clown",
    "Cockatrice",
    "Construct",
    "Coward",
    "Crab",
    "Crocodile",
    "Ctan",
    "Custodes",
    "Cyberman",
    "Cyclops",
    "Dalek",
    "Dauthi",
    "Demigod",
    "Demon",
    "Deserter",
    "Detective",
    "Devil",
    "Dinosaur",
    "Djinn",
    "Doctor",
    "Dog",
    "Dragon",
    "Drake",
    "Dreadnought",
    "Drone",
    "Druid",
    "Dryad",
    "Dwarf",
    "Efreet",
    "Egg",
    "Elder",
    "Eldrazi",
    "Elemental",
    "Elephant",
    "Elf",
    "Elk",
    "Employee",
    "Eye",
    "Faerie",
    "Ferret",
    "Fish",
    "Flagbearer",
    "Fox",
    "Fractal",
    "Frog",
    "Fungus",
    "Gamer",
    "Gargoyle",
    "Germ",
    "Giant",
    "Gith",
    "Gnoll",
    "Gnome",
    "Goat",
    "Goblin",
    "God",
    "Golem",
    "Gorgon",
    "Graveborn",
    "Gremlin",
    "Griffin",
    "Guest",
    "Hag",
    "Halfling",
    "Hamster",
    "Harpy",
    "Hellion",
    "Hippo",
    "Hippogriff",
    "Homarid",
    "Homunculus",
    "Horror",
    "Horse",
    "Human",
    "Hydra",
    "Hyena",
    "Illusion",
    "Imp",
    "Incarnation",
    "Inkling",
    "Inquisitor",
    "Insect",
    "Jackal",
    "Jellyfish",
    "Juggernaut",
    "Kavu",
    "Kirin",
    "Kithkin",
    "Knight",
    "Kobold",
    "Kor",
    "Kraken",
    "Lamia",
    "Lammasu",
    "Leech",
    "Leviathan",
    "Lhurgoyf",
    "Licid",
    "Lizard",
    "Llama",
    "Locus",
    "Manticore",
    "Masticore",
    "Mercenary",
    "Merfolk",
    "Metathran",
    "Minion",
    "Minotaur",
    "Mite",
    "Mole",
    "Monger",
    "Mongoose",
    "Monk",
    "Monkey",
    "Moonfolk",
    "Mount",
    "Mouse",
    "Mutant",
    "Myr",
    "Mystic",
    "Naga",
    "Nautilus",
    "Necron",
    "Nephilim",
    "Nightmare",
    "Nightstalker",
    "Ninja",
    "Noble",
    "Noggle",
    "Nomad",
    "Nymph",
    "Octopus",
    "Ogre",
    "Ooze",
    "Orb",
    "Orc",
    "Orgg",
    "Otter",
    "Ouphe",
    "Ox",
    "Oyster",
    "Pangolin",
    "Peasant",
    "Pegasus",
    "Pentavite",
    "Performer",
    "Pest",
    "Phelddagrif",
    "Phoenix",
    "Phyrexian",
    "Pilot",
    "Pincher",
    "Pirate",
    "Plant",
    "Pony",
    "Praetor",
    "Primarch",
    "Prism",
    "Processor",
    "Rabbit",
    "Raccoon",
    "Ranger",
    "Rat",
    "Rebel",
    "Reflection",
    "Rhino",
    "Rigger",
    "Robot",
    "Rogue",
    "Sable",
    "Salamander",
    "Samurai",
    "Sand",
    "Saproling",
    "Satyr",
    "Scarecrow",
    "Scion",
    "Scorpion",
    "Scout",
    "Sculpture",
    "Serf",
    "Serpent",
    "Servo",
    "Shade",
    "Shaman",
    "Shapeshifter",
    "Shark",
    "Sheep",
    "Siren",
    "Skeleton",
    "Slith",
    "Sliver",
    "Slug",
    "Snail",
    "Snake",
    "Soldier",
    "Soltari",
    "Spawn",
    "Specter",
    "Spellshaper",
    "Sphinx",
    "Spider",
    "Spike",
    "Spirit",
    "Splinter",
    "Sponge",
    "Squid",
    "Squirrel",
    "Starfish",
    "Surrakar",
    "Survivor",
    "Suspect",
    "Tentacle",
    "Tetravite",
    "Thalakos",
    "Thopter",
    "Thrull",
    "Tiefling",
    "Treefolk",
    "Trilobite",
    "Troll",
    "Turtle",
    "Tyranid",
    "Unicorn",
    "Vampire",
    "Vedalken",
    "Viashino",
    "Volver",
    "Wall",
    "Walrus",
    "Warlock",
    "Warrior",
    "Weasel",
    "Weird",
    "Werewolf",
    "Whale",
    "Wizard",
    "Wolf",
    "Wolverine",
    "Wombat",
    "Worm",
    "Wraith",
    "Wurm",
    "Yeti",
    "Zombie",
    "Zubera",
    // ── Land subtypes ──
    "Desert",
    "Forest",
    "Gate",
    "Island",
    "Lair",
    "Mine",
    "Mountain",
    "Plains",
    "Power-Plant",
    "Swamp",
    "Tower",
    "Urza's",
    // ── Artifact subtypes ──
    "Blood",
    "Clue",
    "Contraption",
    "Equipment",
    "Food",
    "Fortification",
    "Gold",
    "Incubator",
    "Junk",
    "Map",
    "Powerstone",
    "Treasure",
    "Vehicle",
    // ── Enchantment subtypes ──
    "Aura",
    "Background",
    "Cartouche",
    "Case",
    "Class",
    "Curse",
    "Role",
    "Room",
    "Rune",
    "Saga",
    "Shard",
    "Shrine",
    // ── Spell subtypes ──
    "Adventure",
    "Arcane",
    "Lesson",
    "Trap",
    // ── Planeswalker subtypes ──
    "Ajani",
    "Aminatou",
    "Angrath",
    "Arlinn",
    "Ashiok",
    "Basri",
    "Bolas",
    "Calix",
    "Chandra",
    "Comet",
    "Dack",
    "Dakkon",
    "Daretti",
    "Davriel",
    "Dihada",
    "Domri",
    "Dovin",
    "Ellywick",
    "Elspeth",
    "Estrid",
    "Freyalise",
    "Garruk",
    "Gideon",
    "Grist",
    "Guff",
    "Huatli",
    "Jace",
    "Jared",
    "Jaya",
    "Jeska",
    "Kaito",
    "Karn",
    "Kasmina",
    "Kaya",
    "Kiora",
    "Koth",
    "Liliana",
    "Lolth",
    "Lukka",
    "Minsc",
    "Mordenkainen",
    "Nahiri",
    "Narset",
    "Niko",
    "Nissa",
    "Nixilis",
    "Oko",
    "Quintorius",
    "Ral",
    "Rowan",
    "Saheeli",
    "Samut",
    "Sarkhan",
    "Serra",
    "Sivitri",
    "Sorin",
    "Szat",
    "Tamiyo",
    "Teferi",
    "Teyo",
    "Tezzeret",
    "Tibalt",
    "Tyvar",
    "Ugin",
    "Urza",
    "Venser",
    "Vivien",
    "Vraska",
    "Will",
    "Windgrace",
    "Wrenn",
    "Xenagos",
    "Yanggu",
    "Yanling",
    "Zariel",
];

/// Check if `text` starts with `prefix` using ASCII case-insensitive comparison,
/// followed by a word boundary (non-alphanumeric or end of string).
fn starts_with_word_ci(text: &str, prefix: &str) -> bool {
    if text.len() < prefix.len() {
        return false;
    }
    // prefix is always ASCII (subtypes/planeswalker names), but text may contain
    // multi-byte UTF-8 (e.g. em dashes). Guard against slicing inside a character.
    if !text.is_char_boundary(prefix.len()) {
        return false;
    }
    if !text[..prefix.len()].eq_ignore_ascii_case(prefix) {
        return false;
    }
    let after = &text[prefix.len()..];
    after.is_empty() || after.starts_with(|c: char| !c.is_alphanumeric())
}

/// Try to match a subtype at the start of text (case-insensitive).
/// Returns `(canonical_name, bytes_consumed)` or `None`.
/// Handles plural forms (regular and irregular).
pub fn parse_subtype(text: &str) -> Option<(String, usize)> {
    // Check irregular plurals first (they take priority over regular matching)
    for &(plural, singular) in SUBTYPE_PLURALS {
        if starts_with_word_ci(text, plural) {
            return Some((singular.to_string(), plural.len()));
        }
    }

    // Check each subtype (singular and regular plural)
    for &subtype in SUBTYPES {
        // Try singular
        if starts_with_word_ci(text, subtype) {
            return Some((subtype.to_string(), subtype.len()));
        }

        // Try regular plural: subtype + "s" — check subtype prefix + 's' at boundary
        let plural_len = subtype.len() + 1;
        if text.len() >= plural_len
            && text.is_char_boundary(subtype.len())
            && text[..subtype.len()].eq_ignore_ascii_case(subtype)
            && text.as_bytes()[subtype.len()] == b's'
        {
            let after = &text[plural_len..];
            if after.is_empty() || after.starts_with(|c: char| !c.is_alphanumeric()) {
                return Some((subtype.to_string(), plural_len));
            }
        }
    }

    None
}

/// Merge two filters into an Or, flattening nested Or branches.
pub fn merge_or_filters(a: TargetFilter, b: TargetFilter) -> TargetFilter {
    let mut filters = Vec::new();
    match a {
        TargetFilter::Or { filters: af } => filters.extend(af),
        other => filters.push(other),
    }
    match b {
        TargetFilter::Or { filters: bf } => filters.extend(bf),
        other => filters.push(other),
    }
    TargetFilter::Or { filters }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_number_digits() {
        assert_eq!(parse_number("3 damage"), Some((3, "damage")));
        assert_eq!(parse_number("10 life"), Some((10, "life")));
    }

    #[test]
    fn parse_number_words() {
        assert_eq!(parse_number("two cards"), Some((2, "cards")));
        assert_eq!(parse_number("a card"), Some((1, "card")));
        assert_eq!(parse_number("an opponent"), Some((1, "opponent")));
        assert_eq!(parse_number("three"), Some((3, "")));
    }

    #[test]
    fn parse_number_a_not_greedy() {
        // "a" should not match inside "attacking"
        assert_eq!(parse_number("attacking"), None);
        assert_eq!(parse_number("another"), None);
    }

    #[test]
    fn parse_number_none() {
        assert_eq!(parse_number("target creature"), None);
        assert_eq!(parse_number(""), None);
    }

    #[test]
    fn strip_reminder_text_basic() {
        assert_eq!(
            strip_reminder_text(
                "Flying (This creature can't be blocked except by creatures with flying.)"
            ),
            "Flying"
        );
    }

    #[test]
    fn strip_reminder_text_nested() {
        assert_eq!(
            strip_reminder_text("Ward {1} (Whenever this becomes the target)"),
            "Ward {1}"
        );
    }

    #[test]
    fn strip_reminder_text_no_parens() {
        assert_eq!(
            strip_reminder_text("Destroy target creature."),
            "Destroy target creature."
        );
    }

    #[test]
    fn self_ref_replaces_tilde() {
        assert_eq!(
            self_ref("~ deals 3 damage", "Lightning Bolt"),
            "Lightning Bolt deals 3 damage"
        );
    }

    #[test]
    fn parse_mana_symbols_basic() {
        let (cost, rest) = parse_mana_symbols("{2}{W}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::White]
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_mana_symbols_hybrid() {
        let (cost, _) = parse_mana_symbols("{G/W}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::GreenWhite]
            }
        );
    }

    #[test]
    fn parse_mana_symbols_zero() {
        let (cost, rest) = parse_mana_symbols("{0}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 0,
                shards: vec![],
            }
        );
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_mana_production_basic() {
        let (colors, _) = parse_mana_production("{G}").unwrap();
        assert_eq!(colors, vec![ManaColor::Green]);
    }

    #[test]
    fn parse_mana_production_multi() {
        let (colors, _) = parse_mana_production("{W}{W}").unwrap();
        assert_eq!(colors, vec![ManaColor::White, ManaColor::White]);
    }

    #[test]
    fn contains_possessive_matches_all_variants() {
        assert!(contains_possessive("into your hand", "into", "hand"));
        assert!(contains_possessive("into their hand", "into", "hand"));
        assert!(contains_possessive("into its owner's hand", "into", "hand"));
        assert!(contains_possessive(
            "into that player's hand",
            "into",
            "hand"
        ));
        assert!(!contains_possessive("into a hand", "into", "hand"));
    }

    #[test]
    fn starts_with_possessive_checks_prefix() {
        assert!(starts_with_possessive(
            "search your library for a card",
            "search",
            "library"
        ));
        assert!(starts_with_possessive(
            "search their library for a card",
            "search",
            "library"
        ));
        assert!(!starts_with_possessive(
            "then search your library",
            "search",
            "library"
        ));
    }

    #[test]
    fn contains_object_pronoun_matches_variants() {
        assert!(contains_object_pronoun(
            "shuffle it into",
            "shuffle",
            "into"
        ));
        assert!(contains_object_pronoun(
            "shuffle them into",
            "shuffle",
            "into"
        ));
        assert!(contains_object_pronoun(
            "shuffle that card into",
            "shuffle",
            "into"
        ));
        assert!(contains_object_pronoun(
            "put those cards onto the battlefield",
            "put",
            "onto"
        ));
        assert!(!contains_object_pronoun(
            "shuffle your into",
            "shuffle",
            "into"
        ));
    }

    // ── parse_subtype building block tests ──

    #[test]
    fn parse_subtype_singular() {
        assert_eq!(parse_subtype("zombie"), Some(("Zombie".to_string(), 6)));
        assert_eq!(parse_subtype("Zombie"), Some(("Zombie".to_string(), 6)));
    }

    #[test]
    fn parse_subtype_regular_plural() {
        assert_eq!(parse_subtype("zombies"), Some(("Zombie".to_string(), 7)));
        assert_eq!(parse_subtype("vampires"), Some(("Vampire".to_string(), 8)));
    }

    #[test]
    fn parse_subtype_irregular_plural() {
        assert_eq!(parse_subtype("elves"), Some(("Elf".to_string(), 5)));
        assert_eq!(parse_subtype("dwarves"), Some(("Dwarf".to_string(), 7)));
        assert_eq!(parse_subtype("wolves"), Some(("Wolf".to_string(), 6)));
    }

    #[test]
    fn parse_subtype_non_creature() {
        assert_eq!(
            parse_subtype("equipment"),
            Some(("Equipment".to_string(), 9))
        );
        assert_eq!(parse_subtype("forest"), Some(("Forest".to_string(), 6)));
        assert_eq!(parse_subtype("aura"), Some(("Aura".to_string(), 4)));
    }

    #[test]
    fn parse_subtype_rejects_non_subtypes() {
        assert_eq!(parse_subtype("creature"), None);
        assert_eq!(parse_subtype("draw"), None);
        assert_eq!(parse_subtype("destroy"), None);
    }

    #[test]
    fn parse_subtype_word_boundary() {
        // "goblin" should match but "goblinking" should not
        assert_eq!(
            parse_subtype("goblin you control"),
            Some(("Goblin".to_string(), 6))
        );
        assert_eq!(parse_subtype("goblinking"), None);
    }
}

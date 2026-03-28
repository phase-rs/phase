use crate::types::ability::{QuantityExpr, QuantityRef, RoundingMode, TargetFilter};
use crate::types::card_type::CoreType;
use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};

/// A borrowed pair of `(original, lowercase)` slices kept in lockstep.
///
/// Eliminates redundant `to_lowercase()` allocations by lowercasing once at the
/// entry point and threading both slices through the parser call chain. All
/// case-insensitive matching operates on `lower`; original-case text is preserved
/// for data construction (e.g. card names, display strings).
#[derive(Debug, Clone, Copy)]
pub struct TextPair<'a> {
    pub original: &'a str,
    pub lower: &'a str,
}

impl<'a> TextPair<'a> {
    pub fn new(original: &'a str, lower: &'a str) -> Self {
        debug_assert_eq!(
            original.len(),
            lower.len(),
            "TextPair: original and lower must have equal byte length"
        );
        debug_assert_eq!(
            original.to_lowercase(),
            lower,
            "TextPair: lower must be the lowercase of original"
        );
        Self { original, lower }
    }

    /// Strip a prefix from the lowered text, advancing both slices in lockstep.
    pub fn strip_prefix(&self, prefix: &str) -> Option<Self> {
        self.lower.strip_prefix(prefix).map(|rest| {
            let consumed = self.lower.len() - rest.len();
            Self {
                original: &self.original[consumed..],
                lower: rest,
            }
        })
    }

    /// Strip a suffix from the lowered text, trimming both slices in lockstep.
    pub fn strip_suffix(&self, suffix: &str) -> Option<Self> {
        self.lower.strip_suffix(suffix).map(|rest| {
            let len = rest.len();
            Self {
                original: &self.original[..len],
                lower: rest,
            }
        })
    }

    pub fn trim_start(&self) -> Self {
        let trimmed = self.lower.trim_start();
        let consumed = self.lower.len() - trimmed.len();
        Self {
            original: &self.original[consumed..],
            lower: trimmed,
        }
    }

    pub fn trim_end(&self) -> Self {
        let trimmed = self.lower.trim_end();
        let len = trimmed.len();
        Self {
            original: &self.original[..len],
            lower: trimmed,
        }
    }

    pub fn trim_end_matches(&self, pat: char) -> Self {
        let trimmed = self.lower.trim_end_matches(pat);
        let len = trimmed.len();
        Self {
            original: &self.original[..len],
            lower: trimmed,
        }
    }

    pub fn starts_with(&self, prefix: &str) -> bool {
        self.lower.starts_with(prefix)
    }

    pub fn ends_with(&self, suffix: &str) -> bool {
        self.lower.ends_with(suffix)
    }

    pub fn contains(&self, needle: &str) -> bool {
        self.lower.contains(needle)
    }

    pub fn is_empty(&self) -> bool {
        self.lower.is_empty()
    }

    pub fn len(&self) -> usize {
        self.lower.len()
    }

    pub fn find(&self, needle: &str) -> Option<usize> {
        self.lower.find(needle)
    }

    pub fn rfind(&self, needle: &str) -> Option<usize> {
        self.lower.rfind(needle)
    }

    /// Split at a byte position, producing two `TextPair` halves.
    ///
    /// `pos` MUST come from this TextPair's own methods (`find`, `strip_prefix`
    /// remainder len, etc.) to guarantee it falls on valid character boundaries.
    pub fn split_at(&self, pos: usize) -> (Self, Self) {
        debug_assert!(
            self.original.is_char_boundary(pos),
            "TextPair::split_at: pos must be a char boundary"
        );
        (
            Self {
                original: &self.original[..pos],
                lower: &self.lower[..pos],
            },
            Self {
                original: &self.original[pos..],
                lower: &self.lower[pos..],
            },
        )
    }

    /// Take a sub-range by byte positions `[start..end]`.
    pub fn slice(&self, start: usize, end: usize) -> Self {
        debug_assert!(self.original.is_char_boundary(start));
        debug_assert!(self.original.is_char_boundary(end));
        Self {
            original: &self.original[start..end],
            lower: &self.lower[start..end],
        }
    }

    /// Find `needle` in the lowered text and return both slices advanced past it.
    ///
    /// Equivalent to `self.find(needle)` + `self.split_at(pos + needle.len()).1`
    /// but expressed as a single operation.
    pub fn strip_after(&self, needle: &str) -> Option<Self> {
        self.lower.find(needle).map(|pos| {
            let after = pos + needle.len();
            Self {
                original: &self.original[after..],
                lower: &self.lower[after..],
            }
        })
    }

    /// Find first `needle` in lowered text, return `(before, after)` excluding needle.
    pub fn split_around(&self, needle: &str) -> Option<(Self, Self)> {
        self.lower.find(needle).map(|pos| {
            let after = pos + needle.len();
            (
                Self {
                    original: &self.original[..pos],
                    lower: &self.lower[..pos],
                },
                Self {
                    original: &self.original[after..],
                    lower: &self.lower[after..],
                },
            )
        })
    }

    /// Find last `needle` in lowered text, return `(before, after)` excluding needle.
    pub fn rsplit_around(&self, needle: &str) -> Option<(Self, Self)> {
        self.lower.rfind(needle).map(|pos| {
            let after = pos + needle.len();
            (
                Self {
                    original: &self.original[..pos],
                    lower: &self.lower[..pos],
                },
                Self {
                    original: &self.original[after..],
                    lower: &self.lower[after..],
                },
            )
        })
    }
}

/// Find `needle` in `text` and return everything after it, or `None`.
///
/// Combines `text.find(needle)` + `&text[pos + needle.len()..]` into one call.
pub fn strip_after<'a>(text: &'a str, needle: &str) -> Option<&'a str> {
    text.find(needle).map(|pos| &text[pos + needle.len()..])
}

/// Find `needle` in `text` and return `(before, after)` excluding needle, or `None`.
pub fn split_around<'a>(text: &'a str, needle: &str) -> Option<(&'a str, &'a str)> {
    text.find(needle)
        .map(|pos| (&text[..pos], &text[pos + needle.len()..]))
}

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
    // "X" → 0 for callers that genuinely want numeric-only (P/T, costs, counters).
    // For effect quantities, use `parse_count_expr` which returns Variable("X") instead.
    if lower.starts_with('x') {
        let rest = &text[1..];
        if rest.is_empty() || rest.starts_with(|c: char| c.is_whitespace()) {
            return Some((0, rest.trim_start()));
        }
    }
    None
}

/// Parse a count expression that may be a variable ("X") or a fixed number.
///
/// Checks for "X"/"x" first → `QuantityExpr::Ref(Variable("X"))`, then falls back to
/// `parse_number` → `QuantityExpr::Fixed`. Returns `None` if neither matches.
/// Use this instead of `parse_number` at call sites that represent effect quantities
/// (draw count, life amount, damage, mill count, scry count, etc.).
pub fn parse_count_expr(text: &str) -> Option<(QuantityExpr, &str)> {
    let text = text.trim_start();
    let lower = text.to_lowercase();
    // CR 107.1a: "half X" expressions — parse the inner quantity, wrap in HalfRounded.
    // Default rounding is Down per CR 107.1a unless "rounded up" is specified.
    if lower.strip_prefix("half ").is_some() {
        let original_after = &text[5..];
        let (inner, rest) = parse_count_expr(original_after)?;
        // Check if remainder contains a rounding override
        let rest_lower = rest.to_lowercase();
        let (rounding, final_rest) =
            if let Some(after_round) = rest_lower.strip_prefix(", rounded up") {
                (RoundingMode::Up, &rest[rest.len() - after_round.len()..])
            } else if let Some(after_round) = rest_lower.strip_prefix(", round up") {
                (RoundingMode::Up, &rest[rest.len() - after_round.len()..])
            } else {
                (RoundingMode::Down, rest)
            };
        return Some((
            QuantityExpr::HalfRounded {
                inner: Box::new(inner),
                rounding,
            },
            final_rest,
        ));
    }
    // CR 107.3a: "X" in Oracle text represents a variable determined at cast time.
    // Accept X followed by whitespace, comma, period, or end-of-string — all valid
    // Oracle text boundaries (e.g., "X cards", "X, rounded up", "X.").
    if lower.starts_with('x') {
        let rest = &text[1..];
        if rest.is_empty() || rest.starts_with(|c: char| !c.is_alphanumeric()) {
            return Some((
                QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                rest.trim_start(),
            ));
        }
    }
    let (n, rest) = parse_number(text)?;
    Some((QuantityExpr::Fixed { value: n as i32 }, rest))
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

        // Case-insensitive matching: the trigger parser may lowercase mana symbols
        // (e.g., "{g}" instead of "{G}"), so normalize before matching.
        let symbol_upper = symbol.to_ascii_uppercase();
        match symbol_upper.as_str() {
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

/// "this \<card_type\>" self-reference phrases in Oracle text.
///
/// Used by: `parse_target` (object recognition), `subject.rs` (subject stripping),
/// `normalize_card_name_refs` (tilde normalization).
///
/// Does NOT include: `"~"` (already handled separately), `"this"` (bare, too ambiguous
/// for prefix matching), `"this card"` (not safe for normalization — context-dependent),
/// `"this spell"` (not safe for normalization — breaks `oracle_casting.rs` spell-specific
/// parsing), `"it"` (context-dependent, needs `ParseContext` resolution).
/// Those are handled locally in their consuming functions.
pub const SELF_REF_TYPE_PHRASES: &[&str] = &[
    "this creature",
    "this permanent",
    "this artifact",
    "this land",
    "this enchantment",
    "this equipment",
    "this aura",
    "this vehicle",
    "this planeswalker",
    "this battle",
];

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
        if !prefix.is_empty() {
            needle.push(' ');
        }
        needle.push_str(word);
        if !suffix.is_empty() {
            needle.push(' ');
        }
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

/// Strip a possessive prefix ("your ", "their ", etc.) and return the matched word + remainder.
///
/// Returns `Some((possessive_word, remainder))` on match, `None` if no possessive found.
/// The `possessive_word` can be mapped to `ControllerRef` by the caller:
/// `"your"/"their"/"that player's"` → `You` (in subject-stripped context),
/// `"its owner's"` needs special handling (no `Owner` variant exists).
pub fn strip_possessive(text: &str) -> Option<(&'static str, &str)> {
    for &poss in POSSESSIVES {
        if let Some(rest) = text.strip_prefix(poss) {
            if let Some(rest) = rest.strip_prefix(' ') {
                return Some((poss, rest));
            }
        }
    }
    None
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
            "W" | "w" => colors.push(ManaColor::White),
            "U" | "u" => colors.push(ManaColor::Blue),
            "B" | "b" => colors.push(ManaColor::Black),
            "R" | "r" => colors.push(ManaColor::Red),
            "G" | "g" => colors.push(ManaColor::Green),
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

/// Infer the core type for a known subtype name.
///
/// Artifact subtypes (Treasure, Food, Clue, Blood, Gold, Map, Equipment, Vehicle)
/// map to `CoreType::Artifact`. Land subtypes (Forest, Plains, etc.) map to
/// `CoreType::Land`. Enchantment subtypes (Aura, Saga, etc.) map to
/// `CoreType::Enchantment`. Returns `None` for creature subtypes (the caller's
/// existing default) or unknown subtypes.
///
/// Used by lord-pattern parsers to avoid defaulting all subtypes to Creature.
pub fn infer_core_type_for_subtype(subtype: &str) -> Option<CoreType> {
    match subtype {
        // Artifact subtypes (CR 205.3g)
        "Treasure" | "Food" | "Clue" | "Blood" | "Gold" | "Map" | "Junk" | "Powerstone"
        | "Equipment" | "Vehicle" | "Fortification" | "Contraption" => Some(CoreType::Artifact),
        // Land subtypes (CR 205.3i)
        "Forest" | "Plains" | "Island" | "Mountain" | "Swamp" | "Desert" | "Gate" | "Locus"
        | "Cave" | "Sphere" | "Mine" | "Tower" | "Power-Plant" => Some(CoreType::Land),
        // Enchantment subtypes (CR 205.3h)
        "Aura" | "Shrine" | "Saga" | "Cartouche" | "Case" | "Class" | "Curse" | "Room"
        | "Shard" | "Rune" | "Background" => Some(CoreType::Enchantment),
        _ => None,
    }
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

/// Count the number of energy symbols ({E} or {e}) in Oracle text.
/// Used to parse "you get {E}{E}" → GainEnergy { amount: 2 }.
pub fn count_energy_symbols(text: &str) -> u32 {
    let lower = text.to_lowercase();
    lower.matches("{e}").count() as u32
}

/// Check if text contains unconsumed conditional connectors that indicate
/// a catch-all pattern may be silently dropping important Oracle text.
/// Used as a safety guard in broad `.contains()` matchers.
///
/// Intentionally excludes " when " and " whenever " — these are trigger connectors
/// in Oracle text, not conditional guards on the main effect being parsed.
pub fn has_unconsumed_conditional(text: &str) -> bool {
    let lower = text.to_lowercase();
    [" unless ", " except ", " as long as "]
        .iter()
        .any(|kw| lower.contains(kw))
}

/// Replace all occurrences of `needle` in `haystack` with `replacement`,
/// case-sensitively, only at word boundaries.
fn replace_all_words_case_sensitive(haystack: &str, needle: &str, replacement: &str) -> String {
    let needle_len = needle.len();
    let mut result = String::with_capacity(haystack.len());
    let mut last_end = 0;

    for (pos, _) in haystack.match_indices(needle) {
        let end = pos + needle_len;
        let at_word_start = pos == 0 || !haystack.as_bytes()[pos - 1].is_ascii_alphanumeric();
        let at_word_end =
            end == haystack.len() || !haystack.as_bytes()[end].is_ascii_alphanumeric();
        if at_word_start && at_word_end && pos >= last_end {
            result.push_str(&haystack[last_end..pos]);
            result.push_str(replacement);
            last_end = end;
        }
    }
    if last_end == 0 {
        return haystack.to_string();
    }
    result.push_str(&haystack[last_end..]);
    result
}

/// Replace all occurrences of `needle` in `haystack` with `replacement`,
/// case-insensitively, only at word boundaries (start/end of string, non-alphanumeric chars).
fn replace_all_words(haystack: &str, needle: &str, replacement: &str) -> String {
    let lower_haystack = haystack.to_lowercase();
    let lower_needle = needle.to_lowercase();
    let needle_len = needle.len();
    let mut result = String::with_capacity(haystack.len());
    let mut last_end = 0;

    for (pos, _) in lower_haystack.match_indices(&lower_needle) {
        let end = pos + needle_len;
        let at_word_start = pos == 0 || !haystack.as_bytes()[pos - 1].is_ascii_alphanumeric();
        let at_word_end =
            end == haystack.len() || !haystack.as_bytes()[end].is_ascii_alphanumeric();
        if at_word_start && at_word_end && pos >= last_end {
            result.push_str(&haystack[last_end..pos]);
            result.push_str(replacement);
            last_end = end;
        }
    }
    if last_end == 0 {
        return haystack.to_string();
    }
    result.push_str(&haystack[last_end..]);
    result
}

// CR 201.4b: A card's Oracle text uses its name to refer to itself.
/// Normalize all self-references in Oracle text to `~`.
///
/// Handles full card name, Alchemy A- prefix, comma-based legendary short names
/// ("Haliya, Guided by Light" → "Haliya"), "of"-based short names
/// ("Rosie Cotton of South Lane" → "Rosie Cotton"), and first-word short names
/// ("Sharuum the Hegemon" → "Sharuum"), plus generic phrases like "this creature".
pub fn normalize_card_name_refs(text: &str, card_name: &str) -> String {
    // Strip A- prefix (Alchemy rebalanced cards in MTGJSON)
    let effective_name = card_name.strip_prefix("A-").unwrap_or(card_name);

    // Replace full card name (word-boundary-aware, all occurrences).
    // Use case-insensitive matching only for multi-word names (proper nouns).
    // Single-word names like "Scheme", "Contraption" are case-sensitive to avoid
    // matching generic English words in Oracle text (e.g., "this scheme in motion").
    let mut result = if effective_name.contains(' ') {
        replace_all_words(text, effective_name, "~")
    } else {
        replace_all_words_case_sensitive(text, effective_name, "~")
    };

    // Comma-based legendary short name: "Haliya, Guided by Light" → "Haliya"
    if let Some(comma_pos) = effective_name.find(", ") {
        let short_name = &effective_name[..comma_pos];
        if short_name.len() >= 3 && !result.contains('~') {
            result = replace_all_words(&result, short_name, "~");
        }
    }

    // "Of"-based short name: "Rosie Cotton of South Lane" → "Rosie Cotton"
    if !result.contains('~') {
        if let Some(of_pos) = effective_name.find(" of ") {
            let short_name = &effective_name[..of_pos];
            if short_name.len() >= 3 {
                result = replace_all_words(&result, short_name, "~");
            }
        }
    }

    // Generic self-references (case-insensitive) — run BEFORE first-word fallback
    // so that cards like "Copy Enchantment" whose Oracle text uses "this enchantment"
    // get the `~` guard set, preventing false-positive first-word matches on "Copy".
    for phrase in SELF_REF_TYPE_PHRASES {
        result = replace_all_words(&result, phrase, "~");
    }

    // Short-name fallback via starts_with: if no prior strategy produced a ~,
    // try progressively shorter prefixes of the card name against the text.
    // E.g. card "Sharuum the Hegemon" → try "Sharuum the" then "Sharuum".
    // "Sharuum" found in "When Sharuum enters" → replace with ~.
    // Longest-first so "Rosie Cotton" matches before "Rosie" alone.
    // Case-sensitive: Oracle text uses proper-noun capitalization for card name
    // references, so "Sharuum" (capitalized) is a self-ref but "mana" (lowercase
    // in "for mana, add") in "Mana Flare" is not.
    if !result.contains('~') {
        let name_words: Vec<&str> = effective_name.split_whitespace().collect();
        for len in (1..name_words.len()).rev() {
            let candidate = name_words[..len].join(" ");
            if candidate.len() >= 2 {
                let replaced = replace_all_words_case_sensitive(&result, &candidate, "~");
                if replaced != result {
                    // Guard: Don't replace subtype references like "Sliver creatures"
                    // when "Sliver" is a prefix of the card name "Sliver Hivelord".
                    // The word before "creatures/creature" is a subtype, not a self-ref.
                    if replaced.contains("~ creatures") || replaced.contains("~ creature") {
                        continue;
                    }
                    result = replaced;
                    break;
                }
            }
        }
    }

    // Restore card name in "named ~" and "chosen name ~" clauses —
    // tilde normalization should not apply inside "named [CardName]" patterns.
    let effective_name_str = effective_name;
    result = result.replace("named ~", &format!("named {effective_name_str}"));

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_card_name_refs tests ---

    #[test]
    fn normalize_first_word_short_name() {
        assert_eq!(
            normalize_card_name_refs("When Sharuum enters", "Sharuum the Hegemon"),
            "When ~ enters"
        );
    }

    #[test]
    fn normalize_a_prefix_full_name() {
        assert_eq!(
            normalize_card_name_refs("When Sprouting Goblin enters", "A-Sprouting Goblin"),
            "When ~ enters"
        );
    }

    #[test]
    fn normalize_first_word_of_pattern() {
        assert_eq!(
            normalize_card_name_refs("When Tivadar enters", "Tivadar of Thorn"),
            "When ~ enters"
        );
    }

    #[test]
    fn normalize_comma_legendary_short_name() {
        assert_eq!(
            normalize_card_name_refs(
                "Whenever Haliya or another creature enters",
                "Haliya, Guided by Light"
            ),
            "Whenever ~ or another creature enters"
        );
    }

    #[test]
    fn normalize_of_based_short_name() {
        assert_eq!(
            normalize_card_name_refs("When Rosie Cotton enters", "Rosie Cotton of South Lane"),
            "When ~ enters"
        );
    }

    #[test]
    fn normalize_multiple_self_refs() {
        assert_eq!(
            normalize_card_name_refs(
                "Test Card deals damage and Test Card gains life",
                "Test Card"
            ),
            "~ deals damage and ~ gains life"
        );
    }

    #[test]
    fn normalize_this_creature() {
        assert_eq!(
            normalize_card_name_refs("this creature enters", "Goblin Chainwhirler"),
            "~ enters"
        );
    }

    #[test]
    fn normalize_this_creature_capital() {
        assert_eq!(
            normalize_card_name_refs("This creature enters tapped", "Some Card"),
            "~ enters tapped"
        );
    }

    #[test]
    fn normalize_no_false_positive_the_prefix() {
        // "The" is 3 chars, below the >= 4 first-word threshold
        assert_eq!(
            normalize_card_name_refs("the battlefield", "The Beamtown Bullies"),
            "the battlefield"
        );
    }

    #[test]
    fn normalize_word_boundary_prevents_partial_match() {
        // "Sliver" should not match inside "Slivers"
        assert_eq!(
            normalize_card_name_refs("Slivers you control", "Sliver Gravemother"),
            "Slivers you control"
        );
    }

    #[test]
    fn normalize_sliver_hivelord_preserves_subtype() {
        // B18: "Sliver" before "creatures" is a subtype reference, not a self-ref
        assert_eq!(
            normalize_card_name_refs(
                "Sliver creatures you control have indestructible.",
                "Sliver Hivelord",
            ),
            "Sliver creatures you control have indestructible."
        );
    }

    #[test]
    fn normalize_no_false_positive_first_word_when_generic_matches() {
        // "Copy Enchantment" — "this enchantment" should match first,
        // preventing "copy" from being falsely replaced in "a copy of"
        let result = normalize_card_name_refs(
            "You may have this enchantment enter as a copy of an enchantment on the battlefield.",
            "Copy Enchantment",
        );
        assert!(
            result.contains("a copy of"),
            "should not replace 'copy' as first-word short name, got: {result}"
        );
        assert!(result.contains('~'), "should replace 'this enchantment'");
    }

    #[test]
    fn normalize_full_name_takes_priority() {
        // Full name match should fire before first-word
        assert_eq!(
            normalize_card_name_refs("Goblin Chainwhirler enters", "Goblin Chainwhirler"),
            "~ enters"
        );
    }

    // --- replace_all_words tests ---

    #[test]
    fn replace_all_words_basic() {
        assert_eq!(
            replace_all_words("hello world hello", "hello", "~"),
            "~ world ~"
        );
    }

    #[test]
    fn replace_all_words_no_partial() {
        assert_eq!(replace_all_words("helloworld", "hello", "~"), "helloworld");
    }

    #[test]
    fn replace_all_words_case_insensitive() {
        assert_eq!(
            replace_all_words("Hello world HELLO", "hello", "~"),
            "~ world ~"
        );
    }

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
    fn parse_count_expr_variable_x() {
        let (qty, rest) = parse_count_expr("X cards").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { .. }
            }
        ));
        assert_eq!(rest, "cards");
    }

    #[test]
    fn parse_count_expr_fixed_number() {
        let (qty, rest) = parse_count_expr("3 cards").unwrap();
        assert!(matches!(qty, QuantityExpr::Fixed { value: 3 }));
        assert_eq!(rest, "cards");
    }

    #[test]
    fn parse_count_expr_word_number() {
        let (qty, rest) = parse_count_expr("two creatures").unwrap();
        assert!(matches!(qty, QuantityExpr::Fixed { value: 2 }));
        assert_eq!(rest, "creatures");
    }

    #[test]
    fn parse_count_expr_article() {
        let (qty, rest) = parse_count_expr("a card").unwrap();
        assert!(matches!(qty, QuantityExpr::Fixed { value: 1 }));
        assert_eq!(rest, "card");
    }

    #[test]
    fn parse_count_expr_bare_x() {
        let (qty, rest) = parse_count_expr("X").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { .. }
            }
        ));
        assert_eq!(rest, "");
    }

    #[test]
    fn parse_count_expr_none_for_text() {
        assert!(parse_count_expr("target creature").is_none());
    }

    #[test]
    fn parse_count_expr_half_x() {
        let (qty, rest) = parse_count_expr("half X cards").unwrap();
        match qty {
            QuantityExpr::HalfRounded { inner, rounding } => {
                assert!(matches!(
                    *inner,
                    QuantityExpr::Ref {
                        qty: QuantityRef::Variable { .. }
                    }
                ));
                assert_eq!(
                    rounding,
                    crate::types::ability::RoundingMode::Down,
                    "Default rounding should be Down per CR 107.1a"
                );
            }
            other => panic!("Expected HalfRounded, got {other:?}"),
        }
        assert_eq!(rest, "cards");
    }

    #[test]
    fn parse_count_expr_half_x_bare() {
        let (qty, _rest) = parse_count_expr("half X").unwrap();
        assert!(matches!(
            qty,
            QuantityExpr::HalfRounded {
                rounding: crate::types::ability::RoundingMode::Down,
                ..
            }
        ));
    }

    #[test]
    fn parse_count_expr_half_x_rounded_up() {
        let (qty, _rest) = parse_count_expr("half X, rounded up").unwrap();
        match qty {
            QuantityExpr::HalfRounded { rounding, .. } => {
                assert_eq!(rounding, crate::types::ability::RoundingMode::Up);
            }
            other => panic!("Expected HalfRounded, got {other:?}"),
        }
    }

    #[test]
    fn parse_count_expr_fixed_regression() {
        // Ensure "3 cards" still returns Fixed, not HalfRounded
        let (qty, rest) = parse_count_expr("3 cards").unwrap();
        assert!(matches!(qty, QuantityExpr::Fixed { value: 3 }));
        assert_eq!(rest, "cards");
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
    fn parse_mana_symbols_lowercase() {
        let (cost, rest) = parse_mana_symbols("{g}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 0,
                shards: vec![ManaCostShard::Green],
            }
        );
        assert_eq!(rest, "");

        let (cost, _) = parse_mana_symbols("{2}{w/u}").unwrap();
        assert_eq!(
            cost,
            ManaCost::Cost {
                generic: 2,
                shards: vec![ManaCostShard::WhiteBlue],
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
    fn starts_with_possessive_empty_prefix() {
        assert!(starts_with_possessive("their graveyard", "", "graveyard"));
        assert!(starts_with_possessive(
            "your library for a card",
            "",
            "library"
        ));
        assert!(starts_with_possessive(
            "its owner's hand and then",
            "",
            "hand"
        ));
        assert!(!starts_with_possessive("a graveyard", "", "graveyard"));
    }

    #[test]
    fn strip_possessive_returns_word_and_rest() {
        assert_eq!(
            strip_possessive("their graveyard"),
            Some(("their", "graveyard"))
        );
        assert_eq!(
            strip_possessive("your library for a card"),
            Some(("your", "library for a card"))
        );
        assert_eq!(
            strip_possessive("its owner's hand"),
            Some(("its owner's", "hand"))
        );
        assert_eq!(strip_possessive("a graveyard"), None);
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

    #[test]
    fn count_energy_symbols_test() {
        assert_eq!(super::count_energy_symbols("you get {e}{e}"), 2);
        assert_eq!(super::count_energy_symbols("you get {E}{E}{E}"), 3);
        assert_eq!(super::count_energy_symbols("{e}"), 1);
        assert_eq!(super::count_energy_symbols("no energy here"), 0);
    }

    #[test]
    fn text_pair_strip_prefix() {
        let original = "Draw two cards";
        let lower = original.to_lowercase();
        let tp = super::TextPair::new(original, &lower);
        let rest = tp.strip_prefix("draw ").unwrap();
        assert_eq!(rest.original, "two cards");
        assert_eq!(rest.lower, "two cards");
        assert!(tp.strip_prefix("discard ").is_none());
    }

    #[test]
    fn text_pair_strip_suffix() {
        let original = "Destroy target creature.";
        let lower = original.to_lowercase();
        let tp = super::TextPair::new(original, &lower);
        let rest = tp.strip_suffix(".").unwrap();
        assert_eq!(rest.original, "Destroy target creature");
        assert_eq!(rest.lower, "destroy target creature");
    }

    #[test]
    fn text_pair_split_at() {
        let original = "Exile target creature";
        let lower = original.to_lowercase();
        let tp = super::TextPair::new(original, &lower);
        let pos = tp.find("target").unwrap();
        let (before, after) = tp.split_at(pos);
        assert_eq!(before.original, "Exile ");
        assert_eq!(after.original, "target creature");
    }

    #[test]
    fn text_pair_trim_start() {
        let original = "  Flying";
        let lower = original.to_lowercase();
        let tp = super::TextPair::new(original, &lower);
        let trimmed = tp.trim_start();
        assert_eq!(trimmed.original, "Flying");
        assert_eq!(trimmed.lower, "flying");
    }

    #[test]
    fn text_pair_em_dash() {
        // Em-dash is 3 bytes in UTF-8, same lowercased
        let original = "Choose one \u{2014}";
        let lower = original.to_lowercase();
        let tp = super::TextPair::new(original, &lower);
        assert!(tp.contains("\u{2014}"));
        let rest = tp.strip_prefix("choose one ").unwrap();
        assert_eq!(rest.original, "\u{2014}");
    }

    // --- strip_after (free function) tests ---

    #[test]
    fn strip_after_finds_needle() {
        assert_eq!(strip_after("hello world foo", "world "), Some("foo"));
    }

    #[test]
    fn strip_after_returns_none_on_miss() {
        assert_eq!(strip_after("hello world", "xyz"), None);
    }

    #[test]
    fn strip_after_at_start() {
        assert_eq!(strip_after("prefix rest", "prefix "), Some("rest"));
    }

    #[test]
    fn strip_after_at_end() {
        assert_eq!(strip_after("hello world", "world"), Some(""));
    }

    #[test]
    fn strip_after_is_case_sensitive() {
        // The free function is intentionally case-sensitive.
        // Cross-string (lower/original) patterns must use find() on lowered + manual slicing.
        assert_eq!(strip_after("Hello World", "hello"), None);
        assert_eq!(strip_after("Hello World", "Hello"), Some(" World"));
    }

    // --- TextPair::strip_after tests ---

    #[test]
    fn text_pair_strip_after_finds_needle() {
        let original = "Destroy target creature unless you pay {2}";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let rest = tp.strip_after("unless you ").unwrap();
        assert_eq!(rest.lower, "pay {2}");
        assert_eq!(rest.original, "pay {2}");
    }

    #[test]
    fn text_pair_strip_after_preserves_original_case() {
        let original = "When This Class becomes level 3, Draw Two Cards";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let rest = tp.strip_after("becomes level ").unwrap();
        // Original case is preserved for the remainder
        assert_eq!(rest.original, "3, Draw Two Cards");
        assert_eq!(rest.lower, "3, draw two cards");
    }

    #[test]
    fn text_pair_strip_after_is_case_insensitive() {
        // TextPair::strip_after matches on the lowered text, so mixed-case originals work.
        let original = "Power And Toughness Are Each Equal To the number of cards";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let rest = tp
            .strip_after("power and toughness are each equal to ")
            .unwrap();
        assert_eq!(rest.original, "the number of cards");
    }

    #[test]
    fn text_pair_strip_after_returns_none_on_miss() {
        let original = "Gain 3 life";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        assert!(tp.strip_after("lose ").is_none());
    }

    // --- split_around (free function) tests ---

    #[test]
    fn split_around_middle() {
        assert_eq!(
            split_around("hello world foo", " world "),
            Some(("hello", "foo"))
        );
    }

    #[test]
    fn split_around_at_start() {
        assert_eq!(split_around("prefix rest", "prefix "), Some(("", "rest")));
    }

    #[test]
    fn split_around_at_end() {
        assert_eq!(split_around("hello world", "world"), Some(("hello ", "")));
    }

    #[test]
    fn split_around_not_found() {
        assert_eq!(split_around("hello world", "xyz"), None);
    }

    #[test]
    fn split_around_first_occurrence() {
        let (before, after) = split_around("a and b and c", " and ").unwrap();
        assert_eq!(before, "a");
        assert_eq!(after, "b and c");
    }

    // --- TextPair::split_around tests ---

    #[test]
    fn text_pair_split_around_preserves_case() {
        let original = "Target Creature Gets +2/+2 And Has Flying";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let (before, after) = tp.split_around(" and ").unwrap();
        assert_eq!(before.original, "Target Creature Gets +2/+2");
        assert_eq!(after.original, "Has Flying");
        assert_eq!(before.lower, "target creature gets +2/+2");
        assert_eq!(after.lower, "has flying");
    }

    #[test]
    fn text_pair_split_around_not_found() {
        let original = "Gain 3 life";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        assert!(tp.split_around(" and ").is_none());
    }

    #[test]
    fn text_pair_split_around_first_occurrence() {
        let original = "A And B And C";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let (before, after) = tp.split_around(" and ").unwrap();
        assert_eq!(before.original, "A");
        assert_eq!(after.original, "B And C");
    }

    #[test]
    fn text_pair_rsplit_around_last_occurrence() {
        let original = "A And B And C";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let (before, after) = tp.rsplit_around(" and ").unwrap();
        assert_eq!(before.original, "A And B");
        assert_eq!(after.original, "C");
    }

    #[test]
    fn text_pair_split_around_multibyte() {
        let original = "Choose one \u{2014} Effect text";
        let lower = original.to_lowercase();
        let tp = TextPair::new(original, &lower);
        let (before, after) = tp.split_around(" \u{2014} ").unwrap();
        assert_eq!(before.original, "Choose one");
        assert_eq!(after.original, "Effect text");
    }
}

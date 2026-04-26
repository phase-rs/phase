use crate::types::keywords::KeywordKind;
use serde::{Deserialize, Serialize};

/// Counter types serialize as flat strings so they can be used as JSON map keys
/// in `HashMap<CounterType, u32>`. Without this, `Generic("quest")` would serialize
/// as `{"Generic":"quest"}` which serde_json rejects as a map key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CounterType {
    Plus1Plus1,
    Minus1Minus1,
    Loyalty,
    /// CR 122.1g + CR 310.4: The number of defense counters on a battle on the
    /// battlefield indicates its defense. A battle with 0 defense is put into
    /// its owner's graveyard as a state-based action (CR 704.5v).
    Defense,
    /// CR 122.1d: When a permanent with a stun counter would become untapped during its
    /// controller's untap step, one stun counter is removed instead of untapping.
    Stun,
    /// CR 714.1: Lore counters track Saga chapter progression.
    Lore,
    /// CR 702.62a + CR 702.63a: Time counters track Suspend / Vanishing duration.
    /// One is removed at the start of the controller's upkeep; when the last is
    /// removed, the suspend "play it without paying its mana cost" trigger fires
    /// (CR 702.62a) or the Vanishing sacrifice trigger fires (CR 702.63a).
    Time,
    /// CR 122.1b: A keyword counter grants its keyword to the permanent (flying,
    /// first strike, deathtouch, lifelink, ...). Uses the parameterless
    /// `KeywordKind` discriminant — keyword counters never carry parameters
    /// (no Ward N / Afflict N / Annihilator N variants exist as counters).
    Keyword(KeywordKind),
    Generic(String),
}

/// CR 122.1b: Parameterless keyword kinds that can appear as counters, paired
/// with their canonical Oracle-text name. Single source of truth for the
/// string↔`KeywordKind` mapping at the parser/serialization boundary —
/// runtime dispatch works on the typed `CounterType::Keyword(kind)` directly.
const KEYWORD_COUNTERS: &[(&str, KeywordKind)] = &[
    ("flying", KeywordKind::Flying),
    ("first strike", KeywordKind::FirstStrike),
    ("double strike", KeywordKind::DoubleStrike),
    ("deathtouch", KeywordKind::Deathtouch),
    ("decayed", KeywordKind::Decayed),
    ("exalted", KeywordKind::Exalted),
    ("haste", KeywordKind::Haste),
    ("hexproof", KeywordKind::Hexproof),
    ("indestructible", KeywordKind::Indestructible),
    ("lifelink", KeywordKind::Lifelink),
    ("menace", KeywordKind::Menace),
    ("reach", KeywordKind::Reach),
    ("shadow", KeywordKind::Shadow),
    ("trample", KeywordKind::Trample),
    ("vigilance", KeywordKind::Vigilance),
];

impl CounterType {
    pub fn as_str(&self) -> &str {
        match self {
            CounterType::Plus1Plus1 => "P1P1",
            CounterType::Minus1Minus1 => "M1M1",
            CounterType::Loyalty => "loyalty",
            CounterType::Defense => "defense",
            CounterType::Stun => "stun",
            CounterType::Lore => "lore",
            CounterType::Time => "time",
            CounterType::Keyword(kind) => KEYWORD_COUNTERS
                .iter()
                .find(|(_, k)| k == kind)
                .map(|(name, _)| *name)
                .expect("KeywordKind stored in CounterType::Keyword must be in KEYWORD_COUNTERS"),
            CounterType::Generic(s) => s.as_str(),
        }
    }
}

impl serde::Serialize for CounterType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str())
    }
}

impl<'de> serde::Deserialize<'de> for CounterType {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let s = String::deserialize(deserializer)?;
        Ok(parse_counter_type(&s))
    }
}

/// Which counter(s) a predicate is matching against.
///
/// CR 122.1: "A counter is a marker placed on an object or player…" — some
/// Oracle text distinguishes counters by type ("a +1/+1 counter"), while
/// other text refers to counters generically ("a counter on it", meaning
/// any type). `CounterMatch::Any` captures the latter case so predicates
/// can sum across every counter type on an object, and `OfType` captures
/// the former by reusing the canonical `CounterType` enum. Prefer this over
/// `Option<CounterType>`: "Any" is a first-class matching mode rather than
/// an absence-of-specification.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CounterMatch {
    /// "a counter on it" — any counter type; predicates sum across all types.
    Any,
    /// A specific counter type, matching the canonical `CounterType` enum.
    OfType(CounterType),
}

pub fn parse_counter_type(text: &str) -> CounterType {
    let trimmed = text.trim().trim_end_matches(" counter").trim();
    match trimmed {
        "P1P1" | "+1/+1" | "plus1plus1" => CounterType::Plus1Plus1,
        "M1M1" | "-1/-1" | "minus1minus1" => CounterType::Minus1Minus1,
        "LOYALTY" | "loyalty" => CounterType::Loyalty,
        "defense" | "DEFENSE" => CounterType::Defense,
        "stun" => CounterType::Stun,
        "lore" | "LORE" => CounterType::Lore,
        "time" | "TIME" => CounterType::Time,
        other => {
            let lower = other.to_lowercase();
            if let Some((_, kind)) = KEYWORD_COUNTERS.iter().find(|(name, _)| *name == lower) {
                CounterType::Keyword(*kind)
            } else {
                // Normalize generic counter names to lowercase so that sources that
                // emit different cases (e.g. replacement parser emits "MINING", cost
                // parser emits "mining") resolve to the same HashMap key at runtime.
                CounterType::Generic(lower)
            }
        }
    }
}

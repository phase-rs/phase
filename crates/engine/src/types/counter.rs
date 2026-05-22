use crate::types::keywords::KeywordKind;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;

/// Counter types serialize as flat strings so they can be used as JSON map keys
/// in `HashMap<CounterType, u32>`. Without this, `Generic("quest")` would serialize
/// as `{"Generic":"quest"}` which serde_json rejects as a map key.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum CounterType {
    Plus1Plus1,
    Minus1Minus1,
    /// CR 122.1a + CR 613.4c: A counter that modifies power and toughness by
    /// independent deltas. `+1/+1` and `-1/-1` keep their legacy variants and
    /// serialized keys for compatibility; asymmetric legacy counters use this
    /// parameterized form instead of proliferating one-off variants.
    PowerToughness {
        power: i32,
        toughness: i32,
    },
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
    /// CR 702.24a + CR 122.1: Age counters track Cumulative Upkeep
    /// duration. Each cumulative-upkeep trigger places one at the start
    /// of its controller's upkeep, and the cost is multiplied by the
    /// total age-counter count on the permanent at resolution time
    /// (CR 702.24b).
    Age,
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
pub(crate) const KEYWORD_COUNTERS: &[(&str, KeywordKind)] = &[
    ("indestructible", KeywordKind::Indestructible),
    ("double strike", KeywordKind::DoubleStrike),
    ("first strike", KeywordKind::FirstStrike),
    ("deathtouch", KeywordKind::Deathtouch),
    ("vigilance", KeywordKind::Vigilance),
    ("hexproof", KeywordKind::Hexproof),
    ("lifelink", KeywordKind::Lifelink),
    ("decayed", KeywordKind::Decayed),
    ("exalted", KeywordKind::Exalted),
    ("trample", KeywordKind::Trample),
    ("flying", KeywordKind::Flying),
    ("menace", KeywordKind::Menace),
    ("shadow", KeywordKind::Shadow),
    ("haste", KeywordKind::Haste),
    ("reach", KeywordKind::Reach),
];

impl CounterType {
    pub fn as_str(&self) -> Cow<'_, str> {
        match self {
            CounterType::Plus1Plus1 => Cow::Borrowed("P1P1"),
            CounterType::Minus1Minus1 => Cow::Borrowed("M1M1"),
            CounterType::PowerToughness { power, toughness } => {
                Cow::Owned(format_power_toughness_counter(*power, *toughness))
            }
            CounterType::Loyalty => Cow::Borrowed("loyalty"),
            CounterType::Defense => Cow::Borrowed("defense"),
            CounterType::Stun => Cow::Borrowed("stun"),
            CounterType::Lore => Cow::Borrowed("lore"),
            CounterType::Time => Cow::Borrowed("time"),
            CounterType::Age => Cow::Borrowed("age"),
            CounterType::Keyword(kind) => KEYWORD_COUNTERS
                .iter()
                .find(|(_, k)| k == kind)
                .map(|(name, _)| *name)
                .expect("KeywordKind stored in CounterType::Keyword must be in KEYWORD_COUNTERS")
                .into(),
            CounterType::Generic(s) => Cow::Borrowed(s.as_str()),
        }
    }

    pub fn power_toughness_delta(&self) -> Option<(i32, i32)> {
        match self {
            CounterType::Plus1Plus1 => Some((1, 1)),
            CounterType::Minus1Minus1 => Some((-1, -1)),
            CounterType::PowerToughness { power, toughness } => Some((*power, *toughness)),
            CounterType::Loyalty
            | CounterType::Defense
            | CounterType::Stun
            | CounterType::Lore
            | CounterType::Time
            | CounterType::Age
            | CounterType::Keyword(_)
            | CounterType::Generic(_) => None,
        }
    }
}

impl serde::Serialize for CounterType {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.as_str().as_ref())
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

impl CounterMatch {
    /// CR 122.1: Boolean predicate — does this matcher accept a counter of
    /// the given type? `Any` accepts every type; `OfType(t)` accepts only
    /// counters of `t`. Predicates that need to *sum* counter quantities
    /// (rather than test a single type) should match on the variants
    /// directly because the `Any` case sums across all entries on an
    /// object — this helper is for the boolean axis only.
    #[inline]
    pub fn matches(&self, counter_type: &CounterType) -> bool {
        match self {
            CounterMatch::Any => true,
            CounterMatch::OfType(expected) => expected == counter_type,
        }
    }
}

pub fn parse_counter_type(text: &str) -> CounterType {
    let trimmed = text.trim().trim_end_matches(" counter").trim();
    try_parse_counter_type(trimmed).unwrap_or_else(|| CounterType::Generic(trimmed.to_lowercase()))
}

/// CR 122.1: Parse a counter *type word* only when it is genuinely recognized —
/// an explicit named type, a +N/+N parameterized type, a keyword counter, or a
/// single bare word (a custom `Generic` counter such as "charge"/"page"/"oil").
/// Returns `None` for an empty or multi-word remainder, so callers that slice
/// the type out of a larger phrase (e.g. trigger counter-placement parsing) can
/// reject leftover subject/verb text instead of manufacturing a bogus
/// `Generic("…")` filter that matches no real counter. `parse_counter_type`
/// keeps its total behavior by falling back to `Generic` for the `None` case.
pub fn try_parse_counter_type(text: &str) -> Option<CounterType> {
    let trimmed = text.trim().trim_end_matches(" counter").trim();
    if trimmed.is_empty() {
        return None;
    }
    match trimmed {
        "P1P1" | "+1/+1" | "plus1plus1" => return Some(CounterType::Plus1Plus1),
        "M1M1" | "-1/-1" | "minus1minus1" => return Some(CounterType::Minus1Minus1),
        "LOYALTY" | "loyalty" => return Some(CounterType::Loyalty),
        "defense" | "DEFENSE" => return Some(CounterType::Defense),
        "stun" => return Some(CounterType::Stun),
        "lore" | "LORE" => return Some(CounterType::Lore),
        "time" | "TIME" => return Some(CounterType::Time),
        "age" => return Some(CounterType::Age),
        _ => {}
    }
    if let Some((power, toughness)) = parse_power_toughness_counter(trimmed) {
        return Some(CounterType::PowerToughness { power, toughness });
    }
    let lower = trimmed.to_lowercase();
    if let Some((_, kind)) = KEYWORD_COUNTERS.iter().find(|(name, _)| *name == lower) {
        return Some(CounterType::Keyword(*kind));
    }
    // A bare single-word remainder is a custom counter name; a multi-word
    // remainder is leftover non-type text and is rejected.
    if lower.split_whitespace().count() == 1 {
        return Some(CounterType::Generic(lower));
    }
    None
}

/// CR 122.1: Parse the type-word slot of cost text — the word that fills the
/// `<type>` in "remove a `<type>` counter" / "remove N `<type>` counters" /
/// "remove all `<type>` counters". The bare noun (no type word, just
/// "counter"/"counters") parses to `CounterMatch::Any`, capturing the "any
/// kind on the chosen permanent" semantics that the cost field is designed
/// for. A real type word parses through `parse_counter_type` and wraps in
/// `CounterMatch::OfType`. This is the single normalization site every cost
/// parser should call when emitting `AbilityCost::RemoveCounter::counter_type`.
pub fn parse_counter_match(text: &str) -> CounterMatch {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("counter") || trimmed.eq_ignore_ascii_case("counters") {
        return CounterMatch::Any;
    }
    CounterMatch::OfType(parse_counter_type(text))
}

fn parse_power_toughness_counter(text: &str) -> Option<(i32, i32)> {
    let (power, toughness) = text.split_once('/')?;
    let power = parse_signed_counter_delta(power)?;
    let toughness = parse_signed_counter_delta(toughness)?;
    Some((power, toughness))
}

fn parse_signed_counter_delta(text: &str) -> Option<i32> {
    let text = text.trim();
    if text.len() < 2 {
        return None;
    }
    let (sign, digits) = text.split_at(1);
    let magnitude = digits.parse::<i32>().ok()?;
    match sign {
        "+" => Some(magnitude),
        "-" => Some(-magnitude),
        _ => None,
    }
}

fn format_power_toughness_counter(power: i32, toughness: i32) -> String {
    format!(
        "{}/{}",
        format_counter_delta(power, toughness),
        format_counter_delta(toughness, power)
    )
}

fn format_counter_delta(value: i32, paired_value: i32) -> String {
    if value == 0 && paired_value < 0 {
        "-0".to_string()
    } else {
        format!("{value:+}")
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_counter_type, CounterType};

    #[test]
    fn parses_legacy_power_toughness_counter_deltas() {
        assert_eq!(
            parse_counter_type("-0/-1"),
            CounterType::PowerToughness {
                power: 0,
                toughness: -1
            }
        );
        assert_eq!(
            parse_counter_type("-0/-2"),
            CounterType::PowerToughness {
                power: 0,
                toughness: -2
            }
        );
        assert_eq!(
            parse_counter_type("-1/-0"),
            CounterType::PowerToughness {
                power: -1,
                toughness: 0
            }
        );
    }

    #[test]
    fn keeps_existing_counter_key_compatibility() {
        assert_eq!(parse_counter_type("+1/+1"), CounterType::Plus1Plus1);
        assert_eq!(parse_counter_type("-1/-1"), CounterType::Minus1Minus1);
        assert_eq!(parse_counter_type("P1P1"), CounterType::Plus1Plus1);
        assert_eq!(parse_counter_type("M1M1"), CounterType::Minus1Minus1);
        assert_eq!(
            parse_counter_type("MINING"),
            CounterType::Generic("mining".to_string())
        );
    }

    #[test]
    fn serializes_parameterized_power_toughness_counter() {
        assert_eq!(
            serde_json::to_string(&CounterType::PowerToughness {
                power: 0,
                toughness: -1
            })
            .unwrap(),
            "\"-0/-1\""
        );
        assert_eq!(
            serde_json::to_string(&CounterType::PowerToughness {
                power: -1,
                toughness: 0
            })
            .unwrap(),
            "\"-1/-0\""
        );
    }

    #[test]
    fn age_counter_serializes_as_age_and_round_trips() {
        let c = CounterType::Age;
        assert_eq!(c.as_str().as_ref(), "age");
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"age\"");
        let back: CounterType = serde_json::from_str(&json).unwrap();
        assert_eq!(back, CounterType::Age);
        assert_eq!(c.power_toughness_delta(), None);
    }
}

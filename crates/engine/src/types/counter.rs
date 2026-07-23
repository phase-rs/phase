use crate::types::keywords::KeywordKind;
use serde::{Deserialize, Serialize};
use std::borrow::Cow;
use std::collections::HashMap;

/// Counter types serialize as flat strings so they can be used as JSON map keys
/// in `HashMap<CounterType, u32>`. Without this, `Generic("quest")` would serialize
/// as `{"Generic":"quest"}` which serde_json rejects as a map key.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
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
    /// CR 714.3 + CR 714.4: Lore counters track Saga chapter progression and
    /// the sacrifice state-based action at the final chapter.
    Lore,
    /// CR 702.62a + CR 702.63a: Time counters track Suspend / Vanishing duration.
    /// One is removed at the start of the controller's upkeep; when the last is
    /// removed, the suspend "play it without paying its mana cost" trigger fires
    /// (CR 702.62a) or the Vanishing sacrifice trigger fires (CR 702.63a).
    Time,
    /// CR 702.32a + CR 122.1: Fade counters track Fading duration. "Fading N"
    /// enters the permanent with N fade counters; at the beginning of its
    /// controller's upkeep one is removed, and if none can be removed the
    /// permanent is sacrificed. Distinct from `Time` (Vanishing/Suspend): a
    /// Fading permanent is sacrificed on the upkeep where it has *no* fade
    /// counter to remove (CR 702.32a), one upkeep later than a Vanishing
    /// permanent with the same number, which is sacrificed when its last time
    /// counter is removed (CR 702.63a).
    Fade,
    /// CR 702.24a + CR 122.1: Age counters track Cumulative Upkeep
    /// duration. Each cumulative-upkeep trigger places one at the start
    /// of its controller's upkeep, and the cost is multiplied by the
    /// total age-counter count on the permanent at resolution time
    /// (CR 702.24b).
    Age,
    /// CR 122.1c: A shield counter creates one replacement effect ("if this
    /// permanent would be destroyed as the result of an effect, instead remove
    /// a shield counter from it") and one prevention effect ("if damage would
    /// be dealt to this permanent, prevent that damage and remove a shield
    /// counter from it"). One or more shield counters share this single pair of
    /// effects. See `game::replacement::consume_shield_counter`.
    Shield,
    /// CR 122.1h: One or more finality counters create a single replacement
    /// effect "If this permanent would be put into a graveyard from the
    /// battlefield, exile it instead." Persistent (no counter removed, unlike
    /// shield CR 122.1c). See `game::replacement::apply_finality_counter_replacement`.
    Finality,
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
            CounterType::Fade => Cow::Borrowed("fade"),
            CounterType::Age => Cow::Borrowed("age"),
            CounterType::Shield => Cow::Borrowed("shield"),
            CounterType::Finality => Cow::Borrowed("finality"),
            CounterType::Keyword(kind) => KEYWORD_COUNTERS
                .iter()
                .find(|(_, k)| k == kind)
                .map(|(name, _)| *name)
                .expect("KeywordKind stored in CounterType::Keyword must be in KEYWORD_COUNTERS")
                .into(),
            CounterType::Generic(s) => Cow::Borrowed(s.as_str()),
        }
    }

    /// Player-facing counter name for prompts and choice descriptions, e.g.
    /// "+1/+1", "-1/-1", "first strike", "vigilance". Unlike [`as_str`], which
    /// produces serialization keys ("P1P1"/"M1M1"), this renders the P/T-shaped
    /// variants in MTG `+N/+M` display form. Non-P/T variants reuse `as_str`.
    pub fn display_phrase(&self) -> Cow<'_, str> {
        match self {
            CounterType::Plus1Plus1 => Cow::Owned(format_power_toughness_counter(1, 1)),
            CounterType::Minus1Minus1 => Cow::Owned(format_power_toughness_counter(-1, -1)),
            CounterType::PowerToughness { power, toughness } => {
                Cow::Owned(format_power_toughness_counter(*power, *toughness))
            }
            _ => self.as_str(),
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
            | CounterType::Fade
            | CounterType::Age
            | CounterType::Shield
            | CounterType::Finality
            | CounterType::Keyword(_)
            | CounterType::Generic(_) => None,
        }
    }

    /// Whether a counter kind is a *monotone* loop resource — one a beneficial
    /// loop (CR 732.2a) only ever drives in one direction within a cycle, so two
    /// cycles of the loop must compare as the same board with the counter projected
    /// out (see `analysis::resource::project_out_resources`).
    ///
    /// `true` (monotone — projected out of loop-equality):
    /// - CR 122.1a + CR 613.4c: `Plus1Plus1`, `Minus1Minus1`, `PowerToughness{..}`
    ///   modify power/toughness in Layer 7c.
    /// - CR 306.5b: `Loyalty` on a planeswalker.
    /// - CR 310.4c: `Defense` on a battle.
    ///
    /// `false` (consumable / duration / state-gating — preserved in loop-equality,
    /// because a loop that *consumes* one of these is making a real board-state
    /// change, not a monotone resource pump):
    /// - CR 122.1d: `Stun` (removed instead of untapping).
    /// - CR 122.1c: `Shield` (removed to prevent destruction/damage).
    /// - CR 702.62a / CR 702.63a: `Time` (Suspend / Vanishing duration).
    /// - CR 702.32a: `Fade` (Fading duration).
    /// - CR 702.24a: `Age` (Cumulative upkeep).
    /// - CR 714.3 + CR 714.4: `Lore` (Saga chapter progression / sacrifice SBA).
    /// - CR 122.1h: `Finality` (death→exile redirect — a state-gating
    ///   replacement, not a monotone pump).
    /// - CR 122.1b: `Keyword(_)` (grants a keyword).
    /// - CR 122.1: `Generic(_)` (arbitrary tracked markers).
    pub fn is_monotone_loop_resource(&self) -> bool {
        match self {
            CounterType::Plus1Plus1
            | CounterType::Minus1Minus1
            | CounterType::PowerToughness { .. }
            | CounterType::Loyalty
            | CounterType::Defense => true,
            CounterType::Stun
            | CounterType::Lore
            | CounterType::Time
            | CounterType::Fade
            | CounterType::Age
            | CounterType::Shield
            | CounterType::Finality
            | CounterType::Keyword(_)
            | CounterType::Generic(_) => false,
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

pub(crate) mod counter_map_serde {
    use super::*;
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub(crate) fn serialize<S>(
        map: &HashMap<CounterType, u32>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut ser_map = serializer.serialize_map(Some(map.len()))?;
        for (counter_type, count) in map {
            ser_map.serialize_entry(counter_type.as_str().as_ref(), count)?;
        }
        ser_map.end()
    }

    pub(crate) fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<CounterType, u32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct CounterMapVisitor;

        impl<'de> Visitor<'de> for CounterMapVisitor {
            type Value = HashMap<CounterType, u32>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a map of counter type keys to counts")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut map = HashMap::new();
                while let Some((counter_type, count)) = access.next_entry::<CounterType, u32>()? {
                    let total = map.entry(counter_type).or_insert(0_u32);
                    let next = (*total)
                        .checked_add(count)
                        .ok_or_else(|| de::Error::custom("counter count overflow"))?;
                    *total = next;
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(CounterMapVisitor)
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
        // Lowercase legacy keys: the pre-fix debug menu persisted these as
        // generic counters; alias them so saved states migrate to typed P/T counters.
        "P1P1" | "p1p1" | "+1/+1" | "plus1plus1" => return Some(CounterType::Plus1Plus1),
        "M1M1" | "m1m1" | "-1/-1" | "minus1minus1" => return Some(CounterType::Minus1Minus1),
        "LOYALTY" | "loyalty" => return Some(CounterType::Loyalty),
        "defense" | "DEFENSE" => return Some(CounterType::Defense),
        "stun" => return Some(CounterType::Stun),
        "lore" | "LORE" => return Some(CounterType::Lore),
        "time" | "TIME" => return Some(CounterType::Time),
        "fade" | "FADE" => return Some(CounterType::Fade),
        "age" => return Some(CounterType::Age),
        "shield" => return Some(CounterType::Shield),
        "finality" => return Some(CounterType::Finality),
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

/// CR 122.1: A counter is a marker on an object or player; an internal map
/// entry with count zero is not a marker and must not satisfy "has a counter"
/// checks (e.g. proliferate CR 701.34a).
pub fn has_positive_counters(counters: &HashMap<CounterType, u32>) -> bool {
    counters.values().any(|&count| count > 0)
}

/// Counter entries currently present on an object or LKI snapshot (count > 0 only).
pub fn positive_counter_entries(
    counters: &HashMap<CounterType, u32>,
) -> impl Iterator<Item = (&CounterType, u32)> {
    counters
        .iter()
        .filter_map(|(counter_type, &count)| (count > 0).then_some((counter_type, count)))
}

/// Counter types currently present on an object or LKI snapshot (count > 0 only).
pub fn positive_counter_types(counters: &HashMap<CounterType, u32>) -> Vec<CounterType> {
    positive_counter_entries(counters)
        .map(|(counter_type, _)| counter_type.clone())
        .collect()
}

/// CR 122.1: Drop zero-count entries so counter presence stays aligned with
/// actual markers and downstream eligibility checks.
pub fn prune_zero_counters(counters: &mut HashMap<CounterType, u32>) {
    counters.retain(|_, count| *count > 0);
}

#[cfg(test)]
mod tests {
    use super::{
        has_positive_counters, parse_counter_type, positive_counter_entries,
        positive_counter_types, prune_zero_counters, try_parse_counter_type, CounterType,
    };
    use std::collections::HashMap;

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
        assert_eq!(parse_counter_type("p1p1"), CounterType::Plus1Plus1);
        assert_eq!(parse_counter_type("M1M1"), CounterType::Minus1Minus1);
        assert_eq!(parse_counter_type("m1m1"), CounterType::Minus1Minus1);
        assert_eq!(
            parse_counter_type("MINING"),
            CounterType::Generic("mining".to_string())
        );
    }

    #[test]
    fn sums_legacy_duplicate_counter_keys_on_deserialize() {
        #[derive(serde::Deserialize)]
        struct CounterMapFixture {
            #[serde(with = "super::counter_map_serde")]
            counters: HashMap<CounterType, u32>,
        }

        let fixture: CounterMapFixture =
            serde_json::from_str(r#"{"counters":{"P1P1":2,"p1p1":3,"M1M1":1,"m1m1":4}}"#).unwrap();

        assert_eq!(fixture.counters.get(&CounterType::Plus1Plus1), Some(&5));
        assert_eq!(fixture.counters.get(&CounterType::Minus1Minus1), Some(&5));
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
    fn shield_counter_parses_serializes_and_has_no_pt_delta() {
        // CR 122.1c: "shield" is a first-class counter type, not a Generic.
        assert_eq!(parse_counter_type("shield"), CounterType::Shield);
        assert_eq!(parse_counter_type("shield counter"), CounterType::Shield);
        assert_eq!(try_parse_counter_type("shield"), Some(CounterType::Shield));
        assert_eq!(CounterType::Shield.as_str().as_ref(), "shield");
        assert_eq!(
            serde_json::to_string(&CounterType::Shield).unwrap(),
            "\"shield\""
        );
        assert_eq!(CounterType::Shield.power_toughness_delta(), None);
    }

    #[test]
    fn finality_counter_parses_serializes_and_has_no_pt_delta() {
        // CR 122.1h: "finality" is a first-class counter type, not a Generic.
        // Serialized key is byte-identical to the legacy Generic("finality").
        assert_eq!(parse_counter_type("finality"), CounterType::Finality);
        assert_eq!(
            parse_counter_type("finality counter"),
            CounterType::Finality
        );
        assert_eq!(
            try_parse_counter_type("finality"),
            Some(CounterType::Finality)
        );
        assert_eq!(CounterType::Finality.as_str().as_ref(), "finality");
        assert_eq!(
            serde_json::to_string(&CounterType::Finality).unwrap(),
            "\"finality\""
        );
        let back: CounterType = serde_json::from_str("\"finality\"").unwrap();
        assert_eq!(back, CounterType::Finality);
        assert_eq!(CounterType::Finality.power_toughness_delta(), None);
    }

    #[test]
    fn monotone_loop_resource_partition_is_exhaustive_and_correct() {
        use crate::types::keywords::KeywordKind;
        // CR 122.1a/613.4c, 306.5b, 310.4c: monotone => projected out.
        for ct in [
            CounterType::Plus1Plus1,
            CounterType::Minus1Minus1,
            CounterType::PowerToughness {
                power: 2,
                toughness: -1,
            },
            CounterType::Loyalty,
            CounterType::Defense,
        ] {
            assert!(ct.is_monotone_loop_resource(), "{ct:?} must be monotone");
        }
        // CR 122.1b/c/d, 702.62a/63a, 702.32a, 702.24a, 714.3/.4: consumable => preserved.
        for ct in [
            CounterType::Stun,
            CounterType::Lore,
            CounterType::Time,
            CounterType::Fade,
            CounterType::Age,
            CounterType::Shield,
            CounterType::Finality,
            CounterType::Keyword(KeywordKind::Flying),
            CounterType::Generic("quest".to_string()),
        ] {
            assert!(!ct.is_monotone_loop_resource(), "{ct:?} must be preserved");
        }
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

    #[test]
    fn has_positive_counters_ignores_zero_entries() {
        let mut counters = HashMap::new();
        counters.insert(CounterType::Plus1Plus1, 0);
        assert!(!has_positive_counters(&counters));
        counters.insert(CounterType::Lore, 1);
        assert!(has_positive_counters(&counters));
    }

    #[test]
    fn positive_counter_types_skips_zero_entries() {
        let mut counters = HashMap::new();
        counters.insert(CounterType::Plus1Plus1, 0);
        counters.insert(CounterType::Generic("charge".to_string()), 2);
        assert_eq!(
            positive_counter_types(&counters),
            vec![CounterType::Generic("charge".to_string())]
        );
    }

    #[test]
    fn positive_counter_entries_skips_zero_entries() {
        let mut counters = HashMap::new();
        counters.insert(CounterType::Plus1Plus1, 0);
        counters.insert(CounterType::Generic("charge".to_string()), 2);
        assert_eq!(
            positive_counter_entries(&counters)
                .map(|(counter_type, count)| (counter_type.clone(), count))
                .collect::<Vec<_>>(),
            vec![(CounterType::Generic("charge".to_string()), 2)]
        );
    }

    #[test]
    fn prune_zero_counters_drops_stale_keys() {
        let mut counters = HashMap::new();
        counters.insert(CounterType::Plus1Plus1, 0);
        counters.insert(CounterType::Stun, 1);
        prune_zero_counters(&mut counters);
        assert!(!counters.contains_key(&CounterType::Plus1Plus1));
        assert_eq!(counters.get(&CounterType::Stun), Some(&1));
    }
}

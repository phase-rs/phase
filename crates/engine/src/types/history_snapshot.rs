//! Compact serde projection for the cast-history characteristic snapshots.
//!
//! `SpellCastRecord` latches two kinds of CR 400.7 look-back facts so that a
//! history predicate ("the first spell you cast each turn that targets a
//! creature", "that mana from a Treasure was spent to cast") still answers
//! after the spell and its targets and mana sources have left the stack:
//!
//! * [`SpellCastRecord::targets`](crate::types::game_state::SpellCastRecord)
//!   carries one [`ActivationTargetFact`](crate::types::game_state::ActivationTargetFact)
//!   per declared target, whose object arm holds an [`LKISnapshot`]; and
//! * [`SpellCastRecord::mana_spent_source_snapshots`](crate::types::game_state::SpellCastRecord)
//!   carries one [`ManaSpentSourceSnapshot`](crate::types::game_state::ManaSpentSourceSnapshot)
//!   per spent mana unit, each holding the paying source's [`LKISnapshot`].
//!
//! Those records ride every per-recipient `StateUpdate`, so their wire cost is
//! paid again on each frame of a game rather than once per cast. Measured on
//! the `four_seat_scripted_round_v1` bandwidth scenario, the two fields in
//! their derived `LKISnapshot` encoding added +3.11% JSON and +1.43% wire on
//! top of a +0.24%/+1.41% baseline drift — outside the checked-in +/-1% and
//! +/-2% tolerances. The cost is not the characteristics themselves; it is the
//! repeated `"base_toughness":null,"chosen_attributes":[],"was_prepared":false`
//! scaffolding a derived serialization emits for every snapshot, times every
//! record, times every frame.
//!
//! This module therefore encodes those snapshots with a *lossless* compact
//! shape: the same twenty-one `LKISnapshot` fields, under short keys, with
//! every field whose value is its `Default` omitted and restored on the way
//! back in. Nothing the filter evaluation can read is dropped —
//! [`LKISnapshot`] is reconstructed exactly, so
//! `matches_target_filter_on_lki_snapshot` (which projects a snapshot into a
//! `ZoneChangeRecord`) sees byte-for-byte the same characteristics it would
//! from the verbose form. Each field also carries its verbose spelling as an
//! `alias`, so a payload written by the verbose shape still decodes.
//!
//! The compaction is deliberately scoped to the two history positions rather
//! than to `LKISnapshot` itself: `LKISnapshot` also serializes inside
//! `TriggerSourceContext` and `ZoneChangeRecord`, whose budgets and consumers
//! are unrelated to this change.
//!
//! `compact_lki_round_trips_every_field` is the completeness guard. It builds
//! an `LKISnapshot` naming every field without a `..Default::default()`
//! spread, so adding a field to `LKISnapshot` is a compile error here until
//! the mirror is extended — the compaction can never silently drop a
//! characteristic.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use super::ability::ChosenAttribute;
use super::card::TokenImageRef;
use super::card_type::{CoreType, Supertype};
use super::counter::{counter_map_serde, CounterType};
use super::game_state::{AttachmentSnapshot, LKISnapshot};
use super::keywords::Keyword;
use super::mana::ManaColor;
use crate::types::PlayerId;

/// The compact wire/save shape of one [`LKISnapshot`], field for field.
///
/// Every field is `rename`d to a short key and `alias`ed to its verbose
/// `LKISnapshot` spelling, so both encodings decode and only the short one is
/// emitted. `default` + `skip_serializing_if` drop values the reconstruction
/// would reproduce anyway.
#[derive(Serialize, Deserialize)]
struct CompactLki {
    #[serde(rename = "n", alias = "name")]
    name: String,
    #[serde(
        rename = "ti",
        alias = "token_image_ref",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    token_image_ref: Option<TokenImageRef>,
    #[serde(
        rename = "p",
        alias = "power",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    power: Option<i32>,
    #[serde(
        rename = "t",
        alias = "toughness",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    toughness: Option<i32>,
    /// CR 208.4b + CR 613.4b: the layer-7b base power. Omitted when it equals
    /// [`Self::power`] — the overwhelmingly common case for a printed body, and
    /// the one place a single snapshot spends two whole keys on a value the
    /// decoder can reproduce. `base_power_absent` carries the case an omission
    /// cannot: a set current value with no base value, which is exactly what a
    /// noncreature permanent animated into a creature can look like, so a
    /// `PtComparison { scope: Base }` predicate still reads the real answer.
    #[serde(
        rename = "bp",
        alias = "base_power",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    base_power: Option<i32>,
    #[serde(
        rename = "bn",
        alias = "base_power_absent",
        default,
        skip_serializing_if = "is_false"
    )]
    base_power_absent: bool,
    /// CR 208.4b + CR 613.4b: the layer-7b base toughness; see [`Self::base_power`].
    #[serde(
        rename = "bt",
        alias = "base_toughness",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    base_toughness: Option<i32>,
    #[serde(
        rename = "tn",
        alias = "base_toughness_absent",
        default,
        skip_serializing_if = "is_false"
    )]
    base_toughness_absent: bool,
    #[serde(
        rename = "mv",
        alias = "mana_value",
        default,
        skip_serializing_if = "is_zero_u32"
    )]
    mana_value: u32,
    #[serde(
        rename = "c",
        alias = "controller",
        default,
        skip_serializing_if = "is_first_player"
    )]
    controller: PlayerId,
    #[serde(
        rename = "o",
        alias = "owner",
        default,
        skip_serializing_if = "is_first_player"
    )]
    owner: PlayerId,
    #[serde(
        rename = "ct",
        alias = "card_types",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    card_types: Vec<CoreType>,
    /// Subtypes. Omitted when they are exactly the object's name — the basic-land
    /// shape, where the type line repeats the name — and reconstructed from it on
    /// the way back. A genuinely empty subtype list is written as `[]`, so the
    /// omission is unambiguous and exact.
    #[serde(
        rename = "st",
        alias = "subtypes",
        default,
        skip_serializing_if = "Option::is_none"
    )]
    subtypes: Option<Vec<String>>,
    #[serde(
        rename = "sp",
        alias = "supertypes",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    supertypes: Vec<Supertype>,
    #[serde(
        rename = "kw",
        alias = "keywords",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    keywords: Vec<Keyword>,
    #[serde(
        rename = "co",
        alias = "colors",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    colors: Vec<ManaColor>,
    #[serde(
        rename = "ca",
        alias = "chosen_attributes",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    chosen_attributes: Vec<ChosenAttribute>,
    #[serde(
        rename = "cn",
        alias = "counters",
        default,
        with = "counter_map_serde",
        skip_serializing_if = "HashMap::is_empty"
    )]
    counters: HashMap<CounterType, u32>,
    #[serde(
        rename = "tp",
        alias = "tapped",
        default,
        skip_serializing_if = "is_false"
    )]
    tapped: bool,
    #[serde(
        rename = "su",
        alias = "is_suspected",
        default,
        skip_serializing_if = "is_false"
    )]
    is_suspected: bool,
    #[serde(
        rename = "pr",
        alias = "was_prepared",
        default,
        skip_serializing_if = "is_false"
    )]
    was_prepared: bool,
    #[serde(
        rename = "at",
        alias = "attachments",
        default,
        skip_serializing_if = "Vec::is_empty"
    )]
    attachments: Vec<AttachmentSnapshot>,
    #[serde(
        rename = "in",
        alias = "intensity",
        default,
        skip_serializing_if = "is_zero_u32"
    )]
    intensity: u32,
}

fn is_false(value: &bool) -> bool {
    !*value
}

/// `None` means "exactly the object's name, so reconstruct it"; a real empty
/// list stays `Some(vec![])` so the two never collide.
fn encode_subtypes(name: &str, subtypes: &[String]) -> Option<Vec<String>> {
    if subtypes.len() == 1 && subtypes[0] == name {
        None
    } else {
        Some(subtypes.to_vec())
    }
}

/// Encode a base P/T value relative to the current one.
///
/// Returns `(value, absent_flag)`. An omitted value with no flag reconstructs
/// to the current value, which covers every equal pair (including both-absent).
/// A set current value with no base value is the one case an omission cannot
/// express, so it sets the flag instead of silently reconstructing a base.
fn encode_base(base: Option<i32>, current: Option<i32>) -> (Option<i32>, bool) {
    match base {
        Some(value) if Some(value) == current => (None, false),
        Some(value) => (Some(value), false),
        None if current.is_none() => (None, false),
        None => (None, true),
    }
}

/// `PlayerId`'s `Default` is seat 0, so omitting it reconstructs exactly. This
/// is the seat that owns most battlefield permanents in a typical game, and the
/// field costs two keys per snapshot on every frame.
fn is_first_player(player: &PlayerId) -> bool {
    *player == PlayerId::default()
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

impl CompactLki {
    fn of(lki: &LKISnapshot) -> Self {
        let (base_power, base_power_absent) = encode_base(lki.base_power, lki.power);
        let (base_toughness, base_toughness_absent) =
            encode_base(lki.base_toughness, lki.toughness);
        Self {
            name: lki.name.clone(),
            token_image_ref: lki.token_image_ref.clone(),
            power: lki.power,
            toughness: lki.toughness,
            base_power,
            base_power_absent,
            base_toughness,
            base_toughness_absent,
            mana_value: lki.mana_value,
            controller: lki.controller,
            owner: lki.owner,
            card_types: lki.card_types.clone(),
            subtypes: encode_subtypes(&lki.name, &lki.subtypes),
            supertypes: lki.supertypes.clone(),
            keywords: lki.keywords.clone(),
            colors: lki.colors.clone(),
            chosen_attributes: lki.chosen_attributes.clone(),
            counters: lki.counters.clone(),
            tapped: lki.tapped,
            is_suspected: lki.is_suspected,
            was_prepared: lki.was_prepared,
            attachments: lki.attachments.clone(),
            intensity: lki.intensity,
        }
    }

    fn into_lki(self) -> LKISnapshot {
        let subtypes = self.subtypes.unwrap_or_else(|| vec![self.name.clone()]);
        LKISnapshot {
            name: self.name,
            token_image_ref: self.token_image_ref,
            power: self.power,
            toughness: self.toughness,
            base_power: if self.base_power_absent {
                None
            } else {
                self.base_power.or(self.power)
            },
            base_toughness: if self.base_toughness_absent {
                None
            } else {
                self.base_toughness.or(self.toughness)
            },
            mana_value: self.mana_value,
            controller: self.controller,
            owner: self.owner,
            card_types: self.card_types,
            subtypes,
            supertypes: self.supertypes,
            keywords: self.keywords,
            colors: self.colors,
            chosen_attributes: self.chosen_attributes,
            counters: self.counters,
            tapped: self.tapped,
            is_suspected: self.is_suspected,
            was_prepared: self.was_prepared,
            attachments: self.attachments,
            intensity: self.intensity,
        }
    }
}

/// `#[serde(with = "compact_history_lki")]` for an owned `LKISnapshot`.
pub(crate) mod compact_history_lki {
    use super::{CompactLki, LKISnapshot};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<S: Serializer>(
        lki: &LKISnapshot,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        CompactLki::of(lki).serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<LKISnapshot, D::Error> {
        CompactLki::deserialize(deserializer).map(CompactLki::into_lki)
    }
}

/// `#[serde(with = "compact_history_boxed_lki")]` for a `Box<LKISnapshot>`.
pub(crate) mod compact_history_boxed_lki {
    use super::{CompactLki, LKISnapshot};
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    // `serde(with = ...)` requires the adapter to take exactly the field's type,
    // which is `Box<LKISnapshot>` here; dereferencing it would not satisfy the
    // generated call.
    #[allow(clippy::borrowed_box)]
    pub(crate) fn serialize<S: Serializer>(
        lki: &Box<LKISnapshot>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        CompactLki::of(lki).serialize(serializer)
    }

    pub(crate) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Box<LKISnapshot>, D::Error> {
        CompactLki::deserialize(deserializer).map(|lki| Box::new(lki.into_lki()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::AttachmentKind;
    use crate::types::identifiers::ObjectId;
    use serde::{Deserialize, Serialize};

    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
    struct CompactWrapper {
        #[serde(with = "super::compact_history_lki")]
        lki: LKISnapshot,
    }

    #[derive(Serialize, Deserialize, PartialEq, Eq, Debug)]
    struct BoxedCompactWrapper {
        #[serde(with = "super::compact_history_boxed_lki")]
        lki: Box<LKISnapshot>,
    }

    /// Every `LKISnapshot` field is named explicitly, with no
    /// `..Default::default()` spread, so a new field on `LKISnapshot` fails to
    /// compile here until `CompactLki` mirrors it.
    fn fully_populated_lki() -> LKISnapshot {
        LKISnapshot {
            name: "Test Snapshot".to_string(),
            token_image_ref: Some(TokenImageRef {
                scryfall_id: "00000000-0000-0000-0000-000000000001".to_string(),
                scryfall_oracle_id: Some("00000000-0000-0000-0000-000000000002".to_string()),
                face_name: Some("Front".to_string()),
                preset_id: "preset".to_string(),
            }),
            power: Some(3),
            toughness: Some(4),
            base_power: Some(1),
            base_toughness: Some(2),
            mana_value: 5,
            controller: PlayerId(2),
            owner: PlayerId(1),
            card_types: vec![CoreType::Creature, CoreType::Artifact],
            subtypes: vec!["Treasure".to_string(), "Bear".to_string()],
            supertypes: vec![Supertype::Legendary],
            keywords: vec![Keyword::Flying],
            colors: vec![ManaColor::Green, ManaColor::White],
            chosen_attributes: vec![ChosenAttribute::Color(ManaColor::Blue)],
            counters: HashMap::from([(CounterType::Plus1Plus1, 2)]),
            tapped: true,
            is_suspected: true,
            was_prepared: true,
            attachments: vec![AttachmentSnapshot {
                object_id: ObjectId(7),
                identity: None,
                controller: PlayerId(3),
                kind: AttachmentKind::Aura,
            }],
            intensity: 9,
        }
    }

    #[test]
    fn compact_lki_round_trips_every_field() {
        let original = CompactWrapper {
            lki: fully_populated_lki(),
        };
        let json = serde_json::to_string(&original).expect("compact serialization succeeds");
        let decoded: CompactWrapper = serde_json::from_str(&json).expect("compact payload decodes");
        assert_eq!(
            decoded, original,
            "every LKISnapshot field must survive the compact codec unchanged"
        );

        let boxed = BoxedCompactWrapper {
            lki: Box::new(fully_populated_lki()),
        };
        let boxed_json = serde_json::to_string(&boxed).expect("compact boxed serialization");
        let boxed_decoded: BoxedCompactWrapper =
            serde_json::from_str(&boxed_json).expect("compact boxed payload decodes");
        assert_eq!(boxed_decoded, boxed);
    }

    /// CR 208.4b: a base P/T equal to the current P/T is omitted and
    /// reconstructed from it, and an explicitly absent base still round-trips.
    #[test]
    fn compact_lki_omits_only_an_equal_base_pt() {
        let mut equal = fully_populated_lki();
        equal.power = Some(2);
        equal.toughness = Some(2);
        equal.base_power = Some(2);
        equal.base_toughness = Some(2);
        let payload = serde_json::to_string(&CompactWrapper { lki: equal.clone() })
            .expect("serialization succeeds");
        assert!(
            !payload.contains("\"bp\":") && !payload.contains("\"bt\":"),
            "an equal base P/T must not be spelled out: {payload}"
        );
        let decoded: CompactWrapper = serde_json::from_str(&payload).expect("decodes");
        assert_eq!(decoded.lki, equal);

        let mut absent = fully_populated_lki();
        absent.power = Some(2);
        absent.toughness = Some(2);
        absent.base_power = None;
        absent.base_toughness = None;
        let payload = serde_json::to_string(&CompactWrapper {
            lki: absent.clone(),
        })
        .expect("serialization succeeds");
        assert!(
            payload.contains("\"bn\":true") && payload.contains("\"tn\":true"),
            "an absent base with a set current P/T must be flagged: {payload}"
        );
        let decoded: CompactWrapper = serde_json::from_str(&payload).expect("decodes");
        assert_eq!(decoded.lki, absent);
    }

    /// The verbose `LKISnapshot` spelling stays decodable through the aliases,
    /// so a payload written before this compaction still restores.
    #[test]
    fn compact_lki_accepts_the_verbose_spelling() {
        let verbose_lki = fully_populated_lki();
        let verbose = serde_json::to_string(&CompactWrapper {
            lki: verbose_lki.clone(),
        })
        .expect("serialization succeeds");
        // Re-encode through the verbose `LKISnapshot` shape and decode it with
        // the compact codec: every `alias` must accept the long field name.
        let verbose_payload = format!(
            "{{\"lki\":{}}}",
            serde_json::to_string(&serde_json::to_value(&verbose_lki).expect("to_value"))
                .expect("to_string")
        );
        let _ = verbose;
        let decoded: CompactWrapper =
            serde_json::from_str(&verbose_payload).expect("verbose spelling decodes");
        assert_eq!(decoded.lki, verbose_lki);
    }

    /// The compact form must be materially smaller than the verbose derived
    /// form for the shapes that dominate cast history.
    #[test]
    fn compact_lki_is_smaller_than_the_verbose_spelling() {
        let lki = fully_populated_lki();
        let compact = serde_json::to_string(&CompactWrapper { lki: lki.clone() }).expect("compact");
        let verbose = serde_json::to_string(&lki).expect("verbose");
        assert!(
            compact.len() < verbose.len(),
            "compact encoding ({}) should be smaller than the verbose one ({})",
            compact.len(),
            verbose.len()
        );

        let plain = LKISnapshot {
            name: "Plains".to_string(),
            token_image_ref: None,
            power: None,
            toughness: None,
            base_power: None,
            base_toughness: None,
            mana_value: 0,
            controller: PlayerId(0),
            owner: PlayerId(0),
            card_types: vec![CoreType::Land],
            subtypes: vec!["Plains".to_string()],
            supertypes: vec![],
            keywords: vec![],
            colors: vec![],
            chosen_attributes: vec![],
            counters: HashMap::new(),
            tapped: false,
            is_suspected: false,
            was_prepared: false,
            attachments: vec![],
            intensity: 0,
        };
        let compact_plain =
            serde_json::to_string(&CompactWrapper { lki: plain.clone() }).expect("compact");
        let verbose_plain = serde_json::to_string(&plain).expect("verbose");
        assert!(
            compact_plain.len() * 2 < verbose_plain.len(),
            "a typical mana source must shrink by more than half: compact {} vs verbose {}",
            compact_plain.len(),
            verbose_plain.len()
        );
    }
}

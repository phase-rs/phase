//! Display-only printing annotations a deck submission carries (the printings sidecar).
//!
//! CR 203.1 + CR 206.1: a card's illustration and expansion symbol have no effect on
//! game play, so a printing is display data and no rules path reads these types.

use std::fmt;
use std::marker::PhantomData;

use serde::de::{DeserializeOwned, DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::{Map, Number, Value};

use crate::types::card::CardFace;

/// One printing of a card: its set code and collector number.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CardPrinting {
    pub set_code: String,
    pub collector_number: String,
}

/// The deck-list field a printing annotation names: one variant per card-section
/// field of `DeckData` (sticker sheets are not cards), serialized as that field's name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeckSection {
    MainDeck,
    Sideboard,
    Commander,
    Companion,
    AttractionDeck,
    PlanarDeck,
    SchemeDeck,
    ContraptionDeck,
    SignatureSpell,
    /// A section name this build does not know (a newer peer's). Decoded so the
    /// annotation cannot fail its deck; never produced by this build.
    #[serde(other)]
    Unknown,
}

/// The owner's explicit printing for `count` copies of `name` in `section`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckPrinting {
    pub section: DeckSection,
    pub name: String,
    pub count: u32,
    pub printing: CardPrinting,
}

/// Resource bound against abuse, not a validity rule: the most elements one deck
/// list's printings sidecar may carry. One sidecar annotates at most every card copy
/// of its list, and the server's name-list ceilings sum to 1 412 copies per list
/// (pinned by a const assertion beside those ceilings in `lobby-broker`'s
/// `inbound_guard.rs`), so 2 048, the next power of two, admits a deck at every
/// ceiling with a distinct printing on every copy. Per deck list, not per frame.
pub const MAX_DECK_PRINTING_ENTRIES: usize = 2048;

/// Resource bound against abuse, not a validity rule: the most bytes of a card name
/// anywhere in a submitted deck list. The name lists' guard and the printings
/// sidecar's bound read this one definition. (The longest card name in the card data
/// is 141 bytes.)
pub const MAX_DECK_CARD_NAME_LEN: usize = 256;

/// Resource bound against abuse, not a validity rule: the most bytes of a printing's
/// set code (the longest published set code is 6 bytes).
pub const MAX_PRINTING_SET_CODE_LEN: usize = 32;

/// Resource bound against abuse, not a validity rule: the most bytes of a printing's
/// collector number (the longest published collector number is 10 bytes).
pub const MAX_PRINTING_COLLECTOR_NUMBER_LEN: usize = 32;

/// The wording of the entry-cap refusal (the same wording `validate_deck_list` uses),
/// so the decoder and the typed check report one message for one constant.
fn too_many_printings(field: &str, max: usize) -> String {
    format!("{field} must contain at most {max} entries")
}

/// The single decoder for a deck list's `printings` field. Each element is read
/// whole and then decoded on its own; one that does not decode is dropped, and a
/// value that is not a sequence decodes as no annotations, so, within
/// `MAX_DECK_PRINTING_ENTRIES` elements, an annotation the codec can read as a value
/// never fails the deck that carries it. More than `MAX_DECK_PRINTING_ENTRIES`
/// elements of any shape fail the decode (resource abuse, A3); the element past the
/// cap is skipped unread, never materialised by this decoder (a codec that buffered
/// the frame first has already read it). An element the
/// codec itself cannot read (a JSON number outside the finite f64 range; on the
/// wasm path a `Set` or an out-of-range BigInt) is the codec's frame error, exactly
/// as the same value in any other field would be; it is propagated, never swallowed,
/// because a streaming deserializer that failed inside an element cannot resume.
pub fn deserialize_deck_printings<'de, D>(deserializer: D) -> Result<Vec<DeckPrinting>, D::Error>
where
    D: Deserializer<'de>,
{
    deserializer.deserialize_any(LenientElements::<DeckPrinting> {
        max_elements: MAX_DECK_PRINTING_ENTRIES,
        field: "printings",
        elements: PhantomData,
    })
}

/// Resource bounds on a deck list's printings sidecar: at most
/// `MAX_DECK_PRINTING_ENTRIES` entries and byte caps on its three strings. Only abuse is
/// refused; an empty, control-character, unknown-section, zero-count or unknown-name
/// annotation is a validity problem that normalization drops and is never an error here.
/// The single authority for every ingress: the server guard, P2P admission and WASM
/// initialize. `field` names the sidecar in the error (e.g. `deck.printings`).
pub fn validate_deck_printings_bounds(
    field: &str,
    printings: &[DeckPrinting],
) -> Result<(), String> {
    if printings.len() > MAX_DECK_PRINTING_ENTRIES {
        return Err(too_many_printings(field, MAX_DECK_PRINTING_ENTRIES));
    }
    for (index, entry) in printings.iter().enumerate() {
        // Destructured without `..`: a string field added to either struct is a
        // compile error here rather than an unbounded field.
        let DeckPrinting {
            section: _,
            name,
            count: _,
            printing:
                CardPrinting {
                    set_code,
                    collector_number,
                },
        } = entry;
        if name.len() > MAX_DECK_CARD_NAME_LEN {
            return Err(format!(
                "{field}[{index}].name must be at most {MAX_DECK_CARD_NAME_LEN} bytes"
            ));
        }
        if set_code.len() > MAX_PRINTING_SET_CODE_LEN {
            return Err(format!(
                "{field}[{index}].printing.set_code must be at most {MAX_PRINTING_SET_CODE_LEN} bytes"
            ));
        }
        if collector_number.len() > MAX_PRINTING_COLLECTOR_NUMBER_LEN {
            return Err(format!(
                "{field}[{index}].printing.collector_number must be at most {MAX_PRINTING_COLLECTOR_NUMBER_LEN} bytes"
            ));
        }
    }
    Ok(())
}

/// Reads each element whole through `AnyElement`, so a malformed element leaves the
/// stream intact, then decodes it into `T` from memory, dropping failures. Reading
/// stops, with an error, at the first element past `max_elements`.
struct LenientElements<T> {
    max_elements: usize,
    field: &'static str,
    elements: PhantomData<fn() -> T>,
}

impl<'de, T: DeserializeOwned> Visitor<'de> for LenientElements<T> {
    type Value = Vec<T>;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("a sequence of annotations")
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Vec<T>, A::Error> {
        let mut kept = Vec::new();
        // Every element read counts toward the cap, kept or dropped, so a frame of
        // malformed elements cannot pass the bound by being dropped first.
        for _ in 0..self.max_elements {
            // `?` here is the codec failing to read the element at all: propagated.
            let Some(raw) = seq.next_element_seed(AnyElement {
                depth_left: ELEMENT_DEPTH,
            })?
            else {
                return Ok(kept);
            };
            // A malformed annotation is dropped, never an error.
            if let Ok(value) = T::deserialize(raw) {
                kept.push(value);
            }
        }
        // At the cap, one more element of any shape is abuse: it is skipped unread by
        // `IgnoredAny` (never materialised by this decoder) and fails the decode,
        // which rejects the frame.
        if seq.next_element::<IgnoredAny>()?.is_some() {
            return Err(serde::de::Error::custom(too_many_printings(
                self.field,
                self.max_elements,
            )));
        }
        Ok(kept)
    }

    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Vec<T>, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_bool<E: serde::de::Error>(self, _: bool) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_i64<E: serde::de::Error>(self, _: i64) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_u64<E: serde::de::Error>(self, _: u64) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_f64<E: serde::de::Error>(self, _: f64) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_str<E: serde::de::Error>(self, _: &str) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_bytes<E: serde::de::Error>(self, _: &[u8]) -> Result<Vec<T>, E> {
        Ok(Vec::new())
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Vec<T>, A::Error> {
        while map.next_entry::<IgnoredAny, IgnoredAny>()?.is_some() {}
        Ok(Vec::new())
    }
}

/// How many levels of one element are read. Every type this module decodes has its
/// deepest field two levels below the element, so any cap of 3 or more never changes
/// whether an element decodes. Levels below the cap are skipped unread with
/// `IgnoredAny` (serde_json skips iteratively, serde-wasm-bindgen without traversal),
/// which keeps this decoder's own recursion far under serde_json's 128-level limit.
/// This is not a resource bound: it never rejects anything.
const ELEMENT_DEPTH: u8 = 16;

/// Materialises one element as a `serde_json::Value` through the codec's
/// `deserialize_any`, from any self-describing codec. Unlike `Value`'s own
/// `Deserialize`, it accepts byte buffers and non-string map keys (both read and
/// dropped) and never treats an object key specially (`Value` reinterprets
/// serde_json's private raw-value token when the `raw_value` feature is on, which
/// `lobby-broker` enables in every server build).
#[derive(Clone, Copy)]
struct AnyElement {
    depth_left: u8,
}

impl AnyElement {
    fn child(self) -> Self {
        // Only called from a visitor, which runs only when `depth_left > 0`.
        Self {
            depth_left: self.depth_left - 1,
        }
    }
}

impl<'de> DeserializeSeed<'de> for AnyElement {
    type Value = Value;

    fn deserialize<D: Deserializer<'de>>(self, deserializer: D) -> Result<Value, D::Error> {
        if self.depth_left == 0 {
            IgnoredAny::deserialize(deserializer)?;
            return Ok(Value::Null);
        }
        deserializer.deserialize_any(self)
    }
}

impl<'de> Visitor<'de> for AnyElement {
    type Value = Value;

    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("any value")
    }

    fn visit_bool<E: serde::de::Error>(self, v: bool) -> Result<Value, E> {
        Ok(Value::Bool(v))
    }
    fn visit_i64<E: serde::de::Error>(self, v: i64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_u64<E: serde::de::Error>(self, v: u64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_i128<E: serde::de::Error>(self, v: i128) -> Result<Value, E> {
        Ok(Number::from_i128(v).map_or(Value::Null, Value::Number))
    }
    fn visit_u128<E: serde::de::Error>(self, v: u128) -> Result<Value, E> {
        Ok(Number::from_u128(v).map_or(Value::Null, Value::Number))
    }
    // A non-finite float (a JS `Infinity` or `NaN`) becomes `Null`.
    fn visit_f64<E: serde::de::Error>(self, v: f64) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_str<E: serde::de::Error>(self, v: &str) -> Result<Value, E> {
        Ok(Value::from(v))
    }
    fn visit_string<E: serde::de::Error>(self, v: String) -> Result<Value, E> {
        Ok(Value::String(v))
    }
    // Bytes (a JS Uint8Array/ArrayBuffer) have no JSON form and no annotation field holds them.
    fn visit_bytes<E: serde::de::Error>(self, _: &[u8]) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_unit<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_none<E: serde::de::Error>(self) -> Result<Value, E> {
        Ok(Value::Null)
    }
    fn visit_some<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
    fn visit_newtype_struct<D: Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }

    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let mut items = Vec::new();
        while let Some(item) = seq.next_element_seed(self.child())? {
            items.push(item);
        }
        Ok(Value::Array(items))
    }

    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        let mut entries = Map::new();
        while let Some(key) = map.next_key_seed(self.child())? {
            let value = map.next_value_seed(self.child())?;
            // A non-string key (a JS `Map` with number keys) names no annotation field.
            if let Value::String(key) = key {
                entries.insert(key, value);
            }
        }
        Ok(Value::Object(entries))
    }
}

/// The identity `round_trip_name` resolves a face by: its printed name plus its Scryfall
/// oracle id when one is recorded. The single face-identity authority for the retained
/// printing record and for counting copies of a face in a pool list.
pub fn deck_face_identity(face: &CardFace) -> (&str, Option<&str>) {
    (face.name.as_str(), face.scryfall_oracle_id.as_deref())
}

/// Index-free form of `DeckEntryLookupName`: which round-trip lookup name `section`'s rows
/// use for a face. The entry index a binding carries is invalidated by command-zone
/// netting, zero-count pruning and companion removal; the face identity is not. The two
/// types also live at different times: the index form in a payload, this one for a game.
/// So this is a separate type rather than a parameter of the payload binding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckFaceLookupName {
    pub section: DeckSection,
    pub face_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle_id: Option<String>,
    /// The normalized (round-trip) name the section's rows carry for this face.
    pub name: String,
}

impl DeckFaceLookupName {
    pub fn of(section: DeckSection, face: &CardFace, name: String) -> Self {
        let (face_name, oracle_id) = deck_face_identity(face);
        Self {
            section,
            face_name: face_name.to_string(),
            oracle_id: oracle_id.map(str::to_string),
            name,
        }
    }

    /// True when this entry names `face` in `section` (identity, never a raw name compare).
    pub fn names(&self, section: DeckSection, face: &CardFace) -> bool {
        self.section == section
            && deck_face_identity(face) == (self.face_name.as_str(), self.oracle_id.as_deref())
    }
}

/// CR 203.1 + CR 206.1: a seat's owner-chosen printings, retained beside its deck pool so
/// an object created from a pool entry after load (a companion put into hand, a sideboard
/// card brought in from outside the game) can carry the owner's printing. Display-only:
/// no rules or AI path reads it. Never written after load: fields are private, the pool
/// holds it behind an `Arc`, and only visibility redaction replaces it (in filtered copies).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeckPrintingRecord {
    /// The loaded payload's normalized rows, unchanged (round-trip names, original order).
    rows: Vec<DeckPrinting>,
    /// One entry per (section, face identity) the payload bound.
    lookup_names: Vec<DeckFaceLookupName>,
}

impl DeckPrintingRecord {
    pub(crate) fn new(rows: Vec<DeckPrinting>, lookup_names: Vec<DeckFaceLookupName>) -> Self {
        Self { rows, lookup_names }
    }

    pub fn rows(&self) -> &[DeckPrinting] {
        &self.rows
    }

    /// The round-trip name `section`'s rows use for `face`.
    pub fn lookup_name(&self, section: DeckSection, face: &CardFace) -> Option<&str> {
        self.lookup_names
            .iter()
            .find(|entry| entry.names(section, face))
            .map(|entry| entry.name.as_str())
    }

    /// The printing of copy `ordinal` (0-based) of `face` in `section`, counting that face's
    /// rows in stream order (the order `entry_copies` consumes them at load). Stateless.
    pub fn printing_of(
        &self,
        section: DeckSection,
        face: &CardFace,
        ordinal: u32,
    ) -> Option<&CardPrinting> {
        let name = self.lookup_name(section, face)?;
        let mut skip = ordinal;
        // Both names are normalized round-trip names (the row's by `normalize_deck_printings`,
        // the binding's by `round_trip_name`), never a raw face-name compare.
        self.rows
            .iter()
            .filter(|row| row.section == section && row.name == name)
            .find_map(|row| {
                if skip < row.count {
                    Some(&row.printing)
                } else {
                    skip -= row.count;
                    None
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::starter_decks::DeckData;

    const CAP_MESSAGE: &str = "printings must contain at most 2048 entries";

    /// A native-shape frame: a `#[serde(flatten)]` envelope over an adjacently tagged
    /// enum, which serde buffers as `Content` before the field decoder runs.
    #[derive(Deserialize)]
    #[serde(tag = "type", content = "data")]
    enum Frame {
        Create { deck: DeckData },
    }

    #[derive(Deserialize)]
    struct Envelope {
        #[serde(flatten)]
        message: Frame,
    }

    const VALID_ELEMENT: &str = r#"{"section":"main_deck","name":"Plains","count":1,"printing":{"set_code":"M21","collector_number":"1"}}"#;

    fn deck_json(elements: &[&str]) -> String {
        format!(r#"{{"main_deck":[],"printings":[{}]}}"#, elements.join(","))
    }

    fn repeated(element: &'static str, count: usize) -> Vec<&'static str> {
        vec![element; count]
    }

    /// Decodes `deck` (a `DeckData` JSON object) through every codec shape the
    /// production frames take, returning each result.
    fn decode_every_shape(deck: &str) -> Vec<(&'static str, Result<DeckData, String>)> {
        let streaming = serde_json::from_str::<DeckData>(deck).map_err(|e| e.to_string());
        let tag_first = serde_json::from_str::<Frame>(&format!(
            r#"{{"type":"Create","data":{{"deck":{deck}}}}}"#
        ))
        .map(|Frame::Create { deck }| deck)
        .map_err(|e| e.to_string());
        let content_first = serde_json::from_str::<Frame>(&format!(
            r#"{{"data":{{"deck":{deck}}},"type":"Create"}}"#
        ))
        .map(|Frame::Create { deck }| deck)
        .map_err(|e| e.to_string());
        let flatten = serde_json::from_str::<Envelope>(&format!(
            r#"{{"type":"Create","data":{{"deck":{deck}}}}}"#
        ))
        .map(|Envelope { message }| {
            let Frame::Create { deck } = message;
            deck
        })
        .map_err(|e| e.to_string());
        let from_value = serde_json::from_str::<Value>(deck)
            .map_err(|e| e.to_string())
            .and_then(|value| serde_json::from_value::<DeckData>(value).map_err(|e| e.to_string()));
        vec![
            ("streaming", streaming),
            ("tag-first", tag_first),
            ("content-first", content_first),
            ("flatten", flatten),
            ("from_value", from_value),
        ]
    }

    /// P2-V1: 2049 elements fail in every shape; 2048 decode and are kept.
    #[test]
    fn decode_refuses_more_than_the_cap_in_every_codec_shape() {
        // Reach guard: at the cap the decoder ran and kept every valid element.
        let at_cap = deck_json(&repeated(VALID_ELEMENT, MAX_DECK_PRINTING_ENTRIES));
        for (shape, result) in decode_every_shape(&at_cap) {
            let deck = result.unwrap_or_else(|e| panic!("{shape}: {e}"));
            assert_eq!(deck.printings.len(), MAX_DECK_PRINTING_ENTRIES, "{shape}");
        }

        // One past the cap: valid elements.
        let over_valid = deck_json(&repeated(VALID_ELEMENT, MAX_DECK_PRINTING_ENTRIES + 1));
        for (shape, result) in decode_every_shape(&over_valid) {
            let err = result.expect_err(shape);
            assert!(err.contains(CAP_MESSAGE), "{shape}: {err}");
        }

        // One past the cap: elements the lenient decoder would drop. The cap counts
        // raw elements before they are dropped.
        let over_malformed = deck_json(&repeated("0", MAX_DECK_PRINTING_ENTRIES + 1));
        for (shape, result) in decode_every_shape(&over_malformed) {
            let err = result.expect_err(shape);
            assert!(err.contains(CAP_MESSAGE), "{shape}: {err}");
        }
    }

    /// P2-V1 boundary and non-sequence siblings: at the cap, malformed elements are
    /// dropped without failing the deck; a non-sequence `printings` decodes empty.
    #[test]
    fn decode_admits_the_cap_of_malformed_elements_and_non_sequences() {
        let at_cap_malformed = deck_json(&repeated("0", MAX_DECK_PRINTING_ENTRIES));
        for (shape, result) in decode_every_shape(&at_cap_malformed) {
            let deck = result.unwrap_or_else(|e| panic!("{shape}: {e}"));
            assert!(deck.printings.is_empty(), "{shape}");
        }
        for non_sequence in ["null", "{}", r#"{"a":[1,2,3]}"#, "7", r#""text""#] {
            let deck = format!(r#"{{"main_deck":[],"printings":{non_sequence}}}"#);
            for (shape, result) in decode_every_shape(&deck) {
                let deck = result.unwrap_or_else(|e| panic!("{shape} {non_sequence}: {e}"));
                assert!(deck.printings.is_empty(), "{shape} {non_sequence}");
            }
        }
    }

    /// P2-V1: the element past the cap is skipped unread by `IgnoredAny`, never
    /// materialised by `AnyElement`. On a streaming codec a number the codec cannot
    /// read (`1e400`) at position 2049 therefore yields the cap error rather than
    /// the codec's own `number out of range`. Streaming shapes only: the buffered
    /// shapes read the frame into serde `Content` before the decoder runs.
    #[test]
    fn element_past_the_cap_is_skipped_unread_on_streaming_codecs() {
        let mut elements = repeated("0", MAX_DECK_PRINTING_ENTRIES);
        elements.push("1e400");
        let deck = deck_json(&elements);
        let results = decode_every_shape(&deck);
        for (shape, result) in &results {
            let err = result.as_ref().expect_err(shape);
            if matches!(*shape, "streaming" | "tag-first") {
                assert!(err.contains(CAP_MESSAGE), "{shape}: {err}");
            } else {
                assert!(err.contains("number out of range"), "{shape}: {err}");
            }
        }

        // Reach guard: the same unreadable number INSIDE the cap is the codec's
        // frame error (propagated, never swallowed), so the sentinel above is
        // discriminating.
        let mut inside = repeated("0", 3);
        inside.push("1e400");
        let (_, streaming) = decode_every_shape(&deck_json(&inside))
            .into_iter()
            .next()
            .expect("streaming shape first");
        let err = streaming.expect_err("1e400 inside the cap");
        assert!(err.contains("number out of range"), "{err}");
    }

    fn entry(name: &str, set_code: &str, collector_number: &str) -> DeckPrinting {
        DeckPrinting {
            section: DeckSection::MainDeck,
            name: name.to_string(),
            count: 1,
            printing: CardPrinting {
                set_code: set_code.to_string(),
                collector_number: collector_number.to_string(),
            },
        }
    }

    /// P2-V2: the entry cap on typed values.
    #[test]
    fn bounds_refuse_more_than_the_cap_of_entries() {
        let at_cap = vec![entry("Plains", "M21", "1"); MAX_DECK_PRINTING_ENTRIES];
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &at_cap),
            Ok(())
        );
        let over = vec![entry("Plains", "M21", "1"); MAX_DECK_PRINTING_ENTRIES + 1];
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &over),
            Err("x.printings must contain at most 2048 entries".to_string())
        );
    }

    /// P2-V2: byte caps on each of the three strings, at and one past the cap.
    #[test]
    fn bounds_refuse_oversized_strings_by_bytes() {
        let name_at = "a".repeat(MAX_DECK_CARD_NAME_LEN);
        let code_at = "s".repeat(MAX_PRINTING_SET_CODE_LEN);
        let number_at = "1".repeat(MAX_PRINTING_COLLECTOR_NUMBER_LEN);
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &[entry(&name_at, &code_at, &number_at)]),
            Ok(())
        );

        let long_name = "a".repeat(MAX_DECK_CARD_NAME_LEN + 1);
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &[entry(&long_name, "M21", "1")]),
            Err("x.printings[0].name must be at most 256 bytes".to_string())
        );
        let long_code = "s".repeat(MAX_PRINTING_SET_CODE_LEN + 1);
        assert_eq!(
            validate_deck_printings_bounds(
                "x.printings",
                &[
                    entry("Plains", "M21", "1"),
                    entry("Plains", &long_code, "1")
                ]
            ),
            Err("x.printings[1].printing.set_code must be at most 32 bytes".to_string())
        );
        let long_number = "1".repeat(MAX_PRINTING_COLLECTOR_NUMBER_LEN + 1);
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &[entry("Plains", "M21", &long_number)]),
            Err("x.printings[0].printing.collector_number must be at most 32 bytes".to_string())
        );

        // Bytes, not chars: 128 two-byte characters sit at the name cap, 129 are over.
        let multibyte_at = "é".repeat(MAX_DECK_CARD_NAME_LEN / 2);
        assert_eq!(multibyte_at.len(), MAX_DECK_CARD_NAME_LEN);
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &[entry(&multibyte_at, "M21", "1")]),
            Ok(())
        );
        let multibyte_over = "é".repeat(MAX_DECK_CARD_NAME_LEN / 2 + 1);
        assert!(validate_deck_printings_bounds(
            "x.printings",
            &[entry(&multibyte_over, "M21", "1")]
        )
        .is_err());
    }

    /// P2-V2 / R1: validity-only problems are never refused by the resource bound.
    #[test]
    fn bounds_admit_invalid_but_in_bounds_annotations() {
        let mut unknown_section = entry("Plains", "M21", "1");
        unknown_section.section = DeckSection::Unknown;
        let mut zero_count = entry("Plains", "M21", "1");
        zero_count.count = 0;
        let annotations = [
            entry("", "", ""),
            entry("Bad\nName", "M\u{0}21", "1\t"),
            entry("No Such Card Anywhere", "ZZZ", "999"),
            unknown_section,
            zero_count,
        ];
        assert_eq!(
            validate_deck_printings_bounds("x.printings", &annotations),
            Ok(())
        );
    }
}

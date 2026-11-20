//! Display-only values: data a client shows that has no rules meaning.
//!
//! CR 203.1 + CR 206.1: a card's illustration and expansion symbol have no effect on
//! game play. CR 104.4b: loop detection compares game states, so such data must never
//! make two otherwise identical states differ. Three mechanisms keep a [`DisplayOnly`]
//! value out of every rules and AI identity: typed equality (always equal), the rules
//! projection ([`rules_projection`]), and constant `Debug`. The same pass reports whether it
//! omitted a non-empty display-only value ([`reported_rules_projection`]).
//! Canonical prompt identities use [`canonical_digest`] on the delivered rules projection:
//! recursively sorted JSON object keys, preserved array order, and serde_json scalar bytes.

use std::fmt;
use std::hash::{Hash, Hasher};

use serde::ser::{
    Error as _, Serialize, SerializeMap, SerializeSeq, SerializeStruct, SerializeStructVariant,
    SerializeTuple, SerializeTupleStruct, SerializeTupleVariant, Serializer,
};
use serde::{Deserialize, Deserializer};
use serde_json::value::Serializer as ValueSerializer;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

/// Opaque SHA-256 identity of canonical JSON bytes, serialized as lowercase hex.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize, Deserialize)]
#[serde(transparent)]
pub struct CanonicalDigest(String);

impl CanonicalDigest {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Encode JSON independently of map iteration order, preserving array order.
pub fn canonical_json_bytes(value: &Value) -> Vec<u8> {
    fn write(value: &Value, bytes: &mut Vec<u8>) {
        match value {
            Value::Object(entries) => {
                bytes.push(b'{');
                let mut keys: Vec<_> = entries.keys().collect();
                keys.sort_unstable();
                for (index, key) in keys.into_iter().enumerate() {
                    if index != 0 {
                        bytes.push(b',');
                    }
                    serde_json::to_writer(&mut *bytes, key)
                        .expect("serializing a JSON string into Vec<u8> cannot fail");
                    bytes.push(b':');
                    write(&entries[key], bytes);
                }
                bytes.push(b'}');
            }
            Value::Array(entries) => {
                bytes.push(b'[');
                for (index, entry) in entries.iter().enumerate() {
                    if index != 0 {
                        bytes.push(b',');
                    }
                    write(entry, bytes);
                }
                bytes.push(b']');
            }
            Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {
                serde_json::to_writer(bytes, value)
                    .expect("serializing a JSON scalar into Vec<u8> cannot fail");
            }
        }
    }
    let mut bytes = Vec::new();
    write(value, &mut bytes);
    bytes
}

/// Hash exactly the UTF-8 bytes returned by [`canonical_json_bytes`].
pub fn canonical_digest(value: &Value) -> CanonicalDigest {
    CanonicalDigest(format!("{:x}", Sha256::digest(canonical_json_bytes(value))))
}

/// The serde newtype name the rules projection recognizes: a reserved serialization name,
/// not a Rust identifier, so no type should be given it (a `#[serde(rename)]` could, and a
/// repository grep keeps it to this one definition). Every wire codec renders a named
/// newtype as its inner value, so the name never reaches a serialized form.
const DISPLAY_ONLY_MARKER: &str = "$phase_engine::private::DisplayOnly";

/// A value shown to players that no rule and no AI decision may read.
///
/// Placement rule: a carrier holds `DisplayOnly<Option<T>>` (the whole optional inside the
/// wrapper), never `Option<DisplayOnly<T>>`, because an outer `Option` is visible to derived
/// equality and to the rules projection. Each carrier field is
/// `#[serde(default, skip_serializing_if = "DisplayOnly::is_none")]`, and the transparent
/// serde form preserves only the inner shape, so an empty carrier is absent from the wire.
/// A carrier is a named struct or struct-variant field only: never `#[serde(flatten)]`, never
/// a map value, a sequence or tuple element, an enum newtype payload or the root. A map
/// value's entry is dropped by the projection, so map membership would become invisible;
/// serde's flatten and internally tagged serializers forward the inner value without the
/// newtype name, so a flattened or internally tagged payload `DisplayOnly` is projected in
/// full.
///
/// Reads are explicit through [`DisplayOnly::get`]; there is deliberately no `Deref`, so a
/// predicate that reads display data is visible to review and to grep.
#[derive(Clone, Copy, Default)]
pub struct DisplayOnly<T>(T);

impl<T> DisplayOnly<T> {
    pub const fn new(value: T) -> Self {
        Self(value)
    }

    pub fn get(&self) -> &T {
        &self.0
    }

    pub fn into_inner(self) -> T {
        self.0
    }
}

impl<T> DisplayOnly<Option<T>> {
    /// The absent-when-empty predicate for `skip_serializing_if` on a carrier field.
    pub fn is_none(&self) -> bool {
        self.0.is_none()
    }
}

/// Always equal: display data never takes part in rules-state equality (CR 104.4b).
impl<T> PartialEq for DisplayOnly<T> {
    fn eq(&self, _other: &Self) -> bool {
        true
    }
}

impl<T> Eq for DisplayOnly<T> {}

/// Constant hash, consistent with the always-true equality.
impl<T> Hash for DisplayOnly<T> {
    fn hash<H: Hasher>(&self, _state: &mut H) {}
}

/// Constant text in both `{:?}` and `{:#?}`, so no Debug-keyed identity or golden text
/// depends on the display data.
impl<T> fmt::Debug for DisplayOnly<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("DisplayOnly(..)")
    }
}

/// Wire-transparent: every codec renders the named newtype as its inner value, and the
/// name is the marker the rules projection recognizes.
impl<T: Serialize> Serialize for DisplayOnly<T> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_newtype_struct(DISPLAY_ONLY_MARKER, &self.0)
    }
}

/// Reads the inner shape; the marker is only needed to write.
impl<'de, T: Deserialize<'de>> Deserialize<'de> for DisplayOnly<T> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        T::deserialize(deserializer).map(Self)
    }
}

/// The serialization the rules and the AI may compare or hash: `value`'s ordinary serde_json
/// serialization with every [`DisplayOnly`] value omitted, recognized by its marker on the
/// typed value (never by a key name; no consumer may strip already-serialized JSON).
///
/// A keyed position (struct or struct-variant field, map entry) drops the entry, so a
/// present and an absent value project identically; a positional one (sequence or tuple
/// element, externally tagged newtype-variant payload, the root) takes `null`. Outside the
/// placement rule the projection is not sound: a `DisplayOnly` map value loses map
/// membership, and a flattened or internally tagged payload `DisplayOnly` reaches the
/// projection in full.
///
/// Equal to `serde_json::to_value(value)` for any value holding no `DisplayOnly` whose
/// `Serialize` impls use only serde's public data model; serde_json's private encodings
/// (`RawValue` and the `arbitrary_precision` number token, recognized by private struct
/// names) are not supported and are not reproduced. Sound only for trees whose `Serialize`
/// impls keep the marker: a custom `Serialize` that re-serializes through
/// `serde_json::Value` beneath a projected root erases it.
pub fn rules_projection<T: Serialize + ?Sized>(value: &T) -> serde_json::Result<Value> {
    project(value, &mut IgnoreOmissions).map(Projected::or_null)
}

/// What a rules projection omitted. A [`DisplayOnly`] value is non-empty when its ordinary
/// rules projection is not `null` (for the placement rule's `DisplayOnly<Option<T>>`, a
/// `Some`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum OmittedDisplayData {
    /// Nothing, or only empty display-only values, was omitted.
    #[default]
    Nothing,
    /// At least one non-empty display-only value was omitted.
    NonEmpty,
}

/// A rules projection together with what it omitted.
#[derive(Clone, Debug, PartialEq)]
pub struct ReportedRulesProjection {
    /// Equal to [`rules_projection`] of the same value.
    pub value: Value,
    pub omitted: OmittedDisplayData,
}

/// [`rules_projection`] plus a report of whether the same pass omitted a non-empty
/// [`DisplayOnly`] value, so a caller can tell a display-bearing value from a display-free
/// one without a second serialization.
///
/// CR 203.1 + CR 206.1: the reported mode may project an omitted payload solely to decide
/// whether it was non-empty; the payload never reaches `value`. Unlike [`rules_projection`],
/// which never serializes an omitted payload, this fails when an omitted payload's own
/// `Serialize` fails, but only for payloads inspected before the report becomes
/// `NonEmpty`: the observer stops inspecting once it has reported non-empty data, so a
/// tuple holding a non-empty `DisplayOnly` followed by a failing one succeeds, and the
/// reverse order fails.
pub fn reported_rules_projection<T: Serialize + ?Sized>(
    value: &T,
) -> serde_json::Result<ReportedRulesProjection> {
    let mut omitted = OmittedDisplayData::Nothing;
    let value = project(value, &mut omitted)?.or_null();
    Ok(ReportedRulesProjection { value, omitted })
}

/// What the projector does with a display-only value it omits.
trait OmissionObserver {
    fn observe<T: Serialize + ?Sized>(&mut self, inner: &T) -> serde_json::Result<()>;
}

/// The ordinary mode: omit immediately; the omitted payload is never serialized.
struct IgnoreOmissions;

impl OmissionObserver for IgnoreOmissions {
    fn observe<T: Serialize + ?Sized>(&mut self, _inner: &T) -> serde_json::Result<()> {
        Ok(())
    }
}

/// The reported mode: becomes `NonEmpty` once an omitted value's own projection is not `null`.
impl OmissionObserver for OmittedDisplayData {
    fn observe<T: Serialize + ?Sized>(&mut self, inner: &T) -> serde_json::Result<()> {
        if *self == Self::NonEmpty {
            return Ok(());
        }
        if let Projected::Kept(projected) = project(inner, &mut IgnoreOmissions)? {
            if !projected.is_null() {
                *self = Self::NonEmpty;
            }
        }
        Ok(())
    }
}

/// The projection of one serialized position: omitted, or kept as a JSON value.
enum Projected {
    Omitted,
    Kept(Value),
}

impl Projected {
    /// A positional position cannot drop an entry, and the length must stay a rules fact,
    /// so an omitted value is the constant `null` there.
    fn or_null(self) -> Value {
        match self {
            Self::Omitted => Value::Null,
            Self::Kept(value) => value,
        }
    }
}

fn project<T: Serialize + ?Sized, O: OmissionObserver>(
    value: &T,
    observer: &mut O,
) -> serde_json::Result<Projected> {
    value.serialize(RulesProjector { observer })
}

/// serde_json's own spelling of a map key, obtained from its value serializer so integer,
/// bool, char and string keys and the "key must be a string" error are not reimplemented.
fn json_map_key<K: Serialize + ?Sized>(key: &K) -> serde_json::Result<String> {
    let mut probe = ValueSerializer.serialize_map(Some(1))?;
    SerializeMap::serialize_key(&mut probe, key)?;
    SerializeMap::serialize_value(&mut probe, &())?;
    match SerializeMap::end(probe)? {
        Value::Object(entries) => entries
            .into_iter()
            .next()
            .map(|(key, _)| key)
            .ok_or_else(|| serde_json::Error::custom("map key serialized to no entry")),
        _ => Err(serde_json::Error::custom(
            "map key serialized to a non-object",
        )),
    }
}

/// Forwards scalar methods to serde_json's value serializer, so number, float, char and
/// string encoding is serde_json's by construction.
macro_rules! forward_scalars {
    ($($method:ident($ty:ty)),* $(,)?) => {
        $(
            fn $method(self, value: $ty) -> serde_json::Result<Projected> {
                ValueSerializer.$method(value).map(Projected::Kept)
            }
        )*
    };
}

/// A `serde::Serializer` that builds the same `Value` as serde_json's own, except that a
/// `DisplayOnly` newtype is omitted.
struct RulesProjector<'r, O> {
    observer: &'r mut O,
}

impl<'r, O: OmissionObserver> Serializer for RulesProjector<'r, O> {
    type Ok = Projected;
    type Error = serde_json::Error;
    type SerializeSeq = ProjectedSeq<'r, O>;
    type SerializeTuple = ProjectedSeq<'r, O>;
    type SerializeTupleStruct = ProjectedSeq<'r, O>;
    type SerializeTupleVariant = ProjectedTupleVariant<'r, O>;
    type SerializeMap = ProjectedMap<'r, O>;
    type SerializeStruct = ProjectedMap<'r, O>;
    type SerializeStructVariant = ProjectedStructVariant<'r, O>;

    forward_scalars! {
        serialize_bool(bool),
        serialize_i8(i8),
        serialize_i16(i16),
        serialize_i32(i32),
        serialize_i64(i64),
        serialize_i128(i128),
        serialize_u8(u8),
        serialize_u16(u16),
        serialize_u32(u32),
        serialize_u64(u64),
        serialize_u128(u128),
        serialize_f32(f32),
        serialize_f64(f64),
        serialize_char(char),
        serialize_str(&str),
        serialize_bytes(&[u8]),
        serialize_unit_struct(&'static str),
    }

    fn serialize_none(self) -> serde_json::Result<Projected> {
        ValueSerializer.serialize_none().map(Projected::Kept)
    }

    fn serialize_unit(self) -> serde_json::Result<Projected> {
        ValueSerializer.serialize_unit().map(Projected::Kept)
    }

    fn serialize_unit_variant(
        self,
        name: &'static str,
        variant_index: u32,
        variant: &'static str,
    ) -> serde_json::Result<Projected> {
        ValueSerializer
            .serialize_unit_variant(name, variant_index, variant)
            .map(Projected::Kept)
    }

    fn serialize_some<T: Serialize + ?Sized>(self, value: &T) -> serde_json::Result<Projected> {
        value.serialize(self)
    }

    fn serialize_newtype_struct<T: Serialize + ?Sized>(
        self,
        name: &'static str,
        value: &T,
    ) -> serde_json::Result<Projected> {
        if name == DISPLAY_ONLY_MARKER {
            // CR 203.1 + CR 206.1: ordinary mode never projects the omitted payload;
            // reported mode may project it solely to determine whether non-empty data
            // was omitted.
            self.observer.observe(value)?;
            return Ok(Projected::Omitted);
        }
        value.serialize(self)
    }

    fn serialize_newtype_variant<T: Serialize + ?Sized>(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        value: &T,
    ) -> serde_json::Result<Projected> {
        let mut entries = Map::new();
        entries.insert(variant.to_owned(), project(value, self.observer)?.or_null());
        Ok(Projected::Kept(Value::Object(entries)))
    }

    fn serialize_seq(self, len: Option<usize>) -> serde_json::Result<ProjectedSeq<'r, O>> {
        Ok(ProjectedSeq {
            observer: self.observer,
            elements: Vec::with_capacity(len.unwrap_or(0)),
        })
    }

    fn serialize_tuple(self, len: usize) -> serde_json::Result<ProjectedSeq<'r, O>> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_struct(
        self,
        _name: &'static str,
        len: usize,
    ) -> serde_json::Result<ProjectedSeq<'r, O>> {
        self.serialize_seq(Some(len))
    }

    fn serialize_tuple_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        len: usize,
    ) -> serde_json::Result<ProjectedTupleVariant<'r, O>> {
        Ok(ProjectedTupleVariant {
            observer: self.observer,
            variant,
            elements: Vec::with_capacity(len),
        })
    }

    fn serialize_map(self, _len: Option<usize>) -> serde_json::Result<ProjectedMap<'r, O>> {
        Ok(ProjectedMap {
            observer: self.observer,
            entries: Map::new(),
            pending_key: None,
        })
    }

    fn serialize_struct(
        self,
        _name: &'static str,
        _len: usize,
    ) -> serde_json::Result<ProjectedMap<'r, O>> {
        self.serialize_map(None)
    }

    fn serialize_struct_variant(
        self,
        _name: &'static str,
        _variant_index: u32,
        variant: &'static str,
        _len: usize,
    ) -> serde_json::Result<ProjectedStructVariant<'r, O>> {
        Ok(ProjectedStructVariant {
            observer: self.observer,
            variant,
            fields: Map::new(),
        })
    }
}

/// Sequence, tuple and tuple-struct elements: positional, so an omitted element is `null`.
struct ProjectedSeq<'r, O> {
    observer: &'r mut O,
    elements: Vec<Value>,
}

impl<O: OmissionObserver> SerializeSeq for ProjectedSeq<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
        self.elements
            .push(project(value, &mut *self.observer)?.or_null());
        Ok(())
    }

    fn end(self) -> serde_json::Result<Projected> {
        Ok(Projected::Kept(Value::Array(self.elements)))
    }
}

impl<O: OmissionObserver> SerializeTuple for ProjectedSeq<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_element<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> serde_json::Result<Projected> {
        SerializeSeq::end(self)
    }
}

impl<O: OmissionObserver> SerializeTupleStruct for ProjectedSeq<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
        SerializeSeq::serialize_element(self, value)
    }

    fn end(self) -> serde_json::Result<Projected> {
        SerializeSeq::end(self)
    }
}

/// Tuple-variant fields: positional, wrapped as `{variant: [..]}`.
struct ProjectedTupleVariant<'r, O> {
    observer: &'r mut O,
    variant: &'static str,
    elements: Vec<Value>,
}

impl<O: OmissionObserver> SerializeTupleVariant for ProjectedTupleVariant<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_field<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
        self.elements
            .push(project(value, &mut *self.observer)?.or_null());
        Ok(())
    }

    fn end(self) -> serde_json::Result<Projected> {
        let mut entries = Map::new();
        entries.insert(self.variant.to_owned(), Value::Array(self.elements));
        Ok(Projected::Kept(Value::Object(entries)))
    }
}

/// Map entries and struct fields: keyed, so an omitted value drops its entry. Map entries
/// are keyed because serde delivers every field of a struct with a flattened field through
/// `SerializeMap`.
struct ProjectedMap<'r, O> {
    observer: &'r mut O,
    entries: Map<String, Value>,
    pending_key: Option<String>,
}

impl<O: OmissionObserver> SerializeMap for ProjectedMap<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_key<T: Serialize + ?Sized>(&mut self, key: &T) -> serde_json::Result<()> {
        self.pending_key = Some(json_map_key(key)?);
        Ok(())
    }

    fn serialize_value<T: Serialize + ?Sized>(&mut self, value: &T) -> serde_json::Result<()> {
        let key = self
            .pending_key
            .take()
            .ok_or_else(|| serde_json::Error::custom("map value serialized before its key"))?;
        if let Projected::Kept(value) = project(value, &mut *self.observer)? {
            self.entries.insert(key, value);
        }
        Ok(())
    }

    fn end(self) -> serde_json::Result<Projected> {
        Ok(Projected::Kept(Value::Object(self.entries)))
    }
}

impl<O: OmissionObserver> SerializeStruct for ProjectedMap<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> serde_json::Result<()> {
        if let Projected::Kept(value) = project(value, &mut *self.observer)? {
            self.entries.insert(key.to_owned(), value);
        }
        Ok(())
    }

    fn end(self) -> serde_json::Result<Projected> {
        Ok(Projected::Kept(Value::Object(self.entries)))
    }
}

/// Struct-variant fields: keyed, wrapped as `{variant: {..}}`.
struct ProjectedStructVariant<'r, O> {
    observer: &'r mut O,
    variant: &'static str,
    fields: Map<String, Value>,
}

impl<O: OmissionObserver> SerializeStructVariant for ProjectedStructVariant<'_, O> {
    type Ok = Projected;
    type Error = serde_json::Error;

    fn serialize_field<T: Serialize + ?Sized>(
        &mut self,
        key: &'static str,
        value: &T,
    ) -> serde_json::Result<()> {
        if let Projected::Kept(value) = project(value, &mut *self.observer)? {
            self.fields.insert(key.to_owned(), value);
        }
        Ok(())
    }

    fn end(self) -> serde_json::Result<Projected> {
        let mut entries = Map::new();
        entries.insert(self.variant.to_owned(), Value::Object(self.fields));
        Ok(Projected::Kept(Value::Object(entries)))
    }
}

#[cfg(test)]
mod tests {
    use std::collections::hash_map::DefaultHasher;
    use std::collections::{BTreeMap, HashMap};

    use serde::Serialize;
    use serde_json::json;

    use super::*;

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Mark {
        set_code: String,
        collector_number: String,
    }

    /// A carrier-shaped struct: the placement rule's field form.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Probe {
        name: String,
        power: i32,
        #[serde(default, skip_serializing_if = "DisplayOnly::is_none")]
        shown: DisplayOnly<Option<Mark>>,
    }

    /// The same shape with a plain optional, the wire twin of `Probe`.
    #[derive(Serialize)]
    struct PlainProbe {
        name: String,
        power: i32,
        #[serde(skip_serializing_if = "Option::is_none")]
        shown: Option<Mark>,
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    enum External {
        Unit,
        Newtype(Probe),
        Tuple(Probe, u8),
        Struct { probe: Probe, n: u8 },
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "kind")]
    enum Internal {
        Struct { probe: Probe, n: u8 },
        Newtype(Probe),
        Wrapped(External),
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(tag = "t", content = "c")]
    enum Adjacent {
        Newtype(Probe),
        Struct { probe: Probe },
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    #[serde(untagged)]
    enum Untagged {
        Number(u64),
        Probe(Probe),
    }

    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Flat {
        id: u64,
        #[serde(flatten)]
        inner: Probe,
    }

    /// One `Probe` at every position the placement rule permits a carrier to reach.
    #[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
    struct Tree {
        field: Probe,
        seq: Vec<Probe>,
        by_id: HashMap<u64, Probe>,
        by_name: BTreeMap<String, Probe>,
        opt: Option<Probe>,
        tuple: (u8, Probe),
        external: Vec<External>,
        internal: Vec<Internal>,
        adjacent: Vec<Adjacent>,
        untagged: Vec<Untagged>,
        flat: Flat,
    }

    /// Number of distinct `Probe` positions in `tree`.
    const POSITIONS: usize = 17;

    fn sentinel(position: usize) -> String {
        format!("SENTINEL{position:02}")
    }

    fn probe(position: usize, present: bool) -> Probe {
        Probe {
            name: "Bear".to_string(),
            power: 2,
            shown: DisplayOnly::new(present.then(|| Mark {
                set_code: sentinel(position),
                collector_number: "7".to_string(),
            })),
        }
    }

    fn tree(present: bool) -> Tree {
        let at = |position| probe(position, present);
        Tree {
            field: at(0),
            seq: vec![at(1)],
            by_id: HashMap::from([(7, at(2))]),
            by_name: BTreeMap::from([("x".to_string(), at(3))]),
            opt: Some(at(4)),
            tuple: (5, at(5)),
            external: vec![
                External::Unit,
                External::Newtype(at(6)),
                External::Tuple(at(7), 3),
                External::Struct { probe: at(8), n: 4 },
            ],
            internal: vec![
                Internal::Struct { probe: at(9), n: 1 },
                Internal::Newtype(at(10)),
                Internal::Wrapped(External::Struct {
                    probe: at(11),
                    n: 9,
                }),
                Internal::Wrapped(External::Tuple(at(12), 9)),
            ],
            adjacent: vec![
                Adjacent::Newtype(at(13)),
                Adjacent::Struct { probe: at(14) },
            ],
            untagged: vec![Untagged::Number(3), Untagged::Probe(at(15))],
            flat: Flat {
                id: 1,
                inner: at(16),
            },
        }
    }

    #[test]
    fn rules_projection_omits_display_only_at_every_nesting_position() {
        let present = tree(true);
        let absent = tree(false);

        // Reach: every position really held a present value, and the wire form carries it
        // (the type is wire-transparent), exactly once per position.
        let wire = serde_json::to_string(&present).unwrap();
        for position in 0..POSITIONS {
            assert_eq!(
                wire.matches(&sentinel(position)).count(),
                1,
                "position {position} must carry its printing on the wire"
            );
        }
        assert_ne!(
            serde_json::to_value(&present).unwrap(),
            serde_json::to_value(&absent).unwrap(),
            "the ordinary serialization sees the display-only value"
        );

        let projected_present = rules_projection(&present).unwrap();
        let projected_absent = rules_projection(&absent).unwrap();
        assert_eq!(projected_present, projected_absent);
        assert_eq!(
            projected_present,
            serde_json::to_value(&absent).unwrap(),
            "the projection of a present tree is the ordinary serialization of the absent one"
        );
        let text = serde_json::to_string(&projected_present).unwrap();
        assert!(!text.contains("SENTINEL"));
        assert!(!text.contains("\"shown\""));

        // Positive controls: a rules difference at a keyed map value, and at an internally
        // tagged payload nested in an externally tagged variant, still changes the projection.
        let mut changed = tree(true);
        changed.by_id.get_mut(&7).unwrap().power = 3;
        assert_ne!(rules_projection(&changed).unwrap(), projected_present);
        let mut changed = tree(true);
        if let Internal::Wrapped(External::Struct { probe, .. }) = &mut changed.internal[2] {
            probe.name = "Elf".to_string();
        } else {
            panic!("fixture position 11 must be the wrapped struct variant");
        }
        assert_ne!(rules_projection(&changed).unwrap(), projected_present);
    }

    #[derive(Serialize)]
    struct UnitStruct;

    #[derive(Serialize)]
    struct TupleStruct(u8, String);

    /// Serializes through `serialize_bytes`, which no derive reaches.
    struct Bytes;

    impl Serialize for Bytes {
        fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
            serializer.serialize_bytes(&[1, 2, 3])
        }
    }

    #[derive(Serialize)]
    struct Scalars {
        boolean: bool,
        small: i8,
        medium: i32,
        large: i64,
        huge: i128,
        unsigned: u64,
        unsigned_huge: u128,
        single: f32,
        double: f64,
        exponent: f64,
        character: char,
        text: String,
        bytes: Bytes,
        absent: Option<u8>,
        present: Option<u8>,
        unit: (),
        unit_struct: UnitStruct,
        tuple_struct: TupleStruct,
        tuple: (u8, String),
        bool_keys: BTreeMap<bool, u8>,
        char_keys: BTreeMap<char, u8>,
        int_keys: BTreeMap<i32, u8>,
        unsigned_keys: HashMap<u64, u8>,
        string_keys: BTreeMap<String, u8>,
        tree: Tree,
    }

    #[test]
    fn rules_projection_equals_to_value_without_display_only() {
        let scalars = Scalars {
            boolean: true,
            small: -8,
            medium: -3,
            large: i64::MIN,
            huge: i128::from(i64::MIN),
            unsigned: u64::MAX,
            unsigned_huge: u128::from(u64::MAX),
            single: 0.1,
            double: 0.1,
            exponent: 1e21,
            character: 'z',
            text: "text".to_string(),
            bytes: Bytes,
            absent: None,
            present: Some(5),
            unit: (),
            unit_struct: UnitStruct,
            tuple_struct: TupleStruct(1, "a".to_string()),
            tuple: (2, "b".to_string()),
            bool_keys: BTreeMap::from([(true, 1), (false, 0)]),
            char_keys: BTreeMap::from([('a', 1)]),
            int_keys: BTreeMap::from([(-3, 1), (40, 2)]),
            unsigned_keys: HashMap::from([(7, 1), (9_000, 2)]),
            string_keys: BTreeMap::from([("k".to_string(), 1)]),
            tree: tree(false),
        };

        let expected = serde_json::to_value(&scalars).unwrap();
        let projected = rules_projection(&scalars).unwrap();
        assert_eq!(projected, expected);
        let text = serde_json::to_string(&projected).unwrap();
        assert_eq!(text, serde_json::to_string(&expected).unwrap());
        // Reach: the tree was large and exercised the keyed and positional forms.
        assert!(text.len() > 500, "fixture must be nontrivial: {text}");
        assert!(text.contains("\"kind\":\"Wrapped\""));
        assert!(text.contains("\"Tuple\":["));
    }

    #[test]
    fn rules_projection_reports_non_string_map_keys_like_serde_json() {
        let keyed = HashMap::from([((1u8, 2u8), 3u8)]);
        let ordinary = serde_json::to_value(&keyed).unwrap_err();
        let projected = rules_projection(&keyed).unwrap_err();
        assert_eq!(projected.to_string(), ordinary.to_string());
        assert!(projected.to_string().contains("key must be a string"));
    }

    #[derive(Serialize)]
    struct OuterOption {
        outer: Option<DisplayOnly<u8>>,
    }

    #[derive(Serialize)]
    enum Positional {
        Payload(DisplayOnly<u8>),
        Pair(DisplayOnly<u8>, u8),
    }

    #[derive(Serialize)]
    struct PositionalStruct(DisplayOnly<u8>, u8);

    #[derive(Serialize)]
    struct Inner {
        a: u8,
    }

    #[derive(Serialize)]
    struct Flattened {
        x: u8,
        #[serde(flatten)]
        p: DisplayOnly<Inner>,
    }

    #[test]
    fn rules_projection_nulls_display_only_at_positional_positions() {
        let one = DisplayOnly::new(1u8);
        assert_eq!(rules_projection(&vec![one]).unwrap(), json!([null]));
        assert_eq!(rules_projection(&(one, 2u8)).unwrap(), json!([null, 2]));
        assert_eq!(
            rules_projection(&PositionalStruct(one, 2)).unwrap(),
            json!([null, 2])
        );
        assert_eq!(
            rules_projection(&Positional::Payload(one)).unwrap(),
            json!({"Payload": null})
        );
        assert_eq!(
            rules_projection(&Positional::Pair(one, 2)).unwrap(),
            json!({"Pair": [null, 2]})
        );
        assert_eq!(rules_projection(&one).unwrap(), Value::Null);
        // A map entry is keyed: its value is dropped, so map membership is lost.
        assert_eq!(
            rules_projection(&HashMap::from([(1u8, one)])).unwrap(),
            json!({})
        );
        // Reach: the ordinary serialization does see the value at the same positions.
        assert_eq!(serde_json::to_value(vec![one]).unwrap(), json!([1]));
        assert_eq!(
            serde_json::to_value(HashMap::from([(1u8, one)])).unwrap(),
            json!({"1": 1})
        );
    }

    #[test]
    fn outer_option_wrapper_is_visible_to_the_projection() {
        let some = OuterOption {
            outer: Some(DisplayOnly::new(1)),
        };
        let none = OuterOption { outer: None };
        assert_eq!(rules_projection(&some).unwrap(), json!({}));
        assert_eq!(rules_projection(&none).unwrap(), json!({"outer": null}));
    }

    #[test]
    fn outer_option_wrapper_collides_at_positional_positions() {
        let some = Some(DisplayOnly::new(1u8));
        let none = None::<DisplayOnly<u8>>;
        // Reach: the wire texts differ, so the collision below is the projection's doing.
        assert_eq!(serde_json::to_string(&vec![some]).unwrap(), "[1]");
        assert_eq!(serde_json::to_string(&vec![none]).unwrap(), "[null]");
        assert_eq!(rules_projection(&vec![some]).unwrap(), json!([null]));
        assert_eq!(rules_projection(&vec![none]).unwrap(), json!([null]));
        assert_eq!(rules_projection(&some).unwrap(), Value::Null);
        assert_eq!(rules_projection(&none).unwrap(), Value::Null);
    }

    #[test]
    fn flattened_display_only_is_visible_to_the_projection() {
        let flattened = Flattened {
            x: 1,
            p: DisplayOnly::new(Inner { a: 9 }),
        };
        assert_eq!(
            rules_projection(&flattened).unwrap(),
            json!({"a": 9, "x": 1})
        );
    }

    #[test]
    fn display_only_serializes_as_its_inner_value() {
        for present in [true, false] {
            let mark = present.then(|| Mark {
                set_code: sentinel(0),
                collector_number: "7".to_string(),
            });
            let carrier = Probe {
                name: "Bear".to_string(),
                power: 2,
                shown: DisplayOnly::new(mark.clone()),
            };
            let twin = PlainProbe {
                name: "Bear".to_string(),
                power: 2,
                shown: mark,
            };
            let text = serde_json::to_string(&carrier).unwrap();
            assert_eq!(text, serde_json::to_string(&twin).unwrap());
            assert_eq!(text.contains(&sentinel(0)), present);
            assert_eq!(text.contains("\"shown\""), present);
        }
    }

    #[test]
    fn display_only_deserializes_transparently() {
        let present = tree(true);
        let wire = serde_json::to_value(&present).unwrap();

        // From text and from a value; HashMap order never matters to a `Value`.
        let from_text: Tree = serde_json::from_str(&serde_json::to_string(&present).unwrap())
            .expect("a present tree decodes from text");
        let from_value: Tree =
            serde_json::from_value(wire.clone()).expect("a present tree decodes from a value");
        for decoded in [&from_text, &from_value] {
            assert_eq!(serde_json::to_value(decoded).unwrap(), wire);
            assert_eq!(
                decoded.field.shown.get().as_ref().map(|m| &m.set_code),
                Some(&sentinel(0))
            );
            // Content-buffered decoders: internally tagged, untagged and flattened.
            assert!(matches!(
                &decoded.internal[1],
                Internal::Newtype(p) if p.shown.get().is_some()
            ));
            assert!(matches!(
                &decoded.untagged[1],
                Untagged::Probe(p) if p.shown.get().is_some()
            ));
            assert!(decoded.flat.inner.shown.get().is_some());
        }

        // Legacy shape: a JSON object without the key decodes to the empty carrier.
        let legacy: Probe = serde_json::from_str(r#"{"name":"Bear","power":2}"#).unwrap();
        assert!(legacy.shown.get().is_none());
    }

    /// A carrier field without `skip_serializing_if`: an empty carrier still reaches the
    /// marker, with `null`.
    #[derive(Serialize)]
    struct NoSkip {
        shown: DisplayOnly<Option<Mark>>,
    }

    #[derive(Serialize)]
    struct Wrapper {
        inner: DisplayOnly<Option<Mark>>,
    }

    /// A payload whose own `Serialize` always fails.
    struct Failing;

    impl Serialize for Failing {
        fn serialize<S: Serializer>(&self, _serializer: S) -> Result<S::Ok, S::Error> {
            Err(<S::Error as serde::ser::Error>::custom("failing payload"))
        }
    }

    fn mark(position: usize) -> Mark {
        Mark {
            set_code: sentinel(position),
            collector_number: "7".to_string(),
        }
    }

    fn reported<T: Serialize + ?Sized>(value: &T) -> OmittedDisplayData {
        reported_rules_projection(value)
            .expect("a serializable value projects")
            .omitted
    }

    #[test]
    fn reported_rules_projection_reports_non_empty_display_data() {
        // The 17-position tree: present -> NonEmpty, absent -> Nothing, and the value is the
        // ordinary projection in both.
        for (present, expected) in [
            (true, OmittedDisplayData::NonEmpty),
            (false, OmittedDisplayData::Nothing),
        ] {
            let tree = tree(present);
            let report = reported_rules_projection(&tree).unwrap();
            assert_eq!(report.omitted, expected, "tree({present})");
            assert_eq!(report.value, rules_projection(&tree).unwrap());
        }
        // Reach: the present tree really carries its 17 printings on the wire.
        let wire = serde_json::to_string(&tree(true)).unwrap();
        assert_eq!(wire.matches("SENTINEL").count(), POSITIONS);

        // An empty carrier without `skip_serializing_if` reaches the marker with `null`.
        let empty = NoSkip {
            shown: DisplayOnly::new(None),
        };
        let filled = NoSkip {
            shown: DisplayOnly::new(Some(mark(0))),
        };
        assert_eq!(
            serde_json::to_value(&empty).unwrap(),
            json!({"shown": null}),
            "reach: the marker is reached with null"
        );
        assert_eq!(reported(&empty), OmittedDisplayData::Nothing);
        assert_eq!(reported(&filled), OmittedDisplayData::NonEmpty);

        // Positional position and a nested carrier whose own projection is non-null.
        assert_eq!(
            reported(&vec![DisplayOnly::new(1u8)]),
            OmittedDisplayData::NonEmpty
        );
        assert_eq!(
            reported(&DisplayOnly::new(Some(Wrapper {
                inner: DisplayOnly::new(None),
            }))),
            OmittedDisplayData::NonEmpty
        );

        // A value holding no `DisplayOnly` omits nothing, and the value is `to_value`.
        let plain = PlainProbe {
            name: "Bear".to_string(),
            power: 2,
            shown: Some(mark(0)),
        };
        let report = reported_rules_projection(&plain).unwrap();
        assert_eq!(report.omitted, OmittedDisplayData::Nothing);
        assert_eq!(report.value, serde_json::to_value(&plain).unwrap());
    }

    #[test]
    fn reported_rules_projection_value_is_the_ordinary_projection() {
        let scalars = Scalars {
            boolean: true,
            small: -8,
            medium: -3,
            large: i64::MIN,
            huge: i128::from(i64::MIN),
            unsigned: u64::MAX,
            unsigned_huge: u128::from(u64::MAX),
            single: 0.1,
            double: 0.1,
            exponent: 1e21,
            character: 'z',
            text: "text".to_string(),
            bytes: Bytes,
            absent: None,
            present: Some(5),
            unit: (),
            unit_struct: UnitStruct,
            tuple_struct: TupleStruct(1, "a".to_string()),
            tuple: (2, "b".to_string()),
            bool_keys: BTreeMap::from([(true, 1), (false, 0)]),
            char_keys: BTreeMap::from([('a', 1)]),
            int_keys: BTreeMap::from([(-3, 1), (40, 2)]),
            unsigned_keys: HashMap::from([(7, 1), (9_000, 2)]),
            string_keys: BTreeMap::from([("k".to_string(), 1)]),
            tree: tree(false),
        };
        let report = reported_rules_projection(&scalars).unwrap();
        assert_eq!(report.value, rules_projection(&scalars).unwrap());
        assert_eq!(report.omitted, OmittedDisplayData::Nothing);
    }

    /// One carrier at each serializer branch that threads the observer.
    #[derive(Serialize)]
    enum Branch {
        Payload(DisplayOnly<Option<Mark>>),
        Pair(DisplayOnly<Option<Mark>>, u8),
        Fields { shown: DisplayOnly<Option<Mark>> },
    }

    #[derive(Serialize)]
    struct TupleStructBranch(DisplayOnly<Option<Mark>>, u8);

    fn carrier(filled: bool) -> DisplayOnly<Option<Mark>> {
        DisplayOnly::new(filled.then(|| mark(0)))
    }

    /// Exactly one non-empty carrier reports `NonEmpty` with the ordinary projection's value;
    /// the same shape with only empty carriers reports `Nothing`.
    fn assert_branch_reports<T: Serialize>(label: &str, build: impl Fn(bool) -> T) {
        let (filled, empty) = (build(true), build(false));
        let report = reported_rules_projection(&filled).unwrap();
        assert_eq!(report.omitted, OmittedDisplayData::NonEmpty, "{label}");
        assert_eq!(report.value, rules_projection(&filled).unwrap(), "{label}");
        assert!(
            !report.value.to_string().contains("SENTINEL"),
            "{label}: the printing is omitted"
        );
        let report = reported_rules_projection(&empty).unwrap();
        assert_eq!(
            report.omitted,
            OmittedDisplayData::Nothing,
            "{label} control"
        );
        assert_eq!(report.value, rules_projection(&empty).unwrap(), "{label}");
    }

    #[test]
    fn every_observer_threading_branch_reports_a_lone_non_empty_carrier() {
        assert_branch_reports("newtype variant payload", |f| Branch::Payload(carrier(f)));
        assert_branch_reports("tuple variant field", |f| Branch::Pair(carrier(f), 1));
        assert_branch_reports("struct variant field", |f| Branch::Fields {
            shown: carrier(f),
        });
        assert_branch_reports("struct field", |f| NoSkip { shown: carrier(f) });
        assert_branch_reports("map value", |f| {
            BTreeMap::from([("k".to_string(), carrier(f))])
        });
        assert_branch_reports("sequence element", |f| vec![carrier(f)]);
        assert_branch_reports("tuple element", |f| (carrier(f), 1u8));
        assert_branch_reports("tuple struct field", |f| TupleStructBranch(carrier(f), 1));
    }

    #[test]
    fn only_the_reported_mode_serializes_an_omitted_payload() {
        // Reach: the payload's error is real.
        assert!(serde_json::to_value(Failing).is_err());

        let carrier = DisplayOnly::new(Some(Failing));
        assert_eq!(
            rules_projection(&carrier).unwrap(),
            Value::Null,
            "the ordinary mode never serializes the omitted payload"
        );
        let error = reported_rules_projection(&carrier).unwrap_err();
        assert!(error.to_string().contains("failing payload"), "{error}");
    }

    fn digest<T: Hash>(value: &T) -> u64 {
        let mut hasher = DefaultHasher::new();
        value.hash(&mut hasher);
        hasher.finish()
    }

    #[test]
    fn display_only_is_equality_hash_and_debug_transparent() {
        let shown = DisplayOnly::new(Some(Mark {
            set_code: sentinel(0),
            collector_number: "7".to_string(),
        }));
        let empty = DisplayOnly::new(None::<Mark>);
        assert!(shown == empty);
        // `Mark` is not `Hash`: the wrapper needs no bound on its inner type.
        assert_eq!(digest(&shown), digest(&empty));
        assert_eq!(format!("{shown:?}"), "DisplayOnly(..)");
        assert_eq!(format!("{empty:?}"), "DisplayOnly(..)");
        assert_eq!(format!("{shown:#?}"), "DisplayOnly(..)");
        assert_eq!(
            shown.get().as_ref().map(|m| m.set_code.as_str()),
            Some("SENTINEL00")
        );
        assert_eq!(DisplayOnly::new(5u8).into_inner(), 5);

        // An enclosing derived struct compares and prints the same, present or absent.
        let with = probe(0, true);
        let without = probe(0, false);
        assert!(with.shown.get().is_some() && without.shown.get().is_none());
        assert_eq!(with, without);
        assert_eq!(format!("{with:?}"), format!("{without:?}"));
        assert_eq!(format!("{with:#?}"), format!("{without:#?}"));
        assert!(format!("{with:?}").contains("shown: DisplayOnly(..)"));
        assert!(!format!("{with:?}").contains("SENTINEL"));

        // Positive control: a rules field still distinguishes both.
        let mut renamed = probe(0, true);
        renamed.name = "Elf".to_string();
        assert_ne!(with, renamed);
        assert_ne!(format!("{with:?}"), format!("{renamed:?}"));
    }

    #[test]
    fn canonical_digest_pins_portable_bytes_and_transparent_codec() {
        let text = r#"{"a":"é😀\"\\","b":[1,{"a":null,"z":true}],"big":18446744073709551615,"f":0.1,"n":-3.5,"neg":-9223372036854775808}"#;
        let value: Value = serde_json::from_str(text).unwrap();
        assert_eq!(canonical_json_bytes(&value), text.as_bytes());
        let digest = canonical_digest(&value);
        assert_eq!(
            digest.as_str(),
            "c6fcc1dcc1daff0582958515408661d88592ab06d1c59b3123b75dc8a7594355"
        );
        fn reverse_objects(value: &Value) -> Value {
            match value {
                Value::Object(entries) => Value::Object(
                    entries
                        .iter()
                        .rev()
                        .map(|(key, value)| (key.clone(), reverse_objects(value)))
                        .collect(),
                ),
                Value::Array(entries) => {
                    Value::Array(entries.iter().map(reverse_objects).collect())
                }
                value => value.clone(),
            }
        }
        assert_eq!(canonical_digest(&reverse_objects(&value)), digest);
        assert_ne!(
            canonical_digest(&serde_json::json!([1, 2])),
            canonical_digest(&serde_json::json!([2, 1]))
        );
        let encoded = serde_json::to_value(&digest).unwrap();
        assert_eq!(encoded, serde_json::json!(digest.as_str()));
        assert_eq!(
            serde_json::from_value::<CanonicalDigest>(encoded).unwrap(),
            digest
        );
    }
}

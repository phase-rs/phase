use std::collections::{HashMap, HashSet};
use std::fmt;
use std::hash::{BuildHasher, Hash};
use std::marker::PhantomData;

use serde::de::{MapAccess, Visitor};
use serde::ser::{SerializeMap, SerializeSeq};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use super::identifiers::{ObjectId, TrackedSetId};
use super::player::PlayerId;

pub(crate) trait NumericMapKey: Sized {
    fn from_u64(value: u64) -> Option<Self>;
}

impl NumericMapKey for u64 {
    fn from_u64(value: u64) -> Option<Self> {
        Some(value)
    }
}

impl NumericMapKey for ObjectId {
    fn from_u64(value: u64) -> Option<Self> {
        Some(Self(value))
    }
}

impl NumericMapKey for PlayerId {
    fn from_u64(value: u64) -> Option<Self> {
        u8::try_from(value).ok().map(Self)
    }
}

impl NumericMapKey for TrackedSetId {
    fn from_u64(value: u64) -> Option<Self> {
        Some(Self(value))
    }
}

struct NumericKey<K>(K);

impl<'de, K> Deserialize<'de> for NumericKey<K>
where
    K: NumericMapKey,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NumericKeyVisitor<K>(PhantomData<K>);

        impl<K> NumericKeyVisitor<K>
        where
            K: NumericMapKey,
        {
            fn convert<E>(value: u64) -> Result<NumericKey<K>, E>
            where
                E: serde::de::Error,
            {
                K::from_u64(value).map(NumericKey).ok_or_else(|| {
                    E::custom(format_args!(
                        "numeric map key {value} is out of range for {}",
                        std::any::type_name::<K>()
                    ))
                })
            }
        }

        impl<'de, K> Visitor<'de> for NumericKeyVisitor<K>
        where
            K: NumericMapKey,
        {
            type Value = NumericKey<K>;

            fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str("a canonical unsigned decimal map key")
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::convert(value)
            }

            fn visit_u8<E>(self, value: u8) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::convert(u64::from(value))
            }

            fn visit_u16<E>(self, value: u16) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::convert(u64::from(value))
            }

            fn visit_u32<E>(self, value: u32) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Self::convert(u64::from(value))
            }

            fn visit_u128<E>(self, value: u128) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                u64::try_from(value)
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Other("u128"), &self))
                    .and_then(Self::convert)
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if value.is_empty()
                    || (value.len() > 1 && value.starts_with('0'))
                    || !value.bytes().all(|byte| byte.is_ascii_digit())
                {
                    return Err(E::invalid_value(serde::de::Unexpected::Str(value), &self));
                }
                let value = value
                    .parse::<u64>()
                    .map_err(|_| E::invalid_value(serde::de::Unexpected::Str(value), &self))?;
                Self::convert(value)
            }
        }

        deserializer.deserialize_any(NumericKeyVisitor(PhantomData))
    }
}

struct NumericHashMapVisitor<K, V>(PhantomData<(K, V)>);

fn cautious_map_capacity<K, V>(hint: Option<usize>) -> usize {
    const MAX_PREALLOC_BYTES: usize = 1024 * 1024;

    let max_entries = MAX_PREALLOC_BYTES
        .checked_div(std::mem::size_of::<(K, V)>())
        .unwrap_or(0);
    hint.unwrap_or(0).min(max_entries)
}

/// Serde adapter for hash maps whose typed keys cannot be represented as JSON
/// object keys. The wire form is a key-sorted sequence of `[key, value]` pairs.
pub(crate) mod hash_map_entries {
    use std::collections::{hash_map::Entry, HashMap};
    use std::fmt;
    use std::hash::{BuildHasher, Hash};
    use std::marker::PhantomData;

    use serde::de::{Error as _, SeqAccess, Visitor};
    use serde::ser::SerializeSeq;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(crate) fn serialize<K, V, H, S>(
        values: &HashMap<K, V, H>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        K: Ord + Serialize,
        V: Serialize,
        S: Serializer,
    {
        let mut entries: Vec<_> = values.iter().collect();
        entries.sort_unstable_by_key(|(key, _)| *key);

        let mut sequence = serializer.serialize_seq(Some(entries.len()))?;
        for (key, value) in entries {
            sequence.serialize_element(&(key, value))?;
        }
        sequence.end()
    }

    struct HashMapEntriesVisitor<K, V, H>(PhantomData<(K, V, H)>);

    impl<'de, K, V, H> Visitor<'de> for HashMapEntriesVisitor<K, V, H>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        H: BuildHasher + Default,
    {
        type Value = HashMap<K, V, H>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("a sequence of unique typed key-value pairs")
        }

        fn visit_seq<A>(self, mut access: A) -> Result<Self::Value, A::Error>
        where
            A: SeqAccess<'de>,
        {
            // A wire-provided size hint is only an optimization. Cap it like
            // Serde's cautious collection allocation; the map grows normally
            // past this cap.
            let capacity = super::cautious_map_capacity::<K, V>(access.size_hint());
            let mut values = HashMap::with_capacity_and_hasher(capacity, H::default());
            while let Some((key, value)) = access.next_element()? {
                match values.entry(key) {
                    Entry::Vacant(entry) => {
                        entry.insert(value);
                    }
                    Entry::Occupied(_) => {
                        return Err(A::Error::custom(
                            "duplicate key in typed hash-map entry sequence",
                        ));
                    }
                }
            }
            Ok(values)
        }
    }

    pub(crate) fn deserialize<'de, K, V, H, D>(
        deserializer: D,
    ) -> Result<HashMap<K, V, H>, D::Error>
    where
        K: Deserialize<'de> + Eq + Hash,
        V: Deserialize<'de>,
        H: BuildHasher + Default,
        D: Deserializer<'de>,
    {
        deserializer.deserialize_seq(HashMapEntriesVisitor(PhantomData))
    }
}

impl<'de, K, V> Visitor<'de> for NumericHashMapVisitor<K, V>
where
    K: Eq + Hash + NumericMapKey,
    V: Deserialize<'de>,
{
    type Value = HashMap<K, V>;

    fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("a map with canonical unsigned numeric keys")
    }

    fn visit_map<A>(self, mut access: A) -> Result<Self::Value, A::Error>
    where
        A: MapAccess<'de>,
    {
        // A wire-provided size hint is only an optimization. Cap it like Serde's
        // cautious collection allocation; the map grows normally past this cap.
        let mut values = HashMap::with_capacity(cautious_map_capacity::<K, V>(access.size_hint()));
        while let Some((NumericKey(key), value)) = access.next_entry()? {
            values.insert(key, value);
        }
        Ok(values)
    }
}

pub(crate) fn deserialize_numeric_hash_map<'de, K, V, D>(
    deserializer: D,
) -> Result<HashMap<K, V>, D::Error>
where
    K: Eq + Hash + NumericMapKey,
    V: Deserialize<'de>,
    D: Deserializer<'de>,
{
    deserializer.deserialize_map(NumericHashMapVisitor(PhantomData))
}

pub(crate) fn deserialize_option_numeric_hash_map<'de, K, V, D>(
    deserializer: D,
) -> Result<Option<HashMap<K, V>>, D::Error>
where
    K: Eq + Hash + NumericMapKey,
    V: Deserialize<'de>,
    D: Deserializer<'de>,
{
    struct OptionalNumericHashMapVisitor<K, V>(PhantomData<(K, V)>);

    impl<'de, K, V> Visitor<'de> for OptionalNumericHashMapVisitor<K, V>
    where
        K: Eq + Hash + NumericMapKey,
        V: Deserialize<'de>,
    {
        type Value = Option<HashMap<K, V>>;

        fn expecting(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
            formatter.write_str("null or a map with canonical unsigned numeric keys")
        }

        fn visit_none<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_unit<E>(self) -> Result<Self::Value, E> {
            Ok(None)
        }

        fn visit_some<D>(self, deserializer: D) -> Result<Self::Value, D::Error>
        where
            D: Deserializer<'de>,
        {
            deserialize_numeric_hash_map(deserializer).map(Some)
        }
    }

    deserializer.deserialize_option(OptionalNumericHashMapVisitor(PhantomData))
}

pub(crate) fn hash_set<T, H, S>(values: &HashSet<T, H>, serializer: S) -> Result<S::Ok, S::Error>
where
    T: Ord + Serialize,
    S: Serializer,
{
    SortedHashSet(values).serialize(serializer)
}

pub(crate) fn option_hash_set<T, H, S>(
    values: &Option<HashSet<T, H>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    T: Ord + Serialize,
    S: Serializer,
{
    values.as_ref().map(SortedHashSet).serialize(serializer)
}

pub(crate) fn hash_map<K, V, H, S>(
    values: &HashMap<K, V, H>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K: Ord + Serialize,
    V: Serialize,
    S: Serializer,
{
    SortedHashMap(values).serialize(serializer)
}

pub(crate) fn option_hash_map<K, V, H, S>(
    values: &Option<HashMap<K, V, H>>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K: Ord + Serialize,
    V: Serialize,
    S: Serializer,
{
    values.as_ref().map(SortedHashMap).serialize(serializer)
}

pub(crate) fn vec_hash_map<K, V, H, S>(
    values: &[HashMap<K, V, H>],
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K: Ord + Serialize,
    V: Serialize,
    S: Serializer,
{
    let mut sequence = serializer.serialize_seq(Some(values.len()))?;
    for value in values {
        sequence.serialize_element(&SortedHashMap(value))?;
    }
    sequence.end()
}

pub(crate) fn serialize_sorted_map_entries<'a, K, V, W, F, S>(
    entries: impl Iterator<Item = (&'a K, &'a V)>,
    wrap_value: F,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K: Ord + Serialize + 'a,
    V: 'a,
    W: Serialize,
    F: Fn(&'a V) -> W,
    S: Serializer,
{
    let mut entries: Vec<_> = entries.collect();
    entries.sort_unstable_by_key(|(key, _)| *key);

    let mut map = serializer.serialize_map(Some(entries.len()))?;
    for (key, value) in entries {
        map.serialize_entry(key, &wrap_value(value))?;
    }
    map.end()
}

pub(crate) fn hash_map_of_hash_set<K, V, H1, H2, S>(
    values: &HashMap<K, HashSet<V, H2>, H1>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K: Ord + Serialize,
    V: Ord + Serialize,
    S: Serializer,
{
    serialize_sorted_map_entries(values.iter(), SortedHashSet, serializer)
}

pub(crate) fn hash_map_of_hash_map<K1, K2, V, H1, H2, S>(
    values: &HashMap<K1, HashMap<K2, V, H2>, H1>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K1: Ord + Serialize,
    K2: Ord + Serialize,
    V: Serialize,
    S: Serializer,
{
    serialize_sorted_map_entries(values.iter(), SortedHashMap, serializer)
}

pub(crate) fn im_hash_set<T, H, S>(
    values: &im::HashSet<T, H>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    T: Clone + Eq + Hash + Ord + Serialize,
    H: BuildHasher,
    S: Serializer,
{
    let mut values: Vec<_> = values.iter().collect();
    values.sort_unstable();
    values.serialize(serializer)
}

pub(crate) fn im_hash_map<K, V, H, S>(
    values: &im::HashMap<K, V, H>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K: Clone + Eq + Hash + Ord + Serialize,
    V: Clone + Serialize,
    H: BuildHasher,
    S: Serializer,
{
    SortedImHashMap(values).serialize(serializer)
}

pub(crate) fn im_hash_map_of_im_hash_map<K1, K2, V, H1, H2, S>(
    values: &im::HashMap<K1, im::HashMap<K2, V, H2>, H1>,
    serializer: S,
) -> Result<S::Ok, S::Error>
where
    K1: Clone + Eq + Hash + Ord + Serialize,
    K2: Clone + Eq + Hash + Ord + Serialize,
    V: Clone + Serialize,
    H1: BuildHasher,
    H2: BuildHasher,
    S: Serializer,
{
    serialize_sorted_map_entries(values.iter(), SortedImHashMap, serializer)
}

struct SortedHashSet<'a, T, H>(&'a HashSet<T, H>);

impl<T, H> Serialize for SortedHashSet<'_, T, H>
where
    T: Ord + Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut values: Vec<_> = self.0.iter().collect();
        values.sort_unstable();
        values.serialize(serializer)
    }
}

struct SortedHashMap<'a, K, V, H>(&'a HashMap<K, V, H>);

impl<K, V, H> Serialize for SortedHashMap<'_, K, V, H>
where
    K: Ord + Serialize,
    V: Serialize,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_sorted_map_entries(self.0.iter(), std::convert::identity, serializer)
    }
}

struct SortedImHashMap<'a, K, V, H>(&'a im::HashMap<K, V, H>);

impl<K, V, H> Serialize for SortedImHashMap<'_, K, V, H>
where
    K: Clone + Eq + Hash + Ord + Serialize,
    V: Clone + Serialize,
    H: BuildHasher,
{
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serialize_sorted_map_entries(self.0.iter(), std::convert::identity, serializer)
    }
}

#[cfg(test)]
pub(crate) mod test_support {
    use std::hash::{BuildHasher, Hasher};

    #[derive(Clone, Default)]
    pub(crate) struct ReverseBuildHasher;

    pub(crate) struct ReverseHasher(u64);

    impl BuildHasher for ReverseBuildHasher {
        type Hasher = ReverseHasher;

        fn build_hasher(&self) -> Self::Hasher {
            ReverseHasher(0)
        }
    }

    impl Hasher for ReverseHasher {
        fn finish(&self) -> u64 {
            u64::MAX - self.0
        }

        fn write(&mut self, bytes: &[u8]) {
            self.0 = bytes
                .iter()
                .fold(0_u64, |value, byte| (value << 8) | u64::from(*byte));
        }

        fn write_u64(&mut self, value: u64) {
            self.0 = value;
        }

        fn write_usize(&mut self, value: usize) {
            self.0 = value as u64;
        }

        fn write_isize(&mut self, value: isize) {
            self.0 = value as u64;
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, HashSet};

    use serde::de::value::{Error as ValueError, MapAccessDeserializer, SeqAccessDeserializer};
    use serde::de::{DeserializeSeed, IntoDeserializer, MapAccess, SeqAccess};
    use serde::{Deserialize, Serialize};

    use super::test_support::ReverseBuildHasher;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

    type Set = HashSet<u64, ReverseBuildHasher>;
    type Map<V> = HashMap<u64, V, ReverseBuildHasher>;
    type StructuredEntryMap = HashMap<(u64, u64), String, ReverseBuildHasher>;

    #[derive(Serialize)]
    struct StandardFixture<'a> {
        #[serde(serialize_with = "super::hash_set")]
        set: &'a Set,
        #[serde(serialize_with = "super::option_hash_set")]
        optional_set: &'a Option<Set>,
        #[serde(serialize_with = "super::hash_map")]
        map: &'a Map<&'static str>,
        #[serde(serialize_with = "super::option_hash_map")]
        optional_map: &'a Option<Map<&'static str>>,
        #[serde(serialize_with = "super::vec_hash_map")]
        maps: &'a Vec<Map<&'static str>>,
        #[serde(serialize_with = "super::hash_map_of_hash_set")]
        map_of_sets: &'a Map<Set>,
        #[serde(serialize_with = "super::hash_map_of_hash_map")]
        map_of_maps: &'a Map<Map<&'static str>>,
    }

    #[test]
    fn standard_hash_adapters_sort_every_unordered_level_and_preserve_vector_order() {
        let set = Set::from_iter([1, 2, 3]);
        assert_eq!(set.iter().copied().collect::<Vec<_>>(), vec![3, 2, 1]);

        let map = Map::from_iter([(1, "one"), (2, "two"), (3, "three")]);
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), vec![3, 2, 1]);

        let optional_set = Some(set.clone());
        let optional_map = Some(map.clone());
        let maps = vec![
            Map::from_iter([(2, "two"), (1, "one")]),
            Map::from_iter([(3, "three"), (2, "two")]),
        ];
        let map_of_sets =
            Map::from_iter([(2, Set::from_iter([3, 1])), (1, Set::from_iter([2, 1]))]);
        let map_of_maps = Map::from_iter([
            (2, Map::from_iter([(3, "three"), (1, "one")])),
            (1, Map::from_iter([(2, "two"), (1, "one")])),
        ]);
        for inner in map_of_sets.values() {
            let values = inner.iter().copied().collect::<Vec<_>>();
            assert!(
                values.windows(2).all(|pair| pair[0] > pair[1]),
                "nested set must expose descending native iteration: {values:?}"
            );
        }
        for inner in map_of_maps.values() {
            let keys = inner.keys().copied().collect::<Vec<_>>();
            assert!(
                keys.windows(2).all(|pair| pair[0] > pair[1]),
                "nested map must expose descending native iteration: {keys:?}"
            );
        }

        let serialized = serde_json::to_string(&StandardFixture {
            set: &set,
            optional_set: &optional_set,
            map: &map,
            optional_map: &optional_map,
            maps: &maps,
            map_of_sets: &map_of_sets,
            map_of_maps: &map_of_maps,
        })
        .expect("fixture should serialize");

        assert_eq!(
            serialized,
            r#"{"set":[1,2,3],"optional_set":[1,2,3],"map":{"1":"one","2":"two","3":"three"},"optional_map":{"1":"one","2":"two","3":"three"},"maps":[{"1":"one","2":"two"},{"2":"two","3":"three"}],"map_of_sets":{"1":[1,2],"2":[1,3]},"map_of_maps":{"1":{"1":"one","2":"two"},"2":{"1":"one","3":"three"}}}"#
        );
    }

    #[test]
    fn standard_hash_adapters_cover_empty_singleton_and_none() {
        let empty_set = Set::default();
        let singleton_set = Set::from_iter([7]);
        let empty_map = Map::default();
        let singleton_map = Map::from_iter([(7, "seven")]);

        #[derive(Serialize)]
        struct BoundaryFixture<'a> {
            #[serde(serialize_with = "super::hash_set")]
            empty_set: &'a Set,
            #[serde(serialize_with = "super::hash_set")]
            singleton_set: &'a Set,
            #[serde(serialize_with = "super::hash_map")]
            empty_map: &'a Map<&'static str>,
            #[serde(serialize_with = "super::hash_map")]
            singleton_map: &'a Map<&'static str>,
            #[serde(serialize_with = "super::option_hash_set")]
            no_set: &'a Option<Set>,
            #[serde(serialize_with = "super::option_hash_map")]
            no_map: &'a Option<Map<&'static str>>,
        }

        assert_eq!(
            serde_json::to_string(&BoundaryFixture {
                empty_set: &empty_set,
                singleton_set: &singleton_set,
                empty_map: &empty_map,
                singleton_map: &singleton_map,
                no_set: &None,
                no_map: &None,
            })
            .expect("fixture should serialize"),
            r#"{"empty_set":[],"singleton_set":[7],"empty_map":{},"singleton_map":{"7":"seven"},"no_set":null,"no_map":null}"#
        );
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    struct HashMapEntriesFixture {
        #[serde(with = "super::hash_map_entries")]
        values: StructuredEntryMap,
    }

    #[test]
    fn hash_map_entries_sorts_typed_keys_and_round_trips_the_exact_pair_shape() {
        let ascending_insertion = HashMapEntriesFixture {
            values: StructuredEntryMap::from_iter([
                ((1, 1), "one".to_string()),
                ((2, 2), "two".to_string()),
                ((3, 3), "three".to_string()),
            ]),
        };
        assert_eq!(
            ascending_insertion
                .values
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![(3, 3), (2, 2), (1, 1)],
            "hostile hashing must expose descending native iteration"
        );

        let reverse_insertion = HashMapEntriesFixture {
            values: StructuredEntryMap::from_iter([
                ((3, 3), "three".to_string()),
                ((2, 2), "two".to_string()),
                ((1, 1), "one".to_string()),
            ]),
        };
        let expected = r#"{"values":[[[1,1],"one"],[[2,2],"two"],[[3,3],"three"]]}"#;
        let ascending_bytes = serde_json::to_string(&ascending_insertion).unwrap();
        let reverse_bytes = serde_json::to_string(&reverse_insertion).unwrap();
        assert_eq!(ascending_bytes, expected);
        assert_eq!(
            reverse_bytes, expected,
            "insertion order must not affect bytes"
        );

        let restored: HashMapEntriesFixture = serde_json::from_str(expected).unwrap();
        assert_eq!(restored, ascending_insertion);
        assert_eq!(serde_json::to_string(&restored).unwrap(), expected);
    }

    #[test]
    fn hash_map_entries_rejects_duplicates_and_malformed_wire_shapes() {
        let duplicate = serde_json::from_str::<HashMapEntriesFixture>(
            r#"{"values":[[[1,1],"first"],[[1,1],"second"]]}"#,
        )
        .unwrap_err();
        assert!(
            duplicate
                .to_string()
                .contains("duplicate key in typed hash-map entry sequence"),
            "duplicate typed keys with different values must be rejected: {duplicate}"
        );

        for (description, malformed) in [
            ("object-shaped map", r#"{"values":{"[1,1]":"one"}}"#),
            ("one-element entry tuple", r#"{"values":[[[1,1]]]}"#),
            (
                "wrong typed-key component",
                r#"{"values":[[[1,"one"],"value"]]}"#,
            ),
        ] {
            assert!(
                serde_json::from_str::<HashMapEntriesFixture>(malformed).is_err(),
                "{description} must be rejected"
            );
        }
    }

    struct EnormousHintSeqAccess {
        entry: Option<serde_json::Value>,
    }

    impl<'de> SeqAccess<'de> for EnormousHintSeqAccess {
        type Error = serde_json::Error;

        fn next_element_seed<T>(&mut self, seed: T) -> Result<Option<T::Value>, Self::Error>
        where
            T: DeserializeSeed<'de>,
        {
            self.entry
                .take()
                .map(|entry| seed.deserialize(entry.into_deserializer()))
                .transpose()
        }

        fn size_hint(&self) -> Option<usize> {
            Some(usize::MAX)
        }
    }

    #[test]
    fn hash_map_entries_caps_untrusted_sequence_size_hints() {
        let access = EnormousHintSeqAccess {
            entry: Some(serde_json::json!([[7, 8], "seven-eight"])),
        };

        let values: StructuredEntryMap =
            super::hash_map_entries::deserialize(SeqAccessDeserializer::new(access)).unwrap();

        assert_eq!(values.get(&(7, 8)).map(String::as_str), Some("seven-eight"));
        assert_eq!(
            super::cautious_map_capacity::<(u64, u64), String>(Some(usize::MAX)),
            1024 * 1024 / std::mem::size_of::<((u64, u64), String)>()
        );
    }

    #[test]
    fn sorted_map_serialization_is_default_transparent_except_for_order() {
        #[derive(Serialize)]
        struct DefaultMap<'a> {
            map: &'a HashMap<ObjectId, &'static str, ReverseBuildHasher>,
        }

        #[derive(Serialize)]
        struct SortedMap<'a> {
            #[serde(serialize_with = "super::hash_map")]
            map: &'a HashMap<ObjectId, &'static str, ReverseBuildHasher>,
        }

        let map = HashMap::from_iter([
            (ObjectId(1), "one"),
            (ObjectId(2), "two"),
            (ObjectId(3), "three"),
        ]);
        assert_eq!(
            map.keys().copied().collect::<Vec<_>>(),
            vec![ObjectId(3), ObjectId(2), ObjectId(1)]
        );

        let default_json = serde_json::to_string(&DefaultMap { map: &map }).unwrap();
        let sorted_json = serde_json::to_string(&SortedMap { map: &map }).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&default_json).unwrap(),
            serde_json::from_str::<serde_json::Value>(&sorted_json).unwrap(),
            "sorting must not change the parsed key/value representation"
        );
        assert_eq!(sorted_json, r#"{"map":{"1":"one","2":"two","3":"three"}}"#);

        let singleton = HashMap::from_iter([(ObjectId(7), "seven")]);
        assert_eq!(
            serde_json::to_string(&DefaultMap { map: &singleton }).unwrap(),
            serde_json::to_string(&SortedMap { map: &singleton }).unwrap(),
            "a singleton has no ordering difference and must be byte-identical"
        );

        let restored_default: HashMap<ObjectId, String> =
            serde_json::from_str(r#"{"1":"one","2":"two","3":"three"}"#).unwrap();
        let restored_sorted: HashMap<ObjectId, String> = serde_json::from_str(
            serde_json::to_value(&SortedMap { map: &map })
                .unwrap()
                .get("map")
                .unwrap()
                .to_string()
                .as_str(),
        )
        .unwrap();
        assert_eq!(restored_default, restored_sorted);
    }

    #[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(tag = "type", content = "data")]
    enum BufferedNumericMap {
        Required {
            #[serde(
                serialize_with = "super::hash_map",
                deserialize_with = "super::deserialize_numeric_hash_map"
            )]
            values: HashMap<ObjectId, String>,
        },
        Optional {
            #[serde(
                serialize_with = "super::option_hash_map",
                deserialize_with = "super::deserialize_option_numeric_hash_map"
            )]
            values: Option<HashMap<ObjectId, String>>,
        },
        Player {
            #[serde(
                serialize_with = "super::hash_map",
                deserialize_with = "super::deserialize_numeric_hash_map"
            )]
            values: HashMap<PlayerId, String>,
        },
    }

    #[test]
    fn buffered_numeric_map_keys_are_field_local_round_trippable_and_strict() {
        let required = BufferedNumericMap::Required {
            values: HashMap::from([
                (ObjectId(2), "two".to_string()),
                (ObjectId(1), "one".to_string()),
            ]),
        };
        let required_value = serde_json::to_value(&required).unwrap();
        assert_eq!(
            required_value["data"]["values"],
            serde_json::json!({"1": "one", "2": "two"})
        );
        assert_eq!(
            serde_json::from_value::<BufferedNumericMap>(required_value).unwrap(),
            required
        );

        for optional in [
            BufferedNumericMap::Optional {
                values: Some(HashMap::from([
                    (ObjectId(2), "two".to_string()),
                    (ObjectId(1), "one".to_string()),
                ])),
            },
            BufferedNumericMap::Optional {
                values: Some(HashMap::new()),
            },
            BufferedNumericMap::Optional { values: None },
        ] {
            let value = serde_json::to_value(&optional).unwrap();
            assert_eq!(
                serde_json::from_value::<BufferedNumericMap>(value).unwrap(),
                optional
            );
        }

        for malformed in [
            "-1",
            "1.0",
            "01",
            " 1",
            "1 ",
            "text",
            "18446744073709551616",
        ] {
            let value = serde_json::json!({
                "type": "Required",
                "data": {"values": {(malformed): "bad"}}
            });
            assert!(
                serde_json::from_value::<BufferedNumericMap>(value).is_err(),
                "malformed key {malformed:?} must be rejected"
            );
        }

        assert!(
            serde_json::from_str::<ObjectId>(r#""1""#).is_err(),
            "ObjectId values must not become string-permissive"
        );

        let players = BufferedNumericMap::Player {
            values: HashMap::from([(PlayerId(7), "seven".to_string())]),
        };
        let players_value = serde_json::to_value(&players).unwrap();
        assert_eq!(
            serde_json::from_value::<BufferedNumericMap>(players_value).unwrap(),
            players,
            "a valid PlayerId key must reach the strict numeric-key adapter"
        );

        let error = serde_json::from_value::<BufferedNumericMap>(serde_json::json!({
            "type": "Player",
            "data": {"values": {"256": "out of range"}}
        }))
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "numeric map key 256 is out of range for engine::types::player::PlayerId"
        );
    }

    struct EnormousHintMapAccess {
        entry: Option<(u64, String)>,
        value: Option<String>,
    }

    impl<'de> MapAccess<'de> for EnormousHintMapAccess {
        type Error = ValueError;

        fn next_key_seed<K>(&mut self, seed: K) -> Result<Option<K::Value>, Self::Error>
        where
            K: DeserializeSeed<'de>,
        {
            let Some((key, value)) = self.entry.take() else {
                return Ok(None);
            };
            self.value = Some(value);
            seed.deserialize(key.into_deserializer()).map(Some)
        }

        fn next_value_seed<V>(&mut self, seed: V) -> Result<V::Value, Self::Error>
        where
            V: DeserializeSeed<'de>,
        {
            seed.deserialize(
                self.value
                    .take()
                    .expect("next_value_seed must follow next_key_seed")
                    .into_deserializer(),
            )
        }

        fn size_hint(&self) -> Option<usize> {
            Some(usize::MAX)
        }
    }

    #[test]
    fn numeric_map_deserializer_caps_untrusted_size_hints() {
        let access = EnormousHintMapAccess {
            entry: Some((7, "seven".to_string())),
            value: None,
        };

        let values: HashMap<ObjectId, String> =
            super::deserialize_numeric_hash_map(MapAccessDeserializer::new(access)).unwrap();

        assert_eq!(values, HashMap::from([(ObjectId(7), "seven".to_string())]));
    }

    #[derive(Serialize)]
    struct ImFixture<'a> {
        #[serde(serialize_with = "super::im_hash_set")]
        set: &'a im::HashSet<u64, ReverseBuildHasher>,
        #[serde(serialize_with = "super::im_hash_map")]
        map: &'a im::HashMap<u64, &'static str, ReverseBuildHasher>,
        #[serde(serialize_with = "super::im_hash_map_of_im_hash_map")]
        map_of_maps: &'a im::HashMap<
            u64,
            im::HashMap<u64, &'static str, ReverseBuildHasher>,
            ReverseBuildHasher,
        >,
    }

    #[test]
    fn im_hash_adapters_sort_plain_and_nested_collections() {
        let set = im::HashSet::from_iter([1_u64, 2, 3]);
        assert_eq!(set.iter().copied().collect::<Vec<_>>(), vec![3, 2, 1]);
        let map = im::HashMap::from_iter([(1_u64, "one"), (2, "two"), (3, "three")]);
        assert_eq!(map.keys().copied().collect::<Vec<_>>(), vec![3, 2, 1]);
        let map_of_maps = im::HashMap::from_iter([
            (
                2_u64,
                im::HashMap::from_iter([(3_u64, "three"), (1, "one")]),
            ),
            (1, im::HashMap::from_iter([(2_u64, "two"), (1, "one")])),
        ]);
        for inner in map_of_maps.values() {
            let keys = inner.keys().copied().collect::<Vec<_>>();
            assert!(
                keys.windows(2).all(|pair| pair[0] > pair[1]),
                "nested map must expose descending native iteration: {keys:?}"
            );
        }

        assert_eq!(
            serde_json::to_string(&ImFixture {
                set: &set,
                map: &map,
                map_of_maps: &map_of_maps,
            })
            .expect("fixture should serialize"),
            r#"{"set":[1,2,3],"map":{"1":"one","2":"two","3":"three"},"map_of_maps":{"1":{"1":"one","2":"two"},"2":{"1":"one","3":"three"}}}"#
        );
    }
}

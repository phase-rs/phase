//! The game-object store.
//!
//! `GameState::clone()` runs once per AI search node, so the object store is a
//! persistent `im::HashMap` whose trie nodes are shared between cloned states.
//! Each value is additionally held behind an `Arc`: a mutable lookup on a
//! shared node then copies that node's `Arc` handles and deep-clones only the
//! one object being written (`Arc::make_mut`), instead of deep-cloning every
//! `GameObject` stored in the node.
//!
//! The API mirrors the subset of `im::HashMap` the engine uses and forwards
//! every structural operation to the underlying map unchanged, so iteration
//! order, lookup semantics, `Debug` output, serialization and panic messages
//! are exactly those of the underlying map keyed the same way.
//!
//! Sharing between a map and its clones is not part of game state. It is never
//! serialized or hashed, and it is observable only through
//! [`ObjectMap::ptr_eq`], [`ObjectMap::shares_object_with`] and
//! [`ObjectMap::unshared_ids`]. Callers may use sharing only to skip work whose
//! result it cannot change.

use std::collections::BTreeSet;
use std::fmt;
use std::ops::{Index, IndexMut};
use std::sync::Arc;

use rustc_hash::FxBuildHasher;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::game::game_object::GameObject;
use crate::types::identifiers::ObjectId;

/// The persistent map backing [`ObjectMap`].
type Inner = im::HashMap<ObjectId, Arc<GameObject>, FxBuildHasher>;

/// Shared-reference iterator over an [`ObjectMap`], in the underlying map's
/// iteration order.
type ObjectIter<'a> = std::iter::Map<
    im::hashmap::Iter<'a, ObjectId, Arc<GameObject>>,
    fn((&'a ObjectId, &'a Arc<GameObject>)) -> (&'a ObjectId, &'a GameObject),
>;

/// Mutable iterator over an [`ObjectMap`], in the underlying map's iteration
/// order.
type ObjectIterMut<'a> = std::iter::Map<
    im::hashmap::IterMut<'a, ObjectId, Arc<GameObject>>,
    fn((&'a ObjectId, &'a mut Arc<GameObject>)) -> (&'a ObjectId, &'a mut GameObject),
>;

/// Iterator over the ids of an [`ObjectMap`], in the underlying map's
/// iteration order.
type ObjectKeys<'a> = im::hashmap::Keys<'a, ObjectId, Arc<GameObject>>;

/// Iterator over the objects of an [`ObjectMap`], in the underlying map's
/// iteration order.
type ObjectValues<'a> = std::iter::Map<
    im::hashmap::Values<'a, ObjectId, Arc<GameObject>>,
    fn(&'a Arc<GameObject>) -> &'a GameObject,
>;

/// Projects a borrowed map entry to the `GameObject` behind its `Arc`.
fn entry_ref<'a>(
    (id, object): (&'a ObjectId, &'a Arc<GameObject>),
) -> (&'a ObjectId, &'a GameObject) {
    (id, &**object)
}

/// Projects a mutably borrowed map entry to the `GameObject` behind its
/// `Arc`, cloning the object first when it is shared.
fn entry_mut<'a>(
    (id, object): (&'a ObjectId, &'a mut Arc<GameObject>),
) -> (&'a ObjectId, &'a mut GameObject) {
    (id, Arc::make_mut(object))
}

/// Projects a borrowed map value to the `GameObject` behind its `Arc`.
fn value_ref(object: &Arc<GameObject>) -> &GameObject {
    object
}

/// Every game object in a [`GameState`](crate::types::game_state::GameState),
/// keyed by `ObjectId`.
///
/// Cloning is cheap and shares every object with the original. A write through
/// [`get_mut`](Self::get_mut), [`IndexMut`] or
/// [`update_where`](Self::update_where) unshares only the objects it reaches.
#[derive(Clone, Default)]
pub struct ObjectMap {
    inner: Inner,
}

impl ObjectMap {
    /// Creates an empty map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Returns the number of objects in the map.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    /// Returns true when the map holds no object.
    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Removes every object from the map.
    pub fn clear(&mut self) {
        self.inner.clear();
    }

    /// Returns true when an object with this id is in the map.
    pub fn contains_key(&self, id: &ObjectId) -> bool {
        self.inner.contains_key(id)
    }

    /// Returns the object with this id, if any.
    ///
    /// Generic over [`Borrow`](std::borrow::Borrow) like `im::HashMap::get`,
    /// so call sites written against the plain map keep compiling unchanged.
    pub fn get<Q>(&self, id: &Q) -> Option<&GameObject>
    where
        ObjectId: std::borrow::Borrow<Q>,
        Q: std::hash::Hash + Eq + ?Sized,
    {
        self.inner.get(id).map(|object| &**object)
    }

    /// Returns the object with this id for writing, if any.
    ///
    /// Copy-on-write: when the object is shared with a clone of this map, only
    /// this object is cloned. Note that the lookup unshares the path to the
    /// key even when the key is absent, so sweeps that usually change nothing
    /// should use [`update_where`](Self::update_where) instead.
    pub fn get_mut(&mut self, id: &ObjectId) -> Option<&mut GameObject> {
        self.inner.get_mut(id).map(Arc::make_mut)
    }

    /// Inserts an object, returning the object previously stored under `id`.
    ///
    /// A replaced object that is still shared with a clone of this map is
    /// cloned to produce the owned return value.
    pub fn insert(&mut self, id: ObjectId, object: GameObject) -> Option<GameObject> {
        self.inner
            .insert(id, Arc::new(object))
            .map(Arc::unwrap_or_clone)
    }

    /// Removes and returns the object stored under `id`, if any.
    ///
    /// A removed object that is still shared with a clone of this map is
    /// cloned to produce the owned return value.
    pub fn remove(&mut self, id: &ObjectId) -> Option<GameObject> {
        self.inner.remove(id).map(Arc::unwrap_or_clone)
    }

    /// Iterates over `(id, object)` pairs in the underlying map's order.
    ///
    /// The iterator types of this map are named (not `impl Trait`) so that
    /// borrows held in block-tail temporaries behave as they do for the
    /// underlying map's own iterators.
    pub fn iter(&self) -> ObjectIter<'_> {
        self.inner.iter().map(entry_ref)
    }

    /// Iterates over `(id, object)` pairs for writing, in the same order as
    /// [`iter`](Self::iter).
    ///
    /// Every visited object is made unique, so this unshares the whole map
    /// from its clones. Use it only for sweeps that really write every object;
    /// otherwise prefer [`update_where`](Self::update_where).
    pub fn iter_mut(&mut self) -> ObjectIterMut<'_> {
        self.inner.iter_mut().map(entry_mut)
    }

    /// Iterates over the object ids in the underlying map's order.
    pub fn keys(&self) -> ObjectKeys<'_> {
        self.inner.keys()
    }

    /// Iterates over the objects in the underlying map's order.
    pub fn values(&self) -> ObjectValues<'_> {
        self.inner.values().map(value_ref)
    }

    /// Returns true when both maps share the same root, which implies they
    /// hold the same objects. A false result does not imply they differ.
    pub fn ptr_eq(&self, other: &Self) -> bool {
        self.inner.ptr_eq(&other.inner)
    }

    /// Returns true when `id` is present in both maps and both entries are the
    /// same allocation, i.e. neither map has written that object since they
    /// were cloned from one another.
    pub fn shares_object_with(&self, other: &Self, id: ObjectId) -> bool {
        match (self.inner.get(&id), other.inner.get(&id)) {
            (Some(mine), Some(theirs)) => Arc::ptr_eq(mine, theirs),
            _ => false,
        }
    }

    /// The ids whose entries differ between this map and `other`: present in
    /// only one of them, or present in both as different allocations. Every
    /// other id is the same allocation in both maps and so holds an identical
    /// object. A rewrite that left an object's content unchanged is still
    /// reported.
    pub fn unshared_ids(&self, other: &Self) -> BTreeSet<ObjectId> {
        if self.ptr_eq(other) {
            return BTreeSet::new();
        }
        let rewritten = self
            .inner
            .iter()
            .filter(|&(id, mine)| {
                other
                    .inner
                    .get(id)
                    .is_none_or(|theirs| !Arc::ptr_eq(mine, theirs))
            })
            .map(|(id, _)| *id);
        let added = other
            .inner
            .keys()
            .filter(|id| !self.inner.contains_key(id))
            .copied();
        rewritten.chain(added).collect()
    }

    /// A map holding the entries of `ids` this map contains, each sharing its
    /// object's allocation with this map. Ids this map lacks are skipped.
    pub fn restricted_to(&self, ids: &BTreeSet<ObjectId>) -> Self {
        Self {
            inner: ids
                .iter()
                .filter_map(|id| self.inner.get(id).map(|object| (*id, Arc::clone(object))))
                .collect(),
        }
    }

    /// Applies `update` to exactly the objects for which `needs` returns true,
    /// visiting them in map iteration order, without unsharing any other
    /// object or trie node.
    ///
    /// When no object matches, the map is not touched at all and stays
    /// [`ptr_eq`](Self::ptr_eq) to its clones.
    ///
    /// Contract: the caller must pass a `needs` such that `!needs(id, object)`
    /// implies `update(id, object)` would leave the object (and any state
    /// `update` captures) unchanged, and `update` must not depend on having
    /// observed the skipped objects. Under that contract the result equals
    /// running `update` over [`iter_mut`](Self::iter_mut).
    pub fn update_where(
        &mut self,
        mut needs: impl FnMut(&ObjectId, &GameObject) -> bool,
        mut update: impl FnMut(&ObjectId, &mut GameObject),
    ) {
        let matching: Vec<ObjectId> = self
            .iter()
            .filter(|(id, object)| needs(id, object))
            .map(|(id, _)| *id)
            .collect();
        for id in &matching {
            if let Some(object) = self.get_mut(id) {
                update(id, object);
            }
        }
    }

    /// Deserializes the map and runs `migrate` on every object, in map
    /// iteration order, stopping at the first error.
    ///
    /// Entries are inserted in the order the underlying map's own
    /// `Deserialize` impl inserts them. Every object is uniquely owned at this
    /// point, so `migrate` never triggers a clone.
    pub(crate) fn deserialize_with_migration<'de, D, E>(
        deserializer: D,
        mut migrate: impl FnMut(&mut GameObject) -> Result<(), E>,
    ) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
        E: fmt::Display,
    {
        let mut inner = Inner::deserialize(deserializer)?;
        for (_, object) in inner.iter_mut() {
            migrate(Arc::make_mut(object)).map_err(serde::de::Error::custom)?;
        }
        Ok(Self { inner })
    }
}

/// Serializes the map in ascending `ObjectId` order, independent of hash
/// layout. This is the `serialize_with` adapter of `GameState::objects`.
pub(crate) fn serialize_sorted<S>(map: &ObjectMap, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    crate::types::deterministic_serde::im_hash_map(&map.inner, serializer)
}

impl fmt::Debug for ObjectMap {
    /// Formats exactly as the underlying map does (`Arc<GameObject>` prints as
    /// `GameObject`).
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Debug::fmt(&self.inner, f)
    }
}

impl Index<&ObjectId> for ObjectMap {
    type Output = GameObject;

    /// Returns the object stored under `id`. Panics, with the underlying
    /// map's message, when the id is absent.
    fn index(&self, id: &ObjectId) -> &GameObject {
        &self.inner[id]
    }
}

impl IndexMut<&ObjectId> for ObjectMap {
    /// Returns the object stored under `id` for writing, cloning it first if
    /// it is shared. Panics, with the underlying map's message, when the id is
    /// absent; it never inserts.
    fn index_mut(&mut self, id: &ObjectId) -> &mut GameObject {
        Arc::make_mut(&mut self.inner[id])
    }
}

impl<'a> IntoIterator for &'a ObjectMap {
    type Item = (&'a ObjectId, &'a GameObject);
    type IntoIter = ObjectIter<'a>;

    /// Iterates as [`ObjectMap::iter`] does.
    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

impl FromIterator<(ObjectId, GameObject)> for ObjectMap {
    /// Collects into the underlying map in the iterator's order, so the
    /// insertion sequence matches collecting into the bare map.
    fn from_iter<I: IntoIterator<Item = (ObjectId, GameObject)>>(iter: I) -> Self {
        Self {
            inner: iter
                .into_iter()
                .map(|(id, object)| (id, Arc::new(object)))
                .collect(),
        }
    }
}

impl Serialize for ObjectMap {
    /// Serializes exactly as the underlying map does (iteration order). The
    /// `GameState::objects` field uses [`serialize_sorted`] instead.
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.inner.serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ObjectMap {
    /// Deserializes exactly as the underlying map does.
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Inner::deserialize(deserializer).map(|inner| Self { inner })
    }
}

/// Shared assertions for the per-site tests of the guarded object sweeps
/// (`update_where` callers in `game/layers.rs`, `game/turns.rs`,
/// `game/exile_links.rs` and `game/triggers.rs`). Each sweep gets a sharing
/// witness, a targeted test and a reference-equality test built from these.
#[cfg(test)]
pub(crate) mod sweep_test_support {
    use crate::game::game_object::GameObject;
    use crate::game::zones::create_object;
    use crate::types::game_state::GameState;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// A fresh two-player state plus one object per `(owner, zone)` entry,
    /// returned with the new ids in the same order.
    pub(crate) fn state_with_objects(objects: &[(PlayerId, Zone)]) -> (GameState, Vec<ObjectId>) {
        let mut state = GameState::new_two_player(42);
        let ids = objects
            .iter()
            .enumerate()
            .map(|(index, &(owner, zone))| {
                create_object(
                    &mut state,
                    CardId(index as u64 + 1),
                    owner,
                    format!("Sweep Fixture {index}"),
                    zone,
                )
            })
            .collect();
        (state, ids)
    }

    /// Mutable access to one fixture object, panicking if it is missing.
    pub(crate) fn object_mut(state: &mut GameState, id: ObjectId) -> &mut GameObject {
        state.objects.get_mut(&id).expect("fixture object exists")
    }

    /// The whole state as a JSON value; the comparison form of every
    /// reference-equality test.
    pub(crate) fn state_value(state: &GameState) -> serde_json::Value {
        serde_json::to_value(state).expect("game state serializes")
    }

    /// One object as a JSON value.
    pub(crate) fn object_value(state: &GameState, id: ObjectId) -> serde_json::Value {
        serde_json::to_value(&state.objects[&id]).expect("game object serializes")
    }

    /// Sharing witness: runs `sweep` on a state in which no object needs the
    /// change and asserts the object store is still pointer-equal to a
    /// pre-call clone. Reach-guard: the store holds at least three objects.
    pub(crate) fn assert_sweep_keeps_store_shared(
        mut state: GameState,
        sweep: impl FnOnce(&mut GameState),
    ) -> GameState {
        assert!(
            state.objects.len() >= 3,
            "the witness needs at least three objects"
        );
        let before = state.clone();
        sweep(&mut state);
        assert!(
            state.objects.ptr_eq(&before.objects),
            "a sweep that changes nothing must leave the object store shared"
        );
        state
    }

    /// Targeted test: runs `sweep` and asserts that `target` was copied while
    /// every other object is still shared with the pre-call clone. Returns
    /// `(before, after)` so the caller can assert that the swept field of
    /// `target` changed (the reach-guard that `update` ran).
    pub(crate) fn run_sweep_targeting(
        mut state: GameState,
        target: ObjectId,
        sweep: impl FnOnce(&mut GameState),
    ) -> (GameState, GameState) {
        let before = state.clone();
        sweep(&mut state);
        assert!(
            !state.objects.shares_object_with(&before.objects, target),
            "the target object must have been written"
        );
        for id in before.objects.keys() {
            if *id != target {
                assert!(
                    state.objects.shares_object_with(&before.objects, *id),
                    "object {id:?} needs no change and must stay shared"
                );
            }
        }
        (before, state)
    }

    /// Reference equality: runs `sweep` and the BASE-shaped unguarded
    /// `reference` on clones of `state` and asserts the two whole states
    /// serialize identically. Reach-guards: the reference result differs from
    /// the input, and each id in `must_change` differs from its pre-call value
    /// in the reference result. Returns `(actual, reference)`.
    pub(crate) fn assert_sweep_matches_reference(
        state: &GameState,
        must_change: &[ObjectId],
        sweep: impl FnOnce(&mut GameState),
        reference: impl FnOnce(&mut GameState),
    ) -> (GameState, GameState) {
        let mut actual = state.clone();
        let mut expected = state.clone();
        reference(&mut expected);
        sweep(&mut actual);
        assert_ne!(
            state_value(&expected),
            state_value(state),
            "the fixture must give the reference sweep something to do"
        );
        for id in must_change {
            assert_ne!(
                object_value(&expected, *id),
                object_value(state, *id),
                "the reference sweep must change object {id:?}"
            );
        }
        assert_eq!(
            state_value(&actual),
            state_value(&expected),
            "the guarded sweep must produce exactly the unguarded result"
        );
        (actual, expected)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    /// The bare map the object store used before `ObjectMap`, used as the
    /// behavioural reference.
    type BareMap = im::HashMap<ObjectId, GameObject, FxBuildHasher>;

    /// Builds a distinguishable battlefield object for `id`.
    fn object(id: u64) -> GameObject {
        GameObject::new(
            ObjectId(id),
            CardId(id),
            PlayerId(0),
            format!("Object {id}"),
            Zone::Battlefield,
        )
    }

    /// Builds a map holding objects `1..=count`.
    fn map_of(count: u64) -> ObjectMap {
        (1..=count).map(|id| (ObjectId(id), object(id))).collect()
    }

    /// JSON value of one object; `GameObject` has no `PartialEq`.
    fn json(object: &GameObject) -> serde_json::Value {
        serde_json::to_value(object).expect("GameObject serializes")
    }

    /// Key sequence of an `ObjectMap` in iteration order.
    fn keys_of(map: &ObjectMap) -> Vec<ObjectId> {
        map.keys().copied().collect()
    }

    /// Key sequence of the bare reference map in iteration order.
    fn bare_keys_of(map: &BareMap) -> Vec<ObjectId> {
        map.keys().copied().collect()
    }

    /// Deterministic xorshift step for the op scripts below.
    fn next_random(seed: &mut u64) -> u64 {
        *seed ^= *seed << 13;
        *seed ^= *seed >> 7;
        *seed ^= *seed << 17;
        *seed
    }

    /// A clone shares the root and every object allocation with its source.
    #[test]
    fn clone_shares_every_object() {
        let map = map_of(40);
        assert!(map.len() >= 40);
        let clone = map.clone();
        assert!(clone.ptr_eq(&map));
        for id in keys_of(&map) {
            assert!(clone.shares_object_with(&map, id), "{id:?} not shared");
        }
        assert!(!clone.shares_object_with(&map, ObjectId(9_999)));
    }

    /// `get_mut` clones only the object it returns and leaves the source
    /// map's copy untouched.
    #[test]
    fn get_mut_unshares_only_the_written_object() {
        let original = map_of(40);
        assert!(original.len() >= 40);
        let mut written = original.clone();
        let target = ObjectId(17);
        let before = json(&original[&target]);

        written.get_mut(&target).expect("target exists").tapped = true;

        assert!(!written.shares_object_with(&original, target));
        for id in keys_of(&original) {
            if id != target {
                assert!(written.shares_object_with(&original, id), "{id:?} unshared");
            }
        }
        assert!(written[&target].tapped);
        assert!(!original[&target].tapped);
        assert_eq!(json(&original[&target]), before);
    }

    /// `insert` and `remove` return the same owned values as the bare map,
    /// whether or not the stored object is shared with a clone.
    #[test]
    fn insert_and_remove_return_owned_values_equal_to_reference() {
        let mut map = ObjectMap::new();
        let mut bare = BareMap::default();
        let mut seed = 0x9E37_79B9_7F4A_7C15_u64;
        let mut returned = 0usize;
        let mut clones = Vec::new();
        for step in 0..600u64 {
            let id = next_random(&mut seed) % 48;
            let (got, expected) = if next_random(&mut seed).is_multiple_of(3) {
                (map.remove(&ObjectId(id)), bare.remove(&ObjectId(id)))
            } else {
                let mut value = object(id);
                value.tapped = step % 2 == 0;
                (
                    map.insert(ObjectId(id), value.clone()),
                    bare.insert(ObjectId(id), value),
                )
            };
            assert_eq!(got.as_ref().map(json), expected.as_ref().map(json));
            returned += usize::from(got.is_some());
            assert_eq!(map.len(), bare.len());
            assert_eq!(map.is_empty(), bare.is_empty());
            assert_eq!(
                map.contains_key(&ObjectId(id)),
                bare.contains_key(&ObjectId(id))
            );
            assert_eq!(
                map.get(&ObjectId(id)).map(json),
                bare.get(&ObjectId(id)).map(json)
            );
            if step % 50 == 0 {
                // Keep some values shared so the shared-return path is covered.
                clones.push(map.clone());
            }
        }
        assert!(returned >= 100, "script returned only {returned} values");
        map.clear();
        assert!(map.is_empty());
    }

    /// Iteration order is that of a bare `im` map driven by the same
    /// operations, for every way the engine builds the store.
    #[test]
    fn iteration_order_matches_bare_im_map() {
        let mut map = ObjectMap::new();
        let mut bare = BareMap::default();
        let mut seed = 0x2545_F491_4F6C_DD1D_u64;
        let mut removals = 0usize;
        for _ in 0..900 {
            let id = next_random(&mut seed) % 400;
            if next_random(&mut seed).is_multiple_of(4) {
                removals += usize::from(map.remove(&ObjectId(id)).is_some());
                bare.remove(&ObjectId(id));
            } else {
                map.insert(ObjectId(id), object(id));
                bare.insert(ObjectId(id), object(id));
            }
            assert_eq!(keys_of(&map), bare_keys_of(&bare));
        }
        assert!(map.len() >= 200, "script left only {} objects", map.len());
        assert!(removals >= 50, "script removed only {removals} objects");

        let expected = bare_keys_of(&bare);
        let from_iter: Vec<ObjectId> = map.iter().map(|(id, _)| *id).collect();
        assert_eq!(from_iter, expected);
        let from_ref: Vec<ObjectId> = (&map).into_iter().map(|(id, _)| *id).collect();
        assert_eq!(from_ref, expected);
        let from_values: Vec<ObjectId> = map.values().map(|object| object.id).collect();
        assert_eq!(from_values, expected);
        let from_iter_mut: Vec<ObjectId> = map.clone().iter_mut().map(|(id, _)| *id).collect();
        assert_eq!(from_iter_mut, expected);
        assert_eq!(map.iter().len(), expected.len());
        assert_eq!(map.values().len(), expected.len());

        // `collect()` construction.
        let pairs: Vec<(ObjectId, GameObject)> = (0..300u64)
            .map(|_| next_random(&mut seed) % 1_000)
            .map(|id| (ObjectId(id), object(id)))
            .collect();
        let collected: ObjectMap = pairs.clone().into_iter().collect();
        let bare_collected: BareMap = pairs.into_iter().collect();
        assert!(collected.len() >= 200);
        assert_eq!(keys_of(&collected), bare_keys_of(&bare_collected));

        // Serde round-trip of both, from the same bytes.
        let wire = serde_json::to_string(&bare).expect("bare map serializes");
        let map_back: ObjectMap = serde_json::from_str(&wire).expect("ObjectMap deserializes");
        let bare_back: BareMap = serde_json::from_str(&wire).expect("bare map deserializes");
        assert_eq!(keys_of(&map_back), bare_keys_of(&bare_back));
        let migrated = ObjectMap::deserialize_with_migration(
            &mut serde_json::Deserializer::from_str(&wire),
            |_| Ok::<(), String>(()),
        )
        .expect("ObjectMap deserializes with migration");
        assert_eq!(keys_of(&migrated), bare_keys_of(&bare_back));
    }

    /// The migrating constructor visits every object once in iteration order
    /// and surfaces the first migration error.
    #[test]
    fn deserialize_with_migration_visits_every_object_in_order() {
        let map = map_of(40);
        let wire = serde_json::to_string(&map).expect("ObjectMap serializes");
        let mut visited = Vec::new();
        let migrated = ObjectMap::deserialize_with_migration(
            &mut serde_json::Deserializer::from_str(&wire),
            |object| {
                visited.push(object.id);
                object.tapped = true;
                Ok::<(), String>(())
            },
        )
        .expect("migration succeeds");
        assert!(migrated.len() >= 40);
        assert_eq!(visited, keys_of(&migrated));
        assert!(migrated.values().all(|object| object.tapped));

        let error = ObjectMap::deserialize_with_migration(
            &mut serde_json::Deserializer::from_str(&wire),
            |_| Err::<(), String>("migration refused".to_string()),
        )
        .expect_err("migration error is surfaced");
        assert!(error.to_string().contains("migration refused"));
    }

    /// Both serialization paths emit the bytes the bare map emitted.
    #[test]
    fn field_serialization_is_byte_identical_to_bare_map_adapter() {
        let mut map = ObjectMap::new();
        let mut bare = BareMap::default();
        let mut seed = 0xD1B5_4A32_D192_ED03_u64;
        for _ in 0..120 {
            let id = next_random(&mut seed) % 5_000;
            map.insert(ObjectId(id), object(id));
            bare.insert(ObjectId(id), object(id));
        }
        assert!(map.len() >= 64);

        let mut sorted = Vec::new();
        serialize_sorted(&map, &mut serde_json::Serializer::new(&mut sorted))
            .expect("sorted adapter serializes");
        let mut bare_sorted = Vec::new();
        crate::types::deterministic_serde::im_hash_map(
            &bare,
            &mut serde_json::Serializer::new(&mut bare_sorted),
        )
        .expect("bare adapter serializes");
        assert_eq!(sorted, bare_sorted);

        let plain = serde_json::to_string(&map).expect("ObjectMap serializes");
        let bare_plain = serde_json::to_string(&bare).expect("bare map serializes");
        assert_eq!(plain, bare_plain);
        // The two paths differ for this key set, so each assertion above pins
        // its own path.
        assert_ne!(sorted, plain.into_bytes());
    }

    /// `Debug` output is the bare map's, including its key ordering.
    #[test]
    fn debug_output_matches_bare_im_map() {
        let mut map = ObjectMap::new();
        let mut bare = BareMap::default();
        let mut seed = 0xA076_1D64_78BD_642F_u64;
        for _ in 0..90 {
            let id = next_random(&mut seed) % 5_000;
            map.insert(ObjectId(id), object(id));
            bare.insert(ObjectId(id), object(id));
        }
        assert!(map.len() >= 64);
        let keys = keys_of(&map);
        assert!(
            !keys.windows(2).all(|pair| pair[0] < pair[1]),
            "fixture keys iterate in ascending order; an iteration-order Debug \
             would be indistinguishable from the bare map's"
        );
        assert_eq!(format!("{map:?}"), format!("{bare:?}"));
    }

    /// Indexing a missing key panics with the bare map's message.
    #[test]
    #[should_panic(expected = "HashMap::index: invalid key")]
    fn index_panics_match_bare_im_map() {
        let map = map_of(3);
        let _ = &map[&ObjectId(99)];
    }

    /// Mutably indexing a missing key panics with the bare map's message and
    /// does not insert.
    #[test]
    #[should_panic(expected = "HashMap::index_mut: invalid key")]
    fn index_mut_panics_match_bare_im_map() {
        let mut map = map_of(3);
        map[&ObjectId(99)].tapped = true;
    }

    /// `IndexMut` on a present key writes copy-on-write, like `get_mut`.
    #[test]
    fn index_mut_unshares_only_the_written_object() {
        let original = map_of(8);
        assert!(original.len() >= 8);
        let mut written = original.clone();
        written[&ObjectId(3)].tapped = true;
        assert!(written[&ObjectId(3)].tapped);
        assert!(!original[&ObjectId(3)].tapped);
        for id in keys_of(&original) {
            assert_eq!(written.shares_object_with(&original, id), id != ObjectId(3));
        }
    }

    /// `update_where` calls `update` for exactly the matching ids, in
    /// iteration order, and asks `needs` about every object once.
    #[test]
    fn update_where_visits_exactly_matching_ids_in_key_order() {
        let mut map = ObjectMap::new();
        let mut seed = 0x1234_5678_9ABC_DEF1_u64;
        for _ in 0..120 {
            let id = next_random(&mut seed) % 5_000;
            map.insert(ObjectId(id), object(id));
        }
        assert!(map.len() >= 64);
        let expected: Vec<ObjectId> = keys_of(&map)
            .into_iter()
            .filter(|id| id.0 % 3 == 0)
            .collect();
        assert!(expected.len() >= 8);
        assert!(
            !expected.windows(2).all(|pair| pair[0] < pair[1]),
            "matching keys iterate in ascending order; a sorted visit order \
             would be indistinguishable from iteration order"
        );

        let mut asked = Vec::new();
        let mut visited = Vec::new();
        map.update_where(
            |id, object| {
                assert_eq!(*id, object.id);
                asked.push(*id);
                id.0 % 3 == 0
            },
            |id, object| {
                assert_eq!(*id, object.id);
                visited.push(*id);
                object.tapped = true;
            },
        );

        assert_eq!(asked, keys_of(&map));
        assert_eq!(visited, expected);
        for (id, object) in &map {
            assert_eq!(object.tapped, id.0 % 3 == 0);
        }
    }

    /// Objects `needs` rejects stay shared with a pre-call clone.
    #[test]
    fn update_where_keeps_non_matching_objects_shared() {
        let mut map = map_of(40);
        assert!(map.len() >= 40);
        let before = map.clone();

        map.update_where(|id, _| id.0 == 7, |_, object| object.tapped = true);

        assert!(map[&ObjectId(7)].tapped);
        assert!(!before[&ObjectId(7)].tapped);
        assert!(!map.shares_object_with(&before, ObjectId(7)));
        for id in keys_of(&before) {
            if id != ObjectId(7) {
                assert!(map.shares_object_with(&before, id), "{id:?} unshared");
            }
        }
    }

    /// With no match the map is not touched, so the root stays shared.
    #[test]
    fn update_where_with_no_match_keeps_root_shared() {
        let mut map = map_of(40);
        assert!(map.len() >= 40);
        let before = map.clone();
        let mut asked = 0usize;

        map.update_where(
            |_, _| {
                asked += 1;
                false
            },
            |_, _| panic!("update must not run when nothing matches"),
        );

        assert_eq!(asked, 40);
        assert!(map.ptr_eq(&before));
    }

    /// A clone has no unshared id. After writes, `unshared_ids` names exactly
    /// the written, inserted and removed ids, in both directions.
    #[test]
    fn unshared_ids_is_empty_for_a_clone_and_names_each_write() {
        let original = map_of(40);
        assert!(original.len() >= 40);
        assert!(original.unshared_ids(&original.clone()).is_empty());

        let mut written = original.clone();
        written
            .get_mut(&ObjectId(7))
            .expect("object 7 exists")
            .tapped = true;
        written.insert(ObjectId(99), object(99));
        written.remove(&ObjectId(12));

        let expected = BTreeSet::from([ObjectId(7), ObjectId(12), ObjectId(99)]);
        assert_eq!(original.unshared_ids(&written), expected);
        assert_eq!(written.unshared_ids(&original), expected);
        assert!(!original.unshared_ids(&written).contains(&ObjectId(8)));
    }

    /// Identity, not equality: a write that leaves an object's content
    /// unchanged still unshares it, so the id is reported.
    #[test]
    fn unshared_ids_names_a_rewrite_with_equal_content() {
        let original = map_of(40);
        assert!(original.len() >= 40);
        let mut written = original.clone();
        let _ = written.get_mut(&ObjectId(21)).expect("object 21 exists");

        assert_eq!(
            json(&written[&ObjectId(21)]),
            json(&original[&ObjectId(21)])
        );
        assert_eq!(
            original.unshared_ids(&written),
            BTreeSet::from([ObjectId(21)])
        );
    }

    /// `restricted_to` keeps the selected objects as the same allocations,
    /// skips ids the map lacks, and serializes through the store's adapter as
    /// the store's own serialized map filtered to those ids.
    #[test]
    fn restricted_to_shares_and_serializes_like_the_store() {
        let map = map_of(40);
        assert!(map.len() >= 40);
        let ids = BTreeSet::from([ObjectId(3), ObjectId(17), ObjectId(31), ObjectId(500)]);

        let restricted = map.restricted_to(&ids);

        assert_eq!(
            keys_of(&restricted).into_iter().collect::<BTreeSet<_>>(),
            BTreeSet::from([ObjectId(3), ObjectId(17), ObjectId(31)])
        );
        for id in [ObjectId(3), ObjectId(17), ObjectId(31)] {
            assert!(restricted.shares_object_with(&map, id), "{id:?} copied");
        }
        let mut whole =
            serialize_sorted(&map, serde_json::value::Serializer).expect("the store serializes");
        whole
            .as_object_mut()
            .expect("the store serializes as a map")
            .retain(|key, _| ["3", "17", "31"].contains(&key.as_str()));
        assert_eq!(whole.as_object().map(serde_json::Map::len), Some(3));
        assert_eq!(
            serialize_sorted(&restricted, serde_json::value::Serializer)
                .expect("the restriction serializes"),
            whole
        );
    }
}

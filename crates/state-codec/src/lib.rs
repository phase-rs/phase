//! Exact JSON-value storage and revisioned FULL/DELTA frames for viewer projections.
//!
//! [`WireState::from_game_state`] uses the engine's deterministic serializer;
//! [`WireState::from_json`] retains the exact JSON value span, including numeric
//! spelling, duplicate keys and internal whitespace. Only outer whitespace is
//! discarded. [`state_hash`] hashes those bytes, never revision metadata.
//!
//! FULL is baseline-independent. DELTA requires matching baseline revision and
//! SHA-256, applies named field/object operations transactionally, and verifies
//! the reconstructed hash. Hashes detect corruption/staleness, not authentication
//! or audience identity. Callers own separate delivered baselines per recipient.
//! Revisions are supplied explicitly, never inferred from JSON or incremented.
//!
//! Structural maps must be uniquely keyed and exactly reproducible by serde;
//! otherwise encoding falls back to FULL. Opaque values retain their raw bytes.
//! DELTA is selected only when its complete serialized size is strictly smaller
//! than FULL. Object comparisons use cached whole-object SHA-256 digests; checked
//! reconstruction still detects omitted changes, including a digest collision.
//! Indexing costs O(input bytes + key indexing), not O(changed objects).
//!
//! Viewer projections must **not** pass through `GameState` deserialization:
//! that decoder can normalize allocators or reject redacted authority. Codec
//! decoding does not establish trusted restore authority or validate game rules.
//!
//! Native verification: `cargo test -p state-codec --test integration`.
//! Opt-in full games: `cargo test -p state-codec self_play::full_ -- --ignored --nocapture`.
//! These tests do not claim independent native/WASM state-construction parity.
//! Add `-- --nocapture` to emit same-stream raw/gzip, operation/order overhead,
//! baseline-memory and cold/warm stage timings per audience. Reconnect delivery
//! is measured separately from the uninterrupted lane. Debug timings are smoke
//! diagnostics only; CPU acceptance measurements require an optimized build.

use std::{
    borrow::Cow,
    fmt,
    io::{self, Write},
    ops::Range,
    sync::{Arc, OnceLock},
};

use engine::types::game_state::GameState;
use serde::{
    de::{self, MapAccess, Visitor},
    ser::SerializeMap,
    Deserialize, Deserializer, Serialize, Serializer,
};
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};

/// An immutable JSON value and optional publication revision; not a game restore.
#[derive(Clone, Debug)]
pub struct WireState {
    json: Arc<RawValue>,
    revision: Option<u64>,
    cache: Arc<StateCache>,
}

#[derive(Debug, Default)]
struct StateCache {
    hash: OnceLock<StateHash>,
    structure: OnceLock<Eligibility>,
}

/// Retained bytes for one unique baseline allocation, not per cloned handle.
///
/// Includes initialized cache storage and vector/key capacities. Object digest
/// storage is separated from index bytes, never counted twice. Excludes allocator
/// headers, Arc bookkeeping and the small caller-owned `WireState` handle.
#[derive(Clone, Copy, Debug, Default)]
pub struct BaselineMemoryUsage {
    pub json_bytes: usize,
    pub index_key_bytes: usize,
    pub object_hash_bytes: usize,
}

impl BaselineMemoryUsage {
    pub fn total_bytes(self) -> usize {
        self.json_bytes + self.index_key_bytes + self.object_hash_bytes
    }
}

/// An opaque, baseline-independent FULL or integrity-checked DELTA frame.
#[derive(Clone, Debug)]
pub struct Frame {
    payload: FramePayload,
}

/// The FULL or DELTA payload carried by a frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameKind {
    Full,
    Delta,
}

#[derive(Clone, Debug)]
enum FramePayload {
    Full(WireState),
    Delta(Delta),
}

#[derive(Clone, Debug)]
struct Delta {
    base_revision: u64,
    revision: u64,
    base_hash: StateHash,
    result_hash: StateHash,
    // One owned operation document; no baseline chain or copied structural trees.
    patch: Arc<RawValue>,
}

/// SHA-256 of the complete retained JSON value bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StateHash([u8; 32]);

impl fmt::Display for StateHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for byte in self.0 {
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

impl Serialize for StateHash {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for StateHash {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = Cow::<'de, str>::deserialize(deserializer)?;
        if text.len() != 64 {
            return Err(de::Error::custom(
                "hash must contain 64 lowercase hex digits",
            ));
        }
        let digit = |byte| match byte {
            b'0'..=b'9' => Some(byte - b'0'),
            b'a'..=b'f' => Some(byte - b'a' + 10),
            _ => None,
        };
        let mut bytes = [0; 32];
        for (slot, pair) in bytes.iter_mut().zip(text.as_bytes().chunks_exact(2)) {
            let high = digit(pair[0]).ok_or_else(|| de::Error::custom("invalid hash digit"))?;
            let low = digit(pair[1]).ok_or_else(|| de::Error::custom("invalid hash digit"))?;
            *slot = (high << 4) | low;
        }
        Ok(Self(bytes))
    }
}

/// JSON sources, malformed framing, and transactional DELTA integrity failures.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum CodecError {
    #[error("invalid codec JSON: {0}")]
    Json(#[from] serde_json::Error),
    #[error("malformed codec frame: {0}")]
    Malformed(&'static str),
    #[error("DELTA requires a baseline")]
    MissingBaseline,
    #[error("baseline revision mismatch: expected {expected}, actual {actual:?}")]
    BaseRevisionMismatch { expected: u64, actual: Option<u64> },
    #[error("baseline hash mismatch: expected {expected}, actual {actual}")]
    BaseHashMismatch {
        expected: StateHash,
        actual: StateHash,
    },
    #[error("result hash mismatch: expected {expected}, actual {actual}")]
    ResultHashMismatch {
        expected: StateHash,
        actual: StateHash,
    },
    #[error("invalid DELTA: {0}")]
    InvalidDelta(&'static str),
}

impl WireState {
    fn from_raw(json: Arc<RawValue>, revision: Option<u64>) -> Self {
        Self {
            json,
            revision,
            cache: Arc::default(),
        }
    }

    /// Serialize deterministically and retain the engine revision outside JSON.
    pub fn from_game_state(state: &GameState) -> Result<Self, CodecError> {
        Ok(Self::from_raw(
            Arc::from(serde_json::value::to_raw_value(state)?),
            Some(state.state_revision),
        ))
    }

    /// Validate one exact JSON value, excluding outer whitespace; no revision inference.
    pub fn from_json(bytes: &[u8]) -> Result<Self, CodecError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CodecError::Malformed("state is not valid UTF-8"))?;
        let json: Box<RawValue> = serde_json::from_str(text)?;
        Ok(Self::from_raw(Arc::from(json), None))
    }

    /// Attach publication metadata while sharing the immutable JSON and cache.
    pub fn with_revision(mut self, revision: u64) -> Self {
        self.revision = Some(revision);
        self
    }

    pub fn revision(&self) -> Option<u64> {
        self.revision
    }
    pub fn as_json(&self) -> &str {
        self.json.get()
    }
    pub fn as_bytes(&self) -> &[u8] {
        self.as_json().as_bytes()
    }

    /// Initialize this baseline's shared index/hash cache and account its storage.
    pub fn baseline_memory_usage(&self) -> BaselineMemoryUsage {
        state_hash(self);
        let mut usage = BaselineMemoryUsage {
            json_bytes: self.as_bytes().len(),
            index_key_bytes: std::mem::size_of::<StateCache>(),
            object_hash_bytes: 0,
        };
        if let Eligibility::Eligible(shape) = self.structure() {
            shape.root.account(&mut usage);
            if let Some(data) = &shape.data {
                data.account(&mut usage);
            }
            if let Some(state) = &shape.state {
                state.account(&mut usage);
            }
            if let Some(objects) = &shape.objects {
                objects.account(&mut usage);
            }
        }
        usage
    }

    fn structure(&self) -> &Eligibility {
        self.cache
            .structure
            .get_or_init(|| match Shape::index(self.as_json()) {
                Ok(shape) => Eligibility::Eligible(shape),
                Err(_) => Eligibility::FullOnly,
            })
    }
}

/// Produce FULL without a baseline, or an exactly verified revisioned DELTA.
pub fn encode(baseline: Option<&WireState>, state: &WireState) -> Result<Frame, CodecError> {
    let full = || Frame {
        payload: FramePayload::Full(state.clone()),
    };
    let Some(base) = baseline else {
        return Ok(full());
    };
    // Read only the named root map, before any revision/layout eligibility work.
    // Duplicate or non-object roots are FULL-only, not encode errors.
    if let Ok(root) = serde_json::from_str::<RawMap<'_>>(state.as_json()) {
        if root.get("type").is_some_and(|raw| {
            serde_json::from_str::<Cow<'_, str>>(raw.get())
                .ok()
                .as_deref()
                == Some("GameStarted")
        }) {
            return Ok(full());
        }
    }
    let (Some(base_revision), Some(revision)) = (base.revision, state.revision) else {
        return Ok(full());
    };
    let (Eligibility::Eligible(before), Eligibility::Eligible(after)) =
        (base.structure(), state.structure())
    else {
        return Ok(full());
    };
    if before.layout != after.layout {
        return Ok(full());
    }
    let patch = diff(base, before, state, after)?;
    let assembled = assemble(base, before, &patch)?;
    let mut check = Comparison::new(state.as_bytes());
    serde_json::to_writer(&mut check, &assembled)?;
    if !check.matches() {
        return Ok(full());
    }
    let base_hash = state_hash(base);
    let result_hash = state_hash(state);
    let candidate = DeltaWriteData {
        base_revision,
        revision,
        base_hash,
        result_hash,
        patch: &patch,
    };
    let delta_size = serialized_size(&FrameEnvelope {
        tag: "Delta",
        data: candidate,
    })?;
    let full_size = serialized_size(&FrameEnvelope {
        tag: "Full",
        data: FullData {
            state: &state.json,
            revision: state.revision,
        },
    })?;
    if !prefer_delta(delta_size, full_size) {
        return Ok(full());
    }
    Ok(Frame {
        payload: FramePayload::Delta(Delta {
            base_revision,
            revision,
            base_hash,
            result_hash,
            patch: Arc::from(serde_json::value::to_raw_value(&patch)?),
        }),
    })
}

/// Check base identity, validate all operations, then expose only a verified result.
pub fn decode(baseline: Option<&WireState>, frame: &Frame) -> Result<WireState, CodecError> {
    match &frame.payload {
        FramePayload::Full(state) => Ok(state.clone()),
        FramePayload::Delta(delta) => {
            let base = baseline.ok_or(CodecError::MissingBaseline)?;
            if base.revision != Some(delta.base_revision) {
                return Err(CodecError::BaseRevisionMismatch {
                    expected: delta.base_revision,
                    actual: base.revision,
                });
            }
            let actual = state_hash(base);
            if actual != delta.base_hash {
                return Err(CodecError::BaseHashMismatch {
                    expected: delta.base_hash,
                    actual,
                });
            }
            let Eligibility::Eligible(shape) = base.structure() else {
                return Err(CodecError::InvalidDelta(
                    "baseline is not structurally eligible",
                ));
            };
            let patch = Patch::parse(&delta.patch)?;
            let assembled = assemble(base, shape, &patch)?;
            let bytes = serde_json::to_vec(&assembled)?;
            let result = WireState::from_json(&bytes)?.with_revision(delta.revision);
            let actual = state_hash(&result);
            if actual != delta.result_hash {
                return Err(CodecError::ResultHashMismatch {
                    expected: delta.result_hash,
                    actual,
                });
            }
            Ok(result)
        }
    }
}

pub fn state_hash(state: &WireState) -> StateHash {
    *state
        .cache
        .hash
        .get_or_init(|| StateHash(Sha256::digest(state.as_bytes()).into()))
}

fn prefer_delta(delta_bytes: usize, full_bytes: usize) -> bool {
    delta_bytes < full_bytes
}

#[derive(Default)]
struct CountingWriter(usize);

impl Write for CountingWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len())
            .ok_or_else(|| io::Error::other("serialized size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

fn serialized_size(value: &impl Serialize) -> Result<usize, CodecError> {
    let mut writer = CountingWriter::default();
    serde_json::to_writer(&mut writer, value)?;
    Ok(writer.0)
}

#[derive(Serialize)]
struct DeltaWriteData<'a, 'b> {
    base_revision: u64,
    revision: u64,
    base_hash: StateHash,
    result_hash: StateHash,
    patch: &'b Patch<'a>,
}

// The envelope remains a two-stage borrowed RawValue reader, never serde content.
#[derive(Deserialize)]
struct Envelope<'a> {
    #[serde(rename = "type", borrow)]
    tag: Cow<'a, str>,
    #[serde(borrow)]
    data: &'a RawValue,
}

#[derive(Deserialize, Serialize)]
struct FullData<'a> {
    #[serde(borrow)]
    state: &'a RawValue,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    revision: Option<u64>,
}

#[derive(Serialize)]
struct FrameEnvelope<'a, T> {
    #[serde(rename = "type")]
    tag: &'a str,
    data: T,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct DeltaData<'a> {
    base_revision: u64,
    revision: u64,
    base_hash: StateHash,
    result_hash: StateHash,
    #[serde(borrow)]
    patch: &'a RawValue,
}

fn object_record<'a, T: Deserialize<'a>>(raw: &'a RawValue) -> Result<T, CodecError> {
    if !raw.get().starts_with('{') {
        return Err(CodecError::Malformed("record must be a JSON object"));
    }
    Ok(serde_json::from_str(raw.get())?)
}

impl Frame {
    /// Identify the payload without serializing the frame.
    pub fn kind(&self) -> FrameKind {
        match &self.payload {
            FramePayload::Full(_) => FrameKind::Full,
            FramePayload::Delta(_) => FrameKind::Delta,
        }
    }

    /// Read named records, accepting either envelope order and escaped tag spellings.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self, CodecError> {
        let text = std::str::from_utf8(bytes)
            .map_err(|_| CodecError::Malformed("frame is not valid UTF-8"))?;
        let raw: &RawValue = serde_json::from_str(text)?;
        let envelope: Envelope<'_> = object_record(raw)?;
        let payload = match envelope.tag.as_ref() {
            "Full" => {
                let data: FullData<'_> = object_record(envelope.data)?;
                FramePayload::Full(WireState::from_raw(
                    Arc::from(data.state.to_owned()),
                    data.revision,
                ))
            }
            "Delta" => {
                let data: DeltaData<'_> = object_record(envelope.data)?;
                Patch::parse(data.patch)?;
                FramePayload::Delta(Delta {
                    base_revision: data.base_revision,
                    revision: data.revision,
                    base_hash: data.base_hash,
                    result_hash: data.result_hash,
                    patch: Arc::from(data.patch.to_owned()),
                })
            }
            _ => return Err(CodecError::Malformed("unsupported frame tag")),
        };
        Ok(Self { payload })
    }

    /// Write compact type-first framing; raw leaves are never normalized.
    pub fn to_bytes(&self) -> Result<Vec<u8>, CodecError> {
        match &self.payload {
            FramePayload::Full(state) => Ok(serde_json::to_vec(&FrameEnvelope {
                tag: "Full",
                data: FullData {
                    state: &state.json,
                    revision: state.revision,
                },
            })?),
            FramePayload::Delta(delta) => {
                // Serialize the typed operation records, not their original whitespace.
                let patch = Patch::parse(&delta.patch)?;
                Ok(serde_json::to_vec(&FrameEnvelope {
                    tag: "Delta",
                    data: DeltaWriteData {
                        base_revision: delta.base_revision,
                        revision: delta.revision,
                        base_hash: delta.base_hash,
                        result_hash: delta.result_hash,
                        patch: &patch,
                    },
                })?)
            }
        }
    }
}

// Ordered structural maps. The visitor accepts objects only and checks decoded
// keys (including escape aliases); opaque leaf bodies are never expanded.
#[derive(Debug, Default)]
struct RawMap<'a> {
    entries: Vec<(Cow<'a, str>, &'a RawValue)>,
    lookup: Vec<usize>,
}

impl<'a> RawMap<'a> {
    fn new(entries: Vec<(Cow<'a, str>, &'a RawValue)>) -> Result<Self, CodecError> {
        let mut lookup: Vec<_> = (0..entries.len()).collect();
        lookup.sort_unstable_by(|&a, &b| entries[a].0.cmp(&entries[b].0));
        if lookup
            .windows(2)
            .any(|pair| entries[pair[0]].0 == entries[pair[1]].0)
        {
            return Err(CodecError::InvalidDelta("duplicate decoded map key"));
        }
        Ok(Self { entries, lookup })
    }

    fn get(&self, key: &str) -> Option<&'a RawValue> {
        self.lookup
            .binary_search_by(|&i| self.entries[i].0.as_ref().cmp(key))
            .ok()
            .map(|i| self.entries[self.lookup[i]].1)
    }

    fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

impl<'de: 'a, 'a> Deserialize<'de> for RawMap<'a> {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct ObjectVisitor;
        impl<'de> Visitor<'de> for ObjectVisitor {
            type Value = RawMap<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a unique-key JSON object")
            }
            fn visit_map<M: MapAccess<'de>>(self, mut map: M) -> Result<Self::Value, M::Error> {
                let mut entries = Vec::new();
                while let Some((key, value)) = map.next_entry::<Cow<'de, str>, &'de RawValue>()? {
                    entries.push((key, value));
                }
                RawMap::new(entries).map_err(de::Error::custom)
            }
        }
        deserializer.deserialize_map(ObjectVisitor)
    }
}

impl Serialize for RawMap<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let mut map = serializer.serialize_map(Some(self.entries.len()))?;
        for (key, value) in &self.entries {
            map.serialize_entry(key, value)?;
        }
        map.end()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
enum PatchLayout {
    State,
    Message,
}

#[derive(Debug)]
enum Eligibility {
    Eligible(Shape),
    FullOnly,
}

#[derive(Debug)]
struct IndexedEntry<D = ()> {
    key: Box<str>,
    range: Range<usize>,
    digest: D,
}

#[derive(Debug)]
struct MapIndex<D = ()> {
    entries: Vec<IndexedEntry<D>>,
    lookup: Vec<usize>,
}

impl MapIndex {
    fn index(backing: &str, raw: &RawValue) -> Result<Self, CodecError> {
        Self::index_with(backing, raw, |_| ())
    }
}

impl<D> MapIndex<D> {
    fn index_with(
        backing: &str,
        raw: &RawValue,
        digest: impl Fn(&RawValue) -> D,
    ) -> Result<Self, CodecError> {
        let map: RawMap<'_> = serde_json::from_str(raw.get())?;
        let mut comparison = Comparison::new(raw.get().as_bytes());
        serde_json::to_writer(&mut comparison, &map)?;
        if !comparison.matches() {
            return Err(CodecError::InvalidDelta("non-lossless structural map"));
        }
        let entries = map
            .entries
            .into_iter()
            .map(|(key, value)| {
                let start = (value.get().as_ptr() as usize)
                    .checked_sub(backing.as_ptr() as usize)
                    .ok_or(CodecError::InvalidDelta("raw span outside backing"))?;
                let end = start
                    .checked_add(value.get().len())
                    .ok_or(CodecError::InvalidDelta("raw span overflow"))?;
                if backing.get(start..end) != Some(value.get()) {
                    return Err(CodecError::InvalidDelta("raw span outside backing"));
                }
                Ok(IndexedEntry {
                    key: key.into_owned().into_boxed_str(),
                    range: start..end,
                    digest: digest(value),
                })
            })
            .collect::<Result<_, CodecError>>()?;
        Ok(Self {
            entries,
            lookup: map.lookup,
        })
    }

    fn get<'a>(&self, backing: &'a str, key: &str) -> Result<Option<&'a RawValue>, CodecError> {
        let Ok(i) = self
            .lookup
            .binary_search_by(|&i| self.entries[i].key.as_ref().cmp(key))
        else {
            return Ok(None);
        };
        self.raw(backing, &self.entries[self.lookup[i]]).map(Some)
    }

    fn raw<'a>(
        &self,
        backing: &'a str,
        entry: &IndexedEntry<D>,
    ) -> Result<&'a RawValue, CodecError> {
        let text = backing
            .get(entry.range.clone())
            .ok_or(CodecError::InvalidDelta("invalid cached raw span"))?;
        Ok(serde_json::from_str(text)?)
    }

    fn borrow<'a>(&'a self, backing: &'a str) -> Result<RawMap<'a>, CodecError> {
        let entries = self
            .entries
            .iter()
            .map(|entry| Ok((Cow::Borrowed(entry.key.as_ref()), self.raw(backing, entry)?)))
            .collect::<Result<_, CodecError>>()?;
        Ok(RawMap {
            entries,
            lookup: self.lookup.clone(),
        })
    }

    fn entry(&self, key: &str) -> Option<&IndexedEntry<D>> {
        self.lookup
            .binary_search_by(|&i| self.entries[i].key.as_ref().cmp(key))
            .ok()
            .map(|i| &self.entries[self.lookup[i]])
    }

    fn account(&self, usage: &mut BaselineMemoryUsage) {
        let hashes = self.entries.capacity() * std::mem::size_of::<D>();
        usage.object_hash_bytes += hashes;
        usage.index_key_bytes += self.entries.capacity() * std::mem::size_of::<IndexedEntry<D>>()
            - hashes
            + self.lookup.capacity() * std::mem::size_of::<usize>()
            + self
                .entries
                .iter()
                .map(|entry| entry.key.len())
                .sum::<usize>();
    }
}

#[derive(Debug)]
struct Shape {
    layout: PatchLayout,
    root: MapIndex,
    data: Option<MapIndex>,
    state: Option<MapIndex>,
    objects: Option<MapIndex<StateHash>>,
}

impl Shape {
    fn index(backing: &str) -> Result<Self, CodecError> {
        let root_raw: &RawValue = serde_json::from_str(backing)?;
        let root = MapIndex::index(backing, root_raw)?;
        let (layout, data, state) = match root.get(backing, "type")? {
            None => (PatchLayout::State, None, None),
            Some(tag) => {
                let tag: Cow<'_, str> = serde_json::from_str(tag.get())?;
                if tag != "StateUpdate" {
                    return Err(CodecError::InvalidDelta("message kind is FULL-only"));
                }
                let data_raw = root
                    .get(backing, "data")?
                    .ok_or(CodecError::InvalidDelta("missing message data"))?;
                let data = MapIndex::index(backing, data_raw)?;
                let state_raw = data
                    .get(backing, "state")?
                    .ok_or(CodecError::InvalidDelta("missing message state"))?;
                let state = MapIndex::index(backing, state_raw)?;
                (PatchLayout::Message, Some(data), Some(state))
            }
        };
        let state_map = state.as_ref().unwrap_or(&root);
        let objects = match state_map.get(backing, "objects")? {
            Some(raw) if raw.get().starts_with('{') => {
                let objects = MapIndex::index_with(backing, raw, |body| {
                    StateHash(Sha256::digest(body.get().as_bytes()).into())
                })?;
                for entry in &objects.entries {
                    if !objects.raw(backing, entry)?.get().starts_with('{') {
                        return Err(CodecError::InvalidDelta("object body must be an object"));
                    }
                }
                Some(objects)
            }
            Some(_) | None => None,
        };
        Ok(Self {
            layout,
            root,
            data,
            state,
            objects,
        })
    }

    fn state(&self) -> &MapIndex {
        self.state.as_ref().unwrap_or(&self.root)
    }
}

// A checked Write sink, not a token editor or parser. It never allocates output.
struct Comparison<'a> {
    expected: &'a [u8],
    offset: usize,
    equal: bool,
}

impl<'a> Comparison<'a> {
    fn new(expected: &'a [u8]) -> Self {
        Self {
            expected,
            offset: 0,
            equal: true,
        }
    }
    fn matches(&self) -> bool {
        self.equal && self.offset == self.expected.len()
    }
}

impl Write for Comparison<'_> {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        match self.offset.checked_add(bytes.len()) {
            Some(end) => {
                self.equal &= self.expected.get(self.offset..end) == Some(bytes);
                self.offset = end;
            }
            None => self.equal = false,
        }
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct FieldPatch<'a> {
    #[serde(default, borrow, skip_serializing_if = "RawMap::is_empty")]
    set: RawMap<'a>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remove: Vec<Cow<'a, str>>,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    order: Option<Vec<Cow<'a, str>>>,
}

#[derive(Default, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct ObjectPatch<'a> {
    #[serde(default, borrow, skip_serializing_if = "RawMap::is_empty")]
    upsert: RawMap<'a>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    remove: Vec<Cow<'a, str>>,
    #[serde(
        default,
        deserialize_with = "present_value",
        skip_serializing_if = "Option::is_none"
    )]
    order: Option<Vec<Cow<'a, str>>>,
}

#[derive(Serialize)]
struct Patch<'a> {
    layout: PatchLayout,
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<FieldPatch<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<FieldPatch<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    state: Option<FieldPatch<'a>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    objects: Option<ObjectPatch<'a>>,
}

// Unlike Option's default reader, a present null is retained and then rejected
// as a non-object operation record, rather than silently becoming "unchanged".
fn present_raw<'de, D: Deserializer<'de>>(de: D) -> Result<Option<&'de RawValue>, D::Error> {
    <&RawValue>::deserialize(de).map(Some)
}

// Optional metadata/order is omitted or typed, never an accepted null no-op.
fn present_value<'de, D, T>(de: D) -> Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    T::deserialize(de).map(Some)
}

impl<'a> Patch<'a> {
    fn parse(raw: &'a RawValue) -> Result<Self, CodecError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Records<'a> {
            layout: PatchLayout,
            #[serde(default, borrow, deserialize_with = "present_raw")]
            message: Option<&'a RawValue>,
            #[serde(default, borrow, deserialize_with = "present_raw")]
            data: Option<&'a RawValue>,
            #[serde(default, borrow, deserialize_with = "present_raw")]
            state: Option<&'a RawValue>,
            #[serde(default, borrow, deserialize_with = "present_raw")]
            objects: Option<&'a RawValue>,
        }
        let records: Records<'_> = object_record(raw)?;
        let patch = Self {
            layout: records.layout,
            message: records.message.map(object_record).transpose()?,
            data: records.data.map(object_record).transpose()?,
            state: records.state.map(object_record).transpose()?,
            objects: records.objects.map(object_record).transpose()?,
        };
        if patch.layout == PatchLayout::State && (patch.message.is_some() || patch.data.is_some()) {
            return Err(CodecError::InvalidDelta(
                "bare state has message/data operations",
            ));
        }
        for field in [&patch.message, &patch.data, &patch.state]
            .into_iter()
            .flatten()
        {
            validate_operations(&field.set, &field.remove, field.order.as_deref())?;
        }
        if let Some(objects) = &patch.objects {
            validate_operations(&objects.upsert, &objects.remove, objects.order.as_deref())?;
            for (_, value) in &objects.upsert.entries {
                if !value.get().starts_with('{') {
                    return Err(CodecError::InvalidDelta("upsert body must be an object"));
                }
            }
        }
        patch.check_conflicts()?;
        Ok(patch)
    }

    fn check_conflicts(&self) -> Result<(), CodecError> {
        fn conflict(
            parent: Option<&FieldPatch<'_>>,
            key: &str,
            child: bool,
        ) -> Result<(), CodecError> {
            if let Some(parent) = parent {
                if child
                    && (parent.set.get(key).is_some() || parent.remove.iter().any(|k| k == key))
                {
                    return Err(CodecError::InvalidDelta("parent/child operation conflict"));
                }
            }
            Ok(())
        }
        conflict(
            self.message.as_ref(),
            "data",
            self.data.is_some() || self.state.is_some() || self.objects.is_some(),
        )?;
        conflict(
            self.data.as_ref(),
            "state",
            self.state.is_some() || self.objects.is_some(),
        )?;
        conflict(self.state.as_ref(), "objects", self.objects.is_some())
    }
}

fn validate_operations(
    set: &RawMap<'_>,
    remove: &[Cow<'_, str>],
    order: Option<&[Cow<'_, str>]>,
) -> Result<(), CodecError> {
    fn unique(keys: &[Cow<'_, str>]) -> bool {
        let mut sorted: Vec<_> = keys.iter().map(AsRef::as_ref).collect();
        sorted.sort_unstable();
        !sorted.windows(2).any(|pair| pair[0] == pair[1])
    }
    if !unique(remove) {
        return Err(CodecError::InvalidDelta("duplicate removal key"));
    }
    if remove.iter().any(|key| set.get(key).is_some()) {
        return Err(CodecError::InvalidDelta("set/remove overlap"));
    }
    if order.is_some_and(|keys| !unique(keys)) {
        return Err(CodecError::InvalidDelta("duplicate order key"));
    }
    Ok(())
}

// A single ordered assembly implementation serves all field levels and objects.
enum Node<'a> {
    Raw(&'a RawValue),
    Map(Vec<(Cow<'a, str>, Node<'a>)>),
}

impl Serialize for Node<'_> {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        match self {
            Self::Raw(raw) => raw.serialize(serializer),
            Self::Map(entries) => {
                let mut map = serializer.serialize_map(Some(entries.len()))?;
                for (key, value) in entries {
                    map.serialize_entry(key, value)?;
                }
                map.end()
            }
        }
    }
}

fn apply_map<'a>(
    base: RawMap<'a>,
    set: Option<&RawMap<'a>>,
    remove: &[Cow<'a, str>],
    order: Option<&[Cow<'a, str>]>,
) -> Result<Node<'a>, CodecError> {
    if let Some(set) = set {
        validate_operations(set, remove, order)?;
    }
    for key in remove {
        if base.get(key).is_none() {
            return Err(CodecError::InvalidDelta("removal target is missing"));
        }
    }
    let mut removed: Vec<_> = remove.iter().map(|key| key.as_ref()).collect();
    removed.sort_unstable();
    let mut entries = Vec::new();
    for (key, value) in &base.entries {
        if removed.binary_search(&key.as_ref()).is_err() {
            entries.push((
                key.clone(),
                Node::Raw(set.and_then(|set| set.get(key)).unwrap_or(value)),
            ));
        }
    }
    if let Some(set) = set {
        for (key, value) in &set.entries {
            if base.get(key).is_none() {
                entries.push((key.clone(), Node::Raw(value)));
            }
        }
    }
    if let Some(order) = order {
        if order.len() != entries.len() {
            return Err(CodecError::InvalidDelta(
                "order is not a complete key permutation",
            ));
        }
        let mut lookup: Vec<_> = (0..entries.len()).collect();
        lookup.sort_unstable_by(|&a, &b| entries[a].0.cmp(&entries[b].0));
        let positions = order
            .iter()
            .map(|key| {
                lookup
                    .binary_search_by(|&i| entries[i].0.as_ref().cmp(key.as_ref()))
                    .map(|i| lookup[i])
                    .map_err(|_| CodecError::InvalidDelta("unknown order key"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        let mut slots: Vec<_> = entries.into_iter().map(Some).collect();
        let mut reordered = Vec::with_capacity(slots.len());
        for position in positions {
            let slot = slots
                .get_mut(position)
                .ok_or(CodecError::InvalidDelta("invalid order position"))?;
            reordered.push(
                slot.take()
                    .ok_or(CodecError::InvalidDelta("repeated order key"))?,
            );
        }
        entries = reordered;
    }
    Ok(Node::Map(entries))
}

fn apply_fields<'a>(
    base: RawMap<'a>,
    patch: Option<&FieldPatch<'a>>,
) -> Result<Node<'a>, CodecError> {
    match patch {
        Some(patch) => apply_map(
            base,
            Some(&patch.set),
            &patch.remove,
            patch.order.as_deref(),
        ),
        None => apply_map(base, None, &[], None),
    }
}

fn replace_child<'a>(parent: &mut Node<'a>, key: &str, child: Node<'a>) -> Result<(), CodecError> {
    let Node::Map(entries) = parent else {
        return Err(CodecError::InvalidDelta("parent must be a map"));
    };
    let (_, value) = entries
        .iter_mut()
        .find(|(k, _)| k == key)
        .ok_or(CodecError::InvalidDelta("child target is missing"))?;
    *value = child;
    Ok(())
}

fn assemble<'a>(
    base: &'a WireState,
    shape: &'a Shape,
    patch: &Patch<'a>,
) -> Result<Node<'a>, CodecError> {
    if shape.layout != patch.layout {
        return Err(CodecError::InvalidDelta("layout mismatch"));
    }
    patch.check_conflicts()?;
    let backing = base.as_json();
    let mut state = apply_fields(shape.state().borrow(backing)?, patch.state.as_ref())?;
    if let Some(objects) = &patch.objects {
        let index = shape.objects.as_ref().ok_or(CodecError::InvalidDelta(
            "objects target is missing or not a map",
        ))?;
        let child = apply_map(
            index.borrow(backing)?,
            Some(&objects.upsert),
            &objects.remove,
            objects.order.as_deref(),
        )?;
        replace_child(&mut state, "objects", child)?;
    }
    match patch.layout {
        PatchLayout::State => {
            if patch.message.is_some() || patch.data.is_some() {
                return Err(CodecError::InvalidDelta(
                    "bare state has message/data operations",
                ));
            }
            Ok(state)
        }
        PatchLayout::Message => {
            let data_index = shape
                .data
                .as_ref()
                .ok_or(CodecError::InvalidDelta("missing data target"))?;
            let mut data = apply_fields(data_index.borrow(backing)?, patch.data.as_ref())?;
            // An absent child patch allows an entire child replacement/removal.
            if patch.state.is_some() || patch.objects.is_some() {
                replace_child(&mut data, "state", state)?;
            }
            let mut message = apply_fields(shape.root.borrow(backing)?, patch.message.as_ref())?;
            if patch.data.is_some() || patch.state.is_some() || patch.objects.is_some() {
                replace_child(&mut message, "data", data)?;
            }
            // A message patch may not change the layout/type contract.
            if let Some(message_patch) = &patch.message {
                if message_patch.remove.iter().any(|key| key == "type")
                    || message_patch.set.get("type").is_some_and(|raw| {
                        serde_json::from_str::<Cow<'_, str>>(raw.get())
                            .ok()
                            .as_deref()
                            != Some("StateUpdate")
                    })
                {
                    return Err(CodecError::InvalidDelta("message type change"));
                }
            }
            Ok(message)
        }
    }
}

fn diff_fields<'a>(
    before: &RawMap<'a>,
    after: &RawMap<'a>,
    child: Option<&str>,
) -> Result<FieldPatch<'a>, CodecError> {
    diff_fields_by(before, after, child, |_, a, b| a.get() == b.get())
}

fn diff_fields_by<'a>(
    before: &RawMap<'a>,
    after: &RawMap<'a>,
    child: Option<&str>,
    equivalent: impl Fn(&str, &RawValue, &RawValue) -> bool,
) -> Result<FieldPatch<'a>, CodecError> {
    let set = RawMap::new(
        after
            .entries
            .iter()
            .filter(|(key, value)| {
                child != Some(key.as_ref())
                    && !before
                        .get(key)
                        .is_some_and(|old| equivalent(key, old, value))
            })
            .map(|(key, value)| (key.clone(), *value))
            .collect(),
    )?;
    let remove: Vec<_> = before
        .entries
        .iter()
        .filter(|(key, _)| child != Some(key.as_ref()) && after.get(key).is_none())
        .map(|(key, _)| key.clone())
        .collect();
    let default: Vec<_> = before
        .entries
        .iter()
        .filter(|(key, _)| after.get(key).is_some())
        .map(|(key, _)| key.as_ref())
        .chain(
            after
                .entries
                .iter()
                .filter(|(key, _)| before.get(key).is_none())
                .map(|(key, _)| key.as_ref()),
        )
        .collect();
    let desired: Vec<_> = after.entries.iter().map(|(key, _)| key.as_ref()).collect();
    let order =
        (default != desired).then(|| after.entries.iter().map(|(key, _)| key.clone()).collect());
    Ok(FieldPatch { set, remove, order })
}

fn diff<'a>(
    base: &'a WireState,
    before: &'a Shape,
    target: &'a WireState,
    after: &'a Shape,
) -> Result<Patch<'a>, CodecError> {
    let common_objects = before.objects.as_ref().zip(after.objects.as_ref());
    let state = diff_fields(
        &before.state().borrow(base.as_json())?,
        &after.state().borrow(target.as_json())?,
        common_objects.map(|_| "objects"),
    )?;
    let objects = common_objects
        .map(|(before, after)| {
            let fields = diff_fields_by(
                &before.borrow(base.as_json())?,
                &after.borrow(target.as_json())?,
                None,
                |key, _, _| {
                    before
                        .entry(key)
                        .zip(after.entry(key))
                        .is_some_and(|(a, b)| a.digest == b.digest)
                },
            )?;
            Ok::<_, CodecError>(ObjectPatch {
                upsert: fields.set,
                remove: fields.remove,
                order: fields.order,
            })
        })
        .transpose()?;
    let (message, data) = match before.layout {
        PatchLayout::State => (None, None),
        PatchLayout::Message => {
            let message = diff_fields(
                &before.root.borrow(base.as_json())?,
                &after.root.borrow(target.as_json())?,
                Some("data"),
            )?;
            let before_data = before
                .data
                .as_ref()
                .ok_or(CodecError::InvalidDelta("missing data index"))?;
            let after_data = after
                .data
                .as_ref()
                .ok_or(CodecError::InvalidDelta("missing data index"))?;
            let data = diff_fields(
                &before_data.borrow(base.as_json())?,
                &after_data.borrow(target.as_json())?,
                Some("state"),
            )?;
            (Some(message), Some(data))
        }
    };
    let nonempty = |field: &FieldPatch<'_>| {
        !field.set.is_empty() || !field.remove.is_empty() || field.order.is_some()
    };
    Ok(Patch {
        layout: before.layout,
        message: message.filter(&nonempty),
        data: data.filter(&nonempty),
        state: Some(state).filter(nonempty),
        objects: objects.filter(|objects| {
            !objects.upsert.is_empty() || !objects.remove.is_empty() || objects.order.is_some()
        }),
    })
}

#[cfg(test)]
mod tests {
    use super::prefer_delta;

    #[test]
    fn equal_or_larger_candidate_selects_full() {
        assert!(prefer_delta(99, 100));
        assert!(!prefer_delta(100, 100));
        assert!(!prefer_delta(101, 100));
    }
}

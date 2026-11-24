//! Test-only cache of the lazily loaded card fixture, kept beside the test binary so later test
//! processes decode its indices instead of re-parsing the export. A file is served only under its
//! key (fixture hash, binary size and mtime) and checksum; any failure falls back to the load.

use std::collections::HashMap;
use std::error::Error;
use std::fs::{self, File, OpenOptions};
use std::hash::Hasher;
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::UNIX_EPOCH;

use serde::de::DeserializeOwned;
use serde::Serialize;

use super::{
    BracketLists, BracketSignals, CardDatabase, CardLegalities, FaceIndex, FaceSlot,
    LegalityFormat, LegalityStatus, RawFace, Ruling,
};
use crate::types::card::LayoutKind;
use crate::types::card_type::{CardType, CoreType, Supertype};

const MAGIC: &[u8; 8] = b"PHFXC\0\0\x01";
/// Magic, fixture hash, binary length, binary mtime (ns), index length, records length, checksum.
const HEADER_LEN: usize = 8 + 8 + 8 + 16 + 8 + 8 + 8;

impl CardDatabase {
    /// Test loader: [`Self::from_export_reader_typing_faces_on_read`] on the decompressed
    /// export, through a cache file beside the running test binary.
    pub fn from_compressed_export_cached<R: Read>(
        mut compressed: R,
        decompress: impl FnOnce(&[u8]) -> io::Result<String>,
    ) -> Result<Self, Box<dyn Error>> {
        let mut bytes = Vec::new();
        compressed.read_to_end(&mut bytes)?;
        let location = std::env::current_exe()
            .ok()
            .and_then(|binary| Location::beside(&binary, &bytes));
        load(&bytes, decompress, location.as_ref())
    }

    /// Test loader for a fixture made of named parts (the integration card shards), through the
    /// same cache. The cache key hashes each part's name, length and bytes; `text` assembles the
    /// export JSON only when no cache file is served.
    pub fn from_parts_cached<'a>(
        parts: impl IntoIterator<Item = (&'a str, &'a [u8])>,
        text: impl FnOnce() -> io::Result<String>,
    ) -> Result<Self, Box<dyn Error>> {
        let hash = parts_hash(parts);
        let location = std::env::current_exe()
            .ok()
            .and_then(|binary| Location::beside_hash(&binary, hash));
        load(&[], |_| text(), location.as_ref())
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) struct Key {
    pub(super) fixture_hash: u64,
    pub(super) binary_len: u64,
    pub(super) binary_mtime_ns: u128,
}

/// Hash of named parts. Each name and body is length-framed, so moving a byte between two parts
/// changes the hash.
fn parts_hash<'a>(parts: impl IntoIterator<Item = (&'a str, &'a [u8])>) -> u64 {
    let mut hasher = std::hash::DefaultHasher::new();
    for (name, bytes) in parts {
        hasher.write_u64(name.len() as u64);
        hasher.write(name.as_bytes());
        hasher.write_u64(bytes.len() as u64);
        hasher.write(bytes);
    }
    hasher.finish()
}

pub(super) fn fixture_hash(bytes: &[u8]) -> u64 {
    // DefaultHasher's fixed algorithm can change only with the toolchain, which rebuilds the binary.
    let mut hasher = std::hash::DefaultHasher::new();
    hasher.write(bytes);
    hasher.finish()
}

#[derive(Debug, PartialEq)]
struct Location {
    path: PathBuf,
    key: Key,
}

impl Location {
    /// The cache file for `fixture` beside `binary`, keyed by the binary's size and mtime.
    fn beside(binary: &Path, fixture: &[u8]) -> Option<Self> {
        Self::beside_hash(binary, fixture_hash(fixture))
    }

    /// [`Self::beside`] for a fixture whose hash the caller already computed.
    fn beside_hash(binary: &Path, fixture_hash: u64) -> Option<Self> {
        let metadata = fs::metadata(binary).ok()?;
        let binary_mtime_ns = metadata
            .modified()
            .ok()?
            .duration_since(UNIX_EPOCH)
            .ok()?
            .as_nanos();
        let mut path = binary.as_os_str().to_owned();
        path.push(".fixture-cache");
        Some(Self {
            path: path.into(),
            key: Key {
                fixture_hash,
                binary_len: metadata.len(),
                binary_mtime_ns,
            },
        })
    }
}

fn load(
    compressed: &[u8],
    decompress: impl FnOnce(&[u8]) -> io::Result<String>,
    location: Option<&Location>,
) -> Result<CardDatabase, Box<dyn Error>> {
    if let Some(db) = location.and_then(|location| read(&location.path, location.key)) {
        return Ok(db);
    }
    let db = CardDatabase::from_export_text_typing_faces_on_read(decompress(compressed)?)?;
    if let Some(location) = location {
        // A failed write only means the next process builds the cache again.
        let _ = write(&location.path, location.key, &db);
    }
    Ok(db)
}

pub(super) fn write(path: &Path, key: Key, db: &CardDatabase) -> Option<()> {
    // Staging first lets an unwritable location fail before the encode is paid for.
    let mut staged = Staged::create(path)?;
    staged.write(&encode(key, db)?)?;
    staged.publish()
}

/// A cache file written under a name unique to this process and call, renamed into place
/// only once complete and synced, so a reader sees a whole file or none.
struct Staged {
    temp: PathBuf,
    path: PathBuf,
    file: Option<File>,
}

impl Staged {
    fn create(path: &Path) -> Option<Self> {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let mut temp = path.as_os_str().to_owned();
        temp.push(format!(
            ".tmp.{}.{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let temp = PathBuf::from(temp);
        let file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
            .ok()?;
        Some(Self {
            temp,
            path: path.to_owned(),
            file: Some(file),
        })
    }

    fn write(&mut self, bytes: &[u8]) -> Option<()> {
        let file = self.file.as_mut()?;
        file.write_all(bytes).ok()?;
        file.sync_all().ok()
    }

    fn publish(mut self) -> Option<()> {
        drop(self.file.take());
        fs::rename(&self.temp, &self.path).ok()?;
        remove_temporaries(&self.path);
        Some(())
    }
}

impl Drop for Staged {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.temp);
    }
}

/// Removes every staged file of `path`: a killed builder leaves its file behind, and a live
/// builder whose file is removed only loses its rename.
fn remove_temporaries(path: &Path) {
    let (Some(dir), Some(name)) = (path.parent(), path.file_name()) else {
        return;
    };
    let mut prefix = name.to_owned();
    prefix.push(".tmp.");
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if entry
            .file_name()
            .as_encoded_bytes()
            .starts_with(prefix.as_encoded_bytes())
        {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// CRC-32 detects every error confined to 32 consecutive bits, so any single flipped byte.
fn checksum(index: &[u8], records: &[u8]) -> u64 {
    let mut hasher = crc32fast::Hasher::new();
    hasher.update(index);
    hasher.update(records);
    u64::from(hasher.finalize())
}

/// The file bytes: header, index section, records section.
fn assemble(key: Key, index: &[u8], records: &[u8]) -> Vec<u8> {
    let mut file = Vec::with_capacity(HEADER_LEN + index.len() + records.len());
    file.extend_from_slice(MAGIC);
    file.extend_from_slice(&key.fixture_hash.to_le_bytes());
    file.extend_from_slice(&key.binary_len.to_le_bytes());
    file.extend_from_slice(&key.binary_mtime_ns.to_le_bytes());
    file.extend_from_slice(&(index.len() as u64).to_le_bytes());
    file.extend_from_slice(&(records.len() as u64).to_le_bytes());
    file.extend_from_slice(&checksum(index, records).to_le_bytes());
    file.extend_from_slice(index);
    file.extend_from_slice(records);
    file
}

struct Writer(Vec<u8>);

impl Writer {
    fn len(&mut self, value: usize) -> Option<()> {
        self.0
            .extend_from_slice(&u32::try_from(value).ok()?.to_le_bytes());
        Some(())
    }

    fn str(&mut self, value: &str) -> Option<()> {
        self.len(value.len())?;
        self.0.extend_from_slice(value.as_bytes());
        Some(())
    }

    fn opt_str(&mut self, value: Option<&str>) -> Option<()> {
        match value {
            None => self.0.push(0),
            Some(value) => {
                self.0.push(1);
                self.str(value)?;
            }
        }
        Some(())
    }

    fn strs(&mut self, values: &[String]) -> Option<()> {
        self.len(values.len())?;
        values.iter().try_for_each(|value| self.str(value))
    }
}

struct Reader<'a>(&'a [u8]);

impl<'a> Reader<'a> {
    fn take(&mut self, n: usize) -> Option<&'a [u8]> {
        let (head, tail) = self.0.split_at_checked(n)?;
        self.0 = tail;
        Some(head)
    }

    fn u8(&mut self) -> Option<u8> {
        Some(self.take(1)?[0])
    }

    fn u64(&mut self) -> Option<u64> {
        Some(u64::from_le_bytes(self.take(8)?.try_into().ok()?))
    }

    fn u128(&mut self) -> Option<u128> {
        Some(u128::from_le_bytes(self.take(16)?.try_into().ok()?))
    }

    fn len(&mut self) -> Option<usize> {
        usize::try_from(u32::from_le_bytes(self.take(4)?.try_into().ok()?)).ok()
    }

    /// A collection length, refused when it exceeds the bytes left: every item takes a byte.
    fn count(&mut self) -> Option<usize> {
        let n = self.len()?;
        (n <= self.0.len()).then_some(n)
    }

    fn str(&mut self) -> Option<&'a str> {
        let n = self.len()?;
        std::str::from_utf8(self.take(n)?).ok()
    }

    fn string(&mut self) -> Option<String> {
        self.str().map(str::to_owned)
    }

    fn opt_string(&mut self) -> Option<Option<String>> {
        match self.u8()? {
            0 => Some(None),
            1 => Some(Some(self.string()?)),
            _ => None,
        }
    }

    fn strings(&mut self) -> Option<Vec<String>> {
        (0..self.count()?).map(|_| self.string()).collect()
    }

    fn atom<'t, T>(&mut self, atoms: &'t [T]) -> Option<&'t T> {
        atoms.get(self.len()?)
    }
}

/// Distinct values of an engine-owned type, each stored once as its serde JSON text, so the
/// file needs no hand-kept table from enum variants to numbers.
#[derive(Default)]
struct Atoms {
    ids: HashMap<String, usize>,
    texts: Vec<String>,
}

impl Atoms {
    fn id<T: Serialize>(&mut self, value: &T) -> Option<usize> {
        let text = serde_json::to_string(value).ok()?;
        let next = self.texts.len();
        let id = *self.ids.entry(text.clone()).or_insert(next);
        if id == next {
            self.texts.push(text);
        }
        Some(id)
    }
}

fn read_atoms<T: DeserializeOwned>(input: &mut Reader) -> Option<Vec<T>> {
    (0..input.count()?)
        .map(|_| serde_json::from_str(input.str()?).ok())
        .collect()
}

#[derive(Default)]
struct AtomTables {
    supertypes: Atoms,
    core_types: Atoms,
    layouts: Atoms,
    formats: Atoms,
    statuses: Atoms,
    bracket_signals: Atoms,
}

fn encode(key: Key, db: &CardDatabase) -> Option<Vec<u8>> {
    let CardDatabase {
        cards,
        face_index,
        name_alias_index,
        oracle_id_index,
        face_order_index,
        search_face_keys,
        layout_index,
        legalities,
        printings_index,
        rulings_index,
        errors,
        bracket_lists,
        bracket_signals_by_name,
        creature_type_vocabulary,
        valid_subtypes: _,
    } = db;
    // An export load leaves these three at these values; decode restores them.
    if !cards.is_empty() || !errors.is_empty() || *bracket_lists != BracketLists::bundled_revision()
    {
        return None;
    }
    let mut atoms = AtomTables::default();
    let mut out = Writer(Vec::new());
    let mut records = Vec::new();

    out.len(face_index.0.len())?;
    for (key, slot) in &face_index.0 {
        let FaceSlot::Raw(raw) = slot else {
            return None;
        };
        let RawFace {
            text,
            record,
            name,
            card_type,
            scryfall_oracle_id,
            face: _,
        } = &**raw;
        let CardType {
            supertypes,
            core_types,
            subtypes,
        } = card_type;
        out.str(key)?;
        out.str(name)?;
        out.opt_str(scryfall_oracle_id.as_deref())?;
        out.len(supertypes.len())?;
        for supertype in supertypes {
            out.len(atoms.supertypes.id(supertype)?)?;
        }
        out.len(core_types.len())?;
        for core_type in core_types {
            out.len(atoms.core_types.id(core_type)?)?;
        }
        out.strs(subtypes)?;
        let record = text.get(record.clone())?;
        out.0
            .extend_from_slice(&u64::try_from(records.len()).ok()?.to_le_bytes());
        out.len(record.len())?;
        records.extend_from_slice(record.as_bytes());
    }
    out.len(name_alias_index.len())?;
    for (alias, key) in name_alias_index {
        out.str(alias)?;
        out.str(key)?;
    }
    out.len(oracle_id_index.len())?;
    for (oracle_id, keys) in oracle_id_index {
        out.str(oracle_id)?;
        out.strs(keys)?;
    }
    out.len(face_order_index.len())?;
    for (key, order) in face_order_index {
        out.str(key)?;
        out.len(*order)?;
    }
    out.strs(search_face_keys)?;
    out.len(layout_index.len())?;
    for (oracle_id, layout) in layout_index {
        out.str(oracle_id)?;
        out.len(atoms.layouts.id(layout)?)?;
    }
    // Few distinct legality maps exist, so each is stored once and named by index per card.
    let mut map_ids: HashMap<Vec<(usize, usize)>, usize> = HashMap::new();
    let mut maps: Vec<Vec<(usize, usize)>> = Vec::new();
    let mut card_maps = Vec::with_capacity(legalities.len());
    for (key, card) in legalities {
        let mut pairs = card
            .iter()
            .map(|(format, status)| Some((atoms.formats.id(format)?, atoms.statuses.id(status)?)))
            .collect::<Option<Vec<_>>>()?;
        pairs.sort_unstable();
        let next = maps.len();
        let id = *map_ids.entry(pairs.clone()).or_insert(next);
        if id == next {
            maps.push(pairs);
        }
        card_maps.push((key, id));
    }
    out.len(maps.len())?;
    for pairs in &maps {
        out.len(pairs.len())?;
        for (format, status) in pairs {
            out.len(*format)?;
            out.len(*status)?;
        }
    }
    out.len(card_maps.len())?;
    for (key, id) in card_maps {
        out.str(key)?;
        out.len(id)?;
    }
    out.len(printings_index.len())?;
    for (key, sets) in printings_index {
        out.str(key)?;
        out.strs(sets)?;
    }
    out.len(rulings_index.len())?;
    for (key, rulings) in rulings_index {
        out.str(key)?;
        out.len(rulings.len())?;
        for Ruling { date, text } in rulings {
            out.str(date)?;
            out.str(text)?;
        }
    }
    out.len(bracket_signals_by_name.len())?;
    for (key, signals) in bracket_signals_by_name {
        out.str(key)?;
        out.len(atoms.bracket_signals.id(signals)?)?;
    }
    out.strs(creature_type_vocabulary)?;

    let AtomTables {
        supertypes,
        core_types,
        layouts,
        formats,
        statuses,
        bracket_signals,
    } = &atoms;
    let mut index = Writer(Vec::new());
    for table in [
        supertypes,
        core_types,
        layouts,
        formats,
        statuses,
        bracket_signals,
    ] {
        index.strs(&table.texts)?;
    }
    index.0.extend_from_slice(&out.0);

    Some(assemble(key, &index.0, &records))
}

/// The database stored at `path` under `key`, or `None` for a missing, foreign or damaged file.
pub(super) fn read(path: &Path, key: Key) -> Option<CardDatabase> {
    let mut file = File::open(path).ok()?;
    let mut header = [0u8; HEADER_LEN];
    file.read_exact(&mut header).ok()?;
    let mut fields = Reader(&header);
    if fields.take(MAGIC.len())? != MAGIC {
        return None;
    }
    let stored = Key {
        fixture_hash: fields.u64()?,
        binary_len: fields.u64()?,
        binary_mtime_ns: fields.u128()?,
    };
    let index_len = usize::try_from(fields.u64()?).ok()?;
    let records_len = usize::try_from(fields.u64()?).ok()?;
    let stored_checksum = fields.u64()?;
    let expected_len = HEADER_LEN
        .checked_add(index_len)?
        .checked_add(records_len)?;
    if stored != key || u64::try_from(expected_len).ok()? != file.metadata().ok()?.len() {
        return None;
    }
    let mut index = vec![0; index_len];
    file.read_exact(&mut index).ok()?;
    let mut records = vec![0; records_len];
    file.read_exact(&mut records).ok()?;
    if checksum(&index, &records) != stored_checksum {
        return None;
    }
    decode(&index, Arc::new(String::from_utf8(records).ok()?))
}

// Nothing here is pre-sized from a count in the file: maps grow by insert and vectors collect
// through `Option`, whose lower size bound is zero.
fn decode(index: &[u8], text: Arc<String>) -> Option<CardDatabase> {
    let mut input = Reader(index);
    let supertype_atoms: Vec<Supertype> = read_atoms(&mut input)?;
    let core_type_atoms: Vec<CoreType> = read_atoms(&mut input)?;
    let layout_atoms: Vec<LayoutKind> = read_atoms(&mut input)?;
    let format_atoms: Vec<LegalityFormat> = read_atoms(&mut input)?;
    let status_atoms: Vec<LegalityStatus> = read_atoms(&mut input)?;
    let bracket_signal_atoms: Vec<BracketSignals> = read_atoms(&mut input)?;

    let faces = input.count()?;
    let mut face_index = HashMap::new();
    for _ in 0..faces {
        let key = input.string()?;
        let name = input.string()?;
        let scryfall_oracle_id = input.opt_string()?;
        let supertypes = (0..input.count()?)
            .map(|_| input.atom(&supertype_atoms).copied())
            .collect::<Option<Vec<_>>>()?;
        let core_types = (0..input.count()?)
            .map(|_| input.atom(&core_type_atoms).copied())
            .collect::<Option<Vec<_>>>()?;
        let subtypes = input.strings()?;
        let start = usize::try_from(input.u64()?).ok()?;
        let record = start..start.checked_add(input.len()?)?;
        text.get(record.clone())?;
        face_index.insert(
            key,
            FaceSlot::Raw(Box::new(RawFace {
                text: Arc::clone(&text),
                record,
                name,
                card_type: CardType {
                    supertypes,
                    core_types,
                    subtypes,
                },
                scryfall_oracle_id,
                face: OnceLock::new(),
            })),
        );
    }
    let n = input.count()?;
    let mut name_alias_index = HashMap::new();
    for _ in 0..n {
        name_alias_index.insert(input.string()?, input.string()?);
    }
    let n = input.count()?;
    let mut oracle_id_index = HashMap::new();
    for _ in 0..n {
        oracle_id_index.insert(input.string()?, input.strings()?);
    }
    let n = input.count()?;
    let mut face_order_index = HashMap::new();
    for _ in 0..n {
        face_order_index.insert(input.string()?, input.len()?);
    }
    let search_face_keys = input.strings()?;
    let n = input.count()?;
    let mut layout_index = HashMap::new();
    for _ in 0..n {
        layout_index.insert(input.string()?, *input.atom(&layout_atoms)?);
    }
    let maps = (0..input.count()?)
        .map(|_| {
            (0..input.count()?)
                .map(|_| Some((*input.atom(&format_atoms)?, *input.atom(&status_atoms)?)))
                .collect::<Option<CardLegalities>>()
        })
        .collect::<Option<Vec<_>>>()?;
    let n = input.count()?;
    let mut legalities = HashMap::new();
    for _ in 0..n {
        legalities.insert(input.string()?, input.atom(&maps)?.clone());
    }
    let n = input.count()?;
    let mut printings_index = HashMap::new();
    for _ in 0..n {
        printings_index.insert(input.string()?, input.strings()?);
    }
    let n = input.count()?;
    let mut rulings_index = HashMap::new();
    for _ in 0..n {
        let key = input.string()?;
        let rulings = (0..input.count()?)
            .map(|_| {
                Some(Ruling {
                    date: input.string()?,
                    text: input.string()?,
                })
            })
            .collect::<Option<Vec<_>>>()?;
        rulings_index.insert(key, rulings);
    }
    let n = input.count()?;
    let mut bracket_signals_by_name = HashMap::new();
    for _ in 0..n {
        bracket_signals_by_name.insert(input.string()?, *input.atom(&bracket_signal_atoms)?);
    }
    let creature_type_vocabulary = input.strings()?;
    if !input.0.is_empty() {
        return None;
    }
    Some(CardDatabase {
        cards: HashMap::new(),
        face_index: FaceIndex(face_index),
        name_alias_index,
        oracle_id_index,
        face_order_index,
        search_face_keys,
        layout_index,
        legalities,
        printings_index,
        rulings_index,
        errors: Vec::new(),
        bracket_lists: BracketLists::bundled_revision(),
        bracket_signals_by_name,
        creature_type_vocabulary,
        valid_subtypes: OnceLock::new(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::card_db::tests::test_face;

    fn export(names: &[&str]) -> Vec<u8> {
        let records: serde_json::Map<String, serde_json::Value> = names
            .iter()
            .map(|name| {
                (
                    name.to_string(),
                    serde_json::to_value(test_face(name)).unwrap(),
                )
            })
            .collect();
        serde_json::to_vec(&records).unwrap()
    }

    fn as_text(bytes: &[u8]) -> io::Result<String> {
        String::from_utf8(bytes.to_vec()).map_err(io::Error::other)
    }

    fn location(dir: &Path, fixture: &[u8], binary_len: u64, binary_mtime_ns: u128) -> Location {
        Location {
            path: dir.join("fixture-cache"),
            key: Key {
                fixture_hash: fixture_hash(fixture),
                binary_len,
                binary_mtime_ns,
            },
        }
    }

    fn loaded(fixture: &[u8], location: &Location) -> Vec<String> {
        load(fixture, as_text, Some(location)).unwrap().card_names()
    }

    fn lazy(fixture: &[u8]) -> CardDatabase {
        CardDatabase::from_export_text_typing_faces_on_read(as_text(fixture).unwrap()).unwrap()
    }

    fn staged_files(dir: &Path) -> Vec<String> {
        fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .filter(|name| name.contains(".tmp."))
            .collect()
    }

    #[test]
    fn a_missing_cache_is_built_and_then_served() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = export(&["Lightning Bolt"]);
        let at = location(dir.path(), &fixture, 7, 11);
        assert_eq!(loaded(&fixture, &at), ["Lightning Bolt"]);
        let served = read(&at.path, at.key).expect("the miss published a cache");
        assert_eq!(served.card_names(), ["Lightning Bolt"]);
        assert_eq!(served.face_index.typed_face_count(), 0);
        assert!(staged_files(dir.path()).is_empty());
    }

    #[test]
    fn a_changed_fixture_or_binary_is_not_served_another_builds_data() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = export(&["Lightning Bolt"]);
        let same_cards_other_bytes = [fixture.as_slice(), b" "].concat();
        let plant = || {
            let at = location(dir.path(), &fixture, 7, 11);
            write(&at.path, at.key, &lazy(&export(&["Words of Worship"]))).unwrap();
        };

        plant();
        assert_eq!(
            loaded(&fixture, &location(dir.path(), &fixture, 7, 11)),
            ["Words of Worship"]
        );
        plant();
        assert_eq!(
            loaded(
                &same_cards_other_bytes,
                &location(dir.path(), &same_cards_other_bytes, 7, 11)
            ),
            ["Lightning Bolt"]
        );
        plant();
        assert_eq!(
            loaded(&fixture, &location(dir.path(), &fixture, 8, 11)),
            ["Lightning Bolt"]
        );
        plant();
        assert_eq!(
            loaded(&fixture, &location(dir.path(), &fixture, 7, 12)),
            ["Lightning Bolt"]
        );
    }

    #[test]
    fn a_damaged_cache_file_falls_back_to_a_correct_load() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = export(&["Lightning Bolt"]);
        let at = location(dir.path(), &fixture, 7, 11);
        write(&at.path, at.key, &lazy(&export(&["Words of Worship"]))).unwrap();
        let intact = fs::read(&at.path).unwrap();
        let mut flipped_magic = intact.clone();
        flipped_magic[0] ^= 1;
        let mut flipped_record_byte = intact.clone();
        *flipped_record_byte.last_mut().unwrap() = b']';
        let index_len = u64::from_le_bytes(intact[40..48].try_into().unwrap()) as usize;
        let name_at = intact[HEADER_LEN..HEADER_LEN + index_len]
            .windows(16)
            .position(|window| window == b"Words of Worship")
            .unwrap();
        let mut flipped_index_byte = intact.clone();
        flipped_index_byte[HEADER_LEN + name_at] = b'X';
        // Six empty atom tables, then a face count as large as the bytes after it.
        let filler = 8_000_000usize;
        let mut index = [0u8; 24].to_vec();
        index.extend_from_slice(&(filler as u32).to_le_bytes());
        index.resize(index.len() + filler, 0xFF);
        let count_beyond_entries = assemble(at.key, &index, &[]);
        let damaged: [(&str, Vec<u8>); 7] = [
            ("truncated", intact[..intact.len() - 1].to_vec()),
            ("extended", [intact.as_slice(), b"\0"].concat()),
            ("garbage", b"not a fixture cache".to_vec()),
            ("flipped magic", flipped_magic),
            ("flipped record byte", flipped_record_byte),
            ("flipped index byte", flipped_index_byte),
            ("count beyond its entries", count_beyond_entries),
        ];
        for (case, bytes) in damaged {
            fs::write(&at.path, bytes).unwrap();
            assert_eq!(loaded(&fixture, &at), ["Lightning Bolt"], "{case}");
        }
        fs::write(&at.path, &intact).unwrap();
        assert_eq!(
            loaded(&fixture, &at),
            ["Words of Worship"],
            "intact control"
        );
    }

    #[test]
    fn an_unwritable_location_falls_back_to_a_correct_load() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = export(&["Lightning Bolt"]);
        let not_a_dir = dir.path().join("not-a-directory");
        fs::write(&not_a_dir, b"").unwrap();
        let at = location(&not_a_dir, &fixture, 7, 11);
        assert_eq!(loaded(&fixture, &at), ["Lightning Bolt"]);
        assert!(staged_files(dir.path()).is_empty());
    }

    #[test]
    fn a_cache_is_published_only_by_renaming_a_complete_uniquely_named_file() {
        let dir = tempfile::tempdir().unwrap();
        let fixture = export(&["Lightning Bolt"]);
        let at = location(dir.path(), &fixture, 7, 11);
        let killed_builder = dir.path().join("fixture-cache.tmp.0.0");
        fs::write(&killed_builder, b"partial").unwrap();

        let mut first = Staged::create(&at.path).unwrap();
        let second = Staged::create(&at.path).unwrap();
        assert_ne!(first.temp, second.temp);
        let marker = format!(".tmp.{}.", std::process::id());
        assert!(first.temp.to_string_lossy().contains(&marker));
        first
            .write(&encode(at.key, &lazy(&fixture)).unwrap())
            .unwrap();
        assert!(read(&at.path, at.key).is_none(), "served before the rename");

        first.publish().unwrap();
        assert_eq!(
            read(&at.path, at.key).unwrap().card_names(),
            ["Lightning Bolt"]
        );
        assert!(staged_files(dir.path()).is_empty(), "staged files left");
        assert!(second.publish().is_none(), "a swept builder still renamed");
    }

    #[test]
    fn the_key_comes_from_the_binary_size_and_modification_time() {
        let dir = tempfile::tempdir().unwrap();
        let binary = dir.path().join("integration_9-0123.test");
        let fixture = export(&["Lightning Bolt"]);
        fs::write(&binary, b"12345").unwrap();
        let mtime = UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        File::options()
            .write(true)
            .open(&binary)
            .unwrap()
            .set_modified(mtime)
            .unwrap();
        assert_eq!(
            Location::beside(&binary, &fixture).unwrap(),
            Location {
                path: dir.path().join("integration_9-0123.test.fixture-cache"),
                key: Key {
                    fixture_hash: fixture_hash(&fixture),
                    binary_len: 5,
                    binary_mtime_ns: 1_700_000_000_000_000_000,
                },
            }
        );
        assert!(Location::beside(&dir.path().join("absent"), &fixture).is_none());
    }

    #[test]
    fn the_parts_hash_follows_every_name_byte_and_boundary() {
        let base = parts_hash([("a.jsonl", &b"xy"[..]), ("b.jsonl", &b"z"[..])]);
        assert_eq!(
            base,
            parts_hash([("a.jsonl", &b"xy"[..]), ("b.jsonl", &b"z"[..])])
        );
        assert_ne!(
            base,
            parts_hash([("a.jsonl", &b"xy"[..]), ("b.jsonl", &b"Z"[..])])
        );
        assert_ne!(
            base,
            parts_hash([("a.jsonl", &b"x"[..]), ("b.jsonl", &b"yz"[..])])
        );
        assert_ne!(
            base,
            parts_hash([("c.jsonl", &b"xy"[..]), ("b.jsonl", &b"z"[..])])
        );
    }
}

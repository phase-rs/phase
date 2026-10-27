use std::collections::{HashMap, HashSet};
use std::ops::ControlFlow;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use serde::{Deserialize, Serialize};

use super::bracket_lists::{BracketLists, BracketSignals};
use super::legality::{normalize_legalities, CardLegalities, LegalityFormat, LegalityStatus};
use super::mtgjson::Ruling;
use crate::types::card::{CardFace, CardRules, LayoutKind, PrintedCardRef};
use crate::types::card_type::CoreType;

use std::io::{BufReader, Read};

/// Shared, cheaply-cloneable handle to the loaded [`CardDatabase`].
///
/// Exists so `GameState` can carry the database for resolvers that must query
/// the whole card corpus at resolution time (Momir's random creature draw)
/// rather than pre-staging a copy of it into state. `Arc` keeps
/// `GameState::clone()` during AI search O(1), matching the `Arc` on
/// `card_face_registry` / `all_card_names`.
///
/// `Debug` is hand-written as a one-line summary: `GameState` derives `Debug`,
/// and a derived impl here would dump every loaded card face on any `{:?}` of
/// a game state.
#[derive(Clone)]
pub struct CardDbHandle(Arc<CardDatabase>);

impl CardDbHandle {
    pub fn new(db: Arc<CardDatabase>) -> Self {
        Self(db)
    }

    pub fn arc(&self) -> &Arc<CardDatabase> {
        &self.0
    }
}

impl std::fmt::Debug for CardDbHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "CardDbHandle({} faces)", self.0.face_index.len())
    }
}

impl std::ops::Deref for CardDbHandle {
    type Target = CardDatabase;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Arc<CardDatabase>> for CardDbHandle {
    fn from(db: Arc<CardDatabase>) -> Self {
        Self(db)
    }
}

#[derive(Default)]
pub struct CardDatabase {
    pub(crate) cards: HashMap<String, CardRules>,
    pub(crate) face_index: HashMap<String, CardFace>,
    pub(crate) name_alias_index: HashMap<String, String>,
    pub(crate) oracle_id_index: HashMap<String, Vec<String>>,
    /// Maps face key (lowercased card name) to its original zero-based position
    /// inside MTGJSON's multi-face record. Export loading flattens faces into a
    /// JSON object, whose iteration order is not a rules authority.
    pub(crate) face_order_index: HashMap<String, usize>,
    /// Deterministic card-search scan order. Built once by database loaders so
    /// interactive search does not allocate and sort the full face index on
    /// every query.
    pub(crate) search_face_keys: Vec<String>,
    /// Maps oracle_id → runtime LayoutKind for multi-face cards.
    /// Populated only from the export path (the MTGJSON path uses `cards` directly).
    /// Enables `rehydrate_game_from_card_db` to determine the correct layout kind
    /// when `get_by_name` returns None (export path doesn't build `CardRules`).
    pub(crate) layout_index: HashMap<String, LayoutKind>,
    pub(crate) legalities: HashMap<String, CardLegalities>,
    /// Maps face key (lowercased card name) → set codes the card was printed in.
    /// Populated only via the export path (MTGJSON `printings` field).
    /// Used by the coverage dashboard to group cards by set.
    pub(crate) printings_index: HashMap<String, Vec<String>>,
    /// Maps face key (lowercased card name) → official WotC rulings.
    /// Populated only via the export path. Only front faces of multi-face
    /// cards carry rulings; back-face lookups return the empty slice.
    pub(crate) rulings_index: HashMap<String, Vec<Ruling>>,
    pub(crate) errors: Vec<(PathBuf, String)>,
    /// Non-MTGJSON bracket-axis name lists. Populated by `with_bracket_lists`
    /// at export time for policy axes MTGJSON does not expose. WASM/server
    /// consumers receive those signals in the already-built database.
    pub(crate) bracket_lists: BracketLists,
    /// Stamped during `from_export_entries` from each `CardExportEntry`'s
    /// `bracket_signals` field. Keyed by lowercased card name. Read by
    /// `bracket_signals_for` at runtime.
    pub(crate) bracket_signals_by_name: HashMap<String, BracketSignals>,
    /// CR 205.3m: creature subtype vocabulary — subtypes from every loaded
    /// creature/kindred/tribal face, minus any subtype that also appears on a
    /// non-creature face (land/artifact/enchantment/spell types that ride a
    /// multi-type face's flat subtype array). Sorted and deduplicated. Seeds
    /// `GameState::all_creature_types` at game start so consumers like
    /// `ChoiceType::CreatureType` (Morophon) and `SharesQuality::CreatureType`
    /// (Coat of Arms, Changeling expansion) see every printed creature type,
    /// not just the subset present in the loaded decks.
    pub(crate) creature_type_vocabulary: Vec<String>,
}

impl CardDatabase {
    /// Build from MTGJSON atomic cards, running the Oracle text parser.
    /// Used by tests and the oracle_gen binary for library-level access.
    pub fn from_mtgjson(mtgjson_path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        super::oracle_loader::load_from_mtgjson(mtgjson_path)
    }

    /// Load from a pre-processed card-data export.
    pub fn from_export(path: &Path) -> Result<Self, Box<dyn std::error::Error>> {
        let file = std::fs::File::open(path)?;
        let reader = BufReader::new(file);
        Self::from_export_reader(reader)
    }

    /// Load a pre-processed card-data export from an already-open reader.
    /// Keeps compressed test fixtures on the same deserialization path as the
    /// production file loader without changing production's buffered-file flow.
    pub fn from_export_reader<R: Read>(reader: R) -> Result<Self, Box<dyn std::error::Error>> {
        let mut raw: serde_json::Value = serde_json::from_reader(BufReader::new(reader))?;
        crate::types::ability::normalize_legacy_damage_source_program(&mut raw);
        let entries: HashMap<String, CardExportEntry> = serde_json::from_value(raw)?;
        Ok(Self::from_export_entries(entries))
    }

    /// Load from a card-data export JSON string.
    /// Used by the WASM bridge to receive card data from the frontend.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let mut raw: serde_json::Value = serde_json::from_str(json)?;
        crate::types::ability::normalize_legacy_damage_source_program(&mut raw);
        let entries: HashMap<String, CardExportEntry> = serde_json::from_value(raw)?;
        Ok(Self::from_export_entries(entries))
    }

    fn from_export_entries(entries: HashMap<String, CardExportEntry>) -> Self {
        let mut face_index = HashMap::with_capacity(entries.len());
        let mut oracle_id_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut face_order_index: HashMap<String, usize> = HashMap::new();
        let mut layout_index: HashMap<String, LayoutKind> = HashMap::new();
        let mut legalities = HashMap::new();
        let mut printings_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut rulings_index: HashMap<String, Vec<Ruling>> = HashMap::new();
        let mut bracket_signals_by_name: HashMap<String, BracketSignals> =
            HashMap::with_capacity(entries.len());

        for (export_key, entry) in entries {
            let storage_key = export_key.to_lowercase();
            if let Some(face_order) = entry.face_index {
                face_order_index.insert(storage_key.clone(), face_order);
            }
            if let Some(oracle_id) = entry.face.scryfall_oracle_id.clone() {
                oracle_id_index
                    .entry(oracle_id.clone())
                    .or_default()
                    .push(storage_key.clone());
                if let Some(layout_kind) = entry.layout.as_deref().and_then(map_layout_str) {
                    layout_index.entry(oracle_id).or_insert(layout_kind);
                }
            }
            face_index.insert(storage_key.clone(), entry.face);
            bracket_signals_by_name.insert(storage_key.clone(), entry.bracket_signals);

            if !entry.printings.is_empty() {
                printings_index.insert(storage_key.clone(), entry.printings);
            }

            if !entry.rulings.is_empty() {
                rulings_index.insert(storage_key.clone(), entry.rulings);
            }

            let normalized = normalize_legalities(&entry.legalities);
            if !normalized.is_empty() {
                legalities.insert(storage_key.clone(), normalized);
            }
        }
        for keys in oracle_id_index.values_mut() {
            keys.sort_by_key(|key| face_order_index.get(key).copied().unwrap_or(usize::MAX));
        }
        let search_face_keys = build_search_face_keys(&face_index, &face_order_index);
        let name_alias_index = build_name_alias_index(face_index.keys());
        let creature_type_vocabulary = collect_creature_type_vocabulary(face_index.values());

        Self {
            cards: HashMap::new(),
            face_index,
            name_alias_index,
            oracle_id_index,
            face_order_index,
            search_face_keys,
            layout_index,
            legalities,
            printings_index,
            rulings_index,
            errors: Vec::new(),
            bracket_lists: BracketLists::default(),
            bracket_signals_by_name,
            creature_type_vocabulary,
        }
    }

    pub fn get_by_name(&self, name: &str) -> Option<&CardRules> {
        let key = self.lookup_key(name);
        self.cards.get(&key)
    }

    pub fn get_face_by_name(&self, name: &str) -> Option<&CardFace> {
        let key = self.lookup_key(name);
        self.face_index.get(&key)
    }

    /// Emit a card-data export JSON containing ONLY the named faces, suitable for
    /// `from_json_str`. Reconstructs each `CardExportEntry` from the in-memory
    /// indices. Legalities are intentionally empty: AI workers never run a
    /// deck-legality check, and the built DB retains only the normalized
    /// `legalities` form (there is no raw `HashMap<String, String>` source to
    /// re-emit — see `from_export_entries`).
    pub fn export_subset_json(&self, names: &std::collections::BTreeSet<String>) -> String {
        let mut out: HashMap<String, CardExportEntry> = HashMap::with_capacity(names.len());
        for name in names {
            let key = self.lookup_key(name);
            let Some(face) = self.face_index.get(&key) else {
                continue;
            };
            let layout = face
                .scryfall_oracle_id
                .as_deref()
                .and_then(|id| self.layout_index.get(id).copied())
                .and_then(layout_kind_to_str)
                .map(str::to_string);
            let entry = CardExportEntry {
                face: face.clone(),
                legalities: HashMap::new(),
                layout,
                face_index: self.face_order_index.get(&key).copied(),
                printings: self.printings_index.get(&key).cloned().unwrap_or_default(),
                rulings: self.rulings_index.get(&key).cloned().unwrap_or_default(),
                bracket_signals: self
                    .bracket_signals_by_name
                    .get(&key)
                    .copied()
                    .unwrap_or_default(),
            };
            // Preserve the database storage key, not merely the printed face
            // name. Meld pairs have two distinct combined-back records with the
            // same printed name and different oracle ids; oracle-gen keeps the
            // loser under a hidden `[oracle-id]` key. Re-keying both by
            // `face.name` here collapsed one half in AI-worker subsets.
            out.insert(key, entry);
        }
        serde_json::to_string(&out).expect("CardExportEntry serialization is infallible")
    }

    /// Resolve a face by its Scryfall oracle id. Used as a fallback when a
    /// name-based lookup fails — e.g. cube/deck imports whose source cached a
    /// pre-reveal placeholder name that no longer matches the printed name.
    /// oracle id is stable across renames, alternate art, and split/Room faces
    /// (which share one oracle id). Returns the first exported face for the id;
    /// for single-face cards that is unambiguous, and split-card imports resolve
    /// by name long before this fallback runs.
    pub fn get_face_by_oracle_id(&self, oracle_id: &str) -> Option<&CardFace> {
        self.oracle_id_index
            .get(oracle_id)?
            .iter()
            .find_map(|name| self.face_index.get(name))
    }

    pub fn get_face_by_printed_ref(&self, printed_ref: &PrintedCardRef) -> Option<&CardFace> {
        self.oracle_id_index
            .get(&printed_ref.oracle_id)?
            .iter()
            .filter_map(|name| self.face_index.get(name))
            .find(|face| face.name == printed_ref.face_name)
    }

    pub fn get_other_face_by_printed_ref(&self, printed_ref: &PrintedCardRef) -> Option<&CardFace> {
        let mut other_faces = self
            .oracle_id_index
            .get(&printed_ref.oracle_id)?
            .iter()
            .filter_map(|name| self.face_index.get(name))
            .filter(|face| face.name != printed_ref.face_name);
        let other = other_faces.next()?;
        if other_faces.next().is_some() {
            return None;
        }
        Some(other)
    }

    pub fn get_legalities(&self, name: &str) -> Option<&CardLegalities> {
        let key = self.lookup_key(name);
        self.legalities.get(&key)
    }

    pub fn legality_status(&self, name: &str, format: LegalityFormat) -> Option<LegalityStatus> {
        self.get_legalities(name)
            .and_then(|m| m.get(&format).copied())
    }

    /// Returns the set codes a card has been printed in (e.g. `["M11", "LEA"]`),
    /// or `None` if the card was loaded via a path that doesn't record printings.
    pub fn printings_for(&self, name: &str) -> Option<&[String]> {
        let key = self.lookup_key(name);
        self.printings_index.get(&key).map(Vec::as_slice)
    }

    /// CR 712.2 + CR 601.3e: whether the face stored under `key` (a
    /// `face_iter` key, already normalized) is the card's FRONT face — the one
    /// a printed card presents outside the battlefield.
    ///
    /// Back faces are stored in `face_index` alongside their fronts and inherit
    /// the whole card's `printings`/`rarities`, so any consumer that enumerates
    /// printings (booster collation, set browsing) must exclude them or the
    /// same physical card is counted twice. Single-faced cards record no face
    /// order and are fronts by definition.
    pub fn is_front_face_key(&self, key: &str) -> bool {
        self.face_order_index.get(key).copied().unwrap_or(0) == 0
    }

    /// Set codes recorded for a `face_iter` key, without the name normalization
    /// [`printings_for`](Self::printings_for) performs. Callers iterating
    /// `face_iter` already hold the storage key; re-deriving it per card costs a
    /// lowercase allocation and an alias lookup for every face in the corpus.
    pub fn printings_for_key(&self, key: &str) -> &[String] {
        self.printings_index
            .get(key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// Returns the official WotC rulings for a card. Returns an empty slice
    /// when the card has no recorded rulings, when the card was loaded via a
    /// path that doesn't record rulings, or when looking up a back-face name
    /// (rulings are attached to the front face only).
    pub fn rulings_for(&self, name: &str) -> &[Ruling] {
        let key = self.lookup_key(name);
        self.rulings_index
            .get(&key)
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    pub fn card_count(&self) -> usize {
        self.cards.len().max(self.face_index.len())
    }

    /// Returns the runtime layout kind for a face identified by oracle_id.
    /// Used by `rehydrate_game_from_card_db` to determine the correct layout
    /// discriminant when `get_by_name` returns None (export loading path).
    pub fn get_layout_kind(&self, oracle_id: &str) -> Option<LayoutKind> {
        self.layout_index.get(oracle_id).copied()
    }

    pub fn export_integrity_errors(&self) -> Vec<String> {
        let mut errors = Vec::new();
        for (oracle_id, layout_kind) in &self.layout_index {
            let face_count = self.oracle_id_index.get(oracle_id).map_or(0, Vec::len);
            if layout_kind_requires_multiple_faces(*layout_kind) && face_count < 2 {
                errors.push(format!(
                    "oracle_id {oracle_id} has layout {layout_kind:?} but only {face_count} exported face(s)"
                ));
            }
        }
        errors.extend(self.unenforceable_static_condition_errors());
        errors
    }

    /// CR 118.12a + CR 601.2f: no exported static ability may carry a condition
    /// whose truth is decided by a round-trip its OWN mode's enforcement point
    /// never runs (`StaticCondition::is_unenforceable_on`).
    ///
    /// Such a condition is a false green, not a bug the player can see: the
    /// layer pipeline hard-codes those leaves to `false`, so the static silently
    /// never applies while coverage reports the gate fully supported. Awesome
    /// Presence (CR 509.1b `CantBeBlocked` + an `UnlessPay` no block-declaration
    /// prompt ever offers) and Hipparion (`BlockRestriction`, same) shipped that
    /// way for exactly as long as the parser-side gate was the only check.
    ///
    /// This is the CORPUS-WIDE half of that gate, and it exists because the
    /// parser-side half is a call-site discipline that has been breached three
    /// times. `oracle_static::static_helpers::gate_static_condition` fires only
    /// where a parser route calls it; this fires on the shipped export no matter
    /// which route built the definition, so a fourth bypass fails CI on the
    /// first card that reaches it. Both read the same predicate, so they cannot
    /// drift apart.
    ///
    /// Reported as an integrity error rather than repaired in place on purpose:
    /// the honest repair needs the clause's Oracle text, which only the parser
    /// has (see `unenforceable_gate_marker`, which labels the gap with it).
    /// Silently substituting a marker here would hide the bypass instead of
    /// surfacing it.
    ///
    /// CR 613.1f + CR 604.1: the walk is over
    /// [`StaticDefinition::walk_self_and_granted`], not over
    /// `face.static_abilities` alone. A `ContinuousModification::
    /// GrantStaticAbility` owns a whole nested `StaticDefinition` — its own
    /// mode, its own scope, and its own `condition` — so the top-level view
    /// leaves every granted definition unchecked, and an unofferable
    /// `UnlessPay` inside one bypasses this backstop exactly the way the
    /// parser-side gate was bypassed three times before it existed. Nesting is
    /// transitive (a granted static may itself grant one), which is why the
    /// recursion lives in the shared walk rather than being open-coded here.
    fn unenforceable_static_condition_errors(&self) -> Vec<String> {
        let mut errors: Vec<String> = self
            .face_index
            .values()
            .flat_map(|face| {
                let mut face_errors = Vec::new();
                for root in &face.static_abilities {
                    // The collector never breaks, so the traversal always runs
                    // to completion and the `ControlFlow` result carries no
                    // information.
                    let _: ControlFlow<()> = root.walk_self_and_granted(&mut |def| {
                        let unenforceable = def
                            .condition
                            .as_ref()
                            .filter(|condition| condition.is_unenforceable_on(&def.mode));
                        if let Some(condition) = unenforceable {
                            face_errors.push(format!(
                                "{}: static {:?} carries a condition its enforcement point can \
                                 never satisfy ({:?}) — it must be routed through \
                                 oracle_static::static_helpers::gate_static_condition",
                                face.name, def.mode, condition
                            ));
                        }
                        ControlFlow::Continue(())
                    });
                }
                face_errors
            })
            .collect();
        // `face_index` is a HashMap, so the natural order is nondeterministic;
        // a CI failure list that reshuffles between runs is unreadable.
        errors.sort();
        errors
    }

    pub fn errors(&self) -> &[(PathBuf, String)] {
        &self.errors
    }

    pub fn iter(&self) -> impl Iterator<Item = (&str, &CardRules)> {
        self.cards.iter().map(|(k, v)| (k.as_str(), v))
    }

    pub fn face_iter(&self) -> impl Iterator<Item = (&str, &CardFace)> {
        self.face_index.iter().map(|(k, v)| (k.as_str(), v))
    }

    /// Every loaded face in the database's deterministic scan order.
    ///
    /// `face_iter` walks a `HashMap` and so yields a different order per
    /// process; this walks the precomputed `search_face_keys` (sorted by
    /// oracle id, then face order, then key), which is identical for any two
    /// loads of the same card data. Use this — never `face_iter` — whenever
    /// the ORDER is load-bearing, such as an RNG draw whose result has to
    /// match across peers and replays.
    pub fn faces_in_scan_order(&self) -> impl Iterator<Item = &CardFace> {
        self.search_face_keys
            .iter()
            .filter_map(|key| self.face_index.get(key))
    }

    /// CR 205.3m: Returns the full creature subtype vocabulary derived from
    /// every loaded creature face. Sorted and deduplicated. Consumers seed
    /// `GameState::all_creature_types` from this so token-only types
    /// (Saproling, Golem, etc.) that no creature card in the loaded decks
    /// shares are still recognized by `SharesQuality::CreatureType`,
    /// `ChoiceType::CreatureType`, and the Changeling expansion.
    pub fn creature_type_vocabulary(&self) -> &[String] {
        &self.creature_type_vocabulary
    }

    /// Returns all card names (title-cased as stored in face data), sorted.
    pub fn card_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self
            .face_index
            .values()
            .map(|face| face.name.clone())
            .collect();
        names.sort();
        names.dedup();
        names
    }

    /// Attach loaded `BracketLists` to the database. Returns `Self` so it can
    /// be chained off `from_export` / `from_json_str` builders.
    pub fn with_bracket_lists(mut self, lists: BracketLists) -> Self {
        self.bracket_lists = lists;
        self
    }

    /// Case-insensitive bracket-signal lookup. Game Changers are card-level
    /// MTGJSON facts stamped into `bracket_signals_by_name`; other axes may
    /// come from either the export or `bracket_lists`. Returns all-false
    /// `BracketSignals` when the name is unknown to both.
    ///
    /// Multi-face combined names (`"A // B"` — partner pairs, MDFCs, split,
    /// etc.) are aggregated face-by-face with logical-OR *before* the
    /// single-face fast path. `lookup_key` collapses combined names to their
    /// front face, so without this pre-split a back-face signal would be
    /// silently dropped whenever the front face is in the export map.
    ///
    /// The split is `split_composite_name`, the one [`Self::lookup_key`] uses.
    /// A narrower split would leave a composite name it does not accept
    /// aggregating only its front face's signals, silently losing a back-face
    /// Game Changer / mass-land-denial / extra-turn signal in Commander
    /// bracket classification.
    ///
    /// A single-faced card whose printed name contains a separator
    /// (`"SP//dr, Piloted by Peni"`, `"Summon: Choco/Mog"`) must NOT be split,
    /// so the whole-name lookup is tried first — the same false-positive
    /// guard, and the same ordering, that `lookup_key` documents. "Whole name"
    /// here spans every source this function reads, `bracket_lists` included,
    /// since a curated list entry may name a card the export map does not
    /// carry, and the unaccented-alias index, which `lookup_key` folds through
    /// before it splits.
    pub fn bracket_signals_for(&self, name: &str) -> BracketSignals {
        // Exact-match guard only: a name that is ITSELF an indexed card must
        // not be split. Deliberately not `lookup_key`, which collapses a
        // composite name to its front face and would therefore report every
        // composite name whose front face is indexed as a "whole name",
        // suppressing the very aggregation this function exists to perform.
        let lower = name.to_lowercase();
        // `bracket_lists` is a third source of whole printed names: it keys on
        // the raw lowercased name with no index membership, so a curated-list
        // card that is absent from the export map is known ONLY here. Omitting
        // it would split such a name on its literal `//` into two nonexistent
        // faces and report all-false, dropping its real signal. It takes the
        // ORIGINAL `name`, not `lower`: `contains` owns its own case folding,
        // so passing the pre-folded copy would fold twice and imply the lookup
        // is case-sensitive to a future caller.
        // The unaccented-alias index is a fourth whole-name source, and
        // `lookup_key` folds through it BEFORE its composite split. Omitting
        // it here would make the two functions disagree about what is a
        // composite name — the exact failure `lookup_key`'s doc warns is
        // wrong by construction — for a single-faced card whose printed name
        // carries a separator and a diacritic, typed unaccented.
        let is_indexed_whole_name = self.face_index.contains_key(&lower)
            || self.cards.contains_key(&lower)
            || self.bracket_lists.contains(name)
            || self
                .name_alias_index
                .contains_key(&fold_card_name_key(name));
        if let Some((a, b)) = split_composite_name(name).filter(|_| !is_indexed_whole_name) {
            let sa = self.signals_for_single_face(a.trim());
            let sb = self.signals_for_single_face(b.trim());
            return BracketSignals {
                game_changer: sa.game_changer || sb.game_changer,
                mass_land_denial: sa.mass_land_denial || sb.mass_land_denial,
                extra_turn: sa.extra_turn || sb.extra_turn,
                efficient_tutor: sa.efficient_tutor || sb.efficient_tutor,
            };
        }
        self.signals_for_single_face(name)
    }

    fn signals_for_single_face(&self, name: &str) -> BracketSignals {
        let key = self.lookup_key(name);
        let list_signals = self.bracket_lists.signals_for(name);
        let Some(card_signals) = self.bracket_signals_by_name.get(&key) else {
            return list_signals;
        };
        BracketSignals {
            game_changer: card_signals.game_changer,
            mass_land_denial: card_signals.mass_land_denial || list_signals.mass_land_denial,
            extra_turn: card_signals.extra_turn || list_signals.extra_turn,
            efficient_tutor: card_signals.efficient_tutor || list_signals.efficient_tutor,
        }
    }

    /// Single authority for resolving any caller-supplied card name — including
    /// a multi-face composite name (`"Front // Back"`) — to a key in
    /// `face_index` / `cards`. Every name-keyed accessor on `CardDatabase`
    /// routes through here or through the [`Self::resolve_name`] it wraps; no
    /// caller may re-implement composite-name splitting.
    ///
    /// Resolution order is significant and must be preserved:
    /// 1. Exact (lowercased) match. This MUST precede the composite split so a
    ///    single-faced card whose printed name contains a separator
    ///    (`"SP//dr, Piloted by Peni"`, `"Summon: Choco/Mog"`) is not mistaken
    ///    for a composite name.
    /// 2. Alias fold (`build_name_alias_index`).
    /// 3. Composite split (`split_composite_name`), taking the **front** face,
    ///    then retrying steps 1 and 2 against that front segment.
    ///
    /// Collapsing to the front face is correct for decklist *identity*
    /// resolution: a composite name denotes exactly one card. CR 709.2: although
    /// split cards have two castable halves, each split card is only one card,
    /// so a deck entry for `"Fire // Ice"` is one copy of that card, not two.
    /// It is deliberately lossy in the other direction — the back half is not
    /// reachable through this function, and CR 709.4a (each split card has two
    /// names, and an effect choosing a name must choose one half, not both)
    /// means name-choice effects must not be routed through this collapse.
    /// Callers that genuinely need per-face data for a composite name must
    /// split the name themselves and query each face, the way
    /// [`Self::bracket_signals_for`] does; do not widen `lookup_key` to return
    /// multiple keys. Such a caller must split with `split_composite_name` and
    /// apply the same exact-match-first guard, or it will disagree with
    /// `lookup_key` about what is a composite name — `bracket_signals_for` is
    /// the worked example.
    ///
    /// `data/card-data.json` stores each face under its own key and contains no
    /// composite `"A // B"` keys, so composite-name support rests entirely on
    /// the composite split in [`Self::resolve_name`] with no data-level
    /// backstop. The regression barrier is therefore the `get_face_by_name`
    /// tests in this module; any refactor of this function must keep them
    /// passing.
    ///
    /// `pub(crate)` so in-crate name-keyed code (notably deck validation, which
    /// keys copy counts and coverage buckets by resolved name) can reuse this
    /// one resolution instead of re-implementing the composite split with a
    /// different — and therefore wrong — ordering.
    pub(crate) fn lookup_key(&self, name: &str) -> String {
        self.resolve_name(name)
            .map_or_else(|| name.to_lowercase(), |resolved| resolved.key)
    }

    /// [`Self::lookup_key`]'s resolution, also reporting which step matched;
    /// `None` when no step did.
    fn resolve_name(&self, name: &str) -> Option<ResolvedName> {
        let lower = name.to_lowercase();
        if self.face_index.contains_key(&lower) || self.cards.contains_key(&lower) {
            return Some(ResolvedName {
                key: lower,
                matched: NameMatch::WholeName,
            });
        }
        if let Some(alias) = self.name_alias_index.get(&fold_card_name_key(name)) {
            return Some(ResolvedName {
                key: alias.clone(),
                matched: NameMatch::WholeName,
            });
        }
        if let Some((front, _)) = split_composite_name(&lower) {
            let front = front.trim();
            if self.face_index.contains_key(front) || self.cards.contains_key(front) {
                return Some(ResolvedName {
                    key: front.to_string(),
                    matched: NameMatch::CompositeFront,
                });
            }
            if let Some(alias) = self.name_alias_index.get(&fold_card_name_key(front)) {
                return Some(ResolvedName {
                    key: alias.clone(),
                    matched: NameMatch::CompositeFront,
                });
            }
        }
        None
    }

    /// The printed spelling of the card `name` resolves to, in the shape
    /// `name` has: a name that resolves whole yields that face's printed
    /// name, and a composite name whose segments name every face of one
    /// card, in face order, yields those faces' printed names joined with
    /// `" // "`. A name that resolves to a single face is never widened to a
    /// composite.
    ///
    /// `None` when `name` does not resolve, when a composite name's segments
    /// are not exactly its card's faces (a partner pair, a reversed or
    /// partial spelling), or when the printed spelling would not resolve back
    /// to the same key through [`Self::lookup_key`].
    pub fn canonical_name(&self, name: &str) -> Option<String> {
        let resolved = self.resolve_name(name)?;
        let face = self.face_index.get(&resolved.key)?;
        let printed = match resolved.matched {
            NameMatch::WholeName => face.name.clone(),
            NameMatch::CompositeFront => {
                let faces = self.faces_of_card(face);
                let segments = composite_name_segments(name);
                let names_every_face = segments.len() == faces.len()
                    && segments.iter().zip(&faces).all(|(segment, card_face)| {
                        self.get_face_by_name(segment)
                            .is_some_and(|named| named.name == card_face.name)
                    });
                if !names_every_face {
                    return None;
                }
                faces
                    .iter()
                    .map(|card_face| card_face.name.as_str())
                    .collect::<Vec<_>>()
                    .join(" // ")
            }
        };
        (self.lookup_key(&printed) == resolved.key).then_some(printed)
    }

    /// The faces stored under `face`'s oracle id, in `oracle_id_index` order;
    /// just `face` when it has no oracle id or none of that id's keys is
    /// indexed.
    fn faces_of_card<'a>(&'a self, face: &'a CardFace) -> Vec<&'a CardFace> {
        let siblings: Vec<&CardFace> = face
            .scryfall_oracle_id
            .as_deref()
            .and_then(|oracle_id| self.oracle_id_index.get(oracle_id))
            .map(|keys| {
                keys.iter()
                    .filter_map(|key| self.face_index.get(key))
                    .collect()
            })
            .unwrap_or_default();
        if siblings.is_empty() {
            vec![face]
        } else {
            siblings
        }
    }
}

pub(crate) fn build_search_face_keys(
    face_index: &HashMap<String, CardFace>,
    face_order_index: &HashMap<String, usize>,
) -> Vec<String> {
    let mut keys: Vec<String> = face_index.keys().cloned().collect();
    keys.sort_by(|left_key, right_key| {
        let left_oracle = face_index
            .get(left_key)
            .and_then(|face| face.scryfall_oracle_id.as_deref())
            .unwrap_or("");
        let right_oracle = face_index
            .get(right_key)
            .and_then(|face| face.scryfall_oracle_id.as_deref())
            .unwrap_or("");
        left_oracle
            .cmp(right_oracle)
            .then_with(|| {
                face_order_index
                    .get(left_key)
                    .copied()
                    .unwrap_or(usize::MAX)
                    .cmp(
                        &face_order_index
                            .get(right_key)
                            .copied()
                            .unwrap_or(usize::MAX),
                    )
            })
            .then_with(|| left_key.cmp(right_key))
    });
    keys
}

/// CR 205.2b + CR 205.3m + CR 308.1: subtype categories are disjoint — a
/// creature type (shared by Creature and Kindred, legacy Tribal, faces) never
/// appears on a non-creature face, while land/artifact/enchantment subtypes
/// always have pure non-creature representatives in the corpus. MTGJSON
/// flattens every face's subtypes into a single array, so a multi-type creature
/// face ("Land Creature — Forest Dryad", "Artifact Creature — Equipment
/// Construct", "Enchantment Creature — Shrine") carries non-creature subtypes
/// (Forest, Equipment, Shrine) alongside the genuine creature type. Collect
/// candidate subtypes from creature/kindred/tribal faces, then subtract every
/// subtype that also appears on any non-creature face — those are
/// land/artifact/enchantment/spell types, never creature types. Returns the
/// sorted, deduped creature-type vocabulary.
pub(crate) fn collect_creature_type_vocabulary<'a>(
    faces: impl Iterator<Item = &'a CardFace>,
) -> Vec<String> {
    let mut creature_candidates: HashSet<&str> = HashSet::new();
    let mut non_creature_subtypes: HashSet<&str> = HashSet::new();
    for face in faces {
        let core_types = &face.card_type.core_types;
        let is_creature_face = core_types.contains(&CoreType::Creature)
            || core_types.contains(&CoreType::Kindred)
            || core_types.contains(&CoreType::Tribal);
        let bucket = if is_creature_face {
            &mut creature_candidates
        } else {
            &mut non_creature_subtypes
        };
        bucket.extend(face.card_type.subtypes.iter().map(String::as_str));
    }
    let mut sorted: Vec<String> = creature_candidates
        .difference(&non_creature_subtypes)
        .map(|s| s.to_string())
        .collect();
    sorted.sort();
    sorted
}

pub(crate) fn build_name_alias_index<'a>(
    keys: impl Iterator<Item = &'a String>,
) -> HashMap<String, String> {
    let mut aliases: HashMap<String, Option<String>> = HashMap::new();
    for key in keys {
        let mut register_alias = |alias: String| {
            aliases
                .entry(alias)
                .and_modify(|existing| {
                    if existing.as_deref() != Some(key.as_str()) {
                        *existing = None;
                    }
                })
                .or_insert_with(|| Some(key.clone()));
        };

        let folded = fold_card_name_key(key);
        if folded != *key {
            register_alias(folded);
        }

        // Deck imports often drop the leading article ("Eleventh Doctor" vs
        // "The Eleventh Doctor"). Register the stripped form when unambiguous.
        if let Some(stripped) = key.strip_prefix("the ").filter(|s| !s.is_empty()) {
            register_alias(fold_card_name_key(stripped));
        }

        // Decks saved while the client's deck repair rewrote a bare `/` to
        // ` // ` carry a name like "Summon: Choco/Mog" as "Summon: Choco // Mog".
        // Register that spelling as an alias of the printed name; the composite
        // split would otherwise read it as a multi-face name.
        if !key.contains("//") {
            let segments: Vec<&str> = key
                .split('/')
                .map(str::trim)
                .filter(|segment| !segment.is_empty())
                .collect();
            if segments.len() > 1 {
                register_alias(fold_card_name_key(&segments.join(" // ")));
            }
        }
    }
    aliases
        .into_iter()
        .filter_map(|(alias, key)| key.map(|key| (alias, key)))
        .collect()
}

/// The face separator of a composite multi-face name: the first `//` (spaced
/// or glued) when the name contains one, otherwise the first single `/`.
/// Returns the text before and after it, untrimmed. A printed name can itself
/// contain either separator (`"SP//dr, Piloted by Peni"`, `"Summon: Choco/Mog"`),
/// so a caller must try the whole name as a key before splitting.
fn split_composite_name(name: &str) -> Option<(&str, &str)> {
    name.split_once("//").or_else(|| name.split_once('/'))
}

/// `name` split at every separator [`split_composite_name`] finds, each
/// segment trimmed.
fn composite_name_segments(name: &str) -> Vec<&str> {
    let mut segments = Vec::new();
    let mut rest = name;
    while let Some((head, tail)) = split_composite_name(rest) {
        segments.push(head.trim());
        rest = tail;
    }
    segments.push(rest.trim());
    segments
}

/// Which step of [`CardDatabase::resolve_name`] matched a name.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NameMatch {
    /// The whole name, exactly or through `name_alias_index`.
    WholeName,
    /// Only the front segment of a composite name.
    CompositeFront,
}

struct ResolvedName {
    key: String,
    matched: NameMatch,
}

fn fold_card_name_key(name: &str) -> String {
    let mut folded = String::with_capacity(name.len());
    for ch in name.chars() {
        for lower in ch.to_lowercase() {
            match lower {
                'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' | 'ă' | 'ą' => folded.push('a'),
                'ç' | 'ć' | 'ĉ' | 'ċ' | 'č' => folded.push('c'),
                'ď' | 'đ' => folded.push('d'),
                'é' | 'è' | 'ê' | 'ë' | 'ē' | 'ĕ' | 'ė' | 'ę' | 'ě' => folded.push('e'),
                'ĝ' | 'ğ' | 'ġ' | 'ģ' => folded.push('g'),
                'ĥ' | 'ħ' => folded.push('h'),
                'í' | 'ì' | 'î' | 'ï' | 'ĩ' | 'ī' | 'ĭ' | 'į' | 'ı' => folded.push('i'),
                'ĵ' => folded.push('j'),
                'ķ' => folded.push('k'),
                'ĺ' | 'ļ' | 'ľ' | 'ŀ' | 'ł' => folded.push('l'),
                'ñ' | 'ń' | 'ņ' | 'ň' | 'ŉ' => folded.push('n'),
                'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ō' | 'ŏ' | 'ő' | 'ø' => folded.push('o'),
                'ŕ' | 'ŗ' | 'ř' => folded.push('r'),
                'ś' | 'ŝ' | 'ş' | 'š' => folded.push('s'),
                'ţ' | 'ť' | 'ŧ' => folded.push('t'),
                'ú' | 'ù' | 'û' | 'ü' | 'ũ' | 'ū' | 'ŭ' | 'ů' | 'ű' | 'ų' => {
                    folded.push('u')
                }
                'ŵ' => folded.push('w'),
                'ý' | 'ÿ' | 'ŷ' => folded.push('y'),
                'ź' | 'ż' | 'ž' => folded.push('z'),
                'æ' => folded.push_str("ae"),
                'œ' => folded.push_str("oe"),
                'þ' => folded.push_str("th"),
                'ð' => folded.push('d'),
                'ß' => folded.push_str("ss"),
                '’' | '‘' | '＇' => folded.push('\''),
                _ => folded.push(lower),
            }
        }
    }
    folded
}

#[derive(Debug, Clone, Deserialize, Serialize)]
struct CardExportEntry {
    #[serde(flatten)]
    face: CardFace,
    #[serde(default)]
    legalities: HashMap<String, String>,
    /// MTGJSON layout string for multi-face cards (e.g. "modal_dfc", "transform").
    #[serde(default)]
    layout: Option<String>,
    /// Original zero-based position of this face within MTGJSON's multi-face
    /// record. Optional so older card-data exports remain loadable.
    #[serde(default)]
    face_index: Option<usize>,
    /// Set codes the card has been printed in (from MTGJSON `printings`).
    #[serde(default)]
    printings: Vec<String>,
    /// Official WotC rulings; populated on the front face only for multi-face cards.
    #[serde(default)]
    rulings: Vec<Ruling>,
    /// Bracket-axis signals stamped by the export pipeline (Task 4). Cards
    /// exported before Task 4 will deserialize to all-false `BracketSignals::default()`.
    #[serde(default)]
    bracket_signals: BracketSignals,
}

fn layout_kind_requires_multiple_faces(layout_kind: LayoutKind) -> bool {
    matches!(
        layout_kind,
        LayoutKind::Split
            | LayoutKind::Flip
            | LayoutKind::Transform
            | LayoutKind::Meld
            | LayoutKind::Adventure
            | LayoutKind::Modal
            | LayoutKind::Omen
            | LayoutKind::Prepare
    )
}

/// Exhaustive inverse of `map_layout_str`: runtime `LayoutKind` → the MTGJSON
/// layout string `from_export_entries` expects. `Single` has no string form
/// (single-face cards carry no layout discriminant). No wildcard arm, so a new
/// `LayoutKind` variant forces a compile error here until it is mapped.
fn layout_kind_to_str(kind: LayoutKind) -> Option<&'static str> {
    match kind {
        LayoutKind::Modal => Some("modal_dfc"),
        LayoutKind::Transform => Some("transform"),
        LayoutKind::Adventure => Some("adventure"),
        LayoutKind::Meld => Some("meld"),
        LayoutKind::Split => Some("split"),
        LayoutKind::Flip => Some("flip"),
        LayoutKind::Omen => Some("omen"),
        LayoutKind::Prepare => Some("prepare"),
        LayoutKind::Single => None,
    }
}

/// Convert MTGJSON layout string to runtime `LayoutKind`.
/// Returns `None` for single-face layouts since they don't need a layout discriminant.
fn map_layout_str(s: &str) -> Option<LayoutKind> {
    match s {
        "modal_dfc" => Some(LayoutKind::Modal),
        "transform" => Some(LayoutKind::Transform),
        "adventure" => Some(LayoutKind::Adventure),
        "meld" => Some(LayoutKind::Meld),
        "split" => Some(LayoutKind::Split),
        "flip" => Some(LayoutKind::Flip),
        "omen" => Some(LayoutKind::Omen),
        // CR 702.xxx: Prepare (Strixhaven) — Adventure-family frame. Assign
        // when WotC publishes SOS CR update.
        "prepare" => Some(LayoutKind::Prepare),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_source_full_symbiote_survives_all_three_loaders() {
        let oracle = "Whenever this creature deals combat damage to a player, look at that many cards from the top of your library. Put one of them into your hand and the rest into your graveyard.\nFind New Host — {2}{U/B}, Exile this card from your graveyard: Put a +1/+1 counter on target creature you control. It gains this card's other abilities. Activate only as a sorcery.";
        let raw = serde_json::json!({"data": {"Symbiote Spider-Man": [{"name":"Symbiote Spider-Man", "manaCost":"{2}{U/B}", "colors":["U","B"], "colorIdentity":["U","B"], "layout":"normal", "type":"Legendary Creature — Symbiote Spider Hero", "types":["Creature"], "subtypes":["Symbiote","Spider","Hero"], "supertypes":["Legendary"], "power":"2", "toughness":"4", "manaValue":3, "identifiers":{}, "text":oracle}]}});
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
        let raw_db = CardDatabase::from_mtgjson(file.path()).unwrap();
        let export = raw_db.export_subset_json(&std::collections::BTreeSet::from([
            "Symbiote Spider-Man".into(),
        ]));
        let exported = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(exported.path(), &export).unwrap();
        let assert_face = |db: &CardDatabase| {
            let face = db.get_face_by_name("Symbiote Spider-Man").unwrap();
            assert_eq!(face.oracle_text.as_deref(), Some(oracle));
            assert_eq!(
                face.triggers.len(),
                1,
                "complete first printed ability retained"
            );
            let json = serde_json::to_string(face).unwrap();
            assert!(!json.contains("Unimplemented"));
            assert!(json.contains("GainActivatedAbilitiesOfTarget"));
            assert!(json.contains(r#""reference":"Source""#));
            assert!(json.contains("AllOther"));
            serde_json::to_value(face).unwrap()
        };
        let expected = assert_face(&raw_db);
        assert_eq!(
            assert_face(&CardDatabase::from_export(exported.path()).unwrap()),
            expected
        );
        assert_eq!(
            assert_face(&CardDatabase::from_json_str(&export).unwrap()),
            expected
        );
    }

    #[test]
    fn u14_source_parity_loader_routes() {
        use crate::types::ability::{
            ChoiceType, ContinuousModification, Effect, FilterProp, ParitySource,
        };
        let oracle = "As this enchantment enters, choose odd or even. (Zero is even.)\nEach creature with mana value of the chosen quality has haste.";
        let raw = serde_json::json!({"data": {"Parity Standard": [{"name":"Parity Standard", "colors":[], "colorIdentity":[], "layout":"normal", "type":"Enchantment", "types":["Enchantment"], "manaValue":0, "identifiers":{}, "text":oracle}]}});
        let file = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(file.path(), serde_json::to_vec(&raw).unwrap()).unwrap();
        let raw_db = CardDatabase::from_mtgjson(file.path()).unwrap();
        let export =
            raw_db.export_subset_json(&std::collections::BTreeSet::from(
                ["Parity Standard".into()],
            ));
        let assert_face = |db: &CardDatabase| {
            let face = db.get_face_by_name("Parity Standard").unwrap();
            assert_eq!(face.oracle_text.as_deref(), Some(oracle));
            assert!(face
                .replacements
                .iter()
                .any(|d| d.execute.as_deref().is_some_and(|a| matches!(
                    a.effect.as_ref(),
                    Effect::Choose {
                        choice_type: ChoiceType::OddOrEven,
                        persist: true,
                        ..
                    }
                ))));
            assert!(face
                .static_abilities
                .iter()
                .any(|s| s.affected.as_ref().is_some_and(|f| {
                    crate::game::filter::filter_contains_filter_prop(f, &|p| {
                        matches!(
                            p,
                            FilterProp::ManaValueParity {
                                parity: ParitySource::SourceChosen
                            }
                        )
                    })
                }) && s
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Haste
                    })));
        };
        assert_face(&raw_db);
        assert_face(&CardDatabase::from_export_reader(export.as_bytes()).unwrap());
        assert_face(&CardDatabase::from_json_str(&export).unwrap());
        for tag in [
            serde_json::json!({"type":"Fixed","value":"Even"}),
            serde_json::json!({"type":"LastNamedChoice"}),
        ] {
            let mut value: serde_json::Value = serde_json::from_str(&export).unwrap();
            value["parity standard"]["static_abilities"][0]["affected"]["properties"][0]
                ["parity"] = tag.clone();
            let db = CardDatabase::from_json_str(&value.to_string()).unwrap();
            let json =
                serde_json::to_value(db.get_face_by_name("Parity Standard").unwrap()).unwrap();
            assert_eq!(
                json["static_abilities"][0]["affected"]["properties"][0]["parity"],
                tag
            );
        }
        let mut wrong: serde_json::Value = serde_json::from_str(&export).unwrap();
        wrong["parity standard"]["static_abilities"][0]["affected"]["properties"][0]["parity"] =
            serde_json::json!({"type":"UnknownParity"});
        assert!(CardDatabase::from_json_str(&wrong.to_string()).is_err());
    }
    use crate::types::ability::{
        AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
    };
    use crate::types::card_type::CardType;
    use crate::types::keywords::Keyword;
    use crate::types::mana::ManaCost;

    fn test_face(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            mana_cost: ManaCost::NoCost,
            card_type: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            defense: None,
            oracle_text: None,
            non_ability_text: None,
            flavor_name: None,
            keywords: Vec::<Keyword>::new(),
            abilities: Vec::<AbilityDefinition>::new(),
            triggers: Vec::<TriggerDefinition>::new(),
            static_abilities: Vec::<StaticDefinition>::new(),
            replacements: Vec::<ReplacementDefinition>::new(),
            cleave_variant: None,
            color_override: None,
            color_identity: vec![],
            scryfall_oracle_id: None,
            modal: None,
            additional_cost: None,
            strive_cost: None,
            casting_restrictions: vec![],
            casting_options: vec![],
            solve_condition: None,
            parse_warnings: vec![],
            brawl_commander: false,
            is_commander: false,
            is_oathbreaker: false,
            deck_copy_limit: None,
            metadata: Default::default(),
            rarities: Default::default(),
            attraction_lights: vec![],
        }
    }

    /// CR 118.12a: the corpus-wide half of the unenforceable-gate authority.
    ///
    /// Both directions matter and neither is exercised by the shipped export
    /// today (the parser gate defers every such condition before it reaches
    /// here), so this is the only thing that proves the gate is not vacuous:
    /// the ACCEPT direction pins that a legitimate `UnlessPay` on a combat-taxed
    /// mode — Ghostly Prison, the card the whole enforcement-point axis exists
    /// to keep working — is not swept up, and the REJECT direction pins that the
    /// same leaf on a mode with no payment prompt fails the export.
    #[test]
    fn export_integrity_rejects_only_conditions_their_mode_can_never_satisfy() {
        use crate::types::ability::{StaticCondition, UnlessPayScaling};
        use crate::types::mana::ManaCost;
        use crate::types::statics::StaticMode;

        let pay_gate = || StaticCondition::UnlessPay {
            cost: ManaCost::NoCost,
            scaling: UnlessPayScaling::default(),
            defended: None,
        };
        let face_with = |name: &str, mode: StaticMode| {
            let mut face = test_face(name);
            let mut def = StaticDefinition::new(mode);
            def.condition = Some(pay_gate());
            face.static_abilities = vec![def];
            face
        };

        // ACCEPT: CR 508.1h — `WaitingFor::CombatTaxPayment` prompts the
        // attacking player at declaration, so the gate is satisfiable.
        let mut taxed = HashMap::new();
        taxed.insert(
            "ghostly prison".to_string(),
            face_with("Ghostly Prison", StaticMode::CantAttack),
        );
        let db =
            CardDatabase::from_json_str(&serde_json::to_string(&taxed).unwrap()).expect("parses");
        assert!(
            db.export_integrity_errors().is_empty(),
            "a payment gate on a combat-taxed mode is enforceable and must pass: {:?}",
            db.export_integrity_errors()
        );

        // REJECT: CR 509.1b — no prompt exists at block declaration against an
        // evasion static, so the layer pipeline hard-codes the leaf `false`.
        let mut untaxed = HashMap::new();
        untaxed.insert(
            "probe".to_string(),
            face_with("Untaxed Probe", StaticMode::CantBeBlocked),
        );
        let db =
            CardDatabase::from_json_str(&serde_json::to_string(&untaxed).unwrap()).expect("parses");
        let errors = db.export_integrity_errors();
        assert!(
            errors.iter().any(|e| e.contains("Untaxed Probe")),
            "a payment gate on a mode with no payment prompt must fail the export \
             no matter which parser route built it, got {errors:?}"
        );
    }

    /// CR 613.1f + CR 604.1 + CR 118.12a: a granted static ability is a static
    /// ability, so the unenforceable-gate backstop must reach the condition on
    /// the definition a `ContinuousModification::GrantStaticAbility` nests —
    /// and on the definition THAT one nests, transitively.
    ///
    /// Regression for the top-level-only view: the outer definition here is
    /// deliberately clean (no condition at all, and a mode that WOULD accept a
    /// payment gate), so the only thing that can fail the export is the inner
    /// definition's leaf. Before the walk existed this face shipped reported as
    /// fully supported while the inner `CantBeBlocked` gate was hard-coded
    /// `false` by the layer pipeline — the exact Awesome Presence shape, one
    /// level down.
    #[test]
    fn export_integrity_reaches_conditions_on_nested_granted_statics() {
        use crate::types::ability::{ContinuousModification, StaticCondition, UnlessPayScaling};
        use crate::types::mana::ManaCost;
        use crate::types::statics::StaticMode;

        let pay_gate = || StaticCondition::UnlessPay {
            cost: ManaCost::NoCost,
            scaling: UnlessPayScaling::default(),
            defended: None,
        };

        // Innermost: CR 509.1b — no block-declaration prompt exists, so this
        // leaf is the unofferable gate.
        let mut inner = StaticDefinition::new(StaticMode::CantBeBlocked);
        inner.condition = Some(pay_gate());

        // Middle: a granted static that is itself clean, proving the walk does
        // not stop at the first level of nesting.
        let mut middle = StaticDefinition::continuous();
        middle.modifications = vec![ContinuousModification::GrantStaticAbility {
            definition: Box::new(inner),
        }];

        // Outer/top-level: clean, and on `CantAttack`, whose CR 508.1h combat-tax
        // prompt makes a payment gate legitimately enforceable — so a top-level
        // -only check finds nothing to report on this face.
        let mut outer = StaticDefinition::new(StaticMode::CantAttack);
        outer.modifications = vec![ContinuousModification::GrantStaticAbility {
            definition: Box::new(middle),
        }];

        let mut faces = HashMap::new();
        let mut face = test_face("Nested Grant Probe");
        face.static_abilities = vec![outer];
        faces.insert("nested grant probe".to_string(), face);
        let db =
            CardDatabase::from_json_str(&serde_json::to_string(&faces).unwrap()).expect("parses");

        let errors = db.export_integrity_errors();
        assert!(
            errors
                .iter()
                .any(|e| e.contains("Nested Grant Probe") && e.contains("CantBeBlocked")),
            "an unofferable payment gate two levels inside GrantStaticAbility must fail \
             the export rather than leaving the card falsely supported, got {errors:?}"
        );

        // ACCEPT direction at depth: the same nesting with an enforceable inner
        // mode must still pass, so the recursion is not a blanket rejection of
        // every nested condition.
        let mut inner_ok = StaticDefinition::new(StaticMode::CantAttack);
        inner_ok.condition = Some(pay_gate());
        let mut outer_ok = StaticDefinition::continuous();
        outer_ok.modifications = vec![ContinuousModification::GrantStaticAbility {
            definition: Box::new(inner_ok),
        }];
        let mut ok_faces = HashMap::new();
        let mut ok_face = test_face("Nested Taxed Probe");
        ok_face.static_abilities = vec![outer_ok];
        ok_faces.insert("nested taxed probe".to_string(), ok_face);
        let ok_db = CardDatabase::from_json_str(&serde_json::to_string(&ok_faces).unwrap())
            .expect("parses");
        assert!(
            ok_db.export_integrity_errors().is_empty(),
            "a nested payment gate on a combat-taxed mode is enforceable and must pass: {:?}",
            ok_db.export_integrity_errors()
        );
    }

    #[test]
    fn from_json_str_parses_legacy_face_map_without_legalities() {
        let mut map = HashMap::new();
        map.insert("test card".to_string(), test_face("Test Card"));
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();
        assert!(db.get_face_by_name("Test Card").is_some());
        assert!(db.get_legalities("Test Card").is_none());
    }

    #[test]
    fn from_json_str_parses_extended_export_with_legalities() {
        let mut map = serde_json::Map::new();
        map.insert(
            "test card".to_string(),
            serde_json::json!({
                "name": "Test Card",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "standard": "Legal",
                    "premodern": "Banned",
                    "commander": "not_legal"
                }
            }),
        );

        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.legality_status("Test Card", LegalityFormat::Standard),
            Some(LegalityStatus::Legal)
        );
        assert_eq!(
            db.legality_status("Test Card", LegalityFormat::Commander),
            Some(LegalityStatus::NotLegal)
        );
        assert_eq!(
            db.legality_status("Test Card", LegalityFormat::Premodern),
            Some(LegalityStatus::Banned)
        );
    }

    #[test]
    fn from_json_str_roundtrips_premodern_legalities_without_set_inference() {
        let mut map = serde_json::Map::new();
        map.insert(
            "lightning bolt".to_string(),
            serde_json::json!({
                "name": "Lightning Bolt",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "premodern": "Legal"
                }
            }),
        );
        map.insert(
            "channel".to_string(),
            serde_json::json!({
                "name": "Channel",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": [], "subtypes": [] },
                "power": null,
                "toughness": null,
                "loyalty": null,
                "defense": null,
                "oracle_text": null,
                "non_ability_text": null,
                "flavor_name": null,
                "keywords": [],
                "abilities": [],
                "triggers": [],
                "static_abilities": [],
                "replacements": [],
                "color_override": null,
                "scryfall_oracle_id": null,
                "legalities": {
                    "premodern": "Banned"
                }
            }),
        );

        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.legality_status("Lightning Bolt", LegalityFormat::Premodern),
            Some(LegalityStatus::Legal)
        );
        assert_eq!(
            db.legality_status("Channel", LegalityFormat::Premodern),
            Some(LegalityStatus::Banned)
        );
    }

    #[test]
    fn name_lookup_accepts_unaccented_aliases() {
        let mut map = HashMap::new();
        map.insert("séance board".to_string(), test_face("Séance Board"));
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("Seance Board")
                .map(|face| face.name.as_str()),
            Some("Séance Board")
        );
    }

    #[test]
    fn name_aliases_skip_ambiguous_folds() {
        let mut map = HashMap::new();
        map.insert("café".to_string(), test_face("Café"));
        map.insert("cafe".to_string(), test_face("Cafe"));
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("Cafe").map(|face| face.name.as_str()),
            Some("Cafe")
        );
    }

    #[test]
    fn combined_face_name_lookup_resolves_front_face() {
        let mut map = HashMap::new();
        map.insert(
            "brigid, clachan's heart".to_string(),
            test_face("Brigid, Clachan's Heart"),
        );
        map.insert(
            "brigid, doun's mind".to_string(),
            test_face("Brigid, Doun's Mind"),
        );
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("Brigid, Clachan's Heart // Brigid, Doun's Mind")
                .map(|face| face.name.as_str()),
            Some("Brigid, Clachan's Heart")
        );
    }

    #[test]
    fn single_face_name_containing_double_slash_resolves_to_itself() {
        // "SP//dr, Piloted by Peni" is a single-faced card whose printed name
        // literally contains "//". lookup_key must match the exact name before
        // falling back to its "//"-split, so the card is not mistaken for a
        // "front // back" combined name (issue #4790).
        let mut map = HashMap::new();
        map.insert(
            "sp//dr, piloted by peni".to_string(),
            test_face("SP//dr, Piloted by Peni"),
        );
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("SP//dr, Piloted by Peni")
                .map(|face| face.name.as_str()),
            Some("SP//dr, Piloted by Peni")
        );
    }

    #[test]
    fn glued_combined_face_name_resolves_front_face() {
        // A hand-typed glued combined name ("Front//Back", no spaces) resolves to
        // the front face via lookup_key's bare-"//" split, identically to the
        // canonical spaced form — so a deck listing a DFC either way still loads.
        let mut map = HashMap::new();
        map.insert("peter parker".to_string(), test_face("Peter Parker"));
        map.insert(
            "the amazing spider-man".to_string(),
            test_face("The Amazing Spider-Man"),
        );
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("Peter Parker//The Amazing Spider-Man")
                .map(|face| face.name.as_str()),
            Some("Peter Parker")
        );
        assert_eq!(
            db.get_face_by_name("Peter Parker // The Amazing Spider-Man")
                .map(|face| face.name.as_str()),
            Some("Peter Parker")
        );
    }

    #[test]
    fn name_lookup_resolves_card_names_without_leading_the() {
        let mut map = serde_json::Map::new();
        map.insert(
            "the eleventh doctor".to_string(),
            serde_json::json!({
                "name": "The Eleventh Doctor",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": ["Time Lord", "Doctor"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null, "legalities": {}
            }),
        );
        map.insert(
            "the séance doctor".to_string(),
            serde_json::json!({
                "name": "The Séance Doctor",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": ["Legendary"], "core_types": ["Creature"], "subtypes": ["Time Lord", "Doctor"] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "non_ability_text": null, "flavor_name": null,
                "keywords": [], "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "color_override": null, "scryfall_oracle_id": null, "legalities": {}
            }),
        );
        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("Eleventh Doctor")
                .map(|face| face.name.as_str()),
            Some("The Eleventh Doctor")
        );
        assert_eq!(
            db.get_face_by_name("Seance Doctor")
                .map(|face| face.name.as_str()),
            Some("The Séance Doctor")
        );
    }

    #[test]
    fn combined_face_name_lookup_resolves_unaccented_front_alias() {
        let mut map = HashMap::new();
        map.insert("séance board".to_string(), test_face("Séance Board"));
        map.insert("planchette".to_string(), test_face("Planchette"));
        let json = serde_json::to_string(&map).unwrap();

        let db = CardDatabase::from_json_str(&json).unwrap();

        assert_eq!(
            db.get_face_by_name("Seance Board // Planchette")
                .map(|face| face.name.as_str()),
            Some("Séance Board")
        );
    }

    #[test]
    fn bracket_signals_lookup_returns_default_when_no_lists_loaded() {
        let db = CardDatabase::default();
        let sig = db.bracket_signals_for("Demonic Tutor");
        assert!(
            sig.is_clean(),
            "default DB has no bracket lists → all signals false"
        );
    }

    #[test]
    fn bracket_signals_lookup_uses_loaded_lists() {
        use crate::database::bracket_lists::BracketLists;
        let lists = BracketLists::from_json_str(
            r#"{ "version":"t", "efficient_tutors":["Demonic Tutor"] }"#,
        )
        .unwrap();
        let db = CardDatabase::default().with_bracket_lists(lists);
        let sig = db.bracket_signals_for("Demonic Tutor");
        assert!(sig.efficient_tutor);
    }

    #[test]
    fn bracket_signals_for_partner_pair_aggregates_face_signals() {
        use crate::database::bracket_lists::BracketLists;
        // Build a database where only the front face is in the export map,
        // marked as a game changer. The back face (Alena) has no signals.
        let json = r#"{
            "halana, kessig ranger": {
                "name": "Halana, Kessig Ranger",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            },
            "alena, trapper founder": {
                "name": "Alena, Trapper Founder",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": false, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let db = CardDatabase::from_json_str(json)
            .unwrap()
            .with_bracket_lists(BracketLists::default());

        // Single-face lookup still works.
        assert!(db.bracket_signals_for("Halana, Kessig Ranger").game_changer);

        // Partner-pair combined name must aggregate across both faces.
        let sig = db.bracket_signals_for("Halana, Kessig Ranger // Alena, Trapper Founder");
        assert!(
            sig.game_changer,
            "partner-pair name must resolve to either face's signals"
        );
    }

    #[test]
    fn bracket_signals_for_partner_pair_picks_up_back_face_only_signal() {
        // Regression: lookup_key("A // B") collapses to the front face's key,
        // so a back-face-only signal must be picked up by the pre-split
        // aggregation, not the single-face fast path.
        let json = r#"{
            "halana, kessig ranger": {
                "name": "Halana, Kessig Ranger",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": false, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            },
            "alena, trapper founder": {
                "name": "Alena, Trapper Founder",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let db = CardDatabase::from_json_str(json).unwrap();
        let sig = db.bracket_signals_for("Halana, Kessig Ranger // Alena, Trapper Founder");
        assert!(
            sig.game_changer,
            "back-face partner signal must survive lookup_key's front-face collapse"
        );
    }

    #[test]
    fn bracket_signals_for_glued_combined_name_picks_up_back_face_signal() {
        // Regression: the pre-split accepted only the spaced " // " form, so a
        // hand-typed glued composite name ("Front//Back") fell through to the
        // single-face fast path, where lookup_key collapses it to the front
        // face — silently dropping a back-face Game Changer signal from
        // Commander bracket classification. The split must accept every
        // composite form lookup_key resolves.
        let json = r#"{
            "halana, kessig ranger": {
                "name": "Halana, Kessig Ranger",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": false, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            },
            "alena, trapper founder": {
                "name": "Alena, Trapper Founder",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let db = CardDatabase::from_json_str(json).unwrap();
        assert!(
            db.bracket_signals_for("Halana, Kessig Ranger//Alena, Trapper Founder")
                .game_changer,
            "glued composite name must aggregate both faces, like the spaced form"
        );
    }

    #[test]
    fn bracket_signals_for_single_face_name_containing_double_slash_is_not_split() {
        // The false-positive guard (issue #4790) applied to bracket signals:
        // "SP//dr, Piloted by Peni" is ONE indexed card whose printed name
        // contains "//". Splitting it would look up two nonexistent faces and
        // report all-false, losing its real signal.
        let json = r#"{
            "sp//dr, piloted by peni": {
                "name": "SP//dr, Piloted by Peni",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let db = CardDatabase::from_json_str(json).unwrap();
        assert!(
            db.bracket_signals_for("SP//dr, Piloted by Peni")
                .game_changer,
            "an indexed whole name containing // must not be split into faces"
        );
    }

    #[test]
    fn bracket_signals_for_glued_single_face_name_known_only_to_bracket_lists_is_not_split() {
        use crate::database::bracket_lists::BracketLists;
        // Glued-form twin of the spaced-form fallback test below, for the
        // false-positive guard: "SP//dr, Piloted by Peni" is ONE printed card
        // whose name contains "//", and here it is known only to the curated
        // lists (empty export map). The whole-name guard must consult
        // `bracket_lists` too, or the name is split into two nonexistent faces
        // and its real mass-land-denial signal is lost.
        let lists = BracketLists::from_json_str(
            r#"{"version":"t","mass_land_denial":["SP//dr, Piloted by Peni"]}"#,
        )
        .unwrap();
        let db = CardDatabase::default().with_bracket_lists(lists);
        assert!(
            db.bracket_signals_for("SP//dr, Piloted by Peni")
                .mass_land_denial,
            "a lists-only whole name containing // must not be split into faces"
        );
    }

    #[test]
    fn bracket_signals_for_partner_pair_falls_back_to_bracket_lists_when_not_in_export() {
        use crate::database::bracket_lists::BracketLists;
        // No export entries — bracket_lists is the source of truth.
        let lists = BracketLists::from_json_str(
            r#"{"version":"t","efficient_tutors":["Halana, Kessig Ranger"]}"#,
        )
        .unwrap();
        let db = CardDatabase::default().with_bracket_lists(lists);
        let sig = db.bracket_signals_for("Halana, Kessig Ranger // Alena, Trapper Founder");
        assert!(
            sig.efficient_tutor,
            "falls back to bracket_lists for partner pair when export map is empty"
        );
    }

    #[test]
    fn creature_type_vocabulary_unions_subtypes_across_creature_faces() {
        // CR 205.3m: vocabulary must include subtypes from every creature
        // face — including "token-only" types like Saproling (#1471) and
        // types whose cards may not be in any loaded deck like Golem (#1472).
        // Non-creature faces (Lightning Bolt) must not contribute.
        let mut map = serde_json::Map::new();
        for (key, name, types, subs) in [
            (
                "saproling token",
                "Saproling Token",
                &["Creature"][..],
                &["Saproling"][..],
            ),
            (
                "walking golem",
                "Walking Golem",
                &["Artifact", "Creature"][..],
                &["Golem"][..],
            ),
            (
                "grizzly bears",
                "Grizzly Bears",
                &["Creature"][..],
                &["Bear"][..],
            ),
            (
                "lightning bolt",
                "Lightning Bolt",
                &["Instant"][..],
                &[][..],
            ),
            // Duplicate subtype across faces must dedupe.
            (
                "polar bears",
                "Polar Bears",
                &["Creature"][..],
                &["Bear"][..],
            ),
        ] {
            map.insert(
                key.to_string(),
                serde_json::json!({
                    "name": name,
                    "mana_cost": { "type": "NoCost" },
                    "card_type": {
                        "supertypes": [],
                        "core_types": types,
                        "subtypes": subs,
                    },
                    "power": null, "toughness": null, "loyalty": null, "defense": null,
                    "oracle_text": null, "abilities": [], "triggers": [],
                    "static_abilities": [], "replacements": [], "keywords": [],
                }),
            );
        }
        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();
        let vocab = db.creature_type_vocabulary();

        assert!(
            vocab.contains(&"Saproling".to_string()),
            "Saproling must appear (token-only creature type)"
        );
        assert!(
            vocab.contains(&"Golem".to_string()),
            "Golem must appear (multi-core-type creature)"
        );
        assert!(vocab.contains(&"Bear".to_string()));
        // Sorted.
        let mut sorted = vocab.to_vec();
        sorted.sort();
        assert_eq!(vocab.to_vec(), sorted, "vocabulary must be sorted");
        // Deduped: "Bear" appears on two faces but only once in the vocab.
        let bear_count = vocab.iter().filter(|s| *s == "Bear").count();
        assert_eq!(bear_count, 1, "duplicate subtypes must dedupe");
    }

    #[test]
    fn creature_type_vocabulary_includes_kindred_and_tribal_only_faces() {
        // CR 205.3m + CR 308.1: kindred (and legacy tribal) cards share the
        // creature subtype list. A face whose only qualifying core type is
        // Kindred or Tribal (e.g. "Tribal Enchantment — Faerie", "Kindred
        // Sorcery — Elf") must still contribute its subtype to the vocabulary,
        // even though no Creature core type is present.
        let mut map = serde_json::Map::new();
        // Legacy Tribal-only face (Bitterblossom-shaped: Tribal Enchantment — Faerie).
        map.insert(
            "fae enchantment".to_string(),
            serde_json::json!({
                "name": "Fae Enchantment",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": [],
                    "core_types": ["Tribal", "Enchantment"],
                    "subtypes": ["Faerie"],
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
            }),
        );
        // Kindred-only face (current-rules shape: Kindred Sorcery — Elf).
        map.insert(
            "elf rite".to_string(),
            serde_json::json!({
                "name": "Elf Rite",
                "mana_cost": { "type": "NoCost" },
                "card_type": {
                    "supertypes": [],
                    "core_types": ["Kindred", "Sorcery"],
                    "subtypes": ["Elf"],
                },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
            }),
        );
        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();
        let vocab = db.creature_type_vocabulary();
        assert!(
            vocab.contains(&"Faerie".to_string()),
            "Faerie must appear from a Tribal-only face (no Creature core type)"
        );
        assert!(
            vocab.contains(&"Elf".to_string()),
            "Elf must appear from a Kindred-only face (no Creature core type)"
        );
    }

    #[test]
    fn creature_type_vocabulary_excludes_non_creature_subtypes_on_mixed_faces() {
        // CR 205.2b/205.3: subtype categories are disjoint. The hard case is a
        // MULTI-type creature face whose flat MTGJSON subtypes array mixes a
        // creature type with a non-creature one: "Land Creature — Forest Dryad"
        // (Forest is a land type) and "Artifact Creature — Equipment Construct"
        // (Equipment is an artifact type). Because those non-creature types also
        // appear on pure non-creature faces (basic Forest, an Equipment
        // artifact), the corpus subtraction must drop them and keep only the
        // genuine creature types (Dryad, Construct). Gating on the *face*'s core
        // type alone (the pre-fix behavior) leaks Forest/Equipment into the
        // creature vocabulary and corrupts Changeling / Coat of Arms / Morophon.
        let mut map = serde_json::Map::new();
        for (key, name, types, subs) in [
            (
                "dryad arbor",
                "Dryad Arbor",
                &["Land", "Creature"][..],
                &["Forest", "Dryad"][..],
            ),
            ("forest", "Forest", &["Land"][..], &["Forest"][..]),
            (
                "equip construct",
                "Walking Toolbox",
                &["Artifact", "Creature"][..],
                &["Equipment", "Construct"][..],
            ),
            (
                "swiftfoot boots",
                "Swiftfoot Boots",
                &["Artifact"][..],
                &["Equipment"][..],
            ),
        ] {
            map.insert(
                key.to_string(),
                serde_json::json!({
                    "name": name,
                    "mana_cost": { "type": "NoCost" },
                    "card_type": {
                        "supertypes": [],
                        "core_types": types,
                        "subtypes": subs,
                    },
                    "power": null, "toughness": null, "loyalty": null, "defense": null,
                    "oracle_text": null, "abilities": [], "triggers": [],
                    "static_abilities": [], "replacements": [], "keywords": [],
                }),
            );
        }
        let json = serde_json::Value::Object(map).to_string();
        let db = CardDatabase::from_json_str(&json).unwrap();
        let vocab = db.creature_type_vocabulary();
        assert!(
            vocab.contains(&"Dryad".to_string()),
            "Dryad is a creature type and must survive, got {vocab:?}"
        );
        assert!(
            vocab.contains(&"Construct".to_string()),
            "Construct is a creature type and must survive, got {vocab:?}"
        );
        assert!(
            !vocab.contains(&"Forest".to_string()),
            "Forest is a land type (appears on a pure Land face) — must not leak, got {vocab:?}"
        );
        assert!(
            !vocab.contains(&"Equipment".to_string()),
            "Equipment is an artifact type (appears on a pure Artifact face) — must not leak, got {vocab:?}"
        );
    }

    #[test]
    fn single_face_name_containing_slash_resolves_to_itself() {
        let mut map = HashMap::new();
        map.insert(
            "summon: choco/mog".to_string(),
            test_face("Summon: Choco/Mog"),
        );
        map.insert("summon: choco".to_string(), test_face("Summon: Choco"));
        let json = serde_json::to_string(&map).unwrap();
        let db = CardDatabase::from_json_str(&json).unwrap();
        assert_eq!(
            db.get_face_by_name("Summon: Choco/Mog")
                .map(|face| face.name.as_str()),
            Some("Summon: Choco/Mog")
        );
    }

    #[test]
    fn single_slash_combined_face_name_resolves_front_face() {
        let mut map = HashMap::new();
        map.insert("revival".to_string(), test_face("Revival"));
        map.insert("revenge".to_string(), test_face("Revenge"));
        map.insert("who".to_string(), test_face("Who"));
        map.insert("what".to_string(), test_face("What"));
        let json = serde_json::to_string(&map).unwrap();
        let db = CardDatabase::from_json_str(&json).unwrap();
        assert_eq!(
            db.get_face_by_name("Revival/Revenge")
                .map(|face| face.name.as_str()),
            Some("Revival")
        );
        assert_eq!(
            db.get_face_by_name("Who / What / When / Where / Why")
                .map(|face| face.name.as_str()),
            Some("Who")
        );
        assert_eq!(
            db.get_face_by_name("Who/What/When/Where/Why")
                .map(|face| face.name.as_str()),
            Some("Who")
        );
        // Reach guard: "Revenge" must still resolve to itself.
        assert_eq!(
            db.get_face_by_name("Revenge")
                .map(|face| face.name.as_str()),
            Some("Revenge")
        );
    }

    #[test]
    fn spaced_spelling_of_a_slash_name_resolves_to_the_printed_name() {
        let mut map = HashMap::new();
        map.insert(
            "summon: choco/mog".to_string(),
            test_face("Summon: Choco/Mog"),
        );
        let json = serde_json::to_string(&map).unwrap();
        let db = CardDatabase::from_json_str(&json).unwrap();
        assert_eq!(
            db.get_face_by_name("Summon: Choco // Mog")
                .map(|face| face.name.as_str()),
            Some("Summon: Choco/Mog")
        );
        assert_eq!(
            db.get_face_by_name("summon: choco // mog")
                .map(|face| face.name.as_str()),
            Some("Summon: Choco/Mog")
        );
        assert!(
            db.get_face_by_name("Summon: Choco").is_none(),
            "the front segment alone is not the alias's printed name"
        );
    }

    #[test]
    fn spaced_slash_aliases_skip_ambiguous_folds() {
        let mut map = HashMap::new();
        map.insert("x/y".to_string(), test_face("X/Y"));
        map.insert("x / y".to_string(), test_face("X / Y"));
        let json = serde_json::to_string(&map).unwrap();
        let db = CardDatabase::from_json_str(&json).unwrap();
        assert_eq!(
            db.get_face_by_name("X/Y").map(|face| face.name.as_str()),
            Some("X/Y")
        );
        assert_eq!(
            db.get_face_by_name("X / Y").map(|face| face.name.as_str()),
            Some("X / Y")
        );
        assert!(
            db.get_face_by_name("X // Y").is_none(),
            "two distinct keys fold to the same spaced alias, so it must not resolve to either"
        );
    }

    #[test]
    fn bracket_signals_for_single_slash_combined_name_picks_up_back_face_signal() {
        let json = r#"{
            "halana, kessig ranger": {
                "name": "Halana, Kessig Ranger",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": false, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            },
            "alena, trapper founder": {
                "name": "Alena, Trapper Founder",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let db = CardDatabase::from_json_str(json).unwrap();
        assert!(
            db.bracket_signals_for("Halana, Kessig Ranger/Alena, Trapper Founder")
                .game_changer,
            "single-slash composite name must aggregate both faces, like the spaced and glued forms"
        );
    }

    #[test]
    fn bracket_signals_for_single_face_name_containing_slash_is_not_split() {
        let json = r#"{
            "summon: choco/mog": {
                "name": "Summon: Choco/Mog",
                "mana_cost": { "type": "NoCost" },
                "card_type": { "supertypes": [], "core_types": ["Creature"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": null, "abilities": [], "triggers": [],
                "static_abilities": [], "replacements": [], "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let db = CardDatabase::from_json_str(json).unwrap();
        assert!(
            db.bracket_signals_for("Summon: Choco/Mog").game_changer,
            "a single-face name containing a bare slash must not be split"
        );
        assert!(
            db.bracket_signals_for("Summon: Choco // Mog").game_changer,
            "the alias-registered spaced spelling must resolve to the same whole name"
        );
    }

    /// Builds a card-data export JSON string from `(key, name, oracle_id, face_index)`
    /// rows, setting `scryfall_oracle_id` and `face_index` on each face (both are
    /// `CardExportEntry` fields; `face` is `#[serde(flatten)]`).
    fn export_json(entries: &[(&str, &str, Option<&str>, Option<usize>)]) -> String {
        let mut map = serde_json::Map::new();
        for (key, name, oracle_id, face_index) in entries {
            let mut face = serde_json::to_value(test_face(name)).unwrap();
            if let Some(oracle_id) = oracle_id {
                face["scryfall_oracle_id"] = serde_json::json!(oracle_id);
            }
            if let Some(face_index) = face_index {
                face["face_index"] = serde_json::json!(face_index);
            }
            map.insert(key.to_string(), face);
        }
        serde_json::Value::Object(map).to_string()
    }

    fn multi_face_db() -> CardDatabase {
        CardDatabase::from_json_str(&export_json(&[
            ("revival", "Revival", Some("o-rr"), Some(0)),
            ("revenge", "Revenge", Some("o-rr"), Some(1)),
            (
                "delver of secrets",
                "Delver of Secrets",
                Some("o-dv"),
                Some(0),
            ),
            (
                "insectile aberration",
                "Insectile Aberration",
                Some("o-dv"),
                Some(1),
            ),
            ("summon: choco/mog", "Summon: Choco/Mog", Some("o-cm"), None),
            ("lightning bolt", "Lightning Bolt", Some("o-lb"), None),
            ("lim-dûl's vault", "Lim-Dûl's Vault", Some("o-ld"), None),
            (
                "the eleventh doctor",
                "The Eleventh Doctor",
                Some("o-ed"),
                None,
            ),
            (
                "sp//dr, piloted by peni",
                "SP//dr, Piloted by Peni",
                Some("o-sp"),
                None,
            ),
            (
                "halana, kessig ranger",
                "Halana, Kessig Ranger",
                Some("o-ha"),
                None,
            ),
            (
                "alena, trapper founder",
                "Alena, Trapper Founder",
                Some("o-al"),
                None,
            ),
        ]))
        .unwrap()
    }

    #[test]
    fn canonical_name_returns_the_printed_name_of_a_whole_name() {
        let db = multi_face_db();
        assert_eq!(
            db.canonical_name("Summon: Choco/Mog"),
            Some("Summon: Choco/Mog".to_string())
        );
        assert_eq!(
            db.canonical_name("Summon: Choco // Mog"),
            Some("Summon: Choco/Mog".to_string())
        );
        assert_eq!(
            db.canonical_name("lightning bolt"),
            Some("Lightning Bolt".to_string())
        );
        assert_eq!(
            db.canonical_name("Lim-Dul's Vault"),
            Some("Lim-Dûl's Vault".to_string())
        );
        assert_eq!(
            db.canonical_name("Eleventh Doctor"),
            Some("The Eleventh Doctor".to_string())
        );
        assert_eq!(
            db.canonical_name("sp//dr, piloted by peni"),
            Some("SP//dr, Piloted by Peni".to_string())
        );
        assert_eq!(db.canonical_name("Summon: Choco"), None);
        assert_eq!(db.canonical_name("Not A Card"), None);
    }

    #[test]
    fn canonical_name_joins_every_face_of_a_composite_spelling() {
        let db = multi_face_db();
        for input in [
            "Revival/Revenge",
            "revival // revenge",
            "Revival//Revenge",
            "Revival // Revenge",
        ] {
            assert_eq!(
                db.canonical_name(input),
                Some("Revival // Revenge".to_string()),
                "input {input:?} must canonicalize to the composite"
            );
        }
        assert_eq!(
            db.canonical_name("Delver of Secrets/Insectile Aberration"),
            Some("Delver of Secrets // Insectile Aberration".to_string())
        );
    }

    #[test]
    fn canonical_name_keeps_a_single_face_spelling_of_a_multi_face_card() {
        let db = multi_face_db();
        assert_eq!(db.canonical_name("Revival"), Some("Revival".to_string()));
        assert_eq!(db.canonical_name("revenge"), Some("Revenge".to_string()));
        assert_eq!(
            db.canonical_name("delver of secrets"),
            Some("Delver of Secrets".to_string())
        );
        assert_eq!(
            db.canonical_name("Insectile Aberration"),
            Some("Insectile Aberration".to_string())
        );
    }

    #[test]
    fn canonical_name_refuses_a_composite_that_is_not_one_cards_faces() {
        let db = multi_face_db();
        for input in [
            "Halana, Kessig Ranger // Alena, Trapper Founder",
            "Revenge // Revival",
            "Revival // Delver of Secrets",
            "Revival // Revenge // Delver of Secrets",
        ] {
            assert!(
                db.get_face_by_name(input).is_some(),
                "reach guard: {input:?} must still resolve through lookup_key"
            );
            assert_eq!(
                db.canonical_name(input),
                None,
                "input {input:?} does not name one card's faces"
            );
        }
    }

    #[test]
    fn faces_of_card_falls_back_to_the_face_itself() {
        let db = multi_face_db();
        let revival = db.face_index.get("revival").unwrap();
        let mut names: Vec<&str> = db
            .faces_of_card(revival)
            .into_iter()
            .map(|f| f.name.as_str())
            .collect();
        names.sort();
        assert_eq!(names, vec!["Revenge", "Revival"]);

        let lonely = test_face("Lonely");
        let faces = db.faces_of_card(&lonely);
        assert_eq!(faces.len(), 1);
        assert!(std::ptr::eq(faces[0], &lonely));

        let mut orphan = test_face("Orphan");
        orphan.scryfall_oracle_id = Some("not-in-index".to_string());
        let faces = db.faces_of_card(&orphan);
        assert_eq!(faces.len(), 1);
        assert!(std::ptr::eq(faces[0], &orphan));
    }

    #[test]
    fn canonical_name_keeps_the_card_its_input_resolves_to() {
        let db = CardDatabase::from_json_str(&export_json(&[
            ("fire", "Fire", Some("o-fi"), Some(0)),
            ("ice", "Ice", Some("o-fi"), Some(1)),
            ("fire [o-sf]", "Fire", Some("o-sf"), Some(1)),
            ("start [o-sf]", "Start", Some("o-sf"), Some(0)),
        ]))
        .unwrap();
        // Reach guard: the hidden key resolves through get_face_by_name.
        assert_eq!(
            db.get_face_by_name("fire [o-sf]").map(|f| f.name.as_str()),
            Some("Fire")
        );
        assert_eq!(db.canonical_name("fire [o-sf]"), None);
        assert_eq!(db.canonical_name("Fire"), Some("Fire".to_string()));
    }

    #[test]
    fn canonical_name_is_a_fixed_point_on_every_printed_name() {
        // allow-full-card-db: whole-corpus canonical-name drift guard — must check every printed name
        let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../client/public/card-data.json");
        if !path.exists() {
            eprintln!(
                "SKIP canonical_name_is_a_fixed_point_on_every_printed_name: card-data.json missing"
            );
            return;
        }
        let db = CardDatabase::from_export(&path).expect("card-data export should load");

        let mut checked_faces = 0;
        for (_, face) in db.face_iter() {
            assert_eq!(
                db.canonical_name(&face.name),
                Some(face.name.clone()),
                "every printed face name must be a fixed point of canonical_name"
            );
            checked_faces += 1;
        }
        assert!(checked_faces > 0, "reach guard: the export must load faces");

        let mut checked_groups_with_some = 0;
        let mut current_oracle_id: Option<&str> = None;
        let mut group: Vec<&CardFace> = Vec::new();
        let mut groups: Vec<Vec<&CardFace>> = Vec::new();
        for face in db.faces_in_scan_order() {
            match (current_oracle_id, face.scryfall_oracle_id.as_deref()) {
                (Some(a), Some(b)) if a == b => group.push(face),
                (_, Some(b)) => {
                    if group.len() >= 2 {
                        groups.push(std::mem::take(&mut group));
                    } else {
                        group.clear();
                    }
                    group.push(face);
                    current_oracle_id = Some(b);
                }
                (_, None) => {
                    if group.len() >= 2 {
                        groups.push(std::mem::take(&mut group));
                    } else {
                        group.clear();
                    }
                    current_oracle_id = None;
                }
            }
        }
        if group.len() >= 2 {
            groups.push(group);
        }

        for group in &groups {
            let names: Vec<&str> = group.iter().map(|f| f.name.as_str()).collect();
            let composite = names.join(" // ");
            match db.canonical_name(&composite) {
                None => {}
                Some(resolved) => {
                    assert_eq!(
                        resolved, composite,
                        "a composite of a group's own faces must canonicalize to itself or None"
                    );
                    checked_groups_with_some += 1;
                    let slash_spelling = names.join("/");
                    assert_eq!(
                        db.canonical_name(&slash_spelling),
                        Some(composite.clone()),
                        "the single-slash spelling of a fixed composite must canonicalize the same way"
                    );
                    assert_eq!(
                        db.lookup_key(&slash_spelling),
                        db.lookup_key(&composite),
                        "lookup_key must agree on both spellings"
                    );
                }
            }
        }
        assert!(
            checked_groups_with_some > 0,
            "reach guard: at least one multi-face group must canonicalize to its composite"
        );
    }

    #[test]
    fn from_json_merges_card_signals_with_list_signals() {
        use crate::database::bracket_lists::BracketLists;

        let json = r#"{
            "demonic tutor": {
                "name": "Demonic Tutor",
                "mana_cost": { "type": "Cost", "shards": [], "generic": 1 },
                "card_type": { "supertypes": [], "core_types": ["Sorcery"], "subtypes": [] },
                "power": null, "toughness": null, "loyalty": null, "defense": null,
                "oracle_text": "Search your library...",
                "abilities": [], "triggers": [], "static_abilities": [], "replacements": [],
                "keywords": [],
                "bracket_signals": {
                    "game_changer": true, "mass_land_denial": false,
                    "extra_turn": false, "efficient_tutor": false
                }
            }
        }"#;
        let lists =
            BracketLists::from_json_str(r#"{"version":"t","efficient_tutors":["Demonic Tutor"]}"#)
                .unwrap();
        let db = CardDatabase::from_json_str(json)
            .unwrap()
            .with_bracket_lists(lists);
        let sig = db.bracket_signals_for("demonic tutor");
        assert!(sig.efficient_tutor);
        assert!(sig.game_changer);
    }
}

#[cfg(test)]
pub(crate) const SOURCE_CHOICE_TWELVE_ATOMIC: &str = r###"{
  "meta": {
    "date": "2026-10-04",
    "version": "5.3.0+20261004"
  },
  "data": {
    "Circle of Protection: Black": [
      {
        "name": "Circle of Protection: Black",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{1}: The next time a black source of your choice would deal damage to you this turn, prevent that damage.",
        "identifiers": {
          "scryfallOracleId": "7a5a8414-4da4-4dd0-93ae-210d50f4d6f6"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn’t need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "2ED",
          "30A",
          "3ED",
          "4BB",
          "4ED",
          "5ED",
          "6ED",
          "7ED",
          "8ED",
          "9ED",
          "CED",
          "CEI",
          "FBB",
          "ICE",
          "ITP",
          "LEB",
          "PTC",
          "RQS",
          "SUM",
          "TMP",
          "WC00",
          "WC97"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "modern": "Legal",
          "oathbreaker": "Legal",
          "oldschool": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Circle of Protection: Blue": [
      {
        "name": "Circle of Protection: Blue",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{1}: The next time a blue source of your choice would deal damage to you this turn, prevent that damage.",
        "identifiers": {
          "scryfallOracleId": "d7572f17-f85d-45c0-ac64-43aac760eafe"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn’t need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "2ED",
          "30A",
          "3ED",
          "4BB",
          "4ED",
          "5ED",
          "6ED",
          "7ED",
          "8ED",
          "CED",
          "CEI",
          "FBB",
          "ICE",
          "LEA",
          "LEB",
          "SUM",
          "TMP"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "modern": "Legal",
          "oathbreaker": "Legal",
          "oldschool": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Circle of Protection: Green": [
      {
        "name": "Circle of Protection: Green",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{1}: The next time a green source of your choice would deal damage to you this turn, prevent that damage.",
        "identifiers": {
          "scryfallOracleId": "41b0f347-1398-4778-bf3f-4007d8a77162"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn’t need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "2ED",
          "30A",
          "3ED",
          "4BB",
          "4ED",
          "5ED",
          "6ED",
          "7ED",
          "8ED",
          "CED",
          "CEI",
          "FBB",
          "ICE",
          "LEA",
          "LEB",
          "PTC",
          "SUM",
          "TMP"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "modern": "Legal",
          "oathbreaker": "Legal",
          "oldschool": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Circle of Protection: Red": [
      {
        "name": "Circle of Protection: Red",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{1}: The next time a red source of your choice would deal damage to you this turn, prevent that damage.",
        "identifiers": {
          "scryfallOracleId": "df2738fe-9cd1-4347-8808-105fcfde1190"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn’t need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "2ED",
          "30A",
          "3ED",
          "4BB",
          "4ED",
          "5ED",
          "6ED",
          "7ED",
          "8ED",
          "9ED",
          "CED",
          "CEI",
          "F05",
          "FBB",
          "ICE",
          "ITP",
          "LEA",
          "LEB",
          "PRM",
          "PTC",
          "RQS",
          "SUM",
          "TMP",
          "WC04",
          "WC97"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "modern": "Legal",
          "oathbreaker": "Legal",
          "oldschool": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Circle of Protection: White": [
      {
        "name": "Circle of Protection: White",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{1}: The next time a white source of your choice would deal damage to you this turn, prevent that damage.",
        "identifiers": {
          "scryfallOracleId": "5f46f86a-9779-4ed3-99cb-76a03d380598"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn’t need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "2ED",
          "30A",
          "3ED",
          "4BB",
          "4ED",
          "5ED",
          "6ED",
          "7ED",
          "8ED",
          "CED",
          "CEI",
          "FBB",
          "ICE",
          "LEA",
          "LEB",
          "SUM",
          "TMP"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "modern": "Legal",
          "oathbreaker": "Legal",
          "oldschool": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Rune of Protection: Black": [
      {
        "name": "Rune of Protection: Black",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{W}: The next time a black source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)",
        "keywords": [
          "Cycling"
        ],
        "identifiers": {
          "scryfallOracleId": "b027302f-80c2-441d-a9fd-bbe64bf5cf8a"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn't need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "USG"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Rune of Protection: Blue": [
      {
        "name": "Rune of Protection: Blue",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{W}: The next time a blue source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)",
        "keywords": [
          "Cycling"
        ],
        "identifiers": {
          "scryfallOracleId": "092dd5be-da62-4198-b7fa-0813f075047c"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn't need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "USG"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Rune of Protection: Green": [
      {
        "name": "Rune of Protection: Green",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{W}: The next time a green source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)",
        "keywords": [
          "Cycling"
        ],
        "identifiers": {
          "scryfallOracleId": "c7fca3f3-b48e-4ac5-be31-ce7ae3c7432c"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn't need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "USG"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Rune of Protection: Red": [
      {
        "name": "Rune of Protection: Red",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{W}: The next time a red source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)",
        "keywords": [
          "Cycling"
        ],
        "identifiers": {
          "scryfallOracleId": "9b3f6b29-675a-4153-9582-03318851b20e"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn't need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "USG"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Rune of Protection: White": [
      {
        "name": "Rune of Protection: White",
        "manaCost": "{1}{W}",
        "manaValue": 2.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Enchantment",
        "types": [
          "Enchantment"
        ],
        "subtypes": [],
        "supertypes": [],
        "text": "{W}: The next time a white source of your choice would deal damage to you this turn, prevent that damage.\nCycling {2} ({2}, Discard this card: Draw a card.)",
        "keywords": [
          "Cycling"
        ],
        "identifiers": {
          "scryfallOracleId": "6f214a42-e38f-4923-8afc-832efdde7975"
        },
        "rulings": [
          {
            "date": "2004-10-04",
            "text": "A source of damage is a permanent, a spell on the stack (including one that creates a permanent), or any object referred to by an object on the stack. A source doesn't need to be capable of dealing damage to be a legal choice."
          },
          {
            "date": "2004-10-04",
            "text": "Can be used even when there is no damage to prevent. It prevents the next damage (if any) from the source this turn."
          }
        ],
        "printings": [
          "USG"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Pilgrim of Justice": [
      {
        "name": "Pilgrim of Justice",
        "manaCost": "{2}{W}",
        "manaValue": 3.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Creature — Human Cleric",
        "types": [
          "Creature"
        ],
        "subtypes": [
          "Human",
          "Cleric"
        ],
        "supertypes": [],
        "power": "1",
        "toughness": "3",
        "text": "Protection from red\n{W}, Sacrifice this creature: The next time a red source of your choice would deal damage this turn, prevent that damage.",
        "keywords": [
          "Protection"
        ],
        "identifiers": {
          "scryfallOracleId": "959b672e-d4e4-49a1-874a-5148f040fadd"
        },
        "printings": [
          "ODY"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "penny": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ],
    "Pilgrim of Virtue": [
      {
        "name": "Pilgrim of Virtue",
        "manaCost": "{2}{W}",
        "manaValue": 3.0,
        "colors": [
          "W"
        ],
        "colorIdentity": [
          "W"
        ],
        "layout": "normal",
        "type": "Creature — Human Cleric",
        "types": [
          "Creature"
        ],
        "subtypes": [
          "Human",
          "Cleric"
        ],
        "supertypes": [],
        "power": "1",
        "toughness": "3",
        "text": "Protection from black\n{W}, Sacrifice this creature: The next time a black source of your choice would deal damage this turn, prevent that damage.",
        "keywords": [
          "Protection"
        ],
        "identifiers": {
          "scryfallOracleId": "e2f73743-95e7-4f03-9b98-5ab5ebc4b724"
        },
        "printings": [
          "ODY"
        ],
        "legalities": {
          "commander": "Legal",
          "duel": "Legal",
          "legacy": "Legal",
          "oathbreaker": "Legal",
          "pauper": "Legal",
          "paupercommander": "Legal",
          "penny": "Legal",
          "predh": "Legal",
          "premodern": "Legal",
          "vintage": "Legal"
        }
      }
    ]
  }
}"###;

#[cfg(test)]
pub(crate) fn source_choice_expected_faces() -> Vec<CardFace> {
    use crate::types::ability::*;
    use crate::types::card_type::CardType;
    use crate::types::keywords::{CyclingCost, Keyword, ProtectionTarget};
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use crate::types::zones::Zone;
    let rows: serde_json::Value = serde_json::from_str(SOURCE_CHOICE_TWELVE_ATOMIC).unwrap();
    let mut faces = Vec::new();
    for (name, rows) in rows["data"].as_object().unwrap() {
        let source = &rows[0];
        let pilgrim = name == "Pilgrim of Justice" || name == "Pilgrim of Virtue";
        let rune = name.starts_with("Rune");
        let color = if name.ends_with("Black") || name.ends_with("Virtue") {
            ManaColor::Black
        } else if name.ends_with("Blue") {
            ManaColor::Blue
        } else if name.ends_with("Green") {
            ManaColor::Green
        } else if name.ends_with("Red") || name.ends_with("Justice") {
            ManaColor::Red
        } else {
            ManaColor::White
        };
        let oracle = source["text"].as_str().unwrap();
        let activation_line = if pilgrim {
            oracle.lines().nth(1).unwrap()
        } else {
            oracle.lines().next().unwrap()
        };
        let activation_cost = if pilgrim {
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::Cost {
                            shards: vec![ManaCostShard::White],
                            generic: 0,
                        },
                    },
                    AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
                ],
            }
        } else {
            AbilityCost::Mana {
                cost: if rune {
                    ManaCost::Cost {
                        shards: vec![ManaCostShard::White],
                        generic: 0,
                    }
                } else {
                    ManaCost::generic(1)
                },
            }
        };
        let prevention = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::PreventDamage {
                lifetime: RedirectionLifetime::OneOpportunity,
                amount: PreventionAmount::All,
                amount_dynamic: None,
                target: if pilgrim {
                    TargetFilter::Any
                } else {
                    TargetFilter::Controller
                },
                recipient_scope: EffectScope::Single,
                scope: PreventionScope::AllDamage,
                damage_source_filter: Some(TargetFilter::ChosenDamageSource {
                    chooser: Some(Chooser::Controller),
                    filter: Some(Box::new(TargetFilter::Typed(
                        TypedFilter::default().properties(vec![FilterProp::HasColor { color }]),
                    ))),
                }),
                prevention_duration: None,
            },
        )
        .cost(activation_cost)
        .description(activation_line.into());
        let mut face = CardFace {
            name: name.clone(),
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: if pilgrim { 2 } else { 1 },
            },
            card_type: CardType {
                supertypes: vec![],
                core_types: vec![if pilgrim {
                    CoreType::Creature
                } else {
                    CoreType::Enchantment
                }],
                subtypes: if pilgrim {
                    vec!["Human".into(), "Cleric".into()]
                } else {
                    vec![]
                },
            },
            power: pilgrim.then_some(PtValue::Fixed(1)),
            toughness: pilgrim.then_some(PtValue::Fixed(3)),
            oracle_text: Some(oracle.into()),
            color_identity: vec![ManaColor::White],
            scryfall_oracle_id: Some(
                source["identifiers"]["scryfallOracleId"]
                    .as_str()
                    .unwrap()
                    .into(),
            ),
            abilities: vec![prevention],
            ..CardFace::default()
        };
        if pilgrim {
            face.keywords
                .push(Keyword::Protection(ProtectionTarget::Color(color)));
        }
        if rune {
            face.keywords
                .push(Keyword::Cycling(CyclingCost::Mana(ManaCost::generic(2))));
            let mut cycling = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana {
                        cost: ManaCost::generic(2),
                    },
                    AbilityCost::Discard {
                        count: QuantityExpr::Fixed { value: 1 },
                        filter: None,
                        selection: CardSelectionMode::Chosen,
                        self_scope: DiscardSelfScope::SourceCard,
                    },
                ],
            });
            cycling.activation_zone = Some(Zone::Hand);
            cycling.ability_tag = Some(AbilityTag::Cycling);
            face.abilities.push(cycling);
        }
        faces.push(face);
    }
    faces
}

#[cfg(test)]
mod complete_source_choice_publication_tests {
    use super::*;

    fn source_program_face() -> CardFace {
        use crate::types::ability::*;
        let prevention = |chooser| Effect::PreventDamage {
            lifetime: RedirectionLifetime::OneOpportunity,
            amount: PreventionAmount::All,
            amount_dynamic: None,
            target: TargetFilter::Controller,
            recipient_scope: EffectScope::Single,
            scope: PreventionScope::AllDamage,
            damage_source_filter: Some(TargetFilter::ChosenDamageSource {
                chooser,
                filter: None,
            }),
            prevention_duration: None,
        };
        // Deliberately typed composition: the first prevention is an actual
        // same-resolution reader; the registered substitute is a later producer.
        let mut register = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::CreateDrawReplacement {
                replacement_effect: Box::new(prevention(Some(Chooser::Controller))),
            },
        );
        register.sub_link = SubAbilityLink::SequentialSibling;
        let reader =
            AbilityDefinition::new(AbilityKind::Spell, prevention(None)).sub_ability(register);
        let choose = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChooseDamageSource {
                source_filter: TargetFilter::Any,
            },
        )
        .cost(AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::generic(1),
        })
        .sub_ability(reader);
        let draw = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::generic(1),
        });
        CardFace {
            name: "Typed source ownership fixture".into(),
            card_type: crate::types::card_type::CardType {
                core_types: vec![CoreType::Artifact],
                ..Default::default()
            },
            abilities: vec![choose, draw],
            ..CardFace::default()
        }
    }

    fn omit_source_choosers(value: &mut serde_json::Value, unit: bool) {
        match value {
            serde_json::Value::Object(object) => {
                if object.get("type").and_then(serde_json::Value::as_str)
                    == Some("ChosenDamageSource")
                {
                    if unit {
                        *value = serde_json::json!("ChosenDamageSource");
                        return;
                    }
                    object.remove("chooser");
                }
                for child in object.values_mut() {
                    omit_source_choosers(child, unit);
                }
            }
            serde_json::Value::Array(values) => {
                for child in values {
                    omit_source_choosers(child, unit);
                }
            }
            _ => {}
        }
    }

    fn restore_legacy_source_program(
        state: &crate::types::game_state::GameState,
        wire: bool,
    ) -> crate::types::game_state::GameState {
        use crate::types::resolution::ResolutionStateWire;
        let mut raw = if wire {
            serde_json::to_value(ResolutionStateWire::from_game_state(state.clone())).unwrap()
        } else {
            serde_json::to_value(state).unwrap()
        };
        omit_source_choosers(&mut raw, false);
        if wire {
            serde_json::from_value::<ResolutionStateWire>(raw)
                .unwrap()
                .into_game_state()
        } else {
            serde_json::from_value(raw).unwrap()
        }
    }

    // CR 608.2c + CR 614.1: the same-resolution reader follows its producer;
    // a registered replacement's substitute resolves at its own later event.
    fn assert_source_program_public_flow(face: &CardFace, canonical: Option<bool>) {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::ability::*;
        use crate::types::actions::GameAction;
        use crate::types::game_state::WaitingFor;
        use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
        use crate::types::mana::{ManaType, ManaUnit};
        use crate::types::phase::Phase;
        use crate::types::replacements::ReplacementEvent;
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_library_top(P0, &["Library A", "Library B"]);
        let damage = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
                damage_source: None,
                excess: None,
            },
        )
        .cost(AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::generic(0),
        });
        let first = scenario
            .add_creature(P0, "First source", 1, 5)
            .with_ability_definition(damage.clone())
            .id();
        let second = scenario
            .add_creature(P0, "Second source", 1, 5)
            .with_ability_definition(damage)
            .id();
        let driver = scenario
            .add_artifact_from_oracle(P0, &face.name, "")
            .with_ability_definition(face.abilities[0].clone())
            .with_ability_definition(face.abilities[1].clone())
            .id();
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]); 2],
        );
        let mut runner = scenario.build();
        if let Some(wire) = canonical {
            *runner.state_mut() = restore_legacy_source_program(runner.state(), wire);
        }
        runner.activate(driver, 0).resolve();
        let offered = |runner: &crate::game::scenario::GameRunner, id| {
            let WaitingFor::DamageSourceChoice {
                player, options, ..
            } = &runner.state().waiting_for
            else {
                panic!("required source prompt: {:?}", runner.state().waiting_for);
            };
            assert_eq!(*player, P0);
            options
                .iter()
                .find(|option| option.identity.reference.object_id == id)
                .expect("exact offered source")
                .identity
                .reference
        };
        let first_ref: ObjectIncarnationRef = offered(&runner, first);
        runner
            .act(GameAction::ChooseDamageSource { source: first_ref })
            .unwrap();
        runner.advance_until_stack_empty();
        let installed = &runner.state().pending_damage_replacements;
        assert!(
            installed.iter().any(
                |definition| definition.event == ReplacementEvent::DamageDone
                    && definition
                        .selected_damage_source
                        .as_ref()
                        .is_some_and(|source| source.identity.reference == first_ref)
            ),
            "the actual own-resolution reader installs its chosen-source shield"
        );
        assert!(installed.iter().any(
            |definition| definition.event == ReplacementEvent::Draw && !definition.is_consumed
        ));
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(runner.state().resolution_stack.is_empty());
        if let Some(wire) = canonical {
            *runner.state_mut() = restore_legacy_source_program(runner.state(), wire);
        }
        runner.activate(driver, 1).resolve();
        let second_ref = offered(&runner, second);
        assert_ne!(second_ref, first_ref);
        assert!(
            runner
                .state()
                .pending_damage_replacements
                .iter()
                .all(|definition| definition
                    .selected_damage_source
                    .as_ref()
                    .is_none_or(|source| source.identity.reference != second_ref)),
            "the later source is still unanswered"
        );
        runner
            .act(GameAction::ChooseDamageSource { source: second_ref })
            .unwrap();
        runner.advance_until_stack_empty();
        assert!(
            runner
                .state()
                .pending_damage_replacements
                .iter()
                .any(
                    |definition| definition.event == ReplacementEvent::DamageDone
                        && definition
                            .selected_damage_source
                            .as_ref()
                            .is_some_and(|source| source.identity.reference == second_ref)
                ),
            "the later body binds its own answer, never the first choice"
        );
        assert_eq!(
            runner.state().players[0].library.len(),
            2,
            "the reached draw was replaced by the later source-choice body"
        );
        for source in [first, second] {
            let before = runner.life(P0);
            runner.activate(source, 0).target_player(P0).resolve();
            assert_eq!(
                runner.life(P0),
                before,
                "each independently selected source is prevented"
            );
            runner.activate(source, 0).target_player(P0).resolve();
            assert_eq!(
                runner.life(P0),
                before - 1,
                "its next independent opportunity is ordinary damage"
            );
        }
        assert_eq!(runner.life(P0), 18);
        assert!(matches!(
            runner.state().waiting_for,
            WaitingFor::Priority { .. }
        ));
        assert!(
            runner.state().stack.is_empty()
                && runner.state().resolution_stack.is_empty()
                && runner.state().deferred_triggers.is_empty()
        );
        assert!(
            runner.state().pending_replacement.is_none()
                && runner.state().pending_combat_lifelink.is_none()
        );
        assert!(runner.state().owed_spell_final_part().is_none());
    }

    #[test]
    fn normal_complete_string_and_file_loaders_keep_later_fresh_choice_and_own_reader() {
        let expected = source_program_face();
        let current = serde_json::json!({"typed source ownership fixture": CardExportEntry {
            face: expected.clone(), legalities: HashMap::new(), layout: Some("normal".into()), face_index: Some(0), printings: vec![], rulings: vec![], bracket_signals: Default::default(),
        }});
        for old in [None, Some(false), Some(true)] {
            let mut entry = current.clone();
            if let Some(unit) = old {
                omit_source_choosers(&mut entry, unit);
            }
            let complete = serde_json::to_string(&entry).unwrap();
            let file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(file.path(), complete.as_bytes()).unwrap();
            assert_eq!(std::fs::read(file.path()).unwrap(), complete.as_bytes());
            for db in [
                CardDatabase::from_json_str(&complete).unwrap(),
                CardDatabase::from_export(file.path()).unwrap(),
            ] {
                assert_eq!(
                    db.faces_in_scan_order().cloned().collect::<Vec<_>>(),
                    vec![expected.clone()]
                );
                assert_source_program_public_flow(
                    db.get_face_by_name(&expected.name).unwrap(),
                    None,
                );
            }
        }
    }

    #[test]
    fn both_canonical_legacy_readers_keep_later_fresh_choice_and_genuine_own_reader() {
        for wire in [false, true] {
            assert_source_program_public_flow(&source_program_face(), Some(wire));
        }
    }

    #[test]
    fn complete_twelve_faces_and_metadata_survive_identical_current_and_authentic_legacy_normal_loaders(
    ) {
        let input = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(input.path(), SOURCE_CHOICE_TWELVE_ATOMIC.as_bytes()).unwrap();
        let raw = CardDatabase::from_mtgjson(input.path()).unwrap();
        let expected = source_choice_expected_faces();
        assert_eq!(expected.len(), 12);
        assert_eq!(
            raw.faces_in_scan_order().cloned().collect::<Vec<_>>(),
            expected
        );
        let names = expected.iter().map(|face| face.name.clone()).collect();
        let fixture: serde_json::Value = serde_json::from_str(SOURCE_CHOICE_TWELVE_ATOMIC).unwrap();
        // The ordinary MTGJSON library loader returns rules, while native
        // publication also writes these complete AtomicCards metadata fields.
        // export_subset_json intentionally drops that native-only metadata;
        // restore it from the independent pinned fixture for BOTH normal
        // complete export readers, rather than testing an AI-worker subset.
        let mut complete: serde_json::Value =
            serde_json::from_str(&raw.export_subset_json(&names)).unwrap();
        for face in &expected {
            let source = &fixture["data"][&face.name][0];
            let entry = &mut complete[face.name.to_lowercase()];
            entry["legalities"] = source["legalities"].clone();
            entry["printings"] = source["printings"].clone();
            entry["rulings"] = source
                .get("rulings")
                .cloned()
                .unwrap_or_else(|| serde_json::json!([]));
            entry["layout"] = serde_json::json!("normal");
            entry["face_index"] = serde_json::json!(0);
        }
        let current = serde_json::to_string(&complete).unwrap();
        let mut legacy = complete;
        fn omit_legacy_chooser(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(object) => {
                    if object.get("type").and_then(serde_json::Value::as_str)
                        == Some("ChosenDamageSource")
                    {
                        object.remove("chooser");
                    }
                    if object.get("type").and_then(serde_json::Value::as_str)
                        == Some("PreventDamage")
                    {
                        object.remove("lifetime");
                    }
                    object.remove("selected_damage_source");
                    for child in object.values_mut() {
                        omit_legacy_chooser(child);
                    }
                }
                serde_json::Value::Array(array) => {
                    for child in array {
                        omit_legacy_chooser(child);
                    }
                }
                _ => {}
            }
        }
        omit_legacy_chooser(&mut legacy);
        let assert_database = |db: &CardDatabase, expected: &[CardFace]| {
            assert_eq!(db.face_index.len(), 12);
            assert_eq!(
                db.faces_in_scan_order().cloned().collect::<Vec<_>>(),
                expected
            );
            for face in expected {
                assert_eq!(db.get_face_by_name(&face.name), Some(face));
                assert_eq!(
                    db.get_face_by_oracle_id(face.scryfall_oracle_id.as_deref().unwrap()),
                    Some(face)
                );
                assert_eq!(db.face_order_index[&face.name.to_lowercase()], 0);
                assert!(db.is_front_face_key(&face.name.to_lowercase()));
                let source = &fixture["data"][&face.name][0];
                assert_eq!(
                    serde_json::to_value(db.rulings_for(&face.name)).unwrap(),
                    source
                        .get("rulings")
                        .cloned()
                        .unwrap_or_else(|| serde_json::json!([]))
                );
                assert_eq!(
                    serde_json::to_value(db.printings_for(&face.name).unwrap()).unwrap(),
                    source["printings"]
                );
                assert_eq!(
                    db.get_layout_kind(face.scryfall_oracle_id.as_deref().unwrap()),
                    Some(LayoutKind::Single)
                );
                for format in LegalityFormat::ALL {
                    let expected = match source["legalities"]
                        .get(format.as_key())
                        .and_then(serde_json::Value::as_str)
                    {
                        Some("Legal") => Some(LegalityStatus::Legal),
                        Some("Restricted") => Some(LegalityStatus::Restricted),
                        Some("Banned") => Some(LegalityStatus::Banned),
                        None => None,
                        other => panic!("pinned unexpected legality {other:?}"),
                    };
                    assert_eq!(db.legality_status(&face.name, format), expected);
                }
                assert_eq!(
                    face.mana_cost.mana_value(),
                    if face.power.is_some() { 3 } else { 2 }
                );
            }
        };
        let mut old_expected = expected.clone();
        for face in &mut old_expected {
            if let crate::types::ability::Effect::PreventDamage { lifetime, .. } =
                face.abilities[0].effect.as_mut()
            {
                *lifetime = crate::types::ability::RedirectionLifetime::Continuous;
            }
        }
        for (complete, expected) in [
            (current, expected),
            (serde_json::to_string(&legacy).unwrap(), old_expected),
        ] {
            let normal_file = tempfile::NamedTempFile::new().unwrap();
            std::fs::write(normal_file.path(), complete.as_bytes()).unwrap();
            assert_eq!(
                std::fs::read(normal_file.path()).unwrap(),
                complete.as_bytes()
            );
            assert_database(&CardDatabase::from_json_str(&complete).unwrap(), &expected);
            assert_database(
                &CardDatabase::from_export(normal_file.path()).unwrap(),
                &expected,
            );
        }
    }
}

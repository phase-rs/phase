use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::bracket_lists::{BracketLists, BracketSignals};
use super::legality::{normalize_legalities, CardLegalities, LegalityFormat, LegalityStatus};
use super::mtgjson::Ruling;
use crate::types::card::{CardFace, CardRules, LayoutKind, PrintedCardRef};
use crate::types::card_type::CoreType;

use std::io::BufReader;

#[derive(Default)]
pub struct CardDatabase {
    pub(crate) cards: HashMap<String, CardRules>,
    pub(crate) face_index: HashMap<String, CardFace>,
    pub(crate) name_alias_index: HashMap<String, String>,
    pub(crate) oracle_id_index: HashMap<String, Vec<String>>,
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
        let entries: HashMap<String, CardExportEntry> = serde_json::from_reader(reader)?;
        Ok(Self::from_export_entries(entries))
    }

    /// Load from a card-data export JSON string.
    /// Used by the WASM bridge to receive card data from the frontend.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let entries: HashMap<String, CardExportEntry> = serde_json::from_str(json)?;
        Ok(Self::from_export_entries(entries))
    }

    fn from_export_entries(entries: HashMap<String, CardExportEntry>) -> Self {
        let mut face_index = HashMap::with_capacity(entries.len());
        let mut oracle_id_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut layout_index: HashMap<String, LayoutKind> = HashMap::new();
        let mut legalities = HashMap::new();
        let mut printings_index: HashMap<String, Vec<String>> = HashMap::new();
        let mut rulings_index: HashMap<String, Vec<Ruling>> = HashMap::new();
        let mut bracket_signals_by_name: HashMap<String, BracketSignals> =
            HashMap::with_capacity(entries.len());

        for (export_key, entry) in entries {
            let storage_key = export_key.to_lowercase();
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
        let name_alias_index = build_name_alias_index(face_index.keys());
        let creature_type_vocabulary = collect_creature_type_vocabulary(face_index.values());

        Self {
            cards: HashMap::new(),
            face_index,
            name_alias_index,
            oracle_id_index,
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
                printings: self.printings_index.get(&key).cloned().unwrap_or_default(),
                rulings: self.rulings_index.get(&key).cloned().unwrap_or_default(),
                bracket_signals: self
                    .bracket_signals_by_name
                    .get(&key)
                    .copied()
                    .unwrap_or_default(),
            };
            out.insert(face.name.clone(), entry);
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
    pub fn bracket_signals_for(&self, name: &str) -> BracketSignals {
        if let Some((a, b)) = name.split_once(" // ") {
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

    fn lookup_key(&self, name: &str) -> String {
        let lower = name.to_lowercase();
        if self.face_index.contains_key(&lower) || self.cards.contains_key(&lower) {
            return lower;
        }
        if let Some(alias) = self.name_alias_index.get(&fold_card_name_key(name)) {
            return alias.clone();
        }
        if let Some((front, _)) = lower.split_once("//") {
            let front = front.trim();
            if self.face_index.contains_key(front) || self.cards.contains_key(front) {
                return front.to_string();
            }
            if let Some(alias) = self.name_alias_index.get(&fold_card_name_key(front)) {
                return alias.clone();
            }
        }
        lower
    }
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
    }
    aliases
        .into_iter()
        .filter_map(|(alias, key)| key.map(|key| (alias, key)))
        .collect()
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

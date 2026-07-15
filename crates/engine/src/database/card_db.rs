use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::bracket_lists::{BracketLists, BracketSignals};
use super::legality::{normalize_legalities, CardLegalities, LegalityFormat, LegalityStatus};
use super::mtgjson::Ruling;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AdditionalCost, ContinuousModification, Effect,
    ReplacementDefinition, ReplacementMode, ResolvedAbility, SearchFoundModifier,
    SpellCastingOption, StaticDefinition, TriggerDefinition,
};
use crate::types::card::{CardFace, CardRules, LayoutKind, PrintedCardRef};
use crate::types::card_type::CoreType;
use crate::types::keywords::Keyword;

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
        validate_replacement_invariants(&entries)
            .map_err(|message| std::io::Error::new(std::io::ErrorKind::InvalidData, message))?;
        Ok(Self::from_export_entries(entries))
    }

    /// Load from a card-data export JSON string.
    /// Used by the WASM bridge to receive card data from the frontend.
    pub fn from_json_str(json: &str) -> Result<Self, serde_json::Error> {
        let entries: HashMap<String, CardExportEntry> = serde_json::from_str(json)?;
        validate_replacement_invariants(&entries).map_err(|message| {
            serde_json::Error::io(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                message,
            ))
        })?;
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

fn validate_replacement_invariants(
    entries: &HashMap<String, CardExportEntry>,
) -> Result<(), String> {
    for (name, entry) in entries {
        validate_card_face_replacement_invariants(&entry.face)
            .map_err(|problem| format!("{name}: {problem}"))?;
    }
    Ok(())
}

/// Placement context for the SearchFound-only runtime effect.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SearchFoundEffectContext {
    Forbidden,
    CanonicalExecute,
}

/// Shared typed validation authority for every load and export path.
///
/// `ApplySearchFoundReplacement` is accepted only as the exact direct execute
/// of a `ReplacementEvent::SearchFound` definition. Every nested carrier is
/// traversed as typed Rust data, so an unrelated JSON object that happens to
/// contain an `event` key can neither evade nor accidentally trigger validation.
pub fn validate_card_face_for_export(face: &CardFace) -> Result<(), String> {
    let root = format!("face[{}]", face.name);
    if let Some(power) = &face.power {
        validate_pt_value(power, &format!("{root}.power"))?;
    }
    if let Some(toughness) = &face.toughness {
        validate_pt_value(toughness, &format!("{root}.toughness"))?;
    }
    for (index, keyword) in face.keywords.iter().enumerate() {
        validate_keyword(keyword, &format!("{root}.keywords[{index}]"))?;
    }
    validate_face_ability_sets(
        &face.abilities,
        &face.triggers,
        &face.static_abilities,
        &face.replacements,
        &root,
    )?;
    if let Some(cleave) = &face.cleave_variant {
        validate_face_ability_sets(
            &cleave.abilities,
            &cleave.triggers,
            &cleave.static_abilities,
            &cleave.replacements,
            &format!("{root}.cleave_variant"),
        )?;
    }
    if let Some(additional) = &face.additional_cost {
        match additional {
            AdditionalCost::Optional { cost, .. } | AdditionalCost::Required(cost) => {
                validate_cost(cost, &format!("{root}.additional_cost"))?;
            }
            AdditionalCost::Kicker { costs, .. } => {
                for (index, cost) in costs.iter().enumerate() {
                    validate_cost(cost, &format!("{root}.additional_cost.kicker[{index}]"))?;
                }
            }
            AdditionalCost::Choice(left, right) => {
                validate_cost(left, &format!("{root}.additional_cost.choice[0]"))?;
                validate_cost(right, &format!("{root}.additional_cost.choice[1]"))?;
            }
        }
    }
    if let Some(modal) = &face.modal {
        validate_modal_choice(modal, &format!("{root}.modal"))?;
    }
    for (index, restriction) in face.casting_restrictions.iter().enumerate() {
        validate_casting_restriction(
            restriction,
            &format!("{root}.casting_restrictions[{index}]"),
        )?;
    }
    for (index, option) in face.casting_options.iter().enumerate() {
        validate_casting_option(option, &format!("{root}.casting_options[{index}]"))?;
    }
    if let Some(condition) = &face.solve_condition {
        validate_solve_condition(condition, &format!("{root}.solve_condition"))?;
    }
    Ok(())
}

pub(crate) fn validate_card_face_replacement_invariants(face: &CardFace) -> Result<(), String> {
    validate_card_face_for_export(face)
}

fn validate_face_ability_sets(
    abilities: &[AbilityDefinition],
    triggers: &[TriggerDefinition],
    statics: &[StaticDefinition],
    replacements: &[ReplacementDefinition],
    path: &str,
) -> Result<(), String> {
    for (index, ability) in abilities.iter().enumerate() {
        validate_ability(ability, &format!("{path}.abilities[{index}]"))?;
    }
    for (index, trigger) in triggers.iter().enumerate() {
        validate_trigger(trigger, &format!("{path}.triggers[{index}]"))?;
    }
    for (index, static_def) in statics.iter().enumerate() {
        validate_static(static_def, &format!("{path}.statics[{index}]"))?;
    }
    for (index, replacement) in replacements.iter().enumerate() {
        validate_replacement(replacement, &format!("{path}.replacements[{index}]"))?;
    }
    Ok(())
}

fn validate_ability(ability: &AbilityDefinition, path: &str) -> Result<(), String> {
    validate_effect(
        ability.effect.as_ref(),
        SearchFoundEffectContext::Forbidden,
        &format!("{path}.effect"),
    )?;
    if let Some(cost) = &ability.cost {
        validate_cost(cost, &format!("{path}.cost"))?;
    }
    if let Some(cost) = &ability.unless_pay {
        validate_cost(&cost.cost, &format!("{path}.unless_pay.cost"))?;
        validate_target_filter(&cost.payer, &format!("{path}.unless_pay.payer"))?;
    }
    for (index, restriction) in ability.activation_restrictions.iter().enumerate() {
        validate_activation_restriction(
            restriction,
            &format!("{path}.activation_restrictions[{index}]"),
        )?;
    }
    if let Some(filter) = &ability.activator_filter {
        validate_player_filter(filter, &format!("{path}.activator_filter"))?;
    }
    if let Some(condition) = &ability.condition {
        validate_ability_condition(condition, &format!("{path}.condition"))?;
    }
    if let Some(duration) = &ability.duration {
        validate_duration(duration, &format!("{path}.duration"))?;
    }
    if let Some(quantity) = &ability.repeat_for {
        validate_quantity_expr(quantity, &format!("{path}.repeat_for"))?;
    }
    if let Some(quantity) = &ability.announced_x {
        validate_quantity_expr(quantity, &format!("{path}.announced_x"))?;
    }
    if let Some(multi_target) = &ability.multi_target {
        validate_quantity_expr(&multi_target.min, &format!("{path}.multi_target.min"))?;
        if let Some(max) = &multi_target.max {
            validate_quantity_expr(max, &format!("{path}.multi_target.max"))?;
        }
    }
    for (index, constraint) in ability.target_constraints.iter().enumerate() {
        validate_target_selection_constraint(
            constraint,
            &format!("{path}.target_constraints[{index}]"),
        )?;
    }
    if let Some(filter) = &ability.target_chooser {
        validate_target_filter(filter, &format!("{path}.target_chooser"))?;
    }
    if let Some(filter) = &ability.player_scope {
        validate_player_filter(filter, &format!("{path}.player_scope"))?;
    }
    if let Some(modal) = &ability.modal {
        validate_modal_choice(modal, &format!("{path}.modal"))?;
    }
    if let Some(repeat) = &ability.repeat_until {
        validate_repeat_continuation(repeat, &format!("{path}.repeat_until"))?;
    }
    if let Some(reduction) = &ability.cost_reduction {
        validate_quantity_expr(&reduction.count, &format!("{path}.cost_reduction.count"))?;
        if let Some(condition) = &reduction.condition {
            validate_parsed_condition(condition, &format!("{path}.cost_reduction.condition"))?;
        }
    }
    if let Some(sub) = &ability.sub_ability {
        validate_ability(sub, &format!("{path}.sub_ability"))?;
    }
    if let Some(else_ability) = &ability.else_ability {
        validate_ability(else_ability, &format!("{path}.else_ability"))?;
    }
    for (index, mode) in ability.mode_abilities.iter().enumerate() {
        validate_ability(mode, &format!("{path}.mode_abilities[{index}]"))?;
    }
    Ok(())
}

fn validate_resolved(ability: &ResolvedAbility, path: &str) -> Result<(), String> {
    validate_effect(
        &ability.effect,
        SearchFoundEffectContext::Forbidden,
        &format!("{path}.effect"),
    )?;
    if let Some(condition) = &ability.condition {
        validate_ability_condition(condition, &format!("{path}.condition"))?;
    }
    validate_spell_context(&ability.context, &format!("{path}.context"))?;
    if let Some(duration) = &ability.duration {
        validate_duration(duration, &format!("{path}.duration"))?;
    }
    if let Some(quantity) = &ability.repeat_for {
        validate_quantity_expr(quantity, &format!("{path}.repeat_for"))?;
    }
    if let Some(quantity) = &ability.announced_x {
        validate_quantity_expr(quantity, &format!("{path}.announced_x"))?;
    }
    if let Some(multi_target) = &ability.multi_target {
        validate_quantity_expr(&multi_target.min, &format!("{path}.multi_target.min"))?;
        if let Some(max) = &multi_target.max {
            validate_quantity_expr(max, &format!("{path}.multi_target.max"))?;
        }
    }
    for (index, constraint) in ability.target_constraints.iter().enumerate() {
        validate_target_selection_constraint(
            constraint,
            &format!("{path}.target_constraints[{index}]"),
        )?;
    }
    if let Some(unless_pay) = &ability.unless_pay {
        validate_cost(&unless_pay.cost, &format!("{path}.unless_pay.cost"))?;
        validate_target_filter(&unless_pay.payer, &format!("{path}.unless_pay.payer"))?;
    }
    if let Some(filter) = &ability.player_scope {
        validate_player_filter(filter, &format!("{path}.player_scope"))?;
    }
    if let Some(filter) = &ability.target_chooser {
        validate_target_filter(filter, &format!("{path}.target_chooser"))?;
    }
    if let Some(modal) = &ability.modal {
        validate_modal_choice(modal, &format!("{path}.modal"))?;
    }
    if let Some(repeat) = &ability.repeat_until {
        validate_repeat_continuation(repeat, &format!("{path}.repeat_until"))?;
    }
    for (field, snapshot) in [
        ("cost_paid_object", ability.cost_paid_object.as_ref()),
        (
            "effect_context_object",
            ability.effect_context_object.as_ref(),
        ),
        ("amassed_army_object", ability.amassed_army_object.as_ref()),
    ] {
        if let Some(snapshot) = snapshot {
            validate_lki_snapshot(&snapshot.lki, &format!("{path}.{field}.lki"))?;
        }
    }
    if let Some(sub) = &ability.sub_ability {
        validate_resolved(sub, &format!("{path}.sub_ability"))?;
    }
    if let Some(else_ability) = &ability.else_ability {
        validate_resolved(else_ability, &format!("{path}.else_ability"))?;
    }
    for (index, mode) in ability.mode_abilities.iter().enumerate() {
        validate_ability(mode, &format!("{path}.mode_abilities[{index}]"))?;
    }
    Ok(())
}

fn validate_trigger(trigger: &TriggerDefinition, path: &str) -> Result<(), String> {
    if let Some(execute) = &trigger.execute {
        validate_ability(execute, &format!("{path}.execute"))?;
    }
    if let Some(unless_pay) = &trigger.unless_pay {
        validate_cost(&unless_pay.cost, &format!("{path}.unless_pay.cost"))?;
        validate_target_filter(&unless_pay.payer, &format!("{path}.unless_pay.payer"))?;
    }
    for (field, filter) in [
        ("valid_card", trigger.valid_card.as_ref()),
        ("valid_target", trigger.valid_target.as_ref()),
        (
            "valid_subject_player",
            trigger.valid_subject_player.as_ref(),
        ),
        ("valid_source", trigger.valid_source.as_ref()),
    ] {
        if let Some(filter) = filter {
            validate_target_filter(filter, &format!("{path}.{field}"))?;
        }
    }
    for (index, clause) in trigger.zone_change_clauses.iter().enumerate() {
        if let Some(filter) = &clause.valid_card {
            validate_target_filter(
                filter,
                &format!("{path}.zone_change_clauses[{index}].valid_card"),
            )?;
        }
    }
    if let Some(constraint) = &trigger.constraint {
        validate_trigger_constraint(constraint, &format!("{path}.constraint"))?;
    }
    if let Some(condition) = &trigger.condition {
        validate_trigger_condition(condition, &format!("{path}.condition"))?;
    }
    Ok(())
}

fn validate_spell_context(
    context: &crate::types::ability::SpellContext,
    path: &str,
) -> Result<(), String> {
    for (index, filter) in context.controller_controlled_as_cast.iter().enumerate() {
        validate_target_filter(
            filter,
            &format!("{path}.controller_controlled_as_cast[{index}]"),
        )?;
    }
    Ok(())
}

fn validate_lki_snapshot(
    snapshot: &crate::types::game_state::LKISnapshot,
    path: &str,
) -> Result<(), String> {
    for (index, keyword) in snapshot.keywords.iter().enumerate() {
        validate_keyword(keyword, &format!("{path}.keywords[{index}]"))?;
    }
    for (index, attribute) in snapshot.chosen_attributes.iter().enumerate() {
        if let crate::types::ability::ChosenAttribute::Keyword(keyword) = attribute {
            validate_keyword(
                keyword,
                &format!("{path}.chosen_attributes[{index}].keyword"),
            )?;
        }
    }
    Ok(())
}

fn validate_trigger_constraint(
    constraint: &crate::types::ability::TriggerConstraint,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::TriggerConstraint;

    match constraint {
        TriggerConstraint::NthSpellThisTurn { filter, .. } => {
            if let Some(filter) = filter {
                validate_target_filter(filter, &format!("{path}.filter"))?;
            }
        }
        TriggerConstraint::OncePerTurn
        | TriggerConstraint::OncePerGame
        | TriggerConstraint::OnlyDuringYourTurn
        | TriggerConstraint::NthDrawThisTurn { .. }
        | TriggerConstraint::OnlyDuringOpponentsTurn
        | TriggerConstraint::OnlyDuringYourMainPhase
        | TriggerConstraint::AtClassLevel { .. }
        | TriggerConstraint::MaxTimesPerTurn { .. }
        | TriggerConstraint::OncePerOpponentPerTurn
        | TriggerConstraint::EventSourceControlledBy { .. } => {}
    }
    Ok(())
}

fn validate_trigger_condition(
    condition: &crate::types::ability::TriggerCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::{AttackersDeclaredCountSubject, TriggerCondition};

    match condition {
        TriggerCondition::QuantityComparison { lhs, rhs, .. } => {
            validate_quantity_expr(lhs, &format!("{path}.lhs"))?;
            validate_quantity_expr(rhs, &format!("{path}.rhs"))?;
        }
        TriggerCondition::DuringPlayersTurn { player } => {
            validate_player_filter(player, &format!("{path}.player"))?
        }
        TriggerCondition::And { conditions } | TriggerCondition::Or { conditions } => {
            for (index, condition) in conditions.iter().enumerate() {
                validate_trigger_condition(condition, &format!("{path}.conditions[{index}]"))?;
            }
        }
        TriggerCondition::Not { condition } => {
            validate_trigger_condition(condition, &format!("{path}.condition"))?
        }
        TriggerCondition::ControlsType { filter }
        | TriggerCondition::DealtDamageThisTurnBySource { source: filter }
        | TriggerCondition::ControlCount { filter, .. }
        | TriggerCondition::ControlsNone { filter }
        | TriggerCondition::DefendingPlayerControlsNone { filter }
        | TriggerCondition::SourceMatchesFilter { filter }
        | TriggerCondition::ZoneChangeObjectMatchesFilter { filter, .. }
        | TriggerCondition::EventDamageSourceMatchesFilter { filter }
        | TriggerCondition::EventObjectMatchesFilter { filter }
        | TriggerCondition::TriggeringSpellTargetsFilter { filter }
        | TriggerCondition::TriggeringSpellMatchesFilter { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        TriggerCondition::MinCoAttackers { filter, .. }
        | TriggerCondition::CastSpellThisTurn { filter } => {
            if let Some(filter) = filter {
                validate_target_filter(filter, &format!("{path}.filter"))?;
            }
        }
        TriggerCondition::AttackersDeclaredCount { subject, .. } => {
            let filter = match subject {
                AttackersDeclaredCountSubject::Controller { filter, .. }
                | AttackersDeclaredCountSubject::AttackTarget { filter, .. } => filter,
            };
            if let Some(filter) = filter {
                validate_target_filter(filter, &format!("{path}.subject.filter"))?;
            }
        }
        TriggerCondition::GainedLife { .. }
        | TriggerCondition::LostLife
        | TriggerCondition::LostLifeLastTurn
        | TriggerCondition::Descended
        | TriggerCondition::NoSpellsCastLastTurn
        | TriggerCondition::TwoOrMoreSpellsCastLastTurn
        | TriggerCondition::SourceEnteredThisTurn
        | TriggerCondition::EchoDue
        | TriggerCondition::SolveConditionMet
        | TriggerCondition::ClassLevelGE { .. }
        | TriggerCondition::SourceIsHarnessed
        | TriggerCondition::AttractionVisitRoll { .. }
        | TriggerCondition::WasCast { .. }
        | TriggerCondition::WasPlayed
        | TriggerCondition::AdditionalCostPaid { .. }
        | TriggerCondition::SourceIsAttacking
        | TriggerCondition::CastVariantPaid { .. }
        | TriggerCondition::CastVariantPaidPersistent { .. }
        | TriggerCondition::ActivatedAbilityIsNonMana
        | TriggerCondition::DealtDamageBySourceThisTurn
        | TriggerCondition::FirstTimeObjectTappedThisTurn
        | TriggerCondition::FirstTimeObjectCountersAddedThisTurn
        | TriggerCondition::WasType { .. }
        | TriggerCondition::LifeTotalGE { .. }
        | TriggerCondition::AttackedThisTurn
        | TriggerCondition::FirstCombatPhaseOfTurn
        | TriggerCondition::HasMaxSpeed
        | TriggerCondition::IsMonarch
        | TriggerCondition::IsInitiative
        | TriggerCondition::NoMonarch
        | TriggerCondition::WasStartingPlayer { .. }
        | TriggerCondition::SpellCastWithVariantThisTurn { .. }
        | TriggerCondition::HasCityBlessing
        | TriggerCondition::CompletedDungeon { .. }
        | TriggerCondition::SourceIsTapped
        | TriggerCondition::SourceIsTransformed
        | TriggerCondition::SourceIsFaceUp
        | TriggerCondition::SourceIsFaceDown
        | TriggerCondition::SourceInZone { .. }
        | TriggerCondition::CounterAddedThisTurn
        | TriggerCondition::TributeNotPaid
        | TriggerCondition::CastDuringPhase { .. }
        | TriggerCondition::CastTimingPermission { .. }
        | TriggerCondition::ManaColorSpent { .. }
        | TriggerCondition::ManaSpentCondition { .. }
        | TriggerCondition::HadCounters { .. }
        | TriggerCondition::ControlsCommander { .. }
        | TriggerCondition::IsRenowned { .. }
        | TriggerCondition::HasCounters { .. }
        | TriggerCondition::ZoneChangeObjectIsTapped
        | TriggerCondition::DamagedPlayerIsEventSourceOwner
        | TriggerCondition::ChosenLabelIs { .. }
        | TriggerCondition::ExceptFirstDrawInDrawStep
        | TriggerCondition::PlacedByAbilitySource => {}
    }
    Ok(())
}

fn validate_replacement(replacement: &ReplacementDefinition, path: &str) -> Result<(), String> {
    replacement
        .validate_search_found_modifier()
        .map_err(|problem| format!("{path}: {problem}"))?;
    if let Some(execute) = &replacement.execute {
        let context =
            if replacement.event == crate::types::replacements::ReplacementEvent::SearchFound {
                SearchFoundEffectContext::CanonicalExecute
            } else {
                SearchFoundEffectContext::Forbidden
            };
        validate_effect(
            execute.effect.as_ref(),
            context,
            &format!("{path}.execute.effect"),
        )?;
        if context == SearchFoundEffectContext::Forbidden {
            validate_ability(execute, &format!("{path}.execute"))?;
        }
    }
    if let Some(execute) = &replacement.runtime_execute {
        validate_resolved(execute, &format!("{path}.runtime_execute"))?;
    }
    if let Some(filter) = &replacement.valid_card {
        validate_target_filter(filter, &format!("{path}.valid_card"))?;
    }
    if let Some(condition) = &replacement.condition {
        validate_replacement_condition(condition, &format!("{path}.condition"))?;
    }
    if let Some(filter) = &replacement.damage_source_filter {
        validate_target_filter(filter, &format!("{path}.damage_source_filter"))?;
    }
    if let Some(filter) = &replacement.redirect_target {
        validate_target_filter(filter, &format!("{path}.redirect_target"))?;
    }
    if let Some(modification) = &replacement.damage_modification {
        validate_damage_modification(modification, &format!("{path}.damage_modification"))?;
    }
    match &replacement.mode {
        ReplacementMode::Mandatory => {}
        ReplacementMode::Optional { decline } => {
            if let Some(decline) = decline {
                validate_ability(decline, &format!("{path}.mode.decline"))?;
            }
        }
        ReplacementMode::MayCost { cost, decline } => {
            validate_cost(cost, &format!("{path}.mode.cost"))?;
            if let Some(decline) = decline {
                validate_ability(decline, &format!("{path}.mode.decline"))?;
            }
        }
    }
    for (index, spec) in replacement.ensure_token_specs.iter().flatten().enumerate() {
        validate_token_spec(spec, &format!("{path}.ensure_token_specs[{index}]"))?;
    }
    if let Some(spec) = &replacement.additional_token_spec {
        validate_token_spec(spec, &format!("{path}.additional_token_spec"))?;
    }
    Ok(())
}

fn validate_replacement_condition(
    condition: &crate::types::ability::ReplacementCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::ReplacementCondition;

    match condition {
        ReplacementCondition::And { conditions } => {
            for (index, condition) in conditions.iter().enumerate() {
                validate_replacement_condition(condition, &format!("{path}.conditions[{index}]"))?;
            }
        }
        ReplacementCondition::UnlessControlsOtherLeq { filter, .. } => {
            validate_typed_filter(filter, &format!("{path}.filter"))?
        }
        ReplacementCondition::UnlessControlsMatching { filter }
        | ReplacementCondition::UnlessControlsCountMatching { filter, .. }
        | ReplacementCondition::DealtDamageThisTurnBySource { source: filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        ReplacementCondition::UnlessQuantity { lhs, rhs, .. }
        | ReplacementCondition::OnlyIfQuantity { lhs, rhs, .. } => {
            validate_quantity_expr(lhs, &format!("{path}.lhs"))?;
            validate_quantity_expr(rhs, &format!("{path}.rhs"))?;
        }
        ReplacementCondition::UnlessControlsSubtype { .. }
        | ReplacementCondition::UnlessPlayerLifeAtMost { .. }
        | ReplacementCondition::UnlessMultipleOpponents
        | ReplacementCondition::UnlessYourTurn
        | ReplacementCondition::HasMaxSpeed
        | ReplacementCondition::CastViaEscape
        | ReplacementCondition::CastVariantPaid { .. }
        | ReplacementCondition::CastFromZone { .. }
        | ReplacementCondition::EnteredFromZone { .. }
        | ReplacementCondition::YouAttackedThisTurn
        | ReplacementCondition::OpponentDamagedThisTurn
        | ReplacementCondition::CastViaKicker { .. }
        | ReplacementCondition::SourceTappedState { .. }
        | ReplacementCondition::EventSourceControlledBy { .. }
        | ReplacementCondition::EffectCausedDiscard
        | ReplacementCondition::OnlyExtraTurn
        | ReplacementCondition::TokenSubtypeMatches { .. }
        | ReplacementCondition::TokenCoreTypeMatches { .. }
        | ReplacementCondition::FirstTokenCreationEachTurn { .. }
        | ReplacementCondition::ExceptFirstDrawInDrawStep
        | ReplacementCondition::ClassLevelGE { .. }
        | ReplacementCondition::DuringUntapStep
        | ReplacementCondition::DuringDrawStep { .. }
        | ReplacementCondition::ControllerControlsSource { .. }
        | ReplacementCondition::Unrecognized { .. } => {}
        ReplacementCondition::IfControlsMatching { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
    }
    Ok(())
}

fn validate_static(static_def: &StaticDefinition, path: &str) -> Result<(), String> {
    validate_static_mode(&static_def.mode, &format!("{path}.mode"))?;
    if let Some(filter) = &static_def.affected {
        validate_target_filter(filter, &format!("{path}.affected"))?;
    }
    if let Some(condition) = &static_def.condition {
        validate_static_condition(condition, &format!("{path}.condition"))?;
    }
    if let Some(condition) = &static_def.per_player_condition {
        validate_parsed_condition(condition, &format!("{path}.per_player_condition"))?;
    }
    for (index, modification) in static_def.modifications.iter().enumerate() {
        validate_continuous_modification(modification, &format!("{path}.modifications[{index}]"))?;
    }
    Ok(())
}

fn validate_static_mode(
    mode: &crate::types::statics::StaticMode,
    path: &str,
) -> Result<(), String> {
    use crate::types::statics::StaticMode;

    match mode {
        StaticMode::CastWithKeyword { keyword } => {
            validate_keyword(keyword, &format!("{path}.keyword"))?
        }
        StaticMode::CantHaveKeyword { keyword } => {
            validate_keyword(keyword, &format!("{path}.keyword"))?
        }
        StaticMode::CastWithAlternativeCost { cost, .. }
        | StaticMode::AlternativeKeywordCost { cost, .. } => {
            validate_cost(cost, &format!("{path}.cost"))?
        }
        StaticMode::ImposeAdditionalCost {
            cost, spell_filter, ..
        } => {
            validate_cost(cost, &format!("{path}.cost"))?;
            if let Some(filter) = spell_filter {
                validate_target_filter(filter, &format!("{path}.spell_filter"))?;
            }
        }
        StaticMode::GraveyardCastPermission {
            extra_cost: Some(extra_cost),
            ..
        }
        | StaticMode::ExileCastPermission {
            extra_cost: Some(extra_cost),
            ..
        } => validate_cost(&extra_cost.cost, &format!("{path}.extra_cost.cost"))?,
        StaticMode::TopOfLibraryCastPermission {
            alt_cost: Some(cost),
            ..
        } => validate_cost(cost, &format!("{path}.alt_cost"))?,
        StaticMode::CantBeActivated { source_filter, .. }
        | StaticMode::AttachmentRestriction {
            filter: source_filter,
        }
        | StaticMode::CantBeBlockedBy {
            filter: source_filter,
        }
        | StaticMode::BlockRestriction {
            filter: source_filter,
        } => validate_target_filter(source_filter, &format!("{path}.filter"))?,
        StaticMode::ModifyCost {
            spell_filter,
            dynamic_count,
            ..
        } => {
            if let Some(filter) = spell_filter {
                validate_target_filter(filter, &format!("{path}.spell_filter"))?;
            }
            if let Some(quantity) = dynamic_count {
                validate_quantity_ref(quantity, &format!("{path}.dynamic_count"))?;
            }
        }
        StaticMode::SuppressTriggers {
            source_filter: filter,
            ..
        } => validate_target_filter(filter, &format!("{path}.spell_filter"))?,
        StaticMode::PerTurnCastLimit {
            spell_filter: Some(filter),
            ..
        }
        | StaticMode::MaxUntapPerType { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        StaticMode::ReduceAbilityCost {
            activator,
            dynamic_count,
            ..
        } => {
            if let Some(filter) = activator {
                validate_player_filter(filter, &format!("{path}.activator"))?;
            }
            if let Some(quantity) = dynamic_count {
                validate_quantity_ref(quantity, &format!("{path}.dynamic_count"))?;
            }
        }
        StaticMode::GraveyardCastPermission {
            extra_cost: None, ..
        }
        | StaticMode::ExileCastPermission {
            extra_cost: None, ..
        }
        | StaticMode::TopOfLibraryCastPermission { alt_cost: None, .. }
        | StaticMode::Continuous
        | StaticMode::DamageNotRemovedDuringCleanup
        | StaticMode::CantAttack
        | StaticMode::CantBlock
        | StaticMode::CantAttackOrBlock
        | StaticMode::CantBecomeSuspected
        | StaticMode::CantBeBlockedExceptBy { .. }
        | StaticMode::MaxAttackersEachCombat { .. }
        | StaticMode::MaxBlockersEachCombat { .. }
        | StaticMode::CantBeTargeted
        | StaticMode::CantBeCast { .. }
        | StaticMode::CantSearchLibrary { .. }
        | StaticMode::RestrictLibrarySearchToTop { .. }
        | StaticMode::ControlPlayersDuringOwnLibrarySearch { .. }
        | StaticMode::CantCauseSacrificeOrExile { .. }
        | StaticMode::CastWithFlash
        | StaticMode::GrantsExtraVote
        | StaticMode::GrantsExtraVillainousChoice
        | StaticMode::ReduceActionCost { .. }
        | StaticMode::ModifyActivationLimit { .. }
        | StaticMode::ActivateAsInstant { .. }
        | StaticMode::CantPayCost { .. }
        | StaticMode::CantGainLife
        | StaticMode::CantLoseLife
        | StaticMode::PlayerProtection(..)
        | StaticMode::MustAttack
        | StaticMode::MustAttackPlayer { .. }
        | StaticMode::MustBlock
        | StaticMode::MustBlockAttacker { .. }
        | StaticMode::CantDraw { .. }
        | StaticMode::DrawFromBottom { .. }
        | StaticMode::DoubleTriggers { .. }
        | StaticMode::IgnoreHexproof
        | StaticMode::ExtraBlockers { .. }
        | StaticMode::RevealTopOfLibrary { .. }
        | StaticMode::RevealHand { .. }
        | StaticMode::TopOfLibraryHasPlot
        | StaticMode::TopOfLibraryPlotPermission
        | StaticMode::CastFromHandFree { .. }
        | StaticMode::LinkedCollectionCounterPlayPermission
        | StaticMode::CountersPersistAcrossZones { .. }
        | StaticMode::CantBeCountered
        | StaticMode::CantBeCopied
        | StaticMode::CantEnterBattlefieldFrom
        | StaticMode::CantCastFrom { .. }
        | StaticMode::CantCastDuring { .. }
        | StaticMode::CantActivateDuring { .. }
        | StaticMode::PerTurnCastLimit {
            spell_filter: None, ..
        }
        | StaticMode::PerTurnDrawLimit { .. }
        | StaticMode::CantBeBlocked
        | StaticMode::CantBeBlockedByMoreThan { .. }
        | StaticMode::CantBeBlockedUnlessAllBlock
        | StaticMode::Protection
        | StaticMode::Indestructible
        | StaticMode::CantBeDestroyed
        | StaticMode::CantBeRegenerated
        | StaticMode::FlashBack
        | StaticMode::Shroud
        | StaticMode::Hexproof
        | StaticMode::Vigilance
        | StaticMode::Menace
        | StaticMode::Reach
        | StaticMode::Flying
        | StaticMode::Trample
        | StaticMode::Deathtouch
        | StaticMode::Lifelink
        | StaticMode::CantTap
        | StaticMode::CantUntap
        | StaticMode::MustBeBlocked { by: None }
        | StaticMode::MustBeBlockedByAll { blockers: None }
        | StaticMode::Goaded
        | StaticMode::CombatAlone { .. }
        | StaticMode::CantCrew
        | StaticMode::CantPhaseIn
        | StaticMode::CrewContribution { .. }
        | StaticMode::MayLookAtTopOfLibrary
        | StaticMode::MayLookAtFaceDown
        | StaticMode::CantBeTurnedFaceUp
        | StaticMode::MayChooseNotToUntap
        | StaticMode::AdditionalLandDrop { .. }
        | StaticMode::EmblemStatic
        | StaticMode::NoMaximumHandSize
        | StaticMode::MaximumHandSize { .. }
        | StaticMode::MayPlayAdditionalLand
        | StaticMode::CantWinTheGame
        | StaticMode::CantLoseTheGame
        | StaticMode::LegendRuleDoesntApply
        | StaticMode::SpeedCanIncreaseBeyondFour
        | StaticMode::DefilerCostReduction { .. }
        | StaticMode::SkipStep { .. }
        | StaticMode::PayLifeAsColoredMana { .. }
        | StaticMode::StepEndUnspentMana { .. }
        | StaticMode::CanAttackWithDefender
        | StaticMode::AttackOnlyNeighbor
        | StaticMode::IgnoreLandwalkForBlocking { .. }
        | StaticMode::CanActivateAbilitiesAsThoughHaste
        | StaticMode::CanBlockShadow
        | StaticMode::AssignNoCombatDamage
        | StaticMode::UntapsDuringEachOtherPlayersUntapStep
        | StaticMode::EntersWithAdditionalCounters { .. }
        | StaticMode::CountersCantBeRemoved { .. }
        | StaticMode::CountsAsNamed { .. }
        | StaticMode::Other(..) => {}
        StaticMode::MustBeBlocked { by: Some(filter) }
        | StaticMode::MustBeBlockedByAll {
            blockers: Some(filter),
        } => validate_target_filter(filter, &format!("{path}.filter"))?,
        StaticMode::SpendManaAsAnyColor {
            spell_filter,
            activation_source_filter,
        } => {
            if let Some(filter) = spell_filter {
                validate_target_filter(filter, &format!("{path}.spell_filter"))?;
            }
            if let Some(filter) = activation_source_filter {
                validate_target_filter(filter, &format!("{path}.activation_source_filter"))?;
            }
        }
    }
    Ok(())
}

fn validate_continuous_modification(
    modification: &ContinuousModification,
    path: &str,
) -> Result<(), String> {
    match modification {
        ContinuousModification::GrantAbility { definition } => {
            validate_ability(definition, &format!("{path}.definition"))?
        }
        ContinuousModification::GrantTrigger { trigger } => {
            validate_trigger(trigger, &format!("{path}.trigger"))?
        }
        ContinuousModification::GrantStaticAbility { definition } => {
            validate_static(definition, &format!("{path}.definition"))?
        }
        ContinuousModification::CopyValues { values, .. } => {
            for (index, ability) in values.abilities.iter().enumerate() {
                validate_ability(ability, &format!("{path}.values.abilities[{index}]"))?;
            }
            for (index, trigger) in values.trigger_definitions.iter().enumerate() {
                validate_trigger(trigger, &format!("{path}.values.triggers[{index}]"))?;
            }
            for (index, replacement) in values.replacement_definitions.iter().enumerate() {
                validate_replacement(replacement, &format!("{path}.values.replacements[{index}]"))?;
            }
            for (index, static_def) in values.static_definitions.iter().enumerate() {
                validate_static(static_def, &format!("{path}.values.statics[{index}]"))?;
            }
            for (index, keyword) in values.keywords.iter().enumerate() {
                validate_keyword(keyword, &format!("{path}.values.keywords[{index}]"))?;
            }
        }
        ContinuousModification::AddKeyword { keyword }
        | ContinuousModification::RemoveKeyword { keyword } => {
            validate_keyword(keyword, &format!("{path}.keyword"))?
        }
        ContinuousModification::AddStaticMode { mode } => {
            validate_static_mode(mode, &format!("{path}.mode"))?
        }
        ContinuousModification::GrantAllActivatedAbilitiesOf { source, cap } => {
            validate_target_filter(source, &format!("{path}.source"))?;
            if let Some(restriction) = cap {
                validate_activation_restriction(restriction, &format!("{path}.cap"))?;
            }
        }
        ContinuousModification::GrantAllTriggeredAbilitiesOf { source } => {
            validate_target_filter(source, &format!("{path}.source"))?
        }
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. }
        | ContinuousModification::AddCounterOnEnter { count: value, .. } => {
            validate_quantity_expr(value, &format!("{path}.value"))?
        }
        ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => {}
    }
    Ok(())
}

fn validate_cost(cost: &AbilityCost, path: &str) -> Result<(), String> {
    match cost {
        AbilityCost::EffectCost { effect } => validate_effect(
            effect,
            SearchFoundEffectContext::Forbidden,
            &format!("{path}.effect"),
        )?,
        AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
            for (index, cost) in costs.iter().enumerate() {
                validate_cost(cost, &format!("{path}[{index}]"))?;
            }
        }
        AbilityCost::PerCounter { target, base, .. } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_cost(base, &format!("{path}.base"))?;
        }
        AbilityCost::ManaDynamic { quantity } => {
            validate_quantity_expr(quantity, &format!("{path}.quantity"))?
        }
        AbilityCost::PayLife { amount }
        | AbilityCost::PayEnergy { amount }
        | AbilityCost::PaySpeed { amount } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?
        }
        AbilityCost::Sacrifice(cost) => {
            validate_target_filter(&cost.target, &format!("{path}.target"))?
        }
        AbilityCost::Discard { count, filter, .. } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(filter) = filter {
                validate_target_filter(filter, &format!("{path}.filter"))?;
            }
        }
        AbilityCost::Exile {
            filter: Some(filter),
            ..
        }
        | AbilityCost::RemoveCounter {
            target: Some(filter),
            ..
        }
        | AbilityCost::ReturnToHand {
            filter: Some(filter),
            ..
        }
        | AbilityCost::Reveal {
            filter: Some(filter),
            ..
        } => validate_target_filter(filter, &format!("{path}.filter"))?,
        AbilityCost::ExileMaterials { materials, .. }
        | AbilityCost::ExileWithAggregate {
            filter: materials, ..
        }
        | AbilityCost::TapCreatures {
            filter: materials, ..
        }
        | AbilityCost::UnattachFrom {
            filter: materials, ..
        } => validate_target_filter(materials, &format!("{path}.filter"))?,
        AbilityCost::Behold {
            filter,
            type_choice,
            ..
        } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            if let Some(choice) = type_choice {
                validate_choice_type(choice, &format!("{path}.type_choice"))?;
            }
        }
        AbilityCost::Mana { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::Exile { filter: None, .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::RemoveCounter { target: None, .. }
        | AbilityCost::ReturnToHand { filter: None, .. }
        | AbilityCost::Unattach
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { filter: None, .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::Unimplemented { .. } => {}
    }
    Ok(())
}

fn validate_keyword(keyword: &Keyword, path: &str) -> Result<(), String> {
    use crate::types::keywords::{
        BestowCost, BuybackCost, CyclingCost, EchoCost, EmbalmCost, EscapeCost, EternalizeCost,
        EvokeCost, FlashbackCost,
    };
    let cost = match keyword {
        Keyword::Bestow(BestowCost::NonMana(cost))
        | Keyword::Buyback(BuybackCost::NonMana(cost))
        | Keyword::Cycling(CyclingCost::NonMana(cost))
        | Keyword::Echo(EchoCost::NonMana(cost))
        | Keyword::Embalm(EmbalmCost::NonMana(cost))
        | Keyword::Escape(EscapeCost::NonMana(cost))
        | Keyword::Eternalize(EternalizeCost::NonMana(cost))
        | Keyword::Evoke(EvokeCost::NonMana(cost))
        | Keyword::Flashback(FlashbackCost::NonMana(cost))
        | Keyword::CumulativeUpkeep(cost)
        | Keyword::Escalate(cost) => Some(cost),
        Keyword::Crew {
            once_per_turn: Some(restriction),
            ..
        } => {
            validate_activation_restriction(restriction, &format!("{path}.once_per_turn"))?;
            None
        }
        Keyword::Enchant(filter)
        | Keyword::Craft {
            materials: filter, ..
        } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            None
        }
        Keyword::Affinity(filter) => {
            validate_typed_filter(filter, &format!("{path}.filter"))?;
            None
        }
        Keyword::Ward(cost) => {
            validate_ward_cost(cost, &format!("{path}.cost"))?;
            None
        }
        Keyword::Mobilize(quantity) | Keyword::Firebending(quantity) => {
            validate_quantity_expr(quantity, &format!("{path}.quantity"))?;
            None
        }
        Keyword::Bestow(BestowCost::Mana(_))
        | Keyword::Buyback(BuybackCost::Mana(_))
        | Keyword::Cycling(CyclingCost::Mana(_))
        | Keyword::Echo(EchoCost::Mana(_))
        | Keyword::Embalm(EmbalmCost::Mana(_))
        | Keyword::Escape(EscapeCost::Mana(_))
        | Keyword::Eternalize(EternalizeCost::Mana(_))
        | Keyword::Evoke(EvokeCost::Mana(_))
        | Keyword::Flashback(FlashbackCost::Mana(_))
        | Keyword::Crew { .. }
        | Keyword::Flying
        | Keyword::FirstStrike
        | Keyword::DoubleStrike
        | Keyword::Trample
        | Keyword::TrampleOverPlaneswalkers
        | Keyword::Deathtouch
        | Keyword::Lifelink
        | Keyword::Vigilance
        | Keyword::Haste
        | Keyword::Reach
        | Keyword::Defender
        | Keyword::Menace
        | Keyword::Indestructible
        | Keyword::Hexproof
        | Keyword::HexproofFrom(_)
        | Keyword::Shroud
        | Keyword::Flash
        | Keyword::Fear
        | Keyword::Intimidate
        | Keyword::Skulk
        | Keyword::Shadow
        | Keyword::Horsemanship
        | Keyword::Wither
        | Keyword::Infect
        | Keyword::Afflict(_)
        | Keyword::StartingIntensity(_)
        | Keyword::Prowess
        | Keyword::Undying
        | Keyword::Persist
        | Keyword::Cascade
        | Keyword::Exalted
        | Keyword::Flanking
        | Keyword::Evolve
        | Keyword::Extort
        | Keyword::Exploit
        | Keyword::Explore
        | Keyword::Ascend
        | Keyword::StartYourEngines
        | Keyword::Dredge(_)
        | Keyword::Modular(_)
        | Keyword::Renown(_)
        | Keyword::Fabricate(_)
        | Keyword::Annihilator(_)
        | Keyword::Bushido(_)
        | Keyword::Frenzy(_)
        | Keyword::Tribute(_)
        | Keyword::Soulbond
        | Keyword::Unearth(_)
        | Keyword::Convoke
        | Keyword::Waterbend
        | Keyword::Delve
        | Keyword::Devoid
        | Keyword::Changeling
        | Keyword::Phasing
        | Keyword::Battlecry
        | Keyword::Decayed
        | Keyword::Unleash
        | Keyword::Riot
        | Keyword::Afterlife(_)
        | Keyword::EtbCounter { .. }
        | Keyword::Reconfigure(_)
        | Keyword::LivingWeapon
        | Keyword::JobSelect
        | Keyword::TotemArmor
        | Keyword::Fading(_)
        | Keyword::Vanishing(_)
        | Keyword::Protection(_)
        | Keyword::Kicker(_)
        | Keyword::Equip(_)
        | Keyword::Landwalk(_)
        | Keyword::Rampage(_)
        | Keyword::Absorb(_)
        | Keyword::Partner(_)
        | Keyword::Companion(_)
        | Keyword::Ninjutsu(_)
        | Keyword::CommanderNinjutsu(_)
        | Keyword::Prowl(_)
        | Keyword::Morph(_)
        | Keyword::Megamorph(_)
        | Keyword::Mayhem(_)
        | Keyword::Madness(_)
        | Keyword::Miracle(_)
        | Keyword::Dash(_)
        | Keyword::Emerge(_)
        | Keyword::Harmonize(_)
        | Keyword::Foretell(_)
        | Keyword::Mutate(_)
        | Keyword::Disturb(_)
        | Keyword::Disguise(_)
        | Keyword::Blitz(_)
        | Keyword::Overload(_)
        | Keyword::Spectacle(_)
        | Keyword::Surge(_)
        | Keyword::Encore(_)
        | Keyword::Casualty(_)
        | Keyword::Entwine(_)
        | Keyword::Outlast(_)
        | Keyword::Scavenge(_)
        | Keyword::Reinforce { .. }
        | Keyword::Fortify(_)
        | Keyword::Prototype { .. }
        | Keyword::Plot(_)
        | Keyword::Offspring(_)
        | Keyword::Impending { .. }
        | Keyword::LevelUp(_)
        | Keyword::Banding
        | Keyword::BandsWithOther(_)
        | Keyword::Epic
        | Keyword::Fuse
        | Keyword::Gravestorm
        | Keyword::Haunt
        | Keyword::Hideaway(_)
        | Keyword::Improvise
        | Keyword::Ingest
        | Keyword::Melee
        | Keyword::Mentor
        | Keyword::Myriad
        | Keyword::Provoke
        | Keyword::Rebound
        | Keyword::Retrace
        | Keyword::Ripple(_)
        | Keyword::SplitSecond
        | Keyword::Storm
        | Keyword::Suspend { .. }
        | Keyword::Totem
        | Keyword::Warp(_)
        | Keyword::Sneak(_)
        | Keyword::WebSlinging(_)
        | Keyword::Gift(_)
        | Keyword::Discover(_)
        | Keyword::Spree
        | Keyword::Ravenous
        | Keyword::Daybound
        | Keyword::Nightbound
        | Keyword::Enlist
        | Keyword::ReadAhead
        | Keyword::Compleated
        | Keyword::Conspire
        | Keyword::Demonstrate
        | Keyword::Dethrone
        | Keyword::DoubleTeam
        | Keyword::LivingMetal
        | Keyword::Poisonous(_)
        | Keyword::Bloodthirst(_)
        | Keyword::Amplify(_)
        | Keyword::Graft(_)
        | Keyword::Devour(_)
        | Keyword::Toxic(_)
        | Keyword::Saddle(_)
        | Keyword::Teamwork(_)
        | Keyword::Soulshift(_)
        | Keyword::Backup(_)
        | Keyword::Squad(_)
        | Keyword::Typecycling { .. }
        | Keyword::Splice { .. }
        | Keyword::Bargain
        | Keyword::Sunburst
        | Keyword::Champion(_)
        | Keyword::Training
        | Keyword::Assist
        | Keyword::Augment
        | Keyword::Aftermath
        | Keyword::JumpStart
        | Keyword::Cipher
        | Keyword::Transmute(_)
        | Keyword::Transfigure(_)
        | Keyword::Recover(_)
        | Keyword::Cleave(_)
        | Keyword::Undaunted
        | Keyword::Paradigm
        | Keyword::Station
        | Keyword::Replicate(_)
        | Keyword::Awaken { .. }
        | Keyword::ForMirrodin
        | Keyword::MoreThanMeetsTheEye(_)
        | Keyword::Freerunning(_)
        | Keyword::Increment
        | Keyword::Specialize(_)
        | Keyword::Offering(_)
        | Keyword::Unknown(_) => None,
    };
    if let Some(cost) = cost {
        validate_cost(cost, &format!("{path}.cost"))?;
    }
    Ok(())
}

fn validate_ward_cost(cost: &crate::types::keywords::WardCost, path: &str) -> Result<(), String> {
    use crate::types::keywords::WardCost;

    match cost {
        WardCost::Sacrifice { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        WardCost::Compound(costs) => {
            for (index, cost) in costs.iter().enumerate() {
                validate_ward_cost(cost, &format!("{path}.costs[{index}]"))?;
            }
        }
        WardCost::Mana(_)
        | WardCost::PayLife(_)
        | WardCost::DiscardCard
        | WardCost::Waterbend(_) => {}
    }
    Ok(())
}

fn validate_activation_restriction(
    restriction: &crate::types::ability::ActivationRestriction,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::ActivationRestriction;

    match restriction {
        ActivationRestriction::RequiresCondition {
            condition: Some(condition),
        } => validate_parsed_condition(condition, &format!("{path}.condition"))?,
        ActivationRestriction::AsSorcery
        | ActivationRestriction::AsInstant
        | ActivationRestriction::DuringYourTurn
        | ActivationRestriction::DuringYourUpkeep
        | ActivationRestriction::DuringCombat
        | ActivationRestriction::BeforeAttackersDeclared
        | ActivationRestriction::BeforeCombatDamage
        | ActivationRestriction::OnlyOnceEachTurn
        | ActivationRestriction::OnlyOnce
        | ActivationRestriction::MaxTimesEachTurn { .. }
        | ActivationRestriction::RequiresCondition { condition: None }
        | ActivationRestriction::IsSolved
        | ActivationRestriction::SourceIsHarnessed
        | ActivationRestriction::ClassLevelIs { .. }
        | ActivationRestriction::LevelCounterRange { .. }
        | ActivationRestriction::CounterThreshold { .. }
        | ActivationRestriction::MatchesCardCastTiming => {}
    }
    Ok(())
}

fn validate_choice_type(
    choice: &crate::types::ability::ChoiceType,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::ChoiceType;

    match choice {
        ChoiceType::Keyword { options, .. } => {
            for (index, keyword) in options.iter().enumerate() {
                validate_keyword(keyword, &format!("{path}.options[{index}]"))?;
            }
        }
        ChoiceType::Opponent {
            restriction: Some(restriction),
        } => validate_player_filter(restriction, &format!("{path}.restriction"))?,
        ChoiceType::CreatureType { .. }
        | ChoiceType::Color { .. }
        | ChoiceType::OddOrEven
        | ChoiceType::BasicLandType
        | ChoiceType::CardType { .. }
        | ChoiceType::CardName
        | ChoiceType::NumberRange { .. }
        | ChoiceType::Labeled { .. }
        | ChoiceType::LandType
        | ChoiceType::CardPredicate { .. }
        | ChoiceType::CardPredicateGuess { .. }
        | ChoiceType::Opponent { restriction: None }
        | ChoiceType::Player
        | ChoiceType::TwoColors
        | ChoiceType::Word
        | ChoiceType::Artist
        | ChoiceType::CounterKind { .. } => {}
    }
    Ok(())
}

fn validate_typed_filter(
    filter: &crate::types::ability::TypedFilter,
    path: &str,
) -> Result<(), String> {
    for (index, property) in filter.properties.iter().enumerate() {
        validate_filter_prop(property, &format!("{path}.properties[{index}]"))?;
    }
    Ok(())
}

fn validate_target_filter(
    filter: &crate::types::ability::TargetFilter,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::TargetFilter;

    match filter {
        TargetFilter::Typed(filter) => validate_typed_filter(filter, path)?,
        TargetFilter::Not { filter } => validate_target_filter(filter, &format!("{path}.filter"))?,
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            for (index, filter) in filters.iter().enumerate() {
                validate_target_filter(filter, &format!("{path}.filters[{index}]"))?;
            }
        }
        TargetFilter::TrackedSetFiltered { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        TargetFilter::ChosenDamageSource {
            filter: Some(filter),
        } => validate_target_filter(filter, &format!("{path}.filter"))?,
        TargetFilter::None
        | TargetFilter::Any
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SelfRef
        | TargetFilter::GrantingObject
        | TargetFilter::SourceOrPaired
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::AttachedTo
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::CostPaidObject
        | TargetFilter::ChosenCard
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSource
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::OriginalSource
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::HasChosenName
        | TargetFilter::ChosenDamageSource { filter: None }
        | TargetFilter::Named { .. }
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => {}
    }
    Ok(())
}

fn validate_filter_prop(
    property: &crate::types::ability::FilterProp,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::FilterProp;

    match property {
        FilterProp::Counters { count, .. } => {
            validate_quantity_expr(count, &format!("{path}.count"))?
        }
        FilterProp::Cmc { value, .. } | FilterProp::PtComparison { value, .. } => {
            validate_quantity_expr(value, &format!("{path}.value"))?
        }
        FilterProp::ControllerMatches { player } => validate_player_filter(player, path)?,
        FilterProp::WithKeyword { value } | FilterProp::WithoutKeyword { value } => {
            validate_keyword(value, &format!("{path}.keyword"))?
        }
        FilterProp::CanEnchant { target }
        | FilterProp::DifferentNameFrom { filter: target }
        | FilterProp::DistinctFrom { reference: target }
        | FilterProp::TargetsOnly { filter: target }
        | FilterProp::Targets { filter: target } => {
            validate_target_filter(target, &format!("{path}.filter"))?
        }
        FilterProp::AnyOf { props } => {
            for (index, property) in props.iter().enumerate() {
                validate_filter_prop(property, &format!("{path}.props[{index}]"))?;
            }
        }
        FilterProp::Not { prop } => validate_filter_prop(prop, &format!("{path}.prop"))?,
        FilterProp::SharesQuality {
            reference: Some(reference),
            ..
        } => validate_target_filter(reference, &format!("{path}.reference"))?,
        FilterProp::Token
        | FilterProp::NonToken
        | FilterProp::RepresentedByCard
        | FilterProp::ControllerChoseLabel { .. }
        | FilterProp::WasPlayed
        | FilterProp::Attacking { .. }
        | FilterProp::Blocking
        | FilterProp::BlockingSource
        | FilterProp::CombatRelation { .. }
        | FilterProp::Unblocked
        | FilterProp::AttackingAlone
        | FilterProp::BlockingAlone
        | FilterProp::Tapped
        | FilterProp::Untapped
        | FilterProp::IsSaddled
        | FilterProp::SaddledSource
        | FilterProp::ConvokedSource
        | FilterProp::ProtectorMatches { .. }
        | FilterProp::HasHasteOrControlledSinceTurnBegan
        | FilterProp::HasKeywordKind { .. }
        | FilterProp::WithoutKeywordKind { .. }
        | FilterProp::ManaValueParity { .. }
        | FilterProp::ManaCostIn { .. }
        | FilterProp::InZone { .. }
        | FilterProp::Owned { .. }
        | FilterProp::Foretold
        | FilterProp::EnchantedBy
        | FilterProp::EquippedBy
        | FilterProp::AttachedToSource
        | FilterProp::AttachedToRecipient
        | FilterProp::HasAttachment { .. }
        | FilterProp::HasAnyAttachmentOf { .. }
        | FilterProp::Another
        | FilterProp::Unpaired
        | FilterProp::OtherThanTriggerObject
        | FilterProp::HasColor { .. }
        | FilterProp::PowerGTSource
        | FilterProp::ColorCount { .. }
        | FilterProp::ManaSymbolCount { .. }
        | FilterProp::HasSupertype { .. }
        | FilterProp::IsChosenCreatureType
        | FilterProp::MostPrevalentCreatureTypeIn { .. }
        | FilterProp::IsChosenColor
        | FilterProp::IsChosenCardType
        | FilterProp::MatchesLastChosenCardPredicate
        | FilterProp::HasSingleTarget
        | FilterProp::Modal
        | FilterProp::NotColor { .. }
        | FilterProp::NotSupertype { .. }
        | FilterProp::Suspected
        | FilterProp::Renowned
        | FilterProp::ToughnessGTPower
        | FilterProp::PowerExceedsBase
        | FilterProp::InTrackedSet { .. }
        | FilterProp::Modified
        | FilterProp::Historic
        | FilterProp::NotHistoric
        | FilterProp::InAnyZone { .. }
        | FilterProp::SharesQuality {
            reference: None, ..
        }
        | FilterProp::WasDealtDamageThisTurn
        | FilterProp::EnteredThisTurn
        | FilterProp::ControlledContinuouslySinceTurnBegan
        | FilterProp::ZoneChangedThisTurn { .. }
        | FilterProp::AttackedThisTurn { .. }
        | FilterProp::BlockedThisTurn
        | FilterProp::AttackedOrBlockedThisTurn
        | FilterProp::CountersPutOnThisTurn { .. }
        | FilterProp::FaceDown
        | FilterProp::Transformed
        | FilterProp::CouldBeTargetedByTriggeringSpell
        | FilterProp::HasXInManaCost
        | FilterProp::HasXInActivationCost
        | FilterProp::WasKicked
        | FilterProp::HasManaAbility
        | FilterProp::HasNoAbilities
        | FilterProp::Named { .. }
        | FilterProp::SameName
        | FilterProp::SameNameAsParentTarget
        | FilterProp::NameMatchesAnyPermanent { .. }
        | FilterProp::IsCommander
        | FilterProp::SharesCreatureTypeWithCommander
        | FilterProp::Other { .. } => {}
    }
    Ok(())
}

fn validate_player_filter(
    filter: &crate::types::ability::PlayerFilter,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::PlayerFilter;

    match filter {
        PlayerFilter::OpponentDealtDamage {
            source: Some(source),
            ..
        } => validate_target_filter(source, &format!("{path}.source"))?,
        PlayerFilter::AllExcept { exclude } => {
            validate_player_filter(exclude, &format!("{path}.exclude"))?
        }
        PlayerFilter::ControlsCount { filter, count, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
        }
        PlayerFilter::PlayerAttribute { attr, value, .. } => {
            validate_quantity_ref(attr, &format!("{path}.attr"))?;
            validate_quantity_expr(value, &format!("{path}.value"))?;
        }
        PlayerFilter::Controller
        | PlayerFilter::Opponent
        | PlayerFilter::DefendingPlayer
        | PlayerFilter::OpponentLostLife
        | PlayerFilter::OpponentGainedLife
        | PlayerFilter::HasLostTheGame
        | PlayerFilter::OpponentDealtDamage { source: None, .. }
        | PlayerFilter::OpponentAttacked { .. }
        | PlayerFilter::OpponentAttackingEnchantedPlayer
        | PlayerFilter::All
        | PlayerFilter::HighestSpeed
        | PlayerFilter::ZoneChangedThisWay
        | PlayerFilter::PerformedActionThisWay { .. }
        | PlayerFilter::OwnersOfCardsExiledBySource
        | PlayerFilter::TriggeringPlayer
        | PlayerFilter::OpponentOtherThanTriggering
        | PlayerFilter::OpponentOfTriggeringPlayer
        | PlayerFilter::OpponentOfTriggeringPlayerNotAttacked
        | PlayerFilter::VotedFor { .. }
        | PlayerFilter::ParentObjectTargetController
        | PlayerFilter::ChosenPlayer { .. }
        | PlayerFilter::ParentObjectTargetOwner => {}
    }
    Ok(())
}

fn validate_static_condition(
    condition: &crate::types::ability::StaticCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::StaticCondition;

    match condition {
        StaticCondition::QuantityComparison { lhs, rhs, .. } => {
            validate_quantity_expr(lhs, &format!("{path}.lhs"))?;
            validate_quantity_expr(rhs, &format!("{path}.rhs"))?;
        }
        StaticCondition::IsPresent {
            filter: Some(filter),
            ..
        } => validate_target_filter(filter, &format!("{path}.filter"))?,
        StaticCondition::And { conditions } | StaticCondition::Or { conditions } => {
            for (index, condition) in conditions.iter().enumerate() {
                validate_static_condition(condition, &format!("{path}.conditions[{index}]"))?;
            }
        }
        StaticCondition::Not { condition } => {
            validate_static_condition(condition, &format!("{path}.condition"))?
        }
        StaticCondition::DefendingPlayerControls { filter }
        | StaticCondition::SourceMatchesFilter { filter }
        | StaticCondition::TopOfLibraryMatches { filter }
        | StaticCondition::RecipientMatchesFilter { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        StaticCondition::UnlessPay { scaling, .. } => {
            validate_unless_pay_scaling(scaling, &format!("{path}.scaling"))?
        }
        StaticCondition::DevotionGE { .. }
        | StaticCondition::IsPresent { filter: None, .. }
        | StaticCondition::ChosenColorIs { .. }
        | StaticCondition::ChosenLabelIs { .. }
        | StaticCondition::HasMaxSpeed
        | StaticCondition::SpeedGE { .. }
        | StaticCondition::DayNightIs { .. }
        | StaticCondition::HasCounters { .. }
        | StaticCondition::CastVariantPaid { .. }
        | StaticCondition::RecipientHasCounters { .. }
        | StaticCondition::ClassLevelGE { .. }
        | StaticCondition::SourceAttackingAlone
        | StaticCondition::SourceIsAttacking
        | StaticCondition::SourceIsBlocking
        | StaticCondition::SourceIsBlocked
        | StaticCondition::IsMonarch
        | StaticCondition::IsInitiative
        | StaticCondition::NoMonarch
        | StaticCondition::HasCityBlessing
        | StaticCondition::CompletedADungeon
        | StaticCondition::WasStartingPlayer { .. }
        | StaticCondition::SpellCastWithVariantThisTurn { .. }
        | StaticCondition::OpponentPoisonAtLeast { .. }
        | StaticCondition::Unrecognized { .. }
        | StaticCondition::DuringYourTurn
        | StaticCondition::SharesColorWithMostCommonColorAmongPermanents
        | StaticCondition::SourceEnteredThisTurn
        | StaticCondition::SourceHasDealtDamage
        | StaticCondition::WasCast { .. }
        | StaticCondition::IsRingBearer
        | StaticCondition::RingLevelAtLeast { .. }
        | StaticCondition::ControlsCommander { .. }
        | StaticCondition::SourceIsTapped
        | StaticCondition::IsTapped { .. }
        | StaticCondition::SourceIsFaceUp
        | StaticCondition::SourceIsSaddled
        | StaticCondition::SourceControllerEquals { .. }
        | StaticCondition::SourceIsEquipped
        | StaticCondition::SourceIsEnchanted
        | StaticCondition::SourceIsMonstrous
        | StaticCondition::SourceIsHarnessed
        | StaticCondition::SourceAttachedToCreature
        | StaticCondition::RecipientAttackingOwnerTarget { .. }
        | StaticCondition::SourceIsPaired
        | StaticCondition::SourceInZone { .. }
        | StaticCondition::EnchantedIsFaceDown
        | StaticCondition::AdditionalCostPaid
        | StaticCondition::CastingAsVariant { .. }
        | StaticCondition::None => {}
    }
    Ok(())
}

fn validate_parsed_condition(
    condition: &crate::types::ability::ParsedCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::ParsedCondition;

    match condition {
        ParsedCondition::QuantityVsEachOpponent { lhs, rhs, .. } => {
            validate_quantity_ref(lhs, &format!("{path}.lhs"))?;
            validate_quantity_ref(rhs, &format!("{path}.rhs"))?;
        }
        ParsedCondition::QuantityComparison { lhs, rhs, .. } => {
            validate_quantity_expr(lhs, &format!("{path}.lhs"))?;
            validate_quantity_expr(rhs, &format!("{path}.rhs"))?;
        }
        ParsedCondition::SourceLacksKeyword { keyword }
        | ParsedCondition::ControlsCreatureWithKeyword { keyword, .. } => {
            validate_keyword(keyword, &format!("{path}.keyword"))?
        }
        ParsedCondition::YouAttackedWithAtLeast {
            filter: Some(filter),
            ..
        }
        | ParsedCondition::YouCastSpellThisTurn {
            filter: Some(filter),
            ..
        } => validate_target_filter(filter, &format!("{path}.filter"))?,
        ParsedCondition::BattlefieldEntriesThisTurn { filter, count: _ }
        | ParsedCondition::SpellTargetsFilter { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        ParsedCondition::PlayerCountAtLeast { filter, .. } => {
            validate_player_filter(filter, &format!("{path}.filter"))?
        }
        ParsedCondition::And { conditions } | ParsedCondition::Or { conditions } => {
            for (index, condition) in conditions.iter().enumerate() {
                validate_parsed_condition(condition, &format!("{path}.conditions[{index}]"))?;
            }
        }
        ParsedCondition::Not { condition } => {
            validate_parsed_condition(condition, &format!("{path}.condition"))?
        }
        ParsedCondition::SourceInZone { .. }
        | ParsedCondition::SourceIsAttacking
        | ParsedCondition::SourceIsAttackingOrBlocking
        | ParsedCondition::SourceIsBlocked
        | ParsedCondition::SourcePowerAtLeast { .. }
        | ParsedCondition::SourceHasCounterAtLeast { .. }
        | ParsedCondition::SourceHasNoCounter { .. }
        | ParsedCondition::SourceEnteredThisTurn
        | ParsedCondition::SourceAttackedThisTurn
        | ParsedCondition::SourceIsCreature
        | ParsedCondition::SourceAttachedTo { .. }
        | ParsedCondition::SourceUntappedAttachedTo { .. }
        | ParsedCondition::SourceIsColor { .. }
        | ParsedCondition::FirstSpellThisGame
        | ParsedCondition::OpponentSearchedLibraryThisTurn
        | ParsedCondition::BeenAttackedThisStep
        | ParsedCondition::ZoneCardCountAtLeast { .. }
        | ParsedCondition::ZoneCardTypeCountAtLeast { .. }
        | ParsedCondition::ZoneCoreTypeCardCountAtLeast { .. }
        | ParsedCondition::ZoneSubtypeCardCountAtLeast { .. }
        | ParsedCondition::OpponentPoisonAtLeast { .. }
        | ParsedCondition::HandSizeExact { .. }
        | ParsedCondition::HandSizeOneOf { .. }
        | ParsedCondition::CreaturesYouControlTotalPowerAtLeast { .. }
        | ParsedCondition::YouControlLandSubtypeAny { .. }
        | ParsedCondition::YouControlSubtypeCountAtLeast { .. }
        | ParsedCondition::YouControlCoreTypeCountAtLeast { .. }
        | ParsedCondition::YouControlColorPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlSubtypeOrGraveyardCardSubtype { .. }
        | ParsedCondition::YouControlLegendaryCreature
        | ParsedCondition::YouControlNamedPlaneswalker { .. }
        | ParsedCondition::YouControlCreatureWithPowerAtLeast { .. }
        | ParsedCondition::YouControlCreatureWithPt { .. }
        | ParsedCondition::YouControlAnotherColorlessCreature
        | ParsedCondition::YouControlSnowPermanentCountAtLeast { .. }
        | ParsedCondition::YouControlDifferentPowerCreatureCountAtLeast { .. }
        | ParsedCondition::YouControlLandsWithSameNameAtLeast { .. }
        | ParsedCondition::YouControlNoCreatures
        | ParsedCondition::YouAttackedThisTurn
        | ParsedCondition::YouAttackedSourceControllerThisTurn
        | ParsedCondition::YouAttackedWithAtLeast { filter: None, .. }
        | ParsedCondition::YouPlayedLandThisTurn
        | ParsedCondition::YouCastSpellThisTurn { filter: None, .. }
        | ParsedCondition::YouCastNoncreatureSpellThisTurn
        | ParsedCondition::YouCastSpellCountAtLeast { .. }
        | ParsedCondition::YouGainedLifeThisTurn
        | ParsedCondition::YouCreatedTokenThisTurn
        | ParsedCondition::YouDiscardedCardThisTurn
        | ParsedCondition::YouSacrificedArtifactThisTurn
        | ParsedCondition::CreatureDiedThisTurn
        | ParsedCondition::YouHadCreatureEnterThisTurn
        | ParsedCondition::YouHadAngelOrBerserkerEnterThisTurn
        | ParsedCondition::YouHadArtifactEnterThisTurn
        | ParsedCondition::CardsLeftYourGraveyardThisTurnAtLeast { .. }
        | ParsedCondition::HasCityBlessing
        | ParsedCondition::IsYourTurn => {}
    }
    Ok(())
}

fn validate_quantity_expr(
    quantity: &crate::types::ability::QuantityExpr,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::QuantityExpr;

    match quantity {
        QuantityExpr::Ref { qty } => validate_quantity_ref(qty, &format!("{path}.qty"))?,
        QuantityExpr::DivideRounded { inner, .. }
        | QuantityExpr::Offset { inner, .. }
        | QuantityExpr::ClampMin { inner, .. }
        | QuantityExpr::Multiply { inner, .. }
        | QuantityExpr::UpTo { max: inner } => {
            validate_quantity_expr(inner, &format!("{path}.inner"))?
        }
        QuantityExpr::Power { exponent, .. } => {
            validate_quantity_expr(exponent, &format!("{path}.exponent"))?
        }
        QuantityExpr::Difference { left, right } => {
            validate_quantity_expr(left, &format!("{path}.left"))?;
            validate_quantity_expr(right, &format!("{path}.right"))?;
        }
        QuantityExpr::Sum { exprs } | QuantityExpr::Max { exprs } => {
            for (index, expr) in exprs.iter().enumerate() {
                validate_quantity_expr(expr, &format!("{path}.exprs[{index}]"))?;
            }
        }
        QuantityExpr::Fixed { .. } => {}
    }
    Ok(())
}

fn validate_pt_value(value: &crate::types::ability::PtValue, path: &str) -> Result<(), String> {
    match value {
        crate::types::ability::PtValue::Quantity(quantity) => {
            validate_quantity_expr(quantity, path)?
        }
        crate::types::ability::PtValue::Fixed(_) | crate::types::ability::PtValue::Variable(_) => {}
    }
    Ok(())
}

fn validate_mana_production(
    produced: &crate::types::ability::ManaProduction,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::ManaProduction;

    match produced {
        ManaProduction::Colorless { count }
        | ManaProduction::AnyOneColor { count, .. }
        | ManaProduction::AnyCombination { count, .. }
        | ManaProduction::ChosenColor { count, .. }
        | ManaProduction::OpponentLandColors { count }
        | ManaProduction::AnyCombinationOfObjectColors { count, .. }
        | ManaProduction::AnyInCommandersColorIdentity { count, .. } => {
            validate_quantity_expr(count, &format!("{path}.count"))?
        }
        ManaProduction::AnyTypeProduceableBy { count, land_filter }
        | ManaProduction::AnyOneColorAmongPermanents {
            count,
            filter: land_filter,
            ..
        } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(land_filter, &format!("{path}.filter"))?;
        }
        ManaProduction::DistinctColorsAmongPermanents { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        ManaProduction::Fixed { .. }
        | ManaProduction::Mixed { .. }
        | ManaProduction::ChoiceAmongExiledColors { .. }
        | ManaProduction::ChoiceAmongCombinations { .. }
        | ManaProduction::TriggerEventManaType => {}
    }
    Ok(())
}

fn validate_game_restriction(
    restriction: &crate::types::ability::GameRestriction,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::{GameRestriction, ProhibitedActivity};

    match restriction {
        GameRestriction::CantEnterBattlefieldFrom { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        GameRestriction::ProhibitActivity {
            activity: ProhibitedActivity::CastSpells { spell_filter },
            ..
        } => {
            if let Some(filter) = spell_filter {
                validate_target_filter(filter, &format!("{path}.activity.spell_filter"))?;
            }
        }
        GameRestriction::DamagePreventionDisabled { .. }
        | GameRestriction::ProhibitActivity { .. } => {}
    }
    Ok(())
}

fn validate_delayed_trigger_condition(
    condition: &crate::types::ability::DelayedTriggerCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::DelayedTriggerCondition;

    match condition {
        DelayedTriggerCondition::WhenDies { filter }
        | DelayedTriggerCondition::WhenLeavesPlayFiltered { filter }
        | DelayedTriggerCondition::WhenEntersBattlefield { filter }
        | DelayedTriggerCondition::WhenDiesOrExiled { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        DelayedTriggerCondition::WheneverEvent { trigger } => {
            validate_trigger(trigger, &format!("{path}.trigger"))?
        }
        DelayedTriggerCondition::WhenNextEvent {
            trigger,
            or_trigger,
            ..
        } => {
            validate_trigger(trigger, &format!("{path}.trigger"))?;
            if let Some(trigger) = or_trigger {
                validate_trigger(trigger, &format!("{path}.or_trigger"))?;
            }
        }
        DelayedTriggerCondition::AtNextPhase { .. }
        | DelayedTriggerCondition::AtNextPhaseForPlayer { .. }
        | DelayedTriggerCondition::WhenLeavesPlay { .. } => {}
    }
    Ok(())
}

fn validate_quantity_ref(
    quantity: &crate::types::ability::QuantityRef,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::{CardTypeSetSource, CastManaSpentMetric, QuantityRef};

    match quantity {
        QuantityRef::ObjectCount { filter }
        | QuantityRef::ObjectCountDistinct { filter, .. }
        | QuantityRef::ObjectCountBySharedQuality { filter, .. }
        | QuantityRef::CountersOnObjects { filter, .. }
        | QuantityRef::Aggregate { filter, .. }
        | QuantityRef::ControlledByEachPlayer { filter, .. }
        | QuantityRef::EnteredThisTurn { filter }
        | QuantityRef::SacrificedThisTurn { filter, .. }
        | QuantityRef::BattlefieldEntriesThisTurn { filter, .. }
        | QuantityRef::ZoneChangeCountThisTurn { filter, .. }
        | QuantityRef::ZoneChangeAggregateThisTurn { filter, .. }
        | QuantityRef::TokensCreatedThisTurn { filter, .. }
        | QuantityRef::DistinctColorsAmongPermanents { filter }
        | QuantityRef::DistinctCounterKindsAmong { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        QuantityRef::TargetObjectManaValue { filter }
        | QuantityRef::FilteredTrackedSetSize { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        QuantityRef::PlayerCount { filter } => {
            validate_player_filter(filter, &format!("{path}.filter"))?
        }
        QuantityRef::ZoneCardCount {
            filter: Some(filter),
            ..
        }
        | QuantityRef::SpellsCastThisTurn {
            filter: Some(filter),
            ..
        }
        | QuantityRef::AttackedThisTurn {
            filter: Some(filter),
            ..
        }
        | QuantityRef::SpellsCastThisGame {
            filter: Some(filter),
            ..
        } => validate_target_filter(filter, &format!("{path}.filter"))?,
        QuantityRef::DamageDealtThisTurn { source, target, .. } => {
            validate_target_filter(source, &format!("{path}.source"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        QuantityRef::CounterAddedThisTurn { target, .. } => {
            validate_target_filter(target, &format!("{path}.target"))?
        }
        QuantityRef::DistinctCardTypes { source }
        | QuantityRef::DistinctSubtypes { source, .. } => {
            if let CardTypeSetSource::Objects { filter } = source {
                validate_target_filter(filter, &format!("{path}.source.filter"))?;
            }
        }
        QuantityRef::ManaSpentToCast {
            metric: CastManaSpentMetric::FromSource { source_filter },
            ..
        } => validate_target_filter(source_filter, &format!("{path}.metric.source_filter"))?,
        QuantityRef::HandSize { .. }
        | QuantityRef::LifeTotal { .. }
        | QuantityRef::GraveyardSize { .. }
        | QuantityRef::LifeAboveStarting
        | QuantityRef::StartingLifeTotal
        | QuantityRef::TriggeringDiscoverValue
        | QuantityRef::CountersOn { .. }
        | QuantityRef::PlayerCounter { .. }
        | QuantityRef::TargetControllerCounter { .. }
        | QuantityRef::Variable { .. }
        | QuantityRef::Power { .. }
        | QuantityRef::Intensity { .. }
        | QuantityRef::Toughness { .. }
        | QuantityRef::ObjectManaValue { .. }
        | QuantityRef::ObjectColorCount { .. }
        | QuantityRef::ObjectNameWordCount { .. }
        | QuantityRef::ObjectTypelineComponentCount { .. }
        | QuantityRef::ManaSymbolsInManaCost { .. }
        | QuantityRef::SelfManaValue
        | QuantityRef::TargetZoneCardCount { .. }
        | QuantityRef::Devotion { .. }
        | QuantityRef::CardsExiledBySource
        | QuantityRef::ExiledCardPower { .. }
        | QuantityRef::ZoneCardCount { filter: None, .. }
        | QuantityRef::BasicLandTypeCount { .. }
        | QuantityRef::TrackedSetSize
        | QuantityRef::TrackedSetAggregate { .. }
        | QuantityRef::ExiledFromHandThisResolution
        | QuantityRef::PreviousEffectAmount { .. }
        | QuantityRef::LifeLostThisTurn { .. }
        | QuantityRef::PartySize { .. }
        | QuantityRef::UnspentMana { .. }
        | QuantityRef::Speed { .. }
        | QuantityRef::EventContextAmount
        | QuantityRef::AttachmentsOnLeavingObject { .. }
        | QuantityRef::EventContextSourceCostX
        | QuantityRef::SpellsCastThisTurn { filter: None, .. }
        | QuantityRef::CrimesCommittedThisTurn
        | QuantityRef::BendTypesThisTurn
        | QuantityRef::LifeGainedThisTurn { .. }
        | QuantityRef::CardsDrawnThisTurn { .. }
        | QuantityRef::LandsPlayedThisTurn { .. }
        | QuantityRef::TurnsTaken
        | QuantityRef::ChosenNumber
        | QuantityRef::AttackedThisTurn { filter: None, .. }
        | QuantityRef::DescendedThisTurn
        | QuantityRef::LoyaltyAbilitiesActivatedThisTurn { .. }
        | QuantityRef::SpellsCastLastTurn
        | QuantityRef::SpellsCastThisGame { filter: None, .. }
        | QuantityRef::CardsDiscardedThisTurn { .. }
        | QuantityRef::PlayerActionsThisTurn { .. }
        | QuantityRef::DungeonsCompleted
        | QuantityRef::CostXPaid
        | QuantityRef::KickerCount
        | QuantityRef::AdditionalCostPaymentCount
        | QuantityRef::AdditionalCostPaymentCountFor { .. }
        | QuantityRef::ConvokedCreatureCount
        | QuantityRef::TimesCostPaidThisResolution
        | QuantityRef::ManaSpentToCast {
            metric: CastManaSpentMetric::Total | CastManaSpentMetric::DistinctColors,
            ..
        }
        | QuantityRef::ColorsInCommandersColorIdentity
        | QuantityRef::CommanderCastFromCommandZoneCount
        | QuantityRef::CommanderManaValue { .. }
        | QuantityRef::VoteCount { .. } => {}
    }
    Ok(())
}

fn validate_perpetual_modification(
    modification: &crate::types::ability::PerpetualModification,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::PerpetualModification;

    match modification {
        PerpetualModification::GrantKeywords { keywords }
        | PerpetualModification::Become { keywords, .. } => {
            for (index, keyword) in keywords.iter().enumerate() {
                validate_keyword(keyword, &format!("{path}.keywords[{index}]"))?;
            }
        }
        PerpetualModification::SetBasePowerToughness { .. }
        | PerpetualModification::ModifyPowerToughness { .. }
        | PerpetualModification::ModifyCost { .. } => {}
    }
    Ok(())
}

fn validate_duration(duration: &crate::types::ability::Duration, path: &str) -> Result<(), String> {
    use crate::types::ability::Duration;

    match duration {
        Duration::ForAsLongAs { condition } => {
            validate_static_condition(condition, &format!("{path}.condition"))?
        }
        Duration::UntilEndOfTurn
        | Duration::UntilEndOfCombat
        | Duration::UntilHostLeavesPlay
        | Duration::UntilSourceExilesAnotherCard
        | Duration::Permanent
        | Duration::UntilNextTurnOf { .. }
        | Duration::UntilEndOfNextTurnOf { .. }
        | Duration::UntilNextStepOf { .. } => {}
    }
    Ok(())
}

fn validate_damage_modification(
    modification: &crate::types::ability::DamageModification,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::DamageModification;

    match modification {
        DamageModification::Plus { value } => {
            validate_quantity_expr(value, &format!("{path}.value"))?
        }
        DamageModification::Double
        | DamageModification::Triple
        | DamageModification::Minus { value: _ }
        | DamageModification::SetToSourcePower
        | DamageModification::SetTo { value: _ }
        | DamageModification::LifeFloor { minimum: _ } => {}
    }
    Ok(())
}

fn validate_until_condition(
    condition: &crate::types::ability::UntilCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::UntilCondition;

    match condition {
        UntilCondition::NextMatches { filter } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        UntilCondition::CumulativeThreshold {
            property: _,
            comparator: _,
            threshold,
        } => validate_quantity_expr(threshold, &format!("{path}.threshold"))?,
    }
    Ok(())
}

fn validate_library_position(
    position: &crate::types::ability::LibraryPosition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::LibraryPosition;

    match position {
        LibraryPosition::BeneathTop { depth } => {
            validate_quantity_expr(depth, &format!("{path}.depth"))?
        }
        LibraryPosition::RandomWithinTop { n } => validate_quantity_expr(n, &format!("{path}.n"))?,
        LibraryPosition::Top | LibraryPosition::Bottom | LibraryPosition::NthFromTop { n: _ } => {}
    }
    Ok(())
}

fn validate_die_roll_modifier(
    modifier: &crate::types::ability::DieRollModifier,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::DieRollModifier;

    match modifier {
        DieRollModifier::Add { value } | DieRollModifier::Subtract { value } => {
            validate_quantity_expr(value, &format!("{path}.value"))?
        }
    }
    Ok(())
}

fn validate_spell_stack_to_graveyard_replacement(
    replacement: &crate::types::ability::SpellStackToGraveyardReplacement,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::SpellStackToGraveyardReplacement;

    match replacement {
        SpellStackToGraveyardReplacement::Library { position } => {
            validate_library_position(position, &format!("{path}.position"))?
        }
        SpellStackToGraveyardReplacement::Exile | SpellStackToGraveyardReplacement::Hand => {}
    }
    Ok(())
}

fn validate_keeper_constraint(
    constraint: &crate::types::ability::KeeperConstraint,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::KeeperConstraint;

    match constraint {
        KeeperConstraint::ExactCount { count } => {
            validate_quantity_expr(count, &format!("{path}.count"))?
        }
    }
    Ok(())
}

fn validate_ability_condition(
    condition: &crate::types::ability::AbilityCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::AbilityCondition;

    match condition {
        AbilityCondition::RevealedHasCardType {
            additional_filter,
            subtype_filter,
            ..
        } => {
            if let Some(filter) = additional_filter {
                validate_filter_prop(filter, &format!("{path}.additional_filter"))?;
            }
            if let Some(filter) = subtype_filter {
                validate_target_filter(filter, &format!("{path}.subtype_filter"))?;
            }
        }
        AbilityCondition::ObjectsShareQuality {
            subject, reference, ..
        } => {
            validate_target_filter(subject, &format!("{path}.subject"))?;
            validate_target_filter(reference, &format!("{path}.reference"))?;
        }
        AbilityCondition::TargetSharesNameWithOtherExiledThisWay { target }
        | AbilityCondition::TriggeringSpellTargetsFilter { filter: target }
        | AbilityCondition::SourceMatchesFilter { filter: target }
        | AbilityCondition::ZoneChangeObjectMatchesFilter { filter: target, .. }
        | AbilityCondition::ControllerControlsMatching { filter: target }
        | AbilityCondition::ControllerControlledMatchingAsCast { filter: target }
        | AbilityCondition::ZoneChangedThisWay { filter: target }
        | AbilityCondition::CostPaidObjectMatchesFilter { filter: target }
        | AbilityCondition::TargetMatchesFilter { filter: target, .. } => {
            validate_target_filter(target, &format!("{path}.filter"))?
        }
        AbilityCondition::TargetHasKeywordInstead { keyword }
        | AbilityCondition::SourceLacksKeyword { keyword } => {
            validate_keyword(keyword, &format!("{path}.keyword"))?
        }
        AbilityCondition::ConditionInstead { inner } => {
            validate_ability_condition(inner, &format!("{path}.inner"))?
        }
        AbilityCondition::And { conditions } | AbilityCondition::Or { conditions } => {
            for (index, condition) in conditions.iter().enumerate() {
                validate_ability_condition(condition, &format!("{path}.conditions[{index}]"))?;
            }
        }
        AbilityCondition::Not { condition } => {
            validate_ability_condition(condition, &format!("{path}.condition"))?
        }
        AbilityCondition::ScopedPlayerMatches { filter } => {
            validate_player_filter(filter, &format!("{path}.filter"))?
        }
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            validate_quantity_expr(lhs, &format!("{path}.lhs"))?;
            validate_quantity_expr(rhs, &format!("{path}.rhs"))?;
        }
        AbilityCondition::PreviousEffectAmount { rhs, .. } => {
            validate_quantity_expr(rhs, &format!("{path}.rhs"))?
        }
        AbilityCondition::AdditionalCostPaid { .. }
        | AbilityCondition::AdditionalCostPaidInstead
        | AbilityCondition::AlternativeManaCostPaid
        | AbilityCondition::EffectOutcome { .. }
        | AbilityCondition::EventOutcomeWon
        | AbilityCondition::CoinFlipOutcome { .. }
        | AbilityCondition::WhenYouDo
        | AbilityCondition::WasCast { .. }
        | AbilityCondition::CastDuringPhase { .. }
        | AbilityCondition::CurrentPhaseIs { .. }
        | AbilityCondition::CastTimingPermission { .. }
        | AbilityCondition::ManaColorSpent { .. }
        | AbilityCondition::SourceEnteredThisTurn
        | AbilityCondition::CastVariantPaid { .. }
        | AbilityCondition::CastVariantPaidInstead { .. }
        | AbilityCondition::HasMaxSpeed
        | AbilityCondition::IsMonarch
        | AbilityCondition::IsInitiative
        | AbilityCondition::HasCityBlessing
        | AbilityCondition::IsRingBearer
        | AbilityCondition::CompletedDungeon { .. }
        | AbilityCondition::HasObjectTarget
        | AbilityCondition::IsYourTurn
        | AbilityCondition::WasStartingPlayer { .. }
        | AbilityCondition::SpellCastWithVariantThisTurn { .. }
        | AbilityCondition::FirstCombatPhaseOfTurn
        | AbilityCondition::FirstEndStepOfTurn
        | AbilityCondition::SourceIsTapped
        | AbilityCondition::SourceAttachedToCreature
        | AbilityCondition::DayNightIsNeither
        | AbilityCondition::DayNightIs { .. }
        | AbilityCondition::NthResolutionThisTurn { .. } => {}
    }
    Ok(())
}

fn validate_repeat_continuation(
    repeat: &crate::types::ability::RepeatContinuation,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::RepeatContinuation;

    match repeat {
        RepeatContinuation::WhileCondition { condition, .. } => {
            validate_ability_condition(condition, &format!("{path}.condition"))?
        }
        RepeatContinuation::ControllerChoice | RepeatContinuation::UntilStopConditions { .. } => {}
    }
    Ok(())
}

fn validate_casting_option(option: &SpellCastingOption, path: &str) -> Result<(), String> {
    if let Some(cost) = &option.cost {
        validate_cost(cost, &format!("{path}.cost"))?;
    }
    if let Some(condition) = &option.condition {
        validate_parsed_condition(condition, &format!("{path}.condition"))?;
    }
    Ok(())
}

fn validate_casting_restriction(
    restriction: &crate::types::ability::CastingRestriction,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::CastingRestriction;

    match restriction {
        CastingRestriction::RequiresCondition {
            condition: Some(condition),
        } => validate_parsed_condition(condition, &format!("{path}.condition"))?,
        CastingRestriction::AsSorcery
        | CastingRestriction::DuringCombat
        | CastingRestriction::DuringOpponentsTurn
        | CastingRestriction::DuringYourTurn
        | CastingRestriction::DuringYourUpkeep
        | CastingRestriction::DuringOpponentsUpkeep
        | CastingRestriction::DuringAnyUpkeep
        | CastingRestriction::DuringYourEndStep
        | CastingRestriction::DuringOpponentsEndStep
        | CastingRestriction::DeclareAttackersStep
        | CastingRestriction::DeclareBlockersStep
        | CastingRestriction::BeforeAttackersDeclared
        | CastingRestriction::BeforeBlockersDeclared
        | CastingRestriction::AfterBlockersDeclared
        | CastingRestriction::BeforeCombatDamage
        | CastingRestriction::AfterCombat
        | CastingRestriction::RequiresCondition { condition: None } => {}
    }
    Ok(())
}

fn validate_modal_choice(
    modal: &crate::types::ability::ModalChoice,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::{ModalSelectionCondition, ModalSelectionConstraint};

    validate_player_filter(&modal.chooser, &format!("{path}.chooser"))?;
    if let Some(quantity) = &modal.dynamic_max_choices {
        validate_quantity_expr(quantity, &format!("{path}.dynamic_max_choices"))?;
    }
    for (index, constraint) in modal.constraints.iter().enumerate() {
        match constraint {
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: ModalSelectionCondition::Static { condition },
                ..
            } => validate_static_condition(
                condition,
                &format!("{path}.constraints[{index}].condition"),
            )?,
            ModalSelectionConstraint::ConditionalMaxChoices {
                condition: ModalSelectionCondition::AdditionalCostPaid { .. },
                ..
            }
            | ModalSelectionConstraint::DifferentTargetPlayers
            | ModalSelectionConstraint::NoRepeatThisTurn
            | ModalSelectionConstraint::NoRepeatThisGame => {}
        }
    }
    Ok(())
}

fn validate_target_selection_constraint(
    constraint: &crate::types::game_state::TargetSelectionConstraint,
    path: &str,
) -> Result<(), String> {
    use crate::types::game_state::TargetSelectionConstraint;

    match constraint {
        TargetSelectionConstraint::TotalManaValue { value, .. } => {
            validate_quantity_expr(value, &format!("{path}.value"))?
        }
        TargetSelectionConstraint::DifferentTargetPlayers
        | TargetSelectionConstraint::DifferentObjectControllers
        | TargetSelectionConstraint::SameZoneOwner { .. } => {}
    }
    Ok(())
}

fn validate_solve_condition(
    condition: &crate::types::ability::SolveCondition,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::SolveCondition;

    match condition {
        SolveCondition::ObjectCount { filter, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?
        }
        SolveCondition::Condition { condition } => {
            validate_static_condition(condition, &format!("{path}.condition"))?
        }
        SolveCondition::Text { .. } => {}
    }
    Ok(())
}

fn validate_unless_pay_scaling(
    scaling: &crate::types::ability::UnlessPayScaling,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::UnlessPayScaling;

    match scaling {
        UnlessPayScaling::PerQuantityRef { quantity }
        | UnlessPayScaling::PerAffectedAndQuantityRef { quantity }
        | UnlessPayScaling::PerAffectedWithRef { quantity } => {
            validate_quantity_ref(quantity, &format!("{path}.quantity"))?
        }
        UnlessPayScaling::Flat | UnlessPayScaling::PerAffectedCreature => {}
    }
    Ok(())
}

fn validate_token_spec(
    spec: &crate::types::proposed_event::TokenSpec,
    path: &str,
) -> Result<(), String> {
    for (index, keyword) in spec.characteristics.keywords.iter().enumerate() {
        validate_keyword(keyword, &format!("{path}.keywords[{index}]"))?;
    }
    for (index, static_def) in spec.static_abilities.iter().enumerate() {
        validate_static(static_def, &format!("{path}.statics[{index}]"))?;
    }
    if let Some(duration) = &spec.sacrifice_at {
        validate_duration(duration, &format!("{path}.sacrifice_at"))?;
    }
    Ok(())
}

fn validate_face_down_profile(
    profile: &crate::types::ability::FaceDownProfile,
    path: &str,
) -> Result<(), String> {
    if let Some(ward) = &profile.ward {
        validate_ward_cost(ward, &format!("{path}.ward"))?;
    }
    Ok(())
}

fn validate_mana_spell_grant(
    grant: &crate::types::mana::ManaSpellGrant,
    path: &str,
) -> Result<(), String> {
    use crate::types::mana::ManaSpellGrant;

    match grant {
        ManaSpellGrant::CantBeCountered => {}
        ManaSpellGrant::AddKeywordUntilEndOfTurn { keyword, .. } => {
            validate_keyword(keyword, &format!("{path}.keyword"))?
        }
        ManaSpellGrant::TriggerOnSpend { filter, ability } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            validate_ability(ability, &format!("{path}.ability"))?;
        }
    }
    Ok(())
}

fn validate_casting_permission(
    permission: &crate::types::ability::CastingPermission,
    path: &str,
) -> Result<(), String> {
    use crate::types::ability::CastingPermission;

    match permission {
        CastingPermission::ExileWithAltAbilityCost {
            cost, constraint, ..
        } => {
            validate_cost(cost, &format!("{path}.cost"))?;
            if let Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                value, ..
            }) = constraint
            {
                validate_quantity_expr(value, &format!("{path}.constraint.value"))?;
            }
        }
        CastingPermission::PlayFromExile {
            duration,
            card_filter,
            ..
        } => {
            validate_duration(duration, &format!("{path}.duration"))?;
            if let Some(filter) = card_filter {
                validate_target_filter(filter, &format!("{path}.card_filter"))?;
            }
        }
        CastingPermission::ExileWithAltCost {
            constraint,
            duration,
            enters_with_modifications,
            ..
        } => {
            if let Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                value, ..
            }) = constraint
            {
                validate_quantity_expr(value, &format!("{path}.constraint.value"))?;
            }
            if let Some(duration) = duration {
                validate_duration(duration, &format!("{path}.duration"))?;
            }
            for (index, modification) in enters_with_modifications.iter().enumerate() {
                validate_continuous_modification(
                    modification,
                    &format!("{path}.enters_with_modifications[{index}]"),
                )?;
            }
        }
        CastingPermission::AdventureCreature
        | CastingPermission::ExileWithEnergyCost
        | CastingPermission::WarpExile { .. }
        | CastingPermission::Plotted { .. }
        | CastingPermission::Foretold { .. } => {}
    }
    Ok(())
}

fn validate_effect(
    effect: &Effect,
    context: SearchFoundEffectContext,
    path: &str,
) -> Result<(), String> {
    if let Effect::ApplySearchFoundReplacement { modifier } = effect {
        if context != SearchFoundEffectContext::CanonicalExecute {
            return Err(format!(
                "{path}: ApplySearchFoundReplacement is forbidden outside the exact SearchFound execute root"
            ));
        }
        validate_search_found_modifier(modifier, path)?;
        return Ok(());
    }

    match effect {
        Effect::ChangeZone {
            target,
            enters_under: _,
            enter_with_counters,
            conditional_enter_with_counters,
            face_down_profile,
            enters_modified_if,
            origin: _,
            destination: _,
            owner_library: _,
            enter_transformed: _,
            enter_tapped: _,
            enters_attacking: _,
            up_to: _,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            for (index, (_, count)) in enter_with_counters.iter().enumerate() {
                validate_quantity_expr(
                    count,
                    &format!("{path}.enter_with_counters[{index}].count"),
                )?;
            }
            for (index, (filter, _, count)) in conditional_enter_with_counters.iter().enumerate() {
                validate_target_filter(
                    filter,
                    &format!("{path}.conditional_enter_with_counters[{index}].filter"),
                )?;
                validate_quantity_expr(
                    count,
                    &format!("{path}.conditional_enter_with_counters[{index}].count"),
                )?;
            }
            if let Some(profile) = face_down_profile {
                validate_face_down_profile(profile, &format!("{path}.face_down_profile"))?;
            }
            if let Some(filter) = enters_modified_if {
                validate_target_filter(filter, &format!("{path}.enters_modified_if"))?;
            }
        }
        Effect::ChangeZoneAll {
            target,
            enters_under: _,
            enter_with_counters,
            face_down_profile,
            origin: _,
            destination: _,
            enter_tapped: _,
            library_position,
            random_order: _,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            for (index, (_, count)) in enter_with_counters.iter().enumerate() {
                validate_quantity_expr(
                    count,
                    &format!("{path}.enter_with_counters[{index}].count"),
                )?;
            }
            if let Some(profile) = face_down_profile {
                validate_face_down_profile(profile, &format!("{path}.face_down_profile"))?;
            }
            if let Some(position) = library_position {
                validate_library_position(position, &format!("{path}.library_position"))?;
            }
        }
        Effect::Manifest {
            target,
            count,
            profile,
            enters_under: _,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(profile) = profile {
                validate_face_down_profile(profile, &format!("{path}.face_down_profile"))?;
            }
        }
        Effect::TurnFaceDown { target, profile } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(profile) = profile {
                validate_face_down_profile(profile, &format!("{path}.face_down_profile"))?;
            }
        }
        Effect::CreateDrawReplacement { replacement_effect }
        | Effect::CreatePlaneswalkReplacement { replacement_effect } => validate_effect(
            replacement_effect,
            SearchFoundEffectContext::Forbidden,
            &format!("{path}.replacement_effect"),
        )?,
        Effect::Draw { count, target } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::Pump {
            power,
            toughness,
            target,
        }
        | Effect::PumpAll {
            power,
            toughness,
            target,
        } => {
            validate_pt_value(power, &format!("{path}.power"))?;
            validate_pt_value(toughness, &format!("{path}.toughness"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::Destroy {
            target,
            cant_regenerate: _,
        }
        | Effect::DestroyAll {
            target,
            cant_regenerate: _,
        } => validate_target_filter(target, &format!("{path}.target"))?,
        Effect::PairWith { target }
        | Effect::Regenerate { target }
        | Effect::RemoveAllDamage { target }
        | Effect::CounterAll { target }
        | Effect::GainControl { target }
        | Effect::GainControlAll { target }
        | Effect::SwitchPT { target }
        | Effect::ExileHaunting { target }
        | Effect::HideawayConceal { target }
        | Effect::Transform { target }
        | Effect::Shuffle { target }
        | Effect::Reveal { target }
        | Effect::PhaseOut { target }
        | Effect::PhaseIn { target }
        | Effect::ForceBlock { target }
        | Effect::BecomePrepared { target }
        | Effect::BecomeUnprepared { target }
        | Effect::BecomeSaddled { target }
        | Effect::ProliferateTarget { target }
        | Effect::Exploit { target }
        | Effect::LoseAllPlayerCounters { target }
        | Effect::PutOnTopOrBottom { target }
        | Effect::Goad { target }
        | Effect::GoadAll { target }
        | Effect::Detain { target }
        | Effect::RemoveFromCombat { target }
        | Effect::BecomeBlocked { target }
        | Effect::TurnFaceUp { target }
        | Effect::ExtraTurn { target }
        | Effect::CrankContraptions { target }
        | Effect::RememberCard { target } => {
            validate_target_filter(target, &format!("{path}.target"))?
        }
        Effect::ControlNextTurn { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::Suspect { target, .. }
        | Effect::Unsuspect { target, .. }
        | Effect::Heist { target, .. }
        | Effect::SetRoomDoorLock { target, .. }
        | Effect::Double { target, .. }
        | Effect::ReassembleContraption { target, .. }
        | Effect::AssembleContraptionOnSprocket { target, .. }
        | Effect::ReassembleContraptionOnSprocket { target, .. }
        | Effect::ApplySticker { target, .. } => {
            validate_target_filter(target, &format!("{path}.target"))?
        }
        Effect::Mill { count, target, .. }
        | Effect::RemoveCounter { count, target, .. }
        | Effect::Sacrifice { count, target, .. }
        | Effect::PutCounter { count, target, .. }
        | Effect::PutCounterAll { count, target, .. }
        | Effect::GivePlayerCounter { count, target, .. }
        | Effect::SkipNextStep { count, target, .. } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::PutAtLibraryPosition {
            target,
            count,
            position,
        } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_library_position(position, &format!("{path}.position"))?;
        }
        Effect::Scry { count, target }
        | Effect::Surveil { count, target }
        | Effect::Connive { count, target }
        | Effect::SkipNextTurn { count, target } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::DealDamage {
            amount,
            target,
            damage_source: _,
            excess: _,
        }
        | Effect::SetLifeTotal { amount, target } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::GainLife { amount, player } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            validate_target_filter(player, &format!("{path}.player"))?;
        }
        Effect::LoseLife { amount, target } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            if let Some(filter) = target {
                validate_target_filter(filter, &format!("{path}.target"))?;
            }
        }
        Effect::DamageAll {
            amount,
            target,
            player_filter,
            damage_source: _,
        } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(filter) = player_filter {
                validate_player_filter(filter, &format!("{path}.player_filter"))?;
            }
        }
        Effect::DamageEachPlayer {
            amount,
            player_filter,
        } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            validate_player_filter(player_filter, &format!("{path}.player_filter"))?;
        }
        Effect::EachDealsDamageEqualToPower {
            sources,
            recipient,
            extra_source,
        } => {
            validate_target_filter(sources, &format!("{path}.sources"))?;
            validate_target_filter(recipient, &format!("{path}.recipient"))?;
            if let Some(filter) = extra_source {
                validate_target_filter(filter, &format!("{path}.extra_source"))?;
            }
        }
        Effect::EachSourceDealsDamage {
            sources,
            amount,
            recipient,
        } => {
            validate_target_filter(sources, &format!("{path}.sources"))?;
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            if let crate::types::ability::EachDamageRecipient::Shared(filter) = recipient {
                validate_target_filter(filter, &format!("{path}.recipient"))?;
            }
        }
        Effect::Fight { target, subject } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(subject, &format!("{path}.subject"))?;
        }
        Effect::Attach { attachment, target } | Effect::UnattachAll { attachment, target } => {
            validate_target_filter(attachment, &format!("{path}.attachment"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::ExchangeControl { target_a, target_b }
        | Effect::ExchangeLifeTotals {
            player_a: target_a,
            player_b: target_b,
        } => {
            validate_target_filter(target_a, &format!("{path}.target_a"))?;
            validate_target_filter(target_b, &format!("{path}.target_b"))?;
        }
        Effect::GiveControl { target, recipient } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(recipient, &format!("{path}.recipient"))?;
        }
        Effect::Discard {
            count,
            target,
            unless_filter,
            filter,
            selection: _,
        } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(filter) = unless_filter {
                validate_target_filter(filter, &format!("{path}.unless_filter"))?;
            }
            if let Some(filter) = filter {
                validate_target_filter(filter, &format!("{path}.filter"))?;
            }
        }
        Effect::Dig {
            player,
            count,
            destination: _,
            keep_count: _,
            keep_count_expr,
            up_to: _,
            filter,
            rest_destination: _,
            reveal: _,
            enter_tapped: _,
            source: _,
        } => {
            validate_target_filter(player, &format!("{path}.player"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(keep_count_expr) = keep_count_expr {
                validate_quantity_expr(keep_count_expr, &format!("{path}.keep_count_expr"))?;
            }
            validate_target_filter(filter, &format!("{path}.filter"))?;
        }
        Effect::SearchOutsideGame { filter, count, .. } | Effect::Seek { filter, count, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
        }
        Effect::RevealHand {
            target,
            card_filter,
            count,
            selection: _,
            choice_optional: _,
            reveal: _,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(card_filter, &format!("{path}.card_filter"))?;
            if let Some(count) = count {
                validate_quantity_expr(count, &format!("{path}.count"))?;
            }
        }
        Effect::RevealTop { player, count: _ } => {
            validate_target_filter(player, &format!("{path}.player"))?
        }
        Effect::ExileTop { player, count, .. } => {
            validate_target_filter(player, &format!("{path}.player"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
        }
        Effect::RevealUntil {
            player,
            filter,
            count,
            ..
        } => {
            validate_target_filter(player, &format!("{path}.player"))?;
            validate_target_filter(filter, &format!("{path}.filter"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
        }
        Effect::ExileFromTopUntil { player, until } => {
            validate_target_filter(player, &format!("{path}.player"))?;
            validate_until_condition(until, &format!("{path}.until"))?;
        }
        Effect::Vote {
            per_choice_effect,
            subject,
            ..
        } => {
            for (index, ability) in per_choice_effect.iter().enumerate() {
                validate_ability(ability, &format!("{path}.per_choice_effect[{index}]"))?;
            }
            if let crate::types::ability::VoteSubject::Objects {
                candidate_filter,
                outcome_template,
            } = subject
            {
                validate_target_filter(
                    candidate_filter,
                    &format!("{path}.subject.candidate_filter"),
                )?;
                validate_ability(outcome_template, &format!("{path}.outcome_template"))?;
            }
        }
        Effect::SeparateIntoPiles {
            object_filter,
            chosen_pile_effect,
            unchosen_pile_effect,
            ..
        } => {
            validate_target_filter(object_filter, &format!("{path}.object_filter"))?;
            validate_ability(chosen_pile_effect, &format!("{path}.chosen_pile_effect"))?;
            if let Some(ability) = unchosen_pile_effect {
                validate_ability(ability, &format!("{path}.unchosen_pile_effect"))?;
            }
        }
        Effect::RevealFromHand { filter, on_decline } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            if let Some(ability) = on_decline {
                validate_ability(ability, &format!("{path}.on_decline"))?;
            }
        }
        Effect::CreateDelayedTrigger {
            condition,
            effect,
            uses_tracked_set: _,
        } => {
            validate_delayed_trigger_condition(condition, &format!("{path}.condition"))?;
            validate_ability(effect, &format!("{path}.effect"))?
        }
        Effect::FlipCoin {
            win_effect,
            lose_effect,
            flipper,
        } => {
            validate_target_filter(flipper, &format!("{path}.flipper"))?;
            if let Some(ability) = win_effect {
                validate_ability(ability, &format!("{path}.win_effect"))?;
            }
            if let Some(ability) = lose_effect {
                validate_ability(ability, &format!("{path}.lose_effect"))?;
            }
        }
        Effect::FlipCoins {
            count,
            win_effect,
            lose_effect,
            flipper,
        } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(flipper, &format!("{path}.flipper"))?;
            if let Some(ability) = win_effect {
                validate_ability(ability, &format!("{path}.win_effect"))?;
            }
            if let Some(ability) = lose_effect {
                validate_ability(ability, &format!("{path}.lose_effect"))?;
            }
        }
        Effect::FlipCoinUntilLose { win_effect } => {
            validate_ability(win_effect, &format!("{path}.win_effect"))?
        }
        Effect::RollDie {
            count,
            sides: _,
            results,
            modifier,
        } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            for (index, result) in results.iter().enumerate() {
                validate_ability(&result.effect, &format!("{path}.results[{index}].effect"))?;
            }
            if let Some(modifier) = modifier {
                validate_die_roll_modifier(modifier, &format!("{path}.modifier"))?;
            }
        }
        Effect::ChooseOneOf { chooser, branches } => {
            validate_player_filter(chooser, &format!("{path}.chooser"))?;
            for (index, branch) in branches.iter().enumerate() {
                validate_ability(branch, &format!("{path}.branches[{index}]"))?;
            }
        }
        Effect::GenericEffect {
            static_abilities,
            duration,
            target,
        } => {
            if let Some(duration) = duration {
                validate_duration(duration, &format!("{path}.duration"))?;
            }
            if let Some(target) = target {
                validate_target_filter(target, &format!("{path}.target"))?;
            }
            for (index, static_def) in static_abilities.iter().enumerate() {
                validate_static(static_def, &format!("{path}.statics[{index}]"))?;
            }
        }
        Effect::AddTargetReplacement {
            target,
            replacement,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_replacement(replacement, &format!("{path}.replacement"))?;
        }
        Effect::Counter {
            target,
            source_rider,
            countered_spell_zone,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            match source_rider {
                Some(crate::types::ability::CounterSourceRider::LosesAbilities {
                    static_def,
                    duration,
                }) => {
                    validate_static(static_def, &format!("{path}.source_rider.static"))?;
                    validate_duration(duration, &format!("{path}.source_rider.duration"))?;
                }
                Some(crate::types::ability::CounterSourceRider::Destroy) | None => {}
            }
            if let Some(replacement) = countered_spell_zone {
                validate_spell_stack_to_graveyard_replacement(
                    replacement,
                    &format!("{path}.countered_spell_zone"),
                )?;
            }
        }
        Effect::Token {
            power,
            toughness,
            keywords,
            count,
            owner,
            attach_to,
            static_abilities,
            enter_with_counters,
            ..
        } => {
            validate_pt_value(power, &format!("{path}.power"))?;
            validate_pt_value(toughness, &format!("{path}.toughness"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_target_filter(owner, &format!("{path}.owner"))?;
            if let Some(filter) = attach_to {
                validate_target_filter(filter, &format!("{path}.attach_to"))?;
            }
            for (index, keyword) in keywords.iter().enumerate() {
                validate_keyword(keyword, &format!("{path}.keywords[{index}]"))?;
            }
            for (index, static_def) in static_abilities.iter().enumerate() {
                validate_static(static_def, &format!("{path}.statics[{index}]"))?;
            }
            for (index, (_, count)) in enter_with_counters.iter().enumerate() {
                validate_quantity_expr(
                    count,
                    &format!("{path}.enter_with_counters[{index}].count"),
                )?;
            }
        }
        Effect::CreateTokenCopyFromPool {
            owner,
            type_filter,
            mv_bound,
            count,
            ..
        } => {
            validate_target_filter(owner, &format!("{path}.owner"))?;
            validate_target_filter(type_filter, &format!("{path}.type_filter"))?;
            validate_quantity_expr(mv_bound, &format!("{path}.mv_bound"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
        }
        Effect::CreateEmblem { statics, triggers } => {
            for (index, static_def) in statics.iter().enumerate() {
                validate_static(static_def, &format!("{path}.statics[{index}]"))?;
            }
            for (index, trigger) in triggers.iter().enumerate() {
                validate_trigger(trigger, &format!("{path}.triggers[{index}]"))?;
            }
        }
        Effect::EpicCopy { spell } => validate_resolved(spell, &format!("{path}.spell"))?,
        Effect::CopySpell {
            target,
            additional_modifications,
            ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            for (index, modification) in additional_modifications.iter().enumerate() {
                validate_continuous_modification(
                    modification,
                    &format!("{path}.additional_modifications[{index}]"),
                )?;
            }
        }
        Effect::BecomeCopy {
            target,
            recipient,
            duration,
            additional_modifications,
            ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(recipient, &format!("{path}.recipient"))?;
            if let Some(duration) = duration {
                validate_duration(duration, &format!("{path}.duration"))?;
            }
            for (index, modification) in additional_modifications.iter().enumerate() {
                validate_continuous_modification(
                    modification,
                    &format!("{path}.additional_modifications[{index}]"),
                )?;
            }
        }
        Effect::CopyTokenOf {
            target,
            owner,
            source_filter,
            count,
            extra_keywords,
            additional_modifications,
            ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(owner, &format!("{path}.owner"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(filter) = source_filter {
                validate_target_filter(filter, &format!("{path}.source_filter"))?;
            }
            for (index, keyword) in extra_keywords.iter().enumerate() {
                validate_keyword(keyword, &format!("{path}.extra_keywords[{index}]"))?;
            }
            for (index, modification) in additional_modifications.iter().enumerate() {
                validate_continuous_modification(
                    modification,
                    &format!("{path}.additional_modifications[{index}]"),
                )?;
            }
        }
        Effect::Animate {
            power,
            toughness,
            target,
            keywords,
            types: _,
            remove_types: _,
        } => {
            if let Some(power) = power {
                validate_pt_value(power, &format!("{path}.power"))?;
            }
            if let Some(toughness) = toughness {
                validate_pt_value(toughness, &format!("{path}.toughness"))?;
            }
            validate_target_filter(target, &format!("{path}.target"))?;
            for (index, keyword) in keywords.iter().enumerate() {
                validate_keyword(keyword, &format!("{path}.keywords[{index}]"))?;
            }
        }
        Effect::Mana {
            produced,
            grants,
            target,
            restrictions: _,
            expiry: _,
        } => {
            validate_mana_production(produced, &format!("{path}.produced"))?;
            for (index, grant) in grants.iter().enumerate() {
                validate_mana_spell_grant(grant, &format!("{path}.grants[{index}]"))?;
            }
            if let Some(filter) = target {
                validate_target_filter(filter, &format!("{path}.target"))?;
            }
        }
        Effect::GrantCastingPermission {
            permission, target, ..
        } => {
            validate_casting_permission(permission, &format!("{path}.permission"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::EachPlayerCopyChosen {
            choose_filter,
            copy_modifications,
            ..
        } => {
            validate_target_filter(choose_filter, &format!("{path}.choose_filter"))?;
            for (index, modification) in copy_modifications.iter().enumerate() {
                validate_continuous_modification(
                    modification,
                    &format!("{path}.copy_modifications[{index}]"),
                )?;
            }
        }
        Effect::Choose { choice_type, .. } => {
            validate_choice_type(choice_type, &format!("{path}.choice_type"))?
        }
        Effect::ApplyPerpetual {
            target,
            modification,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_perpetual_modification(modification, &format!("{path}.modification"))?;
        }
        Effect::SearchLibrary {
            filter,
            count,
            target_player,
            selection_constraint,
            source_zones: _,
            reveal: _,
            split: _,
        } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(filter) = target_player {
                validate_target_filter(filter, &format!("{path}.target_player"))?;
            }
            if let crate::types::ability::SearchSelectionConstraint::MatchEachFilter { filters } =
                selection_constraint
            {
                for (index, filter) in filters.iter().enumerate() {
                    validate_target_filter(
                        filter,
                        &format!("{path}.selection_constraint.filters[{index}]"),
                    )?;
                }
            }
        }
        Effect::TargetOnly { target } => validate_target_filter(target, &format!("{path}.target"))?,
        Effect::AdditionalPhase {
            target,
            count,
            attacker_restriction,
            ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(filter) = attacker_restriction {
                validate_target_filter(filter, &format!("{path}.attacker_restriction"))?;
            }
        }
        Effect::ReturnAsAura {
            enchant_filter,
            grants,
        } => {
            validate_target_filter(enchant_filter, &format!("{path}.enchant_filter"))?;
            for (index, modification) in grants.iter().enumerate() {
                validate_continuous_modification(modification, &format!("{path}.grants[{index}]"))?;
            }
        }
        Effect::AddPendingEntersModifications {
            modifications: grants,
        } => {
            for (index, modification) in grants.iter().enumerate() {
                validate_continuous_modification(
                    modification,
                    &format!("{path}.modifications[{index}]"),
                )?;
            }
        }
        Effect::PayCost { cost, scale, payer } => {
            validate_cost(cost, &format!("{path}.cost"))?;
            if let Some(quantity) = scale {
                validate_quantity_expr(quantity, &format!("{path}.scale"))?;
            }
            validate_target_filter(payer, &format!("{path}.payer"))?;
        }
        Effect::CastFromZone {
            target,
            duration,
            alt_ability_cost,
            constraint,
            ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(duration) = duration {
                validate_duration(duration, &format!("{path}.duration"))?;
            }
            if let Some(cost) = alt_ability_cost {
                validate_cost(cost, &format!("{path}.alt_ability_cost"))?;
            }
            if let Some(crate::types::ability::CastPermissionConstraint::ManaValue {
                value, ..
            }) = constraint
            {
                validate_quantity_expr(value, &format!("{path}.constraint.value"))?;
            }
        }
        Effect::GrantNextSpellAbility {
            modifier,
            spell_filter,
            ..
        } => {
            if let crate::types::game_state::NextSpellModifier::HasKeyword { keyword } = modifier {
                validate_keyword(keyword, &format!("{path}.modifier.keyword"))?;
            }
            if let Some(filter) = spell_filter {
                validate_target_filter(filter, &format!("{path}.spell_filter"))?;
            }
        }
        Effect::Intensify { amount, scope: _ }
        | Effect::GainEnergy { amount }
        | Effect::AddPendingETBCounters { count: amount, .. }
        | Effect::AssembleContraptions { count: amount }
        | Effect::Incubate { count: amount }
        | Effect::Amass { count: amount, .. }
        | Effect::Monstrosity { count: amount }
        | Effect::Renown { count: amount }
        | Effect::Bolster { count: amount }
        | Effect::Adapt { count: amount } => {
            validate_quantity_expr(amount, &format!("{path}.count"))?
        }
        Effect::StartYourEngines { player_scope } => {
            validate_player_filter(player_scope, &format!("{path}.player_scope"))?
        }
        Effect::ChangeSpeed {
            player_scope,
            amount,
            direction: _,
            floor: _,
        } => {
            validate_player_filter(player_scope, &format!("{path}.player_scope"))?;
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
        }
        Effect::ExchangeLifeWithStat { player, .. }
        | Effect::ExploreAll { filter: player, .. }
        | Effect::Behold { filter: player }
        | Effect::FreeCastFromZones { filter: player, .. }
        | Effect::ChooseDamageSource {
            source_filter: player,
        }
        | Effect::BlightEffect { player, .. } => {
            validate_target_filter(player, &format!("{path}.filter"))?
        }
        Effect::SetTapState { target, .. }
        | Effect::DiscardCard { target, .. }
        | Effect::ChooseCard { target, .. }
        | Effect::MultiplyCounter { target, .. }
        | Effect::DoublePT { target, .. }
        | Effect::DoublePTAll { target, .. }
        | Effect::ChooseCounterKind { target } => {
            validate_target_filter(target, &format!("{path}.target"))?
        }
        Effect::BounceAll { target, count, .. } | Effect::CastCopyOfCard { target, count, .. } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(count) = count {
                validate_quantity_expr(count, &format!("{path}.count"))?;
            }
        }
        Effect::CopyTokenBlockingAttacker {
            source_filter,
            owner,
        } => {
            validate_target_filter(source_filter, &format!("{path}.source_filter"))?;
            validate_target_filter(owner, &format!("{path}.owner"))?;
        }
        Effect::GainActivatedAbilitiesOfTarget {
            target,
            recipient,
            duration,
            scope: _,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(recipient, &format!("{path}.recipient"))?;
            if let Some(duration) = duration {
                validate_duration(duration, &format!("{path}.duration"))?;
            }
        }
        Effect::ChooseCounterAdjustment { count, .. } => {
            validate_quantity_expr(count, &format!("{path}.count"))?
        }
        Effect::MoveCounters {
            source,
            count,
            target,
            counter_type: _,
            mode: _,
            selection: _,
        } => {
            validate_target_filter(source, &format!("{path}.source"))?;
            if let Some(count) = count {
                validate_quantity_expr(count, &format!("{path}.count"))?;
            }
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::ForceAttack {
            target,
            required_player,
            duration,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_target_filter(required_player, &format!("{path}.required_player"))?;
            validate_duration(duration, &format!("{path}.duration"))?;
        }
        Effect::ReduceNextSpellCost { spell_filter, .. } => {
            if let Some(filter) = spell_filter {
                validate_target_filter(filter, &format!("{path}.spell_filter"))?;
            }
        }
        Effect::PreventDamage {
            amount: _,
            amount_dynamic,
            target,
            scope: _,
            damage_source_filter,
            prevention_duration,
        } => {
            if let Some(amount) = amount_dynamic {
                validate_quantity_expr(amount, &format!("{path}.amount_dynamic"))?;
            }
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(filter) = damage_source_filter {
                validate_target_filter(filter, &format!("{path}.damage_source_filter"))?;
            }
            if let Some(duration) = prevention_duration {
                validate_duration(duration, &format!("{path}.prevention_duration"))?;
            }
        }
        Effect::LoseTheGame { target } | Effect::WinTheGame { target } => {
            if let Some(target) = target {
                validate_target_filter(target, &format!("{path}.target"))?;
            }
        }
        Effect::PutSticker {
            target,
            count,
            max_ticket_cost,
            ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(max) = max_ticket_cost {
                validate_quantity_expr(max, &format!("{path}.max_ticket_cost"))?;
            }
        }
        Effect::ChooseFromZone { filter, .. } => {
            if let Some(filter) = filter {
                validate_target_filter(filter, &format!("{path}.filter"))?;
            }
        }
        Effect::ChooseObjectsIntoTrackedSet {
            chooser, filter, ..
        } => {
            validate_target_filter(chooser, &format!("{path}.chooser"))?;
            validate_target_filter(filter, &format!("{path}.filter"))?;
        }
        Effect::ChooseAndSacrificeRest {
            categories: _,
            chooser_scope: _,
            choose_filter,
            sacrifice_filter,
            total_power_cap,
            keeper_constraint,
        } => {
            validate_target_filter(choose_filter, &format!("{path}.choose_filter"))?;
            validate_target_filter(sacrifice_filter, &format!("{path}.sacrifice_filter"))?;
            if let Some(cap) = total_power_cap {
                validate_quantity_expr(cap, &format!("{path}.total_power_cap"))?;
            }
            if let Some(constraint) = keeper_constraint {
                validate_keeper_constraint(constraint, &format!("{path}.keeper_constraint"))?;
            }
        }
        Effect::Discover {
            mana_value_limit,
            player,
        } => {
            validate_quantity_expr(mana_value_limit, &format!("{path}.mana_value_limit"))?;
            validate_target_filter(player, &format!("{path}.player"))?;
        }
        Effect::ChooseDrawnThisTurnPayOrTopdeck {
            count,
            life_payment,
            player,
        } => {
            validate_quantity_expr(count, &format!("{path}.count"))?;
            validate_quantity_expr(life_payment, &format!("{path}.life_payment"))?;
            validate_target_filter(player, &format!("{path}.player"))?;
        }
        Effect::ChangeTargets {
            target, forced_to, ..
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            if let Some(filter) = forced_to {
                validate_target_filter(filter, &format!("{path}.forced_to"))?;
            }
        }
        Effect::Cloak {
            target,
            count,
            object_source,
        } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
            if let Some(filter) = object_source {
                validate_target_filter(filter, &format!("{path}.object_source"))?;
            }
        }
        Effect::GrantExtraLoyaltyActivations { amount, target } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            validate_target_filter(target, &format!("{path}.target"))?;
        }
        Effect::Endure { amount, subject } => {
            validate_quantity_expr(amount, &format!("{path}.amount"))?;
            validate_target_filter(subject, &format!("{path}.subject"))?;
        }
        Effect::CreateDamageReplacement {
            source_filter,
            combat_scope: _,
            target_filter: _,
            modification,
            redirect_to: _,
            redirect_amount: _,
            redirect_object_filter,
            recipient_object_filter,
        } => {
            for (field, filter) in [
                ("source_filter", source_filter.as_ref()),
                ("redirect_object_filter", redirect_object_filter.as_ref()),
                ("recipient_object_filter", recipient_object_filter.as_ref()),
            ] {
                if let Some(filter) = filter {
                    validate_target_filter(filter, &format!("{path}.{field}"))?;
                }
            }
            if let Some(modification) = modification {
                validate_damage_modification(modification, &format!("{path}.modification"))?;
            }
        }
        Effect::CombineHost { host, .. } => validate_target_filter(host, &format!("{path}.host"))?,
        Effect::ChooseAugmentAndCombineWithHost { filter, host, .. } => {
            validate_target_filter(filter, &format!("{path}.filter"))?;
            validate_target_filter(host, &format!("{path}.host"))?;
        }
        Effect::PutChosenCounter { target, count } => {
            validate_target_filter(target, &format!("{path}.target"))?;
            validate_quantity_expr(count, &format!("{path}.count"))?;
        }
        Effect::OpponentGuess {
            guesser: _,
            subject,
        } => match subject.as_ref() {
            crate::types::ability::GuessSubject::CommittedChoice { choice_type } => {
                validate_choice_type(choice_type, &format!("{path}.subject.choice_type"))?
            }
            crate::types::ability::GuessSubject::Proposition { lhs, rhs, .. } => {
                validate_quantity_expr(lhs, &format!("{path}.subject.lhs"))?;
                validate_quantity_expr(rhs, &format!("{path}.subject.rhs"))?;
            }
        },
        Effect::Conjure {
            cards,
            library_players,
            destination: _,
            tapped: _,
            library_position,
        } => {
            for (index, card) in cards.iter().enumerate() {
                validate_quantity_expr(&card.count, &format!("{path}.cards[{index}].count"))?;
                if let crate::types::ability::ConjureSource::Duplicate { duplicate_of } =
                    &card.source
                {
                    validate_target_filter(
                        duplicate_of,
                        &format!("{path}.cards[{index}].duplicate_of"),
                    )?;
                }
            }
            if let Some(filter) = library_players {
                validate_player_filter(filter, &format!("{path}.library_players"))?;
            }
            if let Some(position) = library_position {
                validate_library_position(position, &format!("{path}.library_position"))?;
            }
        }
        Effect::ForEachCategory {
            action,
            category: _,
            chooser: _,
        } => match action {
            crate::types::ability::ForEachCategoryAction::PutCounter {
                target,
                count,
                counter_type: _,
            } => {
                validate_target_filter(target, &format!("{path}.action.target"))?;
                validate_quantity_expr(count, &format!("{path}.action.count"))?;
            }
            crate::types::ability::ForEachCategoryAction::ExileFromPool { .. } => {}
        },
        Effect::AddRestriction { restriction } => {
            validate_game_restriction(restriction, &format!("{path}.restriction"))?
        }
        Effect::ApplySearchFoundReplacement { .. } => unreachable!("handled above"),
        Effect::Meld {
            source: _,
            partner: _,
            result: _,
            source_filter,
            partner_filter,
            entry: _,
        } => {
            validate_target_filter(source_filter, &format!("{path}.source_filter"))?;
            validate_target_filter(partner_filter, &format!("{path}.partner_filter"))?;
        }
        Effect::HeistExile
        | Effect::DraftFromSpellbook { .. }
        | Effect::ApplyPostReplacementDamage { .. }
        | Effect::Explore
        | Effect::Investigate
        | Effect::Tribute { .. }
        | Effect::TimeTravel
        | Effect::BecomeMonarch
        | Effect::NoOp
        | Effect::Proliferate
        | Effect::EndTheTurn
        | Effect::EndCombatPhase
        | Effect::Populate
        | Effect::Clash
        | Effect::Myriad
        | Effect::Encore
        | Effect::RegisterBending { .. }
        | Effect::Cleanup { .. }
        | Effect::SwapChosenLabels { .. }
        | Effect::SolveCase
        | Effect::SetClassLevel { .. }
        | Effect::ExileResolvingSpellInsteadOfGraveyard { .. }
        | Effect::RingTemptsYou
        | Effect::VentureIntoDungeon
        | Effect::VentureInto { .. }
        | Effect::TakeTheInitiative
        | Effect::Planeswalk
        | Effect::ChaosEnsues
        | Effect::RedistributeLifeTotals
        | Effect::ReverseTurnOrder
        | Effect::OpenAttractions { .. }
        | Effect::RollToVisitAttractions
        | Effect::AssembleContraptionsFromRollDifference
        | Effect::ProcessRadCounters
        | Effect::Cascade
        | Effect::Ripple { .. }
        | Effect::MiracleCast { .. }
        | Effect::MadnessCast { .. }
        | Effect::GiftDelivery { .. }
        | Effect::ManifestDread
        | Effect::RuntimeHandled { .. }
        | Effect::Learn
        | Effect::Forage
        | Effect::Harness
        | Effect::CollectEvidence { .. }
        | Effect::SetDayNight { .. }
        | Effect::Specialize
        | Effect::Unimplemented { .. } => {}
    }
    Ok(())
}

fn validate_search_found_modifier(
    modifier: &SearchFoundModifier,
    path: &str,
) -> Result<(), String> {
    if modifier.destination != crate::types::zones::Zone::Exile {
        return Err(format!("{path}.modifier.destination: expected Exile"));
    }
    if modifier.play_mode != crate::types::ability::CardPlayMode::Play {
        return Err(format!("{path}.modifier.play_mode: expected Play"));
    }
    Ok(())
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
    use std::sync::Arc;

    use super::*;
    use crate::types::ability::{
        AbilityCondition, AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction,
        CardPlayMode, CastingPermission, ChoiceType, ContinuousModification, ControllerRef,
        CopiableValues, DamageModification, Effect, FaceDownProfile, FilterProp, ManaModification,
        ParsedCondition, PermissionGrantee, PerpetualModification, PlayerScope, PtValue,
        QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition, ReplacementMode,
        ResolvedAbility, SearchFoundModifier, StaticDefinition, TargetFilter, TargetSelectionMode,
        TriggerDefinition, TypedFilter,
    };
    use crate::types::card::CleaveVariant;
    use crate::types::card_type::CardType;
    use crate::types::counter::CounterMatch;
    use crate::types::identifiers::ObjectId;
    use crate::types::keywords::{
        BestowCost, BuybackCost, CyclingCost, EchoCost, EmbalmCost, EscapeCost, EternalizeCost,
        EvokeCost, FlashbackCost, Keyword, WardCost,
    };
    use crate::types::mana::{ManaCost, ManaSpellGrant, ManaType};
    use crate::types::phase::Phase;
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::StaticMode;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::{EtbTapState, Zone};

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

    fn search_found_execute(modifier: SearchFoundModifier) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ApplySearchFoundReplacement { modifier },
        )
    }

    fn unsupported_search_found() -> ReplacementDefinition {
        ReplacementDefinition::new(ReplacementEvent::SearchFound)
            .execute(search_found_execute(SearchFoundModifier {
                destination: Zone::Exile,
                play_mode: CardPlayMode::Play,
                mana_spend_permission: None,
            }))
            .execute(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp))
    }

    fn forbidden_search_found_ability() -> AbilityDefinition {
        search_found_execute(SearchFoundModifier {
            destination: Zone::Exile,
            play_mode: CardPlayMode::Play,
            mana_spend_permission: None,
        })
    }

    fn forbidden_search_found_keyword() -> Keyword {
        Keyword::CumulativeUpkeep(AbilityCost::EffectCost {
            effect: Box::new(forbidden_search_found_ability().effect.as_ref().clone()),
        })
    }

    fn forbidden_search_found_filter() -> TargetFilter {
        TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::WithKeyword {
                value: forbidden_search_found_keyword(),
            }]),
        )
    }

    fn forbidden_search_found_keyword_wrappers() -> Vec<(&'static str, Keyword)> {
        let cost = || AbilityCost::EffectCost {
            effect: Box::new(forbidden_search_found_ability().effect.as_ref().clone()),
        };
        vec![
            ("bestow", Keyword::Bestow(BestowCost::NonMana(cost()))),
            ("buyback", Keyword::Buyback(BuybackCost::NonMana(cost()))),
            ("cycling", Keyword::Cycling(CyclingCost::NonMana(cost()))),
            ("echo", Keyword::Echo(EchoCost::NonMana(cost()))),
            ("embalm", Keyword::Embalm(EmbalmCost::NonMana(cost()))),
            ("escape", Keyword::Escape(EscapeCost::NonMana(cost()))),
            (
                "eternalize",
                Keyword::Eternalize(EternalizeCost::NonMana(cost())),
            ),
            ("evoke", Keyword::Evoke(EvokeCost::NonMana(cost()))),
            (
                "flashback",
                Keyword::Flashback(FlashbackCost::NonMana(cost())),
            ),
            ("cumulative", Keyword::CumulativeUpkeep(cost())),
            ("escalate", Keyword::Escalate(cost())),
        ]
    }

    #[test]
    fn validator_rejects_every_nonmana_keyword_cost_wrapper_with_breadcrumb() {
        for (name, keyword) in forbidden_search_found_keyword_wrappers() {
            let mut face = test_face(name);
            face.keywords.push(keyword);

            let error = validate_card_face_for_export(&face)
                .expect_err("keyword-wrapped runtime effect must fail closed");

            assert!(
                error.contains(".keywords[0].cost.effect"),
                "{name}: {error}"
            );
            assert!(error.contains("forbidden outside"), "{name}: {error}");
        }
    }

    #[test]
    fn validator_traverses_per_counter_target_keyword_effect_cost() {
        let mut face = test_face("per-counter target carrier");
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        ability.cost = Some(AbilityCost::PerCounter {
            counter: crate::types::counter::CounterType::Age,
            target: forbidden_search_found_filter(),
            base: Box::new(AbilityCost::Mana {
                cost: ManaCost::generic(1),
            }),
        });
        face.abilities.push(ability);

        let error = validate_card_face_for_export(&face)
            .expect_err("PerCounter target must traverse through keyword effect costs");
        assert!(
            error.contains(".abilities[0].cost.target.properties[0].keyword.cost.effect"),
            "{error}"
        );
        assert!(error.contains("forbidden outside"), "{error}");
    }

    #[test]
    fn validator_accepts_all_four_face_down_profile_carriers_and_compound_ward() {
        let profile = FaceDownProfile {
            ward: Some(WardCost::Compound(vec![
                WardCost::Mana(ManaCost::generic(2)),
                WardCost::PayLife(1),
            ])),
            ..FaceDownProfile::vanilla_2_2()
        };
        let effects = [
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: Some(profile.clone()),
                enters_modified_if: None,
            },
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                enters_under: None,
                enter_tapped: EtbTapState::Unspecified,
                enter_with_counters: vec![],
                face_down_profile: Some(profile.clone()),
                library_position: None,
                random_order: false,
            },
            Effect::Manifest {
                target: TargetFilter::Controller,
                count: QuantityExpr::Fixed { value: 1 },
                profile: Some(profile.clone()),
                enters_under: None,
            },
            Effect::TurnFaceDown {
                target: TargetFilter::Any,
                profile: Some(profile),
            },
        ];

        for (index, effect) in effects.into_iter().enumerate() {
            let mut face = test_face(&format!("Face-down carrier {index}"));
            face.abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, effect));
            validate_card_face_for_export(&face)
                .unwrap_or_else(|error| panic!("face-down carrier {index}: {error}"));
        }
    }

    #[test]
    fn validator_enforces_exact_minimal_search_found_placement() {
        let modifier = SearchFoundModifier {
            destination: Zone::Exile,
            play_mode: CardPlayMode::Play,
            mana_spend_permission: Some(crate::types::ability::ManaSpendPermission::AnyColor),
        };

        let mut canonical = test_face("Canonical SearchFound");
        canonical.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::SearchFound)
                .execute(search_found_execute(modifier.clone())),
        );
        validate_card_face_for_export(&canonical)
            .expect("exact direct SearchFound execute must validate");

        let mut database_kind = test_face("Wrong SearchFound Ability Kind");
        database_kind.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::SearchFound).execute(
                AbilityDefinition::new(
                    AbilityKind::Database,
                    Effect::ApplySearchFoundReplacement {
                        modifier: modifier.clone(),
                    },
                ),
            ),
        );
        let error = validate_card_face_for_export(&database_kind)
            .expect_err("the canonical SearchFound execute must be a spell ability");
        assert!(error.contains("exact minimal direct canonical"), "{error}");

        let mut ordinary_ability = test_face("Wrong Ability Parent");
        ordinary_ability
            .abilities
            .push(search_found_execute(modifier.clone()));
        let error = validate_card_face_for_export(&ordinary_ability)
            .expect_err("ordinary ability must reject SearchFound runtime effect");
        assert!(error.contains(".abilities[0].effect"), "{error}");
        assert!(error.contains("forbidden outside"), "{error}");

        let mut ordinary_replacement = test_face("Wrong Replacement Event");
        ordinary_replacement.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .execute(search_found_execute(modifier.clone())),
        );
        let error = validate_card_face_for_export(&ordinary_replacement)
            .expect_err("non-SearchFound replacement must reject runtime effect");
        assert!(error.contains(".replacements[0].execute.effect"), "{error}");
        assert!(error.contains("forbidden outside"), "{error}");

        let wrapped_execute = search_found_execute(modifier)
            .sub_ability(AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp));
        let mut wrapped = test_face("Wrapped SearchFound Execute");
        wrapped.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::SearchFound).execute(wrapped_execute),
        );
        let error = validate_card_face_for_export(&wrapped)
            .expect_err("SearchFound execute must be exact and minimal");
        assert!(error.contains("exact minimal direct canonical"), "{error}");
    }

    #[test]
    fn validator_rejects_all_six_continuous_modification_parent_families_with_breadcrumbs() {
        let nested_static = || {
            StaticDefinition::continuous().modifications(vec![
                ContinuousModification::GrantAbility {
                    definition: Box::new(forbidden_search_found_ability()),
                },
            ])
        };
        let copy_values = CopiableValues {
            name: "Copied Carrier".to_string(),
            mana_cost: ManaCost::NoCost,
            color: vec![],
            card_types: CardType::default(),
            power: None,
            toughness: None,
            loyalty: None,
            keywords: vec![],
            abilities: Arc::new(vec![forbidden_search_found_ability()]),
            trigger_definitions: Arc::new(vec![]),
            replacement_definitions: Arc::new(vec![]),
            static_definitions: Arc::new(vec![]),
        };
        let cases = [
            (
                "grant_ability",
                ContinuousModification::GrantAbility {
                    definition: Box::new(forbidden_search_found_ability()),
                },
                ".definition.effect",
            ),
            (
                "grant_trigger",
                ContinuousModification::GrantTrigger {
                    trigger: Box::new(
                        TriggerDefinition::new(TriggerMode::SpellCast)
                            .execute(forbidden_search_found_ability()),
                    ),
                },
                ".trigger.execute.effect",
            ),
            (
                "grant_static",
                ContinuousModification::GrantStaticAbility {
                    definition: Box::new(nested_static()),
                },
                ".definition.modifications[0].definition.effect",
            ),
            (
                "copy_values",
                ContinuousModification::CopyValues {
                    values: Box::new(copy_values),
                    display_source: Default::default(),
                    printed_ref: None,
                    token_image_ref: None,
                },
                ".values.abilities[0].effect",
            ),
            (
                "add_keyword",
                ContinuousModification::AddKeyword {
                    keyword: forbidden_search_found_keyword(),
                },
                ".keyword.cost.effect",
            ),
            (
                "remove_keyword",
                ContinuousModification::RemoveKeyword {
                    keyword: forbidden_search_found_keyword(),
                },
                ".keyword.cost.effect",
            ),
        ];

        for (name, modification, breadcrumb) in cases {
            let mut face = test_face(name);
            face.static_abilities
                .push(StaticDefinition::continuous().modifications(vec![modification]));
            let error = validate_card_face_for_export(&face)
                .expect_err("nested runtime effect must fail closed");
            assert!(error.contains(breadcrumb), "{name}: {error}");
            assert!(error.contains("forbidden outside"), "{name}: {error}");
        }
    }

    #[test]
    fn validator_rejects_nested_search_found_effects_in_effect_carriers() {
        let effect_cost = || AbilityCost::EffectCost {
            effect: Box::new(forbidden_search_found_ability().effect.as_ref().clone()),
        };
        let cases = [
            (
                "mana_grant",
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Colorless {
                        count: QuantityExpr::Fixed { value: 1 },
                    },
                    restrictions: vec![],
                    grants: vec![ManaSpellGrant::TriggerOnSpend {
                        filter: TargetFilter::Any,
                        ability: Box::new(forbidden_search_found_ability()),
                    }],
                    expiry: None,
                    target: None,
                },
                ".effect.grants[0].ability.effect",
            ),
            (
                "casting_permission",
                Effect::GrantCastingPermission {
                    permission: CastingPermission::ExileWithAltAbilityCost {
                        cost: effect_cost(),
                        constraint: None,
                        granted_to: None,
                    },
                    target: TargetFilter::Any,
                    grantee: PermissionGrantee::AbilityController,
                },
                ".effect.permission.cost.effect",
            ),
            (
                "copy_token",
                Effect::CopyTokenOf {
                    target: TargetFilter::Any,
                    owner: TargetFilter::Controller,
                    source_filter: None,
                    enters_attacking: false,
                    tapped: false,
                    count: QuantityExpr::Fixed { value: 1 },
                    extra_keywords: vec![forbidden_search_found_keyword()],
                    additional_modifications: vec![],
                },
                ".effect.extra_keywords[0].cost.effect",
            ),
            (
                "animate",
                Effect::Animate {
                    power: None,
                    toughness: None,
                    types: vec![],
                    remove_types: vec![],
                    target: TargetFilter::Any,
                    keywords: vec![forbidden_search_found_keyword()],
                },
                ".effect.keywords[0].cost.effect",
            ),
            (
                "each_player_copy",
                Effect::EachPlayerCopyChosen {
                    choose_filter: TargetFilter::Any,
                    min: 1,
                    max: 1,
                    copy_modifications: vec![ContinuousModification::AddKeyword {
                        keyword: forbidden_search_found_keyword(),
                    }],
                    scale: None,
                    choose_scope: Default::default(),
                },
                ".effect.copy_modifications[0].keyword.cost.effect",
            ),
            (
                "choice",
                Effect::Choose {
                    choice_type: ChoiceType::Keyword {
                        options: vec![forbidden_search_found_keyword()],
                        count: 1,
                    },
                    persist: false,
                    selection: TargetSelectionMode::Chosen,
                },
                ".effect.choice_type.options[0].cost.effect",
            ),
        ];

        for (name, effect, breadcrumb) in cases {
            let mut face = test_face(name);
            face.abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, effect));
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{name}: {error}");
            assert!(error.contains("forbidden outside"), "{name}: {error}");
        }
    }

    #[test]
    fn validator_traverses_replacement_valid_card_keyword_cost_effect_chain() {
        let mut face = test_face("replacement valid card carrier");
        face.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::DamageDone)
                .valid_card(forbidden_search_found_filter()),
        );

        let error = validate_card_face_for_export(&face)
            .expect_err("replacement valid_card must traverse through keyword effect costs");
        assert!(
            error.contains(".replacements[0].valid_card.properties[0].keyword.cost.effect"),
            "{error}"
        );
        assert!(error.contains("forbidden outside"), "{error}");
    }

    #[test]
    fn validator_traverses_required_effect_filter_quantity_and_perpetual_carriers() {
        let cases = vec![
            (
                "draw target",
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: forbidden_search_found_filter(),
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "draw object count filter",
                Effect::Draw {
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: forbidden_search_found_filter(),
                        },
                    },
                    target: TargetFilter::Controller,
                },
                ".effect.count.qty.filter.properties[0].keyword.cost.effect",
            ),
            (
                "pump target",
                Effect::Pump {
                    power: PtValue::Fixed(1),
                    toughness: PtValue::Fixed(1),
                    target: forbidden_search_found_filter(),
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "destroy target",
                Effect::Destroy {
                    target: forbidden_search_found_filter(),
                    cant_regenerate: false,
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "destroy all target",
                Effect::DestroyAll {
                    target: forbidden_search_found_filter(),
                    cant_regenerate: false,
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "change zone conditional filter",
                Effect::ChangeZone {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::Any,
                    owner_library: false,
                    enter_transformed: false,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enters_attacking: false,
                    up_to: false,
                    enter_with_counters: vec![],
                    conditional_enter_with_counters: vec![(
                        forbidden_search_found_filter(),
                        crate::types::counter::CounterType::Plus1Plus1,
                        QuantityExpr::Fixed { value: 1 },
                    )],
                    face_down_profile: None,
                    enters_modified_if: None,
                },
                ".effect.conditional_enter_with_counters[0].filter.properties[0].keyword.cost.effect",
            ),
            (
                "change zone all counter quantity",
                Effect::ChangeZoneAll {
                    origin: Some(Zone::Graveyard),
                    destination: Zone::Battlefield,
                    target: TargetFilter::Any,
                    enters_under: None,
                    enter_tapped: EtbTapState::Unspecified,
                    enter_with_counters: vec![(
                        crate::types::counter::CounterType::Plus1Plus1,
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount {
                                filter: forbidden_search_found_filter(),
                            },
                        },
                    )],
                    face_down_profile: None,
                    library_position: None,
                    random_order: false,
                },
                ".effect.enter_with_counters[0].count.qty.filter.properties[0].keyword.cost.effect",
            ),
            (
                "apply perpetual keywords",
                Effect::ApplyPerpetual {
                    target: TargetFilter::Any,
                    modification: PerpetualModification::GrantKeywords {
                        keywords: vec![forbidden_search_found_keyword()],
                    },
                },
                ".effect.modification.keywords[0].cost.effect",
            ),
            (
                "add target replacement target",
                Effect::AddTargetReplacement {
                    replacement: Box::new(ReplacementDefinition::new(ReplacementEvent::DamageDone)),
                    target: forbidden_search_found_filter(),
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "pay cost payer",
                Effect::PayCost {
                    cost: AbilityCost::Tap,
                    scale: None,
                    payer: forbidden_search_found_filter(),
                },
                ".effect.payer.properties[0].keyword.cost.effect",
            ),
            (
                "cast from zone target",
                Effect::CastFromZone {
                    target: forbidden_search_found_filter(),
                    without_paying_mana_cost: false,
                    mode: CardPlayMode::Cast,
                    cast_transformed: false,
                    alt_ability_cost: None,
                    constraint: None,
                    duration: None,
                    driver: Default::default(),
                    mana_spend_permission: None,
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "next spell filter",
                Effect::GrantNextSpellAbility {
                    modifier: crate::types::game_state::NextSpellModifier::CantBeCountered,
                    player: PlayerScope::Controller,
                    spell_filter: Some(forbidden_search_found_filter()),
                },
                ".effect.spell_filter.properties[0].keyword.cost.effect",
            ),
            (
                "search library filter",
                Effect::SearchLibrary {
                    source_zones: vec![Zone::Library],
                    filter: forbidden_search_found_filter(),
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: false,
                    target_player: None,
                    selection_constraint: Default::default(),
                    split: None,
                },
                ".effect.filter.properties[0].keyword.cost.effect",
            ),
            (
                "search library target player",
                Effect::SearchLibrary {
                    source_zones: vec![Zone::Library],
                    filter: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    reveal: false,
                    target_player: Some(forbidden_search_found_filter()),
                    selection_constraint: Default::default(),
                    split: None,
                },
                ".effect.target_player.properties[0].keyword.cost.effect",
            ),
            (
                "target only",
                Effect::TargetOnly {
                    target: forbidden_search_found_filter(),
                },
                ".effect.target.properties[0].keyword.cost.effect",
            ),
            (
                "additional phase attacker restriction",
                Effect::AdditionalPhase {
                    target: TargetFilter::Controller,
                    phase: Phase::BeginCombat,
                    after: Phase::PreCombatMain,
                    followed_by: vec![],
                    count: QuantityExpr::Fixed { value: 1 },
                    attacker_restriction: Some(forbidden_search_found_filter()),
                },
                ".effect.attacker_restriction.properties[0].keyword.cost.effect",
            ),
        ];

        for (name, effect, breadcrumb) in cases {
            let mut face = test_face(name);
            face.abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, effect));
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{name}: {error}");
            assert!(error.contains("forbidden outside"), "{name}: {error}");
        }
    }

    #[test]
    fn validator_rejects_nested_search_found_effects_in_conditions_and_filters() {
        let mut condition_face = test_face("ability condition");
        condition_face.abilities.push(
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp).condition(
                AbilityCondition::SourceLacksKeyword {
                    keyword: forbidden_search_found_keyword(),
                },
            ),
        );

        let mut restriction_face = test_face("activation restriction");
        restriction_face.abilities.push(
            AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp).activation_restrictions(
                vec![ActivationRestriction::RequiresCondition {
                    condition: Some(ParsedCondition::SourceLacksKeyword {
                        keyword: forbidden_search_found_keyword(),
                    }),
                }],
            ),
        );

        let mut filter_ability = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        filter_ability.target_chooser = Some(TargetFilter::Typed(
            TypedFilter::default().properties(vec![FilterProp::WithKeyword {
                value: forbidden_search_found_keyword(),
            }]),
        ));
        let mut filter_face = test_face("target filter");
        filter_face.abilities.push(filter_ability);

        let resolved = ResolvedAbility::new(Effect::NoOp, vec![], ObjectId(1), PlayerId(0))
            .condition(AbilityCondition::Not {
                condition: Box::new(AbilityCondition::SourceLacksKeyword {
                    keyword: forbidden_search_found_keyword(),
                }),
            });
        let mut resolved_face = test_face("resolved condition");
        resolved_face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::EpicCopy {
                spell: Box::new(resolved),
            },
        ));

        let cases = [
            (
                condition_face,
                ".abilities[0].condition.keyword.cost.effect",
            ),
            (
                restriction_face,
                ".abilities[0].activation_restrictions[0].condition.keyword.cost.effect",
            ),
            (
                filter_face,
                ".abilities[0].target_chooser.properties[0].keyword.cost.effect",
            ),
            (
                resolved_face,
                ".abilities[0].effect.spell.condition.condition.keyword.cost.effect",
            ),
        ];

        for (face, breadcrumb) in cases {
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{}: {error}", face.name);
            assert!(
                error.contains("forbidden outside"),
                "{}: {error}",
                face.name
            );
        }
    }

    #[test]
    fn validator_rejects_nested_search_found_effects_in_static_and_crew_keywords() {
        let mut static_face = test_face("cant have keyword");
        static_face
            .static_abilities
            .push(StaticDefinition::new(StaticMode::CantHaveKeyword {
                keyword: forbidden_search_found_keyword(),
            }));
        let error = validate_card_face_for_export(&static_face)
            .expect_err("static keyword carrier must fail closed");
        assert!(
            error.contains(".static_abilities[0].mode.keyword.cost.effect"),
            "{error}"
        );

        let mut crew_face = test_face("crew restriction");
        crew_face.keywords.push(Keyword::Crew {
            power: 1,
            once_per_turn: Some(Box::new(ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::SourceLacksKeyword {
                    keyword: forbidden_search_found_keyword(),
                }),
            })),
        });
        let error = validate_card_face_for_export(&crew_face)
            .expect_err("Crew restriction carrier must fail closed");
        assert!(
            error.contains(".keywords[0].once_per_turn.condition.keyword.cost.effect"),
            "{error}"
        );
    }

    #[test]
    fn validator_traverses_ward_quantity_condition_and_dynamic_static_carriers() {
        let hostile_quantity = || QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: forbidden_search_found_filter(),
            },
        };

        let mut ward_face = test_face("ward sacrifice filter");
        ward_face.keywords.push(Keyword::Ward(WardCost::Sacrifice {
            count: 1,
            filter: forbidden_search_found_filter(),
        }));

        let mut quantity_condition = test_face("quantity condition");
        quantity_condition.abilities.push(
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp).condition(
                AbilityCondition::QuantityCheck {
                    lhs: hostile_quantity(),
                    comparator: crate::types::ability::Comparator::GT,
                    rhs: QuantityExpr::Fixed { value: 0 },
                },
            ),
        );

        let mut previous_amount = test_face("previous amount condition");
        previous_amount.abilities.push(
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp).condition(
                AbilityCondition::PreviousEffectAmount {
                    comparator: crate::types::ability::Comparator::GT,
                    rhs: hostile_quantity(),
                    channel: Default::default(),
                },
            ),
        );

        let mut modify_cost = test_face("modify cost dynamic count");
        modify_cost
            .static_abilities
            .push(StaticDefinition::new(StaticMode::ModifyCost {
                mode: crate::types::statics::CostModifyMode::Reduce,
                amount: ManaCost::zero(),
                spell_filter: None,
                dynamic_count: match hostile_quantity() {
                    QuantityExpr::Ref { qty } => Some(qty),
                    _ => unreachable!(),
                },
            }));

        let mut reduce_ability_cost = test_face("ability cost dynamic count");
        reduce_ability_cost
            .static_abilities
            .push(StaticDefinition::new(StaticMode::ReduceAbilityCost {
                mode: crate::types::statics::CostModifyMode::Reduce,
                keyword: "activated".to_string(),
                amount: 1,
                minimum_mana: None,
                dynamic_count: match hostile_quantity() {
                    QuantityExpr::Ref { qty } => Some(qty),
                    _ => unreachable!(),
                },
                exemption: crate::types::statics::ActivationExemption::None,
                activator: None,
            }));

        let cases = [
            (ward_face, ".keywords[0].cost.filter.properties[0]"),
            (
                quantity_condition,
                ".abilities[0].condition.lhs.qty.filter.properties[0]",
            ),
            (
                previous_amount,
                ".abilities[0].condition.rhs.qty.filter.properties[0]",
            ),
            (
                modify_cost,
                ".statics[0].mode.dynamic_count.filter.properties[0]",
            ),
            (
                reduce_ability_cost,
                ".statics[0].mode.dynamic_count.filter.properties[0]",
            ),
        ];

        for (face, breadcrumb) in cases {
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{}: {error}", face.name);
            assert!(
                error.contains("forbidden outside"),
                "{}: {error}",
                face.name
            );
        }
    }

    #[test]
    fn validator_traverses_trigger_condition_constraint_and_zone_clause_carriers() {
        let hostile_quantity = || QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: forbidden_search_found_filter(),
            },
        };

        let mut condition_face = test_face("trigger condition");
        condition_face
            .triggers
            .push(TriggerDefinition::new(TriggerMode::SpellCast).condition(
                crate::types::ability::TriggerCondition::QuantityComparison {
                    lhs: hostile_quantity(),
                    comparator: crate::types::ability::Comparator::GT,
                    rhs: QuantityExpr::Fixed { value: 0 },
                },
            ));

        let mut constraint_face = test_face("trigger constraint");
        constraint_face
            .triggers
            .push(TriggerDefinition::new(TriggerMode::SpellCast).constraint(
                crate::types::ability::TriggerConstraint::NthSpellThisTurn {
                    n: 2,
                    filter: Some(forbidden_search_found_filter()),
                },
            ));

        let mut clause_face = test_face("zone clause");
        clause_face.triggers.push(
            TriggerDefinition::new(TriggerMode::ChangesZone).zone_change_clauses(vec![
                crate::types::ability::ZoneChangeClause {
                    origin: crate::types::ability::OriginConstraint::Any,
                    destination: Some(Zone::Graveyard),
                    destination_constraint: crate::types::ability::OriginConstraint::Any,
                    valid_card: Some(forbidden_search_found_filter()),
                },
            ]),
        );

        let cases = [
            (
                condition_face,
                ".triggers[0].condition.lhs.qty.filter.properties[0]",
            ),
            (
                constraint_face,
                ".triggers[0].constraint.filter.properties[0]",
            ),
            (
                clause_face,
                ".triggers[0].zone_change_clauses[0].valid_card.properties[0]",
            ),
        ];
        for (face, breadcrumb) in cases {
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{}: {error}", face.name);
            assert!(
                error.contains("forbidden outside"),
                "{}: {error}",
                face.name
            );
        }
    }

    #[test]
    fn validator_traverses_remaining_non_effect_typed_carriers() {
        let hostile_quantity = || QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: forbidden_search_found_filter(),
            },
        };

        let mut power_face = test_face("face power");
        power_face.power = Some(PtValue::Quantity(hostile_quantity()));

        let mut keyword_face = test_face("keyword quantity");
        keyword_face
            .keywords
            .push(Keyword::Firebending(hostile_quantity()));

        let mut filter_property_face = test_face("filter property quantity");
        filter_property_face
            .static_abilities
            .push(StaticDefinition::continuous().affected(TargetFilter::Typed(
                TypedFilter::default().properties(vec![FilterProp::Cmc {
                    comparator: crate::types::ability::Comparator::GE,
                    value: hostile_quantity(),
                }]),
            )));

        let mut player_filter_face = test_face("player filter quantity");
        let mut player_filter_ability = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        player_filter_ability.player_scope =
            Some(crate::types::ability::PlayerFilter::ControlsCount {
                relation: crate::types::ability::PlayerRelation::All,
                filter: TargetFilter::Any,
                comparator: crate::types::ability::Comparator::GE,
                count: Box::new(hostile_quantity()),
            });
        player_filter_face.abilities.push(player_filter_ability);

        let mut player_attribute_face = test_face("player attribute quantity");
        let mut player_attribute_ability = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        player_attribute_ability.player_scope =
            Some(crate::types::ability::PlayerFilter::PlayerAttribute {
                relation: crate::types::ability::PlayerRelation::All,
                attr: Box::new(match hostile_quantity() {
                    QuantityExpr::Ref { qty } => qty,
                    _ => unreachable!(),
                }),
                comparator: crate::types::ability::Comparator::GE,
                value: Box::new(QuantityExpr::Fixed { value: 0 }),
            });
        player_attribute_face
            .abilities
            .push(player_attribute_ability);

        let mut modal_face = test_face("modal quantity");
        modal_face.modal = Some(crate::types::ability::ModalChoice {
            dynamic_max_choices: Some(hostile_quantity()),
            ..Default::default()
        });

        let mut casting_restriction_face = test_face("casting restriction quantity");
        casting_restriction_face.casting_restrictions.push(
            crate::types::ability::CastingRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: hostile_quantity(),
                    comparator: crate::types::ability::Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 0 },
                }),
            },
        );

        let mut casting_option_face = test_face("casting option quantity");
        casting_option_face.casting_options.push(
            crate::types::ability::SpellCastingOption::free_cast().condition(
                ParsedCondition::QuantityComparison {
                    lhs: hostile_quantity(),
                    comparator: crate::types::ability::Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 0 },
                },
            ),
        );

        let mut solve_face = test_face("solve quantity");
        solve_face.solve_condition = Some(crate::types::ability::SolveCondition::Condition {
            condition: crate::types::ability::StaticCondition::QuantityComparison {
                lhs: hostile_quantity(),
                comparator: crate::types::ability::Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 0 },
            },
        });

        let mut target_constraint_face = test_face("target constraint quantity");
        let mut target_constraint_ability =
            AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        target_constraint_ability.target_constraints.push(
            crate::types::game_state::TargetSelectionConstraint::TotalManaValue {
                comparator: crate::types::ability::Comparator::LE,
                value: hostile_quantity(),
            },
        );
        target_constraint_face
            .abilities
            .push(target_constraint_ability);

        let mut cost_reduction_face = test_face("cost reduction quantity");
        let mut cost_reduction_ability =
            AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp);
        cost_reduction_ability.cost_reduction = Some(crate::types::ability::CostReduction {
            amount_per: 1,
            count: hostile_quantity(),
            condition: None,
        });
        cost_reduction_face.abilities.push(cost_reduction_ability);

        let mut per_turn_cast_face = test_face("per turn cast filter");
        per_turn_cast_face
            .static_abilities
            .push(StaticDefinition::new(StaticMode::PerTurnCastLimit {
                who: crate::types::statics::ProhibitionScope::AllPlayers,
                max: 1,
                spell_filter: Some(forbidden_search_found_filter()),
            }));

        let mut max_untap_face = test_face("max untap filter");
        max_untap_face
            .static_abilities
            .push(StaticDefinition::new(StaticMode::MaxUntapPerType {
                filter: forbidden_search_found_filter(),
                max: 1,
            }));

        let mut unless_pay_face = test_face("unless pay quantity");
        unless_pay_face
            .static_abilities
            .push(StaticDefinition::continuous().condition(
                crate::types::ability::StaticCondition::UnlessPay {
                    cost: ManaCost::zero(),
                    scaling: crate::types::ability::UnlessPayScaling::PerQuantityRef {
                        quantity: match hostile_quantity() {
                            QuantityExpr::Ref { qty } => qty,
                            _ => unreachable!(),
                        },
                    },
                    defended: None,
                },
            ));

        let cases = [
            (power_face, ".power.qty.filter.properties[0]"),
            (
                keyword_face,
                ".keywords[0].quantity.qty.filter.properties[0]",
            ),
            (
                filter_property_face,
                ".statics[0].affected.properties[0].value.qty.filter.properties[0]",
            ),
            (
                player_filter_face,
                ".abilities[0].player_scope.count.qty.filter.properties[0]",
            ),
            (
                player_attribute_face,
                ".abilities[0].player_scope.attr.filter.properties[0]",
            ),
            (
                modal_face,
                ".modal.dynamic_max_choices.qty.filter.properties[0]",
            ),
            (
                casting_restriction_face,
                ".casting_restrictions[0].condition.lhs.qty.filter.properties[0]",
            ),
            (
                casting_option_face,
                ".casting_options[0].condition.lhs.qty.filter.properties[0]",
            ),
            (
                solve_face,
                ".solve_condition.condition.lhs.qty.filter.properties[0]",
            ),
            (
                target_constraint_face,
                ".abilities[0].target_constraints[0].value.qty.filter.properties[0]",
            ),
            (
                cost_reduction_face,
                ".abilities[0].cost_reduction.count.qty.filter.properties[0]",
            ),
            (per_turn_cast_face, ".statics[0].mode.filter.properties[0]"),
            (max_untap_face, ".statics[0].mode.filter.properties[0]"),
            (
                unless_pay_face,
                ".statics[0].condition.scaling.quantity.filter.properties[0]",
            ),
        ];

        for (face, breadcrumb) in cases {
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{}: {error}", face.name);
            assert!(
                error.contains("forbidden outside"),
                "{}: {error}",
                face.name
            );
        }
    }

    #[test]
    fn validator_traverses_remaining_effect_carriers_with_precise_breadcrumbs() {
        let hostile_quantity = || QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: forbidden_search_found_filter(),
            },
        };
        let effect_face = |name: &str, effect: Effect| {
            let mut face = test_face(name);
            face.abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, effect));
            face
        };

        let dig = effect_face(
            "dig keep count expression",
            Effect::Dig {
                player: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                destination: None,
                keep_count: None,
                keep_count_expr: Some(hostile_quantity()),
                up_to: false,
                filter: TargetFilter::Any,
                rest_destination: None,
                reveal: false,
                enter_tapped: false,
                source: Default::default(),
            },
        );
        let exile_until_match = effect_face(
            "exile until matching filter",
            Effect::ExileFromTopUntil {
                player: TargetFilter::Any,
                until: crate::types::ability::UntilCondition::NextMatches {
                    filter: forbidden_search_found_filter(),
                },
            },
        );
        let exile_until_threshold = effect_face(
            "exile until cumulative threshold",
            Effect::ExileFromTopUntil {
                player: TargetFilter::Any,
                until: crate::types::ability::UntilCondition::CumulativeThreshold {
                    property: crate::types::ability::ObjectProperty::ManaValue,
                    comparator: crate::types::ability::Comparator::GE,
                    threshold: hostile_quantity(),
                },
            },
        );
        let counter = effect_face(
            "counter target filter",
            Effect::Counter {
                target: forbidden_search_found_filter(),
                source_rider: None,
                countered_spell_zone: None,
            },
        );
        let prevention_source = effect_face(
            "prevention source filter",
            Effect::PreventDamage {
                amount: crate::types::ability::PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: Default::default(),
                damage_source_filter: Some(forbidden_search_found_filter()),
                prevention_duration: None,
            },
        );
        let prevention_duration = effect_face(
            "prevention duration condition",
            Effect::PreventDamage {
                amount: crate::types::ability::PreventionAmount::All,
                amount_dynamic: None,
                target: TargetFilter::Any,
                scope: Default::default(),
                damage_source_filter: None,
                prevention_duration: Some(crate::types::ability::Duration::ForAsLongAs {
                    condition: crate::types::ability::StaticCondition::IsPresent {
                        filter: Some(forbidden_search_found_filter()),
                    },
                }),
            },
        );
        let damage_modification = effect_face(
            "damage replacement modification",
            Effect::CreateDamageReplacement {
                source_filter: None,
                combat_scope: None,
                target_filter: None,
                modification: Some(DamageModification::Plus {
                    value: hostile_quantity(),
                }),
                redirect_to: None,
                redirect_amount: None,
                redirect_object_filter: None,
                recipient_object_filter: None,
            },
        );
        let meld_source = effect_face(
            "meld source filter",
            Effect::Meld {
                source: "source".to_string(),
                partner: "partner".to_string(),
                result: "result".to_string(),
                source_filter: forbidden_search_found_filter(),
                partner_filter: TargetFilter::Any,
                entry: Default::default(),
            },
        );
        let meld_partner = effect_face(
            "meld partner filter",
            Effect::Meld {
                source: "source".to_string(),
                partner: "partner".to_string(),
                result: "result".to_string(),
                source_filter: TargetFilter::Any,
                partner_filter: forbidden_search_found_filter(),
                entry: Default::default(),
            },
        );

        let cases = [
            (
                dig,
                ".abilities[0].effect.keep_count_expr.qty.filter.properties[0]",
            ),
            (
                exile_until_match,
                ".abilities[0].effect.until.filter.properties[0]",
            ),
            (
                exile_until_threshold,
                ".abilities[0].effect.until.threshold.qty.filter.properties[0]",
            ),
            (counter, ".abilities[0].effect.target.properties[0]"),
            (
                prevention_source,
                ".abilities[0].effect.damage_source_filter.properties[0]",
            ),
            (
                prevention_duration,
                ".abilities[0].effect.prevention_duration.condition.filter.properties[0]",
            ),
            (
                damage_modification,
                ".abilities[0].effect.modification.value.qty.filter.properties[0]",
            ),
            (
                meld_source,
                ".abilities[0].effect.source_filter.properties[0]",
            ),
            (
                meld_partner,
                ".abilities[0].effect.partner_filter.properties[0]",
            ),
        ];

        for (face, breadcrumb) in cases {
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{}: {error}", face.name);
            assert!(
                error.contains("forbidden outside"),
                "{}: {error}",
                face.name
            );
        }
    }

    #[test]
    fn validator_traverses_library_die_counter_aura_keeper_and_conjure_carriers() {
        let hostile_quantity = || QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: forbidden_search_found_filter(),
            },
        };
        let effect_face = |name: &str, effect: Effect| {
            let mut face = test_face(name);
            face.abilities
                .push(AbilityDefinition::new(AbilityKind::Spell, effect));
            face
        };

        let change_zone_all = effect_face(
            "mass library position",
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                target: TargetFilter::Any,
                enters_under: None,
                enter_tapped: Default::default(),
                enter_with_counters: vec![],
                face_down_profile: None,
                library_position: Some(crate::types::ability::LibraryPosition::BeneathTop {
                    depth: hostile_quantity(),
                }),
                random_order: false,
            },
        );
        let put_at_position = effect_face(
            "put at random library position",
            Effect::PutAtLibraryPosition {
                target: TargetFilter::Any,
                count: QuantityExpr::Fixed { value: 1 },
                position: crate::types::ability::LibraryPosition::RandomWithinTop {
                    n: hostile_quantity(),
                },
            },
        );
        let roll_modifier = effect_face(
            "die roll modifier",
            Effect::RollDie {
                count: QuantityExpr::Fixed { value: 1 },
                sides: 20,
                results: vec![],
                modifier: Some(crate::types::ability::DieRollModifier::Add {
                    value: hostile_quantity(),
                }),
            },
        );
        let counter_destination = effect_face(
            "countered spell destination",
            Effect::Counter {
                target: TargetFilter::Any,
                source_rider: None,
                countered_spell_zone: Some(
                    crate::types::ability::SpellStackToGraveyardReplacement::Library {
                        position: crate::types::ability::LibraryPosition::BeneathTop {
                            depth: hostile_quantity(),
                        },
                    },
                ),
            },
        );
        let return_as_aura = effect_face(
            "return as aura enchant filter",
            Effect::ReturnAsAura {
                enchant_filter: forbidden_search_found_filter(),
                grants: vec![],
            },
        );
        let keeper_constraint = effect_face(
            "keeper cardinality constraint",
            Effect::ChooseAndSacrificeRest {
                categories: vec![],
                chooser_scope: Default::default(),
                choose_filter: TargetFilter::Any,
                sacrifice_filter: TargetFilter::Any,
                total_power_cap: None,
                keeper_constraint: Some(crate::types::ability::KeeperConstraint::ExactCount {
                    count: hostile_quantity(),
                }),
            },
        );
        let conjure_position = effect_face(
            "conjure library position",
            Effect::Conjure {
                cards: vec![],
                destination: Zone::Library,
                tapped: false,
                library_position: Some(crate::types::ability::LibraryPosition::RandomWithinTop {
                    n: hostile_quantity(),
                }),
                library_players: None,
            },
        );

        let cases = [
            (
                change_zone_all,
                ".abilities[0].effect.library_position.depth.qty.filter.properties[0]",
            ),
            (
                put_at_position,
                ".abilities[0].effect.position.n.qty.filter.properties[0]",
            ),
            (
                roll_modifier,
                ".abilities[0].effect.modifier.value.qty.filter.properties[0]",
            ),
            (
                counter_destination,
                ".abilities[0].effect.countered_spell_zone.position.depth.qty.filter.properties[0]",
            ),
            (
                return_as_aura,
                ".abilities[0].effect.enchant_filter.properties[0]",
            ),
            (
                keeper_constraint,
                ".abilities[0].effect.keeper_constraint.count.qty.filter.properties[0]",
            ),
            (
                conjure_position,
                ".abilities[0].effect.library_position.n.qty.filter.properties[0]",
            ),
        ];

        for (face, breadcrumb) in cases {
            let error = validate_card_face_for_export(&face)
                .expect_err("nested SearchFound effect must fail closed");
            assert!(error.contains(breadcrumb), "{}: {error}", face.name);
            assert!(
                error.contains("forbidden outside"),
                "{}: {error}",
                face.name
            );
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
    fn from_json_str_rejects_invalid_search_found_contract() {
        let mut face = test_face("Broken Search Replacement");
        face.replacements.push(
            ReplacementDefinition::new(ReplacementEvent::SearchFound).execute(
                search_found_execute(SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Cast,
                    mana_spend_permission: None,
                }),
            ),
        );
        let json = serde_json::to_string(&HashMap::from([(
            "broken search replacement".to_string(),
            face,
        )]))
        .unwrap();

        assert!(CardDatabase::from_json_str(&json).is_err());
    }

    #[test]
    fn from_json_str_enforces_supported_search_found_execution_contract() {
        let valid = || {
            ReplacementDefinition::new(ReplacementEvent::SearchFound).execute(search_found_execute(
                SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: None,
                },
            ))
        };
        let no_op = AbilityDefinition::new(AbilityKind::Spell, Effect::NoOp);
        let mut runtime_execute = valid();
        runtime_execute.runtime_execute = Some(Box::new(ResolvedAbility::new(
            Effect::NoOp,
            vec![],
            ObjectId(1),
            PlayerId(0),
        )));
        let mut consume_on_apply = valid();
        consume_on_apply.consume_on_apply = true;
        let mut legacy_consumed = valid();
        legacy_consumed.is_consumed = true;
        let invalid = [
            valid().execute(no_op.clone()),
            runtime_execute,
            valid().mode(ReplacementMode::Optional {
                decline: Some(Box::new(no_op)),
            }),
            valid().mode(ReplacementMode::MayCost {
                cost: AbilityCost::Tap,
                decline: None,
            }),
            consume_on_apply,
            legacy_consumed,
        ];

        for (index, definition) in invalid.into_iter().enumerate() {
            let mut face = test_face(&format!("Broken Search Replacement {index}"));
            face.replacements.push(definition);
            let json = serde_json::to_string(&HashMap::from([(
                format!("broken search replacement {index}"),
                face,
            )]))
            .unwrap();
            assert!(
                CardDatabase::from_json_str(&json).is_err(),
                "serialized unsupported SearchFound contract {index} must be rejected"
            );
        }

        let mut optional_face = test_face("Supported Optional Search Replacement");
        optional_face
            .replacements
            .push(valid().mode(ReplacementMode::Optional { decline: None }));
        let json = serde_json::to_string(&HashMap::from([(
            "supported optional search replacement".to_string(),
            optional_face,
        )]))
        .unwrap();
        assert!(CardDatabase::from_json_str(&json).is_ok());
    }

    #[test]
    fn from_json_str_rejects_search_found_fields_from_other_event_classes() {
        let valid = || {
            ReplacementDefinition::new(ReplacementEvent::SearchFound).execute(search_found_execute(
                SearchFoundModifier {
                    destination: Zone::Exile,
                    play_mode: CardPlayMode::Play,
                    mana_spend_permission: None,
                },
            ))
        };
        let invalid = [
            {
                let mut definition = valid();
                definition.destination_zone = Some(Zone::Hand);
                ("destination_zone", definition)
            },
            {
                let mut definition = valid();
                definition.damage_modification = Some(DamageModification::Double);
                ("damage_modification", definition)
            },
            {
                let mut definition = valid();
                definition.quantity_modification = Some(QuantityModification::DOUBLE);
                ("quantity_modification", definition)
            },
            {
                let mut definition = valid();
                definition.token_owner_scope = Some(ControllerRef::You);
                ("token_owner_scope", definition)
            },
            {
                let mut definition = valid();
                definition.mana_modification = Some(ManaModification::ReplaceWith {
                    mana_type: ManaType::Black,
                });
                ("mana_modification", definition)
            },
            {
                let mut definition = valid();
                definition.counter_match = Some(CounterMatch::Any);
                ("counter_match", definition)
            },
        ];

        for (field, definition) in invalid {
            let mut face = test_face(&format!("Broken SearchFound {field}"));
            face.replacements.push(definition);
            let json = serde_json::to_string(&HashMap::from([(
                format!("broken searchfound {field}"),
                face,
            )]))
            .unwrap();
            let error = match CardDatabase::from_json_str(&json) {
                Err(error) => error.to_string(),
                Ok(_) => panic!("unsupported event-specific field must fail card-data load"),
            };
            assert!(
                error.contains(field),
                "load error must identify {field}: {error}"
            );
        }
    }

    #[test]
    fn from_json_str_rejects_invalid_search_found_in_cleave_variant() {
        let mut face = test_face("Broken Cleave Search Replacement");
        face.cleave_variant = Some(CleaveVariant {
            replacements: vec![unsupported_search_found()],
            ..CleaveVariant::default()
        });
        let json = serde_json::to_string(&HashMap::from([(
            "broken cleave search replacement".to_string(),
            face,
        )]))
        .unwrap();

        assert!(CardDatabase::from_json_str(&json).is_err());
    }

    #[test]
    fn from_json_str_rejects_invalid_search_found_nested_in_ability_effect() {
        let mut face = test_face("Broken Nested Search Replacement");
        face.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::AddTargetReplacement {
                replacement: Box::new(unsupported_search_found()),
                target: TargetFilter::Any,
            },
        ));
        let json = serde_json::to_string(&HashMap::from([(
            "broken nested search replacement".to_string(),
            face,
        )]))
        .unwrap();

        assert!(CardDatabase::from_json_str(&json).is_err());
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

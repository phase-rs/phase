//! CR 111.1 + CR 111.10 + CR 111.4: Debug-only catalog of pre-defined token
//! presets. Loaded from `crates/engine/data/known-tokens.toml` (committed
//! phase-native source generated from MTGJSON set token data by the
//! `tokens-gen` bin).
//!
//! The catalog is a fixed engine resource — versioned with code, embedded via
//! `include_str!`. Runtime token-art resolution, named-token parsing, token
//! ability materialization, and the debug-create UI all consume this single
//! engine-typed list of bodies.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::types::card::TokenImageRef;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::TokenCharacteristics;

/// CR 111.10: Stable identifier for predefined-ability artifact tokens. Each
/// variant maps to one arm of `effects::token::predefined_token_abilities`,
/// keyed by subtype string. The cross-reference is asserted in tests so a
/// preset's `category` cannot drift from the runtime ability registry.
///
/// Eldrazi Spawn (also keyed by `predefined_token_abilities`) is *not*
/// listed here — Spawn is a Creature subtype, not an artifact token, so
/// `TokenCategory::Creature` covers it. The engine still attaches the
/// spawn ability at create-time via the same subtype-keyed dispatch.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PredefinedTokenKind {
    Treasure,
    Food,
    Gold,
    Clue,
    Blood,
    Powerstone,
    Map,
    Lander,
}

impl PredefinedTokenKind {
    /// The subtype string consulted by
    /// `effects::token::predefined_token_abilities` at create-token time.
    pub fn subtype_str(&self) -> &'static str {
        match self {
            Self::Treasure => "Treasure",
            Self::Food => "Food",
            Self::Gold => "Gold",
            Self::Clue => "Clue",
            Self::Blood => "Blood",
            Self::Powerstone => "Powerstone",
            Self::Map => "Map",
            Self::Lander => "Lander",
        }
    }
}

/// CR 110.4 dispatch for debug grouping. Exhaustive over the shapes the
/// `tokens-gen` converter produces; the converter errors out on any entry
/// that cannot be classified, forcing this enum to grow deliberately rather
/// than via an `Other` catch-all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenCategory {
    /// CR 111.10: Predefined artifact tokens whose abilities are attached at
    /// runtime by `predefined_token_abilities`.
    PredefinedArtifact { kind: PredefinedTokenKind },
    /// CR 302.1: Any token with the Creature core type.
    Creature,
    /// CR 303.1 + CR 303.4: Aura enchantment token (Roles, Curses, etc.).
    Aura,
    /// CR 301.1 + CR 301.5: Equipment artifact token.
    Equipment,
    /// CR 311.1: Vehicle artifact token.
    Vehicle,
    /// CR 303.1: Non-Aura enchantment token.
    Enchantment,
    /// CR 305.1: Land token (manlands, etc.).
    Land,
    /// CR 301.1: Plain artifact token that isn't Equipment, Vehicle, or a
    /// predefined-ability subtype (Book artifacts, custom curiosities, etc.).
    Artifact,
}

/// How completely this preset's body represents the source mtgish entry.
/// `Full` means a vanilla body + simple keywords + (for predefined-ability
/// subtypes) the engine-attached abilities cover the printed rules text.
/// `PartialMissingAbilities` flags presets where the source entry has
/// Trigger/Activated/PermanentLayerEffect/Equip rule trees that phase.rs
/// cannot yet model — debug spawn produces the body without those rules.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PresetFidelity {
    Full,
    PartialMissingAbilities,
}

/// Catalog-only provenance for token P/T values. Runtime token creation still
/// uses concrete `TokenCharacteristics`; this field records when MTGJSON's
/// token entry used source-defined or dynamic P/T text that cannot be widened
/// into a fixed body without inventing rules text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TokenPtProvenance {
    #[default]
    FixedOrAbsent,
    SourceDefinedOrDynamic {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        power: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        toughness: Option<String>,
    },
}

impl TokenPtProvenance {
    pub fn is_fixed_or_absent(&self) -> bool {
        matches!(self, Self::FixedOrAbsent)
    }

    fn is_source_defined_or_dynamic(&self) -> bool {
        matches!(self, Self::SourceDefinedOrDynamic { .. })
    }
}

/// A single debug-spawnable preset. `body` is shared with `TokenSpec`'s
/// characteristics — single source of truth on the body shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPreset {
    pub id: String,
    pub category: TokenCategory,
    pub fidelity: PresetFidelity,
    #[serde(default, skip_serializing_if = "TokenPtProvenance::is_fixed_or_absent")]
    pub pt_provenance: TokenPtProvenance,
    pub body: TokenCharacteristics,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_card_names: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub source_card_refs: Vec<TokenSourceRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_image_ref: Option<TokenImageRef>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set_code: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub set_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collector_number: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub released_at: Option<String>,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub type_line: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rules_text: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenSourceRef {
    pub card_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scryfall_oracle_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scryfall_id: Option<String>,
}

#[derive(Deserialize)]
struct CatalogFile {
    token: Vec<TokenPreset>,
}

/// Embedded catalog data. Path is relative to this source file:
/// `crates/engine/src/game/token_presets.rs` → `crates/engine/data/known-tokens.toml`.
static PRESETS: LazyLock<Vec<TokenPreset>> = LazyLock::new(|| {
    let raw = include_str!("../../data/known-tokens.toml");
    let parsed: CatalogFile = toml::from_str(raw).expect("known-tokens.toml well-formed");
    // Duplicate-id assertion: every preset must be addressable by a unique
    // stable id (used by the FE for selection state and React keys).
    let mut seen = std::collections::HashSet::new();
    for p in &parsed.token {
        assert!(
            seen.insert(p.id.clone()),
            "known-tokens.toml: duplicate preset id `{}`",
            p.id
        );
    }
    parsed.token
});

/// Returns the full set of debug-spawnable token presets, sorted by category
/// then id for stable display order.
pub fn known_token_presets() -> &'static [TokenPreset] {
    &PRESETS
}

pub fn known_token_preset_by_id(id: &str) -> Option<&'static TokenPreset> {
    known_token_presets().iter().find(|preset| preset.id == id)
}

/// CR 111.4: A token's name and subtype(s) are set by the effect that creates
/// it; for named tokens (Vibranium, Mutavault, …) those characteristics live in
/// the predefined catalog. Resolve the full token body by display name so the
/// Oracle parser can lower `"create a [Name] token"` to a complete
/// `Effect::Token` for the *entire class* of registry-defined named tokens,
/// rather than a hardcoded allowlist. Case-insensitive to match Oracle text
/// casing variance. Returns `None` when a display name maps to multiple distinct
/// bodies (common subtype names like Bear / Angel) and no source context
/// disambiguates the intended token.
pub fn known_token_body_by_name(name: &str) -> Option<&'static TokenCharacteristics> {
    known_token_body_by_name_for_source(name, None)
}

/// Source-scoped variant for Oracle parsing: when a display name is ambiguous,
/// prefer the preset linked to the card currently being parsed. Fall back to a
/// global match only if every matching body is identical.
pub fn known_token_body_by_name_for_source(
    name: &str,
    source_name: Option<&str>,
) -> Option<&'static TokenCharacteristics> {
    let name = name.trim();
    if let Some(source_name) = source_name.map(str::trim).filter(|name| !name.is_empty()) {
        let mut source_matches = known_token_presets().iter().filter(|preset| {
            preset.body.display_name.eq_ignore_ascii_case(name)
                && preset
                    .source_card_names
                    .iter()
                    .any(|candidate| candidate.eq_ignore_ascii_case(source_name))
        });
        if let Some(first) = source_matches.next() {
            let first_body = &first.body;
            return source_matches
                .all(|preset| &preset.body == first_body)
                .then_some(first_body);
        }
    }

    unique_token_body_by_name(name)
}

fn unique_token_body_by_name(name: &str) -> Option<&'static TokenCharacteristics> {
    let mut matches = known_token_presets()
        .iter()
        .filter(|preset| preset.body.display_name.eq_ignore_ascii_case(name));
    let first = matches.next()?;
    let first_body = &first.body;
    matches
        .all(|preset| &preset.body == first_body)
        .then_some(first_body)
}

pub fn find_exact_token_ref(
    state: &GameState,
    source_id: ObjectId,
    body: &TokenCharacteristics,
) -> Option<TokenImageRef> {
    find_token_ref_with_mode(state, source_id, body, TokenRefMatchMode::Exact)
}

/// CR 111.10 + CR 702.175a: Resolve a card-linked token preset for a copy
/// token when the copied body exactly matches a catalog entry. Unlike
/// [`find_exact_token_ref`], never skips body matching for a sole
/// `source_related_token_ids` link — Twinflame-style copies keep source P/T and
/// must not route through an offspring 1/1 preset.
pub fn find_card_linked_copy_token_ref(
    state: &GameState,
    source_id: ObjectId,
    body: &TokenCharacteristics,
) -> Option<TokenImageRef> {
    find_token_ref_with_mode(state, source_id, body, TokenRefMatchMode::CardLinkedCopy)
}

#[derive(Clone, Copy)]
enum TokenRefMatchMode {
    /// `CreateToken` path: a unique `source_related_token_ids` link may resolve
    /// only with exact body matching unless the source identity is confirmed.
    Exact,
    /// `CreateToken` path after source oracle/face identity has narrowed the
    /// preset. Source-defined P/T presets may then match concrete runtime P/T.
    SourceLinkedExact,
    /// `CopyTokenOf` path: always require an exact body match so copies that
    /// keep source P/T (Twinflame, Populate) do not inherit offspring presets.
    CardLinkedCopy,
}

fn find_token_ref_with_mode(
    state: &GameState,
    source_id: ObjectId,
    body: &TokenCharacteristics,
    mode: TokenRefMatchMode,
) -> Option<TokenImageRef> {
    let source = state.objects.get(&source_id);
    let related_ids = source
        .map(|obj| obj.source_related_token_ids.as_slice())
        .unwrap_or(&[]);
    let source_oracle =
        source.and_then(|obj| obj.printed_ref.as_ref().map(|r| r.oracle_id.as_str()));
    let name_only_offspring_copy = source.is_some_and(|obj| {
        obj.keywords
            .iter()
            .chain(obj.base_keywords.iter())
            .any(|keyword| matches!(keyword, crate::types::keywords::Keyword::Offspring(_)))
    });
    if matches!(mode, TokenRefMatchMode::CardLinkedCopy)
        && related_ids.is_empty()
        && source_oracle.is_none()
        && !name_only_offspring_copy
    {
        return None;
    }
    let source_face = source.and_then(|obj| obj.printed_ref.as_ref().map(|r| r.face_name.as_str()));
    let source_name = source
        .map(|obj| obj.name.as_str())
        .or_else(|| state.lki_cache.get(&source_id).map(|lki| lki.name.as_str()));

    if related_ids.is_empty() && source_oracle.is_none() && source_name.is_none() {
        return None;
    }

    if !related_ids.is_empty() {
        let related_presets: Vec<_> = related_ids
            .iter()
            .filter_map(|id| known_token_preset_by_id(id))
            .collect();

        if matches!(mode, TokenRefMatchMode::Exact) {
            if let [preset] = related_presets.as_slice() {
                let source_matches = source_oracle.is_some_and(|oracle_id| {
                    token_preset_has_source_ref(preset, oracle_id, source_face)
                });
                let match_mode = if source_matches {
                    TokenRefMatchMode::SourceLinkedExact
                } else {
                    TokenRefMatchMode::Exact
                };
                if (source_matches || source_oracle.is_none())
                    && token_preset_body_matches(preset, body, match_mode)
                {
                    return preset.token_image_ref.clone();
                }
                return None;
            }
        }

        let matches: Vec<_> = if let Some(oracle_id) = source_oracle {
            let match_mode = if matches!(mode, TokenRefMatchMode::Exact) {
                TokenRefMatchMode::SourceLinkedExact
            } else {
                mode
            };
            related_presets
                .into_iter()
                .filter(|preset| token_preset_has_source_ref(preset, oracle_id, source_face))
                .filter(|preset| token_preset_body_matches(preset, body, match_mode))
                .collect()
        } else {
            related_presets
                .into_iter()
                .filter(|preset| token_body_matches(&preset.body, body))
                .collect()
        };
        let first = matches.first()?;
        if !matches
            .iter()
            .skip(1)
            .all(|preset| token_preset_semantics_match(first, preset))
        {
            return None;
        }
        return first.token_image_ref.clone();
    }

    let mut matches = known_token_presets().iter().filter(|preset| {
        if !token_body_matches(&preset.body, body) {
            return false;
        }
        if let Some(oracle_id) = source_oracle {
            return token_preset_has_source_ref(preset, oracle_id, source_face);
        }
        if let Some(name) = source_name {
            return preset
                .source_card_names
                .iter()
                .any(|candidate| candidate.eq_ignore_ascii_case(name));
        }
        false
    });

    let first = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    first.token_image_ref.clone()
}

/// CR 111.10: The engine names a Role token "<Role> Role" (e.g. "Monster Role",
/// the parser/token-creation convention), but catalog presets — generated from
/// MTGJSON — name the same token by its bare role word ("Monster"). Reconcile the
/// trailing " Role" so a "Monster Role" token body matches its "Monster" face
/// preset. Without this a DFC Role token ("Monster // Sorcerer"), whose source
/// card links to BOTH face presets and so skips the single-preset fast path,
/// never resolves an image ref and renders with no art. Non-Role names have no
/// " Role" suffix and are unaffected; the accompanying subtype/type comparison
/// still prevents a Role body from matching a non-Role preset.
fn role_normalized_display_name(name: &str) -> &str {
    name.strip_suffix(" Role").unwrap_or(name)
}

fn token_body_matches(a: &TokenCharacteristics, b: &TokenCharacteristics) -> bool {
    token_body_identity_matches(a, b) && a.power == b.power && a.toughness == b.toughness
}

fn token_body_identity_matches(a: &TokenCharacteristics, b: &TokenCharacteristics) -> bool {
    role_normalized_display_name(&a.display_name) == role_normalized_display_name(&b.display_name)
        && sorted_debug(&a.core_types) == sorted_debug(&b.core_types)
        && sorted_strings(&a.subtypes) == sorted_strings(&b.subtypes)
        && sorted_debug(&a.supertypes) == sorted_debug(&b.supertypes)
        && sorted_debug(&a.colors) == sorted_debug(&b.colors)
        && sorted_debug(&a.keywords) == sorted_debug(&b.keywords)
}

fn token_preset_body_matches(
    preset: &TokenPreset,
    body: &TokenCharacteristics,
    mode: TokenRefMatchMode,
) -> bool {
    token_body_identity_matches(&preset.body, body)
        && ((preset.body.power == body.power && preset.body.toughness == body.toughness)
            || (matches!(mode, TokenRefMatchMode::SourceLinkedExact)
                && preset.pt_provenance.is_source_defined_or_dynamic()))
}

fn token_preset_semantics_match(a: &TokenPreset, b: &TokenPreset) -> bool {
    // Used only to deduplicate source-related presets that already matched the
    // emitted runtime body/rules semantics. P/T provenance is catalog metadata,
    // not a runtime semantic difference after body matching has selected both
    // candidates.
    a.category == b.category
        && a.fidelity == b.fidelity
        && token_body_matches(&a.body, &b.body)
        && a.rules_text == b.rules_text
}

fn token_preset_has_source_ref(
    preset: &TokenPreset,
    oracle_id: &str,
    source_face: Option<&str>,
) -> bool {
    preset.source_card_refs.iter().any(|source_ref| {
        source_ref.scryfall_oracle_id.as_deref() == Some(oracle_id)
            && source_face.is_none_or(|face| {
                source_ref
                    .face_name
                    .as_deref()
                    .is_none_or(|candidate| candidate == face)
            })
    })
}

fn sorted_strings(values: &[String]) -> Vec<&str> {
    let mut out: Vec<&str> = values.iter().map(String::as_str).collect();
    out.sort_unstable();
    out
}

fn sorted_debug<T: std::fmt::Debug>(values: &[T]) -> Vec<String> {
    let mut out: Vec<String> = values.iter().map(|value| format!("{value:?}")).collect();
    out.sort_unstable();
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::card::PrintedCardRef;
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::keywords::Keyword;
    use crate::types::mana::{ManaColor, ManaCost};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    const FIXED_OOZE_PRESET_ID: &str = "25b62fd5-b036-5c64-88fd-8f50d0675e4d";
    const SOURCE_DEFINED_OOZE_PRESET_ID: &str = "1545ee29-d9c1-57ff-acae-431cfd6d60cf";
    const ROT_LIKE_ORACLE_ID: &str = "8f47c236-46b6-47cb-9ea9-7adfef8fd8ce";
    const SLIME_MOLDING_ORACLE_ID: &str = "e01c8122-9159-4f28-ac6c-338bd889650e";
    const FANATIC_OF_RHONAS_PRESET_ID: &str = "001dd45c-851b-5eb3-9a53-fc9fb2c0e322";
    const IRIDESCENT_VINELASHER_PRESET_ID: &str = "c39bbf40-9bf9-5400-8be3-0fb961f0a643";

    fn green_ooze_body(power: Option<i32>, toughness: Option<i32>) -> TokenCharacteristics {
        TokenCharacteristics {
            display_name: "Ooze".to_string(),
            power,
            toughness,
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Ooze".to_string()],
            supertypes: Vec::new(),
            colors: vec![ManaColor::Green],
            keywords: Vec::new(),
        }
    }

    fn fanatic_of_rhonas_body() -> TokenCharacteristics {
        TokenCharacteristics {
            display_name: "Fanatic of Rhonas".to_string(),
            power: Some(4),
            toughness: Some(4),
            core_types: vec![CoreType::Creature],
            subtypes: vec![
                "Zombie".to_string(),
                "Snake".to_string(),
                "Druid".to_string(),
            ],
            supertypes: Vec::new(),
            colors: vec![ManaColor::Black],
            keywords: Vec::new(),
        }
    }

    fn iridescent_vinelasher_offspring_body() -> TokenCharacteristics {
        TokenCharacteristics {
            display_name: "Iridescent Vinelasher".to_string(),
            power: Some(1),
            toughness: Some(1),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Lizard".to_string(), "Assassin".to_string()],
            supertypes: Vec::new(),
            colors: vec![ManaColor::Black],
            keywords: Vec::new(),
        }
    }

    fn state_with_source(
        source_name: &str,
        oracle_id: Option<&str>,
        related_ids: &[&str],
    ) -> (GameState, ObjectId) {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            source_name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        if let Some(oracle_id) = oracle_id {
            obj.printed_ref = Some(PrintedCardRef {
                oracle_id: oracle_id.to_string(),
                face_name: source_name.to_string(),
            });
        }
        obj.source_related_token_ids
            .extend(related_ids.iter().map(|id| (*id).to_string()));
        (state, source)
    }

    /// Forces `LazyLock` evaluation in `cargo test -p engine` so a malformed
    /// `known-tokens.toml`, an unknown `Keyword`/`CoreType`/`ManaColor`
    /// variant, or a duplicate id panics in CI rather than at first
    /// production access.
    #[test]
    fn catalog_loads_and_validates() {
        let presets = known_token_presets();
        assert!(!presets.is_empty(), "catalog must contain entries");
    }

    /// Every `PredefinedArtifact { kind }` preset must carry the matching
    /// subtype string, and the engine's `predefined_token_abilities` must
    /// have a non-empty ability list for that subtype. This invariant binds
    /// the catalog to the runtime ability registry so a kind cannot drift
    /// from its subtype or from its ability factory.
    #[test]
    fn predefined_artifact_subtypes_match_registry() {
        for preset in known_token_presets() {
            if let TokenCategory::PredefinedArtifact { kind } = &preset.category {
                let expected_subtype = kind.subtype_str();
                assert!(
                    preset.body.subtypes.iter().any(|s| s == expected_subtype),
                    "preset {} category PredefinedArtifact {{ {:?} }} but subtypes are {:?}",
                    preset.id,
                    kind,
                    preset.body.subtypes
                );
                assert!(
                    !crate::game::effects::token::predefined_token_abilities(expected_subtype)
                        .is_empty(),
                    "predefined_token_abilities has no arm for {}",
                    expected_subtype
                );
            }
        }
    }

    #[test]
    fn fixed_source_related_token_requires_exact_body_match() {
        let (state, source) = state_with_source(
            "Rot Like the Scum You Are",
            Some(ROT_LIKE_ORACLE_ID),
            &[FIXED_OOZE_PRESET_ID],
        );

        assert!(find_exact_token_ref(&state, source, &green_ooze_body(None, None)).is_none());
        assert!(find_exact_token_ref(&state, source, &green_ooze_body(Some(3), Some(3))).is_none());
    }

    #[test]
    fn fixed_source_related_token_matches_exact_body() {
        let (state, source) = state_with_source(
            "Rot Like the Scum You Are",
            Some(ROT_LIKE_ORACLE_ID),
            &[FIXED_OOZE_PRESET_ID],
        );

        let image = find_exact_token_ref(&state, source, &green_ooze_body(Some(2), Some(2)))
            .expect("fixed Ooze body should match linked preset image");

        assert_eq!(image.preset_id, FIXED_OOZE_PRESET_ID);
    }

    #[test]
    fn source_defined_source_related_token_may_ignore_runtime_pt_for_create_token() {
        let (state, source) = state_with_source(
            "Slime Molding",
            Some(SLIME_MOLDING_ORACLE_ID),
            &[SOURCE_DEFINED_OOZE_PRESET_ID],
        );

        let image = find_exact_token_ref(&state, source, &green_ooze_body(Some(7), Some(7)))
            .expect("source-defined Ooze should match after source identity is known");

        assert_eq!(image.preset_id, SOURCE_DEFINED_OOZE_PRESET_ID);
    }

    #[test]
    fn source_defined_pt_mismatch_is_not_global_or_copy_match() {
        let body = green_ooze_body(Some(7), Some(7));
        let (global_state, global_source) =
            state_with_source("Slime Molding", Some(SLIME_MOLDING_ORACLE_ID), &[]);
        assert!(find_exact_token_ref(&global_state, global_source, &body).is_none());

        let (ambiguous_state, ambiguous_source) =
            state_with_source("Slime Molding", None, &[SOURCE_DEFINED_OOZE_PRESET_ID]);
        assert!(find_exact_token_ref(&ambiguous_state, ambiguous_source, &body).is_none());

        let (copy_state, copy_source) = state_with_source(
            "Slime Molding",
            Some(SLIME_MOLDING_ORACLE_ID),
            &[SOURCE_DEFINED_OOZE_PRESET_ID],
        );
        assert!(find_card_linked_copy_token_ref(&copy_state, copy_source, &body).is_none());
    }

    #[test]
    fn card_linked_copy_does_not_use_name_only_source_fallback() {
        let body = fanatic_of_rhonas_body();
        let (state, source) = state_with_source("Fanatic of Rhonas", None, &[]);

        assert!(find_card_linked_copy_token_ref(&state, source, &body).is_none());
    }

    #[test]
    fn offspring_card_linked_copy_keeps_name_only_source_fallback() {
        let body = iridescent_vinelasher_offspring_body();
        let (mut state, source) = state_with_source("Iridescent Vinelasher", None, &[]);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .keywords
            .push(Keyword::Offspring(ManaCost::generic(2)));

        let image = find_card_linked_copy_token_ref(&state, source, &body)
            .expect("offspring copy token may use name-only source context");

        assert_eq!(image.preset_id, IRIDESCENT_VINELASHER_PRESET_ID);
    }

    #[test]
    fn card_linked_copy_matches_related_token_body_without_printed_ref() {
        let body = fanatic_of_rhonas_body();
        let (state, source) =
            state_with_source("Fanatic of Rhonas", None, &[FANATIC_OF_RHONAS_PRESET_ID]);

        let image = find_card_linked_copy_token_ref(&state, source, &body)
            .expect("related Fanatic copy token body should match linked preset");

        assert_eq!(image.preset_id, FANATIC_OF_RHONAS_PRESET_ID);
    }

    #[test]
    fn exact_create_token_keeps_name_only_source_fallback() {
        let body = fanatic_of_rhonas_body();
        let (state, source) = state_with_source("Fanatic of Rhonas", None, &[]);

        let image = find_exact_token_ref(&state, source, &body)
            .expect("exact create-token lookup may use name-only source context");

        assert_eq!(image.preset_id, FANATIC_OF_RHONAS_PRESET_ID);
    }
}

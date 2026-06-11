//! CR 111.1 + CR 111.10 + CR 111.4: Debug-only catalog of pre-defined token
//! presets. Loaded from `crates/engine/data/known-tokens.toml` (committed
//! phase-native source generated from MTGJSON set token data by the
//! `tokens-gen` bin).
//!
//! The catalog is a fixed engine resource — versioned with code, embedded via
//! `include_str!`. Frontend reads it through a single WASM export and renders
//! a debug-create dropdown grouped by `TokenCategory`. No game logic
//! consumes presets; the catalog exists purely to give the debug UI a
//! discoverable, engine-typed list of bodies.

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

/// A single debug-spawnable preset. `body` is shared with `TokenSpec`'s
/// characteristics — single source of truth on the body shape.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokenPreset {
    pub id: String,
    pub category: TokenCategory,
    pub fidelity: PresetFidelity,
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
    /// without a body match when the card names exactly one preset.
    Exact,
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
    let source_face = source.and_then(|obj| obj.printed_ref.as_ref().map(|r| r.face_name.as_str()));
    let source_name = source
        .map(|obj| obj.name.as_str())
        .or_else(|| state.lki_cache.get(&source_id).map(|lki| lki.name.as_str()));

    if related_ids.is_empty() && source_oracle.is_none() && source_name.is_none() {
        return None;
    }

    if !related_ids.is_empty() {
        let mut matches = known_token_presets()
            .iter()
            .filter(|preset| related_ids.iter().any(|id| id == &preset.id));

        if matches!(mode, TokenRefMatchMode::Exact) {
            let first = matches.next()?;
            if matches.next().is_none() {
                return first.token_image_ref.clone();
            }
        }

        let mut matches = known_token_presets().iter().filter(|preset| {
            related_ids.iter().any(|id| id == &preset.id) && token_body_matches(&preset.body, body)
        });
        let first = matches.next()?;
        if matches.next().is_some() {
            return None;
        }
        return first.token_image_ref.clone();
    }

    let mut matches = known_token_presets().iter().filter(|preset| {
        if !token_body_matches(&preset.body, body) {
            return false;
        }
        if let Some(oracle_id) = source_oracle {
            return preset.source_card_refs.iter().any(|source_ref| {
                source_ref.scryfall_oracle_id.as_deref() == Some(oracle_id)
                    && source_face.is_none_or(|face| {
                        source_ref
                            .face_name
                            .as_deref()
                            .is_none_or(|candidate| candidate == face)
                    })
            });
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

fn token_body_matches(a: &TokenCharacteristics, b: &TokenCharacteristics) -> bool {
    a.display_name == b.display_name
        && a.power == b.power
        && a.toughness == b.toughness
        && sorted_debug(&a.core_types) == sorted_debug(&b.core_types)
        && sorted_strings(&a.subtypes) == sorted_strings(&b.subtypes)
        && sorted_debug(&a.supertypes) == sorted_debug(&b.supertypes)
        && sorted_debug(&a.colors) == sorted_debug(&b.colors)
        && sorted_debug(&a.keywords) == sorted_debug(&b.keywords)
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
}

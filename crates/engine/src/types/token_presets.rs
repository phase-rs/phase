//! CR 111.1 + CR 111.10 + CR 111.4: Debug-only catalog of pre-defined token
//! presets. Sourced from `crates/engine/data/known-tokens.toml` (committed
//! phase-native source generated from MTGJSON set token data by the
//! `tokens-gen` bin), converted to JSON at build time by `build.rs`, and
//! embedded via `include_bytes!` — the debug-mode toml_edit parse of the
//! ~5.9MB catalog cost ~1s per test process under nextest's
//! process-per-test model.
//!
//! The catalog is a fixed engine resource — versioned with code. Runtime
//! token-art resolution, named-token parsing, token ability materialization,
//! and the debug-create UI all consume this single engine-typed list of
//! bodies.

use std::sync::LazyLock;

use serde::{Deserialize, Deserializer, Serialize};

use crate::types::card::TokenImageRef;
use crate::types::proposed_event::TokenCharacteristics;

/// CR 111.10: Stable identifier for predefined-ability artifact tokens. Each
/// variant maps to one arm of `effects::token::predefined_token_abilities`,
/// keyed by subtype string. The cross-reference is asserted in tests so a
/// preset's `category` cannot drift from the runtime ability registry.
///
/// The CR 701.71a Jace planeswalker token is *not* listed here:
/// `TokenCategory::Planeswalker` carries it, and its loyalty abilities are
/// keyed into `predefined_token_abilities` by the `Jace` subtype.
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
    /// CR 301.7: Vehicle artifact token.
    Vehicle,
    /// CR 303.1: Non-Aura enchantment token.
    Enchantment,
    /// CR 305.1: Land token (manlands, etc.).
    Land,
    /// CR 306.5 + CR 306.3: Any token with the Planeswalker core type — loyalty is a
    /// characteristic only planeswalkers have, so only these presets have a `Some`
    /// loyalty. Its loyalty abilities are attached at
    /// runtime by `predefined_token_abilities`, keyed by planeswalker subtype
    /// (CR 306.3) — not the `PredefinedArtifact` route, because
    /// `PredefinedTokenKind` enumerates artifact subtypes only.
    Planeswalker,
    /// CR 301.1: Plain artifact token that isn't Equipment, Vehicle, or a
    /// predefined-ability subtype (Book artifacts, custom curiosities, etc.).
    Artifact,
}

/// How completely this preset's body represents its source data.
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

    pub(crate) fn is_source_defined_or_dynamic(&self) -> bool {
        matches!(self, Self::SourceDefinedOrDynamic { .. })
    }
}

/// A single debug-spawnable preset. `body` is shared with `TokenSpec`'s
/// characteristics — single source of truth on the body shape.
///
/// Unknown keys are refused by `deserialize_catalog`, not by per-struct serde
/// attributes: the file's shape is a tree of nested tables, and an attribute
/// list only covers the ones someone remembered to name.
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

/// The shape of `known-tokens.toml` and of the hand-authored overlay beside
/// it: one authority for the catalog file, on both serde sides.
#[derive(Serialize, Deserialize)]
struct CatalogFile {
    token: Vec<TokenPreset>,
}

/// Deserialize a catalog file, refusing it if serde ignored any key.
///
/// Every field in the file is optional or defaulted, so a misspelled key would
/// otherwise deserialize to its default and lose the authored value. The check
/// names no table, so it covers every table serde itself walks, including any
/// added to the shape later. It is blind where a type deserializes through a
/// self-describing capture instead: `Keyword` reads a `serde_json::Value`, so
/// an unknown key inside a parameterized keyword payload is never reported —
/// issue #7234's drift class, unreachable while every keyword in the committed
/// catalog is a plain string.
fn deserialize_catalog<'de, D>(deserializer: D) -> Result<CatalogFile, String>
where
    D: Deserializer<'de>,
{
    let mut ignored: Vec<String> = Vec::new();
    let file: CatalogFile =
        serde_ignored::deserialize(deserializer, |path| ignored.push(path.to_string()))
            .map_err(|e| e.to_string())?;
    if ignored.is_empty() {
        return Ok(file);
    }
    // Every ignored key, not just the first: a hand-edited row usually carries
    // its typos together, and one regen per typo is a bad loop to be in.
    let named: Vec<String> = ignored
        .iter()
        .map(|path| match row_id_for_path(&file, path) {
            Some(id) => format!("`{path}` (row `{id}`)"),
            None => format!("`{path}`"),
        })
        .collect();
    Err(format!(
        "unknown key(s) {} — a key serde does not recognize keeps its default, \
         silently dropping what was authored",
        named.join(", ")
    ))
}

/// The `id` of the row an ignored key sits under, for the error message.
fn row_id_for_path<'a>(file: &'a CatalogFile, path: &str) -> Option<&'a str> {
    // `serde_ignored` renders a sequence index as a dotted segment: `token.3.body.powr`.
    let index: usize = path
        .strip_prefix("token.")?
        .split('.')
        .next()?
        .parse()
        .ok()?;
    file.token.get(index).map(|row| row.id.as_str())
}

/// Read a catalog file from the JSON `build.rs` converts the committed TOML
/// into. `.end()` because a `&mut Deserializer` stops after the first document
/// and would otherwise accept whatever follows a truncated or double-written
/// artifact — the plain `serde_json::from_slice` this replaced did reject it.
fn parse_catalog_json(raw: &[u8]) -> Result<CatalogFile, String> {
    let mut deserializer = serde_json::Deserializer::from_slice(raw);
    let file = deserialize_catalog(&mut deserializer)?;
    deserializer.end().map_err(|e| e.to_string())?;
    Ok(file)
}

/// Read catalog rows from a catalog file's TOML (`tokens-gen --overlay`).
pub fn parse_overlay(raw: &str) -> Result<Vec<TokenPreset>, String> {
    deserialize_catalog(toml::Deserializer::new(raw)).map(|file| file.token)
}

/// Render catalog rows back into a catalog file's TOML (`tokens-gen --output`),
/// in the order given: `merge_overlay` owns the id-ascending order that
/// `catalog_is_ordered_and_self_consistent` asserts.
pub fn serialize_catalog(token: Vec<TokenPreset>) -> Result<String, String> {
    toml::to_string_pretty(&CatalogFile { token }).map_err(|e| e.to_string())
}

/// Embedded catalog data: `data/known-tokens.toml` converted to JSON in
/// `OUT_DIR` by `build.rs` (structural conversion — same serde shape).
static PRESETS: LazyLock<Vec<TokenPreset>> = LazyLock::new(|| {
    let raw = include_bytes!(concat!(env!("OUT_DIR"), "/known-tokens.json"));
    let parsed = parse_catalog_json(raw)
        .expect("build.rs-converted known-tokens.json well-formed and free of unknown keys");
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

/// Returns the full set of debug-spawnable token presets, in catalog order:
/// ascending id, which is what `tokens-gen` emits so that a regen produces a
/// minimal diff (asserted by `catalog_is_ordered_and_self_consistent`). Ids are
/// MTGJSON uuids, so this is not a display order — consumers impose their own
/// (the debug UI groups by category and sorts by power/toughness/name).
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

#[cfg(test)]
mod tests {
    use super::*;

    /// One catalog row exercising every nested table the file's shape has:
    /// the row itself, `[token.body]`, `[token.token_image_ref]`,
    /// `[token.pt_provenance.*]` and `[[token.source_card_refs]]`.
    const PROBE_ROW: &str = r#"
[[token]]
id = "probe-id"
category = "Creature"
fidelity = "Full"
set_code = "FRA"

[token.pt_provenance.SourceDefinedOrDynamic]
power = "*"
toughness = "*"

[token.body]
display_name = "Wurm"
power = 1
toughness = 1
core_types = ["Creature"]
subtypes = ["Wurm"]
supertypes = []
colors = ["Green"]
keywords = []

[token.token_image_ref]
scryfall_id = "probe-scryfall-id"
preset_id = "probe-id"

[[token.source_card_refs]]
card_name = "Wurm Maker"
scryfall_oracle_id = "11111111-1111-1111-1111-111111111111"
"#;

    #[test]
    fn a_misspelled_key_in_any_table_is_refused_rather_than_silently_dropped() {
        // Live control: the unmutated row parses, so every `Err` below is the
        // check firing and not a fixture that never parsed.
        let parsed = parse_overlay(PROBE_ROW).expect("the probe row parses");
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed[0].body.power, Some(1));
        assert!(parsed[0].token_image_ref.is_some());

        // A member in each table of the shape, with the dotted path the error
        // must name. The provenance leg is refused by `toml`'s own struct-variant
        // table check rather than by the ignored-key callback; both are refusals,
        // which is the property under test.
        // Each member either renames an optional key or adds one; renaming a
        // required key would red as `missing field` and prove nothing about
        // unknown keys. The expected text cannot be produced by the correct
        // spelling — `toughnes` alone is a substring of `toughness`.
        for (needle, replacement, expected) in [
            ("set_code = ", "set_cod = ", "token.0.set_cod"),
            ("power = 1", "powr = 1", "token.0.body.powr"),
            (
                "preset_id = \"probe-id\"",
                "preset_id = \"probe-id\"\nfacename = \"x\"",
                "token.0.token_image_ref.?.facename",
            ),
            (
                "toughness = \"*\"",
                "toughnes = \"*\"",
                "unexpected keys in table: toughnes",
            ),
        ] {
            let mutated = PROBE_ROW.replacen(needle, replacement, 1);
            assert_ne!(
                mutated, PROBE_ROW,
                "the `{needle}` mutation matched nothing"
            );
            let err = parse_overlay(&mutated).expect_err(
                "an unrecognized key keeps its default, so without this check the authored \
                 value is dropped and the row merges anyway",
            );
            assert!(
                err.contains(expected),
                "error must name `{expected}`: {err}"
            );
        }
    }

    /// `PROBE_ROW` in the JSON shape `build.rs` writes the committed catalog
    /// into, so the JSON parse site is exercised on the same nested tables.
    fn probe_row_json() -> String {
        let token = parse_overlay(PROBE_ROW).expect("the probe row parses");
        serde_json::to_string(&CatalogFile { token }).expect("a catalog file serializes as JSON")
    }

    #[test]
    fn catalog_json_with_an_unknown_key_is_refused() {
        // This proves `parse_catalog_json` refuses unknown keys on a JSON
        // deserializer. It cannot prove the `PRESETS` site calls it: that
        // artifact is `include_bytes!`d at compile time and no test can mutate
        // it. `build.rs` converts through a tolerant `toml::Value`, so this
        // function is the only unknown-key guard the embedded catalog has.
        let json = probe_row_json();
        // Live control: the unmutated document parses.
        assert_eq!(
            parse_catalog_json(json.as_bytes())
                .expect("the probe document parses")
                .token
                .len(),
            1
        );

        let mutated = json.replacen(r#""power":1"#, r#""powr":1"#, 1);
        assert_ne!(mutated, json, "the `power` mutation matched nothing");
        let Err(err) = parse_catalog_json(mutated.as_bytes()) else {
            panic!("a plain `serde_json::from_slice` here accepts the unknown key");
        };
        assert!(err.contains("token.0.body.powr"), "{err}");
    }

    #[test]
    fn catalog_json_with_trailing_bytes_is_refused() {
        let json = probe_row_json();
        // Live control: the same document without the appended bytes parses.
        assert!(parse_catalog_json(json.as_bytes()).is_ok());

        let appended = format!("{json}{json}");
        let Err(err) = parse_catalog_json(appended.as_bytes()) else {
            panic!("without `.end()` a `&mut Deserializer` stops after the first document");
        };
        assert!(err.contains("trailing"), "{err}");
    }

    #[test]
    fn every_ignored_key_is_named_not_just_the_first() {
        let mutated =
            PROBE_ROW
                .replacen("set_code = ", "set_cod = ", 1)
                .replacen("power = 1", "powr = 1", 1);
        let err = parse_overlay(&mutated).expect_err("both keys are unknown");
        assert!(err.contains("token.0.set_cod"), "{err}");
        assert!(err.contains("token.0.body.powr"), "{err}");
        assert!(
            err.contains("probe-id"),
            "the error must name the row: {err}"
        );
    }

    #[test]
    fn the_committed_catalog_and_overlay_pass_the_same_check() {
        // The embedded catalog is loaded through `deserialize_catalog`, so its
        // presence here *is* the zero-ignored-keys result over the real corpus.
        let presets = known_token_presets();
        assert!(!presets.is_empty(), "the catalog loaded empty");
        // The corpus must actually contain the nested tables, or this control
        // walks a shape the check was never exercised against.
        assert!(
            presets.iter().any(|p| p.token_image_ref.is_some()),
            "no `[token.token_image_ref]` table in the corpus"
        );
        assert!(
            presets
                .iter()
                .any(|p| p.pt_provenance.is_source_defined_or_dynamic()),
            "no `[token.pt_provenance.SourceDefinedOrDynamic]` table in the corpus"
        );
        // The overlay may hold no rows (its healthy steady state between preview
        // seasons), so only its parse is asserted here; the probe-row tests above
        // keep the unknown-key check itself under test.
        parse_overlay(include_str!("../../data/known-tokens.overlay.toml"))
            .expect("the committed overlay passes the same check");
    }
}

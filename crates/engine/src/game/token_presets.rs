//! Game-side token catalog consumers: overlay merge for `tokens-gen` and
//! `GameState`-scoped token-art reference resolution. The catalog itself
//! (schema, embed, by-name lookup) lives in `crate::types::token_presets`.

use crate::types::card::TokenImageRef;
use crate::types::card_type::CoreType;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::proposed_event::TokenCharacteristics;

pub use crate::types::token_presets::{
    known_token_body_by_name, known_token_body_by_name_for_source, known_token_preset_by_id,
    known_token_presets, parse_overlay, serialize_catalog, PredefinedTokenKind, PresetFidelity,
    TokenCategory, TokenPreset, TokenPtProvenance, TokenSourceRef,
};

/// What `tokens-gen` did with one `known-tokens.overlay.toml` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayRowOutcome {
    /// Nothing else in the catalog claims this token; the row is in.
    Applied,
    /// The row is in, but another catalog row — a generated preset, or an
    /// earlier overlay row — shares its token name and exactly one half of the
    /// key that would retire it: its set, or its source card. Either MTGJSON
    /// published this token without linking it back to the card that creates
    /// it, or published it under a set code the row does not name, or the
    /// overlay names the token twice, or the name simply collides. A human
    /// decides which.
    AppliedShadowed { by_id: String },
    /// Another catalog row already is this token; it wins and this row is
    /// dropped. When that row is a generated preset, MTGJSON now ships the
    /// token and the overlay row should be deleted.
    Superseded { by_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OverlayRowReport {
    pub overlay_id: String,
    pub set_code: String,
    pub display_name: String,
    pub outcome: OverlayRowOutcome,
}

/// The overlay row's `source_card_refs` entries that name a Scryfall oracle id.
/// A blank one is not an id: it can never equal an MTGJSON entry's.
fn row_source_oracle_ids(row: &TokenPreset) -> impl Iterator<Item = &str> {
    row.source_card_refs
        .iter()
        .filter_map(|source_ref| source_ref.scryfall_oracle_id.as_deref())
        .map(str::trim)
        .filter(|oracle_id| !oracle_id.is_empty())
}

/// Same token name, folded the way `known_token_body_by_name_for_source`
/// folds display names — one authority for the name leg, shared by the
/// supersession key and the mis-keyed-set advisory.
fn same_display_name(a: &TokenPreset, b: &TokenPreset) -> bool {
    a.body
        .display_name
        .trim()
        .eq_ignore_ascii_case(b.body.display_name.trim())
}

/// Same set and same token name. Both legs fold alike — case-insensitively and
/// ignoring surrounding whitespace — so a row authored as `fra ` still names
/// the set MTGJSON publishes as `FRA`.
fn names_same_token(a: &TokenPreset, b: &TokenPreset) -> bool {
    a.set_code.trim().eq_ignore_ascii_case(b.set_code.trim()) && same_display_name(a, b)
}

/// Why this hand-authored row must be refused at the boundary, if it must.
///
/// Two classes. A row that leaves any leg of the token-identity key blank — set
/// code, display name, or a shared source-card oracle id — is unretirable by
/// construction: an overlay `id` is a placeholder the real MTGJSON entry will
/// not share, so supersession rides entirely on that key, and such a row would
/// outlive the MTGJSON data it stands in for with nothing able to report it
/// stale. A creature row with no P/T is the other: CR 208.1 gives every
/// creature a power and a toughness, and serde reads an omitted one as `None`,
/// so the row would merge P/T-less rather than fail.
fn boundary_refusal_reason(row: &TokenPreset) -> Option<&'static str> {
    if row.set_code.trim().is_empty() {
        return Some("names no `set_code`");
    }
    if row.body.display_name.trim().is_empty() {
        return Some("names no `body.display_name`");
    }
    if row_source_oracle_ids(row).next().is_none() {
        return Some("carries no `source_card_refs` entry with a non-blank `scryfall_oracle_id`");
    }
    // CR 208.1: a creature card carries both. Only a source-defined or dynamic
    // P/T may leave them out, because the creating card supplies them instead.
    if row.body.core_types.contains(&CoreType::Creature)
        && !row.pt_provenance.is_source_defined_or_dynamic()
        && (row.body.power.is_none() || row.body.toughness.is_none())
    {
        return Some(
            "is a creature body missing `body.power` or `body.toughness` without declaring a \
             source-defined or dynamic `pt_provenance`",
        );
    }
    None
}

/// Merge hand-authored presets into a generated catalog.
///
/// `known-tokens.toml` is rewritten whole on every regen, so a row MTGJSON has
/// no source for cannot survive in it; `known-tokens.overlay.toml` is the
/// authored input that does. A row is superseded when the catalog already holds
/// a preset with the same `id` — the identity `PRESETS` requires to be unique —
/// or with the same token *identity*: same set and same display name (both
/// folded for case and surrounding whitespace) plus a shared
/// `source_card_refs` `scryfall_oracle_id`. An overlay row's own `id` is a
/// placeholder the real MTGJSON entry will not share, so the id leg alone
/// cannot supersede anything; the creating card is what makes the name leg an
/// identity rather than a label. A row leaving any leg of that key blank is
/// refused — see `boundary_refusal_reason`.
///
/// The scan runs over the growing output, so overlay rows deduplicate against
/// each other exactly as they do against generated data.
///
/// Returns the merged catalog in ascending-`id` order — the order `tokens-gen`
/// serializes and `catalog_is_ordered_and_self_consistent` asserts — with one
/// report per overlay row.
pub fn merge_overlay(
    generated: Vec<TokenPreset>,
    overlay: Vec<TokenPreset>,
) -> Result<(Vec<TokenPreset>, Vec<OverlayRowReport>), String> {
    let mut presets = generated;
    let mut reports = Vec::with_capacity(overlay.len());

    for row in overlay {
        if let Some(reason) = boundary_refusal_reason(&row) {
            return Err(format!(
                "overlay row `{}` ({} / {}) {reason}, so no generated preset could ever \
                 supersede it; a hand-authored row must name its set, its token, and the \
                 card it stands in for",
                row.id, row.set_code, row.body.display_name
            ));
        }

        let mut superseded_by: Option<String> = None;
        let mut shadowed_by: Option<String> = None;
        for preset in &presets {
            let same_name = names_same_token(preset, &row);
            let shares_source = row_source_oracle_ids(&row)
                .any(|oracle_id| token_preset_has_source_ref(preset, oracle_id, None));
            let supersedes = preset.id == row.id || (same_name && shares_source);
            // Advisory when the row names the same token but matches only half
            // the key: the set without the source card, or — the mis-keyed case
            // — the source card under a set code this preset does not carry.
            // Never a refusal: a card reprinted across sets lands here too, and
            // a false positive must cost one printed line and nothing more.
            let shadows = same_name || (shares_source && same_display_name(preset, &row));
            // Lowest id wins rather than first-seen, so a report names the same
            // preset whatever order `tokens-gen`'s directory walk produced.
            let slot = if supersedes {
                &mut superseded_by
            } else if shadows {
                &mut shadowed_by
            } else {
                continue;
            };
            if slot.as_deref().is_none_or(|id| preset.id.as_str() < id) {
                *slot = Some(preset.id.clone());
            }
        }

        let overlay_id = row.id.clone();
        let set_code = row.set_code.clone();
        let display_name = row.body.display_name.clone();
        let outcome = match superseded_by {
            Some(by_id) => OverlayRowOutcome::Superseded { by_id },
            None => {
                let outcome = match shadowed_by {
                    Some(by_id) => OverlayRowOutcome::AppliedShadowed { by_id },
                    None => OverlayRowOutcome::Applied,
                };
                presets.push(row);
                outcome
            }
        };
        reports.push(OverlayRowReport {
            overlay_id,
            set_code,
            display_name,
            outcome,
        });
    }

    presets.sort_by(|a, b| a.id.cmp(&b.id));
    Ok((presets, reports))
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

    let source_gated: Vec<&TokenPreset> = known_token_presets()
        .iter()
        .filter(|preset| {
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
        })
        .collect();
    if !source_gated.is_empty() {
        // Same CR 111.10 dedup the related-ids path applies: multiple presets
        // that are semantically identical differ only in printing — the pick
        // is presentation-only, so take the first deterministically instead of
        // silently resolving nothing (a Role exists on two flip sheets, so a
        // source listed on both used to land here).
        return semantically_unique_ref(&source_gated);
    }

    // No preset lists this source. When EVERY body-matching preset is
    // semantically identical, the token's identity is fully determined by its
    // body alone (the live face: a Role token — CR 111.10 fixes all seven
    // kinds by name, and `role_normalized_display_name` already reconciled the
    // engine's "<Role> Role" naming). The source gate would add nothing but a
    // silent `None`, which strands the display on a name search no printing
    // can satisfy (#7552). Bodies with semantically DIFFERENT presets (art
    // variants with different abilities) still resolve nothing here — the
    // gate's ambiguity protection is preserved.
    let body_only: Vec<&TokenPreset> = known_token_presets()
        .iter()
        .filter(|preset| token_body_matches(&preset.body, body))
        .collect();
    semantically_unique_ref(&body_only)
}

/// The shared "all remaining candidates agree" reduction: `Some` ref iff every
/// preset in `matches` is semantically identical to the first — the choice is
/// then presentation-only and deterministic. Empty or disagreeing sets resolve
/// nothing.
fn semantically_unique_ref(matches: &[&TokenPreset]) -> Option<TokenImageRef> {
    let first = matches.first()?;
    if !matches
        .iter()
        .skip(1)
        .all(|preset| token_preset_semantics_match(first, preset))
    {
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
        // Folded like both name legs of the key. Every id the catalog carries is
        // lowercase, so this can only admit an authored id typed in upper hex.
        source_ref
            .scryfall_oracle_id
            .as_deref()
            .is_some_and(|candidate| candidate.eq_ignore_ascii_case(oracle_id))
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
            loyalty: None,
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
            loyalty: None,
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
            loyalty: None,
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

    /// Forces `LazyLock` evaluation in `cargo test -p phase-engine` so an unknown
    /// `Keyword`/`CoreType`/`ManaColor` variant or a duplicate id panics in
    /// CI rather than at first production access. (Malformed TOML fails
    /// earlier, in `build.rs`; the structural conversion there cannot catch
    /// these typed-schema violations.)
    #[test]
    fn catalog_loads_and_validates() {
        let presets = known_token_presets();
        assert!(!presets.is_empty(), "catalog must contain entries");
    }

    /// Structural properties of `tokens-gen` output, checked against the
    /// committed catalog. Both are per-entry properties with no pinned totals,
    /// so a weekly refresh that only adds tokens stays green.
    ///
    /// Scope, stated precisely because it is narrow: these catch entries
    /// *reordered* relative to generator output, and an image ref pointing at a
    /// different preset. They do NOT catch a body-only rewrite (nothing here
    /// ties `body`/`fidelity`/`source_card_refs` to an identity) nor a deleted
    /// entry (removing from the middle keeps the sequence ascending) — a
    /// deletion is the count floors' job, in
    /// `analyze_token_coverage_treats_source_defined_pt_as_represented`.
    ///
    /// And none of it establishes provenance — that the catalog is tokens-gen's
    /// output for its declared vintage — which needs the gitignored generator
    /// input, absent at test time. See the comment on that coverage test for
    /// the by-hand `tokens-gen`/`cmp` recipe.
    #[test]
    fn catalog_is_ordered_and_self_consistent() {
        let presets = known_token_presets();

        // `tokens_gen.rs` emits `presets.sort_by(|a, b| a.id.cmp(&b.id))`, so a
        // strictly ascending id sequence is a property of every generated
        // catalog. Strict (not `<=`) also re-states the LazyLock's duplicate-id
        // guard at the ordering level.
        for pair in presets.windows(2) {
            assert!(
                pair[0].id < pair[1].id,
                "known-tokens.toml is not strictly id-ascending: `{}` precedes `{}`",
                pair[0].id,
                pair[1].id
            );
        }

        // The generator builds each preset's image ref from the same MTGJSON
        // uuid it uses for `TokenPreset::id`, so a ref pointing at a different
        // preset means the entry was assembled by something other than
        // tokens-gen. Consistency is asserted only where a ref exists: a token
        // with no Scryfall image is legal upstream data, and requiring one
        // would re-introduce a weekly false-red.
        for preset in presets {
            let Some(image_ref) = &preset.token_image_ref else {
                continue;
            };
            assert_eq!(
                image_ref.preset_id, preset.id,
                "preset `{}` carries an image ref for `{}`",
                preset.id, image_ref.preset_id
            );
        }
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

    /// CR 701.71a + CR 306.3: every `Planeswalker` preset must carry a planeswalker
    /// subtype the runtime ability registry knows, so a catalog regeneration cannot
    /// silently drop the row's loyalty abilities.
    #[test]
    fn planeswalker_token_subtypes_match_registry() {
        let mut checked = 0usize;
        for preset in known_token_presets() {
            if !matches!(preset.category, TokenCategory::Planeswalker) {
                continue;
            }
            checked += 1;
            assert!(
                preset.body.subtypes.iter().any(|subtype| {
                    !crate::game::effects::token::predefined_token_abilities(subtype).is_empty()
                }),
                "planeswalker preset {} has subtypes {:?}, none of which the registry knows",
                preset.id,
                preset.body.subtypes,
            );
        }
        assert!(checked > 0, "no Planeswalker presets in the catalog");
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

    /// Builds a merge-level preset. Nothing here names a card: the merge's
    /// population is every row the overlay will ever hold.
    fn merge_preset(
        id: &str,
        set: &str,
        name: &str,
        source_oracle_id: Option<&str>,
    ) -> TokenPreset {
        TokenPreset {
            id: id.to_string(),
            category: TokenCategory::Creature,
            fidelity: PresetFidelity::Full,
            pt_provenance: TokenPtProvenance::FixedOrAbsent,
            body: TokenCharacteristics {
                display_name: name.to_string(),
                power: Some(1),
                toughness: Some(1),
                loyalty: None,
                core_types: vec![CoreType::Creature],
                subtypes: vec![name.to_string()],
                supertypes: Vec::new(),
                colors: vec![ManaColor::Green],
                keywords: Vec::new(),
            },
            source_card_names: Vec::new(),
            source_card_refs: source_oracle_id
                .map(|oracle_id| {
                    vec![TokenSourceRef {
                        card_name: format!("{name} Maker"),
                        face_name: None,
                        scryfall_oracle_id: Some(oracle_id.to_string()),
                        scryfall_id: None,
                    }]
                })
                .unwrap_or_default(),
            token_image_ref: None,
            set_code: set.to_string(),
            set_name: String::new(),
            collector_number: None,
            released_at: None,
            type_line: String::new(),
            rules_text: None,
        }
    }

    const MAKER_ORACLE_ID: &str = "11111111-1111-1111-1111-111111111111";
    const OTHER_MAKER_ORACLE_ID: &str = "22222222-2222-2222-2222-222222222222";

    #[test]
    fn overlay_row_matching_nothing_is_merged_in_id_order() {
        let generated = vec![
            merge_preset("id-a", "AAA", "Beast", Some(MAKER_ORACLE_ID)),
            merge_preset("id-c", "CCC", "Horror", Some(MAKER_ORACLE_ID)),
        ];
        let row = merge_preset("id-b", "BBB", "Wurm", Some(MAKER_ORACLE_ID));

        let (presets, reports) = merge_overlay(generated, vec![row]).expect("row is valid");

        // Present and positioned, not merely "not dropped".
        let ids: Vec<&str> = presets.iter().map(|p| p.id.as_str()).collect();
        assert_eq!(ids, ["id-a", "id-b", "id-c"]);
        assert_eq!(reports.len(), 1);
        assert_eq!(reports[0].outcome, OverlayRowOutcome::Applied);
        assert_eq!(reports[0].overlay_id, "id-b");
    }

    #[test]
    fn mtgjson_supersedes_an_overlay_row_when_it_publishes_that_token() {
        // MTGJSON publishes the token under its own uuid and links it back to
        // the same creating card the overlay row names.
        let generated = vec![merge_preset(
            "mtgjson-id",
            "FRA",
            "Wurm",
            Some(MAKER_ORACLE_ID),
        )];
        let row = merge_preset("placeholder-id", "FRA", "Wurm", Some(MAKER_ORACLE_ID));

        let (presets, reports) = merge_overlay(generated, vec![row]).expect("row is valid");

        let wurms: Vec<&TokenPreset> = presets
            .iter()
            .filter(|p| p.body.display_name == "Wurm")
            .collect();
        assert_eq!(wurms.len(), 1, "an id-only key would leave two");
        // The surviving id is the generated one, so "everything is superseded"
        // still fails this.
        assert_eq!(wurms[0].id, "mtgjson-id");
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::Superseded {
                by_id: "mtgjson-id".to_string()
            }
        );
    }

    #[test]
    fn a_generated_preset_sharing_the_overlay_row_id_supersedes_it() {
        // Same id, different set *and* name: only the id leg can fire, and it
        // must — a duplicate id panics the `PRESETS` `LazyLock` for every
        // consumer.
        let generated = vec![merge_preset(
            "shared-id",
            "AAA",
            "Beast",
            Some(MAKER_ORACLE_ID),
        )];
        let row = merge_preset("shared-id", "BBB", "Wurm", Some(MAKER_ORACLE_ID));

        let (presets, reports) = merge_overlay(generated, vec![row]).expect("row is valid");

        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].body.display_name, "Beast");
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::Superseded {
                by_id: "shared-id".to_string()
            }
        );
    }

    #[test]
    fn a_same_named_token_from_a_different_card_does_not_supersede() {
        // The admitted member the key must refuse: a same-named token published
        // in the same set by a *different* card. The loose `(set, name)` key
        // drops the hand-authored row here.
        let mut generated = merge_preset("mtgjson-id", "FRA", "Wurm", Some(OTHER_MAKER_ORACLE_ID));
        generated.body.power = Some(9);
        generated.body.toughness = Some(9);
        let row = merge_preset("placeholder-id", "FRA", "Wurm", Some(MAKER_ORACLE_ID));

        let (presets, reports) = merge_overlay(vec![generated], vec![row]).expect("row is valid");

        // Positive reach-guard for the negative claim: the row is *present*.
        assert_eq!(presets.len(), 2);
        assert!(presets.iter().any(|p| p.id == "placeholder-id"));
        assert!(presets.iter().any(|p| p.id == "mtgjson-id"));
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::AppliedShadowed {
                by_id: "mtgjson-id".to_string()
            }
        );
    }

    #[test]
    fn neither_a_different_set_nor_a_different_name_supersedes() {
        // Both near-misses share the source card, so only `set_code` and
        // `display_name` keep them from superseding.
        let generated = vec![
            merge_preset("other-set", "XXX", "Wurm", Some(MAKER_ORACLE_ID)),
            merge_preset("other-name", "FRA", "Beast", Some(MAKER_ORACLE_ID)),
        ];
        let row = merge_preset("placeholder-id", "FRA", "Wurm", Some(MAKER_ORACLE_ID));

        let (presets, reports) = merge_overlay(generated, vec![row]).expect("row is valid");

        // The row survives both near-misses; the different-set one shares the
        // creating card, so it is advised rather than silent.
        assert_eq!(presets.len(), 3);
        assert!(presets.iter().any(|p| p.id == "placeholder-id"));
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::AppliedShadowed {
                by_id: "other-set".to_string()
            }
        );
    }

    #[test]
    fn a_mis_keyed_set_code_is_advised_but_never_supersedes() {
        let mtgjson = merge_preset("mtgjson-id", "ZZZ", "Wurm", Some(MAKER_ORACLE_ID));

        // Same token and same creating card, but the row names the Scryfall
        // token-set code MTGJSON does not publish under. The supersession key
        // still refuses it — and must not do so silently.
        let mis_keyed = merge_preset("placeholder-id", "TZZZ", "Wurm", Some(MAKER_ORACLE_ID));
        let (presets, reports) =
            merge_overlay(vec![mtgjson.clone()], vec![mis_keyed]).expect("row is valid");
        assert_eq!(
            presets.len(),
            2,
            "the advisory must not start dropping rows"
        );
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::AppliedShadowed {
                by_id: "mtgjson-id".to_string()
            },
            "an advisory predicate that still requires the set code reports `Applied` here, \
             leaving a placeholder beside the real token with nothing printed"
        );

        // The other end: same name, different set, different creating card —
        // a genuinely different token. The advisory stays discriminating.
        let unrelated = merge_preset("other-id", "TZZZ", "Wurm", Some(OTHER_MAKER_ORACLE_ID));
        let (presets, reports) =
            merge_overlay(vec![mtgjson], vec![unrelated]).expect("row is valid");
        assert_eq!(presets.len(), 2);
        assert_eq!(reports[0].outcome, OverlayRowOutcome::Applied);
    }

    #[test]
    fn overlay_rows_deduplicate_against_each_other_on_both_legs() {
        // The merge scans its own growing output, so the overlay cannot emit a
        // duplicate id on its own. One member at *each* key leg.
        let id_leg = vec![
            merge_preset("dup-id", "AAA", "Beast", Some(MAKER_ORACLE_ID)),
            merge_preset("dup-id", "BBB", "Wurm", Some(MAKER_ORACLE_ID)),
        ];
        let (presets, reports) = merge_overlay(Vec::new(), id_leg).expect("rows are valid");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].body.display_name, "Beast");
        assert_eq!(
            reports[1].outcome,
            OverlayRowOutcome::Superseded {
                by_id: "dup-id".to_string()
            }
        );

        let name_leg = vec![
            merge_preset("first-id", "FRA", "Wurm", Some(MAKER_ORACLE_ID)),
            merge_preset("second-id", "FRA", "Wurm", Some(MAKER_ORACLE_ID)),
        ];
        let (presets, reports) = merge_overlay(Vec::new(), name_leg).expect("rows are valid");
        assert_eq!(presets.len(), 1);
        assert_eq!(presets[0].id, "first-id");
        assert_eq!(
            reports[1].outcome,
            OverlayRowOutcome::Superseded {
                by_id: "first-id".to_string()
            }
        );
    }

    #[test]
    fn an_overlay_row_that_leaves_a_key_leg_blank_is_refused() {
        // Every row is driven against the one MTGJSON entry that retires a
        // well-formed row: the first two legs are the control showing that entry
        // really does retire, so a blank-leg row surviving the merge would be one
        // whose key can never match anything MTGJSON publishes.
        let mtgjson = || {
            vec![merge_preset(
                "mtgjson-id",
                "FRA",
                "Wurm",
                Some(MAKER_ORACLE_ID),
            )]
        };
        let by_mtgjson = OverlayRowOutcome::Superseded {
            by_id: "mtgjson-id".to_string(),
        };

        // In-test positive control: the canonical-case row *is* superseded, so
        // this fixture can fail in the direction the members below guard.
        let (presets, reports) = merge_overlay(
            mtgjson(),
            vec![merge_preset("canon", "FRA", "Wurm", Some(MAKER_ORACLE_ID))],
        )
        .expect("row is valid");
        assert_eq!(presets.len(), 1);
        assert_eq!(reports[0].outcome, by_mtgjson);

        // Member at the other end of the key: a set code MTGJSON publishes as
        // `FRA` is the same set typed `fra`, so the same entry retires it.
        let (presets, reports) = merge_overlay(
            mtgjson(),
            vec![merge_preset("lower", "fra", "Wurm", Some(MAKER_ORACLE_ID))],
        )
        .expect("row is valid");
        assert_eq!(presets.len(), 1, "a case-sensitive set key would leave two");
        assert_eq!(reports[0].outcome, by_mtgjson);

        // Members refused at the boundary: each leaves one leg of the identity
        // key blank, so no MTGJSON entry could ever match it. `Applied` here
        // would be the silent failure — a placeholder beside the real token,
        // under the same name, which `unique_token_body_by_name` resolves to
        // `None`.
        let mut absent_oracle_id = merge_preset("absent-oracle-row", "FRA", "Wurm", None);
        absent_oracle_id.source_card_refs = vec![TokenSourceRef {
            card_name: "Wurm Maker".to_string(),
            face_name: None,
            scryfall_oracle_id: None,
            scryfall_id: Some("not-an-oracle-id".to_string()),
        }];
        for row in [
            merge_preset("no-set-row", "", "Wurm", Some(MAKER_ORACLE_ID)),
            merge_preset("no-name-row", "FRA", "", Some(MAKER_ORACLE_ID)),
            merge_preset("no-refs-row", "FRA", "Wurm", None),
            merge_preset("blank-oracle-row", "FRA", "Wurm", Some("")),
            absent_oracle_id,
        ] {
            let id = row.id.clone();
            let err = merge_overlay(mtgjson(), vec![row])
                .expect_err("a row no MTGJSON entry could retire must be refused");
            assert!(err.contains(&id), "error must name the row: {err}");
        }
    }

    #[test]
    fn an_upper_case_source_oracle_id_still_matches_the_generated_preset() {
        let upper = MAKER_ORACLE_ID.to_ascii_uppercase();
        let mtgjson = merge_preset("mtgjson-id", "ZZZ", "Wurm", Some(MAKER_ORACLE_ID));

        // Same set: comparing oracle ids with `==` reports `AppliedShadowed`
        // here, leaving the placeholder beside the token MTGJSON already ships.
        let row = merge_preset("placeholder-id", "ZZZ", "Wurm", Some(&upper));
        let (_, reports) = merge_overlay(vec![mtgjson.clone()], vec![row]).expect("row is valid");
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::Superseded {
                by_id: "mtgjson-id".to_string()
            }
        );

        // Mis-keyed set as well: with `==` neither leg matches and the outcome
        // is a silent `Applied` — the orphan the fold exists to prevent.
        let row = merge_preset("placeholder-id", "TZZZ", "Wurm", Some(&upper));
        let (_, reports) = merge_overlay(vec![mtgjson], vec![row]).expect("row is valid");
        assert_eq!(
            reports[0].outcome,
            OverlayRowOutcome::AppliedShadowed {
                by_id: "mtgjson-id".to_string()
            }
        );
    }

    #[test]
    fn a_creature_row_without_p_t_is_refused_unless_its_source_defines_them() {
        // One member per term of the disjunction: an omitted `power`, and an
        // omitted `toughness` with the power present.
        let mut no_power = merge_preset("no-power-row", "FRA", "Wurm", Some(MAKER_ORACLE_ID));
        no_power.body.power = None;
        let mut no_toughness =
            merge_preset("no-toughness-row", "FRA", "Wurm", Some(MAKER_ORACLE_ID));
        no_toughness.body.toughness = None;
        for row in [no_power.clone(), no_toughness] {
            let id = row.id.clone();
            let err = merge_overlay(Vec::new(), vec![row]).expect_err(
                "serde reads an omitted P/T key as `None`, so without this leg the row merges \
                 P/T-less instead of failing",
            );
            assert!(err.contains(&id), "error must name the row: {err}");
        }

        // Live control: the same row accepted once the creating card is declared
        // as the P/T source, so the refusal is the missing P/T and not the shape.
        let mut source_defined = no_power;
        source_defined.pt_provenance = TokenPtProvenance::SourceDefinedOrDynamic {
            power: Some("*".to_string()),
            toughness: Some("*".to_string()),
        };
        let (presets, _) =
            merge_overlay(Vec::new(), vec![source_defined]).expect("a declared P/T source is fine");
        assert_eq!(presets.len(), 1);
    }

    #[test]
    fn committed_overlay_rows_merge_clean_against_the_catalog() {
        let overlay = parse_overlay(include_str!("../../data/known-tokens.overlay.toml"))
            .expect("known-tokens.overlay.toml parses as a catalog file");
        let overlay_ids: Vec<&str> = overlay.iter().map(|row| row.id.as_str()).collect();
        let generated: Vec<TokenPreset> = known_token_presets()
            .iter()
            .filter(|preset| !overlay_ids.contains(&preset.id.as_str()))
            .cloned()
            .collect();

        // `merge_overlay`'s only non-test caller is the `tokens-gen` bin, so
        // without this the boundary guard first runs on the committed rows
        // inside the weekly card-data cron. Every row must merge clean: a
        // refusal, a supersession or an advisory all mean the file is stale.
        let (_, reports) = merge_overlay(generated, overlay.clone())
            .expect("every committed overlay row must satisfy the boundary guard");
        // An empty overlay is this hatch's healthy steady state, not a lost
        // row: every row is deleted once MTGJSON ships the token it stood in
        // for, so the file is empty between preview seasons. The loop then has
        // nothing to check, and the live control below is what keeps
        // `merge_overlay` under test in that state.
        for report in &reports {
            assert_eq!(
                report.outcome,
                OverlayRowOutcome::Applied,
                "overlay row `{}` ({} / {}) no longer merges clean; `tokens-gen` prints an \
                 advisory here and the row is probably stale",
                report.overlay_id,
                report.set_code,
                report.display_name
            );
        }

        // Live control: the same call, with one row's set code mis-keyed the
        // way a Scryfall token-set code would be, must find a collision the
        // loop above reports on none of its rows. Built from a catalog preset
        // rather than a committed overlay row, so the control still runs when
        // the overlay is empty. Safe for any preset: catalog set codes are
        // parent codes, never `T`-prefixed token-set codes, so the mis-key
        // cannot satisfy `names_same_token` (set-code equality) and the row
        // reaches the advisory branch instead of being superseded.
        let probe_source = known_token_presets()
            .iter()
            .find(|preset| row_source_oracle_ids(preset).next().is_some())
            .expect("the catalog carries a preset with a source-card oracle id");
        let mut mis_keyed = probe_source.clone();
        mis_keyed.id = format!("{}-probe", mis_keyed.id);
        mis_keyed.set_code = format!("T{}", mis_keyed.set_code);
        let (_, reports) = merge_overlay(known_token_presets().to_vec(), vec![mis_keyed])
            .expect("the probe row is valid");
        assert!(
            matches!(
                reports[0].outcome,
                OverlayRowOutcome::AppliedShadowed { .. }
            ),
            "the advisory scan found nothing on a row built to trip it: {:?}",
            reports[0].outcome
        );
    }

    #[test]
    fn committed_overlay_rows_are_present_in_the_catalog() {
        let overlay = parse_overlay(include_str!("../../data/known-tokens.overlay.toml"))
            .expect("known-tokens.overlay.toml parses as a catalog file");

        // A membership loop over an empty list confirms nothing — but an empty
        // overlay is this hatch's healthy steady state rather than a lost row
        // (see the sibling test), so there is simply nothing to assert here
        // then. The sibling's live control keeps the merge path under test.
        if overlay.is_empty() {
            return;
        }

        let mut seen = std::collections::HashSet::new();
        for row in &overlay {
            assert!(
                seen.insert(row.id.as_str()),
                "known-tokens.overlay.toml: duplicate row id `{}`",
                row.id
            );
            assert_eq!(
                known_token_preset_by_id(&row.id),
                Some(row),
                "overlay row `{}` ({} / {}) is missing from or differs in known-tokens.toml. \
                 Two repairs: re-run `tokens-gen` if the catalog lost the row, or delete the \
                 row from the overlay if `tokens-gen` reported it superseded.",
                row.id,
                row.set_code,
                row.body.display_name
            );
        }
    }
}

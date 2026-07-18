use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

use crate::game::game_object::{AttachTarget, BackFaceData, DisplaySource, GameObject};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, CastingPermission,
    Comparator, ContinuousModification, ControllerRef, CopiableValues, DelayedTriggerCondition,
    Duration, Effect, EffectError, EffectKind, FilterProp, ManaContribution, ManaProduction,
    PermissionGrantee, PlayerFilter, PtValue, QuantityExpr, QuantityRef, ResolvedAbility,
    SacrificeCost, SearchSelectionConstraint, StaticDefinition, TargetFilter, TargetRef,
    TriggerCondition, TriggerDefinition, TypeFilter, TypedFilter,
};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    DelayedTrigger, GameState, LiminalEntry, LiminalTokenAbilityInjection, PendingCopyTokenBatch,
    PendingCounterAddition, PendingCounterPostAction, PendingEffectResolutionEvent,
    TokenEntryEventEmission, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId, TrackedSetId};
use crate::types::keywords::{Keyword, WardCost};
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CopyTokenSpec, ProposedEvent, TokenSpec};
use crate::types::statics::CastFrequency;
use crate::types::triggers::TriggerMode;
use crate::types::zones::{EtbTapState, Zone};

// ── Token script parser ─────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TokenAbilitySource {
    Predefined,
    CatalogRulesText,
    None,
}

#[derive(Debug, Clone)]
pub(crate) struct TokenAbilityMaterialization {
    pub source: TokenAbilitySource,
    pub abilities: Vec<AbilityDefinition>,
    pub trigger_definitions: Vec<TriggerDefinition>,
    pub static_definitions: Vec<StaticDefinition>,
    pub keywords: Vec<Keyword>,
    pub modifications: Vec<ContinuousModification>,
    pub back_face: Option<BackFaceData>,
    pub rules_text: Option<String>,
    pub unparsed_rules_text_lines: Vec<String>,
}

impl TokenAbilityMaterialization {
    fn none() -> Self {
        Self {
            source: TokenAbilitySource::None,
            abilities: Vec::new(),
            trigger_definitions: Vec::new(),
            static_definitions: Vec::new(),
            keywords: Vec::new(),
            modifications: Vec::new(),
            back_face: None,
            rules_text: None,
            unparsed_rules_text_lines: Vec::new(),
        }
    }

    pub(crate) fn has_functional_payload(&self) -> bool {
        !self.abilities.is_empty()
            || !self.trigger_definitions.is_empty()
            || !self.static_definitions.is_empty()
            || !self.keywords.is_empty()
            || !self.modifications.is_empty()
            || self.back_face.is_some()
    }
}

/// CR 111.3 + CR 111.10: Materialize the intrinsic ability payload a token
/// receives from its predefined subtype/name or, if that contributes nothing,
/// from the linked catalog token rules text.
pub(crate) fn materialize_token_ability_payload(
    name: &str,
    subtypes: &[String],
    preset: Option<&crate::game::token_presets::TokenPreset>,
) -> TokenAbilityMaterialization {
    let predefined = materialize_predefined_token_payload(name, subtypes);
    if predefined.has_functional_payload() {
        return predefined;
    }
    preset.map_or_else(
        TokenAbilityMaterialization::none,
        materialize_catalog_token_payload,
    )
}

fn materialize_predefined_token_payload(
    name: &str,
    subtypes: &[String],
) -> TokenAbilityMaterialization {
    let mut materialized = TokenAbilityMaterialization::none();
    let mut abilities_to_add = Vec::new();
    for subtype in subtypes {
        abilities_to_add.extend(predefined_token_abilities(subtype));
    }
    let role_spec = if subtypes.iter().any(|s| s == "Role") {
        predefined_role_token_spec(name)
    } else {
        None
    };
    let is_incubator = subtypes.iter().any(|s| s == "Incubator");

    if abilities_to_add.is_empty() && role_spec.is_none() && !is_incubator {
        return materialized;
    }

    materialized.source = TokenAbilitySource::Predefined;
    materialized.abilities = abilities_to_add;
    if is_incubator {
        materialized.back_face = Some(incubator_phyrexian_back_face());
    }
    for subtype in subtypes {
        if let Some(text) = predefined_token_rules_text(subtype) {
            materialized.rules_text = Some(text.to_string());
            break;
        }
    }
    if let Some(spec) = role_spec {
        materialized.static_definitions = spec.statics;
        materialized.trigger_definitions = spec.triggers;
    }

    materialized
}

fn materialize_catalog_token_payload(
    preset: &crate::game::token_presets::TokenPreset,
) -> TokenAbilityMaterialization {
    let mut materialized = TokenAbilityMaterialization::none();
    let Some(rules_text) = preset.rules_text.as_deref().filter(|text| !text.is_empty()) else {
        return materialized;
    };

    materialized.source = TokenAbilitySource::CatalogRulesText;
    materialized.rules_text = Some(rules_text.to_string());
    let (static_definitions, modifications, unparsed_lines) =
        catalog_rules_text_abilities(rules_text);
    materialized.static_definitions = static_definitions;
    materialized.unparsed_rules_text_lines = unparsed_lines;

    for modification in modifications {
        match modification {
            ContinuousModification::GrantTrigger { trigger } => {
                let mut trigger = *trigger;
                normalize_token_self_lki_trigger(&mut trigger);
                materialized.trigger_definitions.push(trigger);
            }
            ContinuousModification::AddKeyword { keyword } => materialized.keywords.push(keyword),
            ContinuousModification::GrantAbility { definition } => {
                materialized.abilities.push(*definition);
            }
            other => materialized.modifications.push(other),
        }
    }

    materialized
}

/// Parsed token attributes from a Forge token script name.
struct TokenAttrs {
    display_name: String,
    power: Option<i32>,
    toughness: Option<i32>,
    core_types: Vec<CoreType>,
    subtypes: Vec<String>,
    colors: Vec<ManaColor>,
    keywords: Vec<Keyword>,
    supertypes: Vec<Supertype>,
}

/// Parse a Forge token script name into structured attributes.
///
/// Script format (comma-separated scripts use only the first entry):
/// - Creature: `{colors}_{power}_{toughness}[_a][_e]_{subtype}[_{keyword}]`
/// - Variable P/T: `{colors}_x_x[_a][_e]_{subtype}[_{keyword}]`
/// - Artifact: `{colors}_a_{subtype}[_{suffix}]`
/// - Enchantment: `{colors}_e_{subtype}[_{suffix}]`
///
/// Returns `None` for named tokens (e.g. `llanowar_elves`) that don't follow the format.
fn parse_token_script(script: &str) -> Option<TokenAttrs> {
    // Some card data has comma-separated multi-token scripts; use only the first
    let parts: Vec<&str> = script.split(',').next()?.split('_').collect();
    if parts.len() < 2 {
        return None;
    }

    let color_code = parts[0];
    if !color_code.chars().all(|c| "wubrgc".contains(c)) {
        return None;
    }

    let colors = parse_colors(color_code);
    let rest = &parts[1..];

    match rest.first().copied()? {
        // Non-creature artifact: {color}_a_{subtype}[_{suffix}]
        "a" if rest.get(1).is_some_and(|s| s.parse::<i32>().is_err()) => {
            let subtypes = extract_subtypes(&rest[1..]);
            Some(TokenAttrs {
                display_name: format_display_name(&subtypes),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Artifact],
                subtypes,
                colors,
                keywords: vec![],
                supertypes: vec![],
            })
        }
        // Non-creature enchantment: {color}_e_{subtype}[_{suffix}]
        "e" if rest.get(1).is_some_and(|s| s.parse::<i32>().is_err()) => {
            let subtypes = extract_subtypes(&rest[1..]);
            Some(TokenAttrs {
                display_name: format_display_name(&subtypes),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Enchantment],
                subtypes,
                colors,
                keywords: vec![],
                supertypes: vec![],
            })
        }
        // Variable P/T creature: {color}_x_x_{type_parts}
        "x" if rest.get(1) == Some(&"x") => {
            Some(parse_creature_parts(&rest[2..], colors, Some(0), Some(0)))
        }
        // Numeric P/T creature: {color}_{p}_{t}_{type_parts}
        p_str => {
            let power = p_str.parse::<i32>().ok()?;
            let toughness = rest.get(1)?.parse::<i32>().ok()?;
            Some(parse_creature_parts(
                &rest[2..],
                colors,
                Some(power),
                Some(toughness),
            ))
        }
    }
}

/// Build a creature `TokenAttrs` from the segments after power/toughness.
/// Segments may contain type flags (`a`, `e`), subtypes, and keywords.
fn parse_creature_parts(
    segments: &[&str],
    colors: Vec<ManaColor>,
    power: Option<i32>,
    toughness: Option<i32>,
) -> TokenAttrs {
    let mut core_types = vec![CoreType::Creature];
    let mut type_segments: Vec<&str> = Vec::new();

    for &part in segments {
        match part {
            "a" => core_types.push(CoreType::Artifact),
            "e" => core_types.push(CoreType::Enchantment),
            _ => type_segments.push(part),
        }
    }

    let keywords = extract_keywords(&type_segments);
    let subtypes = extract_subtypes(&type_segments);
    let display_name = format_display_name(&subtypes);

    TokenAttrs {
        display_name,
        power,
        toughness,
        core_types,
        subtypes,
        colors,
        keywords,
        supertypes: vec![],
    }
}

// ── Lookup tables ───────────────────────────────────────────────────────

fn parse_colors(code: &str) -> Vec<ManaColor> {
    code.chars()
        .filter_map(|c| match c {
            'w' => Some(ManaColor::White),
            'u' => Some(ManaColor::Blue),
            'b' => Some(ManaColor::Black),
            'r' => Some(ManaColor::Red),
            'g' => Some(ManaColor::Green),
            _ => None, // 'c' = colorless
        })
        .collect()
}

const KNOWN_KEYWORDS: &[(&str, Keyword)] = &[
    ("flying", Keyword::Flying),
    ("first_strike", Keyword::FirstStrike),
    ("double_strike", Keyword::DoubleStrike),
    ("trample", Keyword::Trample),
    ("deathtouch", Keyword::Deathtouch),
    ("lifelink", Keyword::Lifelink),
    ("vigilance", Keyword::Vigilance),
    ("haste", Keyword::Haste),
    ("reach", Keyword::Reach),
    ("defender", Keyword::Defender),
    ("menace", Keyword::Menace),
    ("indestructible", Keyword::Indestructible),
    ("hexproof", Keyword::Hexproof),
    ("prowess", Keyword::Prowess),
    ("changeling", Keyword::Changeling),
    ("infect", Keyword::Infect),
    ("flash", Keyword::Flash),
];

/// Suffixes in token names that are ability descriptions, not subtypes or keywords.
const IGNORED_SUFFIXES: &[&str] = &[
    "sac",
    "draw",
    "noblock",
    "lifegain",
    "lose",
    "con",
    "burn",
    "snipe",
    "pwdestroy",
    "exile",
    "counter",
    "illusory",
    "decayed",
    "opp",
    "life",
    "total",
    "ammo",
    "mana",
    "restrict",
    "tappump",
    "crewbuff",
    "crewsaddlebuff",
    "unblockable",
    "toxic",
    "banding",
    "cardsinhand",
    "mountainwalk",
    "leavedrain",
    "exileplay",
    "search",
    "mill",
    "nosferatu",
    "sound",
    "call",
    "resurgence",
    "grave",
    "pro",
    "red",
    "burst",
    "spiritshadow",
    "landfall",
    "drawcounter",
    "poison",
];

fn lookup_keyword(s: &str) -> Option<Keyword> {
    KNOWN_KEYWORDS
        .iter()
        .find(|(k, _)| *k == s)
        .map(|(_, v)| v.clone())
}

fn is_ignored(s: &str) -> bool {
    IGNORED_SUFFIXES.contains(&s)
}

fn extract_keywords(segments: &[&str]) -> Vec<Keyword> {
    let mut keywords = Vec::new();
    let mut skip_next = false;
    for (i, s) in segments.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if let Some(kw) = lookup_keyword(s) {
            keywords.push(kw);
        } else if *s == "firebending" {
            // Parameterized: "firebending" followed by a numeric segment
            let n = segments
                .get(i + 1)
                .and_then(|v| v.parse::<u32>().ok())
                .unwrap_or(1);
            keywords.push(Keyword::Firebending(QuantityExpr::Fixed {
                value: n as i32,
            }));
            skip_next = segments
                .get(i + 1)
                .is_some_and(|v| v.parse::<u32>().is_ok());
        }
    }
    keywords
}

/// Extract subtypes: anything that isn't a keyword, parameterized keyword, or ignored suffix.
fn extract_subtypes(segments: &[&str]) -> Vec<String> {
    let mut subtypes = Vec::new();
    let mut skip_next = false;
    for (i, s) in segments.iter().enumerate() {
        if skip_next {
            skip_next = false;
            continue;
        }
        if lookup_keyword(s).is_some() || is_ignored(s) {
            continue;
        }
        // Skip parameterized keyword + its numeric argument
        if *s == "firebending" {
            skip_next = segments
                .get(i + 1)
                .is_some_and(|v| v.parse::<u32>().is_ok());
            continue;
        }
        subtypes.push(capitalize(s));
    }
    subtypes
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        None => String::new(),
        Some(c) => c.to_uppercase().collect::<String>() + chars.as_str(),
    }
}

fn format_display_name(subtypes: &[String]) -> String {
    if subtypes.is_empty() {
        "Token".to_string()
    } else {
        subtypes.join(" ")
    }
}

// ── Effect resolver ─────────────────────────────────────────────────────

/// CR 701.7a: To create a token, put the specified token onto the battlefield.
/// CR 111.2: The player who creates a token is its owner.
///
/// Parses Forge token script names (e.g. `w_1_1_soldier_flying`) to extract
/// card types, colors, keywords, and a human-readable display name.
/// Falls back to raw `Name`/`Power`/`Toughness` from the typed Effect fields.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (
        script_name,
        fallback_power,
        fallback_toughness,
        fallback_types,
        fallback_colors,
        fallback_keywords,
        tapped,
        count,
        owner_filter,
        enters_attacking,
        fallback_supertypes,
        token_statics,
        etb_counters,
        attach_to,
    ) = match &ability.effect {
        Effect::Token {
            name,
            power,
            toughness,
            types,
            colors,
            keywords,
            tapped,
            count,
            owner,
            attach_to,
            enters_attacking,
            supertypes,
            static_abilities,
            enter_with_counters,
        } => (
            name.clone(),
            power.clone(),
            toughness.clone(),
            types.clone(),
            colors.clone(),
            keywords.clone(),
            *tapped,
            resolve_quantity_with_targets(state, count, ability).max(0) as u32,
            owner,
            *enters_attacking,
            supertypes.clone(),
            static_abilities.clone(),
            enter_with_counters.clone(),
            attach_to.as_ref(),
        ),
        _ => (
            "Token".to_string(),
            PtValue::Fixed(0),
            PtValue::Fixed(0),
            vec![],
            vec![],
            vec![],
            false,
            1,
            &TargetFilter::Controller,
            false,
            vec![],
            vec![],
            vec![],
            None,
        ),
    };
    let token_owner = resolve_token_owner(state, ability, owner_filter);

    // CR 303.4 + CR 303.4i: Resolve the specified Aura/Role host once, at propose
    // time. ParentTarget reads the first Object target (the for-each loop's
    // per-iteration rebind binds it); Typed/event-context filters resolve via the
    // shared target/event-context path. `None` for ordinary (unattached) tokens.
    let attach_target: Option<AttachTarget> =
        attach_to.and_then(|f| resolve_attach_host(state, ability, f));

    // CR 111.1 + CR 111.4: Resolve the token's characteristics into a
    // self-describing `TokenSpec`. Script-name parsing takes precedence;
    // typed `Effect::Token` fields are the fallback path.
    let parsed = parse_token_script(&script_name).or_else(|| {
        build_token_attrs_from_effect(
            &script_name,
            &fallback_power,
            &fallback_toughness,
            &fallback_types,
            &fallback_colors,
            &fallback_keywords,
            &fallback_supertypes,
            state,
            ability,
        )
    });

    // CR 122.6a: Resolve ETB counter quantities before proposing — the event
    // carries fully-resolved counts, not quantity expressions.
    let resolved_etb_counters: Vec<(CounterType, u32)> = etb_counters
        .iter()
        .map(|(ct, qty)| {
            let n = resolve_quantity_with_targets(state, qty, ability).max(0) as u32;
            (ct.clone(), n)
        })
        .collect();

    let spec = build_token_spec(
        &script_name,
        parsed.as_ref(),
        &fallback_power,
        &fallback_toughness,
        tapped,
        enters_attacking,
        token_statics,
        resolved_etb_counters,
        attach_target,
        ability,
        state,
    );

    // CR 614.1a: Propose entire token batch for replacement pipeline.
    // Replacement effects (Doubling Season, Primal Vigor) modify count.
    let proposed = ProposedEvent::CreateToken {
        owner: token_owner,
        spec: Box::new(spec),
        copy: None,
        enter_tapped: crate::types::proposed_event::EtbTapState::from_seeded_tapped(tapped),
        count,
        applied: state
            .post_replacement_token_choice_applied
            .clone()
            .unwrap_or_default(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if !apply_create_token_after_replacement(state, event, events) {
                return Ok(());
            }
        }
        ReplacementResult::Prevented => {
            // Token creation was prevented entirely
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    }

    // CR 608.2c: Consume the tracked set after reading its size for "this way" counting.
    if matches!(
        &ability.effect,
        Effect::Token {
            count: QuantityExpr::Ref {
                qty: QuantityRef::TrackedSetSize
            },
            ..
        }
    ) {
        if let Some((&id, _)) = state.tracked_object_sets.iter().max_by_key(|(id, _)| id.0) {
            state.tracked_object_sets.remove(&id);
            // CR 608.2c: drop the consumed set's member-cause provenance too so
            // the side map never outlives its `tracked_object_sets` entry.
            state.tracked_set_member_causes.remove(&id);
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// CR 111.1 + CR 111.4 + CR 111.10: Build the resolved `TokenSpec` for a
/// token creation event, combining parsed script attributes with typed
/// `Effect::Token` fallback fields and ability context (source/controller/
/// duration) needed on the post-accept apply path.
#[allow(clippy::too_many_arguments)]
fn build_token_spec(
    script_name: &str,
    parsed: Option<&TokenAttrs>,
    fallback_power: &PtValue,
    fallback_toughness: &PtValue,
    tapped: bool,
    enters_attacking: bool,
    static_abilities: Vec<crate::types::ability::StaticDefinition>,
    enter_with_counters: Vec<(CounterType, u32)>,
    attach_to: Option<AttachTarget>,
    ability: &ResolvedAbility,
    state: &GameState,
) -> TokenSpec {
    use crate::types::proposed_event::TokenCharacteristics;

    let (display_name, power, toughness, core_types, subtypes, supertypes, colors, keywords) =
        if let Some(attrs) = parsed {
            (
                attrs.display_name.clone(),
                attrs.power,
                attrs.toughness,
                attrs.core_types.clone(),
                attrs.subtypes.clone(),
                attrs.supertypes.clone(),
                attrs.colors.clone(),
                attrs.keywords.clone(),
            )
        } else {
            // No parsed attrs — resolve fallback P/T, and defer type/color
            // inference to the apply path's creature-only fallback branch.
            let rp = resolve_pt_value(fallback_power, state, ability);
            let rt = resolve_pt_value(fallback_toughness, state, ability);
            let (p, t, core) = if rp != 0 || rt != 0 {
                (Some(rp), Some(rt), vec![CoreType::Creature])
            } else {
                (None, None, Vec::new())
            };
            (
                script_name.to_string(),
                p,
                t,
                core,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        };

    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name,
            power,
            toughness,
            core_types,
            subtypes,
            supertypes,
            colors,
            keywords,
        },
        script_name: script_name.to_string(),
        static_abilities,
        enter_with_counters,
        tapped,
        enters_attacking,
        sacrifice_at: ability.duration.clone(),
        source_id: ability.source_id,
        controller: ability.controller,
        attach_to,
    }
}

/// CR 702.6a + CR 111.4: Extract only unconditional intrinsic Equip activated
/// abilities from token `static_abilities`. Equipment tokens such as
/// Stoneforged Blade grant Equip via `GrantAbility(Attach SelfRef → creature)`.
/// Conditional or non-equip `GrantAbility` statics remain layer-only.
fn intrinsic_equip_abilities_from_token_statics(
    static_abilities: &[crate::types::ability::StaticDefinition],
) -> Vec<crate::types::ability::AbilityDefinition> {
    use crate::types::ability::{ContinuousModification, Effect, TargetFilter};

    static_abilities
        .iter()
        .filter(|static_def| {
            static_def.condition.is_none()
                && matches!(static_def.affected, None | Some(TargetFilter::SelfRef))
        })
        .flat_map(|static_def| {
            static_def.modifications.iter().filter_map(|modification| {
                let ContinuousModification::GrantAbility { definition } = modification else {
                    return None;
                };
                match definition.effect.as_ref() {
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        ..
                    } => Some(definition.as_ref().clone()),
                    _ => None,
                }
            })
        })
        .collect()
}

/// CR 111.1 + CR 614.1a: Apply an accepted `CreateToken` proposed event.
///
/// Extracted from `resolve` so `handle_replacement_choice` can deliver tokens
/// accepted after a replacement prompt (Doubling Season on a prompted token
/// creation, etc.) through the same code path.
///
/// `event` must be a `ProposedEvent::CreateToken`; other variants are no-ops.
pub fn apply_create_token_after_replacement(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    apply_create_token_after_replacement_with_created_ids(
        state,
        event,
        Vec::new(),
        PendingEffectResolutionEvent::Emit,
        events,
    )
}

pub(crate) fn apply_create_token_after_replacement_with_created_ids(
    state: &mut GameState,
    event: ProposedEvent,
    initial_created_ids: Vec<ObjectId>,
    pause_completion_event: PendingEffectResolutionEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    let ProposedEvent::CreateToken {
        owner,
        spec,
        copy,
        enter_tapped,
        count: final_count,
        ..
    } = event
    else {
        return true;
    };

    if let Some(copy) = copy {
        let status = super::token_copy::apply_copy_token_after_replacement(
            state,
            owner,
            *copy,
            enter_tapped,
            spec.enter_with_counters.clone(),
            final_count,
            events,
        );
        if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
            pending.created_ids.extend(status.created_ids);
        } else {
            state.last_created_token_ids = status.created_ids;
        }
        return match status.completion {
            super::token_copy::CopyTokenApplyCompletion::Completed => true,
            super::token_copy::CopyTokenApplyCompletion::Paused => false,
        };
    }

    let mut created_ids = initial_created_ids;
    created_ids.reserve(final_count as usize);

    for index in 0..final_count {
        let ch = &spec.characteristics;
        let token_image_ref =
            crate::game::token_presets::find_exact_token_ref(state, spec.source_id, ch);
        let obj_id = zones::create_object(
            state,
            CardId(0),
            owner,
            ch.display_name.clone(),
            Zone::Battlefield,
        );

        // CR 613.7d: a token enters the battlefield, so it receives a timestamp.
        // Drawn before the `get_mut` borrow (`next_timestamp` takes `&mut self`).
        let entry_timestamp = state.next_timestamp();

        if let Some(obj) = state.objects.get_mut(&obj_id) {
            // CR 111.1: Mark as token for SBA cleanup (CR 704.5d)
            obj.is_token = true;
            // True token from a TokenSpec — image lives in the generic-token
            // database (Treasure, Spirit, Saproling, Soldier, etc.).
            obj.display_source = DisplaySource::Token;
            obj.token_image_ref = token_image_ref;
            let has_attrs = ch.power.is_some()
                || ch.toughness.is_some()
                || !ch.core_types.is_empty()
                || !ch.subtypes.is_empty()
                || !ch.supertypes.is_empty()
                || !ch.colors.is_empty()
                || !ch.keywords.is_empty();
            if has_attrs {
                obj.power = ch.power;
                obj.toughness = ch.toughness;
                obj.base_name = ch.display_name.clone();
                obj.base_power = ch.power;
                obj.base_toughness = ch.toughness;
                obj.card_types = CardType {
                    supertypes: ch.supertypes.clone(),
                    core_types: ch.core_types.clone(),
                    subtypes: ch.subtypes.clone(),
                };
                obj.base_card_types = obj.card_types.clone();
                obj.color = ch.colors.clone();
                obj.base_color = ch.colors.clone();
                obj.keywords = ch.keywords.clone();
                obj.base_keywords = ch.keywords.clone();
            }
            // CR 400.7 + CR 302.6: Tokens enter the battlefield as new objects
            // and must run the same ETB-state reset as any other permanent
            // (summoning sickness, echo, damage, loyalty-activated flags).
            // Delegate to the single authority for summoning sickness and
            // related transient flags rather than setting them ad-hoc.
            obj.reset_for_battlefield_entry(state.turn_number, entry_timestamp);
            obj.tapped = enter_tapped.resolve(spec.tapped);

            // CR 113.3d + CR 613.1: Apply static abilities from the token
            // definition. Mirror onto `base_static_definitions` so the
            // layers-reset (`base_*` → `*`) at the start of each layers pass
            // doesn't wipe them before layer 7 reads dynamic P/T grants.
            if !spec.static_abilities.is_empty() {
                let static_abilities: Vec<_> = spec
                    .static_abilities
                    .iter()
                    .cloned()
                    .map(normalized_token_static_definition)
                    .collect();
                Arc::make_mut(&mut obj.base_static_definitions)
                    .extend(static_abilities.iter().cloned());
                for static_def in static_abilities {
                    obj.static_definitions.push(static_def);
                }
                // CR 702.6a + CR 111.4: Only intrinsic Equip activated abilities
                // (unconditional SelfRef `GrantAbility(Attach SelfRef → …)`)
                // are copied onto the token object. Other grants stay in the
                // static/layer path only.
                let equip_abilities =
                    intrinsic_equip_abilities_from_token_statics(&spec.static_abilities);
                if !equip_abilities.is_empty() {
                    Arc::make_mut(&mut obj.abilities).extend(equip_abilities.iter().cloned());
                    Arc::make_mut(&mut obj.base_abilities).extend(equip_abilities);
                }
            }
        }

        // CR 508.4: Token enters attacking — not declared as attacker.
        if spec.enters_attacking {
            crate::game::combat::enter_attacking(state, obj_id, spec.source_id, spec.controller);
        }

        // CR 122.6a: Place counters on the token as it enters the battlefield.
        for (counter_index, (counter_type, counter_count)) in
            spec.enter_with_counters.iter().enumerate()
        {
            if *counter_count > 0
                && !super::counters::add_counter_with_replacement(
                    state,
                    owner,
                    obj_id,
                    counter_type.clone(),
                    *counter_count,
                    events,
                )
            {
                state.last_created_token_ids = created_ids.clone();
                let remaining_counters = spec.enter_with_counters[counter_index + 1..]
                    .iter()
                    .filter(|(_, count)| *count > 0)
                    .map(|(counter_type, count)| {
                        crate::types::game_state::PendingCounterAddition::Object {
                            actor: owner,
                            object_id: obj_id,
                            counter_type: counter_type.clone(),
                            count: *count,
                        }
                    })
                    .collect();
                let remaining_count = final_count.saturating_sub(index + 1);
                let post_actions = vec![
                    PendingCounterPostAction::FinalizeTokenEntry {
                        object_id: obj_id,
                        name: spec.characteristics.display_name.clone(),
                        attach_to: spec.attach_to,
                        sacrifice_at: spec.sacrifice_at.clone(),
                        source_id: spec.source_id,
                        controller: spec.controller,
                    },
                    PendingCounterPostAction::ContinueTokenCreation {
                        owner,
                        spec: spec.clone(),
                        enter_tapped,
                        remaining_count,
                    },
                ];
                let completion = match pause_completion_event {
                    PendingEffectResolutionEvent::Emit => {
                        crate::types::game_state::PendingEffectResolved::with_post_actions(
                            EffectKind::Token,
                            spec.source_id,
                            post_actions,
                        )
                    }
                    PendingEffectResolutionEvent::Suppress => crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                        EffectKind::Token,
                        spec.source_id,
                        post_actions,
                    ),
                };
                super::counters::stash_pending_counter_additions(
                    state,
                    remaining_counters,
                    completion,
                );
                return false;
            }
        }

        // CR 111.3 + CR 111.10: Predefined abilities first; catalog rules_text
        // only when the predefined path contributed nothing.
        inject_resolved_token_abilities(state, obj_id);
        // Battlefield entry: request an incremental layer re-derive for just this
        // token. `flush_layers` escalates to a full pass if the token sources a
        // continuous effect / carries counters / etc., or if any active effect
        // reads board population.
        crate::game::layers::mark_layers_entered(state, obj_id);
        crate::game::restrictions::record_battlefield_entry(state, obj_id);
        crate::game::restrictions::record_token_created(state, obj_id);

        // CR 303.4 + CR 303.7: A Role/Aura token created "attached to" a host
        // enters attached. If no legal host was bound, the token is created
        // unattached and the SBA at CR 704.5m (an Aura not attached to an object
        // or player is put into its owner's graveyard) removes it; for multiple
        // same-controller Roles on one host, CR 704.5y keeps only the
        // latest-timestamp Role. (CR 303.4i's strict "the token isn't created"
        // outcome is approximated by this create-then-SBA path.) Single
        // authority: effects::attach.
        if let Some(host) = &spec.attach_to {
            match host {
                AttachTarget::Object(id) => {
                    super::attach::attach_to(state, obj_id, *id);
                }
                AttachTarget::Player(pid) => {
                    super::attach::attach_to_player(state, obj_id, *pid);
                }
            }
        }

        created_ids.push(obj_id);

        // CR 111.1 + CR 603.6a: "An object that enters the battlefield as a
        // token is created in the battlefield zone." Tokens ARE zone changes
        // from outside the game — emit `ZoneChanged { from: None, to:
        // Battlefield }` so every ETB trigger matcher (Elvish Vanguard, Soul
        // Warden, Panharmonicon) fires for tokens through the same code path
        // used for normal battlefield entry. The accompanying `TokenCreated`
        // event is preserved below for token-specific consumers (animation,
        // logging, `LastCreated` target filters).
        let zone_change_record = state
            .objects
            .get(&obj_id)
            .expect("token just created")
            .snapshot_for_zone_change(obj_id, None, Zone::Battlefield);
        events.push(GameEvent::ZoneChanged {
            object_id: obj_id,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(zone_change_record),
        });

        events.push(GameEvent::TokenCreated {
            object_id: obj_id,
            name: spec.characteristics.display_name.clone(),
            source_id: spec.source_id,
        });

        // CR 603.7: Tokens with a limited duration get a delayed sacrifice trigger.
        // Used by Mobilize and similar keywords that create temporary attacking tokens.
        if matches!(spec.sacrifice_at, Some(Duration::UntilEndOfCombat)) {
            state.delayed_triggers.push(DelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::EndCombat,
                },
                ability: ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: TargetFilter::Any,
                        count: QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![TargetRef::Object(obj_id)],
                    spec.source_id,
                    spec.controller,
                ),
                controller: spec.controller,
                source_id: spec.source_id,
                one_shot: true,
            });
        }
    }

    // CR 603.7: Record created token IDs for sub-abilities that reference
    // TargetFilter::LastCreated (e.g., Job select, suspect).
    state.last_created_token_ids = created_ids;
    true
}

pub(crate) fn reserve_liminal_token_object(
    state: &mut GameState,
    owner: PlayerId,
    name: String,
) -> (ObjectId, GameObject) {
    let id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    (
        id,
        GameObject::new(id, CardId(0), owner, name, Zone::Battlefield),
    )
}

pub(crate) fn apply_copiable_values_to_liminal_object(
    object: &mut GameObject,
    values: &CopiableValues,
    display_source: DisplaySource,
    printed_ref: Option<crate::types::card::PrintedCardRef>,
    token_image_ref: Option<crate::types::card::TokenImageRef>,
) {
    object.display_source = display_source;
    object.printed_ref = printed_ref.clone();
    object.base_printed_ref = printed_ref;
    object.token_image_ref = token_image_ref;
    crate::game::printed_cards::install_copiable_values_as_base(object, values);
}

pub(crate) fn commit_liminal_token_entry_and_continue_copy_batch(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    let continuation = liminal_copy_token_continuation_for_event(state, &event);
    let post_actions = continuation
        .clone()
        .map(liminal_copy_token_continuation_post_action)
        .into_iter()
        .collect();
    if !commit_liminal_token_entry_with_post_actions(
        state,
        event,
        events,
        TokenEntryEventEmission::Emit,
        post_actions,
    ) {
        return false;
    }
    continue_liminal_copy_token_batch(state, continuation, events)
}

#[derive(Clone)]
struct LiminalCopyTokenContinuation {
    owner: PlayerId,
    copy: Box<CopyTokenSpec>,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(CounterType, u32)>,
    remaining_count: u32,
}

fn liminal_copy_token_continuation_for_event(
    state: &GameState,
    event: &ProposedEvent,
) -> Option<LiminalCopyTokenContinuation> {
    let ProposedEvent::TokenEntry { entry_ref, .. } = event else {
        return None;
    };
    let entry = state.liminal_entries.get(entry_ref)?;
    let copy = entry.copy_resume.clone()?;
    Some(LiminalCopyTokenContinuation {
        owner: entry.object.owner,
        copy,
        enter_tapped: entry.enter_tapped,
        enter_with_counters: entry.enter_with_counters.clone(),
        remaining_count: entry.remaining_count,
    })
}

fn continue_liminal_copy_token_batch(
    state: &mut GameState,
    continuation: Option<LiminalCopyTokenContinuation>,
    events: &mut Vec<GameEvent>,
) -> bool {
    state.waiting_for = WaitingFor::Priority {
        player: state.active_player,
    };
    if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
        pending.created_ids = state.last_created_token_ids.clone();
    }
    let Some(continuation) = continuation else {
        if state.pending_copy_token_resolution.is_some() {
            super::token_copy::drain_pending_copy_token_resolution(state, events);
        }
        return !state.pending_copy_token_resolution.is_some()
            || matches!(state.waiting_for, WaitingFor::Priority { .. });
    };
    if continuation.remaining_count > 0 {
        let initial_created_ids = state.last_created_token_ids.clone();
        let status = super::token_copy::apply_copy_token_after_replacement_with_created_ids(
            state,
            continuation.owner,
            *continuation.copy,
            continuation.enter_tapped,
            continuation.enter_with_counters,
            continuation.remaining_count,
            initial_created_ids,
            events,
        );
        state.last_created_token_ids = status.created_ids;
        if matches!(
            status.completion,
            super::token_copy::CopyTokenApplyCompletion::Paused
        ) {
            return false;
        }
    }
    if let Some(pending) = state.pending_copy_token_resolution.as_mut() {
        pending.created_ids = state.last_created_token_ids.clone();
    }
    if state.pending_copy_token_resolution.is_some() {
        super::token_copy::drain_pending_copy_token_resolution(state, events);
    }
    !state.pending_copy_token_resolution.is_some()
        || matches!(state.waiting_for, WaitingFor::Priority { .. })
}

fn liminal_copy_token_continuation_post_action(
    continuation: LiminalCopyTokenContinuation,
) -> PendingCounterPostAction {
    PendingCounterPostAction::ContinueLiminalCopyTokenBatch {
        owner: continuation.owner,
        copy: continuation.copy,
        enter_tapped: continuation.enter_tapped,
        enter_with_counters: continuation.enter_with_counters,
        remaining_count: continuation.remaining_count,
    }
}

pub(crate) fn continue_liminal_copy_token_batch_after_counter_pause(
    state: &mut GameState,
    owner: PlayerId,
    copy: Box<CopyTokenSpec>,
    enter_tapped: EtbTapState,
    enter_with_counters: Vec<(CounterType, u32)>,
    remaining_count: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    continue_liminal_copy_token_batch(
        state,
        Some(LiminalCopyTokenContinuation {
            owner,
            copy,
            enter_tapped,
            enter_with_counters,
            remaining_count,
        }),
        events,
    )
}

pub(crate) fn commit_liminal_token_entry_with_event_emission(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
    entry_events: TokenEntryEventEmission,
) -> bool {
    commit_liminal_token_entry_with_post_actions(state, event, events, entry_events, Vec::new())
}

pub(crate) fn commit_liminal_token_entry_with_post_actions(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
    entry_events: TokenEntryEventEmission,
    post_actions_after_finalize: Vec<PendingCounterPostAction>,
) -> bool {
    let ProposedEvent::TokenEntry {
        entry_ref,
        enter_tapped,
        enter_with_counters,
        ..
    } = event
    else {
        return true;
    };
    let Some(mut entry) = state.liminal_entries.remove(&entry_ref) else {
        return true;
    };
    let finalization = liminal_token_entry_finalization_action(entry_ref, &entry, entry_events);
    let counters_to_apply: Vec<_> = enter_with_counters
        .iter()
        .chain(entry.enter_with_counters.iter())
        .cloned()
        .collect();
    entry.object.tapped = enter_tapped.resolve(entry.object.tapped);
    let owner = entry.object.owner;
    state.objects.insert(entry_ref, entry.object);
    // allow-raw-zone: liminal token birth has no from-zone move; TokenEntry already consults entry replacements (CR 111.2 + CR 614.12).
    zones::add_to_zone(state, entry_ref, Zone::Battlefield, owner);

    for (counter_index, (counter_type, counter_count)) in counters_to_apply.iter().enumerate() {
        if *counter_count > 0
            && !super::counters::add_counter_with_replacement(
                state,
                owner,
                entry_ref,
                counter_type.clone(),
                *counter_count,
                events,
            )
        {
            state.last_created_token_ids = entry.created_ids.clone();
            let remaining_counters = counters_to_apply[counter_index + 1..]
                .iter()
                .filter(|(_, count)| *count > 0)
                .map(|(counter_type, count)| PendingCounterAddition::Object {
                    actor: owner,
                    object_id: entry_ref,
                    counter_type: counter_type.clone(),
                    count: *count,
                })
                .collect();
            let mut post_actions = vec![finalization];
            post_actions.extend(post_actions_after_finalize);
            super::counters::stash_pending_counter_additions(
                state,
                remaining_counters,
                crate::types::game_state::PendingEffectResolved::with_post_actions_without_effect(
                    if entry.copy_resume.is_some() {
                        EffectKind::CopyTokenOf
                    } else {
                        EffectKind::Token
                    },
                    entry.source_id,
                    post_actions,
                ),
            );
            return false;
        }
    }

    finalize_committed_liminal_token_entry_from_action(state, finalization, events);
    true
}

fn liminal_token_entry_finalization_action(
    entry_ref: ObjectId,
    entry: &LiminalEntry,
    entry_events: TokenEntryEventEmission,
) -> PendingCounterPostAction {
    PendingCounterPostAction::FinalizeCommittedLiminalTokenEntry {
        object_id: entry_ref,
        name: entry.name.clone(),
        source_id: entry.source_id,
        controller: entry.controller,
        enters_attacking: entry.enters_attacking,
        attach_to: entry.attach_to,
        sacrifice_at: entry.sacrifice_at.clone(),
        created_ids: entry.created_ids.clone(),
        ability_injection: if entry.copy_resume.is_some() {
            LiminalTokenAbilityInjection::PredefinedToken
        } else {
            LiminalTokenAbilityInjection::ResolvedToken
        },
        entry_events,
    }
}

pub(crate) fn finalize_committed_liminal_token_entry_from_action(
    state: &mut GameState,
    action: PendingCounterPostAction,
    events: &mut Vec<GameEvent>,
) -> bool {
    let PendingCounterPostAction::FinalizeCommittedLiminalTokenEntry {
        object_id,
        name,
        source_id,
        controller,
        enters_attacking,
        attach_to,
        sacrifice_at,
        mut created_ids,
        ability_injection,
        entry_events,
    } = action
    else {
        return true;
    };

    match ability_injection {
        LiminalTokenAbilityInjection::PredefinedToken => {
            super::token_copy::finalize_copied_token(state, source_id, object_id);
            inject_predefined_token_abilities(state, object_id);
        }
        LiminalTokenAbilityInjection::ResolvedToken => {
            inject_resolved_token_abilities(state, object_id);
        }
    }
    crate::game::layers::mark_layers_entered(state, object_id);
    crate::game::restrictions::record_battlefield_entry(state, object_id);
    crate::game::restrictions::record_token_created(state, object_id);

    if enters_attacking {
        crate::game::combat::enter_attacking(state, object_id, source_id, controller);
    }
    if let Some(host) = attach_to {
        match host {
            AttachTarget::Object(id) => {
                super::attach::attach_to(state, object_id, id);
            }
            AttachTarget::Player(pid) => {
                super::attach::attach_to_player(state, object_id, pid);
            }
        };
    }

    if matches!(entry_events, TokenEntryEventEmission::Emit) {
        push_committed_token_entry_events(state, object_id, name, source_id, events);
    }
    if matches!(sacrifice_at, Some(Duration::UntilEndOfCombat)) {
        state.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            },
            ability: ResolvedAbility::new(
                Effect::Sacrifice {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                vec![TargetRef::Object(object_id)],
                source_id,
                controller,
            ),
            controller,
            source_id,
            one_shot: true,
        });
    }

    created_ids.push(object_id);
    state.last_created_token_ids = created_ids;
    true
}

pub(crate) fn push_committed_token_entry_events(
    state: &GameState,
    object_id: ObjectId,
    name: String,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    if let Some(token) = state.objects.get(&object_id) {
        let zone_change_record = token.snapshot_for_zone_change(object_id, None, Zone::Battlefield);
        events.push(GameEvent::ZoneChanged {
            object_id,
            from: None,
            to: Zone::Battlefield,
            record: Box::new(zone_change_record),
        });
    }
    events.push(GameEvent::TokenCreated {
        object_id,
        name,
        source_id,
    });
}

// ── Layer B: token-handler batch purity gate (Tier 3) ────────────────────

/// CR 603.2 + CR 603.6a: The §2.2a emits-exactly-{ZoneChanged,TokenCreated}
/// gate. Layer C (`game/stack.rs::observers_are_batch_safe`) probes ONLY the
/// `ZoneChanged(ETB)` + `TokenCreated` events one produced token emits. That
/// probe is COMPLETE only if the resolved spec's creation emits exactly those
/// two events. Every `TokenSpec` field that would emit an additional
/// `GameEvent` (`enter_with_counters` → `CounterAdded`, counters.rs), introduce
/// an interactive replacement (`enter_with_counters` → AddCounter replacement),
/// or mutate extra battlefield state (`enters_attacking` → combat;
/// `sacrifice_at` → delayed trigger, CR 603.7; `attach_to` → host attachments,
/// CR 303.4) is rejected. A spec passing this gate provably emits exactly
/// `{ZoneChanged(ETB), TokenCreated}` per produced token (see the field-by-field
/// proof in `apply_create_token_after_replacement`).
///
/// `characteristics` / `script_name` / `static_abilities` / `tapped` /
/// `source_id` / `controller` are INERT: they set object fields directly or
/// feed the ETB probe and emit no creation-time event beyond the ETB pair.
pub(crate) fn spec_emits_only_etb_pair(spec: &TokenSpec) -> bool {
    spec.enter_with_counters.is_empty() // no CounterAdded event / AddCounter replacement
        && !spec.enters_attacking // no combat-state mutation (CR 508.4)
        && spec.sacrifice_at.is_none() // no delayed trigger (CR 603.7)
        && spec.attach_to.is_none() // no host attachment mutation (CR 303.4)
}

/// CR 603.6a + CR 111.10: The set of event keys a single produced token EMITS as
/// it enters the battlefield, given its core types. Mirrors the event-side
/// deriver exactly (`keys_from_event`, trigger_index.rs:462-468 for the ETB pair
/// and :529-531 for `TokenCreated`): a token entering emits the broad
/// `EnterBattlefield(None)`, one narrow `EnterBattlefield(Some(ct))` per core
/// type, and `TokenCreated`. Kept in lockstep with the deriver so the §2.3a gate
/// reasons about exactly the events siblings would observe.
fn produced_token_emitted_keys(
    produced_core_types: &[CoreType],
) -> Vec<crate::types::triggers::TriggerEventKey> {
    use crate::types::triggers::TriggerEventKey;
    // CR 603.6a: broad ETB key, emitted for every entering permanent, plus one
    // narrow key per core type of the entering object.
    let mut keys = vec![TriggerEventKey::EnterBattlefield(None)];
    keys.extend(
        produced_core_types
            .iter()
            .map(|ct| TriggerEventKey::EnterBattlefield(Some(*ct))),
    );
    // CR 111.10: a token's creation also emits `TokenCreated`.
    keys.push(TriggerEventKey::TokenCreated);
    keys
}

/// CR 603.2 + CR 603.6a + CR 603.3: The §2.3a produced-token-non-observer gate,
/// parameterized by what the produced token actually EMITS on entry. A produced
/// token whose own triggers OBSERVE its in-batch siblings would fire on them —
/// which one-by-one resolution (CR 603.3 topmost-on-stack) lets it do, but a
/// single batched application would not — so such a token cannot batch.
///
/// The gate intersects each trigger's REGISTERED keys (`keys_from_trigger_def`,
/// the EXACT classifier the live index uses, so the observer-key derivation can
/// never drift from registration) with the set of keys the produced token EMITS
/// on entry (`produced_token_emitted_keys`, mirroring CR 603.6a's broad+narrow
/// emission for `produced_core_types`). A landfall trigger registered under
/// `EnterBattlefield(Some(Land))` carried by a Creature copy (which emits only
/// `{None, Some(Creature), TokenCreated}`) does NOT intersect → it cannot
/// observe its creature siblings → batch-safe. A "whenever a creature enters"
/// trigger (`EnterBattlefield(Some(Creature))`) or a broad permanent-ETB trigger
/// (`EnterBattlefield(None)`) DOES intersect a creature copy's emission →
/// refused.
///
/// Conservatively rejects any trigger routed to unclassified (catch-all/dynamic
/// modes fire on everything, so they always observe siblings).
pub(crate) fn produced_token_is_non_observer(
    triggers: &[TriggerDefinition],
    produced_core_types: &[CoreType],
) -> bool {
    let emitted = produced_token_emitted_keys(produced_core_types);
    triggers.iter().all(|def| {
        let (keys, route_unclassified) = crate::game::trigger_index::keys_from_trigger_def(def);
        !route_unclassified && !keys.iter().any(|k| emitted.contains(k))
    })
}

/// CR 614.1a + CR 616.1: The §3.4 MEDIUM-1 interactive-replacement gate. Token
/// creation routes through `replace_event`, which can return `NeedsChoice` (and
/// set `waiting_for`) when a single optional/`MayCost` replacement applies or
/// when ≥2 candidates are ordering-material. A batched run cannot pause for a
/// player choice mid-collapse, so refuse to batch any spec whose creation
/// *could* yield `NeedsChoice`. Mandatory, non-ordering-material replacements
/// (Doubling Season's mandatory Double) are fine and stay per-token (§5.2) —
/// they never produce `NeedsChoice`. Reuses the live pipeline's exact decision
/// functions, side-effect-free (`&GameState`, no `apply_single_replacement`).
fn token_creation_needs_choice(
    state: &GameState,
    spec: &TokenSpec,
    owner: PlayerId,
    enter_tapped: crate::types::proposed_event::EtbTapState,
    count: u32,
) -> bool {
    let registry = replacement::replacement_registry();
    let proposed = ProposedEvent::CreateToken {
        owner,
        spec: Box::new(spec.clone()),
        copy: None,
        enter_tapped,
        count,
        applied: HashSet::new(),
    };
    let candidates = replacement::find_applicable_replacements(state, &proposed, registry);
    if candidates.is_empty() {
        return false;
    }
    // (1) any single optional/MayCost applicable replacement → interactive.
    let any_optional = candidates.iter().any(|rid| {
        state
            .objects
            .get(&rid.source)
            .and_then(|o| o.replacement_definitions.get(rid.index))
            .map(|r| replacement::replacement_mode_is_optional(&r.mode))
            .unwrap_or(true) // unknown ⇒ conservatively interactive
    });
    // (2) ≥2 candidates whose ordering is material → CR 616.1 player choice.
    let ordering_material = candidates.len() >= 2
        && replacement::replacement_ordering_is_material(state, &candidates, &proposed);
    any_optional || ordering_material
}

/// CR 205: Extract the concrete `CoreType` set a `TypeFilter` counts, for the
/// §2.2 disjointness proof. Returns `None` when the filter is not a simple
/// type predicate the disjointness check can reason about (negation,
/// subtype-only, broad `Permanent`/`Card`/`Any`) — the caller then conserves by
/// refusing the batch.
fn type_filter_core_types(filter: &TypeFilter) -> Option<Vec<CoreType>> {
    match filter {
        TypeFilter::Creature => Some(vec![CoreType::Creature]),
        TypeFilter::Land => Some(vec![CoreType::Land]),
        TypeFilter::Artifact => Some(vec![CoreType::Artifact]),
        TypeFilter::Enchantment => Some(vec![CoreType::Enchantment]),
        TypeFilter::Instant => Some(vec![CoreType::Instant]),
        TypeFilter::Sorcery => Some(vec![CoreType::Sorcery]),
        TypeFilter::Planeswalker => Some(vec![CoreType::Planeswalker]),
        TypeFilter::Battle => Some(vec![CoreType::Battle]),
        TypeFilter::Kindred => Some(vec![CoreType::Kindred]),
        TypeFilter::AnyOf(inner) => {
            let mut out = Vec::new();
            for f in inner {
                out.extend(type_filter_core_types(f)?);
            }
            Some(out)
        }
        // Broad / negated / subtype-only filters cannot be proven disjoint from
        // the token's core types — conserve.
        TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Any
        | TypeFilter::Non(_)
        | TypeFilter::Subtype(_) => None,
    }
}

/// CR 205: The concrete `CoreType` set a `TargetFilter` counts, when it is a
/// single-`TypeFilter` `Typed` filter. Any other shape yields `None`.
fn target_filter_counted_core_types(filter: &TargetFilter) -> Option<Vec<CoreType>> {
    match filter {
        TargetFilter::Typed(TypedFilter { type_filters, .. }) => {
            let mut out = Vec::new();
            for f in type_filters {
                out.extend(type_filter_core_types(f)?);
            }
            Some(out)
        }
        _ => None,
    }
}

/// CR 608.2c: Prove a `ConditionInstead` inner condition is invariant across
/// the run because every object-count it reads is over a core-type the token's
/// creation cannot produce. Returns `true` only when EVERY `ObjectCount`
/// quantity inside a `QuantityCheck` is provably disjoint from `token_core_types`.
/// Any other condition shape (or an un-provable filter) returns `false` →
/// conserve.
fn condition_invariant_for_token(
    condition: &crate::types::ability::AbilityCondition,
    token_core_types: &[CoreType],
) -> bool {
    use crate::types::ability::{AbilityCondition, QuantityExpr, QuantityRef};

    let quantity_is_invariant = |expr: &QuantityExpr| -> bool {
        match expr {
            QuantityExpr::Fixed { .. } => true,
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => match target_filter_counted_core_types(filter) {
                // Disjoint ⇒ the token-creation cannot change this count.
                Some(counted) => counted.iter().all(|ct| !token_core_types.contains(ct)),
                None => false,
            },
            // Any other quantity reference is not proven invariant under the
            // run (it may read state the run mutates) — conserve.
            _ => false,
        }
    };

    match condition {
        AbilityCondition::QuantityCheck { lhs, rhs, .. } => {
            quantity_is_invariant(lhs) && quantity_is_invariant(rhs)
        }
        _ => false,
    }
}

/// CR 111.2 + CR 109.4: a base token's controller and characteristics are
/// fixed at creation; the creating source's identity is not a characteristic,
/// so triggers from distinct sources resolve identically. Returns `true` iff
/// `ability.effect` is a base `Effect::Token` whose resolution reads nothing
/// from the source object: the token's owner is the controller (the default
/// `TargetFilter::Controller`), its `count` is a literal `Fixed` (no
/// source-relative quantity), it does not enter attacking (combat reads the
/// source), and it is not attached to a host (attachment reads the source's
/// target). The remaining fields are pure characteristics (name / P/T / types /
/// colors / keywords / supertypes / static abilities / ETB counters) which are
/// baked into the spec and identical across sources — bound but unconstrained.
///
/// EXHAUSTIVE destructure (no `..`): every field of `Effect::Token` is
/// consciously dispositioned, mirroring `resolve_token_spec`. A future field
/// addition forces a compile error here so its source-independence is decided
/// deliberately rather than silently assumed.
pub(crate) fn token_effect_is_source_independent(ability: &ResolvedAbility) -> bool {
    let Effect::Token {
        name: _,
        power: _,
        toughness: _,
        types: _,
        colors: _,
        keywords: _,
        tapped: _,
        count,
        owner,
        attach_to,
        enters_attacking,
        supertypes: _,
        static_abilities: _,
        enter_with_counters: _,
    } = &ability.effect
    else {
        return false;
    };
    matches!(owner, TargetFilter::Controller)
        && matches!(count, QuantityExpr::Fixed { .. })
        && !*enters_attacking
        && attach_to.is_none()
}

/// CR 608.2 + CR 608.2c: Layer B — the Token-handler purity gate. Returns a
/// `BatchPlan` iff resolving this `Effect::Token` `run_len` times one-by-one
/// would produce the identical per-resolution decision and token spec as one
/// batched application of the base `Token` effect.
///
/// v1 batches the base `Effect::Token` (untargeted, `Fixed` count, emitting
/// exactly the ETB pair, with no produced-token observer and no interactive
/// replacement). A `CopyTokenOf`-instead sub-ability whose condition is
/// currently met (the copy branch) is batched along a CONTIGUOUS PREFIX of the
/// run whose copy sources share identical copiable values (CR 707.2) — the
/// prefix length may be shorter than `run_len`, with the remaining entries
/// resolved in a later step. A `ConditionInstead` sub-ability that is currently
/// NOT met is accepted only when its condition is provably invariant across the
/// run (so all N resolutions take the base branch).
///
/// `run_source_ids` are the per-entry source object ids of the contiguous run
/// (resolution order, top-down), needed only by the met-copy prefix path to
/// gather each entry's `SelfRef` copy source. The base-token path ignores them.
pub(crate) fn try_resolve_batch(
    state: &GameState,
    ability: &ResolvedAbility,
    run_len: u32,
    run_source_ids: &[ObjectId],
) -> Option<super::BatchPlan> {
    // The effect must be a bare `Effect::Token` with a literal `Fixed` count.
    let Effect::Token { count, .. } = &ability.effect else {
        return None;
    };
    if !matches!(count, QuantityExpr::Fixed { .. }) {
        return None;
    }

    // Resolve the per-resolution TokenSpec read-only, mirroring `resolve`.
    // HIGH-1: resolve ONCE here — `resolve_token_spec` parses token scripts,
    // resolves quantities, and builds attributes, so the perf-path must not
    // resolve it twice. The resolved spec's `core_types` feed the disjointness
    // invariance proof below directly.
    let (spec, owner, enter_tapped, resolved_count) = resolve_token_spec(state, ability)?;

    // CR 608.2c: A sub-ability changes the resolved effect. Two acceptable
    // shapes: a `ConditionInstead`-gated sub currently NOT met (the base
    // `Token` resolves, provably invariant across the run), or a met
    // `ConditionInstead` copy-instead swap which is batched along a value-equal
    // prefix (CR 707.2). Any other sub shape conserves.
    if let Some(sub) = &ability.sub_ability {
        match &sub.condition {
            Some(crate::types::ability::AbilityCondition::ConditionInstead { inner }) => {
                if super::evaluate_condition(inner, state, ability) {
                    // The swap currently fires: the resolved effect is the
                    // sub's (e.g. CopyTokenOf). Attempt copy-prefix batching.
                    return try_resolve_copy_batch(state, ability, sub, inner, run_source_ids);
                }
                // NOT met: base `Token` resolves. Token core types feed the
                // disjointness invariance proof.
                if !condition_invariant_for_token(inner, &spec.characteristics.core_types) {
                    return None;
                }
            }
            // Any other sub-ability shape (continuation step, sequential
            // sibling, other instead conditions) is not proven batch-safe.
            _ => return None,
        }
    }

    // v1 batches a single base token per resolution. A non-unit per-resolution
    // count (e.g. "create two Insects") is correct to batch but the count-fusion
    // interaction is out of v1 scope (§5.2a) — conserve.
    if resolved_count != 1 {
        return None;
    }

    // §2.2a: the resolved spec must emit exactly {ZoneChanged, TokenCreated}.
    if !spec_emits_only_etb_pair(&spec) {
        return None;
    }

    // §2.3a: the produced token must not itself observe the ETB/TokenCreated
    // events its in-batch siblings emit. The produced token's emission is
    // derived from its own core types (the spec's characteristics).
    if !produced_token_is_non_observer(
        &base_token_trigger_defs(&spec),
        &spec.characteristics.core_types,
    ) {
        return None;
    }

    // §3.4: token creation must not be able to pause for an interactive
    // (optional / order-material) replacement choice.
    if token_creation_needs_choice(state, &spec, owner, enter_tapped, resolved_count) {
        return None;
    }

    Some(super::BatchPlan::token(spec, run_len))
}

/// CR 608.2c + CR 707.2: A met `ConditionInstead` whose swapped effect is a
/// bare `CopyTokenOf { target: SelfRef, … }` copies the run's own source object
/// per entry. When a contiguous prefix of the run's copy sources share
/// identical copiable values (CR 707.2 fingerprints), those N self-copies are
/// equivalent to one batched spec, so the prefix collapses into a single
/// `CopyToken` batch. The prefix may be shorter than `run_len`; the remainder
/// resolves in a later step (which re-enters this path).
///
/// `sub` is the override sub-ability (its effect is the swapped `CopyTokenOf`);
/// `inner` is the already-fired `ConditionInstead` condition. `run_source_ids`
/// are the per-entry source ids (top-down resolution order).
fn try_resolve_copy_batch(
    state: &GameState,
    ability: &ResolvedAbility,
    sub: &ResolvedAbility,
    inner: &crate::types::ability::AbilityCondition,
    run_source_ids: &[ObjectId],
) -> Option<super::BatchPlan> {
    // 1. SHAPE GATE FIRST (cheapest): the swapped effect must be a bare
    //    self-copy with the default single-token shape and no exceptions.
    let Effect::CopyTokenOf {
        target: TargetFilter::SelfRef,
        owner: TargetFilter::Controller,
        source_filter: None,
        enters_attacking: false,
        tapped: false,
        count: QuantityExpr::Fixed { value: 1 },
        extra_keywords,
        additional_modifications,
    } = &sub.effect
    else {
        return None;
    };
    if !extra_keywords.is_empty() || !additional_modifications.is_empty() {
        return None;
    }

    // 2. LAZY-GATHER the run's copy sources (only now, after the shape gate).
    //    Each entry's `target: SelfRef` copy source is that entry's own source
    //    object — exactly `run_source_ids` (top-down resolution order).
    if run_source_ids.len() < 2 {
        // A prefix of fewer than 2 cannot collapse; fall back to sequential.
        return None;
    }

    // 3. Compute the value-equal contiguous prefix (CR 707.2).
    let (prefix_values, prefix_len) =
        super::token_copy::compute_copy_batch_prefix(state, run_source_ids)?;
    if prefix_len < 2 {
        return None;
    }
    if !copy_token_values_emit_only_etb_pair(&prefix_values) {
        return None;
    }

    // 4. H1 INVARIANCE GATE (AFTER prefix): the condition must be invariant over
    //    the COPY's core types (what enters), not the placeholder spec's. A copy
    //    creating Lands gated on a Land count would diverge per resolution.
    if !condition_invariant_for_token(inner, &prefix_values.card_types.core_types) {
        return None;
    }

    // 5. Build the probe spec from the prefix's shared copiable values so the
    //    §2.2a emits-only-ETB-pair gate holds and Layer C's
    //    `zone_change_record_from_spec` reflects the true produced token.
    let probe_spec = copy_probe_spec(ability, &prefix_values);
    if !spec_emits_only_etb_pair(&probe_spec) {
        return None;
    }
    // §2.3a: a copy token inherits the copied permanent's full trigger set
    // (CR 707.2 + CR 707.5 — the copy's ETB triggers fire), so the non-observer
    // gate reads the prefix's copiable trigger definitions — NOT
    // `base_token_trigger_defs` (which only surfaces a base token's Role-subtype
    // triggers). The produced token's emission is derived from the COPY's core
    // types (what enters), so a Scute-shape landfall trigger keyed
    // `EnterBattlefield(Some(Land))` on a Creature copy does NOT intersect the
    // copy's `{None, Some(Creature), TokenCreated}` emission and stays batch-safe.
    if !produced_token_is_non_observer(
        &prefix_values.trigger_definitions,
        &prefix_values.card_types.core_types,
    ) {
        return None;
    }
    let owner = resolve_token_owner(state, ability, &TargetFilter::Controller);
    if token_creation_needs_choice(
        state,
        &probe_spec,
        owner,
        crate::types::proposed_event::EtbTapState::from_seeded_tapped(false),
        1,
    ) {
        return None;
    }

    // 6. Build the count-aware copy-token batch directly. This uses the same
    //    replacement/apply primitive as `CopyTokenOf`, but avoids re-resolving the
    //    self target and recomputing identical copiable values once per stack
    //    entry.
    let top_source_id = *run_source_ids.first()?;
    let top_source = state.objects.get(&top_source_id)?;
    let copy_batch = PendingCopyTokenBatch {
        owner,
        count: prefix_len,
        copy: Box::new(CopyTokenSpec {
            values: Box::new(prefix_values.clone()),
            display_source: top_source.display_source,
            printed_ref: top_source.printed_ref.clone(),
            token_image_ref: top_source.token_image_ref.clone(),
            extra_keywords: extra_keywords.clone(),
            additional_modifications: additional_modifications.clone(),
            tapped: false,
            enters_attacking: false,
            sacrifice_at: ability.duration.clone(),
            source_id: ability.source_id,
            controller: ability.controller,
        }),
    };

    // 7. Hand back the copy-prefix batch.
    Some(super::BatchPlan::copy_token(
        copy_batch,
        EffectKind::from(&sub.effect),
        ability.source_id,
        probe_spec,
        prefix_values.mana_cost.mana_value(),
        prefix_len,
    ))
}

/// CR 306.5b + CR 614.1c + CR 707.2: `CopyTokenOf` seeds intrinsic counters
/// from the copied values while applying the copy. Those counters emit
/// `CounterAdded` and may pause for replacement choices, so the copy-prefix
/// batch may only collapse values whose creation still emits exactly the ETB
/// pair.
fn copy_token_values_emit_only_etb_pair(values: &crate::types::ability::CopiableValues) -> bool {
    crate::game::printed_cards::intrinsic_face_counters(values.loyalty, None).is_empty()
        && crate::game::printed_cards::self_etb_counter_replacements(
            &values.replacement_definitions,
        )
        .is_empty()
}

/// CR 707.2 + CR 603.6a: Build the Layer C / §2.2a probe `TokenSpec` for a
/// copy-prefix batch from the prefix's shared copiable values. The probe needs
/// only the copiable values (CR 707.2): token art comes from the live source at
/// resolution time (`token_copy::resolve`), so no `PrintedCardRef` is threaded
/// through the probe.
pub(crate) fn copy_probe_spec(
    ability: &ResolvedAbility,
    values: &crate::types::ability::CopiableValues,
) -> TokenSpec {
    copy_probe_spec_for(
        ability.source_id,
        ability.controller,
        ability.duration.clone(),
        values,
    )
}

pub(crate) fn copy_probe_spec_for(
    source_id: ObjectId,
    controller: PlayerId,
    sacrifice_at: Option<Duration>,
    values: &crate::types::ability::CopiableValues,
) -> TokenSpec {
    use crate::types::proposed_event::TokenCharacteristics;
    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: values.name.clone(),
            power: values.power,
            toughness: values.toughness,
            core_types: values.card_types.core_types.clone(),
            subtypes: values.card_types.subtypes.clone(),
            supertypes: values.card_types.supertypes.clone(),
            colors: values.color.clone(),
            keywords: values.keywords.clone(),
        },
        script_name: values.name.clone(),
        static_abilities: vec![],
        enter_with_counters: vec![],
        tapped: false,
        enters_attacking: false,
        sacrifice_at,
        source_id,
        controller,
        attach_to: None,
    }
}

/// CR 111.10: Enumerate the trigger definitions a BASE `Token` spec injects on
/// the produced token, WITHOUT creating an object — the §2.3a non-observer gate
/// input. Predefined subtype abilities (`predefined_token_abilities`) are
/// ACTIVATED abilities and register no trigger; spec `static_abilities` are
/// continuous (CR 611) and register no trigger. A `Role` subtype would inject
/// `predefined_role_token_spec(name).triggers`, but Roles are created via
/// `attach_to`, which `spec_emits_only_etb_pair` already excludes — so a
/// passing spec injects no triggers. Collected explicitly (defense in depth):
/// if a future spec ever carries a Role subtype while passing the gate, its
/// triggers are surfaced here for classification.
fn base_token_trigger_defs(spec: &TokenSpec) -> Vec<TriggerDefinition> {
    let mut out: Vec<TriggerDefinition> = Vec::new();
    if spec.characteristics.subtypes.iter().any(|s| s == "Role") {
        if let Some(role) = predefined_role_token_spec(&spec.characteristics.display_name) {
            out.extend(role.triggers);
        }
    }
    out
}

fn normalized_token_static_definition(mut static_def: StaticDefinition) -> StaticDefinition {
    for modification in &mut static_def.modifications {
        if let ContinuousModification::GrantTrigger { trigger } = modification {
            normalize_token_self_lki_trigger(trigger.as_mut());
        }
    }
    static_def
}

fn normalize_token_self_lki_trigger(trigger: &mut TriggerDefinition) {
    if trigger.mode == TriggerMode::ChangesZone
        && trigger.valid_card == Some(TargetFilter::SelfRef)
        && trigger.origin == Some(Zone::Battlefield)
        && trigger.destination == Some(Zone::Graveyard)
    {
        // CR 603.6c + CR 603.10a + CR 111.7: a token's own dies trigger
        // functions from last-known battlefield information and triggers before
        // the token ceases to exist. The runtime LKI scan therefore visits the
        // departed token as a Battlefield source, not as a graveyard source.
        trigger.trigger_zones = vec![Zone::Battlefield];
    }
}

/// CR 111.1 + CR 111.4: Resolve a base `Effect::Token`'s per-resolution
/// `TokenSpec` (+ owner, enter-tap state, resolved count) read-only, mirroring
/// the prefix of `resolve` exactly. Returns `None` for any non-`Token` effect.
pub(crate) fn resolve_token_spec(
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<(
    TokenSpec,
    PlayerId,
    crate::types::proposed_event::EtbTapState,
    u32,
)> {
    let Effect::Token {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
        owner,
        attach_to,
        enters_attacking,
        supertypes,
        static_abilities,
        enter_with_counters,
    } = &ability.effect
    else {
        return None;
    };

    let count = resolve_quantity_with_targets(state, count, ability).max(0) as u32;
    let token_owner = resolve_token_owner(state, ability, owner);
    let attach_target = attach_to
        .as_ref()
        .and_then(|f| resolve_attach_host(state, ability, f));

    let parsed = parse_token_script(name).or_else(|| {
        build_token_attrs_from_effect(
            name, power, toughness, types, colors, keywords, supertypes, state, ability,
        )
    });

    let resolved_etb_counters: Vec<(CounterType, u32)> = enter_with_counters
        .iter()
        .map(|(ct, qty)| {
            let n = resolve_quantity_with_targets(state, qty, ability).max(0) as u32;
            (ct.clone(), n)
        })
        .collect();

    let spec = build_token_spec(
        name,
        parsed.as_ref(),
        power,
        toughness,
        *tapped,
        *enters_attacking,
        static_abilities.clone(),
        resolved_etb_counters,
        attach_target,
        ability,
        state,
    );

    Some((
        spec,
        token_owner,
        crate::types::proposed_event::EtbTapState::from_seeded_tapped(*tapped),
        count,
    ))
}

/// CR 303.4: Resolve the host an Aura/Role token is created
/// "attached to" from its `attach_to: TargetFilter`. Mirrors
/// `attach::resolve_object_filter`'s ParentTarget arm (the first
/// `TargetRef::Object` in `ability.targets`, which the for-each loop's
/// per-iteration rebind populates) plus the event-context path. A `Typed`
/// targeting filter (e.g. "attached to target creature you control") also reads
/// the chosen target out of `ability.targets`. Returns `None` when no legal
/// host has been bound — the apply path then leaves the token unattached and
/// the CR 704.5m SBA (an unattached Aura) moves the orphaned Aura to the
/// graveyard.
///
/// This does NOT duplicate attach legality: the actual attach is performed by
/// `attach::attach_to` / `attach::attach_to_player`, the single authority for
/// CR 701.3a / CR 301.5 / CR 303.4 host validity.
fn resolve_attach_host(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<AttachTarget> {
    match filter {
        // Event-context hosts ("attached to the triggering creature") resolve the
        // triggering event's subject via the shared event-context resolver.
        TargetFilter::TriggeringSource | TargetFilter::AttachedTo => {
            crate::game::targeting::resolve_event_context_target(state, filter, ability.source_id)
                .map(target_ref_to_attach_target)
        }
        // ParentTarget and any targeting filter resolve to the chosen target
        // carried in `ability.targets`. ParentTarget is bound per-iteration by the
        // for-each rebind; a `Typed` targeting filter is the single-target
        // "attached to target creature" case (CR 115.1a). Both read the first
        // `TargetRef::Object` in `ability.targets`. Player-host Auras (CR 303.4
        // permits a player host) are not yet implemented — no current card creates
        // a token attached to a player, so a Player slot yields `None` here.
        _ => ability.targets.iter().find_map(|target| match target {
            TargetRef::Object(id) => Some(AttachTarget::Object(*id)),
            TargetRef::Player(_) => None,
        }),
    }
}

/// Convert a resolved `TargetRef` into an `AttachTarget` host. Player and Object
/// hosts both reach the apply path (CR 303.4 allows player-host Auras).
fn target_ref_to_attach_target(target: TargetRef) -> AttachTarget {
    match target {
        TargetRef::Object(id) => AttachTarget::Object(id),
        TargetRef::Player(id) => AttachTarget::Player(id),
    }
}

/// CR 109.4 + CR 111.2: Resolve the player who creates (and therefore
/// controls) a token from its `owner: TargetFilter`. Single authority for
/// both `Effect::Token` and `Effect::CopyTokenOf` — the latter delegates here
/// so "target opponent creates a token that's a copy of it" routes through the
/// exact same resolution path.
pub(crate) fn resolve_token_owner(
    state: &GameState,
    ability: &ResolvedAbility,
    owner_filter: &TargetFilter,
) -> PlayerId {
    // CR 115.1: Context-ref filters route through the central helper so chain
    // target propagation cannot leak the parent's Player target into a sub
    // CreateToken whose `owner: Controller`. The helper handles
    // ParentTargetController's spell-chain Object lookup centrally.
    if owner_filter.is_context_ref() {
        return super::resolve_player_for_context_ref(state, ability, owner_filter);
    }
    // CR 109.4: Non-context-ref `owner` (e.g. "target opponent creates a
    // token") — the token's creator is the chosen *player* target. Scan
    // `ability.targets` in reverse for the last `TargetRef::Player`, mirroring
    // `relative_filter_controller`. `TargetRef::Object` slots are deliberately
    // ignored: `Effect::CopyTokenOf` can carry an Object slot for the copy
    // *source* alongside the player `owner` slot, and resolving the source
    // object's controller as the token owner would be wrong. When no player
    // slot exists, the controller creates the token.
    ability
        .targets
        .iter()
        .rev()
        .find_map(|target| match target {
            TargetRef::Player(pid) => Some(*pid),
            TargetRef::Object(_) => None,
        })
        .unwrap_or(ability.controller)
}

#[allow(clippy::too_many_arguments)]
fn build_token_attrs_from_effect(
    name: &str,
    power: &PtValue,
    toughness: &PtValue,
    types: &[String],
    colors: &[ManaColor],
    keywords: &[Keyword],
    supertypes: &[Supertype],
    state: &GameState,
    ability: &ResolvedAbility,
) -> Option<TokenAttrs> {
    if types.is_empty()
        && colors.is_empty()
        && keywords.is_empty()
        && matches!(power, PtValue::Fixed(0))
        && matches!(toughness, PtValue::Fixed(0))
    {
        return None;
    }

    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for token_type in types {
        let trimmed = token_type.trim();
        if let Ok(core_type) = CoreType::from_str(trimmed) {
            if !core_types.contains(&core_type) {
                core_types.push(core_type);
            }
        } else if !trimmed.is_empty() {
            subtypes.push(trimmed.to_string());
        }
    }

    let resolved_power = resolve_pt_value(power, state, ability);
    let resolved_toughness = resolve_pt_value(toughness, state, ability);
    if core_types.is_empty() && (resolved_power != 0 || resolved_toughness != 0) {
        core_types.push(CoreType::Creature);
    }

    let has_power_toughness = resolved_power != 0 || resolved_toughness != 0;
    let has_explicit_pt =
        !matches!(power, PtValue::Fixed(0)) || !matches!(toughness, PtValue::Fixed(0));
    let is_creature = core_types.contains(&CoreType::Creature);
    Some(TokenAttrs {
        display_name: name.to_string(),
        power: (is_creature || has_explicit_pt || has_power_toughness).then_some(resolved_power),
        toughness: (is_creature || has_explicit_pt || has_power_toughness)
            .then_some(resolved_toughness),
        core_types,
        subtypes,
        colors: colors.to_vec(),
        keywords: keywords.to_vec(),
        supertypes: supertypes.to_vec(),
    })
}

fn resolve_pt_value(value: &PtValue, state: &GameState, ability: &ResolvedAbility) -> i32 {
    match value {
        PtValue::Fixed(n) => *n,
        PtValue::Variable(_) => 0,
        PtValue::Quantity(expr) => resolve_quantity_with_targets(state, expr, ability),
    }
}

// ── Predefined token abilities (CR 111.10) ────────────────────────────
// Data-driven lookup: subtype → ability constructors.

/// CR 111.10a: Treasure — "{T}, Sacrifice this artifact: Add one mana of any color."
fn treasure_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![
                    ManaColor::White,
                    ManaColor::Blue,
                    ManaColor::Black,
                    ManaColor::Red,
                    ManaColor::Green,
                ],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
}

/// CR 111.10c: Gold — "Sacrifice this token: Add one mana of any color."
fn gold_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: QuantityExpr::Fixed { value: 1 },
                color_options: vec![
                    ManaColor::White,
                    ManaColor::Blue,
                    ManaColor::Black,
                    ManaColor::Red,
                    ManaColor::Green,
                ],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::SelfRef,
        1,
    )))
}

/// CR 111.10b: Food — "{2}, {T}, Sacrifice this artifact: You gain 3 life."
fn food_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            },
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
}

/// CR 111.10f: Clue — "{2}, Sacrifice this artifact: Draw a card."
fn clue_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
}

/// CR 111.10g: Blood — "{1}, {T}, Discard a card, Sacrifice this artifact: Draw a card."
fn blood_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 1,
                },
            },
            AbilityCost::Tap,
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                selection: crate::types::ability::CardSelectionMode::Chosen,
                self_scope: crate::types::ability::DiscardSelfScope::FromHand,
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
}

/// CR 106.1 + CR 701.21a: Eldrazi Spawn — "Sacrifice this token: Add {C}."
/// Modern Eldrazi Spawn printings (from Rise of the Eldrazi onward) use this
/// no-tap sacrifice mana ability. Applied by subtype lookup so every token
/// with subtype "Spawn" gains the ability without per-card registration.
fn spawn_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Sacrifice(SacrificeCost::count(
        TargetFilter::SelfRef,
        1,
    )))
}

/// CR 111.10h: Powerstone — "{T}: Add {C}. This mana can't be spent to cast a nonartifact spell."
fn powerstone_ability() -> AbilityDefinition {
    use crate::types::ability::ManaSpendRestriction;
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation {
                spell_type: "Artifact".to_string(),
                ability: crate::types::mana::AbilityActivationScope::Any,
            }],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap)
}

/// CR 111.10s: Map — "{1}, {T}, Sacrifice this artifact: Target creature you control explores."
fn map_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::TargetOnly {
            target: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Explore,
    ))
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 1,
                },
            },
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
    .activation_restrictions(vec![ActivationRestriction::AsSorcery])
}

/// CR 111.10u: Lander — "{2}, {T}, Sacrifice this token: Search your library
/// for a basic land card, put it onto the battlefield tapped, then shuffle."
fn lander_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        // CR 111.10u: search the controller's library for a basic land card.
        Effect::SearchLibrary {
            filter: TargetFilter::Typed(TypedFilter::land().properties(vec![
                FilterProp::HasSupertype {
                    value: Supertype::Basic,
                },
            ])),
            count: QuantityExpr::Fixed { value: 1 },
            reveal: false,
            target_player: None,
            selection_constraint: SearchSelectionConstraint::default(),
            split: None,
            source_zones: vec![crate::types::zones::Zone::Library],
        },
    )
    .sub_ability(
        AbilityDefinition::new(
            AbilityKind::Activated,
            // CR 614.1c: "enters tapped" is an as-enters replacement effect.
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                enters_under: None,
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
                enters_modified_if: None,
            },
        )
        // CR 111.10u: then shuffle the controller's library.
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            },
        )),
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            },
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
}

/// CR 111.10v: Mutagen — "{1}, {T}, Sacrifice this token: Put a +1/+1 counter
/// on target creature. Activate only as a sorcery."
fn mutagen_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        // CR 122.1: a single +1/+1 counter on the chosen target creature.
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Typed(TypedFilter::creature()),
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 1,
                },
            },
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
    // CR 307.5: "Activate only as a sorcery" — controller has priority, during
    // their main phase, with the stack empty.
    .activation_restrictions(vec![ActivationRestriction::AsSorcery])
}

/// CR 111.10 (Fallout): Junk — "{T}, Sacrifice this artifact: Exile the top card of your
/// library. You may play that card this turn. Activate only as a sorcery."
fn junk_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::ExileTop {
            player: TargetFilter::Controller,
            count: QuantityExpr::Fixed { value: 1 },
            face_down: false,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GrantCastingPermission {
            permission: CastingPermission::PlayFromExile {
                duration: Duration::UntilEndOfTurn,
                granted_to: PlayerId(0),
                frequency: CastFrequency::Unlimited,
                source_id: None,
                invalidation: None,
                exiled_by_ability_controller: None,
                mana_spend_permission: None,
                card_filter: None,
                single_use_group: None,
                single_use: false,
                cast_cost_raise: None,
                land_enter_tapped: crate::types::zones::EtbTapState::Unspecified,
            },
            target: TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            grantee: PermissionGrantee::AbilityController,
        },
    ))
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
    .activation_restrictions(vec![ActivationRestriction::AsSorcery])
}

/// CR 111.10i: Incubator — "{2}: Transform this artifact." Back face is a 0/0
/// Phyrexian artifact creature (see `incubator_phyrexian_back_face`).
fn incubator_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Transform {
            target: TargetFilter::SelfRef,
        },
    )
    .cost(AbilityCost::Mana {
        cost: ManaCost::Cost {
            shards: vec![],
            generic: 2,
        },
    })
}

/// CR 111.10i: Back face of an Incubator double-faced token.
fn incubator_phyrexian_back_face() -> BackFaceData {
    BackFaceData {
        name: "Phyrexian Token".to_string(),
        power: Some(0),
        toughness: Some(0),
        loyalty: None,
        defense: None,
        card_types: CardType {
            supertypes: vec![],
            core_types: vec![CoreType::Artifact, CoreType::Creature],
            subtypes: vec!["Phyrexian".to_string()],
        },
        mana_cost: ManaCost::default(),
        keywords: vec![],
        abilities: vec![],
        trigger_definitions: Default::default(),
        replacement_definitions: Default::default(),
        static_definitions: Default::default(),
        color: vec![],
        printed_ref: None,
        modal: None,
        additional_cost: None,
        strive_cost: None,
        casting_restrictions: vec![],
        casting_options: vec![],
        layout_kind: None,
    }
}

/// CR 111.10 (Duskmourn): Shard — "{2}, Sacrifice this enchantment: Scry 1, then draw a card."
fn shard_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    )
    .sub_ability(AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Draw {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    ))
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            },
            AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
        ],
    })
}

/// CR 111.10: Predefined token abilities keyed by subtype.
/// Returns ability definitions to inject for the given subtype, or empty if none.
pub fn predefined_token_abilities(subtype: &str) -> Vec<AbilityDefinition> {
    match subtype {
        "Treasure" => vec![treasure_ability()],
        "Food" => vec![food_ability()],
        "Gold" => vec![gold_ability()],
        "Clue" => vec![clue_ability()],
        "Blood" => vec![blood_ability()],
        "Powerstone" => vec![powerstone_ability()],
        "Map" => vec![map_ability()],
        "Spawn" => vec![spawn_ability()],
        "Lander" => vec![lander_ability()],
        "Mutagen" => vec![mutagen_ability()],
        "Junk" => vec![junk_ability()],
        "Incubator" => vec![incubator_ability()],
        "Shard" => vec![shard_ability()],
        _ => vec![],
    }
}

/// CR 111.10: human-readable rules text for predefined tokens, keyed by
/// subtype. Mirrors `predefined_token_abilities` arm-for-arm — keep the two
/// `match` blocks edited together (single source of truth). Returns `None`
/// for subtypes whose printed text has not been backfilled; the frontend
/// then renders no alt-text, as it does today.
fn predefined_token_rules_text(subtype: &str) -> Option<&'static str> {
    match subtype {
        // CR 111.10c
        "Gold" => Some("Sacrifice this token: Add one mana of any color."),
        // CR 111.10u
        "Lander" => Some(
            "{2}, {T}, Sacrifice this token: Search your library for a basic \
             land card, put it onto the battlefield tapped, then shuffle.",
        ),
        "Junk" => Some(
            "{T}, Sacrifice this artifact: Exile the top card of your library. \
             You may play that card this turn. Activate only as a sorcery.",
        ),
        "Incubator" => Some("{2}: Transform this artifact."),
        "Shard" => Some("{2}, Sacrifice this enchantment: Scry 1, then draw a card."),
        _ => None,
    }
}

/// CR 303.4: `FilterProp::EnchantedBy` is source-relative when the source is
/// an Aura — at layer-evaluation time the filter resolves to whichever
/// creature this specific Role is attached to, so two Roles on two different
/// creatures only modify their own enchanted creature.
fn enchanted_creature_filter() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::creature().properties(vec![FilterProp::EnchantedBy]))
}

/// Build a `StaticDefinition` whose `affected` is the Role's enchanted
/// creature (CR 303.4) with the given modifications and oracle text.
fn role_static(modifications: Vec<ContinuousModification>, description: &str) -> StaticDefinition {
    StaticDefinition::continuous()
        .affected(enchanted_creature_filter())
        .modifications(modifications)
        .description(description.to_string())
}

/// CR 111.10j: Cursed Role — "Enchanted creature has base power and
/// toughness 1/1." `SetPower`/`SetToughness` apply at layer 7b (set base P/T,
/// `layers.rs:1167-1172`), which is the correct layer for "base power and
/// toughness X/Y". Modifiers in layer 7c (`AddPower` from `+N/+N` pumps,
/// counters, etc.) still stack on top per CR 613.1, so a Cursed creature
/// with +2/+2 ends at 3/3 — the "base" set is the *floor* of the calculation,
/// not a final override.
fn cursed_role_statics() -> Vec<StaticDefinition> {
    vec![role_static(
        vec![
            ContinuousModification::SetPower { value: 1 },
            ContinuousModification::SetToughness { value: 1 },
        ],
        "Enchanted creature has base power and toughness 1/1.",
    )]
}

/// CR 111.10k: Monster Role — "Enchanted creature gets +1/+1 and has trample."
fn monster_role_statics() -> Vec<StaticDefinition> {
    vec![role_static(
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            },
        ],
        "Enchanted creature gets +1/+1 and has trample.",
    )]
}

/// CR 111.10m: Royal Role — "Enchanted creature gets +1/+1 and has ward {1}."
fn royal_role_statics() -> Vec<StaticDefinition> {
    vec![role_static(
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Ward(WardCost::Mana(ManaCost::generic(1))),
            },
        ],
        "Enchanted creature gets +1/+1 and has ward {1}.",
    )]
}

/// CR 111.10p: Virtuous Role — "Enchanted creature gets +1/+1 for each
/// enchantment you control."
///
/// `ControllerRef::You` on the count filter binds to the *Aura's* controller
/// at evaluation time (CR 109.5: an Aura's controller is the player who
/// controls the Aura, not necessarily who controls the enchanted creature),
/// which is the correct reading: "you" in a Role's text is the Role
/// controller. `AddDynamicPower`/`AddDynamicToughness` apply at layer 7c,
/// after `AddPower`/`AddToughness` but before switch-power/toughness.
fn virtuous_role_statics() -> Vec<StaticDefinition> {
    let enchantments_you_control = QuantityExpr::Ref {
        qty: QuantityRef::ObjectCount {
            filter: TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Enchantment).controller(ControllerRef::You),
            ),
        },
    };
    vec![role_static(
        vec![
            ContinuousModification::AddDynamicPower {
                value: enchantments_you_control.clone(),
            },
            ContinuousModification::AddDynamicToughness {
                value: enchantments_you_control,
            },
        ],
        "Enchanted creature gets +1/+1 for each enchantment you control.",
    )]
}

/// CR 111.10r: Young Hero Role — "Enchanted creature has 'Whenever this
/// creature attacks, if its toughness is 3 or less, put a +1/+1 counter on
/// it.'"
///
/// `GrantTrigger` attaches the triggered ability to the enchanted creature
/// via the layer system. Once granted, the trigger's source is the
/// enchanted creature, so:
/// - `valid_card = None` → matches when the source itself attacks
///   (`trigger_matchers::matching_attack_events` defaults to `attacker == source`).
/// - `condition: SelfToughness LE 3` → CR 603.4 intervening-if checked at
///   trigger event time against the enchanted creature's current toughness.
/// - `Effect::PutCounter { target: SelfRef }` → "on it" resolves to the
///   trigger's source, the enchanted creature.
fn young_hero_role_statics() -> Vec<StaticDefinition> {
    let put_counter = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::PutCounter {
            counter_type: CounterType::Plus1Plus1,
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::SelfRef,
        },
    );

    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .execute(put_counter)
        // CR 603.4 intervening-if: SelfToughness ≤ 3 of the trigger source.
        .condition(TriggerCondition::QuantityComparison {
            lhs: QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: crate::types::ability::ObjectScope::Source,
                },
            },
            comparator: Comparator::LE,
            rhs: QuantityExpr::Fixed { value: 3 },
        })
        .description(
            "Whenever this creature attacks, if its toughness is 3 or less, \
             put a +1/+1 counter on it."
                .to_string(),
        );

    vec![role_static(
        vec![ContinuousModification::GrantTrigger {
            trigger: Box::new(trigger),
        }],
        "Enchanted creature has \"Whenever this creature attacks, if its \
         toughness is 3 or less, put a +1/+1 counter on it.\"",
    )]
}

/// CR 111.10n: Sorcerer Role — "Enchanted creature gets +1/+1 and has
/// 'Whenever this creature attacks, scry 1.'"
///
/// Same shape as Royal/Monster (additive +1/+1) plus a `GrantTrigger` for
/// the inner attacks-scry. The granted trigger has no condition (no
/// intervening-if) — Sorcerer's trigger is unconditional, unlike Young
/// Hero's. `Effect::Scry { target: TargetFilter::Controller }` resolves to
/// the granted trigger's source's controller, i.e. the controller of the
/// enchanted creature when it attacks.
fn sorcerer_role_statics() -> Vec<StaticDefinition> {
    let scry_one = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::Scry {
            count: QuantityExpr::Fixed { value: 1 },
            target: TargetFilter::Controller,
        },
    );
    let trigger = TriggerDefinition::new(TriggerMode::Attacks)
        .execute(scry_one)
        .description("Whenever this creature attacks, scry 1.".to_string());

    vec![role_static(
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
            ContinuousModification::GrantTrigger {
                trigger: Box::new(trigger),
            },
        ],
        "Enchanted creature gets +1/+1 and has \"Whenever this creature \
         attacks, scry 1.\"",
    )]
}

/// Per-Role injection payload: continuous modifications for the enchanted
/// creature plus triggers that fire on the *Aura itself* (not granted to
/// the enchanted creature).
///
/// Most Roles have only `statics` populated. Wicked is the only Role today
/// with a self-trigger on the Aura — its dies-trigger fires when the Role
/// token leaves the battlefield, which is fundamentally a property of the
/// token, not of the enchanted creature, so it cannot be expressed as a
/// `GrantTrigger` modification on a static.
#[derive(Default)]
struct RoleSpec {
    statics: Vec<StaticDefinition>,
    triggers: Vec<TriggerDefinition>,
}

impl RoleSpec {
    fn statics_only(statics: Vec<StaticDefinition>) -> Self {
        Self {
            statics,
            triggers: Vec::new(),
        }
    }
}

/// CR 111.10q: Wicked Role — "Enchanted creature gets +1/+1, and 'When
/// this token is put into a graveyard from the battlefield, each opponent
/// loses 1 life.'"
///
/// The +1/+1 is a static affecting the enchanted creature; the dies-trigger
/// is on the Aura itself (CR 111.10q's "this token" refers to the Aura, not
/// the enchanted creature) and is therefore added directly to the token's
/// `trigger_definitions` rather than via `GrantTrigger`.
///
/// `player_scope: PlayerFilter::Opponent` on the inner ability iterates the
/// `LoseLife` once per opponent of the trigger controller, rebinding
/// `controller` per iteration (see `effects/mod.rs:917`). With
/// `target: None`, each iteration's loss applies to the rebound controller
/// — the standard "each opponent loses N life" pattern.
fn wicked_role_spec() -> RoleSpec {
    let pump = role_static(
        vec![
            ContinuousModification::AddPower { value: 1 },
            ContinuousModification::AddToughness { value: 1 },
        ],
        "Enchanted creature gets +1/+1.",
    );

    let opponents_lose_one = AbilityDefinition::new(
        AbilityKind::Database,
        Effect::LoseLife {
            amount: QuantityExpr::Fixed { value: 1 },
            target: None,
        },
    )
    .player_scope(PlayerFilter::Opponent);

    let dies_trigger = TriggerDefinition::new(TriggerMode::ChangesZone)
        .valid_card(TargetFilter::SelfRef)
        .origin(Zone::Battlefield)
        .destination(Zone::Graveyard)
        // CR 603.6c + CR 603.10a + CR 111.7: the token's own dies trigger
        // functions from last-known battlefield information before the token
        // ceases to exist, so the trigger scanner must visit it as a
        // Battlefield source.
        .trigger_zones(vec![Zone::Battlefield])
        .execute(opponents_lose_one)
        .description(
            "When this token is put into a graveyard from the battlefield, \
             each opponent loses 1 life."
                .to_string(),
        );

    RoleSpec {
        statics: vec![pump],
        triggers: vec![dies_trigger],
    }
}

/// CR 111.10: Return the predefined Role token spec by display name, or
/// `None` if `name` is not an implemented Role.
///
/// All Role tokens share the `Role` subtype, so dispatch must be by display
/// name — subtype alone cannot distinguish the seven variants.
///
/// CR 111.10: a Role token's printed name is "<Role> Role" (e.g. "Monster Role"),
/// which is exactly what the parser/token creation assigns as the display name.
/// Strip that trailing " Role" before matching so real tokens dispatch correctly;
/// the bare role word ("Monster") is also accepted for internal/test callers.
fn predefined_role_token_spec(name: &str) -> Option<RoleSpec> {
    let role = name.strip_suffix(" Role").unwrap_or(name);
    match role {
        "Cursed" => Some(RoleSpec::statics_only(cursed_role_statics())),
        "Monster" => Some(RoleSpec::statics_only(monster_role_statics())),
        "Royal" => Some(RoleSpec::statics_only(royal_role_statics())),
        "Sorcerer" => Some(RoleSpec::statics_only(sorcerer_role_statics())),
        "Virtuous" => Some(RoleSpec::statics_only(virtuous_role_statics())),
        "Wicked" => Some(wicked_role_spec()),
        "Young Hero" => Some(RoleSpec::statics_only(young_hero_role_statics())),
        _ => None,
    }
}

/// Inject predefined token abilities based on the token's subtypes and name.
///
/// Two dispatch paths:
/// - **Subtype** (CR 111.10): Treasure, Food, Clue, Blood, Powerstone,
///   Map, Spawn — each subtype contributes a single activated ability
///   (`predefined_token_abilities`).
/// - **Name** (CR 111.10): Role tokens. All seven Roles share the `Role`
///   subtype, so dispatch is by display name via `predefined_role_token_spec`.
///   Roles contribute static abilities that modify the enchanted creature
///   (Cursed/Monster/Royal/Sorcerer/Virtuous/Young Hero) and may also
///   contribute self-triggers on the Aura (Wicked).
///
/// Written to mirror updates onto both `base_*` and live definition fields;
/// the layer pass rebuilds live from base on each pass, but several code
/// paths (SBAs, action enumeration) consult the live set directly between
/// passes so keeping them in sync here avoids a one-frame lag.
/// CR 111.3 + CR 111.10: Apply predefined token abilities first; fall back to
/// catalog `rules_text` only when the predefined path contributed nothing
/// (artifacts, Roles, Incubator, …).
pub(super) fn inject_resolved_token_abilities(
    state: &mut GameState,
    obj_id: crate::types::identifiers::ObjectId,
) {
    let Some(materialized) = materialize_token_ability_payload_for_object(state, obj_id) else {
        return;
    };
    if materialized.source == TokenAbilitySource::CatalogRulesText
        && !materialized.has_functional_payload()
    {
        return;
    }
    apply_token_ability_materialization(state, obj_id, materialized, true);
}

/// CR 111.3 + CR 111.4: Grant catalog `rules_text` when token creation resolved
/// a `token_image_ref` preset whose abilities are not already covered by the
/// predefined path (e.g. SOS Pest attack life gain).
pub(crate) fn inject_catalog_token_abilities(
    state: &mut GameState,
    obj_id: crate::types::identifiers::ObjectId,
) {
    let Some(preset) = state.objects.get(&obj_id).and_then(|obj| {
        obj.token_image_ref.as_ref().and_then(|image_ref| {
            crate::game::token_presets::known_token_preset_by_id(&image_ref.preset_id)
        })
    }) else {
        return;
    };
    let materialized = materialize_catalog_token_payload(preset);
    if materialized.source == TokenAbilitySource::CatalogRulesText
        && materialized.has_functional_payload()
    {
        apply_token_ability_materialization(state, obj_id, materialized, true);
    }
}

fn apply_token_ability_materialization(
    state: &mut GameState,
    obj_id: crate::types::identifiers::ObjectId,
    materialized: TokenAbilityMaterialization,
    suppress_catalog_if_existing_statics: bool,
) -> bool {
    let Some(obj) = state.objects.get_mut(&obj_id) else {
        return false;
    };
    // CR 111.3: A token's abilities are defined by the effect that creates it, so
    // when the creating effect already granted this token abilities via a
    // `with "..."` clause (parsed into `static_definitions` at creation, before
    // this fallback runs), those are authoritative and complete. The catalog
    // preset's `rules_text` is then only a display/art mirror and MUST NOT inject
    // functional abilities — critically, the matched art preset can be a
    // different printing whose text lists extra keyword actions (a Kamigawa
    // "crews Vehicles as though its power were 2 greater" Pilot token rendered
    // with the Aetherdrift "saddles Mounts and crews Vehicles …" art), so
    // injecting it grants a second crew static and doubles the contribution (a
    // 1/1 Pilot crews for 5 instead of 3). Skip functional injection whenever the
    // token already carries granted statics; still record the display rules text.
    // Tokens created by name with no explicit ability clause (Treasure, Pest,
    // Equipment presets) reach here with no prior statics and inject normally.
    if suppress_catalog_if_existing_statics
        && materialized.source == TokenAbilitySource::CatalogRulesText
        && !obj.static_definitions.is_empty()
    {
        if obj.token_rules_text.is_none() {
            obj.token_rules_text = materialized.rules_text;
        }
        return true;
    }

    apply_token_ability_payload(obj, materialized);
    true
}

fn apply_token_ability_payload(obj: &mut GameObject, materialized: TokenAbilityMaterialization) {
    if !materialized.static_definitions.is_empty() {
        Arc::make_mut(&mut obj.base_static_definitions)
            .extend(materialized.static_definitions.iter().cloned());
        for static_def in materialized.static_definitions {
            obj.static_definitions.push(static_def);
        }
    }
    if !materialized.modifications.is_empty() {
        let rules_text = materialized.rules_text.clone().unwrap_or_default();
        let static_def = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(materialized.modifications)
            .description(rules_text);
        Arc::make_mut(&mut obj.base_static_definitions).push(static_def.clone());
        obj.static_definitions.push(static_def);
    }
    if !materialized.trigger_definitions.is_empty() {
        Arc::make_mut(&mut obj.base_trigger_definitions)
            .extend(materialized.trigger_definitions.iter().cloned());
        for trigger in materialized.trigger_definitions {
            obj.trigger_definitions.push(trigger);
        }
    }
    if !materialized.abilities.is_empty() {
        Arc::make_mut(&mut obj.abilities).extend(materialized.abilities.iter().cloned());
        Arc::make_mut(&mut obj.base_abilities).extend(materialized.abilities);
    }
    if !materialized.keywords.is_empty() {
        for keyword in materialized.keywords {
            if !obj.base_keywords.contains(&keyword) {
                obj.base_keywords.push(keyword.clone());
            }
            let already_live = obj.keywords.contains(&keyword); // allow-raw-authority: structural live keyword insertion de-dupe, not an effective keyword query
            if !already_live {
                obj.keywords.push(keyword);
            }
        }
    }
    if obj.back_face.is_none() {
        obj.back_face = materialized.back_face;
    }
    if obj.token_rules_text.is_none() {
        obj.token_rules_text = materialized.rules_text;
    }
}

fn catalog_rules_text_abilities(
    rules_text: &str,
) -> (
    Vec<StaticDefinition>,
    Vec<ContinuousModification>,
    Vec<String>,
) {
    let mut static_definitions = Vec::new();
    let mut modifications = Vec::new();
    let mut unparsed_lines = Vec::new();
    for line in rules_text
        .split('\n')
        .map(str::trim)
        .filter(|line| !line.is_empty())
    {
        let parsed_statics = crate::parser::oracle_static::parse_static_line_multi(line);
        if parsed_statics.is_empty() {
            let parsed_modifications = crate::parser::oracle_static::classify_quoted_inner(line);
            if parsed_modifications.is_empty() {
                unparsed_lines.push(line.to_string());
            } else {
                modifications.extend(parsed_modifications);
            }
        } else {
            static_definitions.extend(
                parsed_statics
                    .into_iter()
                    .map(normalized_token_static_definition),
            );
        }
    }
    (static_definitions, modifications, unparsed_lines)
}

pub(super) fn inject_predefined_token_abilities(
    state: &mut GameState,
    obj_id: crate::types::identifiers::ObjectId,
) -> bool {
    let Some(obj) = state.objects.get(&obj_id) else {
        return false;
    };
    let materialized = materialize_predefined_token_payload(&obj.name, &obj.card_types.subtypes);
    if materialized.source != TokenAbilitySource::Predefined {
        return false;
    }
    apply_token_ability_materialization(state, obj_id, materialized, false)
}

fn materialize_token_ability_payload_for_object(
    state: &GameState,
    obj_id: crate::types::identifiers::ObjectId,
) -> Option<TokenAbilityMaterialization> {
    let obj = state.objects.get(&obj_id)?;
    let preset = obj.token_image_ref.as_ref().and_then(|image_ref| {
        crate::game::token_presets::known_token_preset_by_id(&image_ref.preset_id)
    });

    Some(materialize_token_ability_payload(
        &obj.name,
        &obj.card_types.subtypes,
        preset,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::ability_utils::{
        build_resolved_from_def, build_resolved_from_def_with_targets,
    };
    use crate::game::engine::apply_as_current;
    use crate::game::printed_cards::intrinsic_copiable_values;
    use crate::game::zones::create_object;
    use crate::types::ability::TriggerDefinition;
    use crate::types::actions::GameAction;
    use crate::types::card_type::CardType;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::mana::ManaType;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;
    use std::sync::Arc;

    // ── Parser unit tests ───────────────────────────────────────────────

    #[test]
    fn parse_white_soldier() {
        let a = parse_token_script("w_1_1_soldier").unwrap();
        assert_eq!(a.display_name, "Soldier");
        assert_eq!(a.power, Some(1));
        assert_eq!(a.toughness, Some(1));
        assert!(a.core_types.contains(&CoreType::Creature));
        assert_eq!(a.colors, vec![ManaColor::White]);
        assert_eq!(a.subtypes, vec!["Soldier"]);
    }

    #[test]
    fn liminal_copy_token_trigger_state_serializes() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Trigger Source".to_string(),
            Zone::Battlefield,
        );
        {
            let source = state.objects.get_mut(&source_id).unwrap();
            source.base_trigger_definitions =
                Arc::new(vec![TriggerDefinition::new(TriggerMode::ChangesZone)]);
            source.materialize_base_trigger_definitions();
        }
        let values = intrinsic_copiable_values(state.objects.get(&source_id).unwrap());
        let (token_id, mut token) =
            reserve_liminal_token_object(&mut state, PlayerId(0), values.name.clone());
        token.is_token = true;
        apply_copiable_values_to_liminal_object(
            &mut token,
            &values,
            DisplaySource::Token,
            None,
            None,
        );
        state.objects.insert(token_id, token);

        serde_json::to_string(&state)
            .expect("a liminal copy token with triggered abilities must serialize");
    }

    #[test]
    fn parse_colorless_treasure() {
        let a = parse_token_script("c_a_treasure_sac").unwrap();
        assert_eq!(a.display_name, "Treasure");
        assert!(a.core_types.contains(&CoreType::Artifact));
        assert!(!a.core_types.contains(&CoreType::Creature));
        assert_eq!(a.power, None);
        assert!(a.colors.is_empty());
    }

    #[test]
    fn parse_green_elf_warrior() {
        let a = parse_token_script("g_1_1_elf_warrior").unwrap();
        assert_eq!(a.display_name, "Elf Warrior");
        assert_eq!((a.power, a.toughness), (Some(1), Some(1)));
        assert_eq!(a.colors, vec![ManaColor::Green]);
    }

    #[test]
    fn parse_keywords() {
        let a = parse_token_script("w_4_4_angel_flying_vigilance").unwrap();
        assert_eq!(a.display_name, "Angel");
        assert!(a.keywords.contains(&Keyword::Flying));
        assert!(a.keywords.contains(&Keyword::Vigilance));
        assert!(!a.subtypes.contains(&"Flying".to_string()));
    }

    #[test]
    fn parse_artifact_creature() {
        let a = parse_token_script("c_1_1_a_thopter_flying").unwrap();
        assert_eq!(a.display_name, "Thopter");
        assert!(a.core_types.contains(&CoreType::Creature));
        assert!(a.core_types.contains(&CoreType::Artifact));
        assert!(a.keywords.contains(&Keyword::Flying));
    }

    #[test]
    fn parse_multicolor() {
        let a = parse_token_script("wb_2_1_inkling_flying").unwrap();
        assert_eq!(a.display_name, "Inkling");
        assert!(a.colors.contains(&ManaColor::White));
        assert!(a.colors.contains(&ManaColor::Black));
    }

    #[test]
    fn parse_variable_pt() {
        let a = parse_token_script("g_x_x_ooze").unwrap();
        assert_eq!(a.display_name, "Ooze");
        assert!(a.core_types.contains(&CoreType::Creature));
        assert_eq!((a.power, a.toughness), (Some(0), Some(0)));
    }

    #[test]
    fn parse_enchantment() {
        let a = parse_token_script("c_e_shard_draw").unwrap();
        assert_eq!(a.display_name, "Shard");
        assert!(a.core_types.contains(&CoreType::Enchantment));
        assert!(!a.core_types.contains(&CoreType::Creature));
    }

    #[test]
    fn parse_multi_subtype_with_keyword() {
        let a = parse_token_script("w_2_2_cat_beast_lifelink").unwrap();
        assert_eq!(a.display_name, "Cat Beast");
        assert_eq!(a.subtypes, vec!["Cat", "Beast"]);
        assert!(a.keywords.contains(&Keyword::Lifelink));
    }

    #[test]
    fn parse_comma_separated_scripts_uses_first() {
        let a = parse_token_script("r_1_1_goblin,w_1_1_soldier").unwrap();
        assert_eq!(a.display_name, "Goblin");
        assert_eq!(a.colors, vec![ManaColor::Red]);
    }

    #[test]
    fn parse_returns_none_for_named_tokens() {
        assert!(parse_token_script("llanowar_elves").is_none());
        assert!(parse_token_script("storm_crow").is_none());
    }

    // ── Integration tests ───────────────────────────────────────────────

    fn token_ability(script: &str) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Token {
                name: script.to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    fn resolve_token(script: &str) -> (GameState, Vec<GameEvent>) {
        let mut state = GameState::new_two_player(42);
        let ability = token_ability(script);
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        (state, events)
    }

    #[test]
    fn controller_owned_token_ignores_scoped_player() {
        let mut state = GameState::new_two_player(42);
        let mut ability = token_ability("b_3_3_a_dalek_menace");
        ability.targets = vec![TargetRef::Player(PlayerId(1))];
        ability.set_scoped_player_recursive(PlayerId(1));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let token = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .find(|object| object.is_token)
            .expect("expected Dalek token");
        assert_eq!(token.controller, PlayerId(0));
        assert_eq!(token.owner, PlayerId(0));
    }

    #[test]
    fn creates_creature_with_correct_types() {
        let (state, _) = resolve_token("w_1_1_soldier");
        let obj = &state.objects[&state.battlefield[0]];

        assert_eq!(obj.name, "Soldier");
        assert_eq!(obj.power, Some(1));
        assert_eq!(obj.toughness, Some(1));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.color, vec![ManaColor::White]);
        assert_eq!(obj.card_id, CardId(0));
    }

    #[test]
    fn token_creation_records_creature_etb_after_attributes_are_applied() {
        let (state, _) = resolve_token("w_4_4_angel_flying");

        assert!(state
            .battlefield_entries_this_turn
            .iter()
            .any(|r| r.core_types.contains(&CoreType::Creature) && r.controller == PlayerId(0)));
        assert!(state
            .battlefield_entries_this_turn
            .iter()
            .any(|r| r.controller == PlayerId(0)
                && r.subtypes.iter().any(|s| s.eq_ignore_ascii_case("Angel"))));
    }

    #[test]
    fn creates_artifact_without_creature_type() {
        let (state, _) = resolve_token("c_a_treasure_sac");
        let obj = &state.objects[&state.battlefield[0]];

        assert_eq!(obj.name, "Treasure");
        assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
        assert!(!obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.power, None);
    }

    #[test]
    fn applies_keywords() {
        let (state, _) = resolve_token("r_4_4_dragon_flying");
        let obj = &state.objects[&state.battlefield[0]];

        assert_eq!(obj.name, "Dragon");
        assert_eq!(obj.power, Some(4));
        assert!(obj.keywords.contains(&Keyword::Flying));
        assert_eq!(obj.color, vec![ManaColor::Red]);
    }

    #[test]
    fn fallback_for_plain_name() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "Soldier".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&state.battlefield[0]];
        assert_eq!(obj.name, "Soldier");
        assert_eq!(obj.power, Some(1));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
    }

    #[test]
    fn emits_token_created_event() {
        let (_, events) = resolve_token("w_1_1_soldier");

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TokenCreated { name, .. } if name == "Soldier")));
    }

    /// CR 111.1 + CR 603.6a: Token creation must emit `ZoneChanged { from: None,
    /// to: Battlefield }` so every ETB trigger matcher (Elvish Vanguard, Soul
    /// Warden, Panharmonicon, etc.) fires automatically for tokens without
    /// bespoke per-matcher code paths.
    #[test]
    fn emits_zone_changed_from_none_to_battlefield() {
        let (_, events) = resolve_token("w_1_1_soldier");

        let zc = events
            .iter()
            .find(|e| {
                matches!(
                    e,
                    GameEvent::ZoneChanged {
                        to: Zone::Battlefield,
                        ..
                    }
                )
            })
            .expect("token creation must emit ZoneChanged to Battlefield");

        let GameEvent::ZoneChanged { from, record, .. } = zc else {
            unreachable!();
        };
        assert_eq!(
            *from, None,
            "token creation has no prior zone (CR 111.1 + CR 603.6a)"
        );
        assert_eq!(record.from_zone, None);
        assert_eq!(record.to_zone, Zone::Battlefield);
        assert!(record.is_token, "record should reflect token identity");
    }

    #[test]
    fn emits_effect_resolved_event() {
        let (_, events) = resolve_token("w_1_1_soldier");

        assert!(events.iter().any(|e| matches!(
            e,
            GameEvent::EffectResolved {
                kind: EffectKind::Token,
                ..
            }
        )));
    }

    #[test]
    fn creates_multiple_tokens_with_count() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "w_1_1_soldier".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Two soldiers should be on the battlefield
        assert_eq!(state.battlefield.len(), 2);
        for &obj_id in &state.battlefield {
            let obj = &state.objects[&obj_id];
            assert_eq!(obj.name, "Soldier");
            assert_eq!(obj.power, Some(1));
            assert_eq!(obj.toughness, Some(1));
            assert_eq!(obj.card_id, CardId(0));
        }

        // Two TokenCreated events + one EffectResolved
        let token_events: Vec<_> = events
            .iter()
            .filter(|e| matches!(e, GameEvent::TokenCreated { .. }))
            .collect();
        assert_eq!(token_events.len(), 2);
    }

    #[test]
    fn explicit_artifact_token_uses_typed_fields() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "Treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let obj = &state.objects[&state.battlefield[0]];
        assert_eq!(obj.name, "Treasure");
        assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
        assert!(obj.card_types.subtypes.contains(&"Treasure".to_string()));
        assert_eq!(obj.power, None);
        assert_eq!(obj.toughness, None);
    }

    #[test]
    fn explicit_token_can_enter_tapped() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "Powerstone".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Powerstone".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: true,
                count: QuantityExpr::Fixed { value: 1 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&state.battlefield[0]].tapped);
    }

    #[test]
    fn duration_until_end_of_combat_creates_sacrifice_triggers() {
        use crate::types::ability::DelayedTriggerCondition;
        use crate::types::phase::Phase;

        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "r_1_1_warrior".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec![],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
        .duration(Duration::UntilEndOfCombat);

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        // Two tokens → two delayed sacrifice triggers
        assert_eq!(state.delayed_triggers.len(), 2);
        for trigger in &state.delayed_triggers {
            assert_eq!(
                trigger.condition,
                DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::EndCombat
                }
            );
            assert!(trigger.one_shot);
            assert_eq!(trigger.controller, PlayerId(0));
        }

        // Each trigger targets a distinct token
        let target_ids: Vec<_> = state
            .delayed_triggers
            .iter()
            .filter_map(|t| t.ability.targets.first().cloned())
            .collect();
        assert_eq!(target_ids.len(), 2);
        assert_ne!(target_ids[0], target_ids[1]);
    }

    #[test]
    fn parent_target_controller_owns_created_tokens() {
        let mut state = GameState::new_two_player(42);
        let target_id = zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Target Permanent".to_string(),
            Zone::Battlefield,
        );
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "Map".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Map".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::ParentTargetController,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(target_id)],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let created: Vec<_> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.is_token)
            .collect();
        assert_eq!(created.len(), 2);
        assert!(created
            .iter()
            .all(|object| object.controller == PlayerId(1)));
        assert!(created.iter().all(|object| object.owner == PlayerId(1)));
    }

    // ── Predefined token abilities ────────────────────────────────────

    #[test]
    fn predefined_treasure_has_mana_ability() {
        let abilities = predefined_token_abilities("Treasure");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::Mana { .. }));
        assert!(matches!(
            abilities[0].cost,
            Some(AbilityCost::Composite { .. })
        ));
    }

    /// CR 111.10u: the Lander arm of `predefined_token_abilities` must yield
    /// exactly one activated ability with the `{2}, {T}, Sacrifice` cost and a
    /// basic-land library search. Discriminating: fails on the unpatched
    /// `_ => vec![]` fallback.
    #[test]
    fn predefined_lander_has_search_land_ability() {
        let abilities = predefined_token_abilities("Lander");
        assert_eq!(abilities.len(), 1);
        assert_eq!(abilities[0].kind, AbilityKind::Activated);

        match &*abilities[0].effect {
            Effect::SearchLibrary { filter, .. } => match filter {
                TargetFilter::Typed(tf) => {
                    assert!(tf.type_filters.contains(&TypeFilter::Land));
                    assert!(tf.properties.iter().any(|p| matches!(
                        p,
                        FilterProp::HasSupertype {
                            value: Supertype::Basic
                        }
                    )));
                }
                other => panic!("Lander search filter must be Typed, got {other:?}"),
            },
            other => panic!("Lander effect must be SearchLibrary, got {other:?}"),
        }

        // Chain: SearchLibrary -> ChangeZone(enter_tapped) -> Shuffle.
        let put = abilities[0]
            .sub_ability
            .as_ref()
            .expect("Lander search chains to a ChangeZone step");
        assert!(matches!(
            *put.effect,
            Effect::ChangeZone {
                enter_tapped: crate::types::zones::EtbTapState::Tapped,
                ..
            }
        ));
        let shuffle = put
            .sub_ability
            .as_ref()
            .expect("Lander ChangeZone chains to a Shuffle step");
        assert!(matches!(*shuffle.effect, Effect::Shuffle { .. }));

        match abilities[0].cost.as_ref().expect("Lander needs a cost") {
            AbilityCost::Composite { costs } => {
                assert!(costs.iter().any(|c| matches!(
                    c,
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { generic: 2, .. }
                    }
                )));
                assert!(costs.iter().any(|c| matches!(c, AbilityCost::Tap)));
                assert!(costs.iter().any(|c| {
                    if let AbilityCost::Sacrifice(cost) = c {
                        matches!(cost.target, TargetFilter::SelfRef)
                            && cost.requirement
                                == crate::types::ability::SacrificeRequirement::count(1)
                    } else {
                        false
                    }
                }));
            }
            other => panic!("Lander cost must be Composite, got {other:?}"),
        }
    }

    /// CR 111.10 (Fallout): Junk chains exile-top to a PlayFromExile grant.
    #[test]
    fn predefined_junk_has_exile_top_and_play_permission_chain() {
        let abilities = predefined_token_abilities("Junk");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(
            *abilities[0].effect,
            Effect::ExileTop {
                face_down: false,
                ..
            }
        ));
        let grant = abilities[0]
            .sub_ability
            .as_ref()
            .expect("Junk chains to PlayFromExile grant");
        assert!(matches!(
            *grant.effect,
            Effect::GrantCastingPermission { .. }
        ));
        assert!(abilities[0]
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
    }

    #[test]
    fn predefined_shard_has_scry_then_draw() {
        let abilities = predefined_token_abilities("Shard");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::Scry { .. }));
        assert!(matches!(
            *abilities[0]
                .sub_ability
                .as_ref()
                .expect("Shard chains to Draw")
                .effect,
            Effect::Draw { .. }
        ));
    }

    #[test]
    fn predefined_incubator_has_transform_cost() {
        let abilities = predefined_token_abilities("Incubator");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(
            *abilities[0].effect,
            Effect::Transform {
                target: TargetFilter::SelfRef
            }
        ));
        assert!(matches!(
            abilities[0].cost.as_ref(),
            Some(AbilityCost::Mana {
                cost: ManaCost::Cost { generic: 2, .. }
            })
        ));
    }

    #[test]
    fn predefined_incubator_back_face_is_artifact_creature() {
        let back_face = incubator_phyrexian_back_face();
        assert_eq!(back_face.name, "Phyrexian Token");
        assert_eq!(back_face.power, Some(0));
        assert_eq!(back_face.toughness, Some(0));
        assert!(back_face.color.is_empty());
        assert!(back_face
            .card_types
            .core_types
            .contains(&CoreType::Artifact));
        assert!(back_face
            .card_types
            .core_types
            .contains(&CoreType::Creature));
        assert!(back_face
            .card_types
            .subtypes
            .iter()
            .any(|subtype| subtype == "Phyrexian"));
    }

    #[test]
    fn junk_token_injection_attaches_ability_and_rules_text() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Junk".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes.push("Junk".to_string());
            obj.is_token = true;
        }
        inject_predefined_token_abilities(&mut state, obj_id);
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.abilities.len(), 1);
        assert!(obj
            .token_rules_text
            .as_ref()
            .is_some_and(|t| t.contains("Exile")));
    }

    #[test]
    fn junk_ability_runtime_exiles_top_card_and_grants_play_permission() {
        let mut state = GameState::new_two_player(42);
        let junk = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Junk".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&junk).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes.push("Junk".to_string());
            obj.is_token = true;
        }
        inject_predefined_token_abilities(&mut state, junk);

        let top = create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Top Card".to_string(),
            Zone::Library,
        );
        let ability_def = state.objects[&junk].abilities[0].clone();
        let resolved = build_resolved_from_def(&ability_def, junk, PlayerId(0));
        let mut events = Vec::new();

        super::super::resolve_ability_chain(&mut state, &resolved, &mut events, 0)
            .expect("Junk ability chain should resolve");

        let top_obj = &state.objects[&top];
        assert_eq!(top_obj.zone, Zone::Exile);
        assert!(top_obj
            .casting_permissions
            .iter()
            .any(|permission| matches!(
                permission,
                CastingPermission::PlayFromExile {
                    duration: Duration::UntilEndOfTurn,
                    granted_to,
                    ..
                } if *granted_to == PlayerId(0)
            )));
    }

    /// CR 111.10u: the Lander rules-text arm must be present and describe the
    /// search. Discriminating: fails if Step C's text arm drifts or is removed.
    #[test]
    fn predefined_lander_rules_text_present() {
        let text =
            predefined_token_rules_text("Lander").expect("Lander must expose printed rules text");
        assert!(text.contains("basic land"));
        assert!(text.contains("tapped"));
        assert!(predefined_token_rules_text("Treasure").is_none());
    }

    /// CR 111.10u: a Lander token created via the runtime injection path must
    /// carry the activated ability AND the printed rules text. Discriminating:
    /// on revert the token has zero abilities and `token_rules_text` is `None`.
    #[test]
    fn lander_token_created_with_ability_and_rules_text() {
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Lander".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes.push("Lander".to_string());
            obj.is_token = true;
        }

        inject_predefined_token_abilities(&mut state, obj_id);

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.abilities.len(), 1);
        assert_eq!(obj.abilities[0].kind, AbilityKind::Activated);
        assert_eq!(obj.base_abilities.len(), 1);
        let rules_text = obj
            .token_rules_text
            .as_ref()
            .expect("Lander token must carry printed rules text");
        assert!(rules_text.contains("basic land"));
    }

    /// CR 111.10u + CR 614.1: full pipeline — activating the Lander ability
    /// must search the library, put a basic land onto the battlefield tapped,
    /// and sacrifice the Lander token. Discriminating: impossible to pass
    /// without the `"Lander"` ability arm.
    #[test]
    fn lander_search_chain_resolves_basic_land_tapped() {
        let mut state = GameState::new_two_player(42);

        // A Lander token on the battlefield with its injected ability.
        let lander = create_object(
            &mut state,
            crate::types::identifiers::CardId(1),
            PlayerId(0),
            "Lander".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&lander).unwrap();
            obj.card_types.core_types = vec![CoreType::Artifact];
            obj.card_types.subtypes.push("Lander".to_string());
            obj.is_token = true;
        }
        inject_predefined_token_abilities(&mut state, lander);

        // A basic land in the controller's library to be found.
        let forest = create_object(
            &mut state,
            crate::types::identifiers::CardId(2),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Library,
        );
        {
            let obj = state.objects.get_mut(&forest).unwrap();
            obj.card_types.core_types = vec![CoreType::Land];
            obj.card_types.supertypes.push(Supertype::Basic);
        }

        // Resolve the Lander ability's effect chain directly (isolating the
        // search/ChangeZone/Shuffle resolution from cost payment).
        let ability_def = state.objects[&lander].abilities[0].clone();
        let resolved = build_resolved_from_def(&ability_def, lander, PlayerId(0));
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &resolved, &mut events, 0)
            .expect("Lander search chain should resolve");

        assert!(
            matches!(state.waiting_for, WaitingFor::SearchChoice { .. }),
            "Lander search must prompt a library card choice"
        );

        crate::game::engine::apply(
            &mut state,
            PlayerId(0),
            GameAction::SelectCards {
                cards: vec![forest],
            },
        )
        .expect("selecting the basic land should resolve the search");

        assert_eq!(
            state.objects[&forest].zone,
            Zone::Battlefield,
            "the searched basic land must enter the battlefield"
        );
        assert!(
            state.objects[&forest].tapped,
            "CR 614.1: the searched land must enter tapped"
        );
    }

    #[test]
    fn predefined_food_has_gain_life_ability() {
        let abilities = predefined_token_abilities("Food");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::GainLife { .. }));
    }

    #[test]
    fn predefined_clue_has_draw_ability() {
        let abilities = predefined_token_abilities("Clue");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::Draw { .. }));
    }

    #[test]
    fn predefined_blood_has_draw_ability() {
        let abilities = predefined_token_abilities("Blood");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::Draw { .. }));
    }

    #[test]
    fn predefined_powerstone_has_colorless_mana() {
        let abilities = predefined_token_abilities("Powerstone");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(
            *abilities[0].effect,
            Effect::Mana {
                ref restrictions,
                ..
            } if matches!(
                restrictions.as_slice(),
                [crate::types::ability::ManaSpendRestriction::SpellTypeOrAbilityActivation {
                    spell_type,
                    ability: crate::types::mana::AbilityActivationScope::Any,
                }] if spell_type == "Artifact"
            )
        ));
    }

    #[test]
    fn predefined_map_has_targeted_explore_ability() {
        let abilities = predefined_token_abilities("Map");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(
            *abilities[0].effect,
            Effect::TargetOnly {
                target: TargetFilter::Typed(ref tf)
            } if tf.type_filters.contains(&crate::types::ability::TypeFilter::Creature)
        ));
        assert!(matches!(
            *abilities[0]
                .sub_ability
                .as_ref()
                .expect("map should chain to explore")
                .effect,
            Effect::Explore
        ));
        assert_eq!(
            abilities[0].activation_restrictions,
            vec![ActivationRestriction::AsSorcery]
        );
        match abilities[0].cost.as_ref().expect("map needs a cost") {
            AbilityCost::Composite { costs } => {
                assert!(costs.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { generic: 1, .. }
                    }
                )));
                assert!(costs.iter().any(|cost| matches!(cost, AbilityCost::Tap)));
                assert!(costs.iter().any(|cost| {
                    if let AbilityCost::Sacrifice(sc) = cost {
                        matches!(sc.target, TargetFilter::SelfRef)
                            && sc.requirement
                                == crate::types::ability::SacrificeRequirement::count(1)
                    } else {
                        false
                    }
                }));
            }
            other => panic!("expected composite cost, got {other:?}"),
        }
    }

    #[test]
    fn predefined_mutagen_has_counter_ability() {
        // CR 111.10v: Mutagen — "{1}, {T}, Sacrifice this token: Put a +1/+1
        // counter on target creature. Activate only as a sorcery." (#660)
        let abilities = predefined_token_abilities("Mutagen");
        assert_eq!(abilities.len(), 1);
        match &*abilities[0].effect {
            Effect::PutCounter {
                counter_type,
                count,
                target: TargetFilter::Typed(tf),
            } => {
                assert_eq!(*counter_type, CounterType::Plus1Plus1);
                assert_eq!(*count, QuantityExpr::Fixed { value: 1 });
                assert!(
                    tf.type_filters
                        .contains(&crate::types::ability::TypeFilter::Creature),
                    "must target a creature"
                );
                assert!(
                    tf.controller.is_none(),
                    "Mutagen targets ANY creature, not just controller's"
                );
            }
            other => panic!("expected PutCounter on target creature, got {other:?}"),
        }
        assert_eq!(
            abilities[0].activation_restrictions,
            vec![ActivationRestriction::AsSorcery]
        );
        match abilities[0].cost.as_ref().expect("mutagen needs a cost") {
            AbilityCost::Composite { costs } => {
                assert!(costs.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Mana {
                        cost: ManaCost::Cost { generic: 1, .. }
                    }
                )));
                assert!(costs.iter().any(|cost| matches!(cost, AbilityCost::Tap)));
                assert!(costs.iter().any(|cost| {
                    if let AbilityCost::Sacrifice(sc) = cost {
                        matches!(sc.target, TargetFilter::SelfRef)
                            && sc.requirement
                                == crate::types::ability::SacrificeRequirement::count(1)
                    } else {
                        false
                    }
                }));
            }
            other => panic!("expected composite cost, got {other:?}"),
        }
    }

    #[test]
    fn predefined_spawn_has_colorless_sacrifice_mana_ability() {
        // CR 106.1 + CR 701.21a: Eldrazi Spawn tokens produced by Writhing
        // Chrysalis, Awakening Zone, etc. share a single sacrifice-for-{C}
        // mana ability, injected by subtype.
        let abilities = predefined_token_abilities("Spawn");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::Mana { .. }));
        assert!({
            if let Some(AbilityCost::Sacrifice(sc)) = &abilities[0].cost {
                matches!(sc.target, TargetFilter::SelfRef)
                    && sc.requirement == crate::types::ability::SacrificeRequirement::count(1)
            } else {
                false
            }
        });
    }

    #[test]
    fn focused_writhing_chrysalis_spawn_token_sacrifice_adds_mana_and_triggers_counter() {
        let parsed = crate::parser::parse_oracle_text(
            "Devoid (This card has no color.)\n\
             When you cast this spell, create two 0/1 colorless Eldrazi Spawn creature tokens with \"Sacrifice this token: Add {C}.\"\n\
             Reach\n\
             Whenever you sacrifice another Eldrazi, put a +1/+1 counter on this creature.",
            "Writhing Chrysalis",
            &["devoid".to_string(), "reach".to_string()],
            &["Creature".to_string()],
            &["Eldrazi".to_string(), "Drone".to_string()],
        );

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let chrysalis = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Writhing Chrysalis".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&chrysalis).unwrap();
            obj.card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Eldrazi".to_string(), "Drone".to_string()],
            };
            obj.power = Some(2);
            obj.toughness = Some(3);
            obj.trigger_definitions = parsed.triggers.clone().into();
            Arc::make_mut(&mut obj.base_trigger_definitions).extend(parsed.triggers.clone());
        }

        // Focused runtime coverage: start from the parsed cast-trigger execute
        // ability so this test isolates token resolution, injected token mana
        // abilities, mana-ability cost payment, and sacrifice-trigger handling.
        // Full casting would add unrelated hand/mana/priority setup.
        let create_spawn = parsed.triggers[0]
            .execute
            .as_ref()
            .expect("Writhing Chrysalis cast trigger creates Spawn tokens");
        let ability = build_resolved_from_def(create_spawn, chrysalis, PlayerId(0));
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("Spawn token creation should resolve");

        let spawn = state
            .battlefield
            .iter()
            .copied()
            .find(|id| {
                let object = &state.objects[id];
                object.is_token
                    && object
                        .card_types
                        .subtypes
                        .iter()
                        .any(|subtype| subtype == "Spawn")
            })
            .expect("Writhing Chrysalis should create an Eldrazi Spawn token");

        assert!(
            matches!(
                *state.objects[&spawn].abilities[0].effect,
                Effect::Mana {
                    produced: ManaProduction::Colorless { .. },
                    ..
                }
            ),
            "Spawn token must have the runtime sacrifice-for-colorless mana ability"
        );

        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: spawn,
                ability_index: 0,
            },
        )
        .expect("Spawn mana ability should activate");

        assert_eq!(
            state.players[0].mana_pool.count_color(ManaType::Colorless),
            1,
            "Spawn sacrifice ability should add {{C}}"
        );
        assert!(!state.battlefield.contains(&spawn));
        assert!(
            state.stack.iter().any(|entry| entry.source_id == chrysalis),
            "Writhing Chrysalis should see another Eldrazi sacrificed"
        );

        apply_as_current(&mut state, GameAction::PassPriority).expect("active player passes");
        apply_as_current(&mut state, GameAction::PassPriority).expect("opponent passes");

        assert_eq!(
            state.objects[&chrysalis]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
                .unwrap_or(0),
            1,
            "Writhing Chrysalis sacrifice trigger should resolve to a +1/+1 counter"
        );
    }

    #[test]
    fn catalog_pest_preset_grants_attack_life_trigger() {
        let preset = crate::game::token_presets::known_token_preset_by_id(
            "00a0801d-0212-5890-8957-3cde30f382f9",
        )
        .expect("SOS Pest preset");

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Pest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.is_token = true;
            obj.token_image_ref = preset.token_image_ref.clone();
        }
        inject_catalog_token_abilities(&mut state, obj_id);
        let obj = &state.objects[&obj_id];
        assert_eq!(
            obj.trigger_definitions.len(),
            1,
            "catalog rules_text must install the attacks life trigger intrinsically"
        );
        assert_eq!(
            obj.trigger_definitions[0].definition.mode,
            TriggerMode::Attacks
        );
        assert!(
            !obj.trigger_definitions
                .iter_all()
                .any(|trigger| trigger.definition.mode == TriggerMode::ChangesZone),
            "SOS Pest must keep its printed attack trigger, not the older Pest dies trigger"
        );
        assert_eq!(
            obj.token_rules_text.as_deref(),
            Some("Whenever this token attacks, you gain 1 life.")
        );
    }

    #[test]
    fn catalog_pest_dies_trigger_uses_battlefield_lki_zone() {
        let preset = crate::game::token_presets::known_token_preset_by_id(
            "14c28cbd-1740-5c17-98ea-4aea094067f1",
        )
        .expect("BLC Pest preset");

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Pest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.is_token = true;
            obj.token_image_ref = preset.token_image_ref.clone();
        }
        inject_catalog_token_abilities(&mut state, obj_id);

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.trigger_definitions.len(), 1);
        let trigger = &obj.trigger_definitions[0];
        assert_eq!(trigger.definition.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.definition.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.definition.destination, Some(Zone::Graveyard));
        assert_eq!(
            trigger.definition.trigger_zones,
            vec![Zone::Battlefield],
            "CR 603.10a LKI scans a dying token as a Battlefield source"
        );
    }

    #[test]
    fn catalog_pest_dies_trigger_fires_through_zone_pipeline() {
        use crate::game::triggers::process_triggers;
        use crate::game::zone_pipeline::{move_object, ZoneMoveRequest, ZoneMoveResult};

        let preset = crate::game::token_presets::known_token_preset_by_id(
            "14c28cbd-1740-5c17-98ea-4aea094067f1",
        )
        .expect("BLC Pest preset");

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Pest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.is_token = true;
            obj.token_image_ref = preset.token_image_ref.clone();
        }
        inject_catalog_token_abilities(&mut state, obj_id);

        let mut events = Vec::new();
        let result = move_object(
            &mut state,
            ZoneMoveRequest::effect(obj_id, Zone::Graveyard, obj_id),
            &mut events,
        );
        assert!(matches!(result, ZoneMoveResult::Done));
        process_triggers(&mut state, &events);

        assert_eq!(
            state.stack.len(),
            1,
            "the Pest's own dies trigger must fire from CR 603.10a LKI"
        );
    }

    #[test]
    fn catalog_pest_dies_trigger_fires_after_lethal_damage_sba() {
        use crate::game::sba::check_state_based_actions;
        use crate::game::triggers::process_triggers;

        let preset = crate::game::token_presets::known_token_preset_by_id(
            "14c28cbd-1740-5c17-98ea-4aea094067f1",
        )
        .expect("BLC Pest preset");

        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Pest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.is_token = true;
            obj.token_image_ref = preset.token_image_ref.clone();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        inject_catalog_token_abilities(&mut state, obj_id);

        state.objects.get_mut(&obj_id).unwrap().damage_marked = 1;
        let mut events = Vec::new();
        check_state_based_actions(&mut state, &mut events);
        process_triggers(&mut state, &events);

        assert!(
            !state.objects.contains_key(&obj_id),
            "token destroyed by lethal damage must cease to exist after moving zones"
        );
        assert_eq!(
            state.stack.len(),
            1,
            "the Pest's dies trigger must fire when lethal damage SBAs move it to the graveyard"
        );
        let lki_token_ref = state
            .lki_cache
            .get(&obj_id)
            .and_then(|lki| lki.token_image_ref.as_ref())
            .expect("LKI must preserve the token image ref for dead-token stack display");
        assert_eq!(
            Some(lki_token_ref.preset_id.as_str()),
            preset
                .token_image_ref
                .as_ref()
                .map(|image| image.preset_id.as_str())
        );
    }

    #[test]
    fn catalog_pest_dies_trigger_fires_after_tragic_slip_zero_toughness() {
        use crate::game::scenario::{GameScenario, P0};
        use crate::types::events::GameEvent;

        let preset = crate::game::token_presets::known_token_preset_by_id(
            "14c28cbd-1740-5c17-98ea-4aea094067f1",
        )
        .expect("BLC Pest preset");

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_life(P0, 20);
        let slip = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Tragic Slip",
                true,
                "Target creature gets -1/-1 until end of turn.",
            )
            .id();
        let mut runner = scenario.build();
        let pest = create_object(
            runner.state_mut(),
            CardId(0),
            P0,
            "Pest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = runner.state_mut().objects.get_mut(&pest).unwrap();
            obj.is_token = true;
            obj.token_image_ref = preset.token_image_ref.clone();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(1);
            obj.toughness = Some(1);
        }
        inject_catalog_token_abilities(runner.state_mut(), pest);

        let outcome = runner.cast(slip).target_object(pest).resolve();

        assert!(
            outcome.events().iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == pest
            )),
            "Tragic Slip's -1/-1 must create a zero-toughness battlefield-to-graveyard event"
        );
        assert!(
            !outcome.state().objects.contains_key(&pest),
            "zero-toughness Pest token must cease to exist"
        );
        assert!(
            outcome.state().stack.len() == 1 || outcome.life_delta(P0) == 1,
            "the Pest dies trigger must either remain on the stack or resolve to gain 1 life"
        );
    }

    #[test]
    fn pest_infestation_linked_create_token_grants_catalog_dies_trigger() {
        use crate::types::proposed_event::TokenCharacteristics;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Pest Infestation".to_string(),
            Zone::Battlefield,
        );
        let source_obj = state.objects.get_mut(&source).unwrap();
        source_obj.printed_ref = Some(crate::types::card::PrintedCardRef {
            oracle_id: "1b704798-0c69-4c18-ac7e-42933ce90028".to_string(),
            face_name: "Pest Infestation".to_string(),
        });
        source_obj.source_related_token_ids.extend(
            [
                "5d96727f-b037-5af6-a854-b39b4bc4b5ea",
                "be7c7de8-06e4-5ea4-8faf-18881dbcee45",
                "fda6f4a3-6734-5347-8712-e449ed76e0a8",
            ]
            .into_iter()
            .map(str::to_string),
        );

        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Pest".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Pest".to_string()],
                supertypes: vec![],
                colors: vec![ManaColor::Black, ManaColor::Green],
                keywords: vec![],
            },
            script_name: "Pest".to_string(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: std::collections::HashSet::new(),
        };
        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let pest_id = state.last_created_token_ids[0];
        let obj = &state.objects[&pest_id];
        assert_eq!(
            obj.token_image_ref
                .as_ref()
                .map(|image| image.preset_id.as_str()),
            Some("5d96727f-b037-5af6-a854-b39b4bc4b5ea"),
            "Pest Infestation's multiple equivalent source-linked token ids must resolve to the first matching Pest preset, not fall back to an unrelated Pest"
        );
        assert_eq!(obj.trigger_definitions.len(), 1);
        let trigger = &obj.trigger_definitions[0];
        assert_eq!(trigger.definition.mode, TriggerMode::ChangesZone);
        assert_eq!(trigger.definition.origin, Some(Zone::Battlefield));
        assert_eq!(trigger.definition.destination, Some(Zone::Graveyard));
        let execute = trigger
            .definition
            .execute
            .as_ref()
            .expect("Pest dies trigger effect");
        assert!(matches!(
            *execute.effect,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 1 },
                player: TargetFilter::Controller,
            }
        ));
    }

    #[test]
    fn predefined_treasure_create_token_pipeline_has_exactly_one_mana_ability() {
        use crate::types::proposed_event::TokenCharacteristics;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Rapacious Dragon".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .source_related_token_ids
            .push("0060ce13-67e2-5607-a29b-721c743e6770".to_string());
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Treasure".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Treasure".to_string(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };
        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let treasure_id = state.last_created_token_ids[0];
        let obj = &state.objects[&treasure_id];
        assert!(
            obj.token_image_ref.is_some(),
            "Treasure creation must resolve a catalog preset image ref"
        );
        assert_eq!(
            obj.abilities.len(),
            1,
            "predefined Treasure must carry exactly one sacrifice-for-mana ability"
        );
        assert!(matches!(*obj.abilities[0].effect, Effect::Mana { .. }));
        assert!(
            obj.trigger_definitions.is_empty(),
            "catalog injection must not double-grant predefined Treasure triggers"
        );
    }

    #[test]
    fn predefined_royal_role_create_token_pipeline_has_exactly_one_role_static() {
        use crate::types::proposed_event::TokenCharacteristics;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Royal Treatment".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .source_related_token_ids
            .push("48b5010a-9c00-5cc1-b5e1-f407670846ba".to_string());
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Royal".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Aura".to_string(), "Role".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Royal".to_string(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };
        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let role_id = state.last_created_token_ids[0];
        let obj = &state.objects[&role_id];
        assert!(
            obj.token_image_ref.is_some(),
            "Royal Role creation must resolve a catalog preset image ref"
        );
        assert_eq!(
            obj.static_definitions.len(),
            1,
            "predefined Royal Role must carry exactly one enchanted-creature static"
        );
        assert_eq!(
            obj.base_static_definitions.len(),
            1,
            "base_static_definitions must mirror the single role static"
        );
        assert!(
            obj.abilities.is_empty(),
            "Royal Role has no activated abilities from the predefined path"
        );
        assert!(
            obj.trigger_definitions.is_empty(),
            "catalog injection must not double-grant predefined Royal Role statics"
        );
    }

    /// CR 111.10k: A Role token created by a real card ("Create a Monster Role
    /// token …") is named "Monster Role" — the parser's `known_role_token_identity`
    /// produces the full "<Role> Role" name, which is the printed token name. The
    /// predefined-ability dispatch MUST recognize that full name; matching only the
    /// bare "Monster" left the token with no +1/+1-and-trample static. Regression
    /// for Role tokens (Monstrous Rage, Royal Treatment, …) granting nothing.
    #[test]
    fn predefined_monster_role_full_name_grants_role_static() {
        use crate::types::proposed_event::TokenCharacteristics;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Monstrous Rage".to_string(),
            Zone::Battlefield,
        );
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                // The name the parser actually produces for "a Monster Role token".
                display_name: "Monster Role".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Aura".to_string(), "Role".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Monster Role".to_string(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };
        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let role_id = state.last_created_token_ids[0];
        let obj = &state.objects[&role_id];
        assert_eq!(
            obj.static_definitions.len(),
            1,
            "Monster Role must carry its enchanted-creature +1/+1-and-trample static"
        );
    }

    /// CR 111.3: A Role token is one face of a two-Role DFC ("Monster // Sorcerer"),
    /// so its source card links to BOTH face presets — the single-preset fast path
    /// in `find_exact_token_ref` is skipped and art resolves via body match. The
    /// token is named "Monster Role" but the face preset is "Monster", so the name
    /// comparison must reconcile the trailing " Role"; otherwise the token gets no
    /// image ref and renders with no art (reported for Monstrous Rage). The match
    /// must also select the correct face (Monster, not Sorcerer).
    #[test]
    fn dfc_monster_role_resolves_the_monster_face_art() {
        use crate::types::card::PrintedCardRef;
        use crate::types::proposed_event::TokenCharacteristics;
        use std::collections::HashSet;

        // Monster face of the "Monster // Sorcerer" DFC (WOE), and the Sorcerer face.
        const MONSTER_PRESET: &str = "246f948c-eea9-5f6a-8d19-f8c11c51de94";
        const SORCERER_PRESET: &str = "dd6f5274-9bb4-5acc-9855-55815f497831";

        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(99),
            PlayerId(0),
            "Monstrous Rage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&source).unwrap();
            obj.printed_ref = Some(PrintedCardRef {
                oracle_id: "646a2371-54c0-4492-ac2f-20f109d6108c".to_string(),
                face_name: "Monstrous Rage".to_string(),
            });
            obj.source_related_token_ids =
                vec![MONSTER_PRESET.to_string(), SORCERER_PRESET.to_string()];
        }
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Monster Role".to_string(),
                power: None,
                toughness: None,
                core_types: vec![CoreType::Enchantment],
                subtypes: vec!["Aura".to_string(), "Role".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Monster Role".to_string(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: source,
            controller: PlayerId(0),
            attach_to: None,
        };
        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };
        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let role_id = state.last_created_token_ids[0];
        let image_ref = state.objects[&role_id]
            .token_image_ref
            .clone()
            .expect("DFC Monster Role must resolve an image ref, not render artless");
        assert_eq!(
            image_ref.preset_id, MONSTER_PRESET,
            "must resolve the Monster face preset, not the Sorcerer face"
        );
    }

    #[test]
    fn non_predefined_token_gets_no_abilities() {
        let abilities = predefined_token_abilities("Soldier");
        assert!(abilities.is_empty());
    }

    // ── Role token predefined statics (CR 111.10) ───────────────────────

    /// Test helper — most Role tests only need the statics half of the spec.
    /// Wraps the typical "fetch spec, drop triggers, assert statics" idiom
    /// so per-Role tests stay focused on shape assertions.
    fn predefined_role_token_spec_statics(name: &str) -> Option<Vec<StaticDefinition>> {
        predefined_role_token_spec(name).map(|spec| spec.statics)
    }

    #[test]
    fn predefined_royal_role_has_pump_and_ward() {
        // CR 111.10m: Royal Role — "Enchanted creature gets +1/+1 and has ward {1}."
        let statics = predefined_role_token_spec_statics("Royal").unwrap();
        assert_eq!(statics.len(), 1);
        let s = &statics[0];
        let Some(TargetFilter::Typed(tf)) = s.affected.as_ref() else {
            panic!("affected must be a TypedFilter");
        };
        assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        assert!(s
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(s
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        let ward = s.modifications.iter().find_map(|m| match m {
            ContinuousModification::AddKeyword {
                keyword: Keyword::Ward(cost),
            } => Some(cost),
            _ => None,
        });
        let Some(WardCost::Mana(ManaCost::Cost { generic, .. })) = ward else {
            panic!("Royal Role must grant ward, got {:?}", ward);
        };
        assert_eq!(*generic, 1);
    }

    #[test]
    fn predefined_cursed_role_sets_base_pt_one_one() {
        // CR 111.10j: Cursed Role — "Enchanted creature has base power and
        // toughness 1/1." `SetPower`/`SetToughness` apply at layer 7b
        // (set base P/T). Per CR 613.1, layer 7c modifiers (`AddPower`,
        // counters, +N/+N pumps) still stack on top — Cursed sets the
        // base, it does not pin the final P/T. The encoding must therefore
        // contain SetPower/SetToughness and must NOT contain AddPower/
        // AddToughness (those would conflate "base set" with "additive
        // modifier" and double-count when both apply).
        let statics = predefined_role_token_spec_statics("Cursed").unwrap();
        assert_eq!(statics.len(), 1);
        let s = &statics[0];
        let Some(TargetFilter::Typed(tf)) = s.affected.as_ref() else {
            panic!("affected must be a TypedFilter");
        };
        assert!(tf.properties.contains(&FilterProp::EnchantedBy));
        assert!(s
            .modifications
            .contains(&ContinuousModification::SetPower { value: 1 }));
        assert!(s
            .modifications
            .contains(&ContinuousModification::SetToughness { value: 1 }));
        // Cursed's encoding belongs in layer 7b only — emitting AddPower
        // alongside SetPower would apply +1 in 7c on top of the base set,
        // turning Cursed creatures into 2/2.
        assert!(!s.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddPower { .. } | ContinuousModification::AddToughness { .. }
        )));
    }

    #[test]
    fn predefined_monster_role_pumps_and_grants_trample() {
        // CR 111.10k: Monster Role — "Enchanted creature gets +1/+1 and has trample."
        let statics = predefined_role_token_spec_statics("Monster").unwrap();
        assert_eq!(statics.len(), 1);
        let s = &statics[0];
        assert!(s
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(s
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        assert!(s
            .modifications
            .contains(&ContinuousModification::AddKeyword {
                keyword: Keyword::Trample,
            }));
    }

    #[test]
    fn predefined_virtuous_role_dynamic_pump_per_enchantment() {
        // CR 111.10p: Virtuous Role — "Enchanted creature gets +1/+1 for each
        // enchantment you control." `ControllerRef::You` here is the Aura's
        // controller (CR 109.5), not the enchanted creature's controller.
        let statics = predefined_role_token_spec_statics("Virtuous").unwrap();
        assert_eq!(statics.len(), 1);
        let s = &statics[0];

        let extract_count_filter = |modifications: &[ContinuousModification]| -> TargetFilter {
            for m in modifications {
                if let ContinuousModification::AddDynamicPower {
                    value:
                        QuantityExpr::Ref {
                            qty: QuantityRef::ObjectCount { filter },
                        },
                } = m
                {
                    return filter.clone();
                }
            }
            panic!("expected AddDynamicPower {{ Ref(ObjectCount) }}");
        };
        let count_filter = extract_count_filter(&s.modifications);
        let TargetFilter::Typed(tf) = count_filter else {
            panic!("count filter must be Typed (enchantments you control)");
        };
        assert!(tf.type_filters.contains(&TypeFilter::Enchantment));
        assert_eq!(tf.controller, Some(ControllerRef::You));

        // Toughness mirror must be present — both layer-7c modifications
        // are required for "+1/+1 for each ...".
        assert!(s.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddDynamicToughness {
                value: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { .. }
                }
            }
        )));
    }

    #[test]
    fn predefined_young_hero_role_grants_attacks_trigger_with_intervening_if() {
        // CR 111.10r: Young Hero Role — granted attacks-trigger with
        // SelfToughness ≤ 3 intervening-if and a +1/+1 counter on self.
        let statics = predefined_role_token_spec_statics("Young Hero").unwrap();
        assert_eq!(statics.len(), 1);
        let s = &statics[0];

        let trigger = s
            .modifications
            .iter()
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            })
            .expect("Young Hero must grant a trigger");

        // Mode: Attacks. valid_card: None (matches when source itself attacks
        // — granted to enchanted creature, so source = enchanted creature).
        assert_eq!(trigger.mode, TriggerMode::Attacks);
        assert!(
            trigger.valid_card.is_none(),
            "valid_card must be None so trigger fires off the granted source \
             (enchanted creature), not via a separate filter"
        );

        // Intervening-if: source toughness ≤ 3.
        let condition = trigger.condition.as_ref().expect("condition required");
        let TriggerCondition::QuantityComparison {
            lhs,
            comparator,
            rhs,
        } = condition
        else {
            panic!("condition must be QuantityComparison, got {:?}", condition);
        };
        assert!(matches!(
            lhs,
            QuantityExpr::Ref {
                qty: QuantityRef::Toughness {
                    scope: crate::types::ability::ObjectScope::Source
                }
            }
        ));
        assert_eq!(*comparator, Comparator::LE);
        assert!(matches!(rhs, QuantityExpr::Fixed { value: 3 }));

        // Effect: PutCounter P1P1 ×1 on SelfRef.
        let exec = trigger.execute.as_ref().expect("execute required");
        let Effect::PutCounter {
            counter_type,
            count,
            target,
        } = &*exec.effect
        else {
            panic!("execute effect must be PutCounter, got {:?}", exec.effect);
        };
        assert_eq!(counter_type, &CounterType::Plus1Plus1);
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
        assert!(matches!(target, TargetFilter::SelfRef));
    }

    #[test]
    fn predefined_sorcerer_role_grants_attacks_scry_trigger() {
        // CR 111.10n: Sorcerer Role — +1/+1 plus a granted attacks-trigger
        // that scries 1. Unconditional (no intervening-if).
        let statics = predefined_role_token_spec_statics("Sorcerer").unwrap();
        assert_eq!(statics.len(), 1);
        let s = &statics[0];

        assert!(s
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(s
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));

        let trigger = s
            .modifications
            .iter()
            .find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger),
                _ => None,
            })
            .expect("Sorcerer must grant a trigger");
        assert_eq!(trigger.mode, TriggerMode::Attacks);
        assert!(
            trigger.condition.is_none(),
            "Sorcerer's attacks-scry is unconditional (no intervening-if)"
        );

        let exec = trigger.execute.as_ref().expect("execute required");
        let Effect::Scry { count, target } = &*exec.effect else {
            panic!("execute effect must be Scry, got {:?}", exec.effect);
        };
        assert!(matches!(count, QuantityExpr::Fixed { value: 1 }));
        assert!(matches!(target, TargetFilter::Controller));
    }

    #[test]
    fn predefined_wicked_role_has_pump_static_and_self_dies_trigger() {
        // CR 111.10q: Wicked Role — pump static on the enchanted creature
        // PLUS a self-dies trigger on the Aura that makes each opponent
        // lose 1 life. The trigger lives on the token itself (not granted),
        // and `player_scope: Opponent` on the inner ability iterates the
        // life loss per opponent.
        let spec = predefined_role_token_spec("Wicked").unwrap();
        assert_eq!(spec.statics.len(), 1, "Wicked has one pump static");
        assert_eq!(spec.triggers.len(), 1, "Wicked has one self-dies trigger");

        // Static: +1/+1 on enchanted creature, no keyword.
        let pump = &spec.statics[0];
        assert!(pump
            .modifications
            .contains(&ContinuousModification::AddPower { value: 1 }));
        assert!(pump
            .modifications
            .contains(&ContinuousModification::AddToughness { value: 1 }));
        assert!(
            !pump.modifications.iter().any(|m| matches!(
                m,
                ContinuousModification::AddKeyword { .. }
                    | ContinuousModification::GrantTrigger { .. }
            )),
            "Wicked's static is pure pump — no keyword or granted trigger"
        );

        // Trigger: ChangesZone Battlefield → Graveyard, valid_card = SelfRef.
        let t = &spec.triggers[0];
        assert_eq!(t.mode, TriggerMode::ChangesZone);
        assert_eq!(t.origin, Some(Zone::Battlefield));
        assert_eq!(t.destination, Some(Zone::Graveyard));
        assert_eq!(
            t.valid_card,
            Some(TargetFilter::SelfRef),
            "self-trigger must filter to the Aura itself"
        );
        assert_eq!(
            t.trigger_zones,
            vec![Zone::Battlefield],
            "trigger_zones must use Battlefield so CR 603.10a LKI can find \
             the token before it ceases to exist"
        );

        // Execute: per-opponent LoseLife 1.
        let exec = t.execute.as_ref().expect("execute required");
        assert_eq!(
            exec.player_scope,
            Some(PlayerFilter::Opponent),
            "per-opponent iteration must come from player_scope"
        );
        let Effect::LoseLife { amount, target } = &*exec.effect else {
            panic!("execute effect must be LoseLife, got {:?}", exec.effect);
        };
        assert!(matches!(amount, QuantityExpr::Fixed { value: 1 }));
        assert!(
            target.is_none(),
            "target must be None so each iteration's rebound controller takes the loss"
        );
    }

    #[test]
    fn all_seven_role_token_variants_are_implemented() {
        // CR 111.10: every named Role token must have a spec. Unknown
        // names still return None (the dispatch is exhaustive over Roles,
        // not a catch-all).
        for name in [
            "Cursed",
            "Monster",
            "Royal",
            "Sorcerer",
            "Virtuous",
            "Wicked",
            "Young Hero",
        ] {
            assert!(
                predefined_role_token_spec(name).is_some(),
                "{name} Role must be implemented (CR 111.10)"
            );
        }
        assert!(predefined_role_token_spec("Not A Role").is_none());
    }

    #[test]
    fn inject_adds_royal_role_static_to_token() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Royal".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types
                .subtypes
                .extend(["Aura".to_string(), "Role".to_string()]);
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.is_token = true;
        }

        inject_predefined_token_abilities(&mut state, obj_id);

        let obj = &state.objects[&obj_id];
        assert_eq!(
            obj.static_definitions.len(),
            1,
            "Royal Role must contribute exactly one static"
        );
        assert_eq!(
            obj.base_static_definitions.len(),
            1,
            "base_static_definitions must mirror live statics"
        );
        // Non-Role tokens with the same name must not receive Role statics.
        // Use a Treasure subtype so dispatch reaches the Role-name guard
        // (the early-out only triggers when both dispatch paths are empty);
        // Treasure injects activated abilities but no statics, so a non-zero
        // ability count + zero static count proves the Role guard rejected
        // dispatch on subtype rather than on the early-out path.
        let obj2 = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Royal".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj2).unwrap();
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
        }
        inject_predefined_token_abilities(&mut state, obj2);
        assert_eq!(
            state.objects[&obj2].static_definitions.len(),
            0,
            "A 'Royal'-named token without the Role subtype must not get Role statics"
        );
        assert!(
            !state.objects[&obj2].abilities.is_empty(),
            "Treasure subtype must still have injected its activated ability — \
             this proves dispatch reached the Role-name guard rather than the early-out"
        );
    }

    #[test]
    fn inject_adds_cursed_role_static_to_token() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        // CR 111.10j: Cursed Role full injection path.
        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(5),
            PlayerId(0),
            "Cursed".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types
                .subtypes
                .extend(["Aura".to_string(), "Role".to_string()]);
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.is_token = true;
        }
        inject_predefined_token_abilities(&mut state, obj_id);
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.static_definitions.len(), 1);
        assert_eq!(obj.base_static_definitions.len(), 1);
    }

    #[test]
    fn inject_adds_abilities_to_token() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.subtypes.push("Treasure".to_string());
            obj.is_token = true;
        }

        inject_predefined_token_abilities(&mut state, obj_id);

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.abilities.len(), 1);
        assert!(matches!(*obj.abilities[0].effect, Effect::Mana { .. }));
        assert_eq!(obj.base_abilities.len(), 1);
    }

    #[test]
    fn inject_adds_map_ability_to_map_token() {
        use crate::game::zones::create_object;
        use crate::types::identifiers::CardId;

        let mut state = GameState::new_two_player(42);
        let obj_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Map".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.subtypes.push("Map".to_string());
            obj.is_token = true;
        }

        inject_predefined_token_abilities(&mut state, obj_id);

        let obj = &state.objects[&obj_id];
        assert_eq!(obj.abilities.len(), 1);
        assert!(matches!(
            *obj.abilities[0].effect,
            Effect::TargetOnly { .. }
        ));
        assert!(matches!(
            *obj.abilities[0]
                .sub_ability
                .as_ref()
                .expect("map should chain to explore")
                .effect,
            Effect::Explore
        ));
    }

    #[test]
    fn apply_create_token_mirrors_static_abilities_to_base() {
        // Urza's Saga's chapter II creates a 0/0 Construct whose only saving
        // grace is "+1/+1 for each artifact you control". CR 613.1 resets
        // `static_definitions` from `base_static_definitions` at the start of
        // every layers pass — if the resolver only writes to live `*` and not
        // `base_*`, the boost is wiped before layer 7c reads it and the token
        // dies as a 0/0 to SBAs (CR 704.5f). Both must be populated.
        use crate::types::ability::{
            ContinuousModification, QuantityExpr, QuantityRef, StaticDefinition, TargetFilter,
            TypedFilter,
        };
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let boost = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![
                ContinuousModification::AddDynamicPower {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter::new(
                                crate::types::ability::TypeFilter::Artifact,
                            )),
                        },
                    },
                },
                ContinuousModification::AddDynamicToughness {
                    value: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(TypedFilter::new(
                                crate::types::ability::TypeFilter::Artifact,
                            )),
                        },
                    },
                },
            ]);

        use crate::types::proposed_event::TokenCharacteristics;
        let mut state = GameState::new_two_player(42);
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Construct".to_string(),
                power: Some(0),
                toughness: Some(0),
                core_types: vec![CoreType::Artifact, CoreType::Creature],
                subtypes: vec!["Construct".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Construct".to_string(),
            static_abilities: vec![boost],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(100),
            controller: PlayerId(0),
            attach_to: None,
        };

        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let id = state.last_created_token_ids[0];
        let obj = &state.objects[&id];
        assert_eq!(
            obj.static_definitions.len(),
            1,
            "live static_definitions must carry the boost"
        );
        assert_eq!(
            obj.base_static_definitions.len(),
            1,
            "base_static_definitions must mirror live so the layers reset (CR 613.1) preserves it"
        );
    }

    #[test]
    fn apply_create_token_materializes_intrinsic_equip_ability() {
        use crate::parser::oracle::try_parse_equip;
        use crate::types::ability::{ContinuousModification, StaticDefinition};
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let equip = try_parse_equip("Equip {0}").expect("equip static");
        let equip_static = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::GrantAbility {
                definition: Box::new(equip),
            }]);

        use crate::types::proposed_event::TokenCharacteristics;
        let mut state = GameState::new_two_player(42);
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Stoneforged Blade".to_string(),
                power: Some(0),
                toughness: Some(0),
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Stoneforged Blade".to_string(),
            static_abilities: vec![equip_static],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(100),
            controller: PlayerId(0),
            attach_to: None,
        };

        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let id = state.last_created_token_ids[0];
        let obj = &state.objects[&id];
        assert!(
            obj.abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::Attach { .. })),
            "intrinsic equip must materialize onto obj.abilities"
        );
        assert!(
            obj.base_abilities
                .iter()
                .any(|a| matches!(*a.effect, Effect::Attach { .. })),
            "intrinsic equip must mirror onto base_abilities"
        );
    }

    #[test]
    fn apply_create_token_does_not_materialize_conditional_grant_ability() {
        use crate::parser::oracle::try_parse_equip;
        use crate::types::ability::{ContinuousModification, StaticCondition, StaticDefinition};
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let equip = try_parse_equip("Equip {0}").expect("equip static");
        let conditional_equip = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .condition(StaticCondition::IsPresent { filter: None })
            .modifications(vec![ContinuousModification::GrantAbility {
                definition: Box::new(equip),
            }]);

        use crate::types::proposed_event::TokenCharacteristics;
        let mut state = GameState::new_two_player(42);
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Conditional Blade".to_string(),
                power: Some(0),
                toughness: Some(0),
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Equipment".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Conditional Blade".to_string(),
            static_abilities: vec![conditional_equip],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(101),
            controller: PlayerId(0),
            attach_to: None,
        };

        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let id = state.last_created_token_ids[0];
        let obj = &state.objects[&id];
        assert_eq!(
            obj.static_definitions.len(),
            1,
            "conditional grant must still live in static_definitions"
        );
        assert!(
            obj.abilities.is_empty(),
            "conditional GrantAbility must not leak into obj.abilities"
        );
        assert!(
            obj.base_abilities.is_empty(),
            "conditional GrantAbility must not leak into base_abilities"
        );
    }

    #[test]
    fn apply_create_token_does_not_materialize_non_equip_grant_ability() {
        use crate::types::ability::{
            AbilityDefinition, AbilityKind, ContinuousModification, StaticDefinition,
        };
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let tap_draw = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let grant_static = StaticDefinition::continuous()
            .affected(TargetFilter::SelfRef)
            .modifications(vec![ContinuousModification::GrantAbility {
                definition: Box::new(tap_draw),
            }]);

        use crate::types::proposed_event::TokenCharacteristics;
        let mut state = GameState::new_two_player(42);
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Meteorite".to_string(),
                power: Some(0),
                toughness: Some(0),
                core_types: vec![CoreType::Artifact],
                subtypes: vec![],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "Meteorite".to_string(),
            static_abilities: vec![grant_static],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(102),
            controller: PlayerId(0),
            attach_to: None,
        };

        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        let id = state.last_created_token_ids[0];
        let obj = &state.objects[&id];
        assert_eq!(obj.static_definitions.len(), 1);
        assert!(
            obj.abilities.is_empty(),
            "non-equip GrantAbility must stay layer-only"
        );
        assert!(obj.base_abilities.is_empty());
    }

    #[test]
    fn apply_create_token_populates_last_created_token_ids() {
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        assert!(state.last_created_token_ids.is_empty());

        use crate::types::proposed_event::TokenCharacteristics;
        let spec = TokenSpec {
            characteristics: TokenCharacteristics {
                display_name: "Hero".to_string(),
                power: Some(1),
                toughness: Some(1),
                core_types: vec![CoreType::Creature],
                subtypes: vec!["Hero".to_string()],
                supertypes: vec![],
                colors: vec![],
                keywords: vec![],
            },
            script_name: "c_1_1_hero".to_string(),
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(100),
            controller: PlayerId(0),
            attach_to: None,
        };

        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
            copy: None,
            enter_tapped: crate::types::proposed_event::EtbTapState::Unspecified,
            count: 1,
            applied: HashSet::new(),
        };

        let mut events = vec![];
        apply_create_token_after_replacement(&mut state, event, &mut events);

        assert_eq!(
            state.last_created_token_ids.len(),
            1,
            "should record exactly one created token"
        );
        // The created token should be on the battlefield
        assert!(state.objects.contains_key(&state.last_created_token_ids[0]));
    }

    #[test]
    fn paused_token_etb_counters_preserve_batch_ledger_and_effect_resolution() {
        use std::sync::Arc;

        use crate::types::ability::{QuantityModification, ReplacementDefinition, ReplacementMode};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Choice".to_string(),
            Zone::Battlefield,
        );
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent);
            def.mode = ReplacementMode::Optional { decline: None };
            let obj = state.objects.get_mut(&replacement_source).unwrap();
            obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
            obj.replacement_definitions = vec![def].into();
        }

        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "soldier".to_string(),
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                types: vec!["Creature".to_string(), "Soldier".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 2 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![(
                    CounterType::Plus1Plus1,
                    QuantityExpr::Fixed { value: 1 },
                )],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));

        let mut choice_events = Vec::new();
        for _ in 0..2 {
            let result =
                apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
            choice_events.extend(result.events);
        }

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(
            state.last_created_token_ids.len(),
            2,
            "paused ETB-counter choices must preserve every token created by the batch"
        );
        assert_eq!(
            choice_events
                .iter()
                .filter(|event| matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::Token,
                        source_id: ObjectId(100),
                        ..
                    }
                ))
                .count(),
            1,
            "the token effect should resolve once after the paused batch finishes"
        );
    }

    #[test]
    fn paused_liminal_copy_token_counter_finalizes_entry_after_choice() {
        use std::sync::Arc;

        use crate::game::printed_cards::intrinsic_copiable_values;
        use crate::types::ability::{QuantityModification, ReplacementDefinition, ReplacementMode};
        use crate::types::events::GameEvent;
        use crate::types::game_state::LiminalEntry;
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);
        let replacement_source = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Choice".to_string(),
            Zone::Battlefield,
        );
        {
            let mut def = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .valid_card(TargetFilter::Any)
                .quantity_modification(QuantityModification::Prevent);
            def.mode = ReplacementMode::Optional { decline: None };
            let obj = state.objects.get_mut(&replacement_source).unwrap();
            obj.base_replacement_definitions = Arc::new(vec![def.clone()]);
            obj.replacement_definitions = vec![def].into();
        }

        let copied_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Treasure".to_string(),
            Zone::Battlefield,
        );
        {
            let copied = state.objects.get_mut(&copied_id).unwrap();
            copied.base_card_types = CardType {
                supertypes: vec![],
                core_types: vec![CoreType::Artifact],
                subtypes: vec!["Treasure".to_string()],
            };
            copied.card_types = copied.base_card_types.clone();
            copied.base_name = "Treasure".to_string();
            copied.name = "Treasure".to_string();
        }
        let values = intrinsic_copiable_values(state.objects.get(&copied_id).unwrap());
        let source_id = ObjectId(100);
        let (entry_ref, mut token) =
            reserve_liminal_token_object(&mut state, PlayerId(0), values.name.clone());
        token.is_token = true;
        apply_copiable_values_to_liminal_object(
            &mut token,
            &values,
            DisplaySource::Token,
            None,
            None,
        );
        let timestamp = state.next_timestamp();
        token.reset_for_battlefield_entry(state.turn_number, timestamp);
        state.liminal_entries.insert(
            entry_ref,
            LiminalEntry {
                object: token,
                name: values.name.clone(),
                source_id,
                controller: PlayerId(0),
                enters_attacking: false,
                attach_to: None,
                sacrifice_at: Some(Duration::UntilEndOfCombat),
                remaining_count: 0,
                created_ids: Vec::new(),
                copy_resume: Some(Box::new(CopyTokenSpec {
                    values: Box::new(values.clone()),
                    display_source: DisplaySource::Token,
                    printed_ref: None,
                    token_image_ref: None,
                    extra_keywords: Vec::new(),
                    additional_modifications: Vec::new(),
                    tapped: false,
                    enters_attacking: false,
                    sacrifice_at: Some(Duration::UntilEndOfCombat),
                    source_id,
                    controller: PlayerId(0),
                })),
                spec_resume: None,
                enter_tapped: EtbTapState::Unspecified,
                enter_with_counters: Vec::new(),
                kind: crate::types::game_state::LiminalEntryKind::Token,
                replacement_applied: std::collections::HashSet::new(),
            },
        );

        let event = ProposedEvent::TokenEntry {
            entry_ref,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: vec![(CounterType::Plus1Plus1, 1)],
            applied: HashSet::new(),
        };
        let mut events = Vec::new();
        assert!(
            !commit_liminal_token_entry_and_continue_copy_batch(&mut state, event, &mut events),
            "counter replacement choice should pause liminal entry"
        );
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice { .. }
        ));
        assert!(
            state.objects.contains_key(&entry_ref),
            "liminal entry has already committed its object before the counter choice"
        );
        assert!(
            !state.liminal_entries.contains_key(&entry_ref),
            "the resume path must not depend on the removed liminal entry"
        );

        let result =
            apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 }).unwrap();
        events.extend(result.events);

        assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.last_created_token_ids, vec![entry_ref]);
        let token = state.objects.get(&entry_ref).unwrap();
        assert!(
            token
                .abilities
                .iter()
                .any(|ability| matches!(*ability.effect, Effect::Mana { .. })),
            "Treasure copy must receive predefined token abilities after the counter choice"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::ZoneChanged { object_id, .. } if *object_id == entry_ref)),
            "finalization must emit the liminal token's battlefield entry event"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::TokenCreated { object_id, .. } if *object_id == entry_ref)),
            "finalization must emit the token-created event"
        );
        assert!(
            state
                .delayed_triggers
                .iter()
                .any(|trigger| trigger.source_id == source_id),
            "Mishra-style until-end-of-combat sacrifice must be registered after resume"
        );
    }

    // CR 111.1 + CR 616.1: The Brass's Bounty fix, end to end. A folded
    // "for each X, create a token" ability carries the iteration in `count`
    // (here `Fixed{3}` standing in for 3 lands), so `resolve` proposes ONE
    // batched CreateToken event. With Xorn's `Plus{1}` token replacement on the
    // battlefield, the batch becomes 3 + 1 = 4 tokens.
    //
    // This discriminates the fix from the pre-fix bug: when the same instruction
    // was modeled as `count: 1` + `repeat_for: 3`, the loop emitted three
    // separate count-1 events and Xorn's +1 applied to each — `(1 + 1) * 3 = 6`
    // tokens. Asserting exactly 4 (not 6) proves the single batched event.
    // Xorn is a lone candidate, so no CR 616.1 ordering prompt is involved.
    #[test]
    fn folded_for_each_token_applies_xorn_once_to_the_batch() {
        use crate::game::game_object::GameObject;
        use crate::types::ability::{QuantityModification, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let mut state = GameState::new_two_player(42);

        // Xorn: "create those tokens plus an additional Treasure" — modeled as a
        // CreateToken count `Plus{1}` replacement.
        let xorn_repl = ReplacementDefinition::new(ReplacementEvent::CreateToken)
            .quantity_modification(QuantityModification::Plus { value: 1 });
        let mut xorn = GameObject::new(
            ObjectId(50),
            CardId(1),
            PlayerId(0),
            "Xorn".to_string(),
            Zone::Battlefield,
        );
        xorn.replacement_definitions = vec![xorn_repl].into();
        state.objects.insert(ObjectId(50), xorn);
        state.battlefield.push_back(ObjectId(50));

        // The folded shape: a single Token effect whose `count` carries the
        // per-land quantity (3), with no `repeat_for` loop.
        let ability = ResolvedAbility::new(
            Effect::Token {
                name: "treasure".to_string(),
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                types: vec!["Artifact".to_string(), "Treasure".to_string()],
                colors: vec![],
                keywords: vec![],
                tapped: false,
                count: QuantityExpr::Fixed { value: 3 },
                owner: TargetFilter::Controller,
                attach_to: None,
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.last_created_token_ids.len(),
            4,
            "batched event: 3 + Xorn's 1 = 4 tokens (the pre-fix per-token loop would give 6)"
        );
    }

    // ── attach_to consumption (issue #687 follow-up) ─────────────────────

    /// Build a Role-token `Effect::Token` whose `attach_to` host is supplied by
    /// `attach_to` and whose `repeat_for` (None for single-target) is set by the
    /// caller. The Role enters as Enchantment Aura Role per CR 303.7.
    fn role_token_effect(attach_to: Option<TargetFilter>) -> Effect {
        Effect::Token {
            name: "Cursed Role".to_string(),
            power: PtValue::Fixed(0),
            toughness: PtValue::Fixed(0),
            types: vec![
                "Enchantment".to_string(),
                "Aura".to_string(),
                "Role".to_string(),
            ],
            colors: vec![],
            keywords: vec![],
            tapped: false,
            count: QuantityExpr::Fixed { value: 1 },
            owner: TargetFilter::Controller,
            attach_to,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        }
    }

    fn spawn_creature(state: &mut GameState, controller: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(7),
            controller,
            name.to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        id
    }

    /// CR 303.4: A single-target "Create a Role token attached to target creature
    /// you control" (Betroth the Beast, Guard Change, etc.) attaches the created
    /// Role to the chosen target carried in `ability.targets`. Pre-fix the
    /// `attach_to` field was dropped under `..` and every such token was created
    /// unattached. The Typed targeting filter resolves to the first Object slot.
    #[test]
    fn single_target_role_token_attaches_to_chosen_creature() {
        let mut state = GameState::new_two_player(42);
        let creature = spawn_creature(&mut state, PlayerId(0), "Bear");

        let ability = ResolvedAbility::new(
            role_token_effect(Some(TargetFilter::Typed(
                crate::types::ability::TypedFilter::creature()
                    .controller(crate::types::ability::ControllerRef::You),
            ))),
            vec![TargetRef::Object(creature)],
            ObjectId(100),
            PlayerId(0),
        );

        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        let role = state.last_created_token_ids[0];
        assert_eq!(
            state.objects[&role].attached_to,
            Some(AttachTarget::Object(creature)),
            "Role token must enter attached to the chosen creature"
        );
        assert!(
            state.objects[&creature].attachments.contains(&role),
            "host's attachments list must include the Role"
        );
    }

    /// CR 303.4 + CR 603.7 + CR 109.5: Asinine Antics — for each creature an
    /// opponent controls, create a Cursed Role token attached to that creature.
    /// Drives the real `repeat_for: ObjectCount` loop through
    /// `resolve_ability_chain` so the per-iteration ParentTarget rebind binds each
    /// distinct creature. DISCRIMINATING: before the member-driven gate recognizes
    /// `Token { attach_to }`, `ParentTarget` finds no object slot, the loop never
    /// becomes member-driven, and both Roles end up unattached; post-fix the two
    /// Roles attach to the two distinct opponent creatures.
    #[test]
    fn asinine_antics_attaches_one_role_per_opponent_creature() {
        let mut state = GameState::new_two_player(42);
        let c1 = spawn_creature(&mut state, PlayerId(1), "Opp Creature 1");
        let c2 = spawn_creature(&mut state, PlayerId(1), "Opp Creature 2");

        let mut ability = ResolvedAbility::new(
            role_token_effect(Some(TargetFilter::ParentTarget)),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                ),
            },
        });

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        let role_hosts: std::collections::HashSet<AttachTarget> = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Role"))
            .filter_map(|obj| obj.attached_to)
            .collect();

        assert_eq!(
            role_hosts,
            std::collections::HashSet::from([AttachTarget::Object(c1), AttachTarget::Object(c2)]),
            "exactly one Role per opponent creature, each attached to a distinct host"
        );
    }

    /// CR 303.4g: an `attach_to: ParentTarget` for-each loop with an empty member
    /// set creates zero tokens (member-driven count = 0), so no orphaned Auras
    /// appear.
    #[test]
    fn asinine_antics_no_creatures_creates_no_roles() {
        let mut state = GameState::new_two_player(42);

        let mut ability = ResolvedAbility::new(
            role_token_effect(Some(TargetFilter::ParentTarget)),
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        ability.repeat_for = Some(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(
                    crate::types::ability::TypedFilter::creature()
                        .controller(crate::types::ability::ControllerRef::Opponent),
                ),
            },
        });

        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0).unwrap();

        assert!(
            state.last_created_token_ids.is_empty(),
            "no opponent creatures ⇒ zero Role tokens"
        );
    }

    /// CR 115.1a + CR 601.2c: Betroth the Beast — "Create a Royal Role token
    /// attached to target creature you control." Drives the REAL cast pipeline:
    /// the spell must enter `TargetSelection` (proving `target_filter()` now
    /// surfaces the targetable `attach_to`), the controller selects creature B,
    /// and after resolution the Role attaches to B.
    ///
    /// DISCRIMINATING: before `Effect::Token::target_filter()` surfaces a
    /// targetable `attach_to`, no target slot is generated — `CastSpell` would NOT
    /// enter `TargetSelection`, so the first assertion fails and creature B can
    /// never be chosen.
    #[test]
    fn single_target_role_spell_targets_and_attaches_to_chosen_creature() {
        use crate::types::mana::{ManaType, ManaUnit};

        let parsed = crate::parser::parse_oracle_text(
            "Create a Royal Role token attached to target creature you control.",
            "Betroth the Beast",
            &[],
            &["Sorcery".to_string()],
            &[],
        );
        let spell_ability = parsed
            .abilities
            .iter()
            .find(|a| matches!(*a.effect, Effect::Token { .. }))
            .expect("Betroth the Beast parses to a Token spell ability")
            .clone();

        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let creature_a = spawn_creature(&mut state, PlayerId(0), "Bear A");
        let creature_b = spawn_creature(&mut state, PlayerId(0), "Bear B");

        let spell = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Betroth the Beast".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&spell).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            Arc::make_mut(&mut obj.abilities).push(spell_ability);
            obj.mana_cost = ManaCost::Cost {
                shards: vec![crate::types::mana::ManaCostShard::White],
                generic: 0,
            };
        }
        // Pay {W}.
        state.players[0].mana_pool.add(ManaUnit {
            color: ManaType::White,
            source_id: ObjectId(0),
            pip_id: crate::types::mana::ManaPipId(0),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: Vec::new(),
            grants: vec![],
            expiry: None,
        });

        let result = apply_as_current(
            &mut state,
            GameAction::CastSpell {
                object_id: spell,
                card_id: CardId(903),
                targets: vec![],

                payment_mode: crate::types::game_state::CastPaymentMode::Auto,
            },
        )
        .unwrap();
        assert!(
            matches!(result.waiting_for, WaitingFor::TargetSelection { .. }),
            "targetable attach_to must surface a target slot (got {:?})",
            result.waiting_for
        );

        apply_as_current(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_b)],
            },
        )
        .unwrap();

        // Drive priority passes until the stack resolves.
        for _ in 0..6 {
            if state.stack.is_empty() && matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                break;
            }
            let _ = apply_as_current(&mut state, GameAction::PassPriority).unwrap();
        }

        let role = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .find(|obj| obj.is_token && obj.card_types.subtypes.iter().any(|s| s == "Role"))
            .expect("a Royal Role token must be created");
        assert_eq!(
            role.attached_to,
            Some(AttachTarget::Object(creature_b)),
            "Role must attach to the chosen target (B), not A"
        );
        assert!(
            state.objects[&creature_b]
                .attachments
                .iter()
                .any(|&id| state.objects[&id]
                    .card_types
                    .subtypes
                    .iter()
                    .any(|s| s == "Role")),
            "creature B's attachments must include the Role"
        );
        assert!(
            state.objects[&creature_a].attachments.is_empty(),
            "creature A (not chosen) must have no attachments"
        );
    }

    // ── Equipment-token catalog injection (#942) ────────────────────────

    /// Helper: the single activated equip ability injected onto a token, if any.
    fn injected_equip_ability(
        obj: &crate::game::game_object::GameObject,
    ) -> Option<&AbilityDefinition> {
        obj.abilities
            .iter()
            .find(|a| matches!(*a.effect, Effect::Attach { .. }))
    }

    fn build_catalog_token(state: &mut GameState, name: &str, preset_id: &str) -> ObjectId {
        let preset = crate::game::token_presets::known_token_preset_by_id(preset_id)
            .unwrap_or_else(|| panic!("preset {name} ({preset_id}) must exist"));
        let obj_id = create_object(
            state,
            CardId(0),
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.is_token = true;
            obj.token_image_ref = preset.token_image_ref.clone();
        }
        inject_catalog_token_abilities(state, obj_id);
        obj_id
    }

    #[test]
    fn catalog_rules_text_routes_all_ability_kinds() {
        let (statics, modifications, unparsed_lines) = catalog_rules_text_abilities(
            "Flying\n\
             This creature can't block.\n\
             {T}: Add {G}.\n\
             When this creature dies, you gain 1 life.",
        );
        assert!(unparsed_lines.is_empty());

        assert!(
            statics
                .iter()
                .any(|def| { matches!(def.mode, crate::types::statics::StaticMode::CantBlock) }),
            "static rules text must parse as a full StaticDefinition, got {statics:?}"
        );
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::AddKeyword {
                    keyword: Keyword::Flying
                }
            )),
            "keyword rules text must route to AddKeyword, got {modifications:?}"
        );
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantAbility { definition }
                    if matches!(*definition.effect, Effect::Mana { .. })
            )),
            "activated rules text must route to GrantAbility, got {modifications:?}"
        );
        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantTrigger { .. }
            )),
            "trigger rules text must route to GrantTrigger, got {modifications:?}"
        );
    }

    #[test]
    fn catalog_pilot_preset_grants_crew_contribution_static() {
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id =
            build_catalog_token(&mut state, "Pilot", "6c112277-fd0b-5566-a5f5-0f59216e0444");
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
        }

        assert!(
            state.objects[&obj_id]
                .static_definitions
                .iter_all()
                .any(|def| matches!(
                    def.mode,
                    crate::types::statics::StaticMode::CrewContribution {
                        kind: crate::types::statics::CrewContributionKind::PowerDelta { delta: 2 },
                        ..
                    }
                )),
            "Shorikai Pilot catalog rules_text must inject CrewContribution"
        );
        assert_eq!(
            crate::game::static_abilities::object_crew_power_contribution(
                &state,
                obj_id,
                crate::types::statics::CrewAction::Crew,
            ),
            3,
            "1/1 Shorikai Pilot must contribute 3 power toward crew"
        );
    }

    /// CR 111.3: A Kamigawa Shorikai/Kotori Pilot token ("crews Vehicles as
    /// though its power were 2 greater", a `[Crew]`-only contribution) whose body
    /// matches — and is rendered with — the Aetherdrift Pilot art preset ("saddles
    /// Mounts and crews Vehicles …", a `[Saddle, Crew]` contribution) must NOT
    /// pick up the art preset's static on top of its own. The creating effect's
    /// `with "..."` grant is authoritative; the catalog is display-only here.
    /// Regression: the token was crewing for 5 (1 + 2 + 2) instead of 3 because
    /// the two statics have different `actions` and slipped past an exact-match
    /// de-dupe.
    #[test]
    fn catalog_skips_functional_injection_when_effect_already_granted_crew_static() {
        use crate::types::statics::{CrewAction, CrewContributionKind, StaticMode};
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id = create_object(
            &mut state,
            CardId(0),
            PlayerId(0),
            "Pilot".to_string(),
            Zone::Battlefield,
        );
        // Aetherdrift Pilot preset — a *different* printing than the creating
        // card, carrying a `[Saddle, Crew]` contribution in its rules_text.
        let aetherdrift_pilot = crate::game::token_presets::known_token_preset_by_id(
            "648bee61-604f-58a2-8beb-11faa77a89af",
        )
        .expect("Aetherdrift Pilot preset must exist");
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.is_token = true;
            obj.power = Some(1);
            obj.toughness = Some(1);
            obj.base_power = Some(1);
            obj.base_toughness = Some(1);
            obj.token_image_ref = aetherdrift_pilot.token_image_ref.clone();
            // The creating effect (Shorikai/Kotori) already granted the crew-only
            // static via its `with "..."` clause.
            let with_clause = StaticDefinition::new(StaticMode::CrewContribution {
                kind: CrewContributionKind::PowerDelta { delta: 2 },
                actions: vec![CrewAction::Crew],
            })
            .affected(TargetFilter::SelfRef);
            Arc::make_mut(&mut obj.base_static_definitions).push(with_clause.clone());
            obj.static_definitions.push(with_clause);
        }

        inject_catalog_token_abilities(&mut state, obj_id);

        let crew_statics = state.objects[&obj_id]
            .static_definitions
            .iter_all()
            .filter(|def| matches!(def.mode, StaticMode::CrewContribution { .. }))
            .count();
        assert_eq!(
            crew_statics, 1,
            "the art preset's crew static must not stack on the effect's own grant"
        );
        assert_eq!(
            crate::game::static_abilities::object_crew_power_contribution(
                &state,
                obj_id,
                CrewAction::Crew,
            ),
            3,
            "1/1 Pilot with a single +2 crew delta must contribute 3, not 5"
        );
    }

    #[test]
    fn catalog_cragflame_preset_grants_static_and_equip() {
        // CR 702.6a: Mabel's Cragflame is a two-line catalog rules_text —
        // a static buff line plus a standalone "Equip {2}" activated-ability
        // line. Pre-fix the whole-blob classifier swallowed the equip line, so
        // the token carried the buff but no equip ability. Per-line classify
        // installs both.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id = build_catalog_token(
            &mut state,
            "Cragflame",
            "524e2513-4a49-53bf-a5fa-150dc718c5f1",
        );
        let obj = &state.objects[&obj_id];

        // (a) exactly one activated equip ability: Attach SelfRef → creature you
        // control, {2} mana cost, sorcery-speed (CR 702.6a). This is the
        // discriminating assertion — empty pre-fix.
        let equips: Vec<&AbilityDefinition> = obj
            .abilities
            .iter()
            .filter(|a| matches!(*a.effect, Effect::Attach { .. }))
            .collect();
        assert_eq!(
            equips.len(),
            1,
            "Cragflame must inject exactly one equip activated ability (was zero pre-fix)"
        );
        let equip = equips[0];
        assert!(matches!(
            *equip.effect,
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                ..
            }
        ));
        assert!(
            matches!(
                &equip.cost,
                Some(AbilityCost::Mana { cost }) if cost == &ManaCost::generic(2)
            ),
            "equip cost must be {{2}}, got {:?}",
            equip.cost
        );
        assert!(
            equip
                .activation_restrictions
                .contains(&ActivationRestriction::AsSorcery),
            "equip ability must be sorcery-speed (CR 702.6a)"
        );

        // (b) regression guard: the static buff line is still installed as a
        // static definition affecting the equipped creature.
        assert!(
            !obj.static_definitions.is_empty(),
            "Cragflame must still install its '+1/+1 and has vigilance/trample/haste' static buff"
        );
    }

    #[test]
    fn catalog_cragflame_equip_attaches_and_buffs_creature() {
        // CR 702.6a: activating the injected equip ability attaches Cragflame to
        // a creature you control; the static buff then grants +1/+1 and the
        // keywords once layers re-derive.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let cragflame = build_catalog_token(
            &mut state,
            "Cragflame",
            "524e2513-4a49-53bf-a5fa-150dc718c5f1",
        );

        let bear = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&bear).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.base_power = Some(2);
            obj.base_toughness = Some(2);
        }

        let equip_def = injected_equip_ability(&state.objects[&cragflame])
            .expect("Cragflame must have an injected equip ability")
            .clone();
        let ability = build_resolved_from_def_with_targets(
            &equip_def,
            cragflame,
            PlayerId(0),
            vec![TargetRef::Object(bear)],
        );
        let mut events = Vec::new();
        super::super::resolve_ability_chain(&mut state, &ability, &mut events, 0)
            .expect("equip ability should resolve");

        assert_eq!(
            state.objects[&cragflame].attached_to,
            Some(crate::game::game_object::AttachTarget::Object(bear)),
            "Cragflame must be attached to the bear after equip resolves"
        );
        assert!(state.objects[&bear].attachments.contains(&cragflame));

        crate::game::layers::evaluate_layers(&mut state);
        let buffed = &state.objects[&bear];
        assert_eq!(
            buffed.power,
            Some(3),
            "equipped creature gets +1/+1 (power)"
        );
        assert_eq!(
            buffed.toughness,
            Some(3),
            "equipped creature gets +1/+1 (toughness)"
        );
        for kw in [Keyword::Vigilance, Keyword::Trample, Keyword::Haste] {
            assert!(
                crate::game::keywords::has_keyword(buffed, &kw),
                "equipped creature must gain {kw:?} from Cragflame"
            );
        }
    }

    #[test]
    fn catalog_toggo_rock_preset_grants_equip() {
        // CR 702.6a class coverage: Toggo's Rock is another two-line Equipment
        // catalog token ("Equipped creature has \"...\"" + "Equip {1}"). Per-line
        // classify must install its equip ability too — build for the class of
        // all 8 catalog equip tokens, not Cragflame alone.
        let mut state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let obj_id =
            build_catalog_token(&mut state, "Rock", "1657233e-c9e1-54ff-aa5a-6e2e2846be42");
        let equip = injected_equip_ability(&state.objects[&obj_id])
            .expect("Toggo's Rock must inject an equip activated ability");
        assert!(matches!(
            *equip.effect,
            Effect::Attach {
                attachment: TargetFilter::SelfRef,
                ..
            }
        ));
        assert!(
            matches!(&equip.cost, Some(AbilityCost::Mana { cost }) if cost == &ManaCost::generic(1)),
            "Toggo's Rock equip cost must be {{1}}, got {:?}",
            equip.cost
        );
        assert!(equip
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery));
    }

    #[test]
    fn classify_quoted_inner_equip_line_is_activated_ability_static_line_unchanged() {
        use crate::parser::oracle_static::classify_quoted_inner;

        // A standalone "Equip {N}" line classifies as a GrantAbility wrapping the
        // Effect::Attach activated ability (CR 702.6a) — not an inert AddKeyword.
        let equip = classify_quoted_inner("Equip {2}");
        assert_eq!(equip.len(), 1);
        match &equip[0] {
            ContinuousModification::GrantAbility { definition } => {
                assert!(matches!(*definition.effect, Effect::Attach { .. }));
                assert!(matches!(
                    &definition.cost,
                    Some(AbilityCost::Mana { cost }) if cost == &ManaCost::generic(2)
                ));
                assert!(definition
                    .activation_restrictions
                    .contains(&ActivationRestriction::AsSorcery));
            }
            other => panic!("expected GrantAbility for 'Equip {{2}}', got {other:?}"),
        }

        // The static buff line is unchanged: it must NOT classify as an equip
        // ability, preserving the no-regression contract for single-line presets.
        let buff = classify_quoted_inner("Equipped creature gets +1/+1.");
        assert!(
            !buff
                .iter()
                .any(|m| matches!(m, ContinuousModification::GrantAbility { .. })),
            "static buff line must not be misclassified as an activated equip ability"
        );
        assert!(
            !buff.is_empty(),
            "static buff line must classify to something"
        );
    }

    // ── Ka-Zar / Zabu landfall: parse → resolve → trigger ────────────────

    /// Parse Ka-Zar's ETB token line into a real `Effect::Token` (so the test
    /// exercises the actual parser output, not a hand-built trigger), wrapped in
    /// a `ResolvedAbility` controlled by `controller`.
    fn kazar_token_ability(controller: PlayerId) -> ResolvedAbility {
        let txt = "Create Zabu, a legendary 2/2 green Cat creature token with \"Landfall — Whenever a land you control enters, put a +1/+1 counter on Zabu.\"";
        let effect = crate::parser::oracle_effect::token::try_parse_token(
            &txt.to_lowercase(),
            txt,
            &mut crate::parser::oracle_ir::context::ParseContext::default(),
        )
        .expect("Ka-Zar token line must parse");
        ResolvedAbility::new(effect, vec![], ObjectId(500), controller)
    }

    /// Resolve Ka-Zar's token effect and return the created Zabu's `ObjectId`.
    fn create_zabu(state: &mut GameState, controller: PlayerId) -> ObjectId {
        let ability = kazar_token_ability(controller);
        let mut events = Vec::new();
        resolve(state, &ability, &mut events).unwrap();
        // CR 604.2: run the layers pass so the token's `GrantTrigger` static
        // modification is installed as a live trigger_definition before any land
        // ETB is processed.
        crate::game::layers::flush_layers(state);
        *state
            .battlefield
            .iter()
            .find(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|o| o.is_token && o.name == "Zabu")
            })
            .expect("Zabu token must be on the battlefield")
    }

    /// Put a land onto the battlefield under `land_controller` and fire its ETB
    /// event through the real trigger pipeline, then resolve the stack.
    fn land_enters(state: &mut GameState, land_controller: PlayerId, card_id: u64) {
        let land = create_object(
            state,
            CardId(card_id),
            land_controller,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.controller = land_controller;
            obj.owner = land_controller;
        }
        let mut record = crate::types::game_state::ZoneChangeRecord::test_minimal(
            land,
            Some(Zone::Hand),
            Zone::Battlefield,
        );
        record.name = "Forest".to_string();
        record.core_types = vec![CoreType::Land];
        record.subtypes = vec!["Forest".to_string()];
        record.controller = land_controller;
        record.owner = land_controller;
        let event = GameEvent::ZoneChanged {
            object_id: land,
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(record),
        };
        crate::game::triggers::process_triggers(state, &[event]);
        // Resolve every triggered ability the land ETB put on the stack.
        let mut events = Vec::new();
        while !state.stack.is_empty() {
            crate::game::stack::resolve_top(state, &mut events);
        }
    }

    fn zabu_plus1_counters(state: &GameState, zabu: ObjectId) -> u32 {
        state
            .objects
            .get(&zabu)
            .and_then(|o| o.counters.get(&CounterType::Plus1Plus1).copied())
            .unwrap_or(0)
    }

    /// CR 603.6a + CR 207.2c: A land entering under Zabu's controller fires
    /// Zabu's landfall trigger; the +1/+1 counter lands on ZABU. Discriminating:
    /// reverting the ability-word strip makes the trigger parse as
    /// `GrantAbility(Unimplemented[landfall])`, which installs no live trigger,
    /// so this assertion (`counters == 1`) flips to 0.
    #[test]
    fn zabu_landfall_puts_counter_on_zabu_for_controllers_land() {
        let mut state = GameState::new_two_player(42);
        let zabu = create_zabu(&mut state, PlayerId(0));
        assert_eq!(
            zabu_plus1_counters(&state, zabu),
            0,
            "no counters before ETB"
        );

        land_enters(&mut state, PlayerId(0), 700);

        assert_eq!(
            zabu_plus1_counters(&state, zabu),
            1,
            "a land under Zabu's controller must put one +1/+1 counter on Zabu"
        );
    }

    /// CR 603.6a: "a land YOU control" binds "you" to Zabu's controller, so a
    /// land entering under the OPPONENT's control must NOT fire Zabu's landfall.
    #[test]
    fn zabu_landfall_ignores_opponents_land() {
        let mut state = GameState::new_two_player(42);
        let zabu = create_zabu(&mut state, PlayerId(0));

        land_enters(&mut state, PlayerId(1), 701);

        assert_eq!(
            zabu_plus1_counters(&state, zabu),
            0,
            "an opponent's land must not fire Zabu's landfall trigger"
        );
    }

    /// The counter goes on ZABU, not on Ka-Zar (the source permanent). Build a
    /// distinct Ka-Zar object as the trigger source's controller's other
    /// permanent and confirm it never receives the counter.
    #[test]
    fn zabu_landfall_counter_targets_zabu_not_kazar() {
        let mut state = GameState::new_two_player(42);
        // A stand-in Ka-Zar permanent already on the battlefield under P0.
        let kazar = create_object(
            &mut state,
            CardId(900),
            PlayerId(0),
            "Ka-Zar of the Savage Land".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&kazar)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        let zabu = create_zabu(&mut state, PlayerId(0));

        land_enters(&mut state, PlayerId(0), 702);

        assert_eq!(
            zabu_plus1_counters(&state, zabu),
            1,
            "counter must land on Zabu"
        );
        assert_eq!(
            zabu_plus1_counters(&state, kazar),
            0,
            "counter must NOT land on Ka-Zar"
        );
    }
}

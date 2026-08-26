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
    PendingTokenBattlefieldEntry, TokenEntryEventEmission, WaitingFor,
};
use crate::types::identifiers::{CardId, ObjectId, ObjectIncarnationRef, TrackedSetId};
use crate::types::keywords::{Keyword, WardCost};
use crate::types::mana::{ManaColor, ManaCost};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CopyTokenSpec, ProposedEvent, TokenHostRequest, TokenSpec};
use crate::types::resolved_commands::{
    ResolvedCopyBodyModifications, ResolvedTokenBody, ResolvedTokenCreationCommand,
    ResolvedTokenCreationReplayInvariantError,
};
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
        // CR 111.10k: A Monster Role (like every predefined Role) has enchant creature.
        materialized
            .keywords
            .push(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )));
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
        catalog_rules_text_abilities(rules_text, &preset.body.display_name);
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
    // shared target/event-context path.
    //
    // The result keeps "the instruction named no host" apart from "it named one
    // and nothing bound it": CR 303.4i denies the entry of an Aura token in the
    // second case and says nothing about the first, and the seam that applies
    // that verdict runs after the CR 614 replacement pipeline, far from here.
    let host_request = TokenHostRequest::from_binding(
        attach_to.is_some(),
        attach_to.and_then(|f| resolve_attach_host(state, ability, f)),
    );

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
        host_request,
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
    attach_to: TokenHostRequest,
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
/// CR 303.4i: does the rule deny this entrant's battlefield entry?
///
/// > If an effect attempts to put an Aura onto the battlefield attached to
/// > either an object or player it can't legally enchant or an object or player
/// > that is undefined, … If the Aura is a token, it isn't created.
///
/// Asked per token and on the ACTUAL entrant, after the CR 614 replacement
/// pipeline has settled its characteristics: a replacement effect may create
/// something other than the Aura the instruction described, and CR 303.4i is a
/// question about what is entering rather than about what was announced.
///
/// Both halves are answered by the authority that already owns them —
/// [`attach::authority_is_aura`] for Aura-ness (CR 205.1a: a copy exception can
/// add or remove the subtype, so it reads characteristics, not the effect's
/// `types`), and [`attach::can_attach_to_object`] / [`attach::can_attach_to_player`]
/// for "can't legally enchant". That second pair is the SAME verdict
/// `attach::attach_to` consumes a few lines later, so the gate and the
/// attachment can never disagree about one host.
///
/// [`attach::authority_is_aura`]: super::attach::authority_is_aura
/// [`attach::can_attach_to_object`]: super::attach::can_attach_to_object
/// [`attach::can_attach_to_player`]: super::attach::can_attach_to_player
fn aura_token_entry_denied(state: &GameState, entrant: ObjectId, host: TokenHostRequest) -> bool {
    // CR 303.4h: "If an effect attempts to put a permanent that isn't an Aura,
    // Equipment, or Fortification onto the battlefield attached to an object or
    // player, it enters the battlefield unattached." It is created either way,
    // so CR 303.4i is not its rule.
    if !super::attach::authority_is_aura(state, entrant, super::attach::AttachmentAuthority::Stored)
    {
        return false;
    }
    match host {
        // CR 303.4f — an Aura entering with no effect-specified host has its
        // controller choose one — is a different rule with a different
        // disposition (a choice, not a denial). Its consult belongs to the entry
        // pipeline (`zone_pipeline::entering_aura_hosts`), which the liminal and
        // copy token seams reach and this one does not — a stated gap, not an
        // oversight. Measured over the shipped pool: every Aura-typed token spec
        // names a host, so no card reaches this arm today.
        TokenHostRequest::NotRequested => false,
        // CR 303.4i, "undefined": the instruction named a host and nothing bound
        // it — the shape #7302 reported.
        TokenHostRequest::Unbound => true,
        // CR 303.4i, "can't legally enchant": the host is defined, so the
        // question is legality, and legality is `attach`'s to answer.
        TokenHostRequest::Bound(AttachTarget::Object(host_id)) => {
            !super::attach::can_attach_to_object(state, entrant, host_id)
        }
        TokenHostRequest::Bound(AttachTarget::Player(host_player)) => {
            !super::attach::can_attach_to_player(state, entrant, host_player)
        }
    }
}

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
        if let Some(pending) = state.active_copy_token_mut() {
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

        // CR 614.1: the post-replacement tapped state, resolved once so the
        // shared body installs the same value a CR 733 replay will.
        let resulting_tapped = enter_tapped.resolve(spec.tapped);
        let turn_number = state.turn_number;
        let created_reference = state.objects.get_mut(&obj_id).map(|obj| {
            materialize_token_spec_body(
                obj,
                &spec,
                token_image_ref.clone(),
                turn_number,
                entry_timestamp,
                resulting_tapped,
            );
            ObjectIncarnationRef::from_object(obj)
        });

        // CR 303.4i: settle whether this entrant is created at all, BEFORE the
        // CR 733 birth is journaled (the journal is append-only — a birth
        // recorded here could not be retracted) and before anything the game can
        // observe is emitted. Same decide/act split as the copy seam in
        // `token_copy.rs` and the liminal seam in
        // `commit_liminal_token_entry_with_post_actions`.
        //
        // Everything done so far for this token is silent: `zones::create_object`
        // inserts and zones the object without an event, and
        // `materialize_token_spec_body` only fills it in. `uncreate_unentered_
        // aura_token` undoes exactly that pair, so a denied entry leaves no
        // `TokenCreated`, no `ZoneChanged`, no birth record, no `created_ids`
        // row, and nothing in any graveyard. The loop goes on to the next token
        // of the count; the tail's `last_created_token_ids = created_ids` then
        // publishes only the tokens that were actually created, so a later
        // `TargetFilter::LastCreated` cannot read a denied one — or, when the
        // whole batch is denied, an earlier unrelated batch.
        if aura_token_entry_denied(state, obj_id, spec.attach_to) {
            uncreate_unentered_aura_token(state, obj_id, owner);
            continue;
        }

        // CR 733: journal the settled creation, after the body borrow ends.
        // Counters, the attacking entry, and any later status change journal
        // through their OWN families, so this command covers the birth only.
        if let Some(object) = created_reference {
            let cause = state.current_or_begin_rules_execution_node();
            let command = ResolvedTokenCreationCommand {
                object,
                owner,
                entry_timestamp,
                entry_turn: turn_number,
                body: ResolvedTokenBody::Spec {
                    spec: spec.clone(),
                    token_image_ref: token_image_ref.clone(),
                },
                resulting_tapped,
                resulting_next_object_id: state.next_object_id,
                cause,
            };
            state
                .resolved_rules_journal
                .record_token_creation(command)
                .expect("resolved token creation must have a live journal cause");
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
                        attach_to: spec.attach_to.bound(),
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
        // CR 608.2i battlefield-entry bookkeeping is done by `record_zone_change` inside
        // `push_committed_token_entry_events` below — recording it here too double-counts.
        crate::game::restrictions::record_token_created(state, obj_id);

        // CR 303.4: A Role/Aura token created "attached to" a host enters
        // attached. The ACT half of the CR 303.4i split above: an Aura whose
        // host is undefined or illegal never reaches this line, so this is an
        // attachment that the gate has already found legal, not an attempt.
        // CR 303.4h keeps the other class here — a non-Aura token named a host
        // too, and if that host is illegal it simply enters unattached. For
        // multiple same-controller Roles on one host, CR 704.5z keeps only the
        // latest-timestamp Role. Single authority: effects::attach.
        if let Some(host) = spec.attach_to.bound() {
            match host {
                AttachTarget::Object(id) => {
                    super::attach::attach_to(state, obj_id, id);
                }
                AttachTarget::Player(pid) => {
                    super::attach::attach_to_player(state, obj_id, pid);
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
        // event is emitted for token-specific consumers (animation, logging,
        // `LastCreated` target filters). Single authority for both, and for the
        // CR 400.7 zone-change index the batched replay guard keys on.
        push_committed_token_entry_events(
            state,
            obj_id,
            spec.characteristics.display_name.clone(),
            spec.source_id,
            events,
        );

        // CR 603.7: Tokens with a limited duration get a delayed sacrifice trigger.
        // Used by Mobilize and similar keywords that create temporary attacking tokens.
        if matches!(spec.sacrifice_at, Some(Duration::UntilEndOfCombat)) {
            let sacrifice_token = DelayedTrigger {
                condition: DelayedTriggerCondition::AtNextPhase {
                    phase: Phase::EndCombat,
                },
                ability: Box::new(ResolvedAbility::new(
                    Effect::Sacrifice {
                        target: TargetFilter::Any,
                        count: QuantityExpr::Fixed { value: 1 },
                        min_count: 0,
                    },
                    vec![TargetRef::Object(obj_id)],
                    spec.source_id,
                    spec.controller,
                )),
                controller: spec.controller,
                source_id: spec.source_id,
                one_shot: true,
                provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
            };
            crate::game::triggers::install_delayed_trigger(state, sacrifice_token, events);
        }
    }

    // CR 603.7: Record created token IDs for sub-abilities that reference
    // TargetFilter::LastCreated (e.g., Job select, suspect).
    state.last_created_token_ids = created_ids;
    true
}

/// Materializes one already-resolved CR 111.1 token creation verbatim.
///
/// Unlike every other resolved-command applier, this one CREATES its subject
/// rather than verifying and installing into an existing one, so its
/// precondition is inverted: the recorded id must be ABSENT. It re-runs none of
/// the CR 614 replacement pipeline that decided the token would be created, its
/// count, or its tapped state — all of that was settled when the command was
/// recorded.
///
/// The body is installed through the same `materialize_token_spec_body` /
/// `materialize_token_copy_body` the resolve paths use, so they cannot drift.
///
/// CR 707.9: one copy case is deliberately NOT replayable — exceptions applied
/// after the birth by the unjournaled `apply_token_modifications` seam. That
/// returns `UnreplayableCopyModifications` before anything is materialized,
/// rather than installing a body that is silently missing them.
pub fn apply_resolved_token_creation(
    state: &mut GameState,
    command: &ResolvedTokenCreationCommand,
) -> Result<(), ResolvedTokenCreationReplayInvariantError> {
    let object_id = command.object.object_id;
    if state.objects.contains_key(&object_id) {
        return Err(ResolvedTokenCreationReplayInvariantError::ObjectAlreadyExists(object_id));
    }
    if !state
        .players
        .iter()
        .any(|player| player.id == command.owner)
    {
        return Err(ResolvedTokenCreationReplayInvariantError::UnknownOwner(
            command.owner,
        ));
    }
    if object_id.0 >= command.resulting_next_object_id {
        return Err(
            ResolvedTokenCreationReplayInvariantError::IdAboveHighWater {
                id: object_id,
                high_water: command.resulting_next_object_id,
            },
        );
    }

    // CR 707.9: refuse BEFORE materializing anything, so a body we cannot
    // reproduce exactly never reaches `state.objects`.
    if let ResolvedTokenBody::Copy {
        modifications: ResolvedCopyBodyModifications::DeferredToUnjournaledSeam { modifications },
        ..
    } = &command.body
    {
        return Err(
            ResolvedTokenCreationReplayInvariantError::UnreplayableCopyModifications {
                object: object_id,
                count: modifications.len(),
            },
        );
    }

    let name = match &command.body {
        ResolvedTokenBody::Spec { spec, .. } => spec.characteristics.display_name.clone(),
        ResolvedTokenBody::Copy { copy, .. } => copy.values.name.clone(),
    };
    let mut object = GameObject::new(object_id, CardId(0), command.owner, name, Zone::Battlefield);
    match &command.body {
        ResolvedTokenBody::Spec {
            spec,
            token_image_ref,
        } => materialize_token_spec_body(
            &mut object,
            spec,
            token_image_ref.clone(),
            command.entry_turn,
            command.entry_timestamp,
            command.resulting_tapped,
        ),
        ResolvedTokenBody::Copy {
            copy,
            modifications,
        } => materialize_token_copy_body(
            &mut object,
            copy,
            modifications,
            command.entry_turn,
            command.entry_timestamp,
            command.resulting_tapped,
        ),
    }

    state.objects.insert(object_id, object);
    // allow-raw-zone: replay materializes a token birth, which has no from-zone move (CR 111.1 + CR 614.12).
    zones::add_to_zone(state, object_id, Zone::Battlefield, command.owner);
    // CR 111.3 + CR 111.10: a token's abilities come from the creating effect
    // and the predefined/catalog tables, NOT from the body the command carries,
    // so the body alone materializes a Treasure with no "{T}, Sacrifice this
    // token: Add one mana of any color." Both live paths inject after
    // materializing and before their entry snapshot (Spec: this file, above the
    // `push_committed_token_entry_events` call; Copy: `token_copy.rs`'s
    // `finalize_copied_token` + `inject_predefined_token_abilities`), so replay
    // does the same here, per body variant. The dispatch mirrors
    // `finalize_committed_liminal_token_entry_from_action`'s
    // `LiminalTokenAbilityInjection` match arm-for-arm — a blanket
    // `inject_resolved_token_abilities` would be wrong for the Copy body, whose
    // live authority uses the predefined-only injector after
    // `finalize_copied_token`'s CR 707.2 cast-only strip.
    match &command.body {
        ResolvedTokenBody::Copy { copy, .. } => {
            super::token_copy::finalize_copied_token(state, copy.source_id, object_id);
            inject_predefined_token_abilities(state, object_id);
        }
        ResolvedTokenBody::Spec { .. } => inject_resolved_token_abilities(state, object_id),
    }
    // CR 400.7 + CR 608.2i: the resolve path records the birth through
    // `restrictions::record_zone_change` (`push_committed_token_entry_events`),
    // which appends to this turn's zone-change ledger and assigns the entry's
    // index. Replay must record the same entry: the ledger length IS the index
    // allocator, so a birth that records nothing leaves every later replayed
    // zone change one short of its recorded `turn_zone_change_index` and
    // `apply_resolved_zone_change` fails closed on `TurnRecordIndexMismatch`.
    // The record is reconstructed from the materialized object rather than
    // carried on the command: it is a projection of state this applier has
    // already installed.
    //
    // KNOWN CEILING — two record-visible classes the reconstruction cannot
    // reproduce, both because the LIVE journal point (`record_token_creation`,
    // in the resolve path above) runs BEFORE the live mutations and before the
    // live snapshot, so no call site inside THIS applier can close them; they
    // would need the live journal-record point moved:
    //   (i)  `spec.enter_with_counters` — the live snapshot's
    //        `trigger_source_context.lki.counters` (and P/T, if the counter's
    //        layer bump landed first) carry the entry counters. Counters replay
    //        through their own `ObjectCounter` command, journaled AFTER this
    //        birth, so the reconstructed record here has none.
    //   (ii) `spec.attach_to` (Role/Aura tokens) — `record.attached_to`. Same
    //        reason: attachment replays through the Attachment family.
    // A third class, predefined/catalog ability injection contributing
    // `record.trigger_definitions`, IS closed — by the injection dispatch
    // directly above, which runs before this snapshot exactly as the live paths
    // do. Storing the live record on the command would not close (i) or (ii)
    // either, for the same ordering reason, so it was not done.
    let mut entry_record = state
        .objects
        .get(&object_id)
        .expect("the token was materialized above")
        .snapshot_for_zone_change(object_id, None, Zone::Battlefield);
    crate::game::restrictions::record_zone_change(state, &mut entry_record);
    // CR 111.1: replay must not hand the same id out again to a later allocation.
    state.next_object_id = state.next_object_id.max(command.resulting_next_object_id);
    // CR 613.7d: the birth drew an entry timestamp alongside the object id, and
    // the same reasoning applies to it — replay installs the recorded value, so
    // the timestamp allocator must be carried past it or a later draw reissues
    // it and the two objects are unordered within their CR 613 layer.
    state.adopt_replayed_timestamp(command.entry_timestamp);
    Ok(())
}

/// CR 111.1 + CR 113.3d: Installs an ordinary token's body onto `object`.
///
/// Single authority for the ordinary-token body, shared by the resolve path and
/// the CR 733 replay applier so the two cannot drift. Operates on a `&mut
/// GameObject` rather than on `GameState`, so it serves both orderings: the
/// resolve path inserts first and mutates in place, while replay builds a
/// detached object and inserts it afterwards.
pub(crate) fn materialize_token_spec_body(
    object: &mut GameObject,
    spec: &TokenSpec,
    token_image_ref: Option<crate::types::card::TokenImageRef>,
    turn_number: u32,
    entry_timestamp: u64,
    tapped: bool,
) {
    let ch = &spec.characteristics;
    // CR 111.1: Mark as token for SBA cleanup (CR 704.5d)
    object.is_token = true;
    // True token from a TokenSpec — image lives in the generic-token
    // database (Treasure, Spirit, Saproling, Soldier, etc.).
    object.display_source = DisplaySource::Token;
    object.token_image_ref = token_image_ref;
    let has_attrs = ch.power.is_some()
        || ch.toughness.is_some()
        || !ch.core_types.is_empty()
        || !ch.subtypes.is_empty()
        || !ch.supertypes.is_empty()
        || !ch.colors.is_empty()
        || !ch.keywords.is_empty();
    if has_attrs {
        object.power = ch.power;
        object.toughness = ch.toughness;
        object.base_name = ch.display_name.clone();
        object.base_power = ch.power;
        object.base_toughness = ch.toughness;
        object.layer_base_power = ch.power;
        object.layer_base_toughness = ch.toughness;
        object.card_types = CardType {
            supertypes: ch.supertypes.clone(),
            core_types: ch.core_types.clone(),
            subtypes: ch.subtypes.clone(),
        };
        object.base_card_types = object.card_types.clone();
        object.color = ch.colors.clone();
        object.base_color = ch.colors.clone();
        object.keywords = ch.keywords.clone();
        object.base_keywords = ch.keywords.clone();
    }
    // CR 400.7 + CR 302.6: Tokens enter the battlefield as new objects
    // and must run the same ETB-state reset as any other permanent
    // (summoning sickness, echo, damage, loyalty-activated flags).
    // Delegate to the single authority for summoning sickness and
    // related transient flags rather than setting them ad-hoc.
    object.reset_for_battlefield_entry(turn_number, entry_timestamp);
    object.tapped = tapped;

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
        Arc::make_mut(&mut object.base_static_definitions).extend(static_abilities.iter().cloned());
        for static_def in static_abilities {
            object.static_definitions.push(static_def);
        }
        // CR 702.6a + CR 111.4: Only intrinsic Equip activated abilities
        // (unconditional SelfRef `GrantAbility(Attach SelfRef → …)`)
        // are copied onto the token object. Other grants stay in the
        // static/layer path only.
        let equip_abilities = intrinsic_equip_abilities_from_token_statics(&spec.static_abilities);
        if !equip_abilities.is_empty() {
            Arc::make_mut(&mut object.abilities).extend(equip_abilities.iter().cloned());
            Arc::make_mut(&mut object.base_abilities).extend(equip_abilities);
        }
    }
}

/// CR 707.2 + CR 707.5: Installs a copy token's body onto `object`.
///
/// Single authority for the copy-token body, shared by BOTH production copy
/// seams (the liminal build in `token_copy::apply_copy_token_*` and the direct
/// `create_object` path) and by the CR 733 replay applier, so no two of them can
/// drift. Like `materialize_token_spec_body` it operates on a `&mut GameObject`
/// rather than on `GameState`, which is what lets one implementation serve the
/// liminal build-then-insert ordering and the direct insert-then-build ordering.
///
/// `reset_for_battlefield_entry` touches no copiable characteristic, so running
/// it after the keyword grant (the liminal order) is equivalent to running it
/// before (the former direct order).
pub(crate) fn materialize_token_copy_body(
    object: &mut GameObject,
    copy: &CopyTokenSpec,
    modifications: &ResolvedCopyBodyModifications,
    turn_number: u32,
    entry_timestamp: u64,
    tapped: bool,
) {
    // CR 111.1: Mark as token for SBA cleanup (CR 704.5d)
    object.is_token = true;
    // CR 707.2: the copiable values, plus the display metadata that is not
    // itself copiable. `install_copiable_values_as_base` already installs
    // `loyalty`/`base_loyalty` from `values.loyalty` (CR 306.5b), so no separate
    // loyalty seed is needed here.
    apply_copiable_values_to_liminal_object(
        object,
        &copy.values,
        copy.display_source,
        copy.printed_ref.clone(),
        copy.token_image_ref.clone(),
    );

    // CR 707.9a + CR 702: "except it has [keyword]" — grant additional keywords
    // on top of the copied characteristics. Twinflame's haste copies are the
    // canonical case. Idempotent under repeats.
    for kw in &copy.extra_keywords {
        let already_live = object.keywords.contains(kw); // allow-raw-authority: structural live keyword insertion de-dupe, not an effective keyword query
        if !already_live {
            object.keywords.push(kw.clone());
        }
        if !object.base_keywords.contains(kw) {
            object.base_keywords.push(kw.clone());
        }
    }

    match modifications {
        // CR 707.2: the copiable values are the whole body.
        ResolvedCopyBodyModifications::NoExceptions => {}
        // CR 707.9b/9c: exceptions stamped onto the copiable values before entry.
        ResolvedCopyBodyModifications::Folded {
            modifications,
            all_creature_types,
        } => {
            super::token_copy::apply_immediate_copy_token_modifications_to_object(
                object,
                modifications,
                all_creature_types,
            );
        }
        // Owned by the unjournaled `apply_token_modifications` seam and applied
        // after the birth, so the body here is deliberately without them. Replay
        // refuses this case up front in `apply_resolved_token_creation`.
        ResolvedCopyBodyModifications::DeferredToUnjournaledSeam { .. } => {}
    }

    // CR 400.7 + CR 302.6: Single authority for ETB state. Haste granted via
    // `extra_keywords` is folded in at query time by `has_summoning_sickness`.
    object.reset_for_battlefield_entry(turn_number, entry_timestamp);
    object.tapped = tapped;
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

/// Commit ONE liminal copy-token to the battlefield WITHOUT driving the rest of
/// the batch. Returns `false` if an ETB-counter replacement paused mid-commit (a
/// `ContinueLiminalCopyTokenBatch` post-action was stashed to resume). The
/// per-token batch loop in `apply_copy_token_after_replacement_with_created_ids`
/// calls this and iterates, so minting N copies uses O(1) stack depth — the old
/// commit->continue->apply recursion built one large `im::HashMap` COW frame per
/// token. CR 707.2: shared by every liminal copy-token batch.
pub(crate) fn commit_liminal_copy_token_entry(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    let continuation = liminal_copy_token_continuation_for_event(state, &event);
    commit_liminal_copy_token_entry_with_continuation(state, event, continuation, events)
}

fn commit_liminal_copy_token_entry_with_continuation(
    state: &mut GameState,
    event: ProposedEvent,
    continuation: Option<LiminalCopyTokenContinuation>,
    events: &mut Vec<GameEvent>,
) -> bool {
    let post_actions = continuation
        .map(liminal_copy_token_continuation_post_action)
        .into_iter()
        .collect();
    commit_liminal_token_entry_with_post_actions(
        state,
        event,
        events,
        TokenEntryEventEmission::Emit,
        post_actions,
    )
}

pub(crate) fn commit_liminal_token_entry_and_continue_copy_batch(
    state: &mut GameState,
    event: ProposedEvent,
    events: &mut Vec<GameEvent>,
) -> bool {
    let continuation = liminal_copy_token_continuation_for_event(state, &event);
    if !commit_liminal_copy_token_entry_with_continuation(
        state,
        event,
        continuation.clone(),
        events,
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
        owner: entry.object.projected().owner,
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
    let created_ids = state.last_created_token_ids.clone();
    if let Some(pending) = state.active_copy_token_mut() {
        pending.created_ids = created_ids;
    }
    let Some(continuation) = continuation else {
        if state.active_copy_token().is_some() {
            super::token_copy::drain_pending_copy_token_resolution(state, events);
        }
        return !state.active_copy_token().is_some()
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
    let created_ids = state.last_created_token_ids.clone();
    if let Some(pending) = state.active_copy_token_mut() {
        pending.created_ids = created_ids;
    }
    if state.active_copy_token().is_some() {
        super::token_copy::drain_pending_copy_token_resolution(state, events);
    }
    !state.active_copy_token().is_some() || matches!(state.waiting_for, WaitingFor::Priority { .. })
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

/// CR 303.4g / CR 303.4i: undo a token battlefield entry the Aura-entry rules
/// say was never created.
///
/// The ONLY unhosted-entry disposition the liminal seam has, because the only
/// entrant that seam can hold is a [`crate::types::game_state::TokenProjection`]
/// (CR 111.1). The rule's card-backed dispositions are phrased against a
/// from-zone, so they live where a from-zone exists: the
/// `ProposedEvent::ZoneChange` path in `zone_pipeline`, which re-proposes the
/// owner's-graveyard placement as a fresh, replacement-consulted event.
///
/// The inverse of the `state.objects.insert` + `zones::add_to_zone` pair
/// immediately above the CR 303.4f/g consult, and nothing more. In particular it
/// does NOT roll back `state.next_object_id`: the id was drawn by
/// `reserve_liminal_token_object` and is recorded as a high-water mark on every
/// sibling token's CR 733 birth command (`resulting_next_object_id`), so
/// rewinding the allocator would make a later replay reuse a burnt id.
pub(crate) fn uncreate_unentered_aura_token(
    state: &mut GameState,
    object_id: ObjectId,
    owner: PlayerId,
) {
    // The annotation has to sit on the mover line or the line directly above it
    // (`scripts/zone_authority_census.py::census_file`), so the rationale is
    // stated once here and the annotation itself is the one-liner below.
    //
    // This is the un-entry of a token that CR 303.4g or CR 303.4i says was never
    // created, not a CR 400.7 zone change: there is no from-zone, no destination
    // zone, and nothing may observe it — the whole point is that no `ZoneChanged`,
    // `TokenCreated`, or CR 733 birth record is produced for an entry the rules
    // deny. Routing it through `zone_pipeline` would manufacture exactly the
    // observable event this arm exists to suppress.
    // allow-raw-zone: undoes a CR 303.4g/CR 303.4i-denied token entry; not a CR 400.7 zone change, so no ZoneChanged may be emitted.
    zones::remove_from_zone(state, object_id, Zone::Battlefield, owner);
    state.objects.remove(&object_id);
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
    // CR 111.1: the entrant of a `ProposedEvent::TokenEntry` is a TOKEN
    // projection — the marker that represents a permanent no card represents,
    // and that therefore sits in no zone until this entry commits.
    // `state.liminal_entries` also holds the card-backed CR 701.42 meld
    // projection, whose components are real cards in exile and which enters
    // through `ProposedEvent::ZoneChange` from that real prior zone. A
    // `TokenEntry` naming one would name nothing this seam may act on, so the
    // entry is left exactly where it is — the same no-op as an entry that has
    // already been taken, and, unlike a raw graveyard placement, an outcome
    // that puts no object anywhere.
    if !state
        .liminal_entries
        .get(&entry_ref)
        .is_some_and(|entry| entry.object.is_token_projection())
    {
        return true;
    }
    let Some(mut entry) = state.liminal_entries.remove(&entry_ref) else {
        return true;
    };
    let finalization = liminal_token_entry_finalization_action(entry_ref, &entry, entry_events);
    let counters_to_apply: Vec<_> = enter_with_counters
        .iter()
        .chain(entry.enter_with_counters.iter())
        .cloned()
        .collect();
    entry
        .object
        .set_tapped(enter_tapped.resolve(entry.object.projected().tapped));
    let owner = entry.object.projected().owner;

    // CR 733: the settled copy-token birth journals at the single liminal insert
    // seam. `copy_resume` is `Some` for every production liminal entry of kind
    // Token (`token_copy.rs` is the only production constructor), so this covers
    // the whole liminal copy path. Counters, the attacking entry, and later
    // status changes journal through their OWN families — this command is the
    // birth only, exactly as the ordinary CR 111.1 birth is.
    //
    // The command is BUILT here, before `state.objects.insert` consumes
    // `entry.object`, but RECORDED below, after the CR 303.4f/g consult has
    // settled whether this token is created at all. `ResolvedRulesJournal` is
    // append-only — `record_token_creation` has no retraction anywhere in the
    // tree, verified against `append_command`, which only pushes — so a birth
    // recorded for a token CR 303.4g says "isn't created" could never be taken
    // back. Recording after the consult but BEFORE the attach is also what keeps
    // the journal replayable: `apply_resolved_attachment` rejects an attachment
    // whose object does not exist yet, so the birth must own the lower ordinal.
    let birth_command = entry.copy_resume.clone().map(|copy| {
        // CR 707.9b/9c: the liminal seam folds its immediate exceptions into the
        // copiable values BEFORE entry, so the body is complete here and replay
        // can reapply them from this record.
        let modifications = if copy.additional_modifications.is_empty() {
            ResolvedCopyBodyModifications::NoExceptions
        } else {
            ResolvedCopyBodyModifications::Folded {
                modifications: copy.additional_modifications.clone(),
                // Only the CR 614 replacement consult runs between the build and
                // this commit, and it cannot resolve a type-changing effect, so
                // the live list still matches the one the build folded against.
                all_creature_types: state.all_creature_types.clone(),
            }
        };
        ResolvedTokenCreationCommand {
            object: ObjectIncarnationRef::from_object(entry.object.projected()),
            owner,
            entry_timestamp: entry.object.projected().timestamp,
            // CR 302.6: the entered-turn the liminal build already stamped, read
            // back off the object rather than re-read from the live turn.
            entry_turn: entry
                .object
                .projected()
                .entered_battlefield_turn
                .unwrap_or(state.turn_number),
            body: ResolvedTokenBody::Copy {
                copy,
                modifications,
            },
            resulting_tapped: entry.object.projected().tapped,
            // `reserve_liminal_token_object` advanced the allocator to exactly
            // one past this id when it drew it, however many were drawn since.
            resulting_next_object_id: entry_ref.0 + 1,
            cause: state.current_or_begin_rules_execution_node(),
        }
    });

    state
        .objects
        .insert(entry_ref, entry.object.into_projected());
    // allow-raw-zone: liminal token birth has no from-zone move; TokenEntry already consults entry replacements (CR 111.2 + CR 614.12).
    zones::add_to_zone(state, entry_ref, Zone::Battlefield, owner);

    // CR 303.4f: an Aura entering the battlefield by any means other than
    // resolving as an Aura spell, where the effect doesn't specify a host, has
    // its controller choose what it enchants as it enters. A token that is a
    // copy of an Aura (Yenna, Redtooth Regent; Court of Vantress copying a
    // Curse) carries no effect-specified `attach_to` — `entry.attach_to.is_some()`
    // means the effect DID name a host (Role tokens), so CR 303.4f doesn't apply.
    // Mirrors the `attach_to.is_none()` gate on the ZoneChange entry path.
    //
    // Decided before the birth is journaled and applied after, so the CR 303.4g
    // arm can withhold the birth entirely (see `birth_command` above).
    //
    // WHY THE CONSULT RUNS AFTER THE INSERT, and what that costs. The insert +
    // `add_to_zone` pair above emits nothing — no `ZoneChanged`, no
    // `TokenCreated`, no journal command, no `last_created_token_ids` row — and
    // the `NotCreated` arm below rewinds both, so nothing the game can observe
    // escapes on the denied path. It is a prerequisite, not a shortcut:
    // `entering_aura_hosts` reads the entrant's zone off `state.objects` and
    // reports `NotApplicable` for anything not on the battlefield. It also puts
    // this seam in agreement with the OTHER token seam
    // (`token_copy.rs`'s non-liminal loop, which likewise consults a token it has
    // already created on the battlefield).
    //
    // The residual, stated rather than papered over: an enchant filter that
    // COUNTS a population the entrant belongs to ("enchant creature you control if
    // you control two or more enchantments") observes the entrant as present here
    // and as absent at the pre-entry ZoneChange seam in `zone_pipeline`. Only
    // counting predicates diverge — CR 303.4d self-exclusion is applied
    // explicitly by `legal_aura_attachment_targets`, and `FilterProp::Another` is
    // source-relative to the Aura itself, so both already exclude the entrant on
    // either side. No card in the pool carries a counting enchant ability.
    let hosts = if entry.attach_to.is_none() {
        crate::game::zone_pipeline::entering_aura_hosts(state, entry_ref)
    } else {
        crate::game::zone_pipeline::EnteringAuraHosts::NotApplicable
    };

    // CR 303.4g: "If an Aura is entering the battlefield and there is no legal
    // object or player for it to enchant, the Aura remains in its current zone,
    // unless that zone is the stack. In that case, the Aura is put into its
    // owner's graveyard instead of entering the battlefield. If the Aura is a
    // token, it isn't created."
    //
    // The entry is denied — there is no arm that lets the entrant stay on the
    // battlefield unattached for the CR 704.5m state-based action to sweep,
    // because the rule says this entry never happens. Rewound before anything
    // observes it: no CR 733 birth record (`birth_command` is still unrecorded
    // here), no `TokenCreated`, no battlefield `ZoneChanged`, and no
    // `last_created_token_ids` row.
    if matches!(
        &hosts,
        crate::game::zone_pipeline::EnteringAuraHosts::Hosts { legal_targets, .. }
            if legal_targets.is_empty()
    ) {
        // CR 303.4g + CR 111.1: "If the Aura is a token, it isn't created" is the
        // ONLY disposition available here, and it is available by construction:
        // this seam's entrant is a `LiminalEntrant::Token`, whose CR 111.1
        // token-ness is a property of the type rather than an expectation about
        // a flag. The rule's two card-backed dispositions are phrased against
        // the zone the Aura is entering FROM, which is why they belong to — and
        // only exist on — the `ProposedEvent::ZoneChange` path in
        // `zone_pipeline`, where the owner's-graveyard placement is re-proposed
        // as a fresh event so CR 614.6 graveyard→exile redirects (Rest in Peace,
        // Leyline of the Void) still apply to it. Nothing is placed anywhere
        // here, so there is no placement for a replacement to miss.
        uncreate_unentered_aura_token(state, entry_ref, owner);
        // CR 111.1 + CR 603.7: the anaphora slot still has to be republished, or
        // the batch continuation (which reads it back to seed the next token's
        // `created_ids`) would carry whatever an EARLIER, unrelated effect left
        // there. `entry.created_ids` is this batch's list up to but excluding the
        // token that was not created — exactly what `finalize_committed_liminal_
        // token_entry_from_action` would have assigned before appending, minus
        // the append.
        state.last_created_token_ids = entry.created_ids.clone();
        // Not a pause: the batch loop must go on to the next token in the count.
        return true;
    }

    if let Some(command) = birth_command {
        state
            .resolved_rules_journal
            .record_token_creation(command)
            .expect("resolved copy-token creation must have a live journal cause");
    }

    match crate::game::zone_pipeline::apply_entering_aura_hosts(state, entry_ref, hosts) {
        // `NoLegalHost` is unreachable here: the empty-`legal_targets` arm above
        // returned for every entrant, card-backed included.
        crate::game::zone_pipeline::EnteringAuraAttachment::NotApplicable
        | crate::game::zone_pipeline::EnteringAuraAttachment::Attached
        | crate::game::zone_pipeline::EnteringAuraAttachment::NoLegalHost => {}
        crate::game::zone_pipeline::EnteringAuraAttachment::NeedsChoice {
            controller: chooser,
            legal_targets,
        } => {
            // CR 616.1 carrier: park the entry tail exactly like the
            // enter-with-counters pause below, so the finalize step and any
            // remaining batch continuation run after the host is chosen.
            state.last_created_token_ids = entry.created_ids.clone();
            let remaining_counters = counters_to_apply
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
            state.waiting_for = WaitingFor::ReturnAsAuraTarget {
                player: chooser,
                source_id: entry.source_id,
                returned_id: entry_ref,
                legal_targets,
                pending_effect: Box::new(ResolvedAbility::new(
                    Effect::Attach {
                        attachment: TargetFilter::SelfRef,
                        target: TargetFilter::Any,
                    },
                    Vec::new(),
                    entry.source_id,
                    chooser,
                )),
            };
            return false;
        }
    }

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
        created_ids,
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
    // CR 608.2i battlefield-entry bookkeeping is done by `record_zone_change`, reached from the
    // `entry_events` match below (directly on the `Emit` route, via the parked entry's flush on
    // the `Suppress` route) — recording it here too double-counts.
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

    // CR 400.7 + CR 608.2i + CR 614.12a: the entry RECORD and the entry EVENTS are one indivisible
    // operation over one snapshot, and both wait until the object IS the thing that entered.
    // `Emit` means it already is (nothing is deferred on that route). `Suppress` means it is not
    // yet — `BecomeCopy` has not run and any mandatory as-enters choice is unanswered — so the
    // whole entry is PARKED on `GameState` and realized later by
    // `flush_pending_token_battlefield_entry`. Recording here instead would write CR 400.7's "the
    // state at the moment of the move" from a pre-copy 0/0 Shapeshifter.
    match entry_events {
        TokenEntryEventEmission::Emit => {
            push_committed_token_entry_events(state, object_id, name, source_id, events);
        }
        TokenEntryEventEmission::Suppress => {
            // Overwriting a live parked entry would silently lose its CR 400.7 row AND both of its
            // entry events — the precise failure mode this lifecycle exists to remove. A
            // `debug_assert!` alone does not remove it: it compiles out in release, unlike the
            // `pending_liminal_entry_resume` precedent in `engine_replacement.rs`, which returns an
            // `Err` in every profile. So realize the outgoing entry FIRST (data preserved in every
            // profile), and keep the assert as the debug-profile tripwire, because an entry
            // realized here is realized from a snapshot taken at a moment nobody designed for.
            // Exactly one liminal copy entry can be in flight today: the multi-token continuation
            // runs only after `finish_copy_target_choice_entry` returned `Ok(None)`, i.e. after the
            // copy-completion convergence point already flushed. Measured: zero fires across the
            // engine suite.
            let stranded = state
                .pending_token_battlefield_entry
                .as_ref()
                .map(|pending| pending.object_id);
            if let Some(stranded_id) = stranded {
                flush_pending_token_battlefield_entry(state, stranded_id, events);
            }
            debug_assert!(
                stranded.is_none(),
                "CR 400.7: parking a token battlefield entry over a live pending one: {stranded:?}"
            );
            state.pending_token_battlefield_entry = Some(PendingTokenBattlefieldEntry {
                object_id,
                name,
                source_id,
            });
        }
    }
    if matches!(sacrifice_at, Some(Duration::UntilEndOfCombat)) {
        let sacrifice_token = DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase {
                phase: Phase::EndCombat,
            },
            ability: Box::new(ResolvedAbility::new(
                Effect::Sacrifice {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 0,
                },
                vec![TargetRef::Object(object_id)],
                source_id,
                controller,
            )),
            controller,
            source_id,
            one_shot: true,
            provenance: crate::types::identifiers::DelayedInstallIdentity::LegacyDelayed,
        };
        crate::game::triggers::install_delayed_trigger(state, sacrifice_token, events);
    }

    state.last_created_token_ids = created_ids;
    // Publishing the anaphora slot goes through the guarded authority, so a token that vanished
    // during the counter pause is not named by `TargetFilter::LastCreated` on the same route that
    // withheld its `TokenCreated` and wrote no `created_tokens_this_turn` row. Appending after the
    // assignment is the same position `created_ids.push(object_id)` produced.
    record_last_created_token(state, object_id);

    true
}

/// CR 603.6a + CR 400.7 + CR 111.1: emit a token's battlefield-entry pair — the CR 400.7 zone
/// change and the CR 111.1 token creation.
///
/// The zone-change half is delegated to
/// [`crate::game::zones::record_and_emit_entry_from_no_zone`], the single authority for a
/// `from: None → Battlefield` entry: it records the row through
/// `restrictions::record_zone_change` (which assigns this turn's zone-change index and performs
/// the CR 608.2i battlefield-entry bookkeeping) and emits the `ZoneChanged` carrying that index.
/// This function adds only the token-specific `TokenCreated` (CR 111.1), which the authority must
/// not emit because conjured cards route through it too.
///
/// The index matters: `GameObject::snapshot_for_zone_change` leaves
/// `turn_zone_change_index` at its `0` placeholder for the recorder to overwrite, and the
/// CR 603.2c batched zone-change replay guard (`triggers.rs`) dedups on
/// `(definition_ref, turn_zone_change_index)`. A token entry that never reached the recorder
/// therefore shipped index `0` on the wire, so a SECOND same-turn token batch collided with the
/// first and its batched trigger fire was swallowed.
///
/// Callers must NOT also call `record_battlefield_entry` — the authority's `record_zone_change`
/// does it, and a second call double-counts `battlefield_entries_this_turn`.
///
/// This is the `TokenEntryEventEmission::Emit` half of the lifecycle: the object is already fully
/// realized when the finalize tail runs, so record and emit happen inline. The `Suppress` half
/// parks the entry and realizes it through [`flush_pending_token_battlefield_entry`], which routes
/// back through this same function.
///
/// THE INVARIANT, enforced here rather than at call sites: **`TokenCreated` is emitted if and only
/// if the authority recorded the entry.** `record_and_emit_entry_from_no_zone` returns `None`
/// exactly when `state.objects` has no row for `object_id` (its `?` is on that one lookup and
/// nothing else — `snapshot_for_zone_change` and `record_zone_change` both return non-`Option`
/// values), so `record.is_some()` IS the object-existence predicate, read off the authority's own
/// verdict instead of a duplicated `contains_key`.
///
/// The third token-creation ledger, `last_created_token_ids`, carries the SAME predicate through
/// [`record_last_created_token`] rather than through this function — see that doc for why the write
/// cannot be folded in here without widening the `TargetFilter::LastCreated` slot to callers that
/// deliberately do not claim it.
///
/// Why the predicate lives HERE. `restrictions::record_token_created` — which populates
/// `created_tokens_this_turn` and `players_who_created_token_this_turn` — is itself
/// existence-guarded, so an unconditional emit puts a live trigger event
/// (`trigger_matchers::match_token_created`, keyed in `trigger_index`) on the wire with no ledger
/// row behind it. The damage is a WRONG TRIGGER FIRE, not merely a self-inconsistent read:
/// `match_token_created` applies its CR 111.2 controller filter only inside
/// `if let Some(token_controller) = state.objects.get(object_id)`, and `valid_card_matches`
/// short-circuits `None => true` without reading `state`, so with the object gone the filter is
/// never applied and the matcher returns `true` for a controller it should have rejected.
/// MEASURED: `valid_card=None, valid_target=Controller` → present `false`, gone `true`; the
/// `valid_card=Typed{Creature}` row stays `false` on the gone path and is the negative control.
///
/// Guarding each CALLER instead was tried and abandoned: three successive enumerations of "the
/// routes where a pause can separate creation from emit" each shipped incomplete (`counters.rs`
/// x2 → then `token_copy.rs` → then the `TokenEntryEventEmission::Emit` arm reached via
/// `PendingCounterPostAction::FinalizeCommittedLiminalTokenEntry`), with
/// [`flush_pending_token_battlefield_entry`] disclosed-but-unfixed the whole time. The eight
/// callers are `apply_create_token_after_replacement_with_created_ids`, `gift_delivery.rs`,
/// `token_copy.rs` x2, `counters.rs` x2, the `Emit` arm, and that flush; this is the ONLY
/// production emit of `GameEvent::TokenCreated`, so one predicate inside it closes the class by
/// construction instead of by enumeration. See
/// `a_vanished_counter_paused_token_reports_neither_creation_event_nor_ledger_row`, which drives
/// all five deferred routes.
///
/// KNOWN CLASS-WIDE FIX, deliberately NOT made here: `match_token_created` should resolve the
/// controller from last-known information the way its sibling `valid_card_matches_with_lki` already
/// does for the card filter. That is a change to a shared matcher affecting every gone-object
/// event, so it is a follow-up rather than part of this authority fold.
///
/// NO CR settles whether a token that never successfully entered should fire a creation trigger,
/// because the situation is unreachable in rules terms: CR 704.3 checks state-based actions only
/// "whenever a player would get priority", so nothing can remove the token between its creation and
/// the CR 603.6a enters-the-battlefield check. The gone arm is a defensive engine artifact of
/// deferring the emit past a replacement pause, and the requirement on it is internal agreement,
/// not a rules verdict.
///
/// Returns the recorded row (index assigned) so a caller whose object is a just-created invariant
/// can `.expect(…)` it. The return is load-bearing through the `.expect(…)` at `gift_delivery.rs`
/// and `token_copy.rs`'s uninterrupted copy path only; every other caller discards it. Those two
/// panic on `None` exactly as before — the guard changes only whether an (unobservable, because the
/// unwinding drops `events` and no engine boundary catches it) `TokenCreated` was pushed first.
pub(crate) fn push_committed_token_entry_events(
    state: &mut GameState,
    object_id: ObjectId,
    name: String,
    source_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Option<crate::types::game_state::ZoneChangeRecord> {
    let record = crate::game::zones::record_and_emit_entry_from_no_zone(state, object_id, events);
    if record.is_some() {
        events.push(GameEvent::TokenCreated {
            object_id,
            name,
            source_id,
        });
    }
    record
}

/// CR 111.1: Publish a just-created token into `state.last_created_token_ids`, the THIRD
/// token-creation ledger, under the same object-existence predicate as the other two.
///
/// THE LEDGER TRIPLE, and why this exists. A token creation writes three places, and until this
/// function they did not agree on the object-gone path:
///
/// 1. `created_tokens_this_turn`            — guarded, inside `restrictions::record_token_created`
/// 2. `players_who_created_token_this_turn` — guarded, same function
/// 3. `last_created_token_ids`              — UNGUARDED
///
/// THE WRITER POPULATION, NAMED BY THE QUERY THAT PRODUCED IT AND BY THE TIP IT WAS RUN AT — a
/// count with no command behind it is unfalsifiable, and this ledger has now been mis-swept twice
/// from a too-narrow grep. Counts below are MATCH EVENTS
/// (`rg --pcre2 -U --json … | grep -c '"type":"match"'`), not lines, because two of the buffer
/// writes span three lines each. With
/// `M=(push|extend|clear|insert|append|retain|splice|truncate|remove|drain|resize|pop)`:
///
/// * `rg -n --pcre2 -U "state\s*\.\s*last_created_token_ids\s*(\.\s*M\s*\(|=[^=])" crates/engine/src`
///   → 39 at THIS tip, of which 3 are prose in comments this change added (including the one four
///   lines below), leaving 36 code hits; plus 1 more bound as `s.` (`engine.rs`'s per-turn
///   `.clear()`) = 37 = 20 production writers and 17 inside `#[cfg(test)]`. Exactly ONE production
///   writer publishes a single just-created id — the `push` in this function. The other 19 are
///   clears (4) and bulk republishes (15) of a vector that is itself a clone of this ledger, a
///   `CopyTokenApplyStatus` built one line after `.expect("token just created")`, or the copy-batch
///   buffer below. AT `4b34e5465` the same query returned 39 hits with NO comment among them, for
///   23 production writers, of which 5 published a single just-created id.
/// * `rg -n --pcre2 -U "pending\s*\.\s*created_ids\s*(\.\s*M\s*\(|=[^=])" crates/engine/src`
///   → 11 at THIS tip, of which 1 is prose in a comment this change added (`counters.rs`'s test
///   doc), leaving 10 production writers of `PendingCopyTokenResolution::created_ids`, the
///   copy-batch buffer that `token_copy.rs`'s drain assigns WHOLESALE back onto this ledger, and 0
///   inside `#[cfg(test)]`. Exactly ONE is a single-id publish — the `push` in
///   [`record_last_created_copy_batch_token`]. AT `4b34e5465` the same query returned 11 with no
///   comment among them, for 11 production writers, of which TWO were single-id publishes; both are
///   now that one call.
///
/// THOSE TOTALS ARE POSITIVE CONTROLS, NOT INVARIANTS, and they churn in a specific way worth
/// naming: the query matches its own documentation, so writing prose ABOUT this ledger moves the
/// number. Both raw totals were already stale when the previous revision quoted them, invalidated
/// by the very comments that quoted them. What does NOT churn is the classification — one single-id
/// publish per container, each inside an authority — and that half is enforced executably by
/// `battlefield_entry_authority_census`'s THIRD anchor
/// (`every_single_id_anaphora_publish_lives_in_an_authority`), which pins the production multiset to
/// `{effects/token.rs: 2}` and both hits' enclosing functions to these two. Prefer running that test
/// over trusting the numbers above.
///
/// The `-U` is load-bearing, not decoration: `token_copy.rs:321-323` and `:327-329` write
/// `pending` / `.created_ids` / `.extend(…)` across three lines, so a line-oriented grep reports 9
/// match events where there are 11 — at BOTH tips. Deriving this population from
/// `grep 'last_created_token_ids.push('` is how round 8 found 4 of 5 sites and round 9 left the two
/// buffer siblings unguarded.
///
/// Gating the `GameEvent::TokenCreated` emit in [`push_committed_token_entry_events`] made the
/// event agree with ledgers 1 and 2, which FLIPPED which ledger it disagreed with rather than
/// removing the disagreement: on a deferred route whose token vanished during the pause, the event
/// was withheld and both turn ledgers stayed empty while ledger 3 still held the dead id.
///
/// Ledger 3 is not inert bookkeeping. It is the `TargetFilter::LastCreated` anaphora slot —
/// `game/filter.rs`'s `LastCreated => state.last_created_token_ids.contains(&object_id)` and
/// `game/targeting.rs`'s `LastCreated => state.last_created_token_ids.clone()` — so a dead id in it
/// is a "the token you created" reference pointing at an object that never finished entering.
///
/// WHY NOT FOLD IT INTO `restrictions::record_token_created`, which is where the other two live:
/// MEASURED, its production call sites are a strict SUPERSET of ledger 3's (it additionally runs at
/// `incubate.rs`, `gift_delivery.rs`, `token.rs` x2, `token_copy.rs` and `counters.rs`'s
/// `InjectPredefinedTokenAbilities` arms, none of which publishes the anaphora slot), so folding
/// would silently widen `LastCreated` to routes that deliberately do not claim it. The predicate is
/// therefore single-sourced here while the call-site set stays exactly what it was.
///
/// Not folded into [`push_committed_token_entry_events`] either, for the mirror reason: three of
/// that emitter's eight callers do not publish ledger 3, so pulling the write inside would widen
/// the slot the same way.
///
/// Returns whether the id was published, so the copy-batch mirror in
/// [`record_last_created_copy_batch_token`] can consume the SAME verdict instead of re-deriving it.
pub(crate) fn record_last_created_token(state: &mut GameState, object_id: ObjectId) -> bool {
    let exists = state.objects.contains_key(&object_id);
    if exists {
        state.last_created_token_ids.push(object_id);
    }
    exists
}

/// CR 111.1 + CR 707.2: publish a just-created token into BOTH destinations the
/// anaphora slot has while a copy batch is in flight — [`record_last_created_token`]'s ledger 3 and
/// the in-flight `PendingCopyTokenResolution::created_ids` buffer — under ONE evaluation of the
/// object-existence predicate.
///
/// WHY THIS EXISTS AS A FUNCTION rather than two adjacent statements. `token_copy.rs`'s drain ends
/// with `state.last_created_token_ids = pending.created_ids;` — an ASSIGNMENT, not an append. So
/// the buffer is not a secondary cache of ledger 3; it OVERWRITES it. A caller that guarded the
/// ledger write and then pushed the same id into the buffer one line below published the withheld
/// id anyway and destroyed the guarded list on top of it. That is exactly what shipped at
/// `counters.rs` and `token_copy.rs` after the guard was introduced: the predicate was single-
/// sourced but the *publish* was not, so the guard was defeated one line below itself. Fusing both
/// writes into one call leaves no second statement to forget.
///
/// NOT folded into [`record_last_created_token`] itself, and the distinction is behavioural rather
/// than stylistic: its other three callers (`counters.rs`'s `FinalizeTokenEntry` arm and
/// `EmitCommittedCopyTokenEntry` arm, and `finalize_committed_liminal_token_entry_from_action`)
/// deliberately do NOT mirror. Mirroring there would add a plain (or already-batched) token to a
/// copy batch's `created_ids`, and since that buffer is assigned wholesale onto ledger 3 at the
/// drain, it would silently widen what `TargetFilter::LastCreated` names for "the tokens created
/// this way" — the same widening argument that keeps the predicate out of
/// `restrictions::record_token_created`.
pub(crate) fn record_last_created_copy_batch_token(state: &mut GameState, object_id: ObjectId) {
    if !record_last_created_token(state, object_id) {
        return;
    }
    if let Some(pending) = state.active_copy_token_mut() {
        pending.created_ids.push(object_id);
    }
}

/// CR 400.7 + CR 608.2i + CR 614.12a: realize a postponed token battlefield entry — record it
/// through `record_zone_change` and emit its entry pair — at the first instant the object IS the
/// thing that entered. Record and emit are ONE indivisible operation over ONE owned value, so no
/// route can perform half of it. Returns `false` when no entry is parked for `object_id`.
///
/// Idempotence is structural: [`Option::take_if`] consumes the parked value, so a second call for
/// the same object is a no-op and the duplicate-row class is unrepresentable rather than guarded.
///
/// LOOK-BACK WINDOW (owned, not hidden): between the commit and this flush the token is on the
/// battlefield with ZERO rows on either CR 400.7 / CR 608.2i ledger, and on a paused route that
/// window spans one or more client round-trips. `game/quantity.rs`'s zone-change scans and
/// `restrictions::battlefield_entry_matches_filter` therefore answer "0 entered this turn" for it
/// during the window. That is inherent to postponing, and it is the lesser error: recording early
/// answers "1" with the WRONG object (a 0/0 pre-copy Shapeshifter), which silently mis-answers
/// "each Zombie that entered this turn" rather than under-counting an entry that, per CR 614.12a,
/// has not finished happening.
///
/// SBA SCOPE — what the rules do and do NOT guarantee about the window. CR 704.3 checks
/// state-based actions only when a player would get priority, and CR 704.4 says they pay no
/// attention to what happens during the resolution of a spell or ability, so nothing can remove the
/// token while the entry is PAUSED on a replacement/choice prompt. Neither rule covers the action
/// that finally settles: that action runs its own SBA pass inside `run_post_action_pipeline`, with
/// the entry still parked. That is exactly why [`realize_settled_token_battlefield_entry`] is
/// called from inside `apply_action` BEFORE that pipeline — a copy realized with toughness 0 gets
/// its CR 400.7 row written and its pair emitted before CR 704.5f can bury it.
/// [`crate::game::zones::record_and_emit_entry_from_no_zone`]'s `None` arm remains the fail-safe
/// for an object that is gone by flush time: it records nothing and emits nothing, and
/// [`push_committed_token_entry_events`] now withholds `TokenCreated` on that same verdict, so this
/// route reports NOTHING rather than a creation event with no ledger row behind it.
pub(crate) fn flush_pending_token_battlefield_entry(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> bool {
    let Some(pending) = state
        .pending_token_battlefield_entry
        .take_if(|pending| pending.object_id == object_id)
    else {
        return false;
    };
    push_committed_token_entry_events(
        state,
        pending.object_id,
        pending.name,
        pending.source_id,
        events,
    );
    true
}

/// CR 400.7 + CR 603.6a: realize a parked token battlefield entry once the action carrying it has
/// SETTLED — `WaitingFor::Priority`, the complement of "any pause", so the gate is pause-shape
/// agnostic by construction instead of enumerating prompt variants.
///
/// ONE gate, TWO call sites in `engine.rs`, both settled-action convergence points:
///
/// * inside `apply_action`, immediately before `engine_priority::run_post_action_pipeline` — so the
///   entry pair is in the event set that action's CR 603.2 / CR 603.6a trigger scan reads. This is
///   what makes the copy token's ETB observers ("whenever another creature enters") fire, and it
///   also puts the CR 400.7 row on the ledger before that pipeline's SBA pass (CR 704.3) can bury a
///   0-toughness copy under CR 704.5f.
/// * in `apply_action_boundary_core`, after `apply_action` returned — for the handlers that build
///   an `ActionResult` straight out of the reducer match and never reach that pipeline
///   (`handle_tribute_choice` is the reachable one). That call site converges them onto
///   `engine_priority::run_post_action_pipeline_from` over exactly the slice this realization
///   appended, so the CR 603.6a check runs for them too and their ETB observers fire. For the
///   REALIZED ENTRY the only remaining difference from the in-`apply_action` call is ordering
///   against that action's CR 704.3 SBA pass, which is why both call sites are kept; the handler's
///   OWN earlier events stay outside that scan window by design (`scan_from`).
///
/// Order between the two is irrelevant: the flush's `Option::take_if` makes the second call — and
/// any call after the two in-resolution convergence points in `engine_replacement.rs` /
/// `counters.rs` — a no-op.
///
/// CR 704.5f: when the token is no longer on the battlefield at the settling point, the parked
/// entry is DROPPED — no row, no pair — rather than emitting a battlefield-entry event for an
/// object that is not there, which would make ETB triggers fire for a permanent that has already
/// left. The cost is a lost CR 400.7 row for an entry that did happen. After the in-`apply_action`
/// call above, the only way to reach this branch is a settling action that never runs the pipeline
/// AND removes the token within itself; no production route is known to do both.
///
/// Returns whether an entry pair was actually appended to `events` — `false` for an unsettled
/// action, for nothing parked, for an entry an earlier convergence point already consumed, and
/// for the CR 704.5f drop branch (which does consume the park but emits nothing). The boundary
/// call site gates its CR 603.6a trigger pass on exactly that.
pub(crate) fn realize_settled_token_battlefield_entry(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> bool {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return false;
    }
    let Some(pending_id) = state
        .pending_token_battlefield_entry
        .as_ref()
        .map(|pending| pending.object_id)
    else {
        return false;
    };
    if state.battlefield.contains(&pending_id) {
        flush_pending_token_battlefield_entry(state, pending_id, events)
    } else {
        state.pending_token_battlefield_entry = None;
        false
    }
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
        // no host attachment mutation and no CR 303.4i entry verdict to reach
        && !spec.attach_to.is_requested()
}

/// CR 603.6a + CR 111.1: The set of event keys a single produced token EMITS as
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
    // CR 111.1 ("Some effects put tokens onto the battlefield"): a token's
    // creation also emits `TokenCreated`. NOT CR 111.10, which is the
    // predefined-token characteristics catalog (Treasure/Food/Clue/Role) and
    // says nothing about event emission — the ~58 other CR 111.10 citations in
    // this file are correct for exactly that catalog.
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
    // Delegates to the ONE prompt-cause authority. Term for term HEAD's two
    // disjuncts: `OptionalCandidate` is the `any_optional` scan (with an
    // unresolvable def — every virtual — conservatively optional) and
    // `OrderingMaterial` is the `len() >= 2 && ordering_is_material` conjunct.
    //
    // `MandatoryBodyContinuation` is deliberately NOT read here. A drained body
    // can set a non-priority `waiting_for`, so taking it would be a real
    // token-batching change; it is left to its own change rather than smuggled
    // into this delegation.
    let causes = replacement::proposed_event_prompt_cause(state, &proposed, registry);
    causes.contains(replacement::ReplacementPromptCause::OptionalCandidate)
        || causes.contains(replacement::ReplacementPromptCause::OrderingMaterial)
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
        attach_to: TokenHostRequest::NotRequested,
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
    let host_request = TokenHostRequest::from_binding(
        attach_to.is_some(),
        attach_to
            .as_ref()
            .and_then(|f| resolve_attach_host(state, ability, f)),
    );

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
        host_request,
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
/// graveyard. That observed path is itself a divergence from CR 303.4i, which
/// says an Aura token whose host is undefined is not created at all; the missing
/// guard is tracked separately (#7302). Binding the host correctly is what keeps
/// a card off that path, not a substitute for the guard.
///
/// This does NOT duplicate attach legality: the actual attach is performed by
/// `attach::attach_to` / `attach::attach_to_player`, the single authority for
/// CR 701.3a / CR 301.5 / CR 303.4 host validity.
fn resolve_attach_host(
    state: &GameState,
    ability: &ResolvedAbility,
    filter: &TargetFilter,
) -> Option<AttachTarget> {
    match classify_attach_host_authority(filter) {
        // CR 115.1a: the chosen OBJECT target carried in `ability.targets` — the
        // single-target "attached to target creature" case. A player-valued
        // slot never reaches this arm; `denotes_player_target` routes it to
        // `SelectedPlayerTarget` below.
        AttachHostAuthority::SelectedTarget => first_object_host(ability),
        // CR 115.1a + CR 303.4: the chosen PLAYER target is the host. Curse
        // Auras (Selenia's Curse) are the shipped shape; `attach_to_player`
        // downstream carries the CR 303.4i legality gate, exactly as
        // `attach_to` does for an object host.
        AttachHostAuthority::SelectedPlayerTarget => first_player_host(ability),
        // CR 608.2c + CR 109.4: the resolution-chosen player, read from the
        // resolution's own chosen-player list.
        //
        // Read from the slot directly rather than through
        // `resolve_player_for_context_ref`: that helper falls back to
        // `ability.controller` when the index is unbound, which is right for a
        // sub-effect that must still act ("the chosen player draws a card") and
        // wrong here. An unbound slot means the sentence names nobody, and this
        // path may not invent a host — inventing one is the whole defect this
        // resolver exists to prevent. No host, and CR 704.5m takes it from there.
        AttachHostAuthority::ChosenPlayer(index) => ability
            .chosen_players
            .get(index as usize)
            .copied()
            .map(AttachTarget::Player),
        // Event-context hosts ("attached to the triggering creature") resolve the
        // triggering event's subject via the shared event-context resolver.
        AttachHostAuthority::EventContext => {
            crate::game::targeting::resolve_event_context_target(state, filter, ability.source_id)
                .map(target_ref_to_attach_target)
        }
        // The bare-pronoun host ("attached to it"). It normally reads the chosen
        // target out of `ability.targets` — the for-each rebind binds it per
        // iteration — but the pronoun also appears in abilities that choose no
        // target at all, where there is no back-reference for it to make.
        //
        // CR 608.2k: such a pronoun names the specific untargeted object the
        // ability's trigger condition already referred to ("When this creature
        // enters, create a Monster Role token attached to IT").
        //
        // `ParentTarget` IS that anaphor, and `targeting::resolved_targets` is
        // its authority — so the fallback asks it rather than substituting a
        // neighbouring one. It carries referents this clause has no business
        // re-deriving: the attack batch, the cast spell, the blocked attacker,
        // and the Stationed / VehicleCrewed / Saddled subjects (CR 702.184a,
        // CR 702.122, CR 702.171). On a zone change it hands back the ENTERING
        // object only when that is not the source — Gylwain, Casting Director
        // creates the Role for another creature that entered — and otherwise
        // falls back to the source, which is what "When THIS creature enters …
        // attached to it" needs. Resolving `TriggeringSource` here happened to
        // agree on both zone-change shapes and on nothing else.
        //
        // The fallback is confined to this arm and to an ability that chose
        // NOTHING. A typed targeting filter that legally selected zero targets
        // ("attached to target creature you control" with no legal target) keeps
        // its own no-host outcome: nothing in its text names an untargeted
        // object, so CR 608.2k does not reach it.
        //
        // One host is taken from what may be a batch: the clause creates one
        // token and its pronoun names one thing.
        AttachHostAuthority::Pronoun => first_object_host(ability).or_else(|| {
            ability.targets.is_empty().then(|| {
                crate::game::targeting::resolved_targets(
                    ability,
                    &TargetFilter::ParentTarget,
                    state,
                )
                .into_iter()
                .next()
                .map(target_ref_to_attach_target)
            })?
        }),
        // CR 608.2c: a numbered anaphor resolves against the whole resolving
        // chain's targets, which is why it routes through the same authority
        // `attach::resolve_object_filter` uses rather than reading this clause's
        // nearest target.
        AttachHostAuthority::ParentSlot(index) => {
            crate::game::targeting::resolve_parent_slot_from_root(state, ability, index)
                .map(target_ref_to_attach_target)
        }
        AttachHostAuthority::Source => Some(AttachTarget::Object(ability.source_id)),
        AttachHostAuthority::SpecificObject(id) => Some(AttachTarget::Object(id)),
        AttachHostAuthority::NoHost => None,
    }
}

/// CR 303.4 + CR 608.2c: which authority names the host of a token created
/// "attached to" something.
///
/// Reading the enclosing ability's chosen targets is correct only for a filter
/// that describes a target slot (CR 115.1a). Every other family names its object
/// through its own authority, and a filter that names no object must leave the
/// token hostless rather than inherit whatever the ability happened to select.
enum AttachHostAuthority {
    /// A predicate over objects, which the targeting layer used to choose a
    /// target. The host is that chosen target.
    SelectedTarget,
    /// CR 115.1a + CR 303.4: a target slot that holds a PLAYER, not an object
    /// ("… attached to target opponent"). The host is that chosen player.
    SelectedPlayerTarget,
    /// CR 608.2c + CR 109.4: the Nth resolution-chosen player. Fixed while the
    /// ability resolves, never declared as a target — so it names its player
    /// through the chosen-player list, not through `ability.targets`.
    ChosenPlayer(u8),
    /// An object the triggering event or the resolution context names.
    EventContext,
    /// The bare anaphoric pronoun, which reads the chosen target and otherwise
    /// falls back to the untargeted object the trigger condition named.
    Pronoun,
    /// One numbered slot of the resolving chain's accumulated targets.
    ParentSlot(usize),
    /// The ability's own source object.
    Source,
    /// An object the ability definition names outright.
    SpecificObject(ObjectId),
    /// No host from this path: a player-valued filter, a filter that names no
    /// object at all, or a reference family whose authority this path does not
    /// resolve. The token is then left unattached (see the `resolve_attach_host`
    /// doc comment for what happens to it).
    NoHost,
}

/// The classification is exhaustive over [`TargetFilter`] on purpose: a new
/// variant has to be triaged here rather than inheriting selected-target
/// semantics from a wildcard.
fn classify_attach_host_authority(filter: &TargetFilter) -> AttachHostAuthority {
    let authority = match filter {
        // CR 608.2c + CR 109.4: a reference to the resolution-chosen player is a
        // `Typed` filter BY SHAPE, but it is a context reference — the engine
        // says so through `chosen_player_index`, which is what
        // `is_context_ref` itself consults. Asked ahead of the generic `Typed`
        // arm below, which would otherwise read the ability's chosen targets and
        // attach the token to an unrelated object.
        TargetFilter::Typed(_) if filter.chosen_player_index().is_some() => {
            AttachHostAuthority::ChosenPlayer(
                filter
                    .chosen_player_index()
                    .expect("guarded by the arm above"),
            )
        }
        // CR 115.1a: whether a target slot holds a player or an object is the
        // targeting layer's question, and `denotes_player_target` is the single
        // authority both it and this classification read. "… attached to target
        // opponent" (Selenia, the Cursed Heart) parses to the property-free
        // `Typed` shape, so without this arm its Curse would look for an object
        // target, find none, and enter unattached.
        TargetFilter::Typed(_) if filter.denotes_player_target() => {
            AttachHostAuthority::SelectedPlayerTarget
        }

        // CR 601.3 + CR 608.2c: a composite can CONTAIN a context reference —
        // the parser builds `And { ExiledBySource, Typed }` for "an exiled card
        // that is a creature" — and `is_context_ref` reports the whole filter as
        // one. Its object comes from the exile link, not from a target slot, so
        // it fails closed here rather than reading `ability.targets`. Asked
        // before the object-predicate arm below, which would otherwise claim the
        // composite by its outer shape.
        TargetFilter::And { .. } | TargetFilter::Or { .. } | TargetFilter::Not { .. }
            if filter.is_context_ref() =>
        {
            AttachHostAuthority::NoHost
        }

        // Predicates over objects — what a target slot is chosen with.
        TargetFilter::Any
        | TargetFilter::Typed(_)
        | TargetFilter::Not { .. }
        | TargetFilter::Or { .. }
        | TargetFilter::And { .. }
        | TargetFilter::Named { .. }
        | TargetFilter::HasChosenName
        | TargetFilter::StackSpell
        | TargetFilter::StackAbility { .. } => AttachHostAuthority::SelectedTarget,

        // CR 603.7c + CR 608.2c: event- and resolution-context references.
        TargetFilter::EventTarget
        | TargetFilter::LastCreated
        | TargetFilter::LastRevealed
        | TargetFilter::LastZoneChanged
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::TriggeringSource
        | TargetFilter::AttachedTo => AttachHostAuthority::EventContext,

        TargetFilter::ParentTarget => AttachHostAuthority::Pronoun,
        TargetFilter::ParentTargetSlot { index } => AttachHostAuthority::ParentSlot(*index),
        TargetFilter::SelfRef => AttachHostAuthority::Source,
        TargetFilter::SpecificObject { id } => AttachHostAuthority::SpecificObject(*id),

        // CR 115.1a: the remaining player-valued TARGET SLOTS, which
        // `denotes_player_target` also claims. Kept as their own arm rather than
        // folded into a guard so the variant list stays readable, and asserted
        // to agree with that authority in `attach_host_authority_tests`.
        TargetFilter::Player | TargetFilter::SpecificPlayer { .. } => {
            AttachHostAuthority::SelectedPlayerTarget
        }

        // Player-valued filters that are NOT target slots. CR 303.4 permits a
        // player host, but each of these names its player through a context
        // authority this path does not resolve, so they fail closed rather than
        // guess one.
        TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::ControllerAndControlledPermanents { .. }
        | TargetFilter::Opponent
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::PlayerMatching { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSourceController
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTarget
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => AttachHostAuthority::NoHost,

        // Object references this path does not resolve. Each names its object
        // through an authority of its own (an exile link, a tracked set, a
        // recorded choice, a paid cost); none of them is the enclosing ability's
        // selected target, so an unsupported one yields no host instead.
        // `OriginalSource` never survives to resolution — it is concretized to
        // `SpecificObject` beforehand.
        // CR 702.95b: `SourceOrPaired` names the source AND the creature it is
        // paired with — two objects, not one host — and `is_context_ref` already
        // classifies it as an automatic context reference rather than a target
        // slot. It fails closed here until a host authority for the pair exists.
        TargetFilter::SourceOrPaired
        | TargetFilter::None
        | TargetFilter::GrantingObject
        | TargetFilter::CostPaidObject
        | TargetFilter::ChosenCard
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::OriginalSource => AttachHostAuthority::NoHost,
    };

    // CR 115.1a: the two authorities above are the engine's, not this function's,
    // so the classification is checked against them rather than against a
    // hand-kept list of filters — every filter the engine classifies anywhere is
    // covered, including shapes nobody thought to write down. `is_context_ref`
    // says a filter surfaces no target slot; `denotes_player_target` says the
    // slot holds a player. Either one rules out reading the ability's chosen
    // OBJECT targets.
    debug_assert!(
        !(matches!(authority, AttachHostAuthority::SelectedTarget)
            && (filter.is_context_ref() || filter.denotes_player_target())),
        "{filter:?} is not an object target slot, so it must not inherit the ability's \
         chosen object targets as its attachment host"
    );
    authority
}

/// The first object target the ability chose, which is the host every targeting
/// attachment filter reads. Mirrors `attach::resolve_object_filter`'s
/// ParentTarget arm.
fn first_object_host(ability: &ResolvedAbility) -> Option<AttachTarget> {
    ability.targets.iter().find_map(|target| match target {
        TargetRef::Object(id) => Some(AttachTarget::Object(*id)),
        TargetRef::Player(_) => None,
    })
}

/// The first player target the ability chose — the mirror of
/// [`first_object_host`] for a host filter whose slot holds a player
/// (CR 115.1a). Object slots are skipped rather than converted: an ability can
/// carry both ("tap target creature, then create a Curse attached to target
/// opponent"), and the object slot is the other clause's, not this one's.
fn first_player_host(ability: &ResolvedAbility) -> Option<AttachTarget> {
    ability.targets.iter().find_map(|target| match target {
        TargetRef::Player(id) => Some(AttachTarget::Player(*id)),
        TargetRef::Object(_) => None,
    })
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
            position: crate::types::ability::LibraryPosition::Top,
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
            scope: crate::types::ability::EffectScope::Single,
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
        printed_loyalty: None,
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
        // Built in code from CR 111.10i, not parsed from printed text, so there is
        // no parse to have gone wrong.
        parse_warnings: vec![],
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
        // CR 111.3: A token's abilities are defined as it is created, so these
        // entries are printed slots of the token's own base set — not grants.
        // They must carry a real `Printed` occurrence ref: pushing the bare
        // `TriggerDefinition` would go through `From<TriggerDefinition>` and
        // stamp `TriggerDefinitionOccurrenceRef::Unmaterialized`, which
        // `validate_trigger_definitions` rejects from an observable state and
        // which `#[serde(skip_serializing)]` turns into a hard serialization
        // failure the moment the state crosses the WASM bridge.
        for trigger in materialized.trigger_definitions {
            obj.push_printed_trigger(trigger);
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
    card_name: &str,
) -> (
    Vec<StaticDefinition>,
    Vec<ContinuousModification>,
    Vec<String>,
) {
    // CR 201.5 + CR 201.5a: A card's Oracle text uses its name to refer to
    // itself, and a granted ability that refers to its granter by name refers
    // only to that specific granter. Token catalog rules text is parsed
    // independently of `parse_oracle_ir`'s single entry point, so it needs its
    // own `normalize_card_name_refs` pass here, mirroring `parse_oracle_ir`'s
    // call in `oracle.rs`.
    let rules_text = crate::parser::oracle_util::normalize_card_name_refs(rules_text, card_name);
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
    // CR 201.5a: catch any residual `GRANTING_SELF_PLACEHOLDER` left in the
    // parsed statics'/modifications' display `description`s, mirroring
    // `scrub_granting_placeholder_descriptions`'s whole-tree sweep in
    // `oracle.rs` for this independent parse entry point.
    for def in &mut static_definitions {
        crate::parser::oracle::scrub_static_descriptions(def);
    }
    for modification in &mut modifications {
        crate::parser::oracle::scrub_modification_descriptions(modification);
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

    // ── CR 608.2i: the entry RECORD is not gated on event emission ────────

    /// CR 400.7 + CR 608.2i rows for `object_id`, as `(battlefield_entry_rows, zone_change_rows)`.
    fn ledger_rows(state: &GameState, object_id: ObjectId) -> (usize, usize) {
        (
            state
                .battlefield_entries_this_turn
                .iter()
                .filter(|record| record.object_id == object_id)
                .count(),
            state
                .zone_changes_this_turn
                .iter()
                .filter(|record| {
                    record.object_id == object_id && record.to_zone == Zone::Battlefield
                })
                .count(),
        )
    }

    /// Build a battlefield token and run the liminal finalize tail over it under `emission`,
    /// returning the resulting `(state, token_id, emitted_events)` so callers can inspect the
    /// ledgers, the parked entry, and any later flush.
    fn finalize_liminal_entry_under(
        emission: TokenEntryEventEmission,
    ) -> (GameState, ObjectId, Vec<GameEvent>) {
        let mut state = GameState::new_two_player(42);
        let controller = PlayerId(0);
        let source_id = ObjectId(1);
        let object_id = create_object(
            &mut state,
            CardId(0),
            controller,
            "Record Probe".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        assert!(finalize_committed_liminal_token_entry_from_action(
            &mut state,
            PendingCounterPostAction::FinalizeCommittedLiminalTokenEntry {
                object_id,
                name: "Record Probe".to_string(),
                source_id,
                controller,
                enters_attacking: false,
                attach_to: None,
                sacrifice_at: None,
                created_ids: Vec::new(),
                ability_injection: LiminalTokenAbilityInjection::ResolvedToken,
                entry_events: emission,
            },
            &mut events,
        ));
        (state, object_id, events)
    }

    /// CR 400.7 + CR 614.12a: `Suppress` means the object is NOT yet the thing that entered —
    /// `BecomeCopy` has not run and any mandatory as-enters choice is unanswered — so the record
    /// and the events are parked TOGETHER and realized later, as one operation, from a snapshot
    /// taken at flush. Recording here instead writes CR 400.7's "state at the moment of the move"
    /// from a pre-copy 0/0 Shapeshifter, which is the defect this lifecycle replaces.
    ///
    /// REVERT-PROBE (discriminating, RUN): replace the `Suppress` park with a pre-lifecycle
    /// RECORD-ONLY inline — take `state.objects.get(&object_id)`'s
    /// `snapshot_for_zone_change(object_id, None, Zone::Battlefield)` and pass it straight to
    /// `restrictions::record_zone_change`, emitting nothing — ⇒ the row counts here read `(1, 1)`
    /// and the pending assertion fails, while `suppress_does_not_emit_the_entry_pair` below still
    /// passes — isolating the flip to the record, not the events. The substitution is deliberately
    /// record-only (NOT `zones::record_and_emit_entry_from_no_zone`, which also emits): emitting
    /// would break the paired isolation claim.
    #[test]
    fn suppressed_liminal_entry_parks_instead_of_recording() {
        let (state, object_id, _events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        assert_eq!(
            ledger_rows(&state, object_id),
            (0, 0),
            "CR 614.12a: a Suppress-route token writes NEITHER ledger until it is realized"
        );
        assert_eq!(
            state.pending_token_battlefield_entry,
            Some(PendingTokenBattlefieldEntry {
                object_id,
                name: "Record Probe".to_string(),
                source_id: ObjectId(1),
            }),
            "the whole entry is parked on GameState so it survives any number of round trips"
        );
    }

    /// The other half of the pin: `Suppress` really does withhold the events, so the test above
    /// is measuring a park with no emit rather than an emit that happened anyway.
    #[test]
    fn suppress_does_not_emit_the_entry_pair() {
        let (_state, _object_id, events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { .. } | GameEvent::TokenCreated { .. }
            )),
            "Suppress withholds both entry events; got {events:?}"
        );
    }

    /// CR 400.7 + CR 603.6a: the flush is the single realization authority — it records through
    /// `record_zone_change` AND emits the pair, once. A second call is structurally a no-op
    /// (`Option::take_if` consumed the parked value), which is what makes the duplicate-row class
    /// unrepresentable rather than guarded.
    ///
    /// REVERT-PROBE (discriminating, RUN): swap `take_if` for a non-consuming
    /// `as_ref().filter(..).cloned()` ⇒ the second flush returns `true`, appends a second row to
    /// each ledger and a second event pair, failing the idempotence half while the first-flush
    /// assertions stay green.
    #[test]
    fn flushing_a_parked_entry_records_and_emits_exactly_once() {
        let (mut state, object_id, _events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        let mut events = Vec::new();
        assert!(
            flush_pending_token_battlefield_entry(&mut state, object_id, &mut events),
            "the parked entry is realized by its first flush"
        );
        assert_eq!(
            ledger_rows(&state, object_id),
            (1, 1),
            "realization writes exactly one row on each ledger"
        );
        assert_eq!(
            (
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::ZoneChanged { .. }))
                    .count(),
                events
                    .iter()
                    .filter(|event| matches!(event, GameEvent::TokenCreated { .. }))
                    .count(),
            ),
            (1, 1),
            "realization emits the entry pair exactly once; got {events:?}"
        );
        assert!(state.pending_token_battlefield_entry.is_none());

        let mut second = Vec::new();
        assert!(
            !flush_pending_token_battlefield_entry(&mut state, object_id, &mut second),
            "a second flush finds nothing parked"
        );
        assert_eq!(
            ledger_rows(&state, object_id),
            (1, 1),
            "a second flush adds no row"
        );
        assert!(second.is_empty(), "a second flush emits nothing");
    }

    /// The parked entry is bound to ONE object identity: a flush for a different object must not
    /// consume it. Without this, an unrelated token's realization would emit this token's entry.
    #[test]
    fn flushing_a_foreign_object_id_is_a_no_op() {
        let (mut state, object_id, _events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        let foreign = ObjectId(object_id.0 + 1_000);
        let mut events = Vec::new();
        assert!(!flush_pending_token_battlefield_entry(
            &mut state,
            foreign,
            &mut events
        ));
        assert_eq!(ledger_rows(&state, object_id), (0, 0));
        assert_eq!(ledger_rows(&state, foreign), (0, 0));
        assert!(events.is_empty());
        assert!(
            state
                .pending_token_battlefield_entry
                .as_ref()
                .is_some_and(|pending| pending.object_id == object_id),
            "the binding survives a foreign flush untouched"
        );
    }

    /// CR 704.5f fail-safe: if the object is gone when the flush runs,
    /// `zones::record_and_emit_entry_from_no_zone` has nothing to snapshot, so no CR 400.7 row is
    /// written and NEITHER entry event is emitted — `push_committed_token_entry_events` gates
    /// `TokenCreated` on that same `None` verdict. The class-level coherence pin for this route
    /// (against the `created_tokens_this_turn` ledger, driven through
    /// `apply_pending_counter_post_action`) is `counters.rs`'s
    /// `a_vanished_counter_paused_token_reports_neither_creation_event_nor_ledger_row`.
    #[test]
    fn flushing_after_the_object_left_the_battlefield_records_nothing() {
        let (mut state, object_id, _events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        state.objects.remove(&object_id);
        state.battlefield.retain(|id| *id != object_id);
        let mut events = Vec::new();
        assert!(flush_pending_token_battlefield_entry(
            &mut state,
            object_id,
            &mut events
        ));
        assert_eq!(
            ledger_rows(&state, object_id),
            (0, 0),
            "a vanished object gets no CR 400.7 row"
        );
        assert!(
            events.is_empty(),
            "neither half of the entry pair is emitted for an object that is not there; \
             got {events:?}"
        );
    }

    /// The settled-action GATE that both `engine.rs` convergence points share
    /// ([`realize_settled_token_battlefield_entry`]), exercised over its three arms — including the
    /// CR 704.5f drop branch, which no production drive reaches (see that function's doc comment).
    /// Helper-level by construction: the two production entry points are covered by the Painter /
    /// Fanatic / Watchdog integration drives, which measure WHERE it is called from.
    #[test]
    fn the_settled_gate_realizes_only_a_settled_action_and_drops_a_departed_token() {
        // (i) Mid-prompt: the action has not settled, so nothing is realized.
        let (mut state, object_id, _events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        state.waiting_for = WaitingFor::MeldPairChoice {
            player: PlayerId(0),
            choices: Vec::new(),
        };
        let mut events = Vec::new();
        assert!(
            !realize_settled_token_battlefield_entry(&mut state, &mut events),
            "an unsettled action realizes nothing, so the boundary convergence must not run a \
             trigger pass"
        );
        assert_eq!(ledger_rows(&state, object_id), (0, 0));
        assert!(events.is_empty());
        assert!(
            state.pending_token_battlefield_entry.is_some(),
            "an unsettled action leaves the entry parked for a later round trip"
        );

        // (ii) Settled with the token still on the battlefield: realized, once.
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert!(
            realize_settled_token_battlefield_entry(&mut state, &mut events),
            "a settled action with the token still on the battlefield realizes the pair, which is \
             what gates the CR 603.6a pass at the action boundary"
        );
        assert_eq!(ledger_rows(&state, object_id), (1, 1));
        assert!(state.pending_token_battlefield_entry.is_none());
        assert_eq!(
            events
                .iter()
                .filter(|event| matches!(event, GameEvent::ZoneChanged { .. }))
                .count(),
            1,
            "the settled action carries the entry pair; got {events:?}"
        );

        // (iii) CR 704.5f: settled, but the token has left the battlefield ⇒ the parked entry is
        //       DROPPED without ever reaching the flush — no row and no events. A direct flush on
        //       a gone object now agrees; see
        //       `flushing_after_the_object_left_the_battlefield_records_nothing`. The difference
        //       here is only that the gate consumes the park itself and returns `false`, so there
        //       is no slice for the boundary convergence to scan.
        let (mut departed, departed_id, _events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Suppress);
        departed.battlefield.retain(|id| *id != departed_id);
        departed.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let mut departed_events = Vec::new();
        assert!(
            !realize_settled_token_battlefield_entry(&mut departed, &mut departed_events),
            "the CR 704.5f drop branch consumes the park but emits nothing, so there is no slice \
             for the boundary convergence to scan"
        );
        assert_eq!(ledger_rows(&departed, departed_id), (0, 0));
        assert!(departed_events.is_empty());
        assert!(departed.pending_token_battlefield_entry.is_none());
    }

    /// Serde: the parked entry round-trips, and a `GameState` JSON written before this field
    /// existed still loads (the `#[serde(default)]` save-compat claim).
    #[test]
    fn pending_token_battlefield_entry_round_trips() {
        let mut state = GameState::new_two_player(42);
        state.pending_token_battlefield_entry = Some(PendingTokenBattlefieldEntry {
            object_id: ObjectId(7),
            name: "Record Probe".to_string(),
            source_id: ObjectId(1),
        });
        let encoded = serde_json::to_string(&state).expect("GameState serializes");
        let decoded: GameState = serde_json::from_str(&encoded).expect("GameState deserializes");
        assert_eq!(
            decoded.pending_token_battlefield_entry,
            state.pending_token_battlefield_entry
        );

        let mut without: serde_json::Value =
            serde_json::from_str(&encoded).expect("the encoded state is JSON");
        assert!(
            without
                .as_object_mut()
                .expect("GameState encodes as a JSON object")
                .remove("pending_token_battlefield_entry")
                .is_some(),
            "the key must be present to begin with, or the removal below proves nothing"
        );
        let legacy: GameState =
            serde_json::from_value(without).expect("a save without the key still loads");
        assert!(legacy.pending_token_battlefield_entry.is_none());
    }

    /// The double-count guard for the `Emit` arm: recording in the finalize tail AND inside
    /// `push_committed_token_entry_events` would put two rows on the ledger. Exactly one — and
    /// nothing is parked, because that route's object is already fully realized.
    #[test]
    fn emitted_liminal_entry_records_exactly_one_row() {
        let (state, object_id, events) =
            finalize_liminal_entry_under(TokenEntryEventEmission::Emit);
        let (entries, zone_rows) = ledger_rows(&state, object_id);
        assert_eq!(entries, 1, "Emit records battlefield entry exactly once");
        assert_eq!(zone_rows, 1, "Emit records the zone change exactly once");
        assert!(
            state.pending_token_battlefield_entry.is_none(),
            "the Emit route parks nothing"
        );
        assert!(
            events
                .iter()
                .any(|event| matches!(event, GameEvent::TokenCreated { .. })),
            "Emit still emits the entry pair; got {events:?}"
        );
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
                target: TargetFilter::SelfRef,
                ..
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

    /// CR 111.3: A catalog-materialized token ability is a printed slot of the
    /// token's own base set, so its live entry must carry a real `Printed`
    /// occurrence ref.
    ///
    /// RED before the fix: `apply_token_ability_payload` pushed the bare
    /// `TriggerDefinition`, which `Definitions::push<U: Into<T>>` routed through
    /// `From<TriggerDefinition> for TriggerEntry` and stamped
    /// `TriggerDefinitionOccurrenceRef::Unmaterialized`. That variant is
    /// `#[serde(skip_serializing)]`, so the first time the state crossed the
    /// WASM bridge `to_js` panicked ("the enum variant
    /// `TriggerDefinitionOccurrenceRef::Unmaterialized` cannot be serialized"),
    /// killing the worker — the engine computed the right tokens and then died
    /// handing them to the UI. No in-process test caught it because engine tests
    /// never serialize.
    #[test]
    fn catalog_token_trigger_carries_printed_occurrence_and_serializes() {
        use crate::types::ability::TriggerDefinitionOccurrenceRef;

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

        let entry = obj.trigger_definitions.iter_all().next().unwrap();
        assert!(
            matches!(
                entry.occurrence,
                TriggerDefinitionOccurrenceRef::Printed { .. }
            ),
            "CR 111.3: a catalog token trigger is a printed slot of the token's own \
             base set, got {:?}",
            entry.occurrence
        );

        // The object-local provenance invariant must hold: `Unmaterialized` is
        // explicitly rejected from an observable game state.
        obj.validate_trigger_definitions()
            .expect("catalog token trigger must have observable occurrence provenance");

        // The bridge check. `engine-wasm`'s `to_js` panics on serialization
        // failure, so an unserializable entry is fatal, not degraded.
        serde_json::to_string(obj)
            .expect("catalog token object must serialize for the WASM bridge");
        serde_json::to_string(&state).expect("full game state must serialize for the WASM bridge");
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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

    /// CR 111.10k + CR 704.5m: a Monster Role has enchant creature, so it
    /// must be put into its owner's graveyard when an animated Mishra's
    /// Foundry stops being a creature during cleanup. The token then ceases
    /// to exist, but the battlefield-to-graveyard event still occurs.
    #[test]
    fn monster_role_on_animated_foundry_dies_during_cleanup() {
        let mut state = GameState::new_two_player(42);
        let foundry = create_object(
            &mut state,
            CardId(98),
            PlayerId(0),
            "Mishra's Foundry".to_string(),
            Zone::Battlefield,
        );
        {
            let object = state.objects.get_mut(&foundry).unwrap();
            object.card_types.core_types.push(CoreType::Land);
            object.base_card_types = object.card_types.clone();
        }

        let animate = ResolvedAbility::new(
            Effect::Animate {
                power: Some(PtValue::Fixed(2)),
                toughness: Some(PtValue::Fixed(2)),
                types: vec!["Artifact".to_string(), "Creature".to_string()],
                remove_types: vec![],
                keywords: vec![],
                target: TargetFilter::None,
            },
            vec![],
            foundry,
            PlayerId(0),
        );
        let mut events = Vec::new();
        crate::game::effects::animate::resolve(&mut state, &animate, &mut events).unwrap();
        crate::game::layers::flush_layers(&mut state);
        assert!(
            state.objects[&foundry]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "Mishra's Foundry must be a creature before the Role is created"
        );

        let create_role = ResolvedAbility::new(
            Effect::Token {
                name: "Monster Role".to_string(),
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
                attach_to: Some(TargetFilter::ParentTarget),
                enters_attacking: false,
                supertypes: vec![],
                static_abilities: vec![],
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(foundry)],
            foundry,
            PlayerId(0),
        );
        resolve(&mut state, &create_role, &mut events).unwrap();
        let role = state.last_created_token_ids[0];
        assert_eq!(
            state.objects[&role].attached_to,
            Some(AttachTarget::Object(foundry)),
            "Monster Role must enter attached to Mishra's Foundry"
        );
        assert!(
            // allow-raw-authority: the test verifies the exact intrinsic Enchant filter, which the keyword-kind authority cannot inspect
            state.objects[&role].keywords.iter().any(|keyword| matches!(
                keyword,
                Keyword::Enchant(TargetFilter::Typed(filter))
                    if filter.type_filters.contains(&TypeFilter::Creature)
            )),
            "Monster Role must have the intrinsic enchant creature ability"
        );

        events.clear();
        assert!(crate::game::turns::execute_cleanup(&mut state, &mut events).is_none());
        crate::game::sba::check_state_based_actions(&mut state, &mut events);

        assert!(
            !state.objects[&foundry]
                .card_types
                .core_types
                .contains(&CoreType::Creature),
            "Mishra's Foundry must stop being a creature during cleanup"
        );
        assert!(
            events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged {
                    object_id,
                    from: Some(Zone::Battlefield),
                    to: Zone::Graveyard,
                    ..
                } if *object_id == role
            )),
            "the illegal Aura must move from the battlefield to its owner's graveyard"
        );
        assert!(
            !state.objects.contains_key(&role),
            "a Role token put into a graveyard must cease to exist"
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
        use crate::parser::oracle::try_parse_equip_lowered;
        use crate::types::ability::{ContinuousModification, StaticDefinition};
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let equip = try_parse_equip_lowered("Equip {0}").expect("equip static");
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
        use crate::parser::oracle::try_parse_equip_lowered;
        use crate::types::ability::{ContinuousModification, StaticCondition, StaticDefinition};
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let equip = try_parse_equip_lowered("Equip {0}").expect("equip static");
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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
            attach_to: crate::types::proposed_event::TokenHostRequest::NotRequested,
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

    /// A Rest in Peace-class board-wide `Moved` graveyard→exile redirect,
    /// deliberately NOT a creature: a creature would be a legal host for
    /// `enchant creature` and CR 303.4g would never be reached.
    fn add_graveyard_to_exile_redirect(state: &mut GameState) -> ObjectId {
        use crate::types::ability::{AbilityDefinition, AbilityKind, ReplacementDefinition};
        use crate::types::replacements::ReplacementEvent;

        let rip = create_object(
            state,
            CardId(90_400),
            PlayerId(1),
            "Rest in Peace".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&rip)
            .expect("just created")
            .replacement_definitions
            .push(
                ReplacementDefinition::new(ReplacementEvent::Moved)
                    .destination_zone(Zone::Graveyard)
                    .execute(AbilityDefinition::new(
                        AbilityKind::Spell,
                        Effect::ChangeZone {
                            origin: None,
                            destination: Zone::Exile,
                            target: TargetFilter::SelfRef,
                            owner_library: false,
                            enter_transformed: false,
                            enters_under: None,
                            enter_tapped: EtbTapState::Unspecified,
                            enters_attacking: false,
                            up_to: false,
                            enter_with_counters: vec![],
                            conditional_enter_with_counters: vec![],
                            face_down_profile: None,
                            enters_modified_if: None,
                        },
                    )),
            );
        rip
    }

    /// Build the unhosted liminal Aura fixture: an Aura with `Enchant creature`
    /// in a game with no creature anywhere, so the CR 303.4f consult finds no
    /// legal host and CR 303.4g decides the entry.
    fn unhosted_liminal_aura_entrant(state: &mut GameState) -> (ObjectId, GameObject) {
        use crate::types::keywords::Keyword;

        let (entry_ref, mut entrant) =
            reserve_liminal_token_object(state, PlayerId(0), "Unhosted Aura".to_string());
        entrant.card_types.core_types = vec![CoreType::Enchantment];
        entrant.card_types.subtypes = vec!["Aura".to_string()];
        entrant.base_card_types = entrant.card_types.clone();
        let enchant = Keyword::Enchant(TargetFilter::Typed(
            crate::types::ability::TypedFilter::new(crate::types::ability::TypeFilter::Creature),
        ));
        entrant.keywords = vec![enchant.clone()];
        entrant.base_keywords = vec![enchant];
        let timestamp = state.next_timestamp();
        entrant.reset_for_battlefield_entry(state.turn_number, timestamp);
        (entry_ref, entrant)
    }

    fn liminal_entry_for(
        object: crate::types::game_state::LiminalEntrant,
        source_id: ObjectId,
    ) -> crate::types::game_state::LiminalEntry {
        crate::types::game_state::LiminalEntry {
            object,
            name: "Unhosted Aura".to_string(),
            source_id,
            controller: PlayerId(0),
            enters_attacking: false,
            attach_to: None,
            sacrifice_at: None,
            remaining_count: 0,
            created_ids: Vec::new(),
            copy_resume: None,
            spec_resume: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_with_counters: Vec::new(),
            kind: crate::types::game_state::LiminalEntryKind::Token,
            replacement_applied: std::collections::HashSet::new(),
        }
    }

    /// CR 303.4g + CR 111.1 on the liminal seam: an unhosted entrant never
    /// enters, and because this seam's entrant is a `LiminalEntrant::Token`, "if
    /// the Aura is a token, it isn't created" is the whole disposition — nothing
    /// is placed in any zone.
    ///
    /// A Rest in Peace-class graveyard→exile redirect is on the battlefield
    /// throughout. Its only job is to be available: the seam performs no
    /// placement for it to redirect, so both the graveyard and exile stay empty.
    /// That is the point of the narrowing — the placement that used to bypass
    /// this redirect does not exist any more, rather than existing and being
    /// routed correctly.
    #[test]
    fn an_unhosted_liminal_aura_token_is_not_created_and_reaches_no_zone() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Maker".to_string(),
            Zone::Battlefield,
        );
        let rip = add_graveyard_to_exile_redirect(&mut state);
        // Reach guard: the anaphora slot starts non-empty, so the republish
        // below is observable rather than vacuously equal.
        state.last_created_token_ids = vec![ObjectId(4_242)];

        let (entry_ref, entrant) = unhosted_liminal_aura_entrant(&mut state);
        state.liminal_entries.insert(
            entry_ref,
            liminal_entry_for(
                crate::types::game_state::LiminalEntrant::Token(
                    crate::types::game_state::TokenProjection::materialize(entrant),
                ),
                source_id,
            ),
        );

        let mut events = Vec::new();
        assert!(
            commit_liminal_token_entry_with_post_actions(
                &mut state,
                ProposedEvent::TokenEntry {
                    entry_ref,
                    enter_tapped: EtbTapState::Unspecified,
                    enter_with_counters: Vec::new(),
                    applied: std::collections::HashSet::new(),
                },
                &mut events,
                TokenEntryEventEmission::Emit,
                Vec::new(),
            ),
            "a denied entry is not a pause — the batch loop must continue"
        );

        // CR 303.4g + CR 111.1: "it isn't created" — the object does not exist.
        assert!(
            !state.objects.contains_key(&entry_ref),
            "a token CR 303.4g denies is not created at all"
        );
        assert!(!state.battlefield.iter().any(|&id| id == entry_ref));
        // Nothing observed the entry: no birth, no battlefield ZoneChanged.
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::TokenCreated { object_id, .. } if *object_id == entry_ref
            )),
            "CR 303.4g: no TokenCreated for an entry the rule denies"
        );
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::ZoneChanged { object_id, .. } if *object_id == entry_ref
            )),
            "CR 303.4g: no ZoneChanged at all for an entry the rule denies"
        );
        assert!(
            state
                .resolved_rules_journal
                .entries()
                .iter()
                .all(|entry| !matches!(
                    entry.command,
                    Some(crate::types::resolved_commands::ResolvedRulesCommand::TokenCreation(_))
                )),
            "CR 733: no birth is journaled for a token that isn't created"
        );
        // CR 111.1: the anaphora slot names the tokens THIS effect created, so
        // it is republished as this batch's list (empty here) rather than left
        // holding the earlier, unrelated effect's tokens.
        assert!(state.last_created_token_ids.is_empty());
        // Nothing reached a graveyard, so the redirect that was standing by had
        // nothing to redirect either.
        assert!(state
            .players
            .iter()
            .all(|player| player.graveyard.is_empty()));
        assert!(
            !state.exile.iter().any(|&id| id == entry_ref),
            "the redirect must not have anything to redirect"
        );
        assert!(
            state.objects.contains_key(&rip),
            "reach guard: the redirect was on the battlefield the whole time"
        );
        assert!(state.liminal_entries.is_empty());
    }

    /// The card-backed half of the old dual-disposition test, kept as the
    /// regression for what replaced it: a card-backed projection reaching this
    /// seam is now inert instead of being raw-placed into a graveyard.
    ///
    /// `LiminalEntrant::Card` is the CR 701.42a meld result — a permanent
    /// "represented by two cards" — which enters through
    /// `ProposedEvent::ZoneChange` from the exile its components sit in, and
    /// whose CR 303.4g dispositions are decided there (see
    /// `zone_pipeline::the_stack_origin_graveyard_placement_consults_moved_redirects`
    /// for the replacement-consulted graveyard placement on that path). A
    /// `TokenEntry` naming one names nothing this seam may act on.
    ///
    /// Revert-failing assertion: `graveyard.is_empty()`. The deleted
    /// `place_unentered_aura_in_owners_graveyard` put this entrant into its
    /// owner's graveyard with raw `zones::` calls, past the Rest in Peace-class
    /// redirect that is on the battlefield here — so before the change this
    /// assertion failed, and the exile assertion below failed too.
    #[test]
    fn a_card_backed_liminal_projection_is_never_placed_by_the_token_entry_seam() {
        let mut state = GameState::new_two_player(42);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Maker".to_string(),
            Zone::Battlefield,
        );
        add_graveyard_to_exile_redirect(&mut state);

        let (entry_ref, entrant) = unhosted_liminal_aura_entrant(&mut state);
        state.liminal_entries.insert(
            entry_ref,
            liminal_entry_for(
                crate::types::game_state::LiminalEntrant::Card(entrant),
                source_id,
            ),
        );

        let mut events = Vec::new();
        assert!(
            commit_liminal_token_entry_with_post_actions(
                &mut state,
                ProposedEvent::TokenEntry {
                    entry_ref,
                    enter_tapped: EtbTapState::Unspecified,
                    enter_with_counters: Vec::new(),
                    applied: std::collections::HashSet::new(),
                },
                &mut events,
                TokenEntryEventEmission::Emit,
                Vec::new(),
            ),
            "declining an entrant that is not this seam's is not a pause"
        );

        assert!(
            state
                .players
                .iter()
                .all(|player| player.graveyard.is_empty()),
            "no raw graveyard placement may happen on this path"
        );
        assert!(
            state.exile.is_empty(),
            "and nothing was routed to the redirect's destination either"
        );
        assert!(!state.objects.contains_key(&entry_ref));
        assert!(!state.battlefield.iter().any(|&id| id == entry_ref));
        assert!(events.is_empty(), "nothing observable happened: {events:?}");
        assert!(
            state.liminal_entries.contains_key(&entry_ref),
            "the projection is left exactly where it was, not consumed"
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
                object: crate::types::game_state::LiminalEntrant::Token(
                    crate::types::game_state::TokenProjection::materialize(token),
                ),
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
            "Test Card",
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

    /// Revert-to-red: removing the `card_name` threading in
    /// `catalog_rules_text_abilities` reverts this to `TriggerMode::Unknown`
    /// with the raw text, since the bare "galactus attacks..." subject
    /// matches no recognized pattern and falls through to the terminal
    /// fallback.
    #[test]
    fn catalog_galactus_preset_trigger_subject_normalizes() {
        let (_statics, modifications, _unparsed_lines) = catalog_rules_text_abilities(
            "Flying, trample\nWhenever Galactus attacks, destroy target land.",
            "Galactus",
        );

        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantTrigger { trigger }
                    if trigger.mode == TriggerMode::Attacks
                        && trigger.valid_card == Some(TargetFilter::SelfRef)
            )),
            "card-name subject in an attack trigger must normalize to a \
             self-referential TriggerMode::Attacks, got {modifications:?}"
        );
    }

    /// Revert-to-red: without normalization, no `StaticDefinition` is
    /// produced for this line at all (`parse_self_color_subject`'s `alt()`
    /// doesn't match the bare name "Mechtitan"), and the CDA silently
    /// vanishes into an inert `GrantAbility` instead.
    #[test]
    fn catalog_mechtitan_preset_cda_subject_normalizes() {
        let (static_definitions, _modifications, _unparsed_lines) = catalog_rules_text_abilities(
            "Mechtitan is all colors.\nFlying, vigilance, trample, lifelink, haste",
            "Mechtitan",
        );

        assert!(
            static_definitions.iter().any(|def| {
                def.characteristic_defining
                    && def.affected == Some(TargetFilter::SelfRef)
                    && def.modifications.iter().any(|modification| matches!(
                        modification,
                        ContinuousModification::SetColor { colors }
                            if colors.len() == 5
                    ))
            }),
            "card-name subject in a CDA color-setting static must normalize to a \
             self-referential characteristic-defining SetColor{{all 5 colors}}, got {static_definitions:?}"
        );
    }

    /// Revert-to-red: without normalization, the effect's target is
    /// `TargetFilter::Any` (the unconsumed literal name falls through
    /// `parse_target`'s terminal fallback), meaning the token would copy
    /// any legal target instead of itself.
    #[test]
    fn catalog_council_of_reeds_preset_copy_effect_normalizes() {
        let (_statics, modifications, _unparsed_lines) = catalog_rules_text_abilities(
            "The \"legend rule\" doesn't apply to creatures you control.\n\
             At the beginning of combat on your turn, if you've cast a noncreature \
             spell this turn, create a token that's a copy of Council of Reeds.\n\
             (This token's mana cost is {2}{U}.)",
            "Council of Reeds",
        );

        fn ability_has_self_copy(def: &AbilityDefinition) -> bool {
            matches!(
                *def.effect,
                Effect::CopyTokenOf {
                    target: TargetFilter::SelfRef,
                    ..
                }
            ) || def
                .sub_ability
                .as_deref()
                .is_some_and(ability_has_self_copy)
                || def
                    .else_ability
                    .as_deref()
                    .is_some_and(ability_has_self_copy)
                || def.mode_abilities.iter().any(ability_has_self_copy)
        }

        assert!(
            modifications.iter().any(|modification| matches!(
                modification,
                ContinuousModification::GrantTrigger { trigger }
                    if trigger
                        .execute
                        .as_deref()
                        .is_some_and(ability_has_self_copy)
            )),
            "card-name copy-of subject must normalize to a self-referential \
             CopyTokenOf, got {modifications:?}"
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

    /// CR 201.5a end-to-end proof: Toggo's Rock grants the equipped creature
    /// "{1}, {T}, Sacrifice Rock: This creature deals 2 damage to any
    /// target." Rock (the card literally named in the cost) must be the
    /// object sacrificed — not the host it's equipped to — and the scrub
    /// pass (Step 1) must have removed the raw placeholder character from
    /// the granted ability's description before it ever reaches the host.
    ///
    /// Revert-to-red: reverting the `card_name` threading in
    /// `catalog_rules_text_abilities` (Step 2) does NOT bind the sacrifice
    /// cost to the host. Verified empirically (temporarily disabling the
    /// `normalize_card_name_refs` call and printing the resulting cost): it
    /// produces a generic, UNBOUND
    /// `AbilityCost::Sacrifice(SacrificeCost { target: Typed(TypedFilter {
    /// type_filters: [], controller: None, properties: [] }), .. })` that
    /// matches any permanent, not a host-bound `SelfRef`. Because that filter
    /// is unconstrained, `pay_with(&[rock_id])` below still succeeds even
    /// under the reverted code (Rock trivially satisfies "any permanent"),
    /// and the runtime activate/pay/resolve assertions still pass — they
    /// exercise that the grant-and-activate mechanism works end-to-end (a
    /// real but separate property), not the granter-binding regression
    /// itself. The pre-activation `TargetFilter` equality assertion
    /// immediately below is the one check in this test that is actually
    /// load-bearing for this regression.
    ///
    /// For the placeholder-leak assertion: Rock's granted ability is a
    /// granted *activated* ability, routed through `parse_quoted_ability`
    /// (`oracle_static/grammar.rs`), which already calls its own
    /// `sanitize_granting_placeholder` independent of the two scrub loops
    /// added to `catalog_rules_text_abilities` in this diff — so disabling
    /// only those two loops does not reproduce a leak here. See
    /// `catalog_synthetic_equipment_grant_trigger_scrub_removes_placeholder`
    /// for the case (a granted TRIGGER, with no independent scrubber) that
    /// actually exercises the new scrub loops.
    #[test]
    fn catalog_toggo_rock_sacrifice_cost_binds_to_rock_not_host() {
        use crate::game::scenario::{GameScenario, P0, P1};
        use crate::types::mana::{ManaType, ManaUnit};

        fn sacrifice_target(cost: &AbilityCost) -> Option<&TargetFilter> {
            match cost {
                AbilityCost::Sacrifice(sac) => Some(&sac.target),
                AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
                    costs.iter().find_map(sacrifice_target)
                }
                _ => None,
            }
        }

        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(
            P0,
            vec![ManaUnit::new(ManaType::White, ObjectId(0), false, vec![])],
        );
        let host = scenario.add_creature(P0, "Bearer", 2, 2).id();

        let mut runner = scenario.build();
        let rock_id = build_catalog_token(
            runner.state_mut(),
            "Rock",
            "1657233e-c9e1-54ff-aa5a-6e2e2846be42",
        );
        {
            let st = runner.state_mut();
            st.objects.get_mut(&rock_id).unwrap().attached_to = Some(AttachTarget::Object(host));
            st.layers_dirty.mark_full();
        }
        crate::game::layers::evaluate_layers(runner.state_mut());

        let idx = runner.state().objects[&host]
            .abilities
            .iter()
            .position(|a| a.cost.as_ref().and_then(sacrifice_target).is_some())
            .expect("host must carry Rock's granted sacrifice-cost ability after evaluate_layers");

        assert_eq!(
            runner.state().objects[&host].abilities[idx]
                .cost
                .as_ref()
                .and_then(sacrifice_target),
            Some(&TargetFilter::SpecificObject { id: rock_id }),
            "CR 201.5a: the sacrifice cost must target Rock (the granting object), not the host"
        );
        assert!(
            !runner.state().objects[&host].abilities[idx]
                .description
                .as_deref()
                .unwrap_or_default()
                .contains(crate::parser::oracle_util::GRANTING_SELF_PLACEHOLDER),
            "granted ability description must not leak the raw placeholder char"
        );

        let outcome = runner
            .activate(host, idx)
            .target_player(P1)
            .pay_with(&[rock_id])
            .resolve();

        // CR 111.7 + CR 704.5d: Rock is a token, so once it's sacrificed off the
        // battlefield it ceases to exist as a state-based action and is purged
        // from `state.objects` entirely — it never sits observably in the
        // graveyard the way a card-backed permanent would (contrast
        // `deconstruction_hammer_sacrifice_hits_the_equipment_not_the_host`,
        // whose Hammer is a real card and persists at `zone_of == Graveyard`).
        assert!(
            !outcome.state().objects.contains_key(&rock_id),
            "Rock (the object actually named in the cost) must be sacrificed and, \
             being a token, cease to exist"
        );
        assert_eq!(
            outcome.zone_of(host),
            Zone::Battlefield,
            "the equipped creature survives"
        );
    }

    /// CR 201.5a: proves the two `scrub_static_descriptions` /
    /// `scrub_modification_descriptions` loops at the end of
    /// `catalog_rules_text_abilities` actually do something. Rock's own test
    /// above (a granted *activated* ability, routed through
    /// `parse_quoted_ability`) is scrubbed independently by that grammar's own
    /// `sanitize_granting_placeholder` call and does not exercise these two
    /// loops. A granted *trigger* has no such independent scrubber, so this
    /// synthetic Equipment's granted "Whenever this creature attacks,
    /// sacrifice <self>" trigger — parsed via `parse_static_line_multi` →
    /// `classify_quoted_inner`'s `GrantTrigger` branch, never touching
    /// `parse_quoted_ability` — is the case that actually needs the new
    /// scrub loops.
    ///
    /// Revert-to-red: commenting out the two scrub loops (while leaving
    /// `normalize_card_name_refs` intact) leaves the raw
    /// `GRANTING_SELF_PLACEHOLDER` char in the granted trigger's
    /// `description`, flipping the no-leak assertion below to a failure.
    #[test]
    fn catalog_synthetic_equipment_grant_trigger_scrub_removes_placeholder() {
        let (static_definitions, _modifications, unparsed_lines) = catalog_rules_text_abilities(
            "Equipped creature has \"Whenever this creature attacks, sacrifice Ember Golem.\"",
            "Ember Golem",
        );
        assert!(
            unparsed_lines.is_empty(),
            "line must fully parse, got unparsed: {unparsed_lines:?}"
        );

        fn find_grant_trigger(def: &StaticDefinition) -> Option<&TriggerDefinition> {
            def.modifications.iter().find_map(|m| match m {
                ContinuousModification::GrantTrigger { trigger } => Some(trigger.as_ref()),
                _ => None,
            })
        }

        let trigger = static_definitions
            .iter()
            .find_map(find_grant_trigger)
            .unwrap_or_else(|| {
                panic!(
                    "quoted \"Whenever ...\" body must parse to a GrantTrigger, \
                     got {static_definitions:?}"
                )
            });

        let sacrifices_granter = trigger.execute.as_deref().is_some_and(|def| {
            matches!(
                *def.effect,
                Effect::Sacrifice {
                    target: TargetFilter::GrantingObject,
                    ..
                }
            )
        });
        assert!(
            sacrifices_granter,
            "the granted trigger's sacrifice effect must target the GRANTING object \
             (the equipment itself), got {trigger:?}"
        );

        // NOTE: check the actual `description` fields directly, NOT a
        // `{:?}`-formatted dump of the tree — `Debug` escapes the raw private-use
        // char to the literal text `\u{e0002}`, so searching a Debug string for
        // the real character is always false regardless of whether scrubbing ran.
        let placeholder = crate::parser::oracle_util::GRANTING_SELF_PLACEHOLDER;
        let leaked = static_definitions.iter().any(|def| {
            def.description
                .as_deref()
                .is_some_and(|d| d.contains(placeholder))
                || def.modifications.iter().any(|m| match m {
                    ContinuousModification::GrantTrigger { trigger } => trigger
                        .description
                        .as_deref()
                        .is_some_and(|d| d.contains(placeholder)),
                    _ => false,
                })
        });
        assert!(
            !leaked,
            "the new scrub loops must remove the raw placeholder char from the \
             granted trigger's description, got {static_definitions:#?}"
        );
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

#[cfg(test)]
mod attach_host_authority_tests {
    use super::*;
    use crate::types::ability::{SeatDirection, TypedFilter};

    /// The selected-target class must stay disjoint from
    /// [`TargetFilter::is_context_ref`], the engine's existing authority on which
    /// filters never surface a chosen target slot. Anything that authority calls
    /// a context reference has to resolve through its own authority here, or
    /// yield no host — it may never read `ability.targets`.
    #[test]
    fn selected_target_filters_are_never_context_refs() {
        for filter in [
            TargetFilter::Any,
            TargetFilter::Typed(TypedFilter::creature()),
            TargetFilter::Not {
                filter: Box::new(TargetFilter::Any),
            },
            TargetFilter::Or {
                filters: vec![TargetFilter::Any],
            },
            TargetFilter::And {
                filters: vec![TargetFilter::Any],
            },
            TargetFilter::Named {
                name: "Grizzly Bears".to_string(),
            },
            TargetFilter::HasChosenName,
            TargetFilter::StackSpell,
            TargetFilter::StackAbility {
                controller: None,
                tag: None,
                kind: None,
            },
        ] {
            assert!(
                matches!(
                    classify_attach_host_authority(&filter),
                    AttachHostAuthority::SelectedTarget
                ),
                "fixture guard: {filter:?} is meant to be a selected-target filter"
            );
            assert!(
                !filter.is_context_ref(),
                "{filter:?} is an automatic context reference and must not read the \
                 ability's chosen targets"
            );
        }
    }

    /// The same disjointness read from the other side, which is the direction
    /// that catches a misfiling: every filter the engine calls a context
    /// reference must resolve through an authority of its own or yield no host.
    #[test]
    fn context_references_never_classify_as_a_selected_target() {
        for filter in [
            TargetFilter::SourceOrPaired,
            TargetFilter::SelfRef,
            TargetFilter::CostPaidObject,
            TargetFilter::LastCreated,
            TargetFilter::AttachedTo,
            TargetFilter::EventTarget,
            TargetFilter::ParentTarget,
            TargetFilter::ParentTargetSlot { index: 0 },
            TargetFilter::OriginalSource,
            TargetFilter::TrackedSet {
                id: TrackedSetId(0),
            },
            TargetFilter::PostReplacementDamageSource,
            TargetFilter::Neighbor {
                direction: SeatDirection::Left,
            },
        ] {
            assert!(
                filter.is_context_ref(),
                "fixture guard: {filter:?} is meant to be a context reference"
            );
            assert!(
                !matches!(
                    classify_attach_host_authority(&filter),
                    AttachHostAuthority::SelectedTarget
                ),
                "{filter:?} is a context reference and must not inherit the ability's \
                 chosen targets as its attachment host"
            );
        }
    }

    /// CR 601.3: the case where the two ways of deciding disagree. A composite
    /// that CONTAINS the exile anaphor is a context reference as a whole —
    /// `is_context_ref` says so through `references_exiled_by_source`, which
    /// recurses — while its OUTER shape is `And`, which is otherwise an object
    /// predicate. The parser builds exactly this for "an exiled card that is a
    /// creature", so classifying by shape would read the enclosing ability's
    /// chosen targets for an object the exile link already names.
    #[test]
    fn a_composite_carrying_the_exile_anaphor_is_not_a_selected_target() {
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::ExiledBySource,
                TargetFilter::Typed(TypedFilter::creature()),
            ],
        };
        assert!(
            filter.is_context_ref(),
            "fixture guard: the composite must be a context reference"
        );
        assert!(
            matches!(
                classify_attach_host_authority(&filter),
                AttachHostAuthority::NoHost
            ),
            "a composite naming an exile-linked object has no host authority here"
        );
    }
}

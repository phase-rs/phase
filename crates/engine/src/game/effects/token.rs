use std::collections::HashSet;
use std::str::FromStr;
use std::sync::Arc;

use crate::game::quantity::{resolve_quantity, resolve_quantity_with_targets};
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, ControllerRef,
    DelayedTriggerCondition, Duration, Effect, EffectError, EffectKind, GainLifePlayer,
    ManaContribution, ManaProduction, PtValue, QuantityExpr, QuantityRef, ResolvedAbility,
    TargetFilter, TargetRef, TypedFilter,
};
use crate::types::card_type::{CardType, CoreType, Supertype};
use crate::types::events::GameEvent;
use crate::types::game_state::{DelayedTrigger, GameState};
use crate::types::identifiers::CardId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::mana::ManaCost;
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

// ── Token script parser ─────────────────────────────────────────────────

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
            keywords.push(Keyword::Firebending(n));
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
            enters_attacking,
            supertypes,
            static_abilities,
            enter_with_counters,
            ..
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
        ),
    };
    let token_owner = resolve_token_owner(state, ability, owner_filter);

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
            ability.controller,
            ability.source_id,
        )
    });

    // CR 122.6a: Resolve ETB counter quantities before proposing — the event
    // carries fully-resolved counts, not quantity expressions.
    let resolved_etb_counters: Vec<(String, u32)> = etb_counters
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
        ability,
        state,
    );

    // CR 614.1a: Propose entire token batch for replacement pipeline.
    // Replacement effects (Doubling Season, Primal Vigor) modify count.
    let proposed = ProposedEvent::CreateToken {
        owner: token_owner,
        spec: Box::new(spec),
        enter_tapped: crate::types::proposed_event::EtbTapState::from_seeded_tapped(tapped),
        count,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            apply_create_token_after_replacement(state, event, events);
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

    // CR 609.3: Consume the tracked set after reading its size for "this way" counting.
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
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::from(&ability.effect),
        source_id: ability.source_id,
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
    enter_with_counters: Vec<(String, u32)>,
    ability: &ResolvedAbility,
    state: &GameState,
) -> crate::types::proposed_event::TokenSpec {
    use crate::types::proposed_event::TokenSpec;

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
            let rp = resolve_pt_value(fallback_power, state, ability.controller, ability.source_id);
            let rt = resolve_pt_value(
                fallback_toughness,
                state,
                ability.controller,
                ability.source_id,
            );
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
        display_name,
        script_name: script_name.to_string(),
        power,
        toughness,
        core_types,
        subtypes,
        supertypes,
        colors,
        keywords,
        static_abilities,
        enter_with_counters,
        tapped,
        enters_attacking,
        sacrifice_at: ability.duration.clone(),
        source_id: ability.source_id,
        controller: ability.controller,
    }
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
) {
    let ProposedEvent::CreateToken {
        owner,
        spec,
        enter_tapped,
        count: final_count,
        ..
    } = event
    else {
        return;
    };

    let mut created_ids = Vec::with_capacity(final_count as usize);

    for _ in 0..final_count {
        let obj_id = zones::create_object(
            state,
            CardId(0),
            owner,
            spec.display_name.clone(),
            Zone::Battlefield,
        );

        if let Some(obj) = state.objects.get_mut(&obj_id) {
            // CR 111.1: Mark as token for SBA cleanup (CR 704.5d)
            obj.is_token = true;
            let has_attrs = spec.power.is_some()
                || spec.toughness.is_some()
                || !spec.core_types.is_empty()
                || !spec.subtypes.is_empty()
                || !spec.supertypes.is_empty()
                || !spec.colors.is_empty()
                || !spec.keywords.is_empty();
            if has_attrs {
                obj.power = spec.power;
                obj.toughness = spec.toughness;
                obj.base_power = spec.power;
                obj.base_toughness = spec.toughness;
                obj.card_types = CardType {
                    supertypes: spec.supertypes.clone(),
                    core_types: spec.core_types.clone(),
                    subtypes: spec.subtypes.clone(),
                };
                obj.base_card_types = obj.card_types.clone();
                obj.color = spec.colors.clone();
                obj.base_color = spec.colors.clone();
                obj.keywords = spec.keywords.clone();
                obj.base_keywords = spec.keywords.clone();
            }
            obj.tapped = enter_tapped.resolve(spec.tapped);

            // CR 113.3d: Apply static abilities from the token definition.
            for static_def in &spec.static_abilities {
                obj.static_definitions.push(static_def.clone());
            }
        }

        // CR 508.4: Token enters attacking — not declared as attacker.
        if spec.enters_attacking {
            crate::game::combat::enter_attacking(state, obj_id, spec.source_id, spec.controller);
        }

        // CR 122.6a: Place counters on the token as it enters the battlefield.
        for (counter_type_str, counter_count) in &spec.enter_with_counters {
            if *counter_count > 0 {
                let ct = crate::types::counter::parse_counter_type(counter_type_str);
                super::counters::add_counter_with_replacement(
                    state,
                    obj_id,
                    ct,
                    *counter_count,
                    events,
                );
            }
        }

        // CR 111.10a–v: Inject predefined abilities for known token subtypes.
        inject_predefined_token_abilities(state, obj_id);
        state.layers_dirty = true;
        crate::game::restrictions::record_battlefield_entry(state, obj_id);
        crate::game::restrictions::record_token_created(state, obj_id);

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
            name: spec.display_name.clone(),
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
                        up_to: false,
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
}

fn resolve_token_owner(
    state: &GameState,
    ability: &ResolvedAbility,
    owner_filter: &TargetFilter,
) -> PlayerId {
    match owner_filter {
        TargetFilter::Controller => ability.controller,
        TargetFilter::ParentTargetController => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Object(id) => state.objects.get(id).map(|object| object.controller),
                TargetRef::Player(pid) => Some(*pid),
            })
            .unwrap_or(ability.controller),
        _ => ability
            .targets
            .iter()
            .find_map(|target| match target {
                TargetRef::Player(pid) => Some(*pid),
                TargetRef::Object(id) => state.objects.get(id).map(|object| object.controller),
            })
            .unwrap_or(ability.controller),
    }
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
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
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

    let resolved_power = resolve_pt_value(power, state, controller, source_id);
    let resolved_toughness = resolve_pt_value(toughness, state, controller, source_id);
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

fn resolve_pt_value(
    value: &PtValue,
    state: &GameState,
    controller: crate::types::player::PlayerId,
    source_id: crate::types::identifiers::ObjectId,
) -> i32 {
    match value {
        PtValue::Fixed(n) => *n,
        PtValue::Variable(_) => 0,
        PtValue::Quantity(expr) => resolve_quantity(state, expr, controller, source_id),
    }
}

// ── Predefined token abilities (CR 111.10a–v) ─────────────────────────
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
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Tap,
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            },
        ],
    })
}

/// CR 111.10b: Food — "{2}, {T}, Sacrifice this artifact: You gain 3 life."
fn food_ability() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 3 },
            player: GainLifePlayer::Controller,
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
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            },
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
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            },
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
                random: false,
                self_ref: false,
            },
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            },
        ],
    })
}

/// CR 106.1 + CR 701.16a: Eldrazi Spawn — "Sacrifice this token: Add {C}."
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
        },
    )
    .cost(AbilityCost::Sacrifice {
        target: TargetFilter::SelfRef,
        count: 1,
    })
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
            restrictions: vec![ManaSpendRestriction::SpellTypeOrAbilityActivation(
                "Artifact".to_string(),
            )],
            grants: vec![],
            expiry: None,
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
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            },
        ],
    })
    .activation_restrictions(vec![ActivationRestriction::AsSorcery])
}

/// CR 111.10a–v: Predefined token abilities keyed by subtype.
/// Returns ability definitions to inject for the given subtype, or empty if none.
fn predefined_token_abilities(subtype: &str) -> Vec<AbilityDefinition> {
    match subtype {
        "Treasure" => vec![treasure_ability()],
        "Food" => vec![food_ability()],
        "Clue" => vec![clue_ability()],
        "Blood" => vec![blood_ability()],
        "Powerstone" => vec![powerstone_ability()],
        "Map" => vec![map_ability()],
        "Spawn" => vec![spawn_ability()],
        // TODO: Incubator (transform), Shard, Gold, Junk
        _ => vec![],
    }
}

/// Inject predefined token abilities based on the token's subtypes.
/// Called after token creation to ensure Treasure/Food/Clue/etc. have their
/// standard activated abilities.
pub(super) fn inject_predefined_token_abilities(
    state: &mut GameState,
    obj_id: crate::types::identifiers::ObjectId,
) {
    let subtypes = match state.objects.get(&obj_id) {
        Some(obj) => obj.card_types.subtypes.clone(),
        None => return,
    };
    let mut abilities_to_add = Vec::new();
    for subtype in &subtypes {
        abilities_to_add.extend(predefined_token_abilities(subtype));
    }
    if !abilities_to_add.is_empty() {
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            Arc::make_mut(&mut obj.abilities).extend(abilities_to_add.clone());
            Arc::make_mut(&mut obj.base_abilities).extend(abilities_to_add);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;

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
        assert!(matches!(*abilities[0].effect, Effect::Mana { .. }));
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
                assert!(costs.iter().any(|cost| matches!(
                    cost,
                    AbilityCost::Sacrifice {
                        target: TargetFilter::SelfRef,
                        count: 1
                    }
                )));
            }
            other => panic!("expected composite cost, got {other:?}"),
        }
    }

    #[test]
    fn predefined_spawn_has_colorless_sacrifice_mana_ability() {
        // CR 106.1 + CR 701.16a: Eldrazi Spawn tokens produced by Writhing
        // Chrysalis, Awakening Zone, etc. share a single sacrifice-for-{C}
        // mana ability, injected by subtype.
        let abilities = predefined_token_abilities("Spawn");
        assert_eq!(abilities.len(), 1);
        assert!(matches!(*abilities[0].effect, Effect::Mana { .. }));
        assert!(matches!(
            abilities[0].cost,
            Some(AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
                count: 1,
            })
        ));
    }

    #[test]
    fn non_predefined_token_gets_no_abilities() {
        let abilities = predefined_token_abilities("Soldier");
        assert!(abilities.is_empty());
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
    fn apply_create_token_populates_last_created_token_ids() {
        use crate::types::card_type::CoreType;
        use crate::types::proposed_event::TokenSpec;
        use std::collections::HashSet;

        let mut state = GameState::new_two_player(42);
        assert!(state.last_created_token_ids.is_empty());

        let spec = TokenSpec {
            display_name: "Hero".to_string(),
            script_name: "c_1_1_hero".to_string(),
            power: Some(1),
            toughness: Some(1),
            core_types: vec![CoreType::Creature],
            subtypes: vec!["Hero".to_string()],
            supertypes: vec![],
            colors: vec![],
            keywords: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
            tapped: false,
            enters_attacking: false,
            sacrifice_at: None,
            source_id: ObjectId(100),
            controller: PlayerId(0),
        };

        let event = ProposedEvent::CreateToken {
            owner: PlayerId(0),
            spec: Box::new(spec),
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
}

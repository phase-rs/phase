use std::collections::HashSet;
use std::str::FromStr;

use crate::game::quantity::resolve_quantity;
use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{
    Effect, EffectError, EffectKind, PtValue, QuantityExpr, QuantityRef, ResolvedAbility,
};
use crate::types::card_type::{CardType, CoreType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
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

/// Create a token on the battlefield.
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
        enters_attacking,
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
            enters_attacking,
            ..
        } => (
            name.clone(),
            power.clone(),
            toughness.clone(),
            types.clone(),
            colors.clone(),
            keywords.clone(),
            *tapped,
            resolve_quantity(state, count, ability.controller, ability.source_id).max(0) as u32,
            *enters_attacking,
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
            false,
        ),
    };

    let parsed = parse_token_script(&script_name).or_else(|| {
        build_token_attrs_from_effect(
            &script_name,
            &fallback_power,
            &fallback_toughness,
            &fallback_types,
            &fallback_colors,
            &fallback_keywords,
            state,
            ability.controller,
            ability.source_id,
        )
    });

    let display_name = parsed
        .as_ref()
        .map(|a| a.display_name.clone())
        .unwrap_or_else(|| script_name.clone());

    for _ in 0..count {
        let proposed = ProposedEvent::CreateToken {
            owner: ability.controller,
            name: display_name.clone(),
            applied: HashSet::new(),
        };

        match replacement::replace_event(state, proposed, events) {
            ReplacementResult::Execute(event) => {
                if let ProposedEvent::CreateToken {
                    owner,
                    name: token_name,
                    ..
                } = event
                {
                    let obj_id = zones::create_object(
                        state,
                        CardId(0),
                        owner,
                        token_name.clone(),
                        Zone::Battlefield,
                    );

                    let fallback_pt = if parsed.is_none() {
                        let rp = resolve_pt_value(
                            &fallback_power,
                            state,
                            ability.controller,
                            ability.source_id,
                        );
                        let rt = resolve_pt_value(
                            &fallback_toughness,
                            state,
                            ability.controller,
                            ability.source_id,
                        );
                        Some((rp, rt))
                    } else {
                        None
                    };
                    if let Some(obj) = state.objects.get_mut(&obj_id) {
                        if let Some(attrs) = &parsed {
                            obj.power = attrs.power;
                            obj.toughness = attrs.toughness;
                            obj.base_power = attrs.power;
                            obj.base_toughness = attrs.toughness;
                            obj.card_types = CardType {
                                supertypes: vec![],
                                core_types: attrs.core_types.clone(),
                                subtypes: attrs.subtypes.clone(),
                            };
                            obj.base_card_types = obj.card_types.clone();
                            obj.color = attrs.colors.clone();
                            obj.base_color = attrs.colors.clone();
                            obj.keywords = attrs.keywords.clone();
                            obj.base_keywords = attrs.keywords.clone();
                        } else {
                            let (resolved_power, resolved_toughness) =
                                fallback_pt.unwrap_or((0, 0));
                            if resolved_power != 0 || resolved_toughness != 0 {
                                obj.power = Some(resolved_power);
                                obj.toughness = Some(resolved_toughness);
                                obj.base_power = Some(resolved_power);
                                obj.base_toughness = Some(resolved_toughness);
                                obj.card_types.core_types.push(CoreType::Creature);
                                obj.base_card_types = obj.card_types.clone();
                            }
                        }
                        obj.tapped = tapped;

                        // CR 508.4: Token enters attacking — not declared as attacker
                        if enters_attacking {
                            obj.tapped = true;
                            obj.entered_battlefield_turn = Some(state.turn_number);
                        }
                    }

                    // CR 508.4: Push token into combat.attackers directly
                    if enters_attacking {
                        if let Some(combat) = state.combat.as_mut() {
                            // Determine defending player from the source creature's combat info
                            let defending_player = combat
                                .attackers
                                .iter()
                                .find(|a| a.object_id == ability.source_id)
                                .map(|a| a.defending_player)
                                .unwrap_or_else(|| {
                                    crate::types::player::PlayerId(1 - ability.controller.0)
                                });
                            combat.attackers.push(crate::game::combat::AttackerInfo {
                                object_id: obj_id,
                                defending_player,
                            });
                        }
                    }

                    state.layers_dirty = true;
                    crate::game::restrictions::record_battlefield_entry(state, obj_id);
                    crate::game::restrictions::record_token_created(state, obj_id);

                    events.push(GameEvent::TokenCreated {
                        object_id: obj_id,
                        name: token_name,
                    });
                }
            }
            ReplacementResult::Prevented => {
                // This individual token was prevented — continue creating the rest
                continue;
            }
            ReplacementResult::NeedsChoice(player) => {
                state.waiting_for =
                    crate::game::replacement::replacement_choice_waiting_for(player, state);
                return Ok(());
            }
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

#[allow(clippy::too_many_arguments)]
fn build_token_attrs_from_effect(
    name: &str,
    power: &PtValue,
    toughness: &PtValue,
    types: &[String],
    colors: &[ManaColor],
    keywords: &[Keyword],
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
                attach_to: None,
                enters_attacking: false,
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
            .players_who_had_creature_etb_this_turn
            .contains(&PlayerId(0)));
        assert!(state
            .players_who_had_angel_or_berserker_etb_this_turn
            .contains(&PlayerId(0)));
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
                attach_to: None,
                enters_attacking: false,
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
                attach_to: None,
                enters_attacking: false,
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
                attach_to: None,
                enters_attacking: false,
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
                attach_to: None,
                enters_attacking: false,
            },
            vec![],
            ObjectId(100),
            PlayerId(0),
        );
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(state.objects[&state.battlefield[0]].tapped);
    }
}

use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::game::zones;
use crate::types::ability::{effect_variant_name, Effect, EffectError, ResolvedAbility};
use crate::types::card_type::{CardType, CoreType};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::CardId;
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

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

/// Parse a Forge token script name like `w_1_1_soldier_flying` into structured attributes.
///
/// Format: `{color}_{power}_{toughness}_{subtype}[_{subtype}][_{keyword}]`
/// Or for non-creature artifacts/enchantments: `{color}_a_{subtype}` / `{color}_e_{subtype}`
fn parse_token_script(script: &str) -> Option<TokenAttrs> {
    let parts: Vec<&str> = script.split(',').next()?.split('_').collect();
    if parts.len() < 2 {
        return None;
    }

    let color_code = parts[0];

    // Validate color code: must be all valid color letters
    if !color_code.chars().all(|c| "wubrgc".contains(c)) {
        return None;
    }

    let colors = parse_colors(color_code);
    let rest = &parts[1..];

    // Determine if this is a non-creature artifact/enchantment or a creature
    if rest.first() == Some(&"a") && rest.get(1).is_some_and(|s| s.parse::<i32>().is_err()) {
        // Non-creature artifact: {color}_a_{subtype}[_{suffix}]
        let subtypes = extract_subtypes(&rest[1..]);
        let display_name = format_display_name(&subtypes);
        return Some(TokenAttrs {
            display_name,
            power: None,
            toughness: None,
            core_types: vec![CoreType::Artifact],
            subtypes,
            colors,
            keywords: vec![],
        });
    }
    if rest.first() == Some(&"e") && rest.get(1).is_some_and(|s| s.parse::<i32>().is_err()) {
        // Non-creature enchantment: {color}_e_{subtype}[_{suffix}]
        let subtypes = extract_subtypes(&rest[1..]);
        let display_name = format_display_name(&subtypes);
        return Some(TokenAttrs {
            display_name,
            power: None,
            toughness: None,
            core_types: vec![CoreType::Enchantment],
            subtypes,
            colors,
            keywords: vec![],
        });
    }

    // Creature token: {color}_{power}_{toughness}_{type_flags_and_subtypes_and_keywords}
    let power = rest.first()?.parse::<i32>().ok();
    let toughness = rest.get(1).and_then(|s| s.parse::<i32>().ok());

    // If we can't parse P/T, this isn't a standard token script
    if power.is_none() || toughness.is_none() {
        // Could be x_x for variable P/T
        if rest.first() == Some(&"x") && rest.get(1) == Some(&"x") {
            // Variable P/T creature
            let type_parts = &rest[2..];
            let mut core_types = vec![CoreType::Creature];
            let mut type_segments: Vec<&str> = Vec::new();

            for &part in type_parts {
                match part {
                    "a" => core_types.push(CoreType::Artifact),
                    "e" => core_types.push(CoreType::Enchantment),
                    _ => type_segments.push(part),
                }
            }

            let keywords = extract_keywords(&type_segments);
            let subtypes = extract_subtypes_filtered(&type_segments);
            let display_name = format_display_name(&subtypes);

            return Some(TokenAttrs {
                display_name,
                power: Some(0),
                toughness: Some(0),
                core_types,
                subtypes,
                colors,
                keywords,
            });
        }
        return None;
    }

    let type_parts = &rest[2..];
    let mut core_types = vec![CoreType::Creature];
    let mut type_segments: Vec<&str> = Vec::new();

    for &part in type_parts {
        match part {
            "a" => core_types.push(CoreType::Artifact),
            "e" => core_types.push(CoreType::Enchantment),
            _ => type_segments.push(part),
        }
    }

    let keywords = extract_keywords(&type_segments);
    let subtypes = extract_subtypes_filtered(&type_segments);
    let display_name = format_display_name(&subtypes);

    Some(TokenAttrs {
        display_name,
        power,
        toughness,
        core_types,
        subtypes,
        colors,
        keywords,
    })
}

fn parse_colors(code: &str) -> Vec<ManaColor> {
    let mut colors = Vec::new();
    for c in code.chars() {
        match c {
            'w' => colors.push(ManaColor::White),
            'u' => colors.push(ManaColor::Blue),
            'b' => colors.push(ManaColor::Black),
            'r' => colors.push(ManaColor::Red),
            'g' => colors.push(ManaColor::Green),
            _ => {} // 'c' = colorless, no color added
        }
    }
    colors
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

// Suffixes in token names that are ability descriptions, not subtypes or keywords
const IGNORED_SUFFIXES: &[&str] = &[
    "sac", "draw", "noblock", "lifegain", "lose", "con", "burn", "snipe",
    "pwdestroy", "regenerate", "exile", "counter", "illusory", "decayed",
    "opp", "life", "total", "ammo", "mana", "restrict", "tappump", "crewbuff",
    "crewsaddlebuff", "unblockable", "toxic", "banding", "cardsinhand",
    "mountainwalk", "leavedrain", "firebending", "exileplay", "search",
    "mill", "nosferatu", "sound", "call", "resurgence", "grave", "pro",
    "red", "burst", "spiritshadow", "landfall", "drawcounter",
    "poison", "total_artifacts", "total_lands", "life_total",
];

fn is_keyword(s: &str) -> Option<Keyword> {
    KNOWN_KEYWORDS.iter().find(|(k, _)| *k == s).map(|(_, v)| v.clone())
}

fn is_ignored_suffix(s: &str) -> bool {
    IGNORED_SUFFIXES.contains(&s)
}

fn extract_keywords(segments: &[&str]) -> Vec<Keyword> {
    segments.iter().filter_map(|s| is_keyword(s)).collect()
}

fn extract_subtypes(parts: &[&str]) -> Vec<String> {
    parts
        .iter()
        .filter(|s| !is_ignored_suffix(s))
        .map(|s| capitalize(s))
        .collect()
}

fn extract_subtypes_filtered(segments: &[&str]) -> Vec<String> {
    segments
        .iter()
        .filter(|s| is_keyword(s).is_none() && !is_ignored_suffix(s))
        .map(|s| capitalize(s))
        .collect()
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

/// Create a token creature on the battlefield.
/// Reads token attributes from `Effect::Token { name, power, toughness, .. }`,
/// with fallback to `Name`, `Power`, `Toughness` params.
/// Parses Forge token script names (e.g. `w_1_1_soldier_flying`) to extract
/// card types, colors, keywords, and a human-readable display name.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (script_name, effect_power, effect_toughness) = match &ability.effect {
        Effect::Token {
            name,
            power,
            toughness,
            ..
        } => (name.clone(), Some(*power), Some(*toughness)),
        _ => {
            let n = ability
                .params
                .get("Name")
                .cloned()
                .unwrap_or_else(|| "Token".to_string());
            let p = ability.params.get("Power").and_then(|v| v.parse().ok());
            let t = ability.params.get("Toughness").and_then(|v| v.parse().ok());
            (n, p, t)
        }
    };

    // Parse the Forge token script name for structured attributes
    let parsed = parse_token_script(&script_name);

    let display_name = parsed
        .as_ref()
        .map(|a| a.display_name.clone())
        .unwrap_or_else(|| script_name.clone());

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

                if let Some(obj) = state.objects.get_mut(&obj_id) {
                    if let Some(attrs) = &parsed {
                        // Apply parsed attributes
                        obj.power = attrs.power;
                        obj.toughness = attrs.toughness;
                        obj.base_power = attrs.power;
                        obj.base_toughness = attrs.toughness;
                        obj.card_types = CardType {
                            supertypes: vec![],
                            core_types: attrs.core_types.clone(),
                            subtypes: attrs.subtypes.clone(),
                        };
                        obj.color = attrs.colors.clone();
                        obj.base_color = attrs.colors.clone();
                        obj.keywords = attrs.keywords.clone();
                        obj.base_keywords = attrs.keywords.clone();
                    } else {
                        // Fallback: use effect power/toughness, assume Creature if P/T present
                        obj.power = effect_power;
                        obj.toughness = effect_toughness;
                        if effect_power.is_some() {
                            obj.card_types.core_types.push(CoreType::Creature);
                        }
                    }
                }

                state.layers_dirty = true;

                events.push(GameEvent::TokenCreated {
                    object_id: obj_id,
                    name: token_name,
                });
            }
        }
        ReplacementResult::Prevented => {}
        ReplacementResult::NeedsChoice(player) => {
            let candidate_count = state
                .pending_replacement
                .as_ref()
                .map(|p| p.candidates.len())
                .unwrap_or(0);
            state.waiting_for = crate::types::game_state::WaitingFor::ReplacementChoice {
                player,
                candidate_count,
            };
            return Ok(());
        }
    }

    events.push(GameEvent::EffectResolved {
        api_type: effect_variant_name(&ability.effect).to_string(),
        source_id: ability.source_id,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::identifiers::ObjectId;
    use crate::types::player::PlayerId;
    use std::collections::HashMap;

    #[test]
    fn parse_white_soldier_token() {
        let attrs = parse_token_script("w_1_1_soldier").unwrap();
        assert_eq!(attrs.display_name, "Soldier");
        assert_eq!(attrs.power, Some(1));
        assert_eq!(attrs.toughness, Some(1));
        assert!(attrs.core_types.contains(&CoreType::Creature));
        assert_eq!(attrs.colors, vec![ManaColor::White]);
        assert!(attrs.subtypes.contains(&"Soldier".to_string()));
    }

    #[test]
    fn parse_colorless_treasure_token() {
        let attrs = parse_token_script("c_a_treasure_sac").unwrap();
        assert_eq!(attrs.display_name, "Treasure");
        assert!(attrs.core_types.contains(&CoreType::Artifact));
        assert!(!attrs.core_types.contains(&CoreType::Creature));
        assert_eq!(attrs.power, None);
        assert_eq!(attrs.toughness, None);
        assert!(attrs.colors.is_empty());
    }

    #[test]
    fn parse_green_elf_warrior_token() {
        let attrs = parse_token_script("g_1_1_elf_warrior").unwrap();
        assert_eq!(attrs.display_name, "Elf Warrior");
        assert_eq!(attrs.power, Some(1));
        assert_eq!(attrs.toughness, Some(1));
        assert!(attrs.core_types.contains(&CoreType::Creature));
        assert_eq!(attrs.colors, vec![ManaColor::Green]);
    }

    #[test]
    fn parse_token_with_keywords() {
        let attrs = parse_token_script("w_4_4_angel_flying_vigilance").unwrap();
        assert_eq!(attrs.display_name, "Angel");
        assert_eq!(attrs.power, Some(4));
        assert_eq!(attrs.toughness, Some(4));
        assert!(attrs.keywords.contains(&Keyword::Flying));
        assert!(attrs.keywords.contains(&Keyword::Vigilance));
        assert!(!attrs.subtypes.contains(&"Flying".to_string()));
    }

    #[test]
    fn parse_artifact_creature_token() {
        let attrs = parse_token_script("c_1_1_a_thopter_flying").unwrap();
        assert_eq!(attrs.display_name, "Thopter");
        assert!(attrs.core_types.contains(&CoreType::Creature));
        assert!(attrs.core_types.contains(&CoreType::Artifact));
        assert!(attrs.keywords.contains(&Keyword::Flying));
    }

    #[test]
    fn parse_multicolor_token() {
        let attrs = parse_token_script("wb_2_1_inkling_flying").unwrap();
        assert_eq!(attrs.display_name, "Inkling");
        assert!(attrs.colors.contains(&ManaColor::White));
        assert!(attrs.colors.contains(&ManaColor::Black));
    }

    #[test]
    fn parse_variable_pt_token() {
        let attrs = parse_token_script("g_x_x_ooze").unwrap();
        assert_eq!(attrs.display_name, "Ooze");
        assert!(attrs.core_types.contains(&CoreType::Creature));
        assert_eq!(attrs.power, Some(0));
        assert_eq!(attrs.toughness, Some(0));
    }

    #[test]
    fn parse_enchantment_token() {
        let attrs = parse_token_script("c_e_shard_draw").unwrap();
        assert_eq!(attrs.display_name, "Shard");
        assert!(attrs.core_types.contains(&CoreType::Enchantment));
        assert!(!attrs.core_types.contains(&CoreType::Creature));
    }

    #[test]
    fn parse_returns_none_for_named_tokens() {
        // Named tokens like "llanowar_elves" don't follow the script format
        assert!(parse_token_script("llanowar_elves").is_none());
        assert!(parse_token_script("storm_crow").is_none());
    }

    #[test]
    fn token_creates_creature_with_correct_types() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: Effect::Token {
                name: "w_1_1_soldier".to_string(),
                power: 0,
                toughness: 0,
                types: vec![],
                colors: vec![],
                keywords: vec![],
            },
            params: HashMap::new(),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj_id = state.battlefield[0];
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Soldier");
        assert_eq!(obj.power, Some(1));
        assert_eq!(obj.toughness, Some(1));
        assert!(obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.color, vec![ManaColor::White]);
    }

    #[test]
    fn token_creates_artifact_without_creature_type() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: Effect::Token {
                name: "c_a_treasure_sac".to_string(),
                power: 0,
                toughness: 0,
                types: vec![],
                colors: vec![],
                keywords: vec![],
            },
            params: HashMap::new(),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj_id = state.battlefield[0];
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Treasure");
        assert!(obj.card_types.core_types.contains(&CoreType::Artifact));
        assert!(!obj.card_types.core_types.contains(&CoreType::Creature));
        assert_eq!(obj.power, None);
    }

    #[test]
    fn token_with_keywords_applied() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: Effect::Token {
                name: "r_4_4_dragon_flying".to_string(),
                power: 0,
                toughness: 0,
                types: vec![],
                colors: vec![],
                keywords: vec![],
            },
            params: HashMap::new(),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        let obj_id = state.battlefield[0];
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Dragon");
        assert_eq!(obj.power, Some(4));
        assert!(obj.keywords.contains(&Keyword::Flying));
        assert_eq!(obj.color, vec![ManaColor::Red]);
    }

    #[test]
    fn token_creates_object_on_battlefield() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Token".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::from([
                ("Name".to_string(), "Soldier".to_string()),
                ("Power".to_string(), "1".to_string()),
                ("Toughness".to_string(), "1".to_string()),
            ]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.battlefield.len(), 1);
        let obj_id = state.battlefield[0];
        let obj = &state.objects[&obj_id];
        assert_eq!(obj.name, "Soldier");
        assert_eq!(obj.power, Some(1));
        assert_eq!(obj.toughness, Some(1));
        assert_eq!(obj.card_id, CardId(0));
    }

    #[test]
    fn token_emits_token_created_event() {
        let mut state = GameState::new_two_player(42);
        let ability = ResolvedAbility {
            effect: crate::types::ability::Effect::Other {
                api_type: "Token".to_string(),
                params: std::collections::HashMap::new(),
            },
            params: HashMap::from([("Name".to_string(), "Angel".to_string())]),
            targets: vec![],
            source_id: ObjectId(100),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::TokenCreated { name, .. } if name == "Angel")));
    }
}

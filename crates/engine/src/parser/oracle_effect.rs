use std::str::FromStr;

use super::oracle_static::parse_continuous_modifications;
use super::oracle_target::parse_target;
use super::oracle_util::{
    contains_object_pronoun, contains_possessive, parse_mana_production, parse_number,
    starts_with_possessive, strip_reminder_text,
};
use crate::types::ability::{
    AbilityCondition, AbilityDefinition, AbilityKind, ChoiceType, ControllerRef, CountValue,
    DamageAmount, Duration, Effect, FilterProp, GainLifePlayer, LifeAmount, ManaProduction,
    PtValue, StaticDefinition, TargetFilter, TypeFilter,
};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaColor;
use crate::types::statics::StaticMode;
use crate::types::zones::Zone;

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedEffectClause {
    effect: Effect,
    duration: Option<Duration>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SubjectApplication {
    affected: TargetFilter,
    target: Option<TargetFilter>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct TokenDescription {
    name: String,
    power: Option<PtValue>,
    toughness: Option<PtValue>,
    types: Vec<String>,
    colors: Vec<ManaColor>,
    keywords: Vec<Keyword>,
    tapped: bool,
    count: CountValue,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
struct AnimationSpec {
    power: Option<i32>,
    toughness: Option<i32>,
    colors: Option<Vec<ManaColor>>,
    keywords: Vec<Keyword>,
    types: Vec<String>,
    remove_all_abilities: bool,
}

/// Parse an effect clause from Oracle text into an Effect enum.
/// This handles the verb-based matching for spell effects, activated ability effects,
/// and the effect portion of triggered abilities.
///
/// For compound effects ("Gain 3 life. Draw a card."), call `parse_effect_chain`
/// which splits on sentence boundaries and chains via AbilityDefinition::sub_ability.
pub fn parse_effect(text: &str) -> Effect {
    parse_effect_clause(text).effect
}

fn parse_effect_clause(text: &str) -> ParsedEffectClause {
    let text = text.trim().trim_end_matches('.');
    if text.is_empty() {
        return parsed_clause(Effect::Unimplemented {
            name: "empty".to_string(),
            description: None,
        });
    }

    if let Some((duration, rest)) = strip_leading_duration(text) {
        return with_clause_duration(parse_effect_clause(rest), duration);
    }

    // Mirror the CubeArtisan grammar's high-level sentence shapes:
    // 1) conditionals ("if X, Y"), 2) subject + verb phrase, 3) bare imperative.
    if let Some(stripped) = strip_leading_conditional(text) {
        return parse_effect_clause(&stripped);
    }
    if let Some(clause) = try_parse_subject_continuous_clause(text) {
        return clause;
    }
    if let Some(clause) = try_parse_subject_become_clause(text) {
        return clause;
    }
    if let Some(clause) = try_parse_subject_restriction_clause(text) {
        return clause;
    }
    // "Its controller gains life equal to its power/toughness" — subject must be preserved
    // because the life recipient is not the caster but the targeted permanent's controller.
    if let Some(clause) = try_parse_targeted_controller_gain_life(text) {
        return clause;
    }

    if let Some(stripped) = strip_subject_clause(text) {
        return parse_effect_clause(&stripped);
    }

    let (stripped, duration) = strip_trailing_duration(text);
    let mut clause = parsed_clause(parse_imperative_effect(stripped));
    if clause.duration.is_none() {
        clause.duration = duration;
    }
    clause
}

fn parse_imperative_effect(text: &str) -> Effect {
    let lower = text.to_lowercase();

    // --- Activation restrictions carried in ability sub-chains ---
    if let Some(effect) = try_parse_activate_only_condition(text) {
        return effect;
    }

    // --- Mana production ---
    if let Some(mana_effect) = try_parse_add_mana_effect(text) {
        return mana_effect;
    }

    // --- Damage: "~ deals N damage to {target}" ---
    if let Some(dmg) = try_parse_damage(&lower, text) {
        return dmg;
    }

    // --- Destroy: "destroy target/all {filter}" ---
    if lower.starts_with("destroy all ") || lower.starts_with("destroy each ") {
        let (target, _) = parse_target(&text[8..]); // skip "destroy "
        return Effect::DestroyAll { target };
    }
    if lower.starts_with("destroy ") {
        let (target, _) = parse_target(&text[8..]);
        return Effect::Destroy { target };
    }

    // --- Exile: "exile target/all {filter}" ---
    if lower.starts_with("exile all ") || lower.starts_with("exile each ") {
        let (target, _) = parse_target(&text[6..]);
        return Effect::ChangeZoneAll {
            origin: None,
            destination: Zone::Exile,
            target,
        };
    }
    if let Some(rest_lower) = lower.strip_prefix("exile ") {
        let (target, _) = parse_target(&text[6..]);
        let origin = infer_origin_zone(rest_lower);
        return Effect::ChangeZone {
            origin,
            destination: Zone::Exile,
            target,
        };
    }

    // --- Draw: "draw N card(s)" ---
    if lower.starts_with("draw ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Effect::Draw { count };
    }

    // --- Counter: "counter target spell" ---
    if lower.starts_with("counter ") {
        let (target, _) = parse_target(&text[8..]);
        let target = if text[8..].to_ascii_lowercase().contains("spell") {
            constrain_filter_to_stack(target)
        } else {
            target
        };
        return Effect::Counter { target };
    }

    // --- Life: "gain N life" / "you gain N life" ---
    if lower.contains("gain") && lower.contains("life") {
        let after_gain = if lower.starts_with("you gain ") {
            &text[9..]
        } else if lower.starts_with("gain ") {
            &text[5..]
        } else {
            ""
        };
        if !after_gain.is_empty() {
            let amount = parse_number(after_gain).map(|(n, _)| n as i32).unwrap_or(1);
            return Effect::GainLife {
                amount: LifeAmount::Fixed(amount),
                player: GainLifePlayer::Controller,
            };
        }
    }

    // --- Life loss: "lose N life" / "each opponent loses N life" ---
    if lower.contains("lose") && lower.contains("life") {
        // Extract the number before "life"
        let amount = extract_number_before(&lower, "life").unwrap_or(1) as i32;
        return Effect::LoseLife { amount };
    }

    // --- Pump: "{target} gets +N/+M [until end of turn]" ---
    if lower.contains("gets +")
        || lower.contains("gets -")
        || lower.contains("get +")
        || lower.contains("get -")
    {
        if let Some(pump) = try_parse_pump(&lower, text) {
            return pump;
        }
    }

    // --- Scry ---
    if lower.starts_with("scry ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Effect::Scry { count };
    }

    // --- Surveil ---
    if lower.starts_with("surveil ") {
        let count = parse_number(&text[8..]).map(|(n, _)| n).unwrap_or(1);
        return Effect::Surveil { count };
    }

    // --- Mill ---
    if lower.starts_with("mill ") {
        let count = parse_number(&text[5..]).map(|(n, _)| n).unwrap_or(1);
        return Effect::Mill {
            count,
            target: TargetFilter::Any,
        };
    }

    // --- Tap/Untap ---
    if lower.starts_with("tap ") {
        let (target, _) = parse_target(&text[4..]);
        return Effect::Tap { target };
    }
    if lower.starts_with("untap ") {
        let (target, _) = parse_target(&text[6..]);
        return Effect::Untap { target };
    }

    // --- Sacrifice ---
    if lower.starts_with("sacrifice ") {
        let (target, _) = parse_target(&text[10..]);
        return Effect::Sacrifice { target };
    }

    // --- Discard ---
    // NOTE: Engine has both Effect::Discard and Effect::DiscardCard.
    // Oracle parser always emits Effect::Discard per spec convention.
    if lower.starts_with("discard ") {
        let count = parse_number(&text[8..]).map(|(n, _)| n).unwrap_or(1);
        return Effect::Discard {
            count,
            target: TargetFilter::Any,
        };
    }

    // --- Put counter ---
    if lower.starts_with("put ") && lower.contains("counter") {
        if let Some(counter) = try_parse_put_counter(&lower, text) {
            return counter;
        }
    }

    // --- Return / Bounce ---
    if lower.starts_with("return ") {
        let (target, _) = parse_target(&text[7..]);
        return Effect::Bounce {
            target,
            destination: None,
        };
    }

    // --- Search library ---
    if starts_with_possessive(&lower, "search", "library") {
        return parse_search_library(text, &lower);
    }

    // --- Look at top N / Dig ---
    if lower.starts_with("look at the top ") {
        let count = parse_number(&text[16..]).map(|(n, _)| n).unwrap_or(1);
        return Effect::Dig {
            count,
            destination: None,
        };
    }

    // --- Fight ---
    if lower.starts_with("fight ") {
        let (target, _) = parse_target(&text[6..]);
        return Effect::Fight { target };
    }

    // --- Gain control ---
    if lower.starts_with("gain control of ") {
        let (target, _) = parse_target(&text[16..]);
        return Effect::GainControl { target };
    }

    // --- Token creation: "create N {P/T} {color} {type} creature token(s)" ---
    if lower.starts_with("create ") {
        if let Some(token) = try_parse_token(&lower, text) {
            return token;
        }
    }

    // --- Single-word effects ---
    if lower == "explore" || lower.starts_with("explore.") {
        return Effect::Explore;
    }
    if lower == "proliferate" || lower.starts_with("proliferate.") {
        return Effect::Proliferate;
    }

    // --- Shuffle ---
    if lower.starts_with("shuffle") && lower.contains("library") {
        if lower == "shuffle your library" {
            return Effect::Shuffle {
                target: TargetFilter::Controller,
            };
        }
        if lower == "shuffle their library" {
            return Effect::Shuffle {
                target: TargetFilter::Player,
            };
        }
        // "shuffle it/them/that card into its owner's library" → ChangeZone to Library.
        // The pronoun target is inherited from the parent ability chain.
        if contains_object_pronoun(&lower, "shuffle", "into")
            || contains_object_pronoun(&lower, "shuffles", "into")
        {
            return Effect::ChangeZone {
                origin: None,
                destination: Zone::Library,
                target: TargetFilter::Any,
            };
        }
        // "shuffle your/their graveyard into your/their library"
        if contains_possessive(&lower, "shuffle", "graveyard") {
            return Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                target: TargetFilter::Controller,
            };
        }
        // "shuffle your/their hand into your/their library"
        if contains_possessive(&lower, "shuffle", "hand") {
            return Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                target: TargetFilter::Controller,
            };
        }
        // Unrecognized compound shuffle
        return Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text.to_string()),
        };
    }

    // --- Look at hand: "look at target opponent's hand" / "look at your hand" → RevealHand ---
    if lower.starts_with("look at ") && lower.contains("hand") {
        // Possessive form: "look at your/their hand" → no targeting needed
        if contains_possessive(&lower, "look at", "hand") {
            return Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
            };
        }
        // Targeting form: "look at target opponent's hand"
        let after_look_at = &text[8..]; // skip "look at "
        let (target, _) = parse_target(after_look_at);
        return Effect::RevealHand {
            target,
            card_filter: TargetFilter::Any,
        };
    }

    // --- Reveal ---
    if lower.starts_with("reveal ") {
        // "reveal their/your hand" → RevealHand
        if lower.contains("hand") {
            return Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
            };
        }
        // "reveal the top N cards of your library" → Dig
        let count = if lower.contains("the top ") {
            let after_top = &lower[lower.find("the top ").unwrap() + 8..];
            parse_number(after_top).map(|(n, _)| n).unwrap_or(1)
        } else {
            1
        };
        return Effect::Dig {
            count,
            destination: None,
        };
    }

    // --- Prevent damage ---
    if lower.starts_with("prevent ") {
        return Effect::Unimplemented {
            name: "prevent".to_string(),
            description: Some(text.to_string()),
        };
    }

    // --- Regenerate ---
    if lower.starts_with("regenerate ") {
        return Effect::Unimplemented {
            name: "regenerate".to_string(),
            description: Some(text.to_string()),
        };
    }

    // --- Copy ---
    if lower.starts_with("copy ") {
        let (target, _) = parse_target(&text[5..]);
        return Effect::CopySpell { target };
    }

    // --- Transform ---
    if lower.starts_with("transform ") || lower == "transform" {
        return Effect::Unimplemented {
            name: "transform".to_string(),
            description: Some(text.to_string()),
        };
    }

    // --- Attach ---
    if lower.starts_with("attach ") {
        let to_pos = lower.find(" to ").map(|p| p + 4).unwrap_or(7);
        let (target, _) = parse_target(&text[to_pos..]);
        return Effect::Attach { target };
    }

    // --- Put (mill variant): "put the top N cards ... into ... graveyard" ---
    if lower.starts_with("put the top ") && lower.contains("graveyard") {
        let after = &lower[12..];
        let count = parse_number(after).map(|(n, _)| n).unwrap_or(1);
        return Effect::Mill {
            count,
            target: TargetFilter::Any,
        };
    }

    // --- Put {target} onto/in/on {zone} ---
    if lower.starts_with("put ") {
        if let Some(effect) = try_parse_put_zone_change(&lower, text) {
            return effect;
        }
    }

    // --- Put card on top of library ---
    if lower.starts_with("put ") && lower.contains("on top of") && lower.contains("library") {
        return Effect::ChangeZone {
            origin: None,
            destination: Zone::Library,
            target: TargetFilter::Any,
        };
    }

    // --- "you may " prefix stripping ---
    if lower.starts_with("you may ") {
        return parse_effect(&text[8..]);
    }

    // --- "Choose target X" / "Choose up to N target X" — synonym for targeting ---
    // Strips "choose " prefix and either re-parses (if the remainder has a verb) or
    // produces a TargetOnly effect (if it's a bare target designation).
    if let Some(rest) = lower.strip_prefix("choose ") {
        if is_choose_as_targeting(rest) {
            let stripped = &text["choose ".len()..];
            let inner = parse_effect(stripped);
            // If re-parsing produced a real effect (e.g. "choose target creature.
            // Untap it" where parse_effect handles the verb), return it directly.
            // Otherwise the remainder is a bare target phrase — extract the target.
            if !matches!(inner, Effect::Unimplemented { .. }) {
                return inner;
            }
            let (target, _) = parse_target(stripped);
            return Effect::TargetOnly { target };
        }
    }

    // --- Named choices: "choose a creature type", "choose a color", etc. ---
    if let Some(choice_type) = try_parse_named_choice(&lower) {
        return Effect::Choose { choice_type, persist: false };
    }

    // --- Choose card from revealed hand (absorbed into RevealHand filter) ---
    if lower.starts_with("choose ") && lower.contains("card from it") {
        let filter = parse_choose_filter(&lower);
        return Effect::RevealHand {
            target: TargetFilter::Any,
            card_filter: filter,
        };
    }

    // --- Fallback ---
    let verb = lower.split_whitespace().next().unwrap_or("unknown");
    Effect::Unimplemented {
        name: verb.to_string(),
        description: Some(text.to_string()),
    }
}

/// Determines if text after "choose " is a targeting synonym rather than
/// a modal choice ("choose one —"), color choice, or creature type choice.
///
/// Returns true when the text contains "target" (indicating a targeting phrase)
/// or uses "a/an {type} you/opponent control(s)" (selection-as-targeting).
///
/// Returns false for:
///   - "card from it" — handled separately as RevealHand filter
///   - "a color" / "a creature type" / "a card type" / "a card name" — different mechanics
fn is_choose_as_targeting(rest: &str) -> bool {
    // Already handled elsewhere
    if rest.contains("card from it") {
        return false;
    }

    // If try_parse_named_choice would match "choose {rest}", it's a named choice, not targeting
    let as_full = format!("choose {rest}");
    if try_parse_named_choice(&as_full).is_some() {
        return false;
    }

    // Any phrase containing "target" is a targeting synonym
    if rest.contains("target") {
        return true;
    }

    // "choose up to N" without "target" (e.g. "choose up to two creatures")
    if rest.starts_with("up to ") {
        return true;
    }

    // "choose a/an {type} ... you control / an opponent controls"
    if let Some(after_article) = rest.strip_prefix("a ").or_else(|| rest.strip_prefix("an ")) {
        // Exclude patterns not yet in try_parse_named_choice but still not targeting
        if after_article.starts_with("nonbasic land type") || after_article.starts_with("number") {
            return false;
        }
        // Must reference controller to be targeting-like
        if after_article.contains("you control")
            || after_article.contains("opponent controls")
            || after_article.contains("an opponent controls")
        {
            return true;
        }
    }

    false
}

/// Match "choose a creature type", "choose a color", "choose odd or even",
/// "choose a basic land type", "choose a card type" from lowercased Oracle text.
pub(crate) fn try_parse_named_choice(lower: &str) -> Option<ChoiceType> {
    if !lower.starts_with("choose ") {
        return None;
    }
    let rest = &lower[7..]; // skip "choose "
    if rest.starts_with("a creature type") {
        Some(ChoiceType::CreatureType)
    } else if rest.starts_with("a color") {
        Some(ChoiceType::Color)
    } else if rest.starts_with("odd or even") {
        Some(ChoiceType::OddOrEven)
    } else if rest.starts_with("a basic land type") {
        Some(ChoiceType::BasicLandType)
    } else if rest.starts_with("a card type") {
        Some(ChoiceType::CardType)
    } else if rest.starts_with("a card name")
        || rest.starts_with("a nonland card name")
        || rest.starts_with("a creature card name")
    {
        Some(ChoiceType::CardName)
    } else {
        None
    }
}

fn parse_choose_filter(lower: &str) -> TargetFilter {
    // Extract type info between "choose" and "card from it"
    // Handle both "choose X" and "you choose X" forms
    let after_choose = lower
        .strip_prefix("you choose ")
        .or_else(|| lower.strip_prefix("you may choose "))
        .or_else(|| lower.strip_prefix("choose "))
        .unwrap_or(lower);
    let before_card = after_choose.split("card").next().unwrap_or("");
    let cleaned = before_card
        .trim()
        .trim_start_matches("a ")
        .trim_start_matches("an ")
        .trim();

    let parts: Vec<&str> = cleaned.split(" or ").collect();
    if parts.len() > 1 {
        let filters: Vec<TargetFilter> = parts
            .iter()
            .filter_map(|p| type_str_to_target_filter(p.trim()))
            .collect();
        if filters.len() > 1 {
            return TargetFilter::Or { filters };
        }
        if let Some(f) = filters.into_iter().next() {
            return f;
        }
    }
    if let Some(f) = type_str_to_target_filter(cleaned) {
        return f;
    }
    TargetFilter::Any
}

fn type_str_to_target_filter(s: &str) -> Option<TargetFilter> {
    let card_type = match s {
        "artifact" => Some(TypeFilter::Artifact),
        "creature" => Some(TypeFilter::Creature),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "land" => Some(TypeFilter::Land),
        _ => None,
    };
    card_type.map(|ct| TargetFilter::Typed {
        card_type: Some(ct),
        subtype: None,
        controller: None,
        properties: vec![],
    })
}

/// Extract card type filter from a sub-ability sentence containing "card from it/among".
/// Handles forms like "exile a nonland card from it", "discard a creature card from it".
fn parse_choose_filter_from_sentence(lower: &str) -> TargetFilter {
    let card_pos = match lower.find("card from") {
        Some(pos) => pos,
        None => return TargetFilter::Any,
    };
    // The word immediately before "card from" is the type descriptor
    let word = lower[..card_pos].trim().rsplit(' ').next().unwrap_or("");
    if let Some(negated) = word.strip_prefix("non") {
        if let Some(TargetFilter::Typed { card_type, .. }) = type_str_to_target_filter(negated) {
            return TargetFilter::Typed {
                card_type: Some(TypeFilter::Permanent),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::NonType {
                    value: card_type.map(|ct| format!("{ct:?}")).unwrap_or_default(),
                }],
            };
        }
    }
    type_str_to_target_filter(word).unwrap_or(TargetFilter::Any)
}

fn try_parse_add_mana_effect(text: &str) -> Option<Effect> {
    let trimmed = text.trim();
    let lower = trimmed.to_lowercase();
    if !lower.starts_with("add ") {
        return None;
    }

    let clause = trimmed[4..].trim();
    let (without_where_x, where_x_expression) = strip_trailing_where_x(clause);
    let clause = without_where_x.trim().trim_end_matches(['.', '"']);

    if let Some(produced) = parse_mana_production_clause(clause) {
        return Some(Effect::Mana { produced });
    }

    if let Some((count, rest)) = parse_mana_count_prefix(clause) {
        let count = apply_where_x_count_expression(count, where_x_expression.as_deref());
        let rest = rest.trim().trim_end_matches(['.', '"']).trim();
        let rest_lower = rest.to_lowercase();

        if rest_lower.starts_with("mana of any one color")
            || rest_lower.starts_with("mana of any color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count,
                    color_options: all_mana_colors(),
                },
            });
        }

        if rest_lower.starts_with("mana in any combination of colors") {
            return Some(Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count,
                    color_options: all_mana_colors(),
                },
            });
        }

        if rest_lower.starts_with("mana of the chosen color")
            || rest_lower.starts_with("mana of that color")
        {
            return Some(Effect::Mana {
                produced: ManaProduction::ChosenColor { count },
            });
        }

        const ANY_COMBINATION_PREFIX: &str = "mana in any combination of ";
        if rest_lower.starts_with(ANY_COMBINATION_PREFIX) {
            let color_set_text = rest[ANY_COMBINATION_PREFIX.len()..].trim();
            if let Some(color_options) = parse_mana_color_set(color_set_text) {
                return Some(Effect::Mana {
                    produced: ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                });
            }
        }
    }

    let clause_lower = clause.to_lowercase();
    let fallback_count = parse_mana_count_prefix(clause)
        .map(|(count, _)| count)
        .unwrap_or(CountValue::Fixed(1));
    let fallback_count =
        apply_where_x_count_expression(fallback_count, where_x_expression.as_deref());

    if clause_lower.contains("mana of any one color") || clause_lower.contains("mana of any color")
    {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyOneColor {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
        });
    }

    if clause_lower.contains("mana in any combination of colors") {
        return Some(Effect::Mana {
            produced: ManaProduction::AnyCombination {
                count: fallback_count,
                color_options: all_mana_colors(),
            },
        });
    }

    if clause_lower.contains("mana of the chosen color")
        || clause_lower.contains("mana of that color")
    {
        return Some(Effect::Mana {
            produced: ManaProduction::ChosenColor {
                count: fallback_count,
            },
        });
    }

    None
}

fn try_parse_activate_only_condition(text: &str) -> Option<Effect> {
    let trimmed = text.trim().trim_end_matches('.');
    let lower = trimmed.to_ascii_lowercase();
    let prefix = "activate only if you control ";
    if !lower.starts_with(prefix) {
        return None;
    }

    let raw = &lower[prefix.len()..];
    let mut subtypes = Vec::new();
    for part in raw.split(" or ") {
        let token = part
            .trim()
            .trim_start_matches("a ")
            .trim_start_matches("an ")
            .trim();
        let subtype = match token {
            "plains" => "Plains",
            "island" => "Island",
            "swamp" => "Swamp",
            "mountain" => "Mountain",
            "forest" => "Forest",
            _ => return None,
        };
        if !subtypes.contains(&subtype) {
            subtypes.push(subtype);
        }
    }

    if subtypes.is_empty() {
        return None;
    }

    Some(Effect::Unimplemented {
        name: "activate_only_if_controls_land_subtype_any".to_string(),
        description: Some(subtypes.join("|")),
    })
}

fn parse_mana_production_clause(text: &str) -> Option<ManaProduction> {
    if let Some(color_options) = parse_mana_color_set(text) {
        if color_options.len() > 1 {
            return Some(ManaProduction::AnyOneColor {
                count: CountValue::Fixed(1),
                color_options,
            });
        }
    }

    if let Some((colors, _)) = parse_mana_production(text) {
        return Some(ManaProduction::Fixed { colors });
    }

    if let Some((count, _)) = parse_colorless_mana_production(text) {
        return Some(ManaProduction::Colorless { count });
    }

    None
}

fn parse_colorless_mana_production(text: &str) -> Option<(CountValue, &str)> {
    let mut rest = text.trim_start();
    let mut count = 0u32;

    while rest.starts_with('{') {
        let end = rest.find('}')?;
        let symbol = &rest[1..end];
        if !symbol.eq_ignore_ascii_case("C") {
            break;
        }
        count += 1;
        rest = rest[end + 1..].trim_start();
    }

    if count == 0 {
        return None;
    }

    Some((CountValue::Fixed(count), rest))
}

fn parse_mana_count_prefix(text: &str) -> Option<(CountValue, &str)> {
    let trimmed = text.trim_start();
    if let Some(rest) = trimmed.strip_prefix("X ") {
        return Some((CountValue::Variable("X".to_string()), rest.trim_start()));
    }
    if let Some(rest) = trimmed.strip_prefix("x ") {
        return Some((CountValue::Variable("X".to_string()), rest.trim_start()));
    }
    let (count, rest) = parse_number(trimmed)?;
    Some((CountValue::Fixed(count), rest))
}

fn apply_where_x_count_expression(
    count: CountValue,
    where_x_expression: Option<&str>,
) -> CountValue {
    match (count, where_x_expression) {
        (CountValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            CountValue::Variable(expression.to_string())
        }
        (count, _) => count,
    }
}

fn parse_mana_color_set(text: &str) -> Option<Vec<ManaColor>> {
    let mut rest = text.trim().trim_end_matches(['.', '"']).trim();
    if rest.is_empty() {
        return None;
    }

    let mut colors = Vec::new();
    loop {
        let (parsed, after_symbol) = parse_mana_color_symbol(rest)?;
        for color in parsed {
            if !colors.contains(&color) {
                colors.push(color);
            }
        }

        let next = after_symbol.trim_start();
        if next.is_empty() {
            break;
        }

        if let Some(stripped) = next.strip_prefix("and/or ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix("or ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix("and ") {
            rest = stripped.trim_start();
            continue;
        }
        if let Some(stripped) = next.strip_prefix(',') {
            let stripped = stripped.trim_start();
            if let Some(after_or) = stripped.strip_prefix("or ") {
                rest = after_or.trim_start();
                continue;
            }
            if let Some(after_and_or) = stripped.strip_prefix("and/or ") {
                rest = after_and_or.trim_start();
                continue;
            }
            if let Some(after_and) = stripped.strip_prefix("and ") {
                rest = after_and.trim_start();
                continue;
            }
            rest = stripped;
            continue;
        }
        if let Some(stripped) = next.strip_prefix('/') {
            rest = stripped.trim_start();
            continue;
        }

        return None;
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

fn parse_mana_color_symbol(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let trimmed = text.trim_start();
    if !trimmed.starts_with('{') {
        return None;
    }
    let end = trimmed.find('}')?;
    let symbol = &trimmed[1..end];
    let colors = parse_mana_color_symbol_set(symbol)?;
    Some((colors, &trimmed[end + 1..]))
}

fn parse_mana_color_symbol_set(symbol: &str) -> Option<Vec<ManaColor>> {
    fn parse_single(code: &str) -> Option<ManaColor> {
        match code {
            "W" => Some(ManaColor::White),
            "U" => Some(ManaColor::Blue),
            "B" => Some(ManaColor::Black),
            "R" => Some(ManaColor::Red),
            "G" => Some(ManaColor::Green),
            _ => None,
        }
    }

    let symbol = symbol.trim().to_ascii_uppercase();
    if let Some(color) = parse_single(&symbol) {
        return Some(vec![color]);
    }

    let mut colors = Vec::new();
    for part in symbol.split('/') {
        let color = parse_single(part.trim())?;
        if !colors.contains(&color) {
            colors.push(color);
        }
    }

    if colors.is_empty() {
        None
    } else {
        Some(colors)
    }
}

fn all_mana_colors() -> Vec<ManaColor> {
    vec![
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
}

/// Parse a compound effect chain: split on ". " or ".\n" boundaries and ", then ".
/// Returns an AbilityDefinition with sub_ability chain for compound effects.
pub fn parse_effect_chain(text: &str, kind: AbilityKind) -> AbilityDefinition {
    let sentences = split_effect_sentences(text);
    let mut defs: Vec<AbilityDefinition> = sentences
        .iter()
        .map(|s| {
            let (condition, text) = strip_additional_cost_conditional(s);
            let clause = parse_effect_clause(&text);
            AbilityDefinition {
                kind,
                effect: clause.effect,
                cost: None,
                sub_ability: None,
                duration: clause.duration,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
                condition,
            }
        })
        .collect();

    // For SearchLibrary: inject ChangeZone sub_ability for the destination,
    // parsed from the original search sentence text.
    if !defs.is_empty() && matches!(defs[0].effect, Effect::SearchLibrary { .. }) {
        let search_text = &sentences[0];
        let lower = search_text.to_lowercase();
        let destination = parse_search_destination(&lower);
        let change_zone = AbilityDefinition {
            kind,
            effect: Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination,
                target: TargetFilter::Any,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
            condition: None,
        };
        // Insert ChangeZone as second element (between search and shuffle)
        defs.insert(1, change_zone);
    }

    // For RevealHand: absorb "choose X card from it" sentences as the card_filter
    // on the preceding RevealHand, then remove the redundant definition.
    // "look at target opponent's hand. You may exile a nonland card from it" →
    //   RevealHand gets card_filter: nonland, ChangeZone stays as sub_ability.
    // "reveals their hand. You choose an artifact or creature card from it. Exile that card." →
    //   RevealHand gets card_filter: Or[Artifact, Creature], ChangeZone stays as sub_ability.
    if !defs.is_empty() && matches!(defs[0].effect, Effect::RevealHand { .. }) {
        let mut absorbed_index = None;
        for (idx, sentence) in sentences.iter().enumerate().skip(1) {
            let sub_lower = sentence.to_lowercase();
            if sub_lower.contains("card from it") || sub_lower.contains("card from among") {
                // Choose between sentence-level filter and the independently parsed def's filter
                let card_filter =
                    if sub_lower.starts_with("you choose ") || sub_lower.starts_with("choose ") {
                        // "You choose an artifact or creature card from it" — use parse_choose_filter
                        // which handles "or" conjunctions properly
                        parse_choose_filter(&sub_lower)
                    } else {
                        parse_choose_filter_from_sentence(&sub_lower)
                    };
                if let Effect::RevealHand {
                    card_filter: ref mut cf,
                    ..
                } = defs[0].effect
                {
                    *cf = card_filter;
                }
                // Mark this sentence's def for removal — it was absorbed into the RevealHand
                if idx < defs.len() && matches!(defs[idx].effect, Effect::RevealHand { .. }) {
                    absorbed_index = Some(idx);
                }
                break;
            }
        }
        if let Some(idx) = absorbed_index {
            defs.remove(idx);
        }
    }

    // Chain: last has no sub_ability, each earlier one chains to next
    if defs.len() > 1 {
        let last = defs.pop().unwrap();
        let mut chain = last;
        while let Some(mut prev) = defs.pop() {
            prev.sub_ability = Some(Box::new(chain));
            chain = prev;
        }
        chain
    } else {
        defs.pop().unwrap_or_else(|| AbilityDefinition {
            kind,
            effect: Effect::Unimplemented {
                name: "empty".to_string(),
                description: None,
            },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
            condition: None,
        })
    }
}

fn split_effect_sentences(text: &str) -> Vec<String> {
    text.replace(", then ", ". ")
        .split(". ")
        .map(|s| s.trim().trim_end_matches('.').trim())
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

// --- Search library parser ---

/// Parse "search your library for X" Oracle text into Effect::SearchLibrary.
///
/// Extracts the card filter from the "for a/an <type> card" clause,
/// detects reveal, and identifies the destination from "put it into/onto" text.
/// The destination and shuffle are handled by `parse_effect_chain`'s sentence
/// splitting — ", then shuffle" becomes a chained sub_ability automatically.
fn parse_search_library(_text: &str, lower: &str) -> Effect {
    // Extract what we're searching for: "for a <type> card" or "for a card"
    let filter = if let Some(for_idx) = lower.find("for a ") {
        let after_for = &lower[for_idx + 6..]; // skip "for a "
        parse_search_filter(after_for)
    } else if let Some(for_idx) = lower.find("for an ") {
        let after_for = &lower[for_idx + 7..]; // skip "for an "
        parse_search_filter(after_for)
    } else {
        TargetFilter::Any
    };

    let reveal = lower.contains("reveal");
    let count = if lower.contains("up to two") {
        2
    } else if lower.contains("up to three") {
        3
    } else {
        1
    };

    Effect::SearchLibrary {
        filter,
        count,
        reveal,
    }
}

/// Parse the card type filter from search text like "basic land card, ..."
/// or "creature card with ..." into a TargetFilter.
fn parse_search_filter(text: &str) -> TargetFilter {
    // Find the end of the type description (before comma, period, or "and put")
    let type_end = text
        .find(',')
        .or_else(|| text.find('.'))
        .or_else(|| text.find(" and put"))
        .or_else(|| text.find(" and shuffle"))
        .unwrap_or(text.len());
    let type_text = text[..type_end].trim();

    // Strip trailing "card" or "cards"
    let type_text = type_text
        .strip_suffix(" cards")
        .or_else(|| type_text.strip_suffix(" card"))
        .unwrap_or(type_text)
        .trim();

    // Check for "a card" / "card" alone (Demonic Tutor pattern)
    if type_text == "card" || type_text.is_empty() {
        return TargetFilter::Any;
    }

    // Check for "basic land" pattern
    let is_basic = type_text.contains("basic");
    let clean = type_text.replace("basic ", "");

    // Map type name to TypeFilter
    let card_type = match clean.trim() {
        "land" => Some(TypeFilter::Land),
        "creature" => Some(TypeFilter::Creature),
        "artifact" => Some(TypeFilter::Artifact),
        "enchantment" => Some(TypeFilter::Enchantment),
        "instant" => Some(TypeFilter::Instant),
        "sorcery" => Some(TypeFilter::Sorcery),
        "planeswalker" => Some(TypeFilter::Planeswalker),
        "instant or sorcery" => {
            let mut properties = vec![];
            if is_basic {
                properties.push(FilterProp::HasSupertype {
                    value: "Basic".to_string(),
                });
            }
            return TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Instant),
                        subtype: None,
                        controller: None,
                        properties: properties.clone(),
                    },
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Sorcery),
                        subtype: None,
                        controller: None,
                        properties,
                    },
                ],
            };
        }
        other => {
            // Could be a subtype search: "forest card", "plains card", "equipment card"
            let land_subtypes = ["plains", "island", "swamp", "mountain", "forest"];
            if land_subtypes.contains(&other) {
                let mut properties = vec![];
                if is_basic {
                    properties.push(FilterProp::HasSupertype {
                        value: "Basic".to_string(),
                    });
                }
                return TargetFilter::Typed {
                    card_type: Some(TypeFilter::Land),
                    subtype: Some(capitalize(other)),
                    controller: None,
                    properties,
                };
            }
            if other == "equipment" {
                return TargetFilter::Typed {
                    card_type: Some(TypeFilter::Artifact),
                    subtype: Some("Equipment".to_string()),
                    controller: None,
                    properties: vec![],
                };
            }
            if other == "aura" {
                return TargetFilter::Typed {
                    card_type: Some(TypeFilter::Enchantment),
                    subtype: Some("Aura".to_string()),
                    controller: None,
                    properties: vec![],
                };
            }
            // Fallback: treat as Any
            return TargetFilter::Any;
        }
    };

    let mut properties = vec![];
    if is_basic {
        properties.push(FilterProp::HasSupertype {
            value: "Basic".to_string(),
        });
    }

    TargetFilter::Typed {
        card_type,
        subtype: None,
        controller: None,
        properties,
    }
}

/// Parse the destination zone from search Oracle text.
/// Looks for "put it into your hand", "put it onto the battlefield", etc.
fn parse_search_destination(lower: &str) -> Zone {
    if lower.contains("onto the battlefield") {
        Zone::Battlefield
    } else if contains_possessive(lower, "into", "hand") {
        Zone::Hand
    } else if contains_possessive(lower, "on top of", "library") {
        Zone::Library
    } else if contains_possessive(lower, "into", "graveyard") {
        Zone::Graveyard
    } else {
        // Default destination for tutors is hand
        Zone::Hand
    }
}

/// Capitalize the first letter of a string (for subtype names).
fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

// --- Helper parsers ---

fn parsed_clause(effect: Effect) -> ParsedEffectClause {
    ParsedEffectClause {
        effect,
        duration: None,
    }
}

fn with_clause_duration(mut clause: ParsedEffectClause, duration: Duration) -> ParsedEffectClause {
    if clause.duration.is_none() {
        clause.duration = Some(duration.clone());
    }
    if let Effect::GenericEffect {
        duration: effect_duration,
        ..
    } = &mut clause.effect
    {
        if effect_duration.is_none() {
            *effect_duration = Some(duration);
        }
    }
    clause
}

fn strip_leading_conditional(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if !lower.starts_with("if ") {
        return None;
    }

    let mut paren_depth = 0u32;
    let mut in_quotes = false;

    for (idx, ch) in text.char_indices() {
        match ch {
            '"' => in_quotes = !in_quotes,
            '(' if !in_quotes => paren_depth += 1,
            ')' if !in_quotes => paren_depth = paren_depth.saturating_sub(1),
            ',' if !in_quotes && paren_depth == 0 => {
                let rest = text[idx + 1..].trim();
                if !rest.is_empty() {
                    return Some(rest.to_string());
                }
            }
            _ => {}
        }
    }

    None
}

/// Detect "if this spell's additional cost was paid, {effect}" and return
/// the condition + remaining effect text. Called at the sentence level in
/// parse_effect_chain BEFORE parse_effect_clause, so the condition is preserved
/// rather than being discarded by strip_leading_conditional.
fn strip_additional_cost_conditional(text: &str) -> (Option<AbilityCondition>, String) {
    let lower = text.to_lowercase();
    if let Some(rest) = lower.strip_prefix("if this spell's additional cost was paid, ") {
        let offset = text.len() - rest.len();
        (
            Some(AbilityCondition::AdditionalCostPaid),
            text[offset..].to_string(),
        )
    } else {
        (None, text.to_string())
    }
}

fn strip_leading_duration(text: &str) -> Option<(Duration, &str)> {
    let lower = text.to_lowercase();
    for (prefix, duration) in [
        ("until end of turn, ", Duration::UntilEndOfTurn),
        ("until your next turn, ", Duration::UntilYourNextTurn),
    ] {
        if lower.starts_with(prefix) {
            return Some((duration, text[prefix.len()..].trim()));
        }
    }
    None
}

fn strip_trailing_duration(text: &str) -> (&str, Option<Duration>) {
    let lower = text.to_lowercase();
    for (suffix, duration) in [
        (" this turn", Duration::UntilEndOfTurn),
        (" until end of turn", Duration::UntilEndOfTurn),
        (" until your next turn", Duration::UntilYourNextTurn),
        (
            " until ~ leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
        (
            " until this creature leaves the battlefield",
            Duration::UntilHostLeavesPlay,
        ),
    ] {
        if lower.ends_with(suffix) {
            let end = text.len() - suffix.len();
            return (text[..end].trim_end_matches(',').trim(), Some(duration));
        }
    }
    (text, None)
}

fn try_parse_subject_continuous_clause(text: &str) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = text[verb_start..].trim();
    let application = parse_subject_application(subject)?;
    build_continuous_clause(application, predicate)
}

fn try_parse_subject_become_clause(text: &str) -> Option<ParsedEffectClause> {
    let verb_start = find_predicate_start(text)?;
    let subject = text[..verb_start].trim();
    let predicate = deconjugate_verb(text[verb_start..].trim());
    if !predicate.to_lowercase().starts_with("become ") {
        return None;
    }
    let application = parse_subject_application(subject)?;
    build_become_clause(application, &predicate)
}

fn try_parse_subject_restriction_clause(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    let (subject, predicate) = if let Some(pos) = lower.find(" can't ") {
        (text[..pos].trim(), text[pos + 1..].trim())
    } else if let Some(pos) = lower.find(" cannot ") {
        (text[..pos].trim(), text[pos + 1..].trim())
    } else {
        return None;
    };
    let application = parse_subject_application(subject)?;
    build_restriction_clause(application, predicate)
}

fn parse_subject_application(subject: &str) -> Option<SubjectApplication> {
    let lower = subject.to_lowercase();

    if lower.starts_with("target ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, true);
    }
    if lower.starts_with("all ") || lower.starts_with("each ") {
        let (filter, _) = parse_target(subject);
        return subject_filter_application(filter, false);
    }
    if lower.starts_with("enchanted creature")
        || lower.starts_with("enchanted permanent")
        || lower.starts_with("equipped creature")
    {
        let (filter, _) = parse_target(subject);
        return Some(SubjectApplication {
            affected: filter,
            target: None,
        });
    }
    if lower == "creatures you control" || lower == "other creatures you control" {
        return Some(SubjectApplication {
            affected: TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            },
            target: None,
        });
    }
    if matches!(
        lower.as_str(),
        "this"
            | "it"
            | "this card"
            | "this creature"
            | "this permanent"
            | "this artifact"
            | "this land"
    ) {
        return Some(SubjectApplication {
            affected: TargetFilter::SelfRef,
            target: None,
        });
    }

    None
}

fn subject_filter_application(filter: TargetFilter, targeted: bool) -> Option<SubjectApplication> {
    if matches!(filter, TargetFilter::Player | TargetFilter::Controller) {
        return None;
    }
    Some(SubjectApplication {
        target: targeted.then_some(filter.clone()),
        affected: filter,
    })
}

fn build_continuous_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    if let Some((power, toughness, duration)) = parse_pump_clause(&normalized) {
        let effect = if let Some(target) = application.target.clone() {
            Effect::Pump {
                power,
                toughness,
                target,
            }
        } else if application.affected == TargetFilter::SelfRef {
            Effect::Pump {
                power,
                toughness,
                target: TargetFilter::SelfRef,
            }
        } else {
            Effect::PumpAll {
                power,
                toughness,
                target: application.affected,
            }
        };
        return Some(ParsedEffectClause { effect, duration });
    }

    let (predicate, duration) = strip_trailing_duration(&normalized);
    let modifications = parse_continuous_modifications(predicate);
    if modifications.is_empty() {
        return None;
    }

    if let Some((power, toughness)) = extract_pump_modifiers(&modifications) {
        let effect = if let Some(target) = application.target.clone() {
            Effect::Pump {
                power,
                toughness,
                target,
            }
        } else if application.affected == TargetFilter::SelfRef {
            Effect::Pump {
                power,
                toughness,
                target: TargetFilter::SelfRef,
            }
        } else {
            Effect::PumpAll {
                power,
                toughness,
                target: application.affected,
            }
        };
        return Some(ParsedEffectClause { effect, duration });
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(application.affected),
                modifications,
                condition: None,
                affected_zone: None,
                effect_zone: None,
                characteristic_defining: false,
                description: Some(predicate.to_string()),
            }],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
    })
}

fn build_become_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = strip_trailing_duration(&normalized);
    let become_text = predicate.strip_prefix("become ")?.trim();
    let animation = parse_animation_spec(become_text)?;
    let modifications = animation_modifications(&animation);
    if modifications.is_empty() {
        return None;
    }

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(application.affected),
                modifications,
                condition: None,
                affected_zone: None,
                effect_zone: None,
                characteristic_defining: false,
                description: Some(predicate.to_string()),
            }],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
    })
}

fn build_restriction_clause(
    application: SubjectApplication,
    predicate: &str,
) -> Option<ParsedEffectClause> {
    let normalized = deconjugate_verb(predicate);
    let (predicate, duration) = strip_trailing_duration(&normalized);
    let lower = predicate.to_lowercase();

    let mode = if matches!(lower.as_str(), "can't block" | "cannot block") {
        StaticMode::CantBlock
    } else if matches!(lower.as_str(), "can't be blocked" | "cannot be blocked") {
        StaticMode::Other("CantBeBlocked".to_string())
    } else {
        return None;
    };

    Some(ParsedEffectClause {
        effect: Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode,
                affected: Some(application.affected),
                modifications: vec![],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                characteristic_defining: false,
                description: Some(predicate.to_string()),
            }],
            duration: duration.clone(),
            target: application.target,
        },
        duration,
    })
}

fn extract_pump_modifiers(
    modifications: &[crate::types::ability::ContinuousModification],
) -> Option<(PtValue, PtValue)> {
    let mut power = None;
    let mut toughness = None;

    for modification in modifications {
        match modification {
            crate::types::ability::ContinuousModification::AddPower { value } => {
                power = Some(PtValue::Fixed(*value));
            }
            crate::types::ability::ContinuousModification::AddToughness { value } => {
                toughness = Some(PtValue::Fixed(*value));
            }
            _ => return None,
        }
    }

    Some((power?, toughness?))
}

/// Detect "its controller gains life equal to its power" and similar patterns where
/// the targeted permanent's controller gains life based on the permanent's stats.
fn try_parse_targeted_controller_gain_life(text: &str) -> Option<ParsedEffectClause> {
    let lower = text.to_lowercase();
    if !lower.starts_with("its controller ") {
        return None;
    }
    if !lower.contains("gain") || !lower.contains("life") {
        return None;
    }
    let amount = if lower.contains("equal to its power") || lower.contains("its power") {
        LifeAmount::TargetPower
    } else {
        // Try to parse a fixed amount: "its controller gains 3 life"
        let after = &lower["its controller ".len()..];
        let after = after
            .strip_prefix("gains ")
            .or_else(|| after.strip_prefix("gain "))
            .unwrap_or(after);
        LifeAmount::Fixed(parse_number(after).map(|(n, _)| n as i32).unwrap_or(1))
    };
    Some(parsed_clause(Effect::GainLife {
        amount,
        player: GainLifePlayer::TargetedController,
    }))
}

fn strip_subject_clause(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    if !starts_with_subject_prefix(&lower) {
        return None;
    }

    let verb_start = find_predicate_start(text)?;
    let predicate = text[verb_start..].trim();
    if predicate.is_empty() {
        return None;
    }

    Some(deconjugate_verb(predicate))
}

fn try_parse_damage(lower: &str, _text: &str) -> Option<Effect> {
    // Match: "~ deals N damage to {target}" / "deal N damage to {target}"
    // and variable forms like "deal that much damage" or
    // "deal damage equal to its power".
    let pos = lower.find("deals ").or_else(|| lower.find("deal "))?;
    let verb_len = if lower[pos..].starts_with("deals ") {
        6
    } else {
        5
    };
    let after = &_text[pos + verb_len..];
    let after_lower = &lower[pos + verb_len..];

    let (amount, after_target) = if let Some((n, rest)) = parse_number(after_lower) {
        if rest.starts_with("damage") {
            (
                DamageAmount::Fixed(n as i32),
                &after[after.len() - rest.len() + "damage".len()..],
            )
        } else {
            return None;
        }
    } else if after_lower.starts_with("that much damage") {
        (
            DamageAmount::Variable("that much".to_string()),
            &after["that much damage".len()..],
        )
    } else if after_lower.starts_with("damage equal to ") {
        let amount_text = &after["damage equal to ".len()..];
        let to_pos = amount_text.to_lowercase().find(" to ")?;
        (
            DamageAmount::Variable(amount_text[..to_pos].trim().to_string()),
            &amount_text[to_pos + 4..],
        )
    } else {
        return None;
    };

    let after_to = after_target
        .trim()
        .strip_prefix("to ")
        .unwrap_or(after_target)
        .trim();
    if after_to.starts_with("each ") {
        let (target, _) = parse_target(after_to);
        return Some(Effect::DamageAll { amount, target });
    }

    let (target, _) = parse_target(after_to);
    Some(Effect::DealDamage { amount, target })
}

fn try_parse_pump(lower: &str, text: &str) -> Option<Effect> {
    // Match "+N/+M", "+X/+0", "-X/-X", etc.
    let re_pos = lower.find("gets ").or_else(|| lower.find("get "))?;
    let offset = if lower[re_pos..].starts_with("gets ") {
        5
    } else {
        4
    };
    let after = text[re_pos + offset..].trim();
    let token_end = after
        .find(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .unwrap_or(after.len());
    let token = &after[..token_end];
    parse_pt_modifier(token).map(|(power, toughness)| Effect::Pump {
        power,
        toughness,
        target: TargetFilter::Any,
    })
}

fn parse_pump_clause(predicate: &str) -> Option<(PtValue, PtValue, Option<Duration>)> {
    let (without_where, where_x_expression) = strip_trailing_where_x(predicate);
    let (without_duration, duration) = strip_trailing_duration(without_where);
    let lower = without_duration.to_lowercase();

    let after = if lower.starts_with("gets ") {
        &without_duration[5..]
    } else if lower.starts_with("get ") {
        &without_duration[4..]
    } else {
        return None;
    }
    .trim_start();

    let token_end = after
        .find(|c: char| c.is_whitespace() || c == ',' || c == '.')
        .unwrap_or(after.len());
    let token = &after[..token_end];
    let trailing = after[token_end..]
        .trim_start_matches(|c: char| c == ',' || c.is_whitespace())
        .trim();
    if !trailing.is_empty() {
        return None;
    }

    let (power, toughness) = parse_pt_modifier(token)?;
    let power = apply_where_x_expression(power, where_x_expression.as_deref());
    let toughness = apply_where_x_expression(toughness, where_x_expression.as_deref());

    Some((power, toughness, duration))
}

fn strip_trailing_where_x(text: &str) -> (&str, Option<String>) {
    let lower = text.to_lowercase();
    for needle in [", where x is ", " where x is "] {
        if let Some(pos) = lower.find(needle) {
            let expression = text[pos + needle.len()..]
                .trim()
                .trim_end_matches('.')
                .trim()
                .to_string();
            if expression.is_empty() {
                return (text, None);
            }
            return (
                text[..pos].trim_end_matches(',').trim_end(),
                Some(expression),
            );
        }
    }
    (text, None)
}

fn apply_where_x_expression(value: PtValue, where_x_expression: Option<&str>) -> PtValue {
    match (value, where_x_expression) {
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("X") => {
            PtValue::Variable(expression.to_string())
        }
        (PtValue::Variable(alias), Some(expression)) if alias.eq_ignore_ascii_case("-X") => {
            PtValue::Variable(format!("-({expression})"))
        }
        (value, _) => value,
    }
}

fn parse_pt_modifier(text: &str) -> Option<(PtValue, PtValue)> {
    let token = text.trim();
    let slash = token.find('/')?;
    let power = parse_signed_pt_component(token[..slash].trim())?;
    let toughness = parse_signed_pt_component(token[slash + 1..].trim())?;
    Some((power, toughness))
}

fn parse_signed_pt_component(text: &str) -> Option<PtValue> {
    let text = text.trim();
    if text.is_empty() {
        return None;
    }

    let (sign, body) = if let Some(rest) = text.strip_prefix('+') {
        (1, rest.trim())
    } else if let Some(rest) = text.strip_prefix('-') {
        (-1, rest.trim())
    } else {
        (1, text)
    };

    if body.eq_ignore_ascii_case("x") {
        return Some(if sign < 0 {
            PtValue::Variable("-X".to_string())
        } else {
            PtValue::Variable("X".to_string())
        });
    }

    let value = body.parse::<i32>().ok()?;
    Some(PtValue::Fixed(sign * value))
}

fn try_parse_put_counter(lower: &str, _text: &str) -> Option<Effect> {
    // "put N {type} counter(s) on {target}"
    let after_put = lower[4..].trim();
    let (count, rest) = parse_number(after_put)?;
    // Next word is counter type (e.g. "+1/+1", "loyalty", "charge")
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let raw_type = &rest[..type_end];
    let counter_type = normalize_counter_type(raw_type);

    // Skip "counter" or "counters" keyword, then parse target after "on"
    let after_type = rest[type_end..].trim_start();
    let after_counter_word = after_type
        .strip_prefix("counters")
        .or_else(|| after_type.strip_prefix("counter"))
        .map(|s| s.trim_start())
        .unwrap_or(after_type);

    let target = if let Some(target_text) = after_counter_word.strip_prefix("on ") {
        if target_text.starts_with("this ")
            || target_text.starts_with("~")
            || target_text == "it"
            || target_text.starts_with("it ")
            || target_text.starts_with("itself")
        {
            TargetFilter::SelfRef
        } else {
            parse_target(target_text).0
        }
    } else {
        TargetFilter::SelfRef
    };

    Some(Effect::PutCounter {
        counter_type,
        count: count as i32,
        target,
    })
}

/// Normalize oracle-text counter type strings to canonical engine names.
fn normalize_counter_type(raw: &str) -> String {
    match raw {
        "+1/+1" => "P1P1".to_string(),
        "-1/-1" => "M1M1".to_string(),
        other => other.to_string(),
    }
}

fn try_parse_token(_lower: &str, text: &str) -> Option<Effect> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // "create a token that's a copy of {target}"
    if lower.contains("token that's a copy of") || lower.contains("token thats a copy of") {
        let copy_pos = lower.find("copy of ").map(|p| p + 8).unwrap_or(lower.len());
        let (target, _) = parse_target(&text[copy_pos..]);
        return Some(Effect::CopySpell { target });
    }

    let after = text[7..].trim();
    let token = parse_token_description(after)?;
    Some(Effect::Token {
        name: token.name,
        power: token.power.unwrap_or(PtValue::Fixed(0)),
        toughness: token.toughness.unwrap_or(PtValue::Fixed(0)),
        types: token.types,
        colors: token.colors,
        keywords: token.keywords,
        tapped: token.tapped,
        count: token.count,
    })
}

fn try_parse_put_zone_change(lower: &str, text: &str) -> Option<Effect> {
    let after_put = &text[4..];
    let after_put_lower = &lower[4..];

    for (needle, destination) in [
        (" onto the battlefield", Zone::Battlefield),
        (" into your hand", Zone::Hand),
        (" into its owner's hand", Zone::Hand),
        (" into their owner's hand", Zone::Hand),
        (" into your graveyard", Zone::Graveyard),
        (" into its owner's graveyard", Zone::Graveyard),
        (" into their owner's graveyard", Zone::Graveyard),
        (" on the bottom of", Zone::Library),
        (" on top of", Zone::Library),
    ] {
        if let Some(pos) = after_put_lower.find(needle) {
            let target_text = after_put[..pos].trim();
            if target_text.is_empty() {
                return None;
            }
            let (target, _) = parse_target(target_text);
            return Some(Effect::ChangeZone {
                origin: infer_origin_zone(after_put_lower),
                destination,
                target,
            });
        }
    }

    None
}

fn infer_origin_zone(lower: &str) -> Option<Zone> {
    if contains_possessive(lower, "from", "graveyard") || lower.contains("from a graveyard") {
        Some(Zone::Graveyard)
    } else if lower.contains("from exile") {
        Some(Zone::Exile)
    } else if contains_possessive(lower, "from", "hand") {
        Some(Zone::Hand)
    } else if contains_possessive(lower, "from", "library") {
        Some(Zone::Library)
    } else {
        None
    }
}

fn parse_token_description(text: &str) -> Option<TokenDescription> {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();
    if lower.contains(" attached to ") {
        return None;
    }

    let (mut count, leading_name, mut rest) =
        if let Some((count, rest)) = parse_token_count_prefix(text) {
            (count, None, rest)
        } else if let Some((name, rest)) = parse_named_token_preamble(text) {
            (CountValue::Fixed(1), Some(name), rest)
        } else {
            return None;
        };
    let mut tapped = false;

    loop {
        let trimmed = rest.trim_start();
        if let Some(stripped) = trimmed.strip_prefix("tapped ") {
            tapped = true;
            rest = stripped;
            continue;
        }
        if let Some(stripped) = trimmed.strip_prefix("untapped ") {
            rest = stripped;
            continue;
        }
        break;
    }

    rest = strip_token_supertypes(rest);

    let (mut power, mut toughness, rest) =
        if let Some((power, toughness, rest)) = parse_token_pt_prefix(rest) {
            (Some(power), Some(toughness), rest)
        } else {
            (None, None, rest)
        };

    let (colors, rest) = parse_token_color_prefix(rest);
    let (descriptor, suffix) = split_token_head(rest)?;
    let (name_override, suffix) = parse_token_name_clause(suffix);
    let keywords = parse_token_keyword_clause(suffix);
    let (mut name, types) = parse_token_identity(descriptor)?;

    if suffix.to_lowercase().contains(" attached to ") {
        return None;
    }

    if let Some(name_override) = leading_name.or(name_override) {
        name = name_override;
    }

    if let Some(where_expression) = extract_token_where_x_expression(suffix) {
        if matches!(&count, CountValue::Variable(alias) if alias == "X") {
            count = CountValue::Variable(where_expression.clone());
        }
        if matches!(&power, Some(PtValue::Variable(alias)) if alias == "X") {
            power = Some(PtValue::Variable(where_expression.clone()));
        }
        if matches!(&toughness, Some(PtValue::Variable(alias)) if alias == "X") {
            toughness = Some(PtValue::Variable(where_expression));
        }
    }

    if let Some(count_expression) = extract_token_count_expression(suffix) {
        if matches!(&count, CountValue::Variable(alias) if alias == "count") {
            count = CountValue::Variable(count_expression);
        }
    }

    if power.is_none() || toughness.is_none() {
        if let Some(pt_expression) = extract_token_pt_expression(suffix) {
            power = Some(PtValue::Variable(pt_expression.clone()));
            toughness = Some(PtValue::Variable(pt_expression));
        }
    }

    let is_creature = types.iter().any(|token_type| token_type == "Creature");
    if is_creature && (power.is_none() || toughness.is_none()) {
        return None;
    }

    Some(TokenDescription {
        name,
        power,
        toughness,
        types,
        colors,
        keywords,
        tapped,
        count,
    })
}

fn parse_token_count_prefix(text: &str) -> Option<(CountValue, &str)> {
    let trimmed = text.trim_start();
    let lower = trimmed.to_lowercase();
    if let Some(rest) = trimmed.strip_prefix("X ") {
        return Some((CountValue::Variable("X".to_string()), rest));
    }
    if let Some(rest) = trimmed.strip_prefix("x ") {
        return Some((CountValue::Variable("X".to_string()), rest));
    }
    if let Some(rest) = trimmed.strip_prefix("that many ") {
        return Some((CountValue::Variable("that many".to_string()), rest));
    }
    if let Some(rest) = trimmed.strip_prefix("a number of ") {
        return Some((CountValue::Variable("count".to_string()), rest));
    }
    let (count, rest) = parse_number(trimmed)?;
    if count == 0 && lower.starts_with('x') {
        return None;
    }
    Some((CountValue::Fixed(count), rest))
}

fn parse_named_token_preamble(text: &str) -> Option<(String, &str)> {
    let comma = text.find(',')?;
    let name = text[..comma].trim().trim_matches('"');
    if name.is_empty() {
        return None;
    }

    let after_comma = text[comma + 1..].trim_start();
    let rest = after_comma
        .strip_prefix("a ")
        .or_else(|| after_comma.strip_prefix("an "))?;
    Some((name.to_string(), rest))
}

fn parse_token_pt_prefix(text: &str) -> Option<(PtValue, PtValue, &str)> {
    let text = text.trim_start();
    let word_end = text.find(char::is_whitespace).unwrap_or(text.len());
    let token = &text[..word_end];
    let slash = token.find('/')?;
    let power = token[..slash].trim();
    let toughness = token[slash + 1..].trim();
    let power = parse_token_pt_component(power)?;
    let toughness = parse_token_pt_component(toughness)?;
    Some((power, toughness, text[word_end..].trim_start()))
}

fn parse_token_pt_component(text: &str) -> Option<PtValue> {
    if text.eq_ignore_ascii_case("x") {
        return Some(PtValue::Variable("X".to_string()));
    }
    text.parse::<i32>().ok().map(PtValue::Fixed)
}

fn strip_token_supertypes(mut text: &str) -> &str {
    loop {
        let trimmed = text.trim_start();
        let Some(stripped) = ["legendary ", "snow ", "basic "]
            .iter()
            .find_map(|prefix| trimmed.strip_prefix(prefix))
        else {
            return trimmed;
        };
        text = stripped;
    }
}

fn parse_token_color_prefix(mut text: &str) -> (Vec<ManaColor>, &str) {
    let mut colors = Vec::new();

    loop {
        let trimmed = text.trim_start();
        let Some((color, rest)) = strip_color_word(trimmed) else {
            break;
        };
        if let Some(color) = color {
            colors.push(color);
        }
        text = rest;

        let trimmed = text.trim_start();
        if let Some(rest) = trimmed.strip_prefix("and ") {
            text = rest;
            continue;
        }
        if let Some(rest) = trimmed.strip_prefix(", ") {
            text = rest;
            continue;
        }
        break;
    }

    (colors, text.trim_start())
}

fn strip_color_word(text: &str) -> Option<(Option<ManaColor>, &str)> {
    for (word, color) in [
        ("white", Some(ManaColor::White)),
        ("blue", Some(ManaColor::Blue)),
        ("black", Some(ManaColor::Black)),
        ("red", Some(ManaColor::Red)),
        ("green", Some(ManaColor::Green)),
        ("colorless", None),
    ] {
        if let Some(rest) = text.strip_prefix(word) {
            if rest.is_empty() || rest.starts_with(char::is_whitespace) {
                return Some((color, rest.trim_start()));
            }
        }
    }
    None
}

fn split_token_head(text: &str) -> Option<(&str, &str)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" token")?;
    let head = text[..pos].trim();
    let mut suffix = &text[pos + 6..];
    if let Some(stripped) = suffix.strip_prefix('s') {
        suffix = stripped;
    }
    if head.is_empty() {
        return None;
    }
    Some((head, suffix.trim()))
}

fn parse_token_name_clause(text: &str) -> (Option<String>, &str) {
    let trimmed = text.trim_start();
    let Some(after_named) = trimmed.strip_prefix("named ") else {
        return (None, trimmed);
    };

    let lower = after_named.to_lowercase();
    let mut end = after_named.len();
    for needle in [" with ", " attached ", ",", "."] {
        if let Some(pos) = lower.find(needle) {
            end = end.min(pos);
        }
    }

    let name = after_named[..end].trim().trim_matches('"');
    let rest = after_named[end..].trim_start();
    if name.is_empty() {
        (None, rest)
    } else {
        (Some(name.to_string()), rest)
    }
}

fn extract_token_where_x_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let pos = lower.find("where x is ")?;
    Some(
        text[pos + "where x is ".len()..]
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_count_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let pos = lower.find("equal to ")?;
    Some(
        text[pos + "equal to ".len()..]
            .trim()
            .trim_end_matches('.')
            .to_string(),
    )
}

fn extract_token_pt_expression(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    for needle in [
        "power and toughness are each equal to ",
        "power and toughness is each equal to ",
    ] {
        if let Some(pos) = lower.find(needle) {
            return Some(
                text[pos + needle.len()..]
                    .trim()
                    .trim_matches('"')
                    .trim_end_matches('.')
                    .to_string(),
            );
        }
    }
    None
}

fn parse_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "snow" | "legendary" | "basic" => {}
            _ => subtypes.push(title_case_word(word)),
        }
    }

    if core_types.is_empty() {
        return known_named_token_identity(descriptor);
    }

    let name = if subtypes.is_empty() {
        "Token".to_string()
    } else {
        subtypes.join(" ")
    };

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_owned(&mut types, subtype);
    }

    Some((name, types))
}

fn known_named_token_identity(descriptor: &str) -> Option<(String, Vec<String>)> {
    let name = match descriptor.trim().to_lowercase().as_str() {
        "treasure" => "Treasure",
        "food" => "Food",
        "clue" => "Clue",
        "blood" => "Blood",
        "map" => "Map",
        "powerstone" => "Powerstone",
        "junk" => "Junk",
        "shard" => "Shard",
        _ => return None,
    };

    Some((
        name.to_string(),
        vec!["Artifact".to_string(), name.to_string()],
    ))
}

fn parse_token_keyword_clause(text: &str) -> Vec<Keyword> {
    let trimmed = text.trim_start();
    let Some(after_with) = trimmed.strip_prefix("with ") else {
        return Vec::new();
    };

    let raw_clause = after_with
        .split('"')
        .next()
        .unwrap_or(after_with)
        .split(" where ")
        .next()
        .unwrap_or(after_with)
        .split(" attached ")
        .next()
        .unwrap_or(after_with)
        .trim()
        .trim_end_matches('.')
        .trim_end_matches(" and")
        .trim();

    split_token_keyword_list(raw_clause)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect()
}

fn split_token_keyword_list(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    for chunk in text.split(", and ") {
        for sub in chunk.split(" and ") {
            for item in sub.split(", ") {
                let trimmed = item.trim();
                if !trimmed.is_empty() {
                    parts.push(trimmed);
                }
            }
        }
    }
    parts
}

fn map_token_keyword(text: &str) -> Option<Keyword> {
    let trimmed = text.trim();
    if trimmed.eq_ignore_ascii_case("all creature types") {
        return Some(Keyword::Changeling);
    }
    match Keyword::from_str(trimmed) {
        Ok(Keyword::Unknown(_)) => None,
        Ok(keyword) => Some(keyword),
        Err(_) => None,
    }
}

fn title_case_word(word: &str) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

fn push_unique_string(values: &mut Vec<String>, value: &str) {
    if !values.iter().any(|existing| existing == value) {
        values.push(value.to_string());
    }
}

fn push_unique_owned(values: &mut Vec<String>, value: String) {
    if !values.iter().any(|existing| existing == &value) {
        values.push(value);
    }
}

fn parse_animation_spec(text: &str) -> Option<AnimationSpec> {
    let lower = text.to_lowercase();
    if lower.contains(" copy of ")
        || lower.contains(" of your choice")
        || lower.contains(" all activated abilities ")
        || lower.contains(" loses all other card types ")
        || lower.contains(" all colors")
    {
        return None;
    }

    let mut spec = AnimationSpec::default();
    let mut rest = text.trim().trim_end_matches('.');

    for suffix in [
        " and loses all other abilities",
        " and it loses all other abilities",
        " and loses all abilities",
    ] {
        if rest.to_lowercase().ends_with(suffix) {
            let end = rest.len() - suffix.len();
            rest = rest[..end].trim_end_matches(',').trim();
            spec.remove_all_abilities = true;
            break;
        }
    }

    if let Some(stripped) = rest.strip_prefix("a ") {
        rest = stripped;
    } else if let Some(stripped) = rest.strip_prefix("an ") {
        rest = stripped;
    }

    if let Some((power, toughness, after_pt)) = parse_fixed_become_pt_prefix(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = after_pt;
    }

    if let Some((descriptor, power, toughness)) = split_animation_base_pt_clause(rest) {
        spec.power = Some(power);
        spec.toughness = Some(toughness);
        rest = descriptor;
    }

    let (descriptor, keywords) = split_animation_keyword_clause(rest);
    spec.keywords = keywords;
    rest = descriptor;

    if let Some((colors, after_colors)) = parse_animation_color_prefix(rest) {
        spec.colors = Some(colors);
        rest = after_colors;
    }

    spec.types = parse_animation_types(rest, spec.power.is_some() || spec.toughness.is_some());

    if spec.power.is_none()
        && spec.toughness.is_none()
        && spec.colors.is_none()
        && spec.keywords.is_empty()
        && spec.types.is_empty()
        && !spec.remove_all_abilities
    {
        None
    } else {
        Some(spec)
    }
}

fn animation_modifications(
    spec: &AnimationSpec,
) -> Vec<crate::types::ability::ContinuousModification> {
    use crate::types::ability::ContinuousModification;
    use crate::types::card_type::CoreType;

    let mut modifications = Vec::new();

    if let Some(power) = spec.power {
        modifications.push(ContinuousModification::SetPower { value: power });
    }
    if let Some(toughness) = spec.toughness {
        modifications.push(ContinuousModification::SetToughness { value: toughness });
    }
    if let Some(colors) = &spec.colors {
        modifications.push(ContinuousModification::SetColor {
            colors: colors.clone(),
        });
    }
    if spec.remove_all_abilities {
        modifications.push(ContinuousModification::RemoveAllAbilities);
    }
    for keyword in &spec.keywords {
        modifications.push(ContinuousModification::AddKeyword {
            keyword: keyword.clone(),
        });
    }
    for type_name in &spec.types {
        if let Ok(core_type) = CoreType::from_str(type_name) {
            modifications.push(ContinuousModification::AddType { core_type });
        } else {
            modifications.push(ContinuousModification::AddSubtype {
                subtype: type_name.clone(),
            });
        }
    }

    modifications
}

fn parse_animation_color_prefix(text: &str) -> Option<(Vec<ManaColor>, &str)> {
    let mut rest = text.trim_start();
    let mut saw_color = false;
    let mut colors = Vec::new();

    loop {
        if let Some(stripped) = strip_prefix_word(rest, "colorless") {
            saw_color = true;
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "white") {
            saw_color = true;
            colors.push(ManaColor::White);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "blue") {
            saw_color = true;
            colors.push(ManaColor::Blue);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "black") {
            saw_color = true;
            colors.push(ManaColor::Black);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "red") {
            saw_color = true;
            colors.push(ManaColor::Red);
            rest = stripped;
        } else if let Some(stripped) = strip_prefix_word(rest, "green") {
            saw_color = true;
            colors.push(ManaColor::Green);
            rest = stripped;
        } else {
            break;
        }

        if let Some(stripped) = rest.strip_prefix("and ") {
            rest = stripped;
            continue;
        }
        break;
    }

    saw_color.then_some((colors, rest.trim_start()))
}

fn strip_prefix_word<'a>(text: &'a str, word: &str) -> Option<&'a str> {
    let rest = text.strip_prefix(word)?;
    if rest.is_empty() {
        Some(rest)
    } else if rest.starts_with(' ') {
        Some(rest.trim_start())
    } else {
        None
    }
}

fn parse_fixed_become_pt_prefix(text: &str) -> Option<(i32, i32, &str)> {
    let (power, toughness, rest) = parse_token_pt_prefix(text)?;
    match (power, toughness) {
        (PtValue::Fixed(power), PtValue::Fixed(toughness)) => Some((power, toughness, rest)),
        _ => None,
    }
}

fn split_animation_base_pt_clause(text: &str) -> Option<(&str, i32, i32)> {
    let lower = text.to_lowercase();
    let pos = lower.find(" with base power and toughness ")?;
    let descriptor = text[..pos].trim_end_matches(',').trim();
    let pt_text = text[pos + " with base power and toughness ".len()..].trim();
    let (power, toughness, _) = parse_fixed_become_pt_prefix(pt_text)?;
    Some((descriptor, power, toughness))
}

fn parse_animation_types(text: &str, infer_creature: bool) -> Vec<String> {
    let descriptor = text
        .trim()
        .trim_end_matches(',')
        .trim_end_matches(" in addition to its other types")
        .trim();
    if descriptor.is_empty() {
        return Vec::new();
    }

    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    for word in descriptor.split_whitespace() {
        match word.to_lowercase().as_str() {
            "artifact" => push_unique_string(&mut core_types, "Artifact"),
            "creature" => push_unique_string(&mut core_types, "Creature"),
            "enchantment" => push_unique_string(&mut core_types, "Enchantment"),
            "land" => push_unique_string(&mut core_types, "Land"),
            "planeswalker" => push_unique_string(&mut core_types, "Planeswalker"),
            "legendary" | "basic" | "snow" | "" => {}
            other => subtypes.push(title_case_word(other)),
        }
    }

    if core_types.is_empty() && subtypes.is_empty() {
        return Vec::new();
    }
    if core_types.is_empty() && infer_creature {
        push_unique_string(&mut core_types, "Creature");
    }

    let mut types = core_types;
    for subtype in subtypes {
        push_unique_owned(&mut types, subtype);
    }

    types
}

fn split_animation_keyword_clause(text: &str) -> (&str, Vec<Keyword>) {
    let lower = text.to_lowercase();
    let Some(pos) = lower.find(" with ") else {
        return (text, Vec::new());
    };

    let prefix = text[..pos].trim_end_matches(',').trim();
    let keyword_text = text[pos + 6..]
        .split('"')
        .next()
        .unwrap_or("")
        .trim()
        .trim_end_matches('.');
    let keywords = split_token_keyword_list(keyword_text)
        .into_iter()
        .filter_map(map_token_keyword)
        .collect();
    (prefix, keywords)
}

/// Strip third-person 's' from the first word: "discards a card" → "discard a card".
fn deconjugate_verb(text: &str) -> String {
    let text = text.trim();
    let first_space = text.find(' ').unwrap_or(text.len());
    let verb = &text[..first_space];
    let rest = &text[first_space..];
    let base = normalize_verb_token(verb);
    format!("{}{}", base, rest)
}

fn starts_with_subject_prefix(lower: &str) -> bool {
    [
        "all ",
        "each opponent ",
        "each player ",
        "enchanted ",
        "equipped ",
        "it ",
        "its controller ",
        "target ",
        "that ",
        "the chosen ",
        "they ",
        "this ",
        "those ",
        "you ",
    ]
    .iter()
    .any(|prefix| lower.starts_with(prefix))
}

fn find_predicate_start(text: &str) -> Option<usize> {
    const VERBS: &[&str] = &[
        "add",
        "attack",
        "become",
        "can",
        "cast",
        "choose",
        "copy",
        "counter",
        "create",
        "deal",
        "discard",
        "draw",
        "exile",
        "explore",
        "fight",
        "gain",
        "get",
        "have",
        "look",
        "lose",
        "mill",
        "pay",
        "put",
        "regenerate",
        "reveal",
        "return",
        "sacrifice",
        "scry",
        "search",
        "shuffle",
        "surveil",
        "tap",
        "transform",
        "untap",
    ];

    let lower = text.to_lowercase();
    let mut word_start = None;

    for (idx, ch) in lower.char_indices() {
        if ch.is_whitespace() {
            if let Some(start) = word_start.take() {
                let token = &lower[start..idx];
                if VERBS.contains(&normalize_verb_token(token).as_str()) {
                    return Some(start);
                }
            }
            continue;
        }

        if word_start.is_none() {
            word_start = Some(idx);
        }
    }

    if let Some(start) = word_start {
        let token = &lower[start..];
        if VERBS.contains(&normalize_verb_token(token).as_str()) {
            return Some(start);
        }
    }

    None
}

fn normalize_verb_token(token: &str) -> String {
    let token = token.trim_matches(|c: char| !c.is_alphabetic());
    match token {
        "does" => "do".to_string(),
        "has" => "have".to_string(),
        "is" => "be".to_string(),
        "copies" => "copy".to_string(),
        _ if token.ends_with('s') && !token.ends_with("ss") => token[..token.len() - 1].to_string(),
        _ => token.to_string(),
    }
}

fn extract_number_before(text: &str, before_word: &str) -> Option<u32> {
    let pos = text.find(before_word)?;
    let prefix = text[..pos].trim();
    let last_word = prefix.split_whitespace().last()?;
    last_word.parse::<u32>().ok()
}

fn constrain_filter_to_stack(filter: TargetFilter) -> TargetFilter {
    match filter {
        TargetFilter::Typed {
            card_type,
            subtype,
            controller,
            mut properties,
        } => {
            if !properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
            {
                properties.push(FilterProp::InZone { zone: Zone::Stack });
            }
            TargetFilter::Typed {
                card_type,
                subtype,
                controller,
                properties,
            }
        }
        TargetFilter::Or { filters } => TargetFilter::Or {
            filters: filters.into_iter().map(constrain_filter_to_stack).collect(),
        },
        TargetFilter::And { filters } => TargetFilter::And {
            filters: filters.into_iter().map(constrain_filter_to_stack).collect(),
        },
        other => other,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ContinuousModification, ManaProduction, TypeFilter};
    use crate::types::mana::ManaColor;

    #[test]
    fn effect_lightning_bolt() {
        let e = parse_effect("Lightning Bolt deals 3 damage to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any
            }
        ));
    }

    #[test]
    fn effect_murder() {
        let e = parse_effect("Destroy target creature");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }
            }
        ));
    }

    #[test]
    fn effect_giant_growth() {
        let e = parse_effect("Target creature gets +3/+3 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                ..
            }
        ));
    }

    #[test]
    fn effect_counterspell() {
        let e = parse_effect("Counter target spell");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Typed { properties, .. }
            } if properties
                .iter()
                .any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
        ));
    }

    #[test]
    fn effect_annul_has_stack_restricted_targets() {
        let e = parse_effect("Counter target artifact or enchantment spell");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Or { filters }
            } if filters.iter().all(|f| {
                matches!(
                    f,
                    TargetFilter::Typed { properties, .. }
                        if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
                )
            })
        ));
    }

    #[test]
    fn effect_disdainful_stroke_has_cmc_and_stack_restriction() {
        let e = parse_effect("Counter target spell with mana value 4 or greater");
        assert!(matches!(
            e,
            Effect::Counter {
                target: TargetFilter::Typed { properties, .. }
            } if properties.iter().any(|p| matches!(p, FilterProp::InZone { zone: Zone::Stack }))
                && properties.iter().any(|p| matches!(p, FilterProp::CmcGE { value: 4 }))
        ));
    }

    #[test]
    fn effect_mana_production() {
        let e = parse_effect("Add {W}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Fixed { ref colors }
            } if colors == &vec![ManaColor::White]
        ));
    }

    #[test]
    fn effect_gain_life() {
        let e = parse_effect("You gain 3 life");
        assert!(matches!(
            e,
            Effect::GainLife {
                amount: LifeAmount::Fixed(3),
                ..
            }
        ));
    }

    #[test]
    fn effect_bounce() {
        let e = parse_effect("Return target creature to its owner's hand");
        assert!(matches!(e, Effect::Bounce { .. }));
    }

    #[test]
    fn effect_draw() {
        let e = parse_effect("Draw two cards");
        assert!(matches!(e, Effect::Draw { count: 2 }));
    }

    #[test]
    fn effect_scry() {
        let e = parse_effect("Scry 2");
        assert!(matches!(e, Effect::Scry { count: 2 }));
    }

    #[test]
    fn effect_disenchant() {
        let e = parse_effect("Destroy target artifact or enchantment");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Or { .. }
            }
        ));
    }

    #[test]
    fn effect_explore() {
        let e = parse_effect("Explore");
        assert!(matches!(e, Effect::Explore));
    }

    #[test]
    fn effect_unimplemented_fallback() {
        let e = parse_effect("Fateseal 2");
        assert!(matches!(e, Effect::Unimplemented { .. }));
    }

    #[test]
    fn effect_chain_revitalize() {
        let def = parse_effect_chain("You gain 3 life. Draw a card.", AbilityKind::Spell);
        assert!(matches!(
            def.effect,
            Effect::GainLife {
                amount: LifeAmount::Fixed(3),
                ..
            }
        ));
        assert!(def.sub_ability.is_some());
        assert!(matches!(
            def.sub_ability.unwrap().effect,
            Effect::Draw { count: 1 }
        ));
    }

    #[test]
    fn effect_its_controller_gains_life_equal_to_power() {
        // Swords to Plowshares: "Its controller gains life equal to its power."
        let e = parse_effect("Its controller gains life equal to its power");
        assert!(
            matches!(
                e,
                Effect::GainLife {
                    amount: LifeAmount::TargetPower,
                    player: GainLifePlayer::TargetedController
                }
            ),
            "Expected TargetPower + TargetedController, got {e:?}"
        );
    }

    #[test]
    fn effect_chain_with_em_dash() {
        // Regression: em dash (U+2014, 3 bytes) must not cause a byte-boundary panic
        let def = parse_effect_chain(
            "Spell mastery — Draw two cards. You gain 2 life.",
            AbilityKind::Spell,
        );
        // First sentence contains the em dash, should parse (possibly as unimplemented)
        assert!(def.sub_ability.is_some());
    }

    #[test]
    fn effect_shuffle_library() {
        let e = parse_effect("Shuffle your library");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Controller,
            }
        ));
    }

    #[test]
    fn effect_shuffle_their_library() {
        let e = parse_effect("Shuffle their library");
        assert!(matches!(
            e,
            Effect::Shuffle {
                target: TargetFilter::Player,
            }
        ));
    }

    #[test]
    fn compound_shuffle_it_into_library() {
        let e = parse_effect("Shuffle it into its owner's library");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn compound_shuffle_graveyard_into_library() {
        let e = parse_effect("Shuffle your graveyard into your library");
        assert!(matches!(
            e,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Graveyard),
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn compound_shuffle_hand_into_library() {
        let e = parse_effect("Shuffle your hand into your library");
        assert!(matches!(
            e,
            Effect::ChangeZoneAll {
                origin: Some(Zone::Hand),
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn parse_search_basic_land_to_hand() {
        let e = parse_effect(
            "Search your library for a basic land card, reveal it, put it into your hand",
        );
        match e {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
            } => {
                assert_eq!(count, 1);
                assert!(reveal);
                match filter {
                    TargetFilter::Typed {
                        card_type,
                        properties,
                        ..
                    } => {
                        assert_eq!(card_type, Some(TypeFilter::Land));
                        assert!(properties.iter().any(|p| matches!(
                            p,
                            FilterProp::HasSupertype { value } if value == "Basic"
                        )));
                    }
                    other => panic!("Expected Typed filter, got {:?}", other),
                }
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_creature_to_hand() {
        let e = parse_effect(
            "Search your library for a creature card, reveal it, put it into your hand",
        );
        match e {
            Effect::SearchLibrary {
                filter,
                count,
                reveal,
            } => {
                assert_eq!(count, 1);
                assert!(reveal);
                assert!(matches!(
                    filter,
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Creature),
                        ..
                    }
                ));
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_any_card_to_hand() {
        let e = parse_effect("Search your library for a card, put it into your hand");
        match e {
            Effect::SearchLibrary { filter, count, .. } => {
                assert_eq!(count, 1);
                assert!(matches!(filter, TargetFilter::Any));
            }
            other => panic!("Expected SearchLibrary, got {:?}", other),
        }
    }

    #[test]
    fn parse_search_land_to_battlefield() {
        let e = parse_effect(
            "Search your library for a basic land card, put it onto the battlefield tapped",
        );
        assert!(matches!(e, Effect::SearchLibrary { .. }));
    }

    #[test]
    fn search_ability_chain_has_changezone_and_shuffle() {
        // Full Oracle text for a tutor like Worldly Tutor / Rampant Growth
        let def = parse_effect_chain(
            "Search your library for a creature card, reveal it, put it into your hand, then shuffle your library",
            AbilityKind::Spell,
        );
        // First effect: SearchLibrary
        assert!(
            matches!(def.effect, Effect::SearchLibrary { .. }),
            "Expected SearchLibrary, got {:?}",
            def.effect
        );

        // Second in chain: ChangeZone to Hand
        let sub1 = def
            .sub_ability
            .as_ref()
            .expect("Should have sub_ability (ChangeZone)");
        assert!(
            matches!(
                sub1.effect,
                Effect::ChangeZone {
                    destination: Zone::Hand,
                    ..
                }
            ),
            "Expected ChangeZone to Hand, got {:?}",
            sub1.effect
        );

        // Third in chain: Shuffle
        let sub2 = sub1
            .sub_ability
            .as_ref()
            .expect("Should have sub_ability (Shuffle)");
        assert!(
            matches!(sub2.effect, Effect::Shuffle { .. }),
            "Expected Shuffle, got {:?}",
            sub2.effect
        );
    }

    #[test]
    fn search_ability_chain_battlefield_destination() {
        let def = parse_effect_chain(
            "Search your library for a basic land card, put it onto the battlefield tapped, then shuffle your library",
            AbilityKind::Spell,
        );
        assert!(matches!(def.effect, Effect::SearchLibrary { .. }));

        let sub1 = def.sub_ability.as_ref().expect("ChangeZone sub");
        assert!(
            matches!(
                sub1.effect,
                Effect::ChangeZone {
                    destination: Zone::Battlefield,
                    ..
                }
            ),
            "Expected ChangeZone to Battlefield, got {:?}",
            sub1.effect
        );
    }

    #[test]
    fn effect_reveal_top_cards() {
        let e = parse_effect("Reveal the top 3 cards of your library");
        assert!(matches!(e, Effect::Dig { count: 3, .. }));
    }

    #[test]
    fn effect_prevent_damage() {
        let e = parse_effect("Prevent the next 3 damage");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "prevent"
        ));
    }

    #[test]
    fn effect_regenerate() {
        let e = parse_effect("Regenerate target creature");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "regenerate"
        ));
    }

    #[test]
    fn effect_copy_spell() {
        let e = parse_effect("Copy target spell");
        assert!(matches!(e, Effect::CopySpell { .. }));
    }

    #[test]
    fn effect_transform() {
        let e = parse_effect("Transform this creature");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "transform"
        ));
    }

    #[test]
    fn effect_attach() {
        let e = parse_effect("Attach this to target creature");
        assert!(matches!(e, Effect::Attach { .. }));
    }

    #[test]
    fn effect_each_opponent_discards() {
        let e = parse_effect("Each opponent discards a card");
        assert!(matches!(e, Effect::Discard { count: 1, .. }));
    }

    #[test]
    fn effect_you_may_draw() {
        let e = parse_effect("You may draw a card");
        assert!(matches!(e, Effect::Draw { count: 1 }));
    }

    #[test]
    fn effect_it_gets_pump() {
        let e = parse_effect("It gets +2/+2 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                ..
            }
        ));
    }

    #[test]
    fn effect_they_draw() {
        let e = parse_effect("They draw two cards");
        assert!(matches!(e, Effect::Draw { count: 2 }));
    }

    #[test]
    fn effect_add_mana_any_color() {
        let e = parse_effect("Add one mana of any color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(1),
                    ref color_options,
                }
            } if color_options == &vec![
                ManaColor::White,
                ManaColor::Blue,
                ManaColor::Black,
                ManaColor::Red,
                ManaColor::Green,
            ]
        ));
    }

    #[test]
    fn effect_add_three_mana_any_one_color() {
        let e = parse_effect("Add three mana of any one color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(3),
                    ..
                }
            }
        ));
    }

    #[test]
    fn effect_add_two_mana_in_any_combination_of_colors() {
        let e = parse_effect("Add two mana in any combination of colors");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: CountValue::Fixed(2),
                    ..
                }
            }
        ));
    }

    #[test]
    fn effect_add_one_mana_of_the_chosen_color() {
        let e = parse_effect("Add one mana of the chosen color");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::ChosenColor {
                    count: CountValue::Fixed(1)
                }
            }
        ));
    }

    #[test]
    fn effect_add_x_mana_any_one_color_with_where_clause() {
        let e =
            parse_effect("Add X mana of any one color, where X is the number of lands you control");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Variable(ref value),
                    ..
                }
            } if value == "the number of lands you control"
        ));
    }

    #[test]
    fn effect_add_x_mana_in_any_combination_of_constrained_colors() {
        let e = parse_effect("Add X mana in any combination of {U} and/or {R}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyCombination {
                    count: CountValue::Variable(ref value),
                    ref color_options,
                }
            } if value == "X"
                && color_options == &vec![ManaColor::Blue, ManaColor::Red]
        ));
    }

    #[test]
    fn effect_add_colorless_mana_symbol() {
        let e = parse_effect("Add {C}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: CountValue::Fixed(1)
                }
            }
        ));
    }

    #[test]
    fn effect_add_color_choice_symbols() {
        let e = parse_effect("add {r} or {g}");
        assert!(matches!(
            e,
            Effect::Mana {
                produced: ManaProduction::AnyOneColor {
                    count: CountValue::Fixed(1),
                    ref color_options,
                }
            } if color_options == &vec![ManaColor::Red, ManaColor::Green]
        ));
    }

    #[test]
    fn effect_put_top_cards_into_graveyard() {
        let e = parse_effect("Put the top 3 cards of your library into your graveyard");
        assert!(matches!(e, Effect::Mill { count: 3, .. }));
    }

    #[test]
    fn effect_create_colored_token() {
        let e = parse_effect("Create a 1/1 white Soldier creature token");
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: CountValue::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn effect_create_treasure_token() {
        let e = parse_effect("Create a Treasure token");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                ref types,
                power: PtValue::Fixed(0),
                toughness: PtValue::Fixed(0),
                count: CountValue::Fixed(1),
                ..
            } if name == "Treasure" && types == &vec!["Artifact".to_string(), "Treasure".to_string()]
        ));
    }

    #[test]
    fn effect_create_tapped_powerstone_token() {
        let e = parse_effect("Create a tapped Powerstone token");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                tapped: true,
                ..
            } if name == "Powerstone"
        ));
    }

    #[test]
    fn effect_create_multicolor_artifact_creature_token() {
        let e = parse_effect("Create a 2/1 white and black Inkling creature token with flying");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(1),
                ref colors,
                ref keywords,
                ..
            } if name == "Inkling"
                && colors == &vec![ManaColor::White, ManaColor::Black]
                && keywords == &vec![Keyword::Flying]
        ));
    }

    #[test]
    fn effect_create_named_artifact_token() {
        let e = parse_effect(
            "Create a colorless artifact token named Etherium Cell with \"{T}, Sacrifice this token: Add one mana of any color.\"",
        );
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                ref types,
                ..
            } if name == "Etherium Cell" && types == &vec!["Artifact".to_string()]
        ));
    }

    #[test]
    fn effect_create_attached_role_stays_unimplemented() {
        let e = parse_effect("Create a Monster Role token attached to target creature you control");
        assert!(matches!(
            e,
            Effect::Unimplemented { ref name, .. } if name == "create"
        ));
    }

    #[test]
    fn effect_create_named_legendary_creature_token() {
        let e = parse_effect("Create Voja, a legendary 2/2 green and white Wolf creature token");
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                ref colors,
                ref types,
                ..
            } if name == "Voja"
                && colors == &vec![ManaColor::Green, ManaColor::White]
                && types == &vec!["Creature".to_string(), "Wolf".to_string()]
        ));
    }

    #[test]
    fn effect_create_named_legendary_artifact_token() {
        let e = parse_effect(
            "Create Tamiyo's Notebook, a legendary colorless artifact token with \"{T}: Draw a card.\"",
        );
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                ref types,
                ..
            } if name == "Tamiyo's Notebook" && types == &vec!["Artifact".to_string()]
        ));
    }

    #[test]
    fn effect_create_variable_count_fixed_pt_token() {
        let e = parse_effect("Create X 1/1 white Soldier creature tokens");
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: CountValue::Variable(ref value),
                ..
            } if value == "X"
        ));
    }

    #[test]
    fn effect_create_variable_pt_token_from_where_clause() {
        let e = parse_effect(
            "Create an X/X green Ooze creature token, where X is the greatest power among creatures you control",
        );
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                count: CountValue::Fixed(1),
                ..
            } if value == "the greatest power among creatures you control"
                && value2 == "the greatest power among creatures you control"
        ));
    }

    #[test]
    fn effect_create_variable_named_artifact_tokens() {
        let e = parse_effect(
            "Create X Map tokens, where X is one plus the number of opponents who control an artifact",
        );
        assert!(matches!(
            e,
            Effect::Token {
                ref name,
                count: CountValue::Variable(ref value),
                ref types,
                ..
            } if name == "Map"
                && value == "one plus the number of opponents who control an artifact"
                && types == &vec!["Artifact".to_string(), "Map".to_string()]
        ));
    }

    #[test]
    fn effect_create_number_of_tokens_equal_to_expression() {
        let e = parse_effect(
            "Create a number of 1/1 red Goblin creature tokens equal to two plus the number of cards named Goblin Gathering in your graveyard",
        );
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                count: CountValue::Variable(ref value),
                ..
            } if value == "two plus the number of cards named Goblin Gathering in your graveyard"
        ));
    }

    #[test]
    fn effect_create_token_with_quoted_variable_pt() {
        let e = parse_effect(
            "Create a red Elemental creature token with trample and \"This token's power and toughness are each equal to the number of instant and sorcery cards in your graveyard\"",
        );
        assert!(matches!(
            e,
            Effect::Token {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                ref keywords,
                ..
            } if value == "the number of instant and sorcery cards in your graveyard"
                && value2 == "the number of instant and sorcery cards in your graveyard"
                && keywords == &vec![Keyword::Trample]
        ));
    }

    #[test]
    fn effect_that_creature_gets() {
        let e = parse_effect("That creature gets +1/+1 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(1),
                toughness: PtValue::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn effect_target_player_draws() {
        let e = parse_effect("Target player draws a card");
        assert!(matches!(e, Effect::Draw { count: 1 }));
    }

    #[test]
    fn effect_this_creature_gets() {
        let e = parse_effect("This creature gets +2/+2 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                ..
            }
        ));
    }

    #[test]
    fn effect_target_creature_gets_variable_pump() {
        let e = parse_effect("Target creature gets +X/+0 until end of turn");
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Fixed(0),
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                },
            } if value == "X"
        ));
    }

    #[test]
    fn effect_target_creature_gets_negative_variable_pump_with_where_clause() {
        let e = parse_effect(
            "Target creature gets -X/-X until end of turn, where X is the number of Elves you control",
        );
        assert!(matches!(
            e,
            Effect::Pump {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                },
            } if value == "-(the number of Elves you control)"
                && value2 == "-(the number of Elves you control)"
        ));
    }

    #[test]
    fn effect_creatures_you_control_get_variable_pump() {
        let e = parse_effect(
            "Creatures you control get +X/+X until end of turn, where X is the number of cards in your hand",
        );
        assert!(matches!(
            e,
            Effect::PumpAll {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Variable(ref value2),
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    controller: Some(ControllerRef::You),
                    ..
                },
            } if value == "the number of cards in your hand"
                && value2 == "the number of cards in your hand"
        ));
    }

    #[test]
    fn effect_chain_variable_pump_preserves_duration() {
        let def = parse_effect_chain(
            "Target creature gets +X/+0 until end of turn, where X is the number of creatures you control",
            AbilityKind::Spell,
        );
        assert_eq!(def.duration, Some(Duration::UntilEndOfTurn));
        assert!(matches!(
            def.effect,
            Effect::Pump {
                power: PtValue::Variable(ref value),
                toughness: PtValue::Fixed(0),
                ..
            } if value == "the number of creatures you control"
        ));
    }

    #[test]
    fn effect_if_kicked_destroys() {
        let e = parse_effect("if it was kicked, destroy target enchantment");
        assert!(matches!(
            e,
            Effect::Destroy {
                target: TargetFilter::Typed { .. }
            }
        ));
    }

    #[test]
    fn effect_target_creature_gains_keyword_uses_continuous_effect() {
        let e = parse_effect("Target creature gains flying until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                ..
            }
        ));
    }

    #[test]
    fn effect_all_creatures_gain_keywords_uses_continuous_effect() {
        let e = parse_effect("All creatures gain trample and haste until end of turn");
        assert!(matches!(e, Effect::GenericEffect { target: None, .. }));
    }

    #[test]
    fn effect_target_creature_becomes_blue_uses_continuous_effect() {
        let e = parse_effect("Target creature becomes blue until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].modifications.contains(&ContinuousModification::SetColor {
                    colors: vec![ManaColor::Blue],
                })
        ));
    }

    #[test]
    fn effect_self_becomes_colored_creature_with_keyword() {
        let e = parse_effect(
            "Until end of turn, this land becomes a 4/4 blue and black Shark creature with deathtouch",
        );
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::SelfRef)
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 4 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 4 })
                && static_abilities[0].modifications.contains(&ContinuousModification::SetColor {
                    colors: vec![ManaColor::Blue, ManaColor::Black],
                })
                && static_abilities[0].modifications.contains(
                    &ContinuousModification::AddKeyword {
                        keyword: Keyword::Deathtouch,
                    }
                )
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Creature,
                    })
                && static_abilities[0].modifications.contains(
                    &ContinuousModification::AddSubtype {
                        subtype: "Shark".to_string(),
                    }
                )
        ));
    }

    #[test]
    fn effect_land_becomes_creature_with_all_creature_types() {
        let e =
            parse_effect("This land becomes a 3/3 creature with vigilance and all creature types");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::SelfRef)
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 3 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 3 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Creature,
                    })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Vigilance,
                    })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddKeyword {
                        keyword: Keyword::Changeling,
                    })
        ));
    }

    #[test]
    fn effect_self_becomes_artifact_creature_with_base_pt() {
        let e =
            parse_effect("This artifact becomes a 2/2 Beast artifact creature until end of turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: None,
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].affected == Some(TargetFilter::SelfRef)
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetPower { value: 2 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::SetToughness { value: 2 })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Artifact,
                    })
                && static_abilities[0]
                    .modifications
                    .contains(&ContinuousModification::AddType {
                        core_type: crate::types::card_type::CoreType::Creature,
                    })
                && static_abilities[0].modifications.contains(
                    &ContinuousModification::AddSubtype {
                        subtype: "Beast".to_string(),
                    }
                )
        ));
    }

    #[test]
    fn effect_target_creature_cant_block_uses_rule_static() {
        let e = parse_effect("Target creature can't block this turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].mode == StaticMode::CantBlock
                && static_abilities[0].affected == Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    subtype: None,
                    controller: None,
                    properties: vec![],
                })
        ));
    }

    #[test]
    fn effect_target_creature_cant_be_blocked_uses_rule_static() {
        let e = parse_effect("Target creature can't be blocked this turn");
        assert!(matches!(
            e,
            Effect::GenericEffect {
                target: Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                }),
                static_abilities,
                ..
            } if static_abilities.len() == 1
                && static_abilities[0].mode
                    == StaticMode::Other("CantBeBlocked".to_string())
        ));
    }

    #[test]
    fn effect_chain_preserves_leading_duration_prefix() {
        let def = parse_effect_chain(
            "Until end of turn, target creature gains flying",
            AbilityKind::Spell,
        );
        assert_eq!(
            def.duration,
            Some(crate::types::ability::Duration::UntilEndOfTurn)
        );
        assert!(matches!(def.effect, Effect::GenericEffect { .. }));
    }

    #[test]
    fn effect_deal_damage_all_imperative() {
        let e = parse_effect("Deal 1 damage to each opponent");
        assert!(matches!(
            e,
            Effect::DamageAll {
                amount: DamageAmount::Fixed(1),
                ..
            }
        ));
    }

    #[test]
    fn effect_deal_that_much_damage() {
        let e = parse_effect("Deal that much damage to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: DamageAmount::Variable(ref value),
                ..
            } if value == "that much"
        ));
    }

    #[test]
    fn effect_deal_damage_equal_to_expression() {
        let e = parse_effect("Deal damage equal to its power to any target");
        assert!(matches!(
            e,
            Effect::DealDamage {
                amount: DamageAmount::Variable(ref value),
                ..
            } if value == "its power"
        ));
    }

    #[test]
    fn effect_put_target_on_bottom_of_library() {
        let e = parse_effect("Put target creature on the bottom of its owner's library");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Library,
                ..
            }
        ));
    }

    #[test]
    fn effect_put_card_onto_battlefield() {
        let e = parse_effect("Put a land card from your hand onto the battlefield");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                origin: Some(Zone::Hand),
                destination: Zone::Battlefield,
                ..
            }
        ));
    }

    #[test]
    fn effect_put_target_into_hand() {
        let e = parse_effect("Put target nonland permanent into your hand");
        assert!(matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Hand,
                ..
            }
        ));
    }

    #[test]
    fn put_counter_this_creature_is_self_ref() {
        // "Whenever you gain life, put a +1/+1 counter on this creature." (Ajani's Pridemate)
        let e = parse_effect("put a +1/+1 counter on this creature");
        assert!(matches!(
            e,
            Effect::PutCounter {
                counter_type: ref ct,
                count: 1,
                target: TargetFilter::SelfRef,
            } if ct == "P1P1"
        ));
    }

    #[test]
    fn put_counter_on_it_is_self_ref() {
        let e = parse_effect("put a +1/+1 counter on it");
        assert!(matches!(
            e,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn put_counter_on_itself_is_self_ref() {
        let e = parse_effect("put a +1/+1 counter on itself");
        assert!(matches!(
            e,
            Effect::PutCounter {
                target: TargetFilter::SelfRef,
                ..
            }
        ));
    }

    #[test]
    fn put_counter_on_target_creature_is_typed() {
        let e = parse_effect("put a +1/+1 counter on target creature");
        assert!(matches!(
            e,
            Effect::PutCounter {
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    ..
                },
                ..
            }
        ));
    }

    #[test]
    fn put_counter_normalizes_plus1plus1_type() {
        let e = parse_effect("put a +1/+1 counter on this creature");
        let Effect::PutCounter { counter_type, .. } = e else {
            panic!("Expected PutCounter");
        };
        assert_eq!(counter_type, "P1P1");
    }

    #[test]
    fn put_counter_normalizes_minus1minus1_type() {
        let e = parse_effect("put a -1/-1 counter on this creature");
        let Effect::PutCounter { counter_type, .. } = e else {
            panic!("Expected PutCounter");
        };
        assert_eq!(counter_type, "M1M1");
    }

    #[test]
    fn put_counter_generic_type_passthrough() {
        let e = parse_effect("put a charge counter on this permanent");
        let Effect::PutCounter { counter_type, .. } = e else {
            panic!("Expected PutCounter");
        };
        assert_eq!(counter_type, "charge");
    }

    #[test]
    fn parse_activate_only_condition_with_two_land_subtypes() {
        let e = parse_effect("Activate only if you control an Island or a Swamp.");
        assert!(matches!(
            e,
            Effect::Unimplemented {
                name,
                description: Some(description),
            } if name == "activate_only_if_controls_land_subtype_any" && description == "Island|Swamp"
        ));
    }

    #[test]
    fn parse_activate_only_condition_non_land_clause_falls_back() {
        let e = parse_effect("Activate only if you control a creature with power 4 or greater.");
        assert!(matches!(
            e,
            Effect::Unimplemented {
                name,
                description: Some(_),
            } if name == "activate"
        ));
    }

    // --- RevealHand / "look at" tests ---

    #[test]
    fn parse_look_at_target_opponent_hand() {
        let e = parse_effect("look at target opponent's hand");
        match e {
            Effect::RevealHand {
                target,
                card_filter,
            } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed {
                        card_type: None,
                        controller: Some(ControllerRef::Opponent),
                        ..
                    }
                ));
                assert_eq!(card_filter, TargetFilter::Any);
            }
            other => panic!("Expected RevealHand, got {:?}", other),
        }
    }

    #[test]
    fn parse_look_at_possessive_hand() {
        let e = parse_effect("look at your hand");
        assert!(matches!(
            e,
            Effect::RevealHand {
                target: TargetFilter::Any,
                card_filter: TargetFilter::Any,
            }
        ));
    }

    #[test]
    fn parse_deep_cavern_bat_chain() {
        let def = parse_effect_chain(
            "look at target opponent's hand. You may exile a nonland card from it until this creature leaves the battlefield",
            AbilityKind::Spell,
        );
        // First effect: RevealHand with opponent target and nonland card_filter
        match &def.effect {
            Effect::RevealHand {
                target,
                card_filter,
            } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed {
                        card_type: None,
                        controller: Some(ControllerRef::Opponent),
                        ..
                    }
                ));
                assert!(matches!(
                    card_filter,
                    TargetFilter::Typed {
                        properties,
                        ..
                    } if !properties.is_empty()
                ));
            }
            other => panic!("Expected RevealHand, got {:?}", other),
        }
        // Sub-ability: ChangeZone to Exile with duration
        let sub = def.sub_ability.as_ref().expect("should have sub_ability");
        assert!(matches!(
            sub.effect,
            Effect::ChangeZone {
                destination: Zone::Exile,
                ..
            }
        ));
        assert_eq!(sub.duration, Some(Duration::UntilHostLeavesPlay));
    }

    #[test]
    fn parse_choose_filter_nonland() {
        let filter = parse_choose_filter_from_sentence("exile a nonland card from it");
        assert!(matches!(
            filter,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Permanent),
                properties,
                ..
            } if properties.iter().any(|p| matches!(p, FilterProp::NonType { value } if value == "Land"))
        ));
    }

    #[test]
    fn parse_choose_filter_creature() {
        let filter = parse_choose_filter_from_sentence("exile a creature card from it");
        assert!(matches!(
            filter,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                ..
            }
        ));
    }

    #[test]
    fn trailing_duration_until_leaves() {
        let (rest, duration) =
            strip_trailing_duration("exile a card until ~ leaves the battlefield");
        assert_eq!(duration, Some(Duration::UntilHostLeavesPlay));
        assert_eq!(rest, "exile a card");
    }

    // --- "Choose" as targeting synonym ---

    #[test]
    fn choose_target_creature_parses_as_targeting() {
        // "Choose target creature" (Grave Strength first sentence)
        let e = parse_effect("Choose target creature");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_two_target_creatures() {
        // "Choose two target creatures controlled by the same player" (Incriminate)
        let e = parse_effect("Choose two target creatures controlled by the same player");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_up_to_two_target_permanent_cards() {
        // "Choose up to two target permanent cards in your graveyard" (Brought Back)
        let e =
            parse_effect("Choose up to two target permanent cards in your graveyard that were put there from the battlefield this turn");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_a_type_you_control() {
        // "Choose a Giant creature you control" (Crush Underfoot)
        let e = parse_effect("Choose a Giant creature you control");
        assert!(
            !matches!(e, Effect::Unimplemented { .. }),
            "expected parsed effect, got Unimplemented: {:?}",
            e
        );
    }

    #[test]
    fn choose_does_not_match_targeting_as_color() {
        // "Choose a color" should NOT be treated as targeting — it's a named choice
        let e = parse_effect("Choose a color");
        assert!(!matches!(e, Effect::TargetOnly { .. }));
        assert!(matches!(e, Effect::Choose { .. }));
    }

    #[test]
    fn choose_does_not_match_targeting_as_creature_type() {
        // "Choose a creature type" should NOT be treated as targeting — it's a named choice
        let e = parse_effect("Choose a creature type");
        assert!(!matches!(e, Effect::TargetOnly { .. }));
        assert!(matches!(e, Effect::Choose { .. }));
    }

    #[test]
    fn choose_card_from_it_still_works() {
        // "Choose a creature card from it" should still produce RevealHand
        let e = parse_effect("Choose a creature card from it");
        assert!(matches!(e, Effect::RevealHand { .. }));
    }

    #[test]
    fn is_choose_as_targeting_helper() {
        assert!(is_choose_as_targeting("target creature"));
        assert!(is_choose_as_targeting("target creature you control"));
        assert!(is_choose_as_targeting("up to two target creatures"));
        assert!(is_choose_as_targeting(
            "two target creatures controlled by the same player"
        ));
        assert!(is_choose_as_targeting("a giant creature you control"));
        assert!(!is_choose_as_targeting("a color"));
        assert!(!is_choose_as_targeting("a creature type"));
        assert!(!is_choose_as_targeting("a card type"));
        assert!(!is_choose_as_targeting("one —"));
        assert!(!is_choose_as_targeting("a creature card from it"));
    }

    #[test]
    fn choose_a_creature_type() {
        let e = parse_effect("Choose a creature type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CreatureType,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_color() {
        let e = parse_effect("Choose a color");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::Color,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_odd_or_even() {
        let e = parse_effect("Choose odd or even");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::OddOrEven,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_basic_land_type() {
        let e = parse_effect("Choose a basic land type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::BasicLandType,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_card_type() {
        let e = parse_effect("Choose a card type");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardType,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_card_name() {
        let e = parse_effect("Choose a card name");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardName,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_nonland_card_name() {
        let e = parse_effect("Choose a nonland card name");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardName,
                persist: false,
            }
        );
    }

    #[test]
    fn choose_a_creature_card_name() {
        let e = parse_effect("Choose a creature card name");
        assert_eq!(
            e,
            Effect::Choose {
                choice_type: ChoiceType::CardName,
                persist: false,
            }
        );
    }
}

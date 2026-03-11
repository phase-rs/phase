use std::str::FromStr;

use super::oracle_static::parse_continuous_modifications;
use super::oracle_target::parse_target;
use super::oracle_util::{parse_mana_production, parse_number, strip_reminder_text};
use crate::types::ability::{
    AbilityDefinition, AbilityKind, ControllerRef, CountValue, DamageAmount, Duration, Effect,
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
    if let Some(stripped) = strip_subject_clause(text) {
        return parse_effect_clause(&stripped);
    }

    parsed_clause(parse_imperative_effect(text))
}

fn parse_imperative_effect(text: &str) -> Effect {
    let lower = text.to_lowercase();

    // --- Mana production: "add {G}", "add one mana of any color", "add {C}" ---
    if lower.starts_with("add ") {
        if let Some((colors, _)) = parse_mana_production(&text[4..]) {
            return Effect::Mana { produced: colors };
        }
        if lower.contains("mana of any color") || lower.contains("one mana of any color") {
            return Effect::Mana { produced: vec![] };
        }
        // {C} — colorless mana (not in ManaColor, produce empty for "any")
        if lower[4..].trim().starts_with("{c}") {
            return Effect::Mana { produced: vec![] };
        }
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
    if lower.starts_with("exile ") {
        let (target, _) = parse_target(&text[6..]);
        return Effect::ChangeZone {
            origin: None,
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
            return Effect::GainLife { amount };
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
    if lower.starts_with("search your library") || lower.starts_with("search their library") {
        return Effect::ChangeZone {
            origin: Some(Zone::Library),
            destination: Zone::Hand,
            target: TargetFilter::Any,
        };
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
    if lower.starts_with("shuffle ") && lower.contains("library") {
        return Effect::Unimplemented {
            name: "shuffle".to_string(),
            description: Some(text.to_string()),
        };
    }

    // --- Reveal ---
    if lower.starts_with("reveal ") {
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

    // --- Fallback ---
    let verb = lower.split_whitespace().next().unwrap_or("unknown");
    Effect::Unimplemented {
        name: verb.to_string(),
        description: Some(text.to_string()),
    }
}

/// Parse a compound effect chain: split on ". " or ".\n" boundaries and ", then ".
/// Returns an AbilityDefinition with sub_ability chain for compound effects.
pub fn parse_effect_chain(text: &str, kind: AbilityKind) -> AbilityDefinition {
    let sentences = split_effect_sentences(text);
    let mut defs: Vec<AbilityDefinition> = sentences
        .iter()
        .map(|s| {
            let clause = parse_effect_clause(s);
            AbilityDefinition {
                kind,
                effect: clause.effect,
                cost: None,
                sub_ability: None,
                duration: clause.duration,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            }
        })
        .collect();

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
    let Some(pos) = lower.find("deals ").or_else(|| lower.find("deal ")) else {
        return None;
    };
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
    let after_put = &lower[4..].trim();
    let (count, rest) = parse_number(after_put)?;
    // Next word is counter type
    let type_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let counter_type = rest[..type_end].to_string();
    Some(Effect::PutCounter {
        counter_type,
        count: count as i32,
        target: TargetFilter::Any,
    })
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
    if lower.contains("from your graveyard") || lower.contains("from a graveyard") {
        Some(Zone::Graveyard)
    } else if lower.contains("from exile") {
        Some(Zone::Exile)
    } else if lower.contains("from your hand") {
        Some(Zone::Hand)
    } else if lower.contains("from your library") {
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
    match Keyword::from_str(text.trim()) {
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
        || lower.contains(" all creature types")
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{ContinuousModification, TypeFilter};
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
        assert!(matches!(e, Effect::Counter { .. }));
    }

    #[test]
    fn effect_mana_production() {
        let e = parse_effect("Add {W}");
        assert!(matches!(e, Effect::Mana { produced } if produced == vec![ManaColor::White]));
    }

    #[test]
    fn effect_gain_life() {
        let e = parse_effect("You gain 3 life");
        assert!(matches!(e, Effect::GainLife { amount: 3 }));
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
        assert!(matches!(def.effect, Effect::GainLife { amount: 3 }));
        assert!(def.sub_ability.is_some());
        assert!(matches!(
            def.sub_ability.unwrap().effect,
            Effect::Draw { count: 1 }
        ));
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
            Effect::Unimplemented { ref name, .. } if name == "shuffle"
        ));
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
        assert!(matches!(e, Effect::Mana { ref produced } if produced.is_empty()));
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
}

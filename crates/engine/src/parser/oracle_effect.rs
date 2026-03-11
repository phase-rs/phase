use super::oracle_target::parse_target;
use super::oracle_util::{parse_mana_production, parse_number};
use crate::types::ability::{AbilityDefinition, AbilityKind, DamageAmount, Effect, TargetFilter};
use crate::types::zones::Zone;

/// Parse an effect clause from Oracle text into an Effect enum.
/// This handles the verb-based matching for spell effects, activated ability effects,
/// and the effect portion of triggered abilities.
///
/// For compound effects ("Gain 3 life. Draw a card."), call `parse_effect_chain`
/// which splits on sentence boundaries and chains via AbilityDefinition::sub_ability.
pub fn parse_effect(text: &str) -> Effect {
    let text = text.trim().trim_end_matches('.');
    let lower = text.to_lowercase();

    // --- Mana production: "add {G}" ---
    if lower.starts_with("add ") {
        if let Some((colors, _)) = parse_mana_production(&text[4..]) {
            return Effect::Mana { produced: colors };
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
        .map(|s| AbilityDefinition {
            kind,
            effect: parse_effect(s),
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
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

fn try_parse_damage(lower: &str, _text: &str) -> Option<Effect> {
    // Match: "~ deals N damage to {target}" or "deals N damage to each {filter}"
    if let Some(pos) = lower.find("deals ") {
        let after = &lower[pos + 6..];
        if let Some((n, rest)) = parse_number(after) {
            if rest.to_lowercase().starts_with("damage") {
                let after_damage = rest.trim_start_matches(|c: char| c.is_alphabetic()).trim();
                let after_to = after_damage.strip_prefix("to ").unwrap_or(after_damage);
                // "each" → DamageAll
                if after_to.starts_with("each ") {
                    let (target, _) = parse_target(after_to);
                    return Some(Effect::DamageAll {
                        amount: DamageAmount::Fixed(n as i32),
                        target,
                    });
                }
                let (target, _) = parse_target(after_to);
                return Some(Effect::DealDamage {
                    amount: DamageAmount::Fixed(n as i32),
                    target,
                });
            }
        }
    }
    None
}

fn try_parse_pump(lower: &str, text: &str) -> Option<Effect> {
    // Match "+N/+M" or "+N/-M" pattern
    let re_pos = lower.find("gets ").or_else(|| lower.find("get "))?;
    let offset = if lower[re_pos..].starts_with("gets ") {
        5
    } else {
        4
    };
    let after = &text[re_pos + offset..].trim();
    parse_pt_modifier(after).map(|(p, t)| Effect::Pump {
        power: p,
        toughness: t,
        target: TargetFilter::Any,
    })
}

fn parse_pt_modifier(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let slash = text.find('/')?;
    let power_str = &text[..slash];
    let rest = &text[slash + 1..];
    let toughness_end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
    let toughness_str = &rest[..toughness_end];
    let p = power_str.replace('+', "").parse::<i32>().ok()?;
    let t = toughness_str.replace('+', "").parse::<i32>().ok()?;
    Some((p, t))
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

fn try_parse_token(lower: &str, _text: &str) -> Option<Effect> {
    // "create N {P/T} {color} {type} creature token(s) [with {keywords}]"
    let after = &lower[7..]; // skip "create "
    let (count, rest) = parse_number(after).unwrap_or((1, after));
    // Try to find P/T pattern
    if let Some((p, t)) = parse_pt_modifier(rest) {
        let type_name = "Token".to_string(); // simplified
        return Some(Effect::Token {
            name: type_name,
            power: p,
            toughness: t,
            types: vec!["Creature".to_string()],
            colors: vec![],
            keywords: vec![],
            count,
        });
    }
    // Fallback: unstructured token
    Some(Effect::Unimplemented {
        name: "create".to_string(),
        description: Some(lower.to_string()),
    })
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
    use crate::types::ability::TypeFilter;
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
                power: 3,
                toughness: 3,
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
}

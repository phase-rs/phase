use crate::types::ability::{
    ContinuousModification, ControllerRef, FilterProp, StaticDefinition, TargetFilter, TypeFilter,
};
use crate::types::keywords::Keyword;
use crate::types::statics::StaticMode;
use super::oracle_util::strip_reminder_text;

/// Parse a static/continuous ability line into a StaticDefinition.
/// Handles: "Enchanted creature gets +N/+M", "has {keyword}",
/// "Creatures you control get +N/+M", etc.
pub fn parse_static_line(text: &str) -> Option<StaticDefinition> {
    let text = strip_reminder_text(text);
    let lower = text.to_lowercase();

    // --- "Enchanted creature gets +N/+M" or "has {keyword}" ---
    if lower.starts_with("enchanted creature ") {
        return parse_continuous_gets_has(
            &text[19..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EnchantedBy],
            },
        );
    }

    // --- "Equipped creature gets +N/+M" ---
    if lower.starts_with("equipped creature ") {
        return parse_continuous_gets_has(
            &text[18..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EquippedBy],
            },
        );
    }

    // --- "Creatures you control get +N/+M" ---
    if lower.starts_with("creatures you control ") {
        return parse_continuous_gets_has(
            &text[22..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            },
        );
    }

    // --- "Other creatures you control get +N/+M" ---
    if lower.starts_with("other creatures you control ") {
        return parse_continuous_gets_has(
            &text[28..],
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            },
        );
    }

    // --- "~ can't be blocked" ---
    if lower.contains("can't be blocked") {
        return Some(StaticDefinition {
            mode: StaticMode::CantBlock,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    // --- "~ can't attack" ---
    if lower.contains("can't attack") {
        return Some(StaticDefinition {
            mode: StaticMode::CantAttack,
            affected: Some(TargetFilter::SelfRef),
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            characteristic_defining: false,
            description: Some(text.to_string()),
        });
    }

    None
}

/// Parse "gets +N/+M [and has {keyword}]" after the subject.
fn parse_continuous_gets_has(text: &str, affected: TargetFilter) -> Option<StaticDefinition> {
    let lower = text.to_lowercase();
    let mut modifications = Vec::new();

    // Check for "gets +N/+M"
    if lower.starts_with("gets ") || lower.starts_with("get ") {
        let offset = if lower.starts_with("gets ") { 5 } else { 4 };
        let after = &text[offset..].trim();
        if let Some((p, t)) = parse_pt_mod(after) {
            modifications.push(ContinuousModification::AddPower { value: p });
            modifications.push(ContinuousModification::AddToughness { value: t });
        }
    }

    // Check for "and has {keyword}" or "has {keyword}"
    if let Some(pos) = lower.find("has ") {
        let keyword_text = &text[pos + 4..].trim().to_lowercase();
        if let Some(kw) = map_keyword(keyword_text) {
            modifications.push(ContinuousModification::AddKeyword { keyword: kw });
        }
    }

    if modifications.is_empty() {
        return None;
    }

    Some(StaticDefinition {
        mode: StaticMode::Continuous,
        affected: Some(affected),
        modifications,
        condition: None,
        affected_zone: None,
        effect_zone: None,
        characteristic_defining: false,
        description: None,
    })
}

fn parse_pt_mod(text: &str) -> Option<(i32, i32)> {
    let text = text.trim();
    let slash = text.find('/')?;
    let p_str = &text[..slash];
    let rest = &text[slash + 1..];
    let t_end = rest
        .find(|c: char| c.is_whitespace() || c == '.' || c == ',')
        .unwrap_or(rest.len());
    let t_str = &rest[..t_end];
    let p = p_str.replace('+', "").parse::<i32>().ok()?;
    let t = t_str.replace('+', "").parse::<i32>().ok()?;
    Some((p, t))
}

fn map_keyword(text: &str) -> Option<Keyword> {
    let word = text.split(|c: char| c.is_whitespace() || c == '.').next()?.trim();
    match word {
        "flying" => Some(Keyword::Flying),
        "trample" => Some(Keyword::Trample),
        "lifelink" => Some(Keyword::Lifelink),
        "vigilance" => Some(Keyword::Vigilance),
        "haste" => Some(Keyword::Haste),
        "deathtouch" => Some(Keyword::Deathtouch),
        "first strike" => Some(Keyword::FirstStrike),
        "double strike" => Some(Keyword::DoubleStrike),
        "reach" => Some(Keyword::Reach),
        "menace" => Some(Keyword::Menace),
        "hexproof" => Some(Keyword::Hexproof),
        "indestructible" => Some(Keyword::Indestructible),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn static_bonesplitter() {
        let def = parse_static_line("Equipped creature gets +2/+0.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.modifications.contains(&ContinuousModification::AddPower { value: 2 }));
        assert!(def.modifications.contains(&ContinuousModification::AddToughness { value: 0 }));
    }

    #[test]
    fn static_rancor() {
        let def = parse_static_line("Enchanted creature gets +2/+0 and has trample.").unwrap();
        assert!(def.modifications.len() >= 3); // +2, +0, trample
        assert!(def.modifications.contains(&ContinuousModification::AddKeyword { keyword: Keyword::Trample }));
    }

    #[test]
    fn static_cant_be_blocked() {
        let def = parse_static_line("Questing Beast can't be blocked by creatures with power 2 or less.").unwrap();
        assert!(matches!(def.mode, StaticMode::CantBlock));
    }

    #[test]
    fn static_creatures_you_control() {
        let def = parse_static_line("Creatures you control get +1/+1.").unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(matches!(def.affected, Some(TargetFilter::Typed { controller: Some(ControllerRef::You), .. })));
    }
}

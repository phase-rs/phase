use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter};
use crate::types::zones::Zone;

use super::oracle_util::contains_possessive;

/// Parse a target description from Oracle text, returning (filter, remaining_text).
/// Consumes the longest matching target phrase.
pub fn parse_target(text: &str) -> (TargetFilter, &str) {
    let text = text.trim_start();
    let lower = text.to_lowercase();

    // Self-reference: "~" (normalized from card name / "this creature" etc.)
    if let Some(rest) = text.strip_prefix('~') {
        return (TargetFilter::SelfRef, rest.trim_start());
    }

    // "any target"
    if lower.starts_with("any target") {
        return (TargetFilter::Any, &text[10..]);
    }

    // "target player or planeswalker"
    if lower.starts_with("target player or planeswalker") {
        return (
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Player,
                    typed(TypeFilter::Planeswalker, None, vec![]),
                ],
            },
            &text[29..],
        );
    }

    // "target opponent"
    if lower.starts_with("target opponent") {
        return (
            TargetFilter::Typed {
                card_type: None,
                subtype: None,
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            },
            &text[15..],
        );
    }

    // "target player"
    if lower.starts_with("target player") {
        return (TargetFilter::Player, &text[13..]);
    }

    // "each opponent"
    if lower.starts_with("each opponent") {
        return (
            TargetFilter::Typed {
                card_type: None,
                subtype: None,
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            },
            &text[13..],
        );
    }

    // "target" + type phrase
    if lower.starts_with("target ") {
        let (filter, rest) = parse_type_phrase(&text[7..]);
        return (filter, rest);
    }

    // "all" / "each" + type phrase (for *All effects)
    if lower.starts_with("all ") {
        let (filter, rest) = parse_type_phrase(&text[4..]);
        return (filter, rest);
    }
    if lower.starts_with("each ") {
        let (filter, rest) = parse_type_phrase(&text[5..]);
        return (filter, rest);
    }

    // "enchanted creature"
    if lower.starts_with("enchanted creature") {
        return (
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EnchantedBy],
            },
            &text[18..],
        );
    }

    // "equipped creature"
    if lower.starts_with("equipped creature") {
        return (
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EquippedBy],
            },
            &text[17..],
        );
    }

    (TargetFilter::Any, text)
}

/// Parse a type phrase like "creature", "nonland permanent", "artifact or enchantment",
/// "creature you control", "creature an opponent controls".
pub fn parse_type_phrase(text: &str) -> (TargetFilter, &str) {
    let lower = text.to_lowercase();
    let mut pos = 0;
    let mut properties = Vec::new();
    let lower_trimmed = lower.trim_start();
    let offset = lower.len() - lower_trimmed.len();
    pos += offset;

    // Handle color prefix: "white creature", "red spell", etc.
    let color_prop = parse_color_prefix(&lower[pos..]);
    if let Some((ref prop, color_len)) = color_prop {
        properties.push(prop.clone());
        pos += color_len;
    }

    // Handle "non" prefix
    let (negated_type, non_prefix) = parse_non_prefix(&lower[pos..]);
    if non_prefix > 0 {
        pos += non_prefix;
    }

    // Parse the core type
    let (card_type, subtype, type_len) = parse_core_type(&lower[pos..]);
    pos += type_len;

    if let Some(neg) = negated_type {
        properties.push(FilterProp::NonType { value: neg });
    }

    // Check for "or" combinator: "artifact or enchantment", "creature or artifact you control"
    let rest_lower = lower[pos..].trim_start();
    let rest_offset = lower[pos..].len() - rest_lower.len();
    if rest_lower.starts_with("or ") {
        let or_text = &text[pos + rest_offset + 3..];
        let (other_filter, final_rest) = parse_type_phrase(or_text);
        let mut left = typed(card_type.unwrap_or(TypeFilter::Any), subtype, properties);

        // Distribute shared controller suffix from right branch to left:
        // "creature or artifact you control" → both get "you control"
        if let TargetFilter::Typed {
            controller: Some(ref ctrl),
            ..
        } = other_filter
        {
            if let TargetFilter::Typed {
                controller: ref mut left_ctrl,
                ..
            } = left
            {
                if left_ctrl.is_none() {
                    *left_ctrl = Some(ctrl.clone());
                }
            }
        }

        return (
            TargetFilter::Or {
                filters: vec![left, other_filter],
            },
            final_rest,
        );
    }

    // Check controller suffix
    let mut controller = parse_controller_suffix(&lower[pos..]);
    let ctrl_len = controller.as_ref().map_or(0, |c| match c {
        ControllerRef::You => " you control".len(),
        ControllerRef::Opponent => " an opponent controls".len(),
    });
    pos += ctrl_len;

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_mana_value_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check "with power N or less/greater" suffix
    if let Some((prop, consumed)) = parse_power_suffix(&lower[pos..]) {
        properties.push(prop);
        pos += consumed;
    }

    // Check zone suffix: "card from a graveyard", "card in your graveyard", "from exile", etc.
    if let Some((zone_prop, zone_ctrl, consumed)) = parse_zone_suffix(&lower[pos..]) {
        properties.push(zone_prop);
        pos += consumed;
        // Apply zone-derived controller if we don't already have one
        if controller.is_none() {
            controller = zone_ctrl;
        }
    }

    let filter = TargetFilter::Typed {
        card_type,
        subtype,
        controller,
        properties,
    };

    (filter, &text[pos..])
}

fn parse_non_prefix(text: &str) -> (Option<String>, usize) {
    if let Some(rest) = text.strip_prefix("non") {
        let end = rest.find(|c: char| c.is_whitespace()).unwrap_or(rest.len());
        let negated = rest[..end].to_string();
        // We consumed "non{type} " but the core type is the NEXT word, so return just the negated type
        (
            Some(negated),
            3 + end + if rest.len() > end { 1 } else { 0 },
        )
    } else {
        (None, 0)
    }
}

fn parse_core_type(text: &str) -> (Option<TypeFilter>, Option<String>, usize) {
    let types: &[(&str, TypeFilter)] = &[
        ("creatures", TypeFilter::Creature),
        ("creature", TypeFilter::Creature),
        ("permanents", TypeFilter::Permanent),
        ("permanent", TypeFilter::Permanent),
        ("artifacts", TypeFilter::Artifact),
        ("artifact", TypeFilter::Artifact),
        ("enchantments", TypeFilter::Enchantment),
        ("enchantment", TypeFilter::Enchantment),
        ("instants", TypeFilter::Instant),
        ("instant", TypeFilter::Instant),
        ("sorceries", TypeFilter::Sorcery),
        ("sorcery", TypeFilter::Sorcery),
        ("planeswalkers", TypeFilter::Planeswalker),
        ("planeswalker", TypeFilter::Planeswalker),
        ("lands", TypeFilter::Land),
        ("land", TypeFilter::Land),
        ("spell", TypeFilter::Card),
        ("card", TypeFilter::Card),
    ];

    for (word, tf) in types {
        if text.starts_with(word) {
            return (Some(tf.clone()), None, word.len());
        }
    }

    (None, None, 0)
}

fn parse_controller_suffix(text: &str) -> Option<ControllerRef> {
    let trimmed = text.trim_start();
    if trimmed.starts_with("you control") {
        Some(ControllerRef::You)
    } else if trimmed.starts_with("an opponent controls") {
        Some(ControllerRef::Opponent)
    } else {
        None
    }
}

/// Parse a color adjective prefix: "white ", "blue ", "black ", "red ", "green ".
/// Returns (FilterProp::HasColor, bytes consumed including trailing space).
fn parse_color_prefix(text: &str) -> Option<(FilterProp, usize)> {
    let colors = [
        ("white ", "White"),
        ("blue ", "Blue"),
        ("black ", "Black"),
        ("red ", "Red"),
        ("green ", "Green"),
    ];
    for (prefix, color_name) in &colors {
        if text.starts_with(prefix) {
            return Some((
                FilterProp::HasColor {
                    color: color_name.to_string(),
                },
                prefix.len(),
            ));
        }
    }
    None
}

/// Parse "with power N or less" / "with power N or greater" suffix.
/// Returns (FilterProp, bytes consumed from the original text).
fn parse_power_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("with power ")?;
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return None;
    }
    let value: i32 = rest[..num_end].parse().ok()?;
    let after_num = rest[num_end..].trim_start();

    let (prop, after) = if let Some(a) = after_num.strip_prefix("or less") {
        (FilterProp::PowerLE { value }, a)
    } else if let Some(a) = after_num.strip_prefix("or greater") {
        (FilterProp::PowerGE { value }, a)
    } else {
        return None;
    };
    Some((prop, text.len() - after.len()))
}

/// Parse "with mana value N or less" / "with mana value N or greater" suffix.
/// Returns (FilterProp, bytes consumed from the original text).
fn parse_mana_value_suffix(text: &str) -> Option<(FilterProp, usize)> {
    let trimmed = text.trim_start();
    let rest = trimmed.strip_prefix("with mana value ")?;
    let num_end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    if num_end == 0 {
        return None;
    }
    let value: u32 = rest[..num_end].parse().ok()?;
    let after_num = rest[num_end..].trim_start();

    let (prop, after) = if let Some(a) = after_num.strip_prefix("or greater") {
        (FilterProp::CmcGE { value }, a)
    } else if let Some(a) = after_num.strip_prefix("or less") {
        (FilterProp::CmcLE { value }, a)
    } else {
        return None;
    };
    Some((prop, text.len() - after.len()))
}

fn typed(
    card_type: TypeFilter,
    subtype: Option<String>,
    properties: Vec<FilterProp>,
) -> TargetFilter {
    TargetFilter::Typed {
        card_type: Some(card_type),
        subtype,
        controller: None,
        properties,
    }
}

/// Parse a zone suffix like "card from a graveyard", "from your graveyard", "from exile".
/// Returns (FilterProp::InZone, optional ControllerRef, bytes consumed).
///
/// Handles:
/// - Possessive: "from your graveyard", "from their graveyard", "from its owner's graveyard"
/// - Indefinite: "from a graveyard", "in a graveyard"
/// - Direct: "from exile", "in exile"
///
/// Skips optional leading "card"/"cards" before zone detection.
fn parse_zone_suffix(text: &str) -> Option<(FilterProp, Option<ControllerRef>, usize)> {
    let trimmed = text.trim_start();
    let leading_ws = text.len() - trimmed.len();

    // Skip optional "card"/"cards" before zone preposition
    let (after_card, card_skip) = if let Some(rest) = trimmed.strip_prefix("cards ") {
        (rest, "cards ".len())
    } else if let Some(rest) = trimmed.strip_prefix("card ") {
        (rest, "card ".len())
    } else {
        (trimmed, 0)
    };

    let zones: &[(&str, Zone)] = &[
        ("graveyard", Zone::Graveyard),
        ("exile", Zone::Exile),
        ("hand", Zone::Hand),
        ("library", Zone::Library),
    ];

    for prep in &["from", "in"] {
        for &(zone_word, ref zone) in zones {
            // Possessive: "from your graveyard", "from their graveyard"
            if contains_possessive(after_card, prep, zone_word) {
                let pattern = format!("{prep} your {zone_word}");
                let ctrl = if after_card.to_lowercase().contains(&pattern) {
                    Some(ControllerRef::You)
                } else {
                    None
                };
                // Find end of the zone word in after_card
                let zone_end = after_card
                    .to_lowercase()
                    .find(zone_word)
                    .map(|i| i + zone_word.len())
                    .unwrap_or(after_card.len());
                return Some((
                    FilterProp::InZone { zone: *zone },
                    ctrl,
                    leading_ws + card_skip + zone_end,
                ));
            }

            // Indefinite: "from a graveyard", "in a graveyard"
            let indef = format!("{prep} a {zone_word}");
            if after_card.to_lowercase().starts_with(&indef) {
                return Some((
                    FilterProp::InZone { zone: *zone },
                    None,
                    leading_ws + card_skip + indef.len(),
                ));
            }

            // Direct (no article): "from exile", "in exile"
            let direct = format!("{prep} {zone_word}");
            if after_card.to_lowercase().starts_with(&direct) {
                // Make sure it's not a possessive that we missed
                let after = &after_card[direct.len()..];
                if after.is_empty()
                    || after.starts_with(|c: char| c.is_whitespace() || c == ',' || c == '.')
                {
                    return Some((
                        FilterProp::InZone { zone: *zone },
                        None,
                        leading_ws + card_skip + direct.len(),
                    ));
                }
            }
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn any_target() {
        let (f, rest) = parse_target("any target");
        assert_eq!(f, TargetFilter::Any);
        assert_eq!(rest, "");
    }

    #[test]
    fn target_creature() {
        let (f, _) = parse_target("target creature");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![],
            }
        );
    }

    #[test]
    fn target_creature_you_control() {
        let (f, _) = parse_target("target creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            }
        );
    }

    #[test]
    fn target_nonland_permanent() {
        let (f, _) = parse_target("target nonland permanent");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Permanent),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::NonType {
                    value: "land".to_string()
                }],
            }
        );
    }

    #[test]
    fn target_artifact_or_enchantment() {
        let (f, _) = parse_target("target artifact or enchantment");
        match f {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
            }
            _ => panic!("Expected Or filter, got {:?}", f),
        }
    }

    #[test]
    fn target_player() {
        let (f, _) = parse_target("target player");
        assert_eq!(f, TargetFilter::Player);
    }

    #[test]
    fn enchanted_creature() {
        let (f, _) = parse_target("enchanted creature");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EnchantedBy],
            }
        );
    }

    #[test]
    fn equipped_creature() {
        let (f, _) = parse_target("equipped creature");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::EquippedBy],
            }
        );
    }

    #[test]
    fn each_opponent() {
        let (f, _) = parse_target("each opponent");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: None,
                subtype: None,
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }
        );
    }

    #[test]
    fn target_opponent() {
        let (f, _) = parse_target("target opponent");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: None,
                subtype: None,
                controller: Some(ControllerRef::Opponent),
                properties: vec![],
            }
        );
    }

    #[test]
    fn or_type_distributes_controller() {
        // "creature or artifact you control" → both branches get You controller
        let (f, _) = parse_target("target creature or artifact you control");
        match f {
            TargetFilter::Or { filters } => {
                assert_eq!(filters.len(), 2);
                assert_eq!(
                    filters[0],
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Creature),
                        subtype: None,
                        controller: Some(ControllerRef::You),
                        properties: vec![],
                    }
                );
                assert_eq!(
                    filters[1],
                    TargetFilter::Typed {
                        card_type: Some(TypeFilter::Artifact),
                        subtype: None,
                        controller: Some(ControllerRef::You),
                        properties: vec![],
                    }
                );
            }
            _ => panic!("Expected Or filter, got {:?}", f),
        }
    }

    #[test]
    fn tilde_is_self_ref() {
        let (f, rest) = parse_target("~");
        assert_eq!(f, TargetFilter::SelfRef);
        assert_eq!(rest, "");
    }

    #[test]
    fn tilde_with_trailing_text() {
        let (f, rest) = parse_target("~ to its owner's hand");
        assert_eq!(f, TargetFilter::SelfRef);
        assert!(rest.contains("to its owner"));
    }

    #[test]
    fn white_creature_you_control() {
        let (f, _) = parse_type_phrase("white creature you control");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::HasColor {
                    color: "White".to_string()
                }],
            }
        );
    }

    #[test]
    fn red_spell() {
        let (f, _) = parse_type_phrase("red spell");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::HasColor {
                    color: "Red".to_string()
                }],
            }
        );
    }

    #[test]
    fn spell_with_mana_value_4_or_greater() {
        let (f, _) = parse_type_phrase("spell with mana value 4 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::CmcGE { value: 4 }],
            }
        );
    }

    #[test]
    fn creature_you_control_with_power_2_or_less() {
        let (f, rest) = parse_type_phrase("creature you control with power 2 or less enter");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::PowerLE { value: 2 }],
            }
        );
        // Remaining text should be the event verb
        assert!(rest.trim_start().starts_with("enter"), "rest = {:?}", rest);
    }

    #[test]
    fn creature_with_power_3_or_greater() {
        let (f, _) = parse_type_phrase("creature with power 3 or greater");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::PowerGE { value: 3 }],
            }
        );
    }

    #[test]
    fn target_card_from_a_graveyard() {
        let (f, rest) = parse_target("target card from a graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard
                }],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_creature_card_in_your_graveyard() {
        let (f, rest) = parse_target("target creature card in your graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard
                }],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_card_from_exile() {
        let (f, rest) = parse_target("target card from exile");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::InZone { zone: Zone::Exile }],
            }
        );
        assert_eq!(rest.trim(), "");
    }

    #[test]
    fn target_card_in_a_graveyard() {
        let (f, _) = parse_target("target card in a graveyard");
        assert_eq!(
            f,
            TargetFilter::Typed {
                card_type: Some(TypeFilter::Card),
                subtype: None,
                controller: None,
                properties: vec![FilterProp::InZone {
                    zone: Zone::Graveyard
                }],
            }
        );
    }
}

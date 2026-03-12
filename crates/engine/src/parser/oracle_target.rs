use crate::types::ability::{ControllerRef, FilterProp, TargetFilter, TypeFilter};

/// Parse a target description from Oracle text, returning (filter, remaining_text).
/// Consumes the longest matching target phrase.
pub fn parse_target(text: &str) -> (TargetFilter, &str) {
    let text = text.trim_start();
    let lower = text.to_lowercase();

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

    // Check for "or" combinator: "artifact or enchantment"
    let rest_lower = lower[pos..].trim_start();
    let rest_offset = lower[pos..].len() - rest_lower.len();
    if rest_lower.starts_with("or ") {
        let or_text = &text[pos + rest_offset + 3..];
        let (other_filter, final_rest) = parse_type_phrase(or_text);
        let left = typed(card_type.unwrap_or(TypeFilter::Any), subtype, properties);
        return (
            TargetFilter::Or {
                filters: vec![left, other_filter],
            },
            final_rest,
        );
    }

    // Check controller suffix
    let controller = parse_controller_suffix(&lower[pos..]);
    let ctrl_len = controller.as_ref().map_or(0, |c| match c {
        ControllerRef::You => " you control".len(),
        ControllerRef::Opponent => " an opponent controls".len(),
    });
    pos += ctrl_len;

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
        ("spell", TypeFilter::Any),
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
}

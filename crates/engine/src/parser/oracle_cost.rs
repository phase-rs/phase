use super::oracle_target::{parse_target, parse_type_phrase};
use super::oracle_util::parse_mana_symbols;
use super::oracle_util::parse_number;
use crate::types::ability::{AbilityCost, FilterProp, TargetFilter, TypedFilter};
use crate::types::zones::Zone;

/// Parse the cost portion before `:` in an Oracle activated ability.
/// Input: the raw text before the colon, e.g., "{T}", "{2}{W}, Sacrifice a creature", "Pay 3 life".
/// Returns an AbilityCost (possibly Composite for multi-part costs).
pub fn parse_oracle_cost(text: &str) -> AbilityCost {
    let text = text.trim();

    // Split on ", " for composite costs
    let parts: Vec<&str> = split_cost_parts(text);
    if parts.len() > 1 {
        let costs: Vec<AbilityCost> = parts.iter().map(|p| parse_single_cost(p.trim())).collect();
        return AbilityCost::Composite { costs };
    }

    parse_single_cost(text)
}

fn split_cost_parts(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let mut start = 0;
    let mut brace_depth = 0u32;
    let bytes = text.as_bytes();
    let mut i = 0;

    while i < text.len() {
        let ch = text[i..].chars().next().expect("valid UTF-8");
        match ch {
            '{' => brace_depth += 1,
            '}' => brace_depth = brace_depth.saturating_sub(1),
            ',' if brace_depth == 0 => {
                let part = text[start..i].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = i + 1;
            }
            ' ' if brace_depth == 0 && bytes[i..].starts_with(b" and ") => {
                let part = text[start..i].trim();
                if !part.is_empty() {
                    parts.push(part);
                }
                start = i + " and ".len();
                i += " and ".len() - 1;
            }
            _ => {}
        }
        i += ch.len_utf8();
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

pub fn parse_single_cost(text: &str) -> AbilityCost {
    let text = text.trim();
    let lower = text.to_lowercase();

    // {T} — tap
    if lower == "{t}" {
        return AbilityCost::Tap;
    }

    // {Q} — untap
    if lower == "{q}" {
        return AbilityCost::Untap;
    }

    // Loyalty: [+N], [-N], [0]
    if text.starts_with('[') {
        if let Some(end) = text.find(']') {
            let inner = &text[1..end];
            // Handle minus sign variants: −, –, -
            let normalized = inner.replace(['−', '–'], "-");
            if let Ok(n) = normalized.parse::<i32>() {
                return AbilityCost::Loyalty { amount: n };
            }
            // +N
            if let Some(stripped) = normalized.strip_prefix('+') {
                if let Ok(n) = stripped.parse::<i32>() {
                    return AbilityCost::Loyalty { amount: n };
                }
            }
        }
    }

    // "Sacrifice ~" / "Sacrifice a/an {filter}"
    if lower.starts_with("sacrifice ") {
        let rest = &text[10..].trim();
        if rest.to_lowercase().starts_with('~')
            || rest.to_lowercase().starts_with("cardname")
            || rest.to_lowercase().starts_with("this ")
        {
            return AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef,
            };
        }
        // "Sacrifice a {filter}"
        let rest_lower = rest.to_lowercase();
        let rest_no_article = if rest_lower.starts_with("a ") {
            &rest[2..]
        } else if rest_lower.starts_with("an ") {
            &rest[3..]
        } else {
            rest
        };
        let (filter, _) = parse_target(&format!("target {}", rest_no_article));
        return AbilityCost::Sacrifice { target: filter };
    }

    // "Pay N life" / "N life"
    if (lower.starts_with("pay ") || lower.ends_with(" life")) && lower.contains("life") {
        let rest = lower.strip_prefix("pay ").unwrap_or(&lower);
        if let Some(n) = rest
            .split_whitespace()
            .next()
            .and_then(|w| w.parse::<u32>().ok())
        {
            return AbilityCost::PayLife { amount: n };
        }
    }

    // "Discard a card" / "Discard N cards"
    if let Some(rest) = lower.strip_prefix("discard ") {
        if rest.starts_with("a card") {
            return AbilityCost::Discard {
                count: 1,
                filter: None,
                random: false,
            };
        }
        if let Ok(n) = rest.split_whitespace().next().unwrap_or("").parse::<u32>() {
            return AbilityCost::Discard {
                count: n,
                filter: None,
                random: false,
            };
        }
        return AbilityCost::Discard {
            count: 1,
            filter: None,
            random: false,
        };
    }

    if let Some(rest) = lower.strip_prefix("exile ") {
        let count = parse_number(rest).map(|(n, _)| n).unwrap_or(1);
        let filter_start = parse_number(&text[6..])
            .map(|(_, remaining)| remaining)
            .unwrap_or(&text[6..]);
        let filter_text = strip_count_article_prefix(filter_start);
        let (filter, remainder) = parse_type_phrase(filter_text);
        if remainder.trim().is_empty() {
            let zone = extract_filter_zone(&filter);
            return AbilityCost::Exile {
                count,
                zone,
                filter: Some(filter),
            };
        }
    }

    // "Blight N"
    if let Some(rest) = lower.strip_prefix("blight ") {
        let count = rest
            .split_whitespace()
            .next()
            .and_then(|w| w.parse::<u32>().ok())
            .unwrap_or(1);
        return AbilityCost::Blight { count };
    }

    // "Remove N {type} counter(s) from ~"
    if lower.starts_with("remove ") && lower.contains("counter") {
        let words: Vec<&str> = text.split_whitespace().collect();
        if words.len() >= 4 {
            let count = words[1].parse::<u32>().unwrap_or(1);
            let counter_type = words[2].to_string();
            return AbilityCost::RemoveCounter {
                count,
                counter_type,
                target: None,
            };
        }
    }

    // "Tap an untapped creature you control" / "Tap two untapped creatures you control"
    if let Some(rest) = lower.strip_prefix("tap ") {
        let (count, filter_text) = if let Some(rest) = rest.strip_prefix("an untapped ") {
            (1, rest)
        } else if let Some(rest) = rest.strip_prefix("an ") {
            (1, rest)
        } else if let Some((n, rest)) = super::oracle_util::parse_number(rest) {
            let rest = rest
                .trim_start()
                .strip_prefix("untapped ")
                .unwrap_or(rest.trim_start());
            (n, rest)
        } else {
            (0, "")
        };

        if count > 0 {
            let target_text = format!("target {filter_text}");
            let (filter, remainder) = parse_target(&target_text);
            if remainder.trim().is_empty() {
                return AbilityCost::TapCreatures { count, filter };
            }
        }
    }

    // "Pay {N}{W}" — mana cost with "pay" prefix
    if let Some(mana_text) = lower.strip_prefix("pay ") {
        let mana_text = mana_text.trim();
        if mana_text.starts_with('{') {
            if let Some((cost, rest)) = parse_mana_symbols(mana_text) {
                if rest.trim().is_empty() {
                    return AbilityCost::Mana { cost };
                }
            }
        }
    }

    // Mana cost: {N}{W}{U} etc.
    if text.starts_with('{') {
        if let Some((cost, rest)) = parse_mana_symbols(text) {
            if rest.trim().is_empty() {
                return AbilityCost::Mana { cost };
            }
        }
    }

    AbilityCost::Unimplemented {
        description: text.to_string(),
    }
}

fn strip_count_article_prefix(text: &str) -> &str {
    let trimmed = text.trim();
    if let Some(rest) = trimmed.strip_prefix("a ") {
        return rest;
    }
    if let Some(rest) = trimmed.strip_prefix("an ") {
        return rest;
    }

    trimmed
}

fn extract_filter_zone(filter: &TargetFilter) -> Option<Zone> {
    match filter {
        TargetFilter::Typed(TypedFilter { properties, .. }) => properties.iter().find_map(|prop| {
            if let FilterProp::InZone { zone } = prop {
                Some(*zone)
            } else {
                None
            }
        }),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TypeFilter, TypedFilter};
    use crate::types::mana::{ManaCost, ManaCostShard};

    #[test]
    fn cost_tap() {
        assert_eq!(parse_oracle_cost("{T}"), AbilityCost::Tap);
    }

    #[test]
    fn cost_untap() {
        assert_eq!(parse_oracle_cost("{Q}"), AbilityCost::Untap);
    }

    #[test]
    fn cost_mana() {
        assert_eq!(
            parse_oracle_cost("{2}{W}"),
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 2,
                    shards: vec![ManaCostShard::White]
                }
            }
        );
    }

    #[test]
    fn cost_tap_and_mana_composite() {
        match parse_oracle_cost("{T}, {1}") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert_eq!(costs[0], AbilityCost::Tap);
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }

    #[test]
    fn cost_zero_mana() {
        assert_eq!(
            parse_oracle_cost("{0}"),
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    generic: 0,
                    shards: vec![],
                }
            }
        );
    }

    #[test]
    fn cost_sacrifice_self() {
        assert_eq!(
            parse_oracle_cost("Sacrifice ~"),
            AbilityCost::Sacrifice {
                target: TargetFilter::SelfRef
            }
        );
    }

    #[test]
    fn cost_sacrifice_creature() {
        match parse_oracle_cost("Sacrifice a creature") {
            AbilityCost::Sacrifice { target } => {
                assert!(matches!(
                    target,
                    TargetFilter::Typed(TypedFilter {
                        card_type: Some(TypeFilter::Creature),
                        ..
                    })
                ));
            }
            other => panic!("Expected Sacrifice, got {:?}", other),
        }
    }

    #[test]
    fn cost_tap_untapped_creature_you_control() {
        assert_eq!(
            parse_oracle_cost("Tap an untapped creature you control"),
            AbilityCost::TapCreatures {
                count: 1,
                filter: TargetFilter::Typed(
                    TypedFilter::creature().controller(crate::types::ability::ControllerRef::You)
                ),
            }
        );
    }

    #[test]
    fn cost_pay_life() {
        assert_eq!(
            parse_oracle_cost("Pay 3 life"),
            AbilityCost::PayLife { amount: 3 }
        );
    }

    #[test]
    fn cost_loyalty_positive() {
        assert_eq!(
            parse_oracle_cost("[+2]"),
            AbilityCost::Loyalty { amount: 2 }
        );
    }

    #[test]
    fn cost_loyalty_negative() {
        assert_eq!(
            parse_oracle_cost("[−3]"),
            AbilityCost::Loyalty { amount: -3 }
        );
    }

    #[test]
    fn cost_loyalty_zero() {
        assert_eq!(parse_oracle_cost("[0]"), AbilityCost::Loyalty { amount: 0 });
    }

    #[test]
    fn cost_discard() {
        assert_eq!(
            parse_oracle_cost("Discard a card"),
            AbilityCost::Discard {
                count: 1,
                filter: None,
                random: false
            }
        );
    }

    #[test]
    fn cost_composite_tap_mana_sacrifice() {
        match parse_oracle_cost("{T}, {2}{B}, Sacrifice a creature") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 3);
                assert_eq!(costs[0], AbilityCost::Tap);
                assert!(matches!(costs[2], AbilityCost::Sacrifice { .. }));
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }

    #[test]
    fn cost_composite_pay_life_and_exile_card() {
        match parse_oracle_cost("Pay 1 life and exile a blue card from your hand") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2);
                assert_eq!(costs[0], AbilityCost::PayLife { amount: 1 });
                assert!(matches!(costs[1], AbilityCost::Exile { .. }));
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }

    #[test]
    fn cost_exile_colored_card_from_hand() {
        match parse_oracle_cost("Exile a blue card from your hand") {
            AbilityCost::Exile {
                count,
                zone,
                filter,
            } => {
                assert_eq!(count, 1);
                assert_eq!(zone, Some(crate::types::zones::Zone::Hand));
                assert!(matches!(
                    filter,
                    Some(TargetFilter::Typed(TypedFilter {
                        controller: Some(crate::types::ability::ControllerRef::You),
                        ..
                    }))
                ));
            }
            other => panic!("Expected Exile, got {:?}", other),
        }
    }

    #[test]
    fn cost_blight() {
        assert_eq!(
            parse_oracle_cost("Blight 2"),
            AbilityCost::Blight { count: 2 }
        );
    }

    #[test]
    fn cost_blight_one() {
        assert_eq!(
            parse_oracle_cost("Blight 1"),
            AbilityCost::Blight { count: 1 }
        );
    }

    #[test]
    fn cost_composite_tap_blight() {
        match parse_oracle_cost("{1}{R}, {T}, Blight 1") {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 3);
                assert!(matches!(costs[0], AbilityCost::Mana { .. }));
                assert_eq!(costs[1], AbilityCost::Tap);
                assert_eq!(costs[2], AbilityCost::Blight { count: 1 });
            }
            other => panic!("Expected Composite, got {:?}", other),
        }
    }
}

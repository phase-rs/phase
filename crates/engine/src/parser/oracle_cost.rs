use super::oracle_target::parse_target;
use super::oracle_util::parse_mana_symbols;
use crate::types::ability::{AbilityCost, TargetFilter};

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

    for (i, ch) in text.char_indices() {
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
            _ => {}
        }
    }
    let last = text[start..].trim();
    if !last.is_empty() {
        parts.push(last);
    }
    parts
}

fn parse_single_cost(text: &str) -> AbilityCost {
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
            || rest.to_lowercase() == "this creature"
            || rest.to_lowercase() == "this artifact"
            || rest.to_lowercase() == "this enchantment"
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

    // "Pay N life"
    if lower.starts_with("pay ") && lower.contains("life") {
        let rest = &lower[4..];
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

    // "Exile {filter} from your graveyard"
    if lower.starts_with("exile ") && lower.contains("from your graveyard") {
        let rest = &text[6..];
        let gy_pos = rest
            .to_lowercase()
            .find("from your graveyard")
            .unwrap_or(rest.len());
        let filter_text = rest[..gy_pos].trim();
        // Simple: count if starts with number
        let count = filter_text
            .split_whitespace()
            .next()
            .and_then(|w| w.parse::<u32>().ok())
            .unwrap_or(1);
        return AbilityCost::Exile {
            count,
            zone: Some(crate::types::zones::Zone::Graveyard),
            filter: None,
        };
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
}

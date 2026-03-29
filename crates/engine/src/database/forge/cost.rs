use crate::types::ability::{AbilityCost, TargetFilter};
use crate::types::mana::{ManaCost, ManaCostShard};

use super::filter::translate_filter;
use super::types::ForgeTranslateError;

/// Translate a Forge cost string into an `AbilityCost`.
///
/// Forge cost format: space-separated tokens like `"2 G T"`, `"Sac<1/CARDNAME>"`,
/// `"PayLife<3>"`. Mana tokens are accumulated and combined.
pub(crate) fn translate_cost(cost_str: &str) -> Result<AbilityCost, ForgeTranslateError> {
    let cost_str = cost_str.trim();
    if cost_str.is_empty() {
        return Ok(AbilityCost::Mana {
            cost: ManaCost::zero(),
        });
    }

    let mut costs: Vec<AbilityCost> = Vec::new();
    let mut mana_shards: Vec<ManaCostShard> = Vec::new();
    let mut mana_generic: u32 = 0;

    for token in cost_str.split_whitespace() {
        match token {
            "T" => costs.push(AbilityCost::Tap),
            "Q" => costs.push(AbilityCost::Untap),

            // Sacrifice: Sac<N/Filter>
            t if t.starts_with("Sac<") => {
                let inner = t
                    .strip_prefix("Sac<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let (count, filter) = parse_count_filter(inner)?;
                costs.push(AbilityCost::Sacrifice {
                    target: filter,
                    count,
                });
            }

            // Pay life: PayLife<N>
            t if t.starts_with("PayLife<") => {
                let inner = t
                    .strip_prefix("PayLife<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("0");
                let amount: u32 = inner
                    .parse()
                    .map_err(|_| ForgeTranslateError::UnparsableCost(token.to_string()))?;
                costs.push(AbilityCost::PayLife { amount });
            }

            // Discard: Discard<N/Filter>
            t if t.starts_with("Discard<") => {
                let inner = t
                    .strip_prefix("Discard<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let (count, filter) = parse_count_filter(inner)?;
                let filter = if filter == TargetFilter::Any {
                    None
                } else {
                    Some(filter)
                };
                costs.push(AbilityCost::Discard {
                    count,
                    filter,
                    random: false,
                    self_ref: false,
                });
            }

            // Exile from zone: Exile<N/Zone/Filter>
            t if t.starts_with("Exile<") => {
                let inner = t
                    .strip_prefix("Exile<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let parts: Vec<&str> = inner.splitn(3, '/').collect();
                let count: u32 = parts.first().and_then(|s| s.parse().ok()).unwrap_or(1);
                costs.push(AbilityCost::Exile {
                    count,
                    zone: None,
                    filter: None,
                });
            }

            // Add counter (planeswalker loyalty+): AddCounter<N/TYPE>
            t if t.starts_with("AddCounter<") => {
                let inner = t
                    .strip_prefix("AddCounter<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let amount: i32 = inner
                    .split('/')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1);
                costs.push(AbilityCost::Loyalty { amount });
            }

            // Subtract counter (planeswalker loyalty-): SubCounter<N/TYPE>
            t if t.starts_with("SubCounter<") => {
                let inner = t
                    .strip_prefix("SubCounter<")
                    .and_then(|s| s.strip_suffix('>'))
                    .unwrap_or("");
                let amount: i32 = inner
                    .split('/')
                    .next()
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(1);
                costs.push(AbilityCost::Loyalty { amount: -amount });
            }

            // Mana tokens: single letters or numbers
            "W" => mana_shards.push(ManaCostShard::White),
            "U" => mana_shards.push(ManaCostShard::Blue),
            "B" => mana_shards.push(ManaCostShard::Black),
            "R" => mana_shards.push(ManaCostShard::Red),
            "G" => mana_shards.push(ManaCostShard::Green),
            "C" => mana_shards.push(ManaCostShard::Colorless),
            "X" => mana_shards.push(ManaCostShard::X),

            // Generic mana (numbers)
            t if t.parse::<u32>().is_ok() => {
                mana_generic += t.parse::<u32>().unwrap();
            }

            _ => {
                // Unknown cost token — skip for graceful degradation
            }
        }
    }

    // Add accumulated mana cost
    if !mana_shards.is_empty() || mana_generic > 0 {
        costs.push(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: mana_shards,
                generic: mana_generic,
            },
        });
    }

    match costs.len() {
        0 => Ok(AbilityCost::Mana {
            cost: ManaCost::zero(),
        }),
        1 => Ok(costs.into_iter().next().unwrap()),
        _ => Ok(AbilityCost::Composite { costs }),
    }
}

/// Parse Forge mana cost string (space-separated: "2 W W", "R") into `ManaCost`.
#[allow(dead_code)]
pub(crate) fn forge_mana_to_cost(mana_str: &str) -> ManaCost {
    let mana_str = mana_str.trim();
    if mana_str.is_empty() || mana_str == "no cost" {
        return ManaCost::NoCost;
    }

    let mut shards = Vec::new();
    let mut generic = 0u32;

    for token in mana_str.split_whitespace() {
        match token {
            "W" => shards.push(ManaCostShard::White),
            "U" => shards.push(ManaCostShard::Blue),
            "B" => shards.push(ManaCostShard::Black),
            "R" => shards.push(ManaCostShard::Red),
            "G" => shards.push(ManaCostShard::Green),
            "C" => shards.push(ManaCostShard::Colorless),
            "X" => shards.push(ManaCostShard::X),
            t if t.parse::<u32>().is_ok() => {
                generic += t.parse::<u32>().unwrap();
            }
            _ => {}
        }
    }

    ManaCost::Cost { shards, generic }
}

/// Parse "N/Filter" into (count, TargetFilter).
fn parse_count_filter(inner: &str) -> Result<(u32, TargetFilter), ForgeTranslateError> {
    if let Some((count_str, filter_str)) = inner.split_once('/') {
        let count: u32 = count_str.parse().unwrap_or(1);
        let filter = translate_filter(filter_str)?;
        Ok((count, filter))
    } else if let Ok(count) = inner.parse::<u32>() {
        Ok((count, TargetFilter::Any))
    } else {
        let filter = translate_filter(inner)?;
        Ok((1, filter))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tap_cost() {
        assert!(matches!(translate_cost("T").unwrap(), AbilityCost::Tap));
    }

    #[test]
    fn test_mana_cost() {
        match translate_cost("2 G").unwrap() {
            AbilityCost::Mana {
                cost: ManaCost::Cost { shards, generic },
            } => {
                assert_eq!(generic, 2);
                assert_eq!(shards, vec![ManaCostShard::Green]);
            }
            other => panic!("expected Mana, got {other:?}"),
        }
    }

    #[test]
    fn test_composite_cost() {
        match translate_cost("2 G T").unwrap() {
            AbilityCost::Composite { costs } => {
                assert_eq!(costs.len(), 2); // Tap + Mana
                assert!(costs.iter().any(|c| matches!(c, AbilityCost::Tap)));
            }
            other => panic!("expected Composite, got {other:?}"),
        }
    }

    #[test]
    fn test_sacrifice_cost() {
        match translate_cost("Sac<1/CARDNAME>").unwrap() {
            AbilityCost::Sacrifice { target, count } => {
                assert_eq!(count, 1);
                assert_eq!(target, TargetFilter::SelfRef);
            }
            other => panic!("expected Sacrifice, got {other:?}"),
        }
    }

    #[test]
    fn test_pay_life_cost() {
        match translate_cost("PayLife<3>").unwrap() {
            AbilityCost::PayLife { amount } => assert_eq!(amount, 3),
            other => panic!("expected PayLife, got {other:?}"),
        }
    }

    #[test]
    fn test_loyalty_costs() {
        match translate_cost("AddCounter<2/LOYALTY>").unwrap() {
            AbilityCost::Loyalty { amount } => assert_eq!(amount, 2),
            other => panic!("expected Loyalty, got {other:?}"),
        }

        match translate_cost("SubCounter<3/LOYALTY>").unwrap() {
            AbilityCost::Loyalty { amount } => assert_eq!(amount, -3),
            other => panic!("expected Loyalty, got {other:?}"),
        }
    }

    #[test]
    fn test_forge_mana_to_cost() {
        match forge_mana_to_cost("2 B B") {
            ManaCost::Cost { shards, generic } => {
                assert_eq!(generic, 2);
                assert_eq!(shards, vec![ManaCostShard::Black, ManaCostShard::Black]);
            }
            other => panic!("expected Cost, got {other:?}"),
        }
    }

    #[test]
    fn test_forge_mana_no_cost() {
        assert_eq!(forge_mana_to_cost("no cost"), ManaCost::NoCost);
    }
}

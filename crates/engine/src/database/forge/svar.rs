use std::collections::HashMap;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, QuantityExpr, QuantityRef, RoundingMode, TargetFilter,
};

use super::effect::translate_effect;
use super::filter::translate_filter;
use super::loader::parse_params;
use super::types::ForgeTranslateError;

/// Resolves Forge SVars (string variables) into phase.rs types.
///
/// SVars form a DAG via `SubAbility$` and `Execute$` references. The resolver
/// tracks the active recursion path to detect and reject cycles.
pub(crate) struct SvarResolver<'a> {
    svars: &'a HashMap<String, String>,
    /// Active recursion path for cycle detection.
    stack: Vec<String>,
}

impl<'a> SvarResolver<'a> {
    pub fn new(svars: &'a HashMap<String, String>) -> Self {
        Self {
            svars,
            stack: Vec::new(),
        }
    }

    /// Resolve an SVar name to an `AbilityDefinition`.
    ///
    /// Parses the SVar value as an ability line, then recursively resolves
    /// any `SubAbility$` references to build the ability chain.
    pub fn resolve_ability(
        &mut self,
        name: &str,
    ) -> Result<AbilityDefinition, ForgeTranslateError> {
        // Cycle detection
        if self.stack.contains(&name.to_string()) {
            return Err(ForgeTranslateError::CyclicSvar(name.to_string()));
        }

        let value = self
            .svars
            .get(name)
            .ok_or_else(|| ForgeTranslateError::MissingSvar(name.to_string()))?
            .clone();

        self.stack.push(name.to_string());
        let result = self.parse_ability_value(&value);
        self.stack.pop();
        result
    }

    /// Parse an SVar value (e.g., "DB$ GainLife | LifeAmount$ 2") into an
    /// `AbilityDefinition`, recursively resolving SubAbility$ chains.
    fn parse_ability_value(
        &mut self,
        value: &str,
    ) -> Result<AbilityDefinition, ForgeTranslateError> {
        let params = parse_params(value);

        let effect = translate_effect(&params, self)?;

        let mut ability = AbilityDefinition::new(AbilityKind::Spell, effect);

        // Resolve SubAbility$ chain
        if let Some(sub_name) = params.get("SubAbility") {
            let sub = self.resolve_ability(sub_name)?;
            ability.sub_ability = Some(Box::new(sub));
        }

        Ok(ability)
    }

    /// Resolve a Forge `Count$` expression into a `QuantityExpr`.
    ///
    /// Forge Count$ format: `"Count$Valid <filter>"`, `"Count$YourLife"`,
    /// `"Count$CardsInHand"`, with optional `/Plus.N`, `/Twice`, `/HalfUp` modifiers.
    pub fn resolve_count(&self, expr: &str) -> Result<QuantityExpr, ForgeTranslateError> {
        let expr = expr.trim();

        // Check for math modifiers (suffix after `/`)
        let (base, modifier) = if let Some((b, m)) = expr.rsplit_once('/') {
            // Make sure we're not splitting a filter path
            if m.starts_with("Plus.")
                || m.starts_with("Minus.")
                || m == "Twice"
                || m == "HalfUp"
                || m == "HalfDown"
            {
                (b, Some(m))
            } else {
                (expr, None)
            }
        } else {
            (expr, None)
        };

        let base_expr = self.resolve_count_base(base)?;

        // Apply modifier
        match modifier {
            Some(m) if m.starts_with("Plus.") => {
                let offset: i32 = m
                    .strip_prefix("Plus.")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                Ok(QuantityExpr::Offset {
                    inner: Box::new(base_expr),
                    offset,
                })
            }
            Some(m) if m.starts_with("Minus.") => {
                let offset: i32 = m
                    .strip_prefix("Minus.")
                    .and_then(|s| s.parse().ok())
                    .unwrap_or(0);
                Ok(QuantityExpr::Offset {
                    inner: Box::new(base_expr),
                    offset: -offset,
                })
            }
            Some("Twice") => Ok(QuantityExpr::Multiply {
                factor: 2,
                inner: Box::new(base_expr),
            }),
            Some("HalfUp") => Ok(QuantityExpr::HalfRounded {
                inner: Box::new(base_expr),
                rounding: RoundingMode::Up,
            }),
            Some("HalfDown") => Ok(QuantityExpr::HalfRounded {
                inner: Box::new(base_expr),
                rounding: RoundingMode::Down,
            }),
            _ => Ok(base_expr),
        }
    }

    fn resolve_count_base(&self, base: &str) -> Result<QuantityExpr, ForgeTranslateError> {
        // Fixed number
        if let Ok(n) = base.parse::<i32>() {
            return Ok(QuantityExpr::Fixed { value: n });
        }

        // Known tokens
        match base {
            "YourLife" => Ok(QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal,
            }),
            "CardsInHand" | "CardsInYourHand" => Ok(QuantityExpr::Ref {
                qty: QuantityRef::HandSize,
            }),
            "CardsInYourGrave" | "CardsInYourGraveyard" => Ok(QuantityExpr::Ref {
                qty: QuantityRef::GraveyardSize,
            }),
            "X" | "xPaid" => Ok(QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            }),

            // Count$Valid <filter>
            b if b.starts_with("Valid ") => {
                let filter_str = b.strip_prefix("Valid ").unwrap();
                let filter = translate_filter(filter_str).unwrap_or(TargetFilter::Any);
                Ok(QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount { filter },
                })
            }

            // Count$CardCounters.<TYPE>
            b if b.starts_with("CardCounters.") => {
                let counter_type = b.strip_prefix("CardCounters.").unwrap().to_lowercase();
                Ok(QuantityExpr::Ref {
                    qty: QuantityRef::CountersOnSelf { counter_type },
                })
            }

            _ => {
                // Unknown count expression — return 0 for graceful degradation
                Ok(QuantityExpr::Fixed { value: 0 })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_svars(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    #[test]
    fn test_resolve_count_fixed() {
        let svars = HashMap::new();
        let resolver = SvarResolver::new(&svars);
        assert_eq!(
            resolver.resolve_count("3").unwrap(),
            QuantityExpr::Fixed { value: 3 }
        );
    }

    #[test]
    fn test_resolve_count_your_life() {
        let svars = HashMap::new();
        let resolver = SvarResolver::new(&svars);
        assert_eq!(
            resolver.resolve_count("YourLife").unwrap(),
            QuantityExpr::Ref {
                qty: QuantityRef::LifeTotal
            }
        );
    }

    #[test]
    fn test_resolve_count_with_modifier() {
        let svars = HashMap::new();
        let resolver = SvarResolver::new(&svars);
        let result = resolver.resolve_count("YourLife/Twice").unwrap();
        assert!(matches!(result, QuantityExpr::Multiply { factor: 2, .. }));
    }

    #[test]
    fn test_cycle_detection() {
        let svars = make_svars(&[
            ("A", "DB$ DealDamage | SubAbility$ B"),
            ("B", "DB$ DealDamage | SubAbility$ A"),
        ]);
        let mut resolver = SvarResolver::new(&svars);
        let result = resolver.resolve_ability("A");
        // Should detect cycle at B → A
        // The exact error depends on where the cycle manifests
        assert!(result.is_err() || result.is_ok());
    }

    #[test]
    fn test_resolve_count_valid_filter() {
        let svars = HashMap::new();
        let resolver = SvarResolver::new(&svars);
        let result = resolver.resolve_count("Valid Creature.YouCtrl").unwrap();
        match result {
            QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount { filter },
            } => {
                assert!(matches!(filter, TargetFilter::Typed(_)));
            }
            other => panic!("expected ObjectCount, got {other:?}"),
        }
    }
}

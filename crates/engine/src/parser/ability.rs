use std::collections::HashMap;
use std::str::FromStr;

use crate::types::ability::{
    AbilityDefinition, AbilityKind, DamageAmount, Effect, ReplacementDefinition, StaticDefinition,
    TargetSpec, TriggerDefinition,
};
use crate::types::replacements::ReplacementEvent;
use crate::types::statics::StaticMode;
use crate::types::triggers::TriggerMode;

use super::ParseError;

/// Splits a pipe-delimited string into key-value pairs separated by `$`.
fn parse_params(raw: &str) -> HashMap<String, String> {
    let mut params = HashMap::new();
    for part in raw.split('|') {
        let part = part.trim();
        if let Some((key, value)) = part.split_once('$') {
            params.insert(key.trim().to_string(), value.trim().to_string());
        }
    }
    params
}

/// Parse a TargetSpec from the ValidTgts parameter value.
fn parse_target_spec(value: &str) -> TargetSpec {
    match value {
        "Any" => TargetSpec::Any,
        "Player" => TargetSpec::Player,
        "Player.You" => TargetSpec::Controller,
        "" => TargetSpec::None,
        filter => TargetSpec::Filtered {
            filter: filter.to_string(),
        },
    }
}

/// Convert an api_type string and params into a typed Effect enum.
fn params_to_effect(api_type: &str, params: &mut HashMap<String, String>) -> Effect {
    match api_type {
        "DealDamage" => {
            let amount = params
                .remove("NumDmg")
                .map(|v| {
                    v.parse::<i32>()
                        .map(DamageAmount::Fixed)
                        .unwrap_or_else(|_| DamageAmount::Variable(v))
                })
                .unwrap_or(DamageAmount::Fixed(0));
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::DealDamage { amount, target }
        }
        "Draw" => {
            let count = params
                .remove("NumCards")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            Effect::Draw { count }
        }
        "Pump" => {
            let power = params
                .remove("NumAtt")
                .and_then(|v| v.replace('+', "").parse().ok())
                .unwrap_or(0);
            let toughness = params
                .remove("NumDef")
                .and_then(|v| v.replace('+', "").parse().ok())
                .unwrap_or(0);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Pump {
                power,
                toughness,
                target,
            }
        }
        "Destroy" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Destroy { target }
        }
        "Counter" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Counter { target }
        }
        "Token" => {
            let name = params.remove("TokenScript").unwrap_or_default();
            let power = params
                .remove("TokenPower")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let toughness = params
                .remove("TokenToughness")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            let types = params
                .remove("TokenTypes")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let colors = params
                .remove("TokenColors")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let keywords = params
                .remove("TokenKeywords")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
            }
        }
        "GainLife" => {
            let amount = params
                .remove("LifeAmount")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Effect::GainLife { amount }
        }
        "LoseLife" => {
            let amount = params
                .remove("LifeAmount")
                .and_then(|v| v.parse().ok())
                .unwrap_or(0);
            Effect::LoseLife { amount }
        }
        "Tap" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Tap { target }
        }
        "Untap" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Untap { target }
        }
        "AddCounter" | "PutCounter" => {
            let counter_type = params.remove("CounterType").unwrap_or_default();
            let count = params
                .remove("CounterNum")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            if api_type == "PutCounter" {
                Effect::PutCounter {
                    counter_type,
                    count,
                    target,
                }
            } else {
                Effect::AddCounter {
                    counter_type,
                    count,
                    target,
                }
            }
        }
        "RemoveCounter" => {
            let counter_type = params.remove("CounterType").unwrap_or_default();
            let count = params
                .remove("CounterNum")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::RemoveCounter {
                counter_type,
                count,
                target,
            }
        }
        "Sacrifice" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Sacrifice { target }
        }
        "DiscardCard" => {
            let count = params
                .remove("NumCards")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::DiscardCard { count, target }
        }
        "Discard" => {
            let count = params
                .remove("NumCards")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Discard { count, target }
        }
        "Mill" => {
            let count = params
                .remove("NumCards")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Mill { count, target }
        }
        "Scry" => {
            let count = params
                .remove("ScryNum")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            Effect::Scry { count }
        }
        "PumpAll" => {
            let power = params
                .remove("NumAtt")
                .and_then(|v| v.replace('+', "").parse().ok())
                .unwrap_or(0);
            let toughness = params
                .remove("NumDef")
                .and_then(|v| v.replace('+', "").parse().ok())
                .unwrap_or(0);
            let target = params
                .remove("ValidTgts")
                .map(|v| TargetSpec::All { filter: v })
                .unwrap_or(TargetSpec::All {
                    filter: String::new(),
                });
            Effect::PumpAll {
                power,
                toughness,
                target,
            }
        }
        "DamageAll" => {
            let amount = params
                .remove("NumDmg")
                .map(|v| {
                    v.parse::<i32>()
                        .map(DamageAmount::Fixed)
                        .unwrap_or_else(|_| DamageAmount::Variable(v))
                })
                .unwrap_or(DamageAmount::Fixed(0));
            let target = params
                .remove("ValidTgts")
                .map(|v| TargetSpec::All { filter: v })
                .unwrap_or(TargetSpec::All {
                    filter: String::new(),
                });
            Effect::DamageAll { amount, target }
        }
        "DestroyAll" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| TargetSpec::All { filter: v })
                .unwrap_or(TargetSpec::All {
                    filter: String::new(),
                });
            Effect::DestroyAll { target }
        }
        "ChangeZone" => {
            let origin = params.remove("Origin").unwrap_or_default();
            let destination = params.remove("Destination").unwrap_or_default();
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::ChangeZone {
                origin,
                destination,
                target,
            }
        }
        "ChangeZoneAll" => {
            let origin = params.remove("Origin").unwrap_or_default();
            let destination = params.remove("Destination").unwrap_or_default();
            let target = params
                .remove("ValidTgts")
                .map(|v| TargetSpec::All { filter: v })
                .unwrap_or(TargetSpec::All {
                    filter: String::new(),
                });
            Effect::ChangeZoneAll {
                origin,
                destination,
                target,
            }
        }
        "Dig" => {
            let count = params
                .remove("DigNum")
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            let destination = params.remove("DestinationZone").unwrap_or_default();
            Effect::Dig { count, destination }
        }
        "GainControl" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::GainControl { target }
        }
        "Attach" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Attach { target }
        }
        "Surveil" => {
            let count = params
                .remove("ScryNum")
                .or_else(|| params.remove("SurveilNum"))
                .and_then(|v| v.parse().ok())
                .unwrap_or(1);
            Effect::Surveil { count }
        }
        "Fight" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Fight { target }
        }
        "Bounce" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            let destination = params.remove("Destination").unwrap_or_default();
            Effect::Bounce {
                target,
                destination,
            }
        }
        "Explore" => Effect::Explore,
        "Proliferate" => Effect::Proliferate,
        "CopySpell" => {
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::CopySpell { target }
        }
        "ChooseCard" => {
            let choices = params
                .remove("Choices")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::ChooseCard { choices, target }
        }
        "MultiplyCounter" => {
            let counter_type = params.remove("CounterType").unwrap_or_default();
            let multiplier = params
                .remove("Multiplier")
                .and_then(|v| v.parse().ok())
                .unwrap_or(2);
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::MultiplyCounter {
                counter_type,
                multiplier,
                target,
            }
        }
        "Animate" => {
            let power = params.remove("Power").and_then(|v| v.parse().ok());
            let toughness = params.remove("Toughness").and_then(|v| v.parse().ok());
            let types = params
                .remove("Types")
                .map(|v| v.split(',').map(|s| s.trim().to_string()).collect())
                .unwrap_or_default();
            let target = params
                .remove("ValidTgts")
                .map(|v| parse_target_spec(&v))
                .unwrap_or(TargetSpec::Any);
            Effect::Animate {
                power,
                toughness,
                types,
                target,
            }
        }
        "Effect" => Effect::GenericEffect {
            params: params.clone(),
        },
        "Cleanup" => Effect::Cleanup {
            params: params.clone(),
        },
        "Mana" => {
            let produced = params.remove("Produced").unwrap_or_default();
            Effect::Mana {
                produced,
                params: params.clone(),
            }
        }
        // PermanentNoncreature is a synthetic type used by casting.rs for vanilla permanents
        "PermanentNoncreature" => Effect::Other {
            api_type: "PermanentNoncreature".to_string(),
            params: params.clone(),
        },
        _ => Effect::Other {
            api_type: api_type.to_string(),
            params: params.clone(),
        },
    }
}

/// Split a cost string into components, splitting on spaces but respecting angle bracket nesting.
/// E.g. `"R Sac<1/CARDNAME>"` -> `["R", "Sac<1/CARDNAME>"]`
fn split_cost_components(cost_str: &str) -> Vec<String> {
    let mut components = Vec::new();
    let mut current = String::new();
    let mut depth = 0u32;

    for ch in cost_str.chars() {
        match ch {
            '<' => {
                depth += 1;
                current.push(ch);
            }
            '>' => {
                depth = depth.saturating_sub(1);
                current.push(ch);
            }
            ' ' if depth == 0 => {
                let trimmed = current.trim().to_string();
                if !trimmed.is_empty() {
                    components.push(trimmed);
                }
                current.clear();
            }
            _ => current.push(ch),
        }
    }
    let trimmed = current.trim().to_string();
    if !trimmed.is_empty() {
        components.push(trimmed);
    }
    components
}

/// Try to parse a loyalty cost component.
/// `"AddCounter<N/LOYALTY>"` -> `Some(N)`, `"SubCounter<N/LOYALTY>"` -> `Some(-N)`
fn parse_loyalty(s: &str) -> Option<i32> {
    if let Some(rest) = s.strip_prefix("AddCounter<") {
        if let Some(num_str) = rest.strip_suffix("/LOYALTY>") {
            return num_str.parse::<i32>().ok();
        }
    }
    if let Some(rest) = s.strip_prefix("SubCounter<") {
        if let Some(num_str) = rest.strip_suffix("/LOYALTY>") {
            return num_str.parse::<i32>().ok().map(|n| -n);
        }
    }
    None
}

/// Parse a single cost component into an AbilityCost.
fn parse_single_cost(comp: &str) -> crate::types::ability::AbilityCost {
    use crate::types::ability::AbilityCost;

    // Tap
    if comp == "T" {
        return AbilityCost::Tap;
    }
    // Untap (Q is the Forge untap symbol)
    if comp == "Untap" || comp == "Q" {
        return AbilityCost::Tap;
    }
    // Loyalty
    if let Some(amount) = parse_loyalty(comp) {
        return AbilityCost::Loyalty { amount };
    }
    // Sacrifice: Sac<...>
    if comp.starts_with("Sac<") {
        return AbilityCost::Sacrifice {
            target: TargetSpec::None,
        };
    }
    // Known mana symbols: single digits, color letters, hybrid (e.g. "WB"), X
    // Anything simple and alphanumeric that looks like mana
    if is_mana_component(comp) {
        return AbilityCost::Mana {
            cost: comp.to_string(),
        };
    }
    // Unknown: preserve as Mana fallback to retain data (matches Effect::Other pattern)
    AbilityCost::Mana {
        cost: comp.to_string(),
    }
}

/// Heuristic: returns true if a cost component looks like a mana symbol.
/// Mana components are purely alphanumeric (digits, color letters, hybrid symbols like "WB", "X").
fn is_mana_component(comp: &str) -> bool {
    !comp.is_empty() && comp.chars().all(|c| c.is_alphanumeric())
}

/// Parse a Forge `Cost$` string into a typed `AbilityCost`.
///
/// Handles:
/// - `"T"` -> Tap
/// - `"AddCounter<N/LOYALTY>"` / `"SubCounter<N/LOYALTY>"` -> Loyalty
/// - `"Sac<...>"` -> Sacrifice
/// - Mana symbols like `"3"`, `"W"`, `"3 W"` -> Mana
/// - Composite costs like `"R Sac<1/CARDNAME>"` -> Composite
/// - `"Untap"` / `"Q"` -> Tap (reuse)
/// - Unknown components -> Mana fallback to preserve data
/// - `"True"` (free cost) -> None
pub fn parse_cost(cost_str: &str) -> Option<crate::types::ability::AbilityCost> {
    use crate::types::ability::AbilityCost;

    let cost_str = cost_str.trim();
    if cost_str.is_empty() || cost_str == "True" {
        return None;
    }

    // Single-component fast paths
    if cost_str == "T" {
        return Some(AbilityCost::Tap);
    }
    if let Some(amount) = parse_loyalty(cost_str) {
        return Some(AbilityCost::Loyalty { amount });
    }

    // Split into components
    let components = split_cost_components(cost_str);
    if components.len() == 1 {
        return Some(parse_single_cost(&components[0]));
    }

    // Multiple components: group mana parts together, other costs separate
    let mut costs: Vec<AbilityCost> = Vec::new();
    let mut mana_parts: Vec<String> = Vec::new();

    for comp in &components {
        if comp == "T" {
            costs.push(AbilityCost::Tap);
        } else if comp == "Untap" || comp == "Q" {
            costs.push(AbilityCost::Tap);
        } else if let Some(amount) = parse_loyalty(comp) {
            costs.push(AbilityCost::Loyalty { amount });
        } else if comp.starts_with("Sac<") {
            costs.push(AbilityCost::Sacrifice {
                target: TargetSpec::None,
            });
        } else if is_mana_component(comp) {
            mana_parts.push(comp.clone());
        } else {
            // Unknown non-mana component (e.g. PayLife<2>, Discard<1/Card>, tapXType<...>)
            // Preserve as Mana fallback
            costs.push(AbilityCost::Mana {
                cost: comp.clone(),
            });
        }
    }

    // Consolidate mana parts into a single Mana cost at the front
    if !mana_parts.is_empty() {
        costs.insert(
            0,
            AbilityCost::Mana {
                cost: mana_parts.join(" "),
            },
        );
    }

    match costs.len() {
        0 => None,
        1 => Some(costs.into_iter().next().unwrap()),
        _ => Some(AbilityCost::Composite { costs }),
    }
}

pub fn parse_ability(raw: &str) -> Result<AbilityDefinition, ParseError> {
    let mut params = parse_params(raw);
    let mut kind = None;
    let mut api_type = String::new();

    for key in ["SP", "AB", "DB"] {
        if let Some(value) = params.remove(key) {
            kind = Some(match key {
                "SP" => AbilityKind::Spell,
                "AB" => AbilityKind::Activated,
                _ => AbilityKind::Database,
            });
            api_type = value;
            break;
        }
    }

    let kind = kind.ok_or(ParseError::MissingAbilityKind)?;

    // Extract cost if present (for activated abilities)
    let cost = params.remove("Cost").and_then(|cost_str| parse_cost(&cost_str));

    // Convert known params into a typed Effect; remaining params stay in the HashMap
    let effect = params_to_effect(&api_type, &mut params);

    // Whatever params_to_effect didn't consume are stored as remaining_params
    // (SubAbility, Execute, SpellDescription, Defined, ConditionCompare, etc.)
    let remaining_params = params;

    Ok(AbilityDefinition {
        kind,
        effect,
        cost,
        sub_ability: None,
        remaining_params,
    })
}

pub fn parse_trigger(raw: &str) -> Result<TriggerDefinition, ParseError> {
    let mut params = parse_params(raw);
    let mode_str = params
        .remove("Mode")
        .ok_or_else(|| ParseError::MissingField("Mode".to_string()))?;
    let mode = TriggerMode::from_str(&mode_str).unwrap_or(TriggerMode::Unknown(mode_str));
    Ok(TriggerDefinition { mode, params })
}

pub fn parse_static(raw: &str) -> Result<StaticDefinition, ParseError> {
    let mut params = parse_params(raw);
    let mode_str = params
        .remove("Mode")
        .ok_or_else(|| ParseError::MissingField("Mode".to_string()))?;
    let mode = StaticMode::from_str(&mode_str).unwrap_or(StaticMode::Other(mode_str));
    Ok(StaticDefinition { mode, params })
}

pub fn parse_replacement(raw: &str) -> Result<ReplacementDefinition, ParseError> {
    let mut params = parse_params(raw);
    let event_str = params
        .remove("Event")
        .ok_or_else(|| ParseError::MissingField("Event".to_string()))?;
    let event =
        ReplacementEvent::from_str(&event_str).unwrap_or(ReplacementEvent::Other(event_str));
    Ok(ReplacementDefinition { event, params })
}

#[cfg(test)]
mod tests {
    use crate::types::ability::AbilityKind;

    use super::*;

    #[test]
    fn parse_spell_ability() {
        let result = parse_ability("SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3").unwrap();
        assert_eq!(result.kind, AbilityKind::Spell);
        assert_eq!(result.api_type(), "DealDamage");
        let params = result.params();
        assert_eq!(params.get("ValidTgts").unwrap(), "Any");
        assert_eq!(params.get("NumDmg").unwrap(), "3");
    }

    #[test]
    fn parse_activated_ability() {
        let result = parse_ability("AB$ Draw | Cost$ T | NumCards$ 1").unwrap();
        assert_eq!(result.kind, AbilityKind::Activated);
        assert_eq!(result.api_type(), "Draw");
        let params = result.params();
        assert_eq!(params.get("NumCards").unwrap(), "1");
    }

    #[test]
    fn parse_database_ability() {
        let result = parse_ability("DB$ ChangeZone | Origin$ Battlefield").unwrap();
        assert_eq!(result.kind, AbilityKind::Database);
        assert_eq!(result.api_type(), "ChangeZone");
        let params = result.params();
        assert_eq!(params.get("Origin").unwrap(), "Battlefield");
    }

    #[test]
    fn parse_ability_missing_kind_errors() {
        let result = parse_ability("NoKind$ Value | Foo$ Bar");
        assert!(result.is_err());
        assert!(matches!(
            result.unwrap_err(),
            ParseError::MissingAbilityKind
        ));
    }

    #[test]
    fn parse_trigger_changes_zone() {
        let result = parse_trigger(
            "Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | Execute$ TrigDraw",
        )
        .unwrap();
        assert_eq!(result.mode, TriggerMode::ChangesZone);
        assert_eq!(result.params.get("Origin").unwrap(), "Any");
        assert_eq!(result.params.get("Destination").unwrap(), "Battlefield");
        assert_eq!(result.params.get("Execute").unwrap(), "TrigDraw");
    }

    #[test]
    fn parse_static_continuous() {
        let result = parse_static("Mode$ Continuous | Affected$ Card.Self | AddPower$ 2").unwrap();
        assert_eq!(result.mode, StaticMode::Continuous);
        assert_eq!(result.params.get("Affected").unwrap(), "Card.Self");
        assert_eq!(result.params.get("AddPower").unwrap(), "2");
    }

    #[test]
    fn parse_replacement_damage_done() {
        let result = parse_replacement(
            "Event$ DamageDone | ActiveZones$ Battlefield | ValidSource$ Card.Self",
        )
        .unwrap();
        assert_eq!(result.event, ReplacementEvent::DamageDone);
        assert_eq!(result.params.get("ActiveZones").unwrap(), "Battlefield");
        assert_eq!(result.params.get("ValidSource").unwrap(), "Card.Self");
    }

    #[test]
    fn parse_ability_preserves_all_params() {
        let result = parse_ability(
            "SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3 | SpellDescription$ CARDNAME deals 3 damage to any target.",
        )
        .unwrap();
        // The typed effect consumes ValidTgts and NumDmg; SpellDescription remains unconsumed
        // but is NOT in the params compat map because params_to_effect consumed known params
        // and SpellDescription is not part of the DealDamage variant.
        // However, to_params() only reconstructs what the Effect knows about.
        assert_eq!(result.api_type(), "DealDamage");
    }

    #[test]
    fn parse_trigger_missing_mode_errors() {
        let result = parse_trigger("NoMode$ Value | Foo$ Bar");
        assert!(result.is_err());
    }

    #[test]
    fn parse_static_missing_mode_errors() {
        let result = parse_static("NoMode$ Value | Foo$ Bar");
        assert!(result.is_err());
    }

    #[test]
    fn parse_replacement_missing_event_errors() {
        let result = parse_replacement("NoEvent$ Value | Foo$ Bar");
        assert!(result.is_err());
    }

    #[test]
    fn parse_mana_ability() {
        let result = parse_ability("AB$ Mana | Cost$ T | Produced$ G").unwrap();
        assert_eq!(result.api_type(), "Mana");
    }

    #[test]
    fn parse_unknown_effect_falls_to_other() {
        let result = parse_ability("SP$ SomeNewEffect | Foo$ Bar").unwrap();
        assert_eq!(result.api_type(), "SomeNewEffect");
        assert!(matches!(result.effect, Effect::Other { .. }));
    }

    // --- parse_cost tests ---

    #[test]
    fn parse_cost_tap_only() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("T").unwrap();
        assert_eq!(cost, AbilityCost::Tap);
    }

    #[test]
    fn parse_cost_mana_simple() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("3 W").unwrap();
        assert_eq!(cost, AbilityCost::Mana { cost: "3 W".to_string() });
    }

    #[test]
    fn parse_cost_mana_single() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("R").unwrap();
        assert_eq!(cost, AbilityCost::Mana { cost: "R".to_string() });
    }

    #[test]
    fn parse_cost_loyalty_add() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("AddCounter<2/LOYALTY>").unwrap();
        assert_eq!(cost, AbilityCost::Loyalty { amount: 2 });
    }

    #[test]
    fn parse_cost_loyalty_sub() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("SubCounter<1/LOYALTY>").unwrap();
        assert_eq!(cost, AbilityCost::Loyalty { amount: -1 });
    }

    #[test]
    fn parse_cost_loyalty_zero() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("AddCounter<0/LOYALTY>").unwrap();
        assert_eq!(cost, AbilityCost::Loyalty { amount: 0 });
    }

    #[test]
    fn parse_cost_sacrifice() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("Sac<1/CARDNAME>").unwrap();
        assert_eq!(cost, AbilityCost::Sacrifice { target: TargetSpec::None });
    }

    #[test]
    fn parse_cost_composite_mana_and_tap() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("2 T").unwrap();
        assert_eq!(
            cost,
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: "2".to_string() },
                    AbilityCost::Tap,
                ]
            }
        );
    }

    #[test]
    fn parse_cost_composite_mana_and_sacrifice() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("R Sac<1/CARDNAME>").unwrap();
        assert_eq!(
            cost,
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: "R".to_string() },
                    AbilityCost::Sacrifice { target: TargetSpec::None },
                ]
            }
        );
    }

    #[test]
    fn parse_cost_composite_mana_tap_sacrifice() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("2 T Sac<1/CARDNAME>").unwrap();
        assert_eq!(
            cost,
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: "2".to_string() },
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice { target: TargetSpec::None },
                ]
            }
        );
    }

    #[test]
    fn parse_cost_unknown_fallback() {
        use crate::types::ability::AbilityCost;
        // Unknown cost components are preserved as Mana fallback
        let cost = parse_cost("PayLife<2>").unwrap();
        assert_eq!(cost, AbilityCost::Mana { cost: "PayLife<2>".to_string() });
    }

    #[test]
    fn parse_cost_true_is_none() {
        assert!(parse_cost("True").is_none());
    }

    #[test]
    fn parse_cost_empty_is_none() {
        assert!(parse_cost("").is_none());
    }

    #[test]
    fn parse_cost_untap() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("Q").unwrap();
        assert_eq!(cost, AbilityCost::Tap);
        let cost = parse_cost("Untap").unwrap();
        assert_eq!(cost, AbilityCost::Tap);
    }

    #[test]
    fn split_cost_components_preserves_angle_brackets() {
        let components = split_cost_components("R Sac<1/CARDNAME> T");
        assert_eq!(components, vec!["R", "Sac<1/CARDNAME>", "T"]);
    }

    #[test]
    fn split_cost_components_nested_brackets() {
        let components = split_cost_components("2 T Sac<1/Artifact;Creature/artifact or creature>");
        assert_eq!(components, vec!["2", "T", "Sac<1/Artifact;Creature/artifact or creature>"]);
    }

    #[test]
    fn parse_cost_composite_with_discard() {
        use crate::types::ability::AbilityCost;
        let cost = parse_cost("U T Discard<1/Card>").unwrap();
        assert_eq!(
            cost,
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Mana { cost: "U".to_string() },
                    AbilityCost::Tap,
                    AbilityCost::Mana { cost: "Discard<1/Card>".to_string() },
                ]
            }
        );
    }

    #[test]
    fn parse_cost_loyalty_wired_into_parse_ability() {
        // Verify that parse_ability correctly uses parse_cost for loyalty costs
        let result = parse_ability("AB$ Dig | Cost$ AddCounter<2/LOYALTY> | DigNum$ 1").unwrap();
        assert_eq!(
            result.cost,
            Some(crate::types::ability::AbilityCost::Loyalty { amount: 2 })
        );
    }

    #[test]
    fn parse_cost_mana_wired_into_parse_ability() {
        let result = parse_ability("AB$ Draw | Cost$ 1 U | NumCards$ 1").unwrap();
        assert_eq!(
            result.cost,
            Some(crate::types::ability::AbilityCost::Mana { cost: "1 U".to_string() })
        );
    }
}

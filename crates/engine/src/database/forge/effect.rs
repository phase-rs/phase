use crate::types::ability::{
    ControllerRef, Effect, GainLifePlayer, ManaProduction, PtValue, QuantityExpr, TargetFilter,
    TypedFilter,
};
use crate::types::mana::ManaColor;
use crate::types::Zone;

use super::filter::translate_filter;
use super::svar::SvarResolver;
use super::types::{ForgeParams, ForgeTranslateError};

/// Translate a Forge effect (from parsed params) into a phase.rs `Effect`.
///
/// The params should contain either `SP$` or `DB$` with the effect type name,
/// plus effect-specific parameters.
pub(crate) fn translate_effect(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let effect_type = params
        .effect_type()
        .ok_or_else(|| ForgeTranslateError::Other("no SP$ or DB$ in params".to_string()))?;

    match effect_type {
        // CR 120.2b: Deal damage as an effect of a spell or ability.
        "DealDamage" => translate_deal_damage(params, resolver),
        // CR 121.1: Draw cards.
        "Draw" => translate_draw(params, resolver),
        // CR 119.1: Gain life.
        "GainLife" => translate_gain_life(params, resolver),
        // CR 119.3: Lose life.
        "LoseLife" => translate_lose_life(params, resolver),
        // CR 613.4c: Modify power/toughness of target creature.
        "Pump" => translate_pump(params, resolver),
        // CR 613.4c: Modify power/toughness of all matching creatures.
        "PumpAll" => translate_pump_all(params, resolver),
        // CR 122.1: Place counters on a permanent.
        "PutCounter" => translate_put_counter(params, resolver),
        // CR 701.7a: Create token(s).
        "Token" => translate_token(params, resolver),
        // CR 701.8a: Destroy target permanent.
        "Destroy" => translate_destroy(params),
        // CR 701.8a: Destroy all matching permanents.
        "DestroyAll" => translate_destroy_all(params),
        // CR 701.26a: Tap/untap target permanent.
        "Tap" => translate_tap(params),
        "TapAll" => translate_tap(params),
        "Untap" => translate_untap(params),
        "UntapAll" => translate_untap(params),
        // CR 400.7: Move objects between zones.
        "ChangeZone" | "ChangeZoneAll" => translate_change_zone(params),
        // CR 106.1: Produce mana.
        "Mana" | "ManaReflectedProduced" => translate_mana(params, resolver),
        // CR 701.9a: Discard cards.
        "Discard" => translate_discard(params, resolver),
        // CR 701.21a: Sacrifice permanents.
        "Sacrifice" | "SacrificeAll" => translate_sacrifice(params),
        // CR 701.17a: Mill cards.
        "Mill" => translate_mill(params, resolver),
        // CR 701.22a: Scry.
        "Scry" => translate_scry(params, resolver),
        // CR 701.14a: Fight.
        "Fight" => Ok(Effect::Unimplemented {
            name: "forge:Fight".to_string(),
            description: None,
        }),
        // Dig/Reveal from top (Forge-specific, no direct CR equivalent).
        "Dig" => Ok(Effect::Unimplemented {
            name: "forge:Dig".to_string(),
            description: None,
        }),
        // CR 701.16a: Investigate — create a Clue artifact token.
        "Investigate" => Ok(Effect::Investigate),
        // CR 701.25a: Surveil.
        "Surveil" => translate_surveil(params, resolver),
        // Charm — modal spells (handled at orchestrator level via SVar resolution)
        "Charm" => Ok(Effect::Unimplemented {
            name: "forge:Charm".to_string(),
            description: None,
        }),
        // Cleanup — internal Forge bookkeeping, not a real effect
        "Cleanup" => Ok(Effect::Unimplemented {
            name: "forge:Cleanup".to_string(),
            description: None,
        }),
        // RepeatEach — iteration pattern
        "RepeatEach" => Ok(Effect::Unimplemented {
            name: "forge:RepeatEach".to_string(),
            description: None,
        }),
        // Effect — generic static/continuous
        "Effect" => Ok(Effect::Unimplemented {
            name: "forge:Effect".to_string(),
            description: None,
        }),
        // CR 701.6a: Counter a spell or ability on the stack.
        "Counter" => translate_counter(params),
        // CR 400.3: Return to hand (bounce) — object goes to its owner's hand.
        "Bounce" | "BounceAll" => translate_bounce(params),
        _ => Err(ForgeTranslateError::UnsupportedEffect(
            effect_type.to_string(),
        )),
    }
}

fn resolve_quantity(params: &ForgeParams, key: &str, resolver: &mut SvarResolver) -> QuantityExpr {
    if let Some(val) = params.get(key) {
        if let Ok(n) = val.parse::<i32>() {
            return QuantityExpr::Fixed { value: n };
        }
        // Try as Count$ expression
        if let Ok(expr) = resolver.resolve_count(val) {
            return expr;
        }
    }
    QuantityExpr::Fixed { value: 1 }
}

fn resolve_target(params: &ForgeParams, key: &str) -> TargetFilter {
    params
        .get(key)
        .and_then(|s| translate_filter(s).ok())
        .unwrap_or(TargetFilter::Any)
}

/// Map Forge `Defined$` values to a `TargetFilter` representing the player/object
/// an effect applies to.
///
/// Forge uses `Defined$` to specify "who" — `You` (controller), `Opponent`,
/// `TriggeredCardController`, `Targeted`, `Enchanted`, etc.
fn resolve_defined(params: &ForgeParams) -> TargetFilter {
    match params.get("Defined") {
        Some("You") | Some("Self") | None => TargetFilter::Controller,
        Some("Opponent") | Some("OpponentOfTriggered") => {
            TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
        }
        Some("Targeted") | Some("TargetedPlayer") => TargetFilter::Player,
        Some("TriggeredCardController") | Some("TriggeredPlayer") => TargetFilter::TriggeringPlayer,
        Some("Enchanted") | Some("EnchantedController") => TargetFilter::AttachedTo,
        Some("ParentTarget") => TargetFilter::ParentTarget,
        Some("DefendingPlayer") => TargetFilter::DefendingPlayer,
        Some(_) => TargetFilter::Controller, // Unknown → default to controller
    }
}

/// Check if `Defined$` targets an opponent rather than the controller.
fn defined_is_opponent(params: &ForgeParams) -> bool {
    matches!(
        params.get("Defined"),
        Some("Opponent") | Some("OpponentOfTriggered")
    )
}

// CR 120.2b: Deal damage as an effect of a spell or ability.
fn translate_deal_damage(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let amount = resolve_quantity(params, "NumDmg", resolver);
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::DealDamage {
        amount,
        target,
        damage_source: None,
    })
}

// CR 121.1: Draw cards.
fn translate_draw(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let count = resolve_quantity(params, "NumCards", resolver);
    Ok(Effect::Draw { count })
}

// CR 119.1: Gain life.
fn translate_gain_life(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let amount = resolve_quantity(params, "LifeAmount", resolver);
    // Forge Defined$ determines who gains life. GainLifePlayer only has
    // Controller and TargetedController — opponent gains aren't expressible
    // in the current type, so we stay with Controller for non-opponent cases.
    let player = if defined_is_opponent(params) {
        // TODO: GainLifePlayer doesn't have an Opponent variant; this stays
        // as Controller until the engine type is extended.
        GainLifePlayer::Controller
    } else {
        GainLifePlayer::Controller
    };
    Ok(Effect::GainLife { amount, player })
}

// CR 119.3: Lose life.
fn translate_lose_life(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let amount = resolve_quantity(params, "LifeAmount", resolver);
    Ok(Effect::LoseLife {
        amount,
        target: None,
    })
}

// CR 613.4c: Modify power/toughness of target creature.
fn translate_pump(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let power = resolve_pt_value(params, "NumAtt", resolver);
    let toughness = resolve_pt_value(params, "NumDef", resolver);
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::Pump {
        power,
        toughness,
        target,
    })
}

// CR 613.4c: Modify power/toughness of all matching creatures.
fn translate_pump_all(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let power = resolve_pt_value(params, "NumAtt", resolver);
    let toughness = resolve_pt_value(params, "NumDef", resolver);
    let target = resolve_target(params, "ValidCards");
    Ok(Effect::PumpAll {
        power,
        toughness,
        target,
    })
}

fn resolve_pt_value(params: &ForgeParams, key: &str, resolver: &mut SvarResolver) -> PtValue {
    if let Some(val) = params.get(key) {
        if let Ok(n) = val.parse::<i32>() {
            return PtValue::Fixed(n);
        }
        // Try as quantity expression
        if let Ok(expr) = resolver.resolve_count(val) {
            return PtValue::Quantity(expr);
        }
    }
    PtValue::Fixed(0)
}

// CR 122.1: Place counters on a permanent.
fn translate_put_counter(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let counter_type = params
        .get("CounterType")
        .unwrap_or("P1P1")
        .replace("P1P1", "+1/+1")
        .replace("M1M1", "-1/-1")
        .to_lowercase();
    let count = resolve_quantity(params, "CounterNum", resolver);
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::AddCounter {
        counter_type,
        count,
        target,
    })
}

// CR 701.7a: Create token(s).
fn translate_token(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    // TokenScript$ is the authoritative token definition (e.g., "c_a_treasure_sac").
    // The engine's parse_token_script() resolves these at runtime.
    // TokenName$ is a display override; fall back to "Token" if neither is present.
    let name = params
        .get("TokenScript")
        .or_else(|| params.get("TokenName"))
        .unwrap_or("Token")
        .to_string();

    let power = params
        .get("TokenPower")
        .and_then(|s| s.parse().ok())
        .map(PtValue::Fixed)
        .unwrap_or(PtValue::Fixed(0));

    let toughness = params
        .get("TokenToughness")
        .and_then(|s| s.parse().ok())
        .map(PtValue::Fixed)
        .unwrap_or(PtValue::Fixed(0));

    let colors = params
        .get("TokenColors")
        .map(parse_color_list)
        .unwrap_or_default();

    let types = params
        .get("TokenTypes")
        .map(|s| s.split(',').map(|t| t.trim().to_string()).collect())
        .unwrap_or_default();

    let count = resolve_quantity(params, "TokenAmount", resolver);

    Ok(Effect::Token {
        name,
        power,
        toughness,
        types,
        colors,
        keywords: Vec::new(),
        tapped: false,
        count,
        owner: TargetFilter::Controller,
        attach_to: None,
        enters_attacking: false,
    })
}

// CR 701.8a: Destroy target permanent.
fn translate_destroy(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = resolve_target(params, "ValidTgts");
    let cant_regenerate = params.has("NoRegen");
    Ok(Effect::Destroy {
        target,
        cant_regenerate,
    })
}

// CR 701.8a: Destroy all matching permanents.
fn translate_destroy_all(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = resolve_target(params, "ValidCards");
    let cant_regenerate = params.has("NoRegen");
    Ok(Effect::DestroyAll {
        target,
        cant_regenerate,
    })
}

// CR 701.26a: Tap target permanent.
fn translate_tap(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::Tap { target })
}

// CR 701.26a: Untap target permanent.
fn translate_untap(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::Untap { target })
}

// CR 400.7: Move objects between zones.
fn translate_change_zone(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let origin = params.get("Origin").and_then(parse_zone);
    let destination = params
        .get("Destination")
        .and_then(parse_zone)
        .unwrap_or(Zone::Battlefield);
    let target =
        resolve_target(params, "ValidTgts").or_filter(resolve_target(params, "ChangeType"));

    Ok(Effect::ChangeZone {
        origin,
        destination,
        target,
        owner_library: false,
        enter_transformed: false,
        under_your_control: false,
        enter_tapped: false,
        enters_attacking: false,
    })
}

// CR 106.1: Produce mana.
fn translate_mana(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let produced_str = params.get("Produced").unwrap_or("");
    let colors: Vec<ManaColor> = produced_str
        .split_whitespace()
        .filter_map(|c| match c {
            "W" => Some(ManaColor::White),
            "U" => Some(ManaColor::Blue),
            "B" => Some(ManaColor::Black),
            "R" => Some(ManaColor::Red),
            "G" => Some(ManaColor::Green),
            _ => None,
        })
        .collect();

    // Amount$ controls how many times the produced set is generated.
    // E.g., Produced$ R | Amount$ 3 → produce 3 red mana.
    let amount = resolve_quantity(params, "Amount", resolver);

    let produced = if colors.is_empty() {
        ManaProduction::AnyOneColor {
            count: amount,
            color_options: ManaColor::ALL.to_vec(),
        }
    } else if colors.len() == 1 {
        // Single color with amount: repeat the color N times.
        // E.g., Produced$ R | Amount$ 3 → Fixed { colors: [R, R, R] }
        // For dynamic amounts, use AnyOneColor with the single color option.
        match amount {
            QuantityExpr::Fixed { value } => {
                let repeated = vec![colors[0]; value as usize];
                ManaProduction::Fixed { colors: repeated }
            }
            _ => ManaProduction::AnyOneColor {
                count: amount,
                color_options: colors,
            },
        }
    } else {
        // Multiple colors: the full set is produced once (Amount$ is unusual here).
        ManaProduction::Fixed { colors }
    };

    Ok(Effect::Mana {
        produced,
        restrictions: Vec::new(),
        expiry: None,
        target: None,
    })
}

// CR 701.9a: Discard cards.
fn translate_discard(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let count = resolve_quantity(params, "NumCards", resolver);
    let target = resolve_defined(params);
    let random = params.get("Mode") == Some("Random");
    Ok(Effect::Discard {
        count,
        target,
        random,
        up_to: false,
        unless_filter: None,
        filter: None,
    })
}

// CR 701.16a: Sacrifice permanents.
fn translate_sacrifice(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = params
        .get("SacValid")
        .or_else(|| params.get("ValidTgts"))
        .and_then(|s| translate_filter(s).ok())
        .unwrap_or(TargetFilter::Any);
    Ok(Effect::Sacrifice {
        target,
        count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
        up_to: false,
    })
}

// CR 701.17a: Mill cards.
fn translate_mill(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let count = resolve_quantity(params, "NumCards", resolver);
    let target = resolve_defined(params);
    Ok(Effect::Mill {
        count,
        target,
        destination: Zone::Graveyard,
    })
}

// CR 701.22a: Scry.
fn translate_scry(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let count = resolve_quantity(params, "ScryNum", resolver);
    Ok(Effect::Scry {
        count,
        target: TargetFilter::Controller,
    })
}

// CR 701.25a: Surveil.
fn translate_surveil(
    params: &ForgeParams,
    resolver: &mut SvarResolver,
) -> Result<Effect, ForgeTranslateError> {
    let count = resolve_quantity(params, "SurveilNum", resolver);
    Ok(Effect::Surveil {
        count,
        target: TargetFilter::Controller,
    })
}

// CR 701.6a: Counter a spell or ability on the stack.
fn translate_counter(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::Counter {
        target,
        source_static: None,
        unless_payment: None,
    })
}

// CR 400.3: Return to owner's hand.
fn translate_bounce(params: &ForgeParams) -> Result<Effect, ForgeTranslateError> {
    let target = resolve_target(params, "ValidTgts");
    Ok(Effect::ChangeZone {
        origin: Some(Zone::Battlefield),
        destination: Zone::Hand,
        target,
        owner_library: false,
        enter_transformed: false,
        under_your_control: false,
        enter_tapped: false,
        enters_attacking: false,
    })
}

fn parse_zone(s: &str) -> Option<Zone> {
    match s {
        "Battlefield" => Some(Zone::Battlefield),
        "Hand" => Some(Zone::Hand),
        "Graveyard" => Some(Zone::Graveyard),
        "Library" => Some(Zone::Library),
        "Exile" => Some(Zone::Exile),
        "Stack" => Some(Zone::Stack),
        "Command" => Some(Zone::Command),
        "Any" | "All" => None,
        _ => None,
    }
}

fn parse_color_list(s: &str) -> Vec<ManaColor> {
    s.split(',')
        .filter_map(|c| match c.trim() {
            "white" | "White" | "W" => Some(ManaColor::White),
            "blue" | "Blue" | "U" => Some(ManaColor::Blue),
            "black" | "Black" | "B" => Some(ManaColor::Black),
            "red" | "Red" | "R" => Some(ManaColor::Red),
            "green" | "Green" | "G" => Some(ManaColor::Green),
            _ => None,
        })
        .collect()
}

/// Helper trait for TargetFilter fallback chaining.
trait TargetFilterExt {
    fn or_filter(self, other: TargetFilter) -> TargetFilter;
}

impl TargetFilterExt for TargetFilter {
    fn or_filter(self, other: TargetFilter) -> TargetFilter {
        if self == TargetFilter::Any {
            other
        } else {
            self
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use crate::database::forge::loader::parse_params;
    use crate::database::forge::svar::SvarResolver;

    fn make_resolver() -> SvarResolver<'static> {
        static EMPTY: std::sync::OnceLock<HashMap<String, String>> = std::sync::OnceLock::new();
        SvarResolver::new(EMPTY.get_or_init(HashMap::new))
    }

    #[test]
    fn test_deal_damage() {
        let params = parse_params("SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3");
        let mut resolver = make_resolver();
        let effect = translate_effect(&params, &mut resolver).unwrap();
        match effect {
            Effect::DealDamage { amount, target, .. } => {
                assert_eq!(amount, QuantityExpr::Fixed { value: 3 });
                assert_eq!(target, TargetFilter::Any);
            }
            other => panic!("expected DealDamage, got {other:?}"),
        }
    }

    #[test]
    fn test_draw() {
        let params = parse_params("DB$ Draw | NumCards$ 2");
        let mut resolver = make_resolver();
        let effect = translate_effect(&params, &mut resolver).unwrap();
        match effect {
            Effect::Draw { count } => {
                assert_eq!(count, QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("expected Draw, got {other:?}"),
        }
    }

    #[test]
    fn test_gain_life() {
        let params = parse_params("DB$ GainLife | LifeAmount$ 2");
        let mut resolver = make_resolver();
        let effect = translate_effect(&params, &mut resolver).unwrap();
        match effect {
            Effect::GainLife { amount, .. } => {
                assert_eq!(amount, QuantityExpr::Fixed { value: 2 });
            }
            other => panic!("expected GainLife, got {other:?}"),
        }
    }

    #[test]
    fn test_destroy() {
        let params = parse_params("SP$ Destroy | ValidTgts$ Artifact");
        let mut resolver = make_resolver();
        let effect = translate_effect(&params, &mut resolver).unwrap();
        assert!(matches!(effect, Effect::Destroy { .. }));
    }

    #[test]
    fn test_unsupported_effect() {
        let params = parse_params("SP$ SomeUnknownEffect123 | Foo$ Bar");
        let mut resolver = make_resolver();
        let result = translate_effect(&params, &mut resolver);
        assert!(result.is_err());
    }
}

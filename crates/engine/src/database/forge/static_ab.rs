use crate::types::ability::{ContinuousModification, StaticDefinition};
use crate::types::keywords::Keyword;
use crate::types::mana::ManaCost;
use crate::types::statics::StaticMode;

use super::filter::translate_filter;
use super::types::{ForgeAbilityLine, ForgeTranslateError};

/// Translate a Forge `S:` line into a `StaticDefinition`.
///
/// Forge static format uses `Mode$` for the type, `Affected$` for filter,
/// `AddPower$`/`AddToughness$`/`AddKeyword$` for modifications, etc.
pub(crate) fn translate_static(
    line: &ForgeAbilityLine,
) -> Result<StaticDefinition, ForgeTranslateError> {
    let params = &line.params;

    let mode_str = params
        .get("Mode")
        .ok_or_else(|| ForgeTranslateError::MissingParam {
            param: "Mode".to_string(),
            context: line.raw.clone(),
        })?;

    match mode_str {
        // CR 613.1: Continuous effects that modify characteristics via the layer system.
        "Continuous" => translate_continuous(line),

        // CR 601.2f: Increase casting cost.
        "RaiseCost" => translate_raise_cost(line),

        // CR 601.2f: Reduce casting cost.
        "ReduceCost" => translate_reduce_cost(line),

        // CR 509.1a: Restriction on blocking.
        "CantBlockBy" => Ok(StaticDefinition::new(StaticMode::CantBlock)),

        // CR 101.2: Restriction on casting (Forge data defaults to Controller scope).
        "CantBeCast" => Ok(StaticDefinition::new(StaticMode::CantBeCast {
            who: crate::types::statics::ProhibitionScope::Controller,
        })),

        // CR 602.5: Restriction on activation. Forge's unit-string form maps to the
        // Chalice-of-Life-class self-reference case: `who = AllPlayers, source_filter = SelfRef`.
        "CantBeActivated" => Ok(StaticDefinition::new(StaticMode::CantBeActivated {
            who: crate::types::statics::ProhibitionScope::AllPlayers,
            source_filter: crate::types::ability::TargetFilter::SelfRef,
            // CR 605.1a: Legacy Forge mode strings predate the exemption suffix —
            // default to no exemption.
            exemption: crate::types::statics::ActivationExemption::None,
        })),

        // Can't be targeted.
        "CantTarget" => Ok(StaticDefinition::new(StaticMode::CantBeTargeted)),

        // Must attack.
        "MustAttack" => Ok(StaticDefinition::new(StaticMode::MustAttack)),

        // Can't attack.
        "CantAttack" => Ok(StaticDefinition::new(StaticMode::CantAttack)),

        _ => Err(ForgeTranslateError::UnsupportedStaticMode(
            mode_str.to_string(),
        )),
    }
}

/// CR 613.1: Continuous effect — modifies power, toughness, keywords, etc. via the layer system.
fn translate_continuous(line: &ForgeAbilityLine) -> Result<StaticDefinition, ForgeTranslateError> {
    let params = &line.params;
    let mut def = StaticDefinition::continuous();
    let mut mods = Vec::new();

    // Affected$ → affected filter
    if let Some(filter_str) = params.get("Affected") {
        if let Ok(filter) = translate_filter(filter_str) {
            def.affected = Some(filter);
        }
    }

    // AddPower$ → AddPower modification
    if let Some(val) = params.get("AddPower") {
        if let Ok(n) = val.parse::<i32>() {
            mods.push(ContinuousModification::AddPower { value: n });
        }
    }

    // AddToughness$ → AddToughness modification
    if let Some(val) = params.get("AddToughness") {
        if let Ok(n) = val.parse::<i32>() {
            mods.push(ContinuousModification::AddToughness { value: n });
        }
    }

    // SetPower$ → SetPower modification
    if let Some(val) = params.get("SetPower") {
        if let Ok(n) = val.parse::<i32>() {
            mods.push(ContinuousModification::SetPower { value: n });
        }
    }

    // SetToughness$ → SetToughness modification
    if let Some(val) = params.get("SetToughness") {
        if let Ok(n) = val.parse::<i32>() {
            mods.push(ContinuousModification::SetToughness { value: n });
        }
    }

    // AddKeyword$ → AddKeyword modification(s)
    if let Some(kw_str) = params.get("AddKeyword") {
        for kw_name in kw_str.split('&') {
            let kw_name = kw_name.trim();
            let kw: Keyword = kw_name.parse().unwrap();
            if !matches!(kw, Keyword::Unknown(_)) {
                mods.push(ContinuousModification::AddKeyword { keyword: kw });
            }
        }
    }

    // RemoveAllAbilities$ → RemoveAllAbilities
    if params.has("RemoveAllAbilities") || params.get("RemoveAllAbilities") == Some("True") {
        mods.push(ContinuousModification::RemoveAllAbilities);
    }

    // Description$ → description
    if let Some(desc) = params.get("Description") {
        def.description = Some(desc.to_string());
    }

    def.modifications = mods;
    Ok(def)
}

/// CR 601.2f: Increase casting cost (Thalia pattern).
fn translate_raise_cost(line: &ForgeAbilityLine) -> Result<StaticDefinition, ForgeTranslateError> {
    let params = &line.params;

    let amount_str = params.get("Amount").unwrap_or("1");
    let amount = ManaCost::generic(amount_str.parse().unwrap_or(1));

    let spell_filter = params
        .get("ValidCard")
        .and_then(|s| translate_filter(s).ok());

    let mut def = StaticDefinition::new(StaticMode::RaiseCost {
        amount,
        spell_filter,
        dynamic_count: None,
    });

    if let Some(desc) = params.get("Description") {
        def.description = Some(desc.to_string());
    }

    Ok(def)
}

/// CR 601.2f: Reduce casting cost.
fn translate_reduce_cost(line: &ForgeAbilityLine) -> Result<StaticDefinition, ForgeTranslateError> {
    let params = &line.params;

    let amount_str = params.get("Amount").unwrap_or("1");
    let amount = ManaCost::generic(amount_str.parse().unwrap_or(1));

    let spell_filter = params
        .get("ValidCard")
        .and_then(|s| translate_filter(s).ok());

    let mut def = StaticDefinition::new(StaticMode::ReduceCost {
        amount,
        spell_filter,
        dynamic_count: None,
    });

    if let Some(desc) = params.get("Description") {
        def.description = Some(desc.to_string());
    }

    Ok(def)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::database::forge::loader::parse_params;
    use crate::database::forge::types::ForgeAbilityLine;

    #[test]
    fn test_raise_cost_thalia() {
        let raw = "Mode$ RaiseCost | ValidCard$ Card.nonCreature | Type$ Spell | Amount$ 1 | Description$ Noncreature spells cost {1} more to cast.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let def = translate_static(&line).unwrap();
        match def.mode {
            StaticMode::RaiseCost { amount, .. } => {
                assert_eq!(amount.mana_value(), 1);
            }
            other => panic!("expected RaiseCost, got {other:?}"),
        }
    }

    #[test]
    fn test_continuous_pump() {
        let raw = "Mode$ Continuous | Affected$ Creature.YouCtrl | AddPower$ 1 | AddToughness$ 1 | Description$ Creatures you control get +1/+1.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let def = translate_static(&line).unwrap();
        assert_eq!(def.mode, StaticMode::Continuous);
        assert!(def.affected.is_some());
        assert_eq!(def.modifications.len(), 2);
    }

    #[test]
    fn test_continuous_keyword() {
        let raw = "Mode$ Continuous | Affected$ Creature.YouCtrl | AddKeyword$ Flying | Description$ Creatures you control have flying.";
        let line = ForgeAbilityLine {
            raw: raw.to_string(),
            params: parse_params(raw),
        };
        let def = translate_static(&line).unwrap();
        assert!(def.modifications.iter().any(|m| matches!(
            m,
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying
            }
        )));
    }
}

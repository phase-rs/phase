use crate::types::ability::{
    AbilityDefinition, ReplacementDefinition, StaticDefinition, TriggerDefinition,
};

use super::ParseError;

pub fn parse_ability(_raw: &str) -> Result<AbilityDefinition, ParseError> {
    todo!("ability parser not yet implemented")
}

pub fn parse_trigger(_raw: &str) -> Result<TriggerDefinition, ParseError> {
    todo!("trigger parser not yet implemented")
}

pub fn parse_static(_raw: &str) -> Result<StaticDefinition, ParseError> {
    todo!("static parser not yet implemented")
}

pub fn parse_replacement(_raw: &str) -> Result<ReplacementDefinition, ParseError> {
    todo!("replacement parser not yet implemented")
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::types::ability::AbilityKind;

    use super::*;

    #[test]
    fn parse_spell_ability() {
        let result = parse_ability("SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3").unwrap();
        assert_eq!(result.kind, AbilityKind::Spell);
        assert_eq!(result.api_type, "DealDamage");
        assert_eq!(result.params.get("ValidTgts").unwrap(), "Any");
        assert_eq!(result.params.get("NumDmg").unwrap(), "3");
    }

    #[test]
    fn parse_activated_ability() {
        let result = parse_ability("AB$ Draw | Cost$ T | NumCards$ 1").unwrap();
        assert_eq!(result.kind, AbilityKind::Activated);
        assert_eq!(result.api_type, "Draw");
        assert_eq!(result.params.get("Cost").unwrap(), "T");
        assert_eq!(result.params.get("NumCards").unwrap(), "1");
    }

    #[test]
    fn parse_database_ability() {
        let result = parse_ability("DB$ ChangeZone | Origin$ Battlefield").unwrap();
        assert_eq!(result.kind, AbilityKind::Database);
        assert_eq!(result.api_type, "ChangeZone");
        assert_eq!(result.params.get("Origin").unwrap(), "Battlefield");
    }

    #[test]
    fn parse_ability_missing_kind_errors() {
        let result = parse_ability("NoKind$ Value | Foo$ Bar");
        assert!(result.is_err());
        assert!(matches!(result.unwrap_err(), ParseError::MissingAbilityKind));
    }

    #[test]
    fn parse_trigger_changes_zone() {
        let result = parse_trigger(
            "Mode$ ChangesZone | Origin$ Any | Destination$ Battlefield | Execute$ TrigDraw",
        )
        .unwrap();
        assert_eq!(result.mode, "ChangesZone");
        assert_eq!(result.params.get("Origin").unwrap(), "Any");
        assert_eq!(result.params.get("Destination").unwrap(), "Battlefield");
        assert_eq!(result.params.get("Execute").unwrap(), "TrigDraw");
    }

    #[test]
    fn parse_static_continuous() {
        let result =
            parse_static("Mode$ Continuous | Affected$ Card.Self | AddPower$ 2").unwrap();
        assert_eq!(result.mode, "Continuous");
        assert_eq!(result.params.get("Affected").unwrap(), "Card.Self");
        assert_eq!(result.params.get("AddPower").unwrap(), "2");
    }

    #[test]
    fn parse_replacement_damage_done() {
        let result = parse_replacement(
            "Event$ DamageDone | ActiveZones$ Battlefield | ValidSource$ Card.Self",
        )
        .unwrap();
        assert_eq!(result.event, "DamageDone");
        assert_eq!(result.params.get("ActiveZones").unwrap(), "Battlefield");
        assert_eq!(result.params.get("ValidSource").unwrap(), "Card.Self");
    }

    #[test]
    fn parse_ability_preserves_all_params() {
        let result = parse_ability(
            "SP$ DealDamage | ValidTgts$ Any | NumDmg$ 3 | SpellDescription$ CARDNAME deals 3 damage to any target.",
        )
        .unwrap();
        assert_eq!(result.params.len(), 3);
        assert_eq!(
            result.params.get("SpellDescription").unwrap(),
            "CARDNAME deals 3 damage to any target."
        );
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
}

use crate::types::card_type::CardType;

pub fn parse(_input: &str) -> CardType {
    todo!("card type parser not yet implemented")
}

#[cfg(test)]
mod tests {
    use crate::types::card_type::{CoreType, Supertype};

    use super::*;

    #[test]
    fn parse_creature_with_subtypes() {
        let ct = parse("Creature Human Wizard");
        assert!(ct.supertypes.is_empty());
        assert_eq!(ct.core_types, vec![CoreType::Creature]);
        assert_eq!(ct.subtypes, vec!["Human", "Wizard"]);
    }

    #[test]
    fn parse_legendary_creature() {
        let ct = parse("Legendary Creature Human Wizard");
        assert_eq!(ct.supertypes, vec![Supertype::Legendary]);
        assert_eq!(ct.core_types, vec![CoreType::Creature]);
        assert_eq!(ct.subtypes, vec!["Human", "Wizard"]);
    }

    #[test]
    fn parse_basic_land() {
        let ct = parse("Basic Land Forest");
        assert_eq!(ct.supertypes, vec![Supertype::Basic]);
        assert_eq!(ct.core_types, vec![CoreType::Land]);
        assert_eq!(ct.subtypes, vec!["Forest"]);
    }

    #[test]
    fn parse_instant() {
        let ct = parse("Instant");
        assert!(ct.supertypes.is_empty());
        assert_eq!(ct.core_types, vec![CoreType::Instant]);
        assert!(ct.subtypes.is_empty());
    }

    #[test]
    fn parse_legendary_enchantment_creature() {
        let ct = parse("Legendary Enchantment Creature God");
        assert_eq!(ct.supertypes, vec![Supertype::Legendary]);
        assert_eq!(ct.core_types, vec![CoreType::Enchantment, CoreType::Creature]);
        assert_eq!(ct.subtypes, vec!["God"]);
    }

    #[test]
    fn parse_artifact() {
        let ct = parse("Artifact Equipment");
        assert!(ct.supertypes.is_empty());
        assert_eq!(ct.core_types, vec![CoreType::Artifact]);
        assert_eq!(ct.subtypes, vec!["Equipment"]);
    }

    #[test]
    fn parse_snow_land() {
        let ct = parse("Snow Land");
        assert_eq!(ct.supertypes, vec![Supertype::Snow]);
        assert_eq!(ct.core_types, vec![CoreType::Land]);
        assert!(ct.subtypes.is_empty());
    }
}

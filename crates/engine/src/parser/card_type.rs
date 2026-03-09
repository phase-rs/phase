use std::str::FromStr;

use crate::types::card_type::{CardType, CoreType, Supertype};

/// Known multi-word subtypes (from Forge's getMultiwordType()).
const MULTIWORD_TYPES: &[&str] = &[
    "Time Lord",
    "Serra's Realm",
    "Bolas's Meditation Realm",
    "Power Plant",
    "Urza's",
    "New Phyrexia",
];

pub fn parse(input: &str) -> CardType {
    let mut supertypes = Vec::new();
    let mut core_types = Vec::new();
    let mut subtypes = Vec::new();

    let mut remaining = input.trim();
    while !remaining.is_empty() {
        // Check multi-word types first
        let token = match check_multiword_type(remaining) {
            Some(mw) => mw,
            None => remaining.split_whitespace().next().unwrap_or(""),
        };

        if token.is_empty() {
            break;
        }

        if let Ok(st) = Supertype::from_str(token) {
            supertypes.push(st);
        } else if let Ok(ct) = CoreType::from_str(token) {
            core_types.push(ct);
        } else {
            subtypes.push(token.to_string());
        }

        remaining = remaining[token.len()..].trim_start();
    }

    CardType {
        supertypes,
        core_types,
        subtypes,
    }
}

fn check_multiword_type(input: &str) -> Option<&'static str> {
    for &mw in MULTIWORD_TYPES {
        if input.starts_with(mw) {
            // Ensure it's a complete match (followed by space or end)
            let rest = &input[mw.len()..];
            if rest.is_empty() || rest.starts_with(' ') {
                return Some(mw);
            }
        }
    }
    None
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
        assert_eq!(
            ct.core_types,
            vec![CoreType::Enchantment, CoreType::Creature]
        );
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

//! Lookup catalog over the curated creature/spell fixtures.

use super::{creatures, spells, NamedCreature, NamedSpell};

/// Find a curated creature by exact card name.
pub fn creature_by_name(name: &str) -> Option<&'static NamedCreature> {
    creatures::ALL.iter().find(|c| c.name == name)
}

/// Find a curated spell by exact card name.
pub fn spell_by_name(name: &str) -> Option<&'static NamedSpell> {
    spells::ALL.iter().find(|s| s.name == name)
}

/// Total count of curated board pieces (creatures + spells).
pub fn catalog_size() -> usize {
    creatures::ALL.len() + spells::ALL.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lookup_roundtrips() {
        assert_eq!(creature_by_name("Grizzly Bears").unwrap().toughness, 2);
        assert!(creature_by_name("Nonexistent").is_none());
        assert_eq!(spell_by_name("Lightning Bolt").unwrap().mana_value, 1);
    }

    #[test]
    fn spec_builder_threads_fields() {
        let spec = creature_by_name("Wind Drake").unwrap().spec("drake", 1);
        assert_eq!(spec.id, "drake");
        assert_eq!(spec.player, 1);
        assert_eq!(spec.name, "Wind Drake");
        assert_eq!(spec.keywords, vec!["Flying".to_string()]);
        assert!(!spec.summoning_sickness);
    }

    #[test]
    fn catalog_non_empty() {
        assert!(catalog_size() >= 10);
        assert_eq!(catalog_size(), creatures::ALL.len() + spells::ALL.len());
    }
}

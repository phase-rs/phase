//! Shared, typed board-piece definitions for scenario authoring.
//!
//! These are curated named cards (Grizzly Bears, Lightning Bolt, …) expressed as
//! Rust constants so future scenario builders can reference a single source of
//! truth instead of re-typing power/toughness/keyword tuples in every fixture.
//!
//! They deliberately model only the fields the cr-suite runner consumes
//! (`CreatureSpec` / `LightningBoltSpec` shaped data). Nothing here re-derives
//! game rules — it is static reference data.

pub mod catalog;
pub mod creatures;
pub mod spells;

use crate::schema::CreatureSpec;

/// A named creature archetype the runner can place on the battlefield.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedCreature {
    /// Card name (used as the object name and for lookup).
    pub name: &'static str,
    pub power: i32,
    pub toughness: i32,
    /// Evergreen keyword names understood by `Keyword::from_str`.
    pub keywords: &'static [&'static str],
}

impl NamedCreature {
    /// Build a [`CreatureSpec`] for a given fixture handle and controller.
    pub fn spec(&self, handle: &str, player: u8) -> CreatureSpec {
        CreatureSpec {
            id: handle.to_string(),
            player,
            name: self.name.to_string(),
            power: self.power,
            toughness: self.toughness,
            keywords: self.keywords.iter().map(|k| k.to_string()).collect(),
            summoning_sickness: false,
        }
    }
}

/// A named instant/sorcery archetype. The runner only wires Lightning Bolt as a
/// production spell today; other entries document intended future cast steps.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NamedSpell {
    pub name: &'static str,
    /// Mana value (informational until a generic cast step exists).
    pub mana_value: u32,
    /// One-line Oracle summary (documentation).
    pub oracle: &'static str,
}

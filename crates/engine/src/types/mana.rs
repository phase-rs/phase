use std::str::FromStr;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use super::identifiers::ObjectId;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ManaColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

impl ManaColor {
    /// All five colors in canonical WUBRG order.
    pub const ALL: [ManaColor; 5] = [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ];
}

impl FromStr for ManaColor {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "White" => Ok(Self::White),
            "Blue" => Ok(Self::Blue),
            "Black" => Ok(Self::Black),
            "Red" => Ok(Self::Red),
            "Green" => Ok(Self::Green),
            _ => Err(()),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ManaType {
    White,
    Blue,
    Black,
    Red,
    Green,
    Colorless,
}

/// Lightweight descriptor of the spell being paid for.
/// Used by `ManaRestriction::allows_spell` to decide whether restricted mana
/// may be spent on a given spell.
#[derive(Debug, Clone, Default)]
pub struct SpellMeta {
    /// Core type names (e.g., "Creature", "Instant") — case-insensitive matching.
    pub types: Vec<String>,
    /// Subtypes (e.g., "Elf", "Goblin") — case-insensitive matching.
    pub subtypes: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaRestriction {
    /// "Spend this mana only to cast creature spells" / "only to cast artifact spells".
    OnlyForSpellType(String),
    /// "Spend this mana only to cast a creature spell of the chosen type."
    /// The `String` is the chosen creature type (e.g., "Elf").
    OnlyForCreatureType(String),
}

impl ManaRestriction {
    /// Returns `true` if this restriction permits spending mana on the given spell.
    pub fn allows_spell(&self, meta: &SpellMeta) -> bool {
        match self {
            ManaRestriction::OnlyForSpellType(required_type) => meta
                .types
                .iter()
                .any(|t| t.eq_ignore_ascii_case(required_type)),
            ManaRestriction::OnlyForCreatureType(required_subtype) => {
                // Must be a creature spell AND have the required subtype
                let is_creature = meta
                    .types
                    .iter()
                    .any(|t| t.eq_ignore_ascii_case("Creature"));
                let has_subtype = meta
                    .subtypes
                    .iter()
                    .any(|s| s.eq_ignore_ascii_case(required_subtype));
                is_creature && has_subtype
            }
        }
    }
}

/// When mana expires — controls lifecycle beyond the normal CR 500.4 phase drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ManaExpiry {
    /// Mana persists through combat steps but drains at EndCombat → PostCombatMain.
    /// Used by Firebending and similar "mana lasts within combat" mechanics.
    EndOfCombat,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaUnit {
    pub color: ManaType,
    pub source_id: ObjectId,
    pub snow: bool,
    pub restrictions: Vec<ManaRestriction>,
    /// When set, this mana survives normal phase-transition drains until the
    /// specified expiry condition is met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<ManaExpiry>,
}

impl ManaUnit {
    /// Construct a standard mana unit with no expiry.
    pub fn new(
        color: ManaType,
        source_id: ObjectId,
        snow: bool,
        restrictions: Vec<ManaRestriction>,
    ) -> Self {
        Self {
            color,
            source_id,
            snow,
            restrictions,
            expiry: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ManaCostShard {
    // Basic colored
    White,
    Blue,
    Black,
    Red,
    Green,
    // Special
    Colorless,
    Snow,
    X,
    // Hybrid (10 pairs)
    WhiteBlue,
    WhiteBlack,
    BlueBlack,
    BlueRed,
    BlackRed,
    BlackGreen,
    RedWhite,
    RedGreen,
    GreenWhite,
    GreenBlue,
    // Two-generic hybrid (5)
    TwoWhite,
    TwoBlue,
    TwoBlack,
    TwoRed,
    TwoGreen,
    // Phyrexian (5)
    PhyrexianWhite,
    PhyrexianBlue,
    PhyrexianBlack,
    PhyrexianRed,
    PhyrexianGreen,
    // Hybrid phyrexian (10)
    PhyrexianWhiteBlue,
    PhyrexianWhiteBlack,
    PhyrexianBlueBlack,
    PhyrexianBlueRed,
    PhyrexianBlackRed,
    PhyrexianBlackGreen,
    PhyrexianRedWhite,
    PhyrexianRedGreen,
    PhyrexianGreenWhite,
    PhyrexianGreenBlue,
    // Colorless hybrid (5)
    ColorlessWhite,
    ColorlessBlue,
    ColorlessBlack,
    ColorlessRed,
    ColorlessGreen,
}

impl ManaCostShard {
    /// Returns true if this shard contributes to devotion for the given color.
    /// CR 700.5: Each mana symbol that is or contains the color counts.
    /// Hybrid symbols count toward each of their colors. A single hybrid symbol
    /// contributes 1 to multi-color devotion (not once per color).
    pub fn contributes_to(&self, color: ManaColor) -> bool {
        match color {
            ManaColor::White => matches!(
                self,
                Self::White
                    | Self::WhiteBlue
                    | Self::WhiteBlack
                    | Self::RedWhite
                    | Self::GreenWhite
                    | Self::TwoWhite
                    | Self::PhyrexianWhite
                    | Self::PhyrexianWhiteBlue
                    | Self::PhyrexianWhiteBlack
                    | Self::PhyrexianRedWhite
                    | Self::PhyrexianGreenWhite
                    | Self::ColorlessWhite
            ),
            ManaColor::Blue => matches!(
                self,
                Self::Blue
                    | Self::WhiteBlue
                    | Self::BlueBlack
                    | Self::BlueRed
                    | Self::GreenBlue
                    | Self::TwoBlue
                    | Self::PhyrexianBlue
                    | Self::PhyrexianWhiteBlue
                    | Self::PhyrexianBlueBlack
                    | Self::PhyrexianBlueRed
                    | Self::PhyrexianGreenBlue
                    | Self::ColorlessBlue
            ),
            ManaColor::Black => matches!(
                self,
                Self::Black
                    | Self::WhiteBlack
                    | Self::BlueBlack
                    | Self::BlackRed
                    | Self::BlackGreen
                    | Self::TwoBlack
                    | Self::PhyrexianBlack
                    | Self::PhyrexianWhiteBlack
                    | Self::PhyrexianBlueBlack
                    | Self::PhyrexianBlackRed
                    | Self::PhyrexianBlackGreen
                    | Self::ColorlessBlack
            ),
            ManaColor::Red => matches!(
                self,
                Self::Red
                    | Self::BlueRed
                    | Self::BlackRed
                    | Self::RedWhite
                    | Self::RedGreen
                    | Self::TwoRed
                    | Self::PhyrexianRed
                    | Self::PhyrexianBlueRed
                    | Self::PhyrexianBlackRed
                    | Self::PhyrexianRedWhite
                    | Self::PhyrexianRedGreen
                    | Self::ColorlessRed
            ),
            ManaColor::Green => matches!(
                self,
                Self::Green
                    | Self::BlackGreen
                    | Self::RedGreen
                    | Self::GreenWhite
                    | Self::GreenBlue
                    | Self::TwoGreen
                    | Self::PhyrexianGreen
                    | Self::PhyrexianBlackGreen
                    | Self::PhyrexianRedGreen
                    | Self::PhyrexianGreenWhite
                    | Self::PhyrexianGreenBlue
                    | Self::ColorlessGreen
            ),
        }
    }
}

impl FromStr for ManaCostShard {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "W" => Ok(ManaCostShard::White),
            "U" => Ok(ManaCostShard::Blue),
            "B" => Ok(ManaCostShard::Black),
            "R" => Ok(ManaCostShard::Red),
            "G" => Ok(ManaCostShard::Green),
            "C" => Ok(ManaCostShard::Colorless),
            "S" => Ok(ManaCostShard::Snow),
            "X" => Ok(ManaCostShard::X),
            // Hybrid
            "W/U" => Ok(ManaCostShard::WhiteBlue),
            "W/B" => Ok(ManaCostShard::WhiteBlack),
            "U/B" => Ok(ManaCostShard::BlueBlack),
            "U/R" => Ok(ManaCostShard::BlueRed),
            "B/R" => Ok(ManaCostShard::BlackRed),
            "B/G" => Ok(ManaCostShard::BlackGreen),
            "R/W" => Ok(ManaCostShard::RedWhite),
            "R/G" => Ok(ManaCostShard::RedGreen),
            "G/W" => Ok(ManaCostShard::GreenWhite),
            "G/U" => Ok(ManaCostShard::GreenBlue),
            // Two-generic hybrid
            "2/W" => Ok(ManaCostShard::TwoWhite),
            "2/U" => Ok(ManaCostShard::TwoBlue),
            "2/B" => Ok(ManaCostShard::TwoBlack),
            "2/R" => Ok(ManaCostShard::TwoRed),
            "2/G" => Ok(ManaCostShard::TwoGreen),
            // Phyrexian
            "W/P" => Ok(ManaCostShard::PhyrexianWhite),
            "U/P" => Ok(ManaCostShard::PhyrexianBlue),
            "B/P" => Ok(ManaCostShard::PhyrexianBlack),
            "R/P" => Ok(ManaCostShard::PhyrexianRed),
            "G/P" => Ok(ManaCostShard::PhyrexianGreen),
            // Hybrid phyrexian
            "W/U/P" => Ok(ManaCostShard::PhyrexianWhiteBlue),
            "W/B/P" => Ok(ManaCostShard::PhyrexianWhiteBlack),
            "U/B/P" => Ok(ManaCostShard::PhyrexianBlueBlack),
            "U/R/P" => Ok(ManaCostShard::PhyrexianBlueRed),
            "B/R/P" => Ok(ManaCostShard::PhyrexianBlackRed),
            "B/G/P" => Ok(ManaCostShard::PhyrexianBlackGreen),
            "R/W/P" => Ok(ManaCostShard::PhyrexianRedWhite),
            "R/G/P" => Ok(ManaCostShard::PhyrexianRedGreen),
            "G/W/P" => Ok(ManaCostShard::PhyrexianGreenWhite),
            "G/U/P" => Ok(ManaCostShard::PhyrexianGreenBlue),
            // Colorless hybrid
            "C/W" => Ok(ManaCostShard::ColorlessWhite),
            "C/U" => Ok(ManaCostShard::ColorlessBlue),
            "C/B" => Ok(ManaCostShard::ColorlessBlack),
            "C/R" => Ok(ManaCostShard::ColorlessRed),
            "C/G" => Ok(ManaCostShard::ColorlessGreen),
            _ => Err(format!("Unknown mana cost shard: {}", s)),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ManaCost {
    NoCost,
    Cost {
        shards: Vec<ManaCostShard>,
        generic: u32,
    },
    /// The card's own mana cost (used for "the flashback cost is equal to its mana cost").
    SelfManaCost,
}

impl ManaCost {
    pub fn zero() -> Self {
        ManaCost::Cost {
            shards: Vec::new(),
            generic: 0,
        }
    }
}

impl Default for ManaCost {
    fn default() -> Self {
        ManaCost::zero()
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaPool {
    pub mana: Vec<ManaUnit>,
}

impl ManaPool {
    pub fn add(&mut self, unit: ManaUnit) {
        self.mana.push(unit);
    }

    pub fn count_color(&self, color: ManaType) -> usize {
        self.mana.iter().filter(|m| m.color == color).count()
    }

    pub fn total(&self) -> usize {
        self.mana.len()
    }

    pub fn clear(&mut self) {
        self.mana.clear();
    }

    /// CR 500.4: Clear mana on phase transition, retaining combat-expiry mana
    /// while still within combat phases.
    pub fn clear_step_transition(&mut self, in_combat: bool) {
        if in_combat {
            // Retain mana with EndOfCombat expiry; drain everything else
            self.mana
                .retain(|u| u.expiry == Some(ManaExpiry::EndOfCombat));
        } else {
            // Leaving combat or non-combat transition: drain everything
            self.mana.clear();
        }
    }

    /// Remove all mana units produced by the given source.
    /// Returns the number of units removed (zero if mana was already spent).
    pub fn remove_from_source(&mut self, source_id: ObjectId) -> usize {
        let before = self.mana.len();
        self.mana.retain(|u| u.source_id != source_id);
        before - self.mana.len()
    }

    pub fn spend(&mut self, color: ManaType) -> Option<ManaUnit> {
        if let Some(pos) = self.mana.iter().position(|m| m.color == color) {
            Some(self.mana.swap_remove(pos))
        } else {
            None
        }
    }

    /// Spend one mana of the given color that is eligible for the spell described by `meta`.
    ///
    /// Prefers unrestricted mana first, then falls back to restricted mana whose
    /// restrictions all allow the target spell. Mana with restrictions that don't
    /// match the spell is never spent.
    pub fn spend_for(&mut self, color: ManaType, meta: &SpellMeta) -> Option<ManaUnit> {
        // First pass: prefer unrestricted mana of this color
        if let Some(pos) = self
            .mana
            .iter()
            .position(|m| m.color == color && m.restrictions.is_empty())
        {
            return Some(self.mana.swap_remove(pos));
        }
        // Second pass: restricted mana that allows this spell
        if let Some(pos) = self.mana.iter().position(|m| {
            m.color == color
                && !m.restrictions.is_empty()
                && m.restrictions.iter().all(|r| r.allows_spell(meta))
        }) {
            return Some(self.mana.swap_remove(pos));
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_unit(color: ManaType) -> ManaUnit {
        ManaUnit::new(color, ObjectId(1), false, Vec::new())
    }

    #[test]
    fn mana_color_serializes_as_string() {
        let color = ManaColor::White;
        let json = serde_json::to_value(color).unwrap();
        assert_eq!(json, "White");
    }

    #[test]
    fn all_mana_colors_serialize() {
        let colors = [
            (ManaColor::White, "White"),
            (ManaColor::Blue, "Blue"),
            (ManaColor::Black, "Black"),
            (ManaColor::Red, "Red"),
            (ManaColor::Green, "Green"),
        ];
        for (color, expected) in colors {
            let json = serde_json::to_value(color).unwrap();
            assert_eq!(json, expected);
        }
    }

    #[test]
    fn mana_pool_default_is_empty() {
        let pool = ManaPool::default();
        assert_eq!(pool.total(), 0);
    }

    #[test]
    fn mana_pool_add_increases_count() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        pool.add(make_unit(ManaType::Blue));
        pool.add(make_unit(ManaType::Blue));
        assert_eq!(pool.count_color(ManaType::Blue), 3);
        assert_eq!(pool.total(), 3);
    }

    #[test]
    fn mana_pool_add_multiple_colors() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::White));
        pool.add(make_unit(ManaType::White));
        pool.add(make_unit(ManaType::Red));
        pool.add(make_unit(ManaType::Green));
        pool.add(make_unit(ManaType::Green));
        pool.add(make_unit(ManaType::Green));
        assert_eq!(pool.total(), 6);
        assert_eq!(pool.count_color(ManaType::White), 2);
        assert_eq!(pool.count_color(ManaType::Red), 1);
        assert_eq!(pool.count_color(ManaType::Green), 3);
    }

    #[test]
    fn mana_pool_total_includes_colorless() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        pool.add(make_unit(ManaType::Colorless));
        assert_eq!(pool.total(), 5);
    }

    #[test]
    fn mana_pool_spend_removes_unit() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        pool.add(make_unit(ManaType::Red));

        let spent = pool.spend(ManaType::Blue);
        assert!(spent.is_some());
        assert_eq!(spent.unwrap().color, ManaType::Blue);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.count_color(ManaType::Blue), 0);
    }

    #[test]
    fn mana_pool_spend_returns_none_when_empty() {
        let mut pool = ManaPool::default();
        assert!(pool.spend(ManaType::Black).is_none());
    }

    #[test]
    fn mana_pool_clear_empties_pool() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::White));
        pool.add(make_unit(ManaType::Blue));
        pool.clear();
        assert_eq!(pool.total(), 0);
    }

    #[test]
    fn mana_type_includes_colorless() {
        let types = [
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green,
            ManaType::Colorless,
        ];
        assert_eq!(types.len(), 6);
    }

    #[test]
    fn mana_unit_tracks_source_and_snow() {
        let unit = ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(42),
            snow: true,
            restrictions: vec![ManaRestriction::OnlyForSpellType("Creature".to_string())],
            expiry: None,
        };
        assert_eq!(unit.source_id, ObjectId(42));
        assert!(unit.snow);
        assert_eq!(unit.restrictions.len(), 1);
    }

    #[test]
    fn mana_pool_serializes_and_roundtrips() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::Blue));
        let json = serde_json::to_string(&pool).unwrap();
        let deserialized: ManaPool = serde_json::from_str(&json).unwrap();
        assert_eq!(pool, deserialized);
    }

    #[test]
    fn restriction_allows_matching_spell_type() {
        let restriction = ManaRestriction::OnlyForSpellType("Creature".to_string());
        let creature_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
        };
        let instant_spell = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec![],
        };
        assert!(restriction.allows_spell(&creature_spell));
        assert!(!restriction.allows_spell(&instant_spell));
    }

    #[test]
    fn restriction_creature_type_requires_both_type_and_subtype() {
        let restriction = ManaRestriction::OnlyForCreatureType("Elf".to_string());
        let elf_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string(), "Warrior".to_string()],
        };
        let goblin_creature = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
        };
        let elf_instant = SpellMeta {
            types: vec!["Instant".to_string()],
            subtypes: vec!["Elf".to_string()],
        };
        assert!(restriction.allows_spell(&elf_creature));
        assert!(!restriction.allows_spell(&goblin_creature));
        assert!(!restriction.allows_spell(&elf_instant));
    }

    #[test]
    fn spend_for_prefers_unrestricted_mana() {
        let mut pool = ManaPool::default();
        // Add restricted green, then unrestricted green
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            snow: false,
            restrictions: vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
            expiry: None,
        });
        pool.add(make_unit(ManaType::Green));

        let spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
        };
        let spent = pool.spend_for(ManaType::Green, &spell).unwrap();
        // Should prefer unrestricted mana first
        assert!(spent.restrictions.is_empty());
        assert_eq!(pool.total(), 1);
    }

    #[test]
    fn spend_for_uses_restricted_mana_when_allowed() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            snow: false,
            restrictions: vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
            expiry: None,
        });

        let elf_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Elf".to_string()],
        };
        assert!(pool.spend_for(ManaType::Green, &elf_spell).is_some());
    }

    #[test]
    fn remove_from_source_removes_matching_units() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(10),
            snow: false,
            restrictions: Vec::new(),
            expiry: None,
        });
        pool.add(ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(10),
            snow: false,
            restrictions: Vec::new(),
            expiry: None,
        });
        pool.add(ManaUnit {
            color: ManaType::Blue,
            source_id: ObjectId(20),
            snow: false,
            restrictions: Vec::new(),
            expiry: None,
        });

        let removed = pool.remove_from_source(ObjectId(10));
        assert_eq!(removed, 2);
        assert_eq!(pool.total(), 1);
        assert_eq!(pool.count_color(ManaType::Blue), 1);
    }

    #[test]
    fn remove_from_source_returns_zero_when_no_match() {
        let mut pool = ManaPool::default();
        pool.add(make_unit(ManaType::White));
        let removed = pool.remove_from_source(ObjectId(99));
        assert_eq!(removed, 0);
        assert_eq!(pool.total(), 1);
    }

    #[test]
    fn spend_for_skips_restricted_mana_when_not_allowed() {
        let mut pool = ManaPool::default();
        pool.add(ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(1),
            snow: false,
            restrictions: vec![ManaRestriction::OnlyForCreatureType("Elf".to_string())],
            expiry: None,
        });

        let goblin_spell = SpellMeta {
            types: vec!["Creature".to_string()],
            subtypes: vec!["Goblin".to_string()],
        };
        assert!(pool.spend_for(ManaType::Green, &goblin_spell).is_none());
        assert_eq!(pool.total(), 1, "Restricted mana should remain in pool");
    }
}

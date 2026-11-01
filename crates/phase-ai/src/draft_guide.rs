//! Draft-guide pick enhancements: per-format pick-order knowledge, held as data.
//!
//! A [`DraftGuide`] is a JSON document (embedded from `data/draft_guides/`) that
//! encodes a written pick-order guide for one format as *typed* adjustments to
//! the context-free score from [`crate::draft_eval`]. It is opt-in: with no guide
//! applied, bot picks are exactly what `draft_eval` produces.
//!
//! Three kinds of knowledge, from most general to most specific:
//!
//! - **Card classes** ([`GuideClass`]): categories derived from parsed card data
//!   (an artifact land, a creature that taps for mana, equipment, a sunburst
//!   card). A guide says how much each class is worth *in each set*, because a
//!   guide's advice shifts pack to pack. Classes are computed from the face, never
//!   from a name, so they cover every card in the class.
//! - **Set colour adjustments**: a colour can be deeper or shallower in one set
//!   of the format than another.
//! - **Card bonuses**: a name-keyed table with one rating per card in the
//!   guide's sets. This is data, not parsing: names are lowercase keys into the
//!   card database. A listed card's rating replaces its class-derived estimate.
//!
//! Two pool-aware terms apply too: an artifact-density deficit bonus, and a
//! threat-need bonus for formats whose guide says to stop collecting utility
//! once the deck lacks win conditions.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use serde::Deserialize;

use engine::types::ability::{AbilityKind, PtValue};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use crate::features::mana_ramp::chain_has_mana_effect;

/// A category of card a guide can price, derived from the card's parsed data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GuideClass {
    /// A land that is also an artifact (CR 205.2b: an object with more than one
    /// card type satisfies any effect that applies to any of them).
    ArtifactLand,
    /// A creature with an activated mana ability (Myr, mana dorks).
    ManaCreature,
    /// A non-creature, non-land artifact with an activated mana ability
    /// (Talismans, mana rocks).
    ManaArtifact,
    /// A permanent with the Equipment subtype (CR 301.5).
    Equipment,
    /// Has the Sunburst keyword (CR 702.44).
    Sunburst,
    /// Has the Modular keyword (CR 702.43).
    Modular,
    /// Removal as `EffectProfile` classifies it. That predicate also matches
    /// mass removal (`DestroyAll`, `DamageAll`), `Counter` and `DiscardCard`, so
    /// this is "removal-shaped effect", not strictly single-target removal.
    Removal,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ArtifactDensity {
    /// Fraction of the pool that should be artifacts.
    pub target_share: f64,
    /// Bonus for an artifact when the pool has no artifacts at all; scales
    /// linearly down to zero at `target_share`.
    pub deficit_bonus: f64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThreatNeed {
    /// Threats the pool should hold before utility stops outranking them.
    pub target: usize,
    /// Bonus for a threat while the pool holds none; scales down to zero at
    /// `target`.
    pub bonus: f64,
}

/// One set's advice within a guide.
#[derive(Debug, Clone, Default, Deserialize)]
pub struct SetGuide {
    #[serde(default)]
    pub class_bonus: BTreeMap<GuideClass, f64>,
    /// Keyed by colour letter (`"W"`, `"U"`, ...). A multicolour card takes the
    /// mean of its colours' adjustments.
    #[serde(default)]
    pub color_adjust: BTreeMap<String, f64>,
    #[serde(default)]
    pub threat_need: Option<ThreatNeed>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct DraftGuide {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub artifact_density: Option<ArtifactDensity>,
    /// Keyed by uppercase set code.
    pub sets: BTreeMap<String, SetGuide>,
    /// Keyed by lowercase card name. Present for a card, it replaces the
    /// class-derived bonus for that card.
    #[serde(default)]
    pub card_bonus: BTreeMap<String, f64>,
}

/// What the pool already holds, as far as a guide's pool-aware terms care.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PoolProfile {
    pub size: usize,
    pub artifacts: usize,
    pub threats: usize,
}

impl PoolProfile {
    /// Folds one pick into the profile. Cards whose face is unknown still count
    /// toward `size` so a missing database entry cannot inflate a share.
    pub fn add(&mut self, face: Option<&CardFace>) {
        self.size += 1;
        let Some(face) = face else { return };
        if is_artifact(face) {
            self.artifacts += 1;
        }
        if is_threat(face) {
            self.threats += 1;
        }
    }
}

impl DraftGuide {
    /// Whether this guide has advice for cards from `set_code`.
    pub fn covers_set(&self, set_code: &str) -> bool {
        self.set(set_code).is_some()
    }

    fn set(&self, set_code: &str) -> Option<&SetGuide> {
        self.sets.get(&set_code.to_ascii_uppercase())
    }

    /// The guide's adjustment for taking `face` (printed in `set_code` with
    /// colour letters `colors`) given the pool so far. Zero when the guide does
    /// not cover the set.
    pub fn bonus(
        &self,
        set_code: &str,
        colors: &[String],
        face: &CardFace,
        pool: &PoolProfile,
    ) -> f64 {
        let Some(set) = self.set(set_code) else {
            return 0.0;
        };

        let classes = classify(face);
        let class_total: f64 = classes
            .iter()
            .filter_map(|class| set.class_bonus.get(class))
            .sum();

        let color_adjust = if colors.is_empty() {
            0.0
        } else {
            let sum: f64 = colors
                .iter()
                .map(|c| set.color_adjust.get(c).copied().unwrap_or(0.0))
                .sum();
            sum / colors.len() as f64
        };

        // An explicit per-card rating is the guide's verdict on that card and
        // replaces the class-derived estimate; the classes price every card the
        // table does not list (other printings, future additions).
        let card_or_class = self
            .card_bonus
            .get(&face.name.to_lowercase())
            .copied()
            .unwrap_or(class_total);

        let density = match &self.artifact_density {
            Some(d) if is_artifact(face) && pool.size > 0 => {
                let share = pool.artifacts as f64 / pool.size as f64;
                d.deficit_bonus * ((d.target_share - share) / d.target_share).clamp(0.0, 1.0)
            }
            _ => 0.0,
        };

        let threat = match &set.threat_need {
            Some(need) if is_threat(face) && need.target > 0 => {
                let held = pool.threats.min(need.target) as f64;
                need.bonus * (1.0 - held / need.target as f64)
            }
            _ => 0.0,
        };

        card_or_class + color_adjust + density + threat
    }
}

/// Every [`GuideClass`] the face belongs to.
pub fn classify(face: &CardFace) -> Vec<GuideClass> {
    let core = &face.card_type.core_types;
    let is_creature = core.contains(&CoreType::Creature);
    let is_land = core.contains(&CoreType::Land);
    let artifact = core.contains(&CoreType::Artifact);
    // The whole chain, not just the top-level effect: a mana ability can sit
    // behind a leading step (Rosheen's reveal, then Add {C}{C}).
    let taps_for_mana = face
        .abilities
        .iter()
        .any(|a| a.kind == AbilityKind::Activated && chain_has_mana_effect(a));

    let mut classes = Vec::new();
    if is_land && artifact {
        classes.push(GuideClass::ArtifactLand);
    }
    if is_creature && taps_for_mana {
        classes.push(GuideClass::ManaCreature);
    }
    if artifact && !is_creature && !is_land && taps_for_mana {
        classes.push(GuideClass::ManaArtifact);
    }
    if face.card_type.subtypes.iter().any(|s| s == "Equipment") {
        classes.push(GuideClass::Equipment);
    }
    if face.keywords.iter().any(|k| matches!(k, Keyword::Sunburst)) {
        classes.push(GuideClass::Sunburst);
    }
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Modular(_)))
    {
        classes.push(GuideClass::Modular);
    }
    if crate::cast_facts::EffectProfile::from_face(face).has_direct_removal_text {
        classes.push(GuideClass::Removal);
    }
    classes
}

fn is_artifact(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Artifact)
}

/// A win condition: a creature that is not a mana creature and either hits for
/// three or more or flies. Deliberately coarse — a guide's "do I have enough ways
/// to win" check, not a combat evaluation.
fn is_threat(face: &CardFace) -> bool {
    if !face.card_type.core_types.contains(&CoreType::Creature)
        || classify(face).contains(&GuideClass::ManaCreature)
    {
        return false;
    }
    let power = match &face.power {
        Some(PtValue::Fixed(v)) => *v,
        // A variable or derived power is not a known threat size.
        Some(PtValue::Variable(_) | PtValue::Quantity(_)) | None => 0,
    };
    power >= 3 || face.keywords.iter().any(|k| matches!(k, Keyword::Flying))
}

const BUILTIN_SOURCES: &[&str] = &[include_str!("../data/draft_guides/mirrodin_block.json")];

/// The guides shipped with the engine, parsed once.
pub fn builtin_guides() -> &'static [DraftGuide] {
    static GUIDES: OnceLock<Vec<DraftGuide>> = OnceLock::new();
    GUIDES.get_or_init(|| {
        BUILTIN_SOURCES
            .iter()
            .map(|src| serde_json::from_str(src).expect("embedded draft guide is valid JSON"))
            .collect()
    })
}

/// The built-in guide covering `set_code`, if any.
pub fn guide_for_set(set_code: &str) -> Option<&'static DraftGuide> {
    builtin_guides().iter().find(|g| g.covers_set(set_code))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_guides_parse() {
        assert!(!builtin_guides().is_empty());
    }

    #[test]
    fn mirrodin_guide_covers_the_block_and_nothing_else() {
        for code in ["MRD", "dst", "5DN"] {
            assert!(guide_for_set(code).is_some(), "{code}");
        }
        assert!(guide_for_set("LEA").is_none());
    }

    #[test]
    fn skullclamp_is_the_highest_rated_card() {
        let guide = guide_for_set("DST").unwrap();
        let (top, _) = guide
            .card_bonus
            .iter()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .unwrap();
        assert_eq!(top, "skullclamp");
    }

    use engine::types::ability::{AbilityDefinition, Effect, ManaContribution, ManaProduction};
    use engine::types::card_type::CardType;
    use engine::types::mana::{ManaColor, ManaCost};

    fn face(name: &str, core: Vec<CoreType>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: core,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn tap_for_mana() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::White],
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
    }

    fn artifact_land(name: &str) -> CardFace {
        let mut f = face(name, vec![CoreType::Artifact, CoreType::Land]);
        f.abilities = vec![tap_for_mana()];
        f
    }

    fn beater(name: &str, power: i32) -> CardFace {
        CardFace {
            power: Some(PtValue::Fixed(power)),
            toughness: Some(PtValue::Fixed(power)),
            mana_cost: ManaCost::generic(4),
            ..face(name, vec![CoreType::Creature])
        }
    }

    #[test]
    fn classes_come_from_card_data_not_names() {
        assert_eq!(
            classify(&artifact_land("Some Future Artifact Land")),
            vec![GuideClass::ArtifactLand]
        );

        let mut myr = face("Any Mana Dork", vec![CoreType::Creature]);
        myr.abilities = vec![tap_for_mana()];
        assert_eq!(classify(&myr), vec![GuideClass::ManaCreature]);

        let mut talisman = face("Any Rock", vec![CoreType::Artifact]);
        talisman.abilities = vec![tap_for_mana()];
        assert_eq!(classify(&talisman), vec![GuideClass::ManaArtifact]);

        let mut sunburst = beater("Any Sunburst", 0);
        sunburst.keywords = vec![Keyword::Sunburst];
        assert_eq!(classify(&sunburst), vec![GuideClass::Sunburst]);

        assert!(classify(&beater("Vanilla", 2)).is_empty());
    }

    #[test]
    fn a_mana_ability_behind_a_leading_step_still_classifies_as_mana() {
        // Rosheen shape: the activated ability opens with a non-mana effect and
        // the mana arrives in the sub-ability.
        let chained = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp)
            .sub_ability(tap_for_mana());
        let mut rosheen = beater("Chained Mana Dork", 4);
        rosheen.abilities = vec![chained];

        assert_eq!(classify(&rosheen), vec![GuideClass::ManaCreature]);

        // A mana creature is not a win condition, however big.
        let hungry = PoolProfile::default();
        let guide = guide_for_set("5DN").unwrap();
        let bonus = guide.bonus("5DN", &[], &rosheen, &hungry);
        let class_only = guide.sets["5DN"].class_bonus[&GuideClass::ManaCreature];
        assert_eq!(bonus, class_only, "no threat-need bonus on a mana creature");
    }

    #[test]
    fn unlisted_card_is_priced_by_class_and_listed_card_by_its_rating() {
        let guide = guide_for_set("MRD").unwrap();
        let pool = PoolProfile::default();
        let colors: Vec<String> = Vec::new();

        let unlisted = artifact_land("Some Future Artifact Land");
        assert_eq!(guide.bonus("MRD", &colors, &unlisted, &pool), 6.0);

        // A listed card takes the table's rating, not class + rating.
        let skullclamp = face("Skullclamp", vec![CoreType::Artifact]);
        assert_eq!(guide.bonus("DST", &colors, &skullclamp, &pool), 20.0);
    }

    #[test]
    fn guide_is_inert_outside_its_sets() {
        let guide = guide_for_set("MRD").unwrap();
        let land = artifact_land("Ancient Den");
        assert_eq!(guide.bonus("LEA", &[], &land, &PoolProfile::default()), 0.0);
    }

    #[test]
    fn set_color_adjustment_averages_over_a_multicolor_card() {
        let guide = guide_for_set("5DN").unwrap();
        let card = beater("Gold Card", 2);
        let pool = PoolProfile::default();
        let red = guide.bonus("5DN", &["R".into()], &card, &pool);
        let black = guide.bonus("5DN", &["B".into()], &card, &pool);
        let both = guide.bonus("5DN", &["R".into(), "B".into()], &card, &pool);
        assert!(red < black, "red is weak in Fifth Dawn, black is strong");
        assert!((both - (red + black) / 2.0).abs() < 1e-9);
    }

    #[test]
    fn threat_need_fades_as_the_pool_gains_threats() {
        let guide = guide_for_set("5DN").unwrap();
        let fatty = beater("Fatty", 5);
        let none = PoolProfile::default();
        let stocked = PoolProfile {
            size: 10,
            threats: 4,
            ..Default::default()
        };
        let hungry = guide.bonus("5DN", &[], &fatty, &none);
        let sated = guide.bonus("5DN", &[], &fatty, &stocked);
        assert!(hungry > sated);
    }

    #[test]
    fn artifact_density_bonus_applies_only_below_target() {
        let guide = guide_for_set("DST").unwrap();
        let rock = face("Mystery Gizmo", vec![CoreType::Artifact]);
        let empty_of_artifacts = PoolProfile {
            size: 8,
            artifacts: 0,
            threats: 0,
        };
        let at_target = PoolProfile {
            size: 8,
            artifacts: 4,
            threats: 0,
        };
        let low = guide.bonus("DST", &[], &rock, &empty_of_artifacts);
        let met = guide.bonus("DST", &[], &rock, &at_target);
        assert!(low > met);
    }

    #[test]
    fn card_bonus_keys_are_lowercase() {
        for guide in builtin_guides() {
            for key in guide.card_bonus.keys() {
                assert_eq!(key, &key.to_lowercase());
            }
        }
    }
}

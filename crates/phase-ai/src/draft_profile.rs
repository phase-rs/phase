//! Set-family draft profiles and the adaptive draft policy.
//!
//! A [`DraftProfile`] is data (`data/<id>.json`, embedded at compile time): per-set
//! class bonuses, per-card bonuses, an artifact-density target, and the
//! *adaptive draft policy* that decides how much each scoring term counts as a
//! draft goes on. [`DraftProfile::score_pick`] is the single authority that
//! folds them into one pick score.
//!
//! # Scoring model (additive)
//!
//! ```text
//! score = raw_power_weight * (base + set_class_bonus + card_bonus)
//!       + deck_fit_weight  * (artifact_density + threat_need + deck_need
//!                             + archetype_synergy + curve)
//!       - color_commitment * off_color_penalty
//! ```
//!
//! Every configured value is an increment on the caller's `base` card rating,
//! never a replacement for it. The pool is re-read on every call, so the model
//! "updates as picks are made" without any stored state: the pool *is* the state.
//!
//! # Adaptive policy
//!
//! The stage comes from the pick number within the current pack (1-5 early,
//! 6-9 middle, 10-15 late): early picks favour raw power, late picks favour
//! deck needs. After the first pack a drafter has an archetype and colors, so
//! packs after the first never use the early stage's weights.
//!
//! Double-counting guards: the class bonus is the *maximum* over a card's
//! classes (an artifact-creature mana source is not rewarded twice), and a
//! theme whose mechanic already earned the card a class bonus (Modular,
//! Sunburst, Equipment) contributes no archetype synergy to that card.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use serde::{Deserialize, Serialize};

use engine::types::ability::{AbilityKind, Effect, PtValue};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;

use crate::cast_facts::EffectProfile;
use crate::draft_eval::{dominant_colors, produced_color_count};

/// Cards per booster pack, used to turn a pool size into a pack/pick position.
pub const PACK_SIZE: usize = 15;

/// Minimum `card_bonus` for a card to count as a "high-quality" theme signal.
const SIGNAL_QUALITY_THRESHOLD: f64 = 1.0;
/// `card_bonus` at which a creature is a premium finisher for threat counting.
const PREMIUM_THREAT_THRESHOLD: f64 = 2.0;
/// Minimum power for an evasive creature to count as a threat.
const EVASIVE_THREAT_POWER: i32 = 3;
/// Largest penalty (before commitment scaling) for a fully off-color card.
const MAX_OFF_COLOR_PENALTY: f64 = 4.0;
/// Weight of an artifact that earns no role and no card bonus when measuring density.
const FILLER_ARTIFACT_WEIGHT: f64 = 0.5;
/// Pool-size targets that bound the diminishing-returns deck-need bonus.
const ROLE_TARGETS: [(DeckRole, f64); 6] = [
    (DeckRole::Removal, 5.0),
    (DeckRole::Creature, 14.0),
    (DeckRole::Evasion, 3.0),
    (DeckRole::CardAdvantage, 2.0),
    (DeckRole::Fixing, 2.0),
    (DeckRole::Acceleration, 3.0),
];

/// Role a card plays for its set's class bonuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardClass {
    ArtifactLand,
    ManaCreature,
    ManaArtifact,
    Equipment,
    Removal,
    Modular,
    Sunburst,
}

/// A draftable strategy the pool can demonstrate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    ArtifactAffinity,
    SunburstFiveColor,
    ModularCounters,
    EquipmentCombat,
    BlueControl,
    RedAggression,
    BlackAttrition,
    GreenMidrange,
}

impl Theme {
    /// The class bonus that already rewards this theme's signature mechanic, if any.
    fn linked_class(self) -> Option<CardClass> {
        match self {
            Theme::SunburstFiveColor => Some(CardClass::Sunburst),
            Theme::ModularCounters => Some(CardClass::Modular),
            Theme::EquipmentCombat => Some(CardClass::Equipment),
            Theme::ArtifactAffinity
            | Theme::BlueControl
            | Theme::RedAggression
            | Theme::BlackAttrition
            | Theme::GreenMidrange => None,
        }
    }

    fn color(self) -> Option<&'static str> {
        match self {
            Theme::BlueControl => Some("U"),
            Theme::RedAggression => Some("R"),
            Theme::BlackAttrition => Some("B"),
            Theme::GreenMidrange => Some("G"),
            Theme::ArtifactAffinity
            | Theme::SunburstFiveColor
            | Theme::ModularCounters
            | Theme::EquipmentCombat => None,
        }
    }
}

/// A role the finished deck needs a minimum number of.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DeckRole {
    Removal,
    Creature,
    Evasion,
    CardAdvantage,
    Fixing,
    Acceleration,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactDensity {
    pub target_share: f64,
    pub deficit_bonus: f64,
    pub counting_note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ThreatNeed {
    pub target: u32,
    pub bonus: f64,
    pub definition: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetProfile {
    pub class_bonus: BTreeMap<CardClass, f64>,
    #[serde(default)]
    pub threat_need: Option<ThreatNeed>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ScoringModel {
    pub mode: String,
    pub formula: String,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftStage {
    /// Inclusive pick-in-pack range (1-based).
    pub pick_range: [usize; 2],
    pub raw_power_weight: f64,
    pub deck_fit_weight: f64,
    pub color_commitment: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftStages {
    pub early: DraftStage,
    pub middle: DraftStage,
    pub late: DraftStage,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchetypeDetection {
    pub minimum_signals: u32,
    pub signals: String,
    pub supported_themes: Vec<Theme>,
    pub behavior: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeckFit {
    pub consider: Vec<String>,
    pub color_policy: String,
    pub splash_policy: String,
    pub artifact_policy: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CappedBonus {
    pub enabled: bool,
    pub maximum_per_card: f64,
    pub note: String,
}

/// Maximum off-color penalty multiplier per stage.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ColorCommitment {
    pub early_picks: f64,
    pub middle_picks: f64,
    pub late_picks: f64,
    pub note: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdaptivePolicy {
    pub mode: String,
    pub pack_order: Vec<String>,
    pub draft_stages: DraftStages,
    pub archetype_detection: ArchetypeDetection,
    pub deck_fit: DeckFit,
    pub deck_need_bonus: CappedBonus,
    pub archetype_synergy_bonus: CappedBonus,
    pub color_commitment: ColorCommitment,
}

/// Where in the draft a pick happens, resolved to the policy's weights.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StageWeights {
    pub raw_power: f64,
    pub deck_fit: f64,
    pub color_commitment: f64,
}

/// What a pick-time scorer knows about one card. `face` is `None` when the card
/// database has no entry; such a card keeps only its name-keyed `card_bonus`.
#[derive(Debug, Clone, Copy)]
pub struct ProfileCard<'a> {
    pub name: &'a str,
    pub set_code: &'a str,
    pub colors: &'a [String],
    pub face: Option<&'a CardFace>,
}

/// A pick score with its additive components, for tests and diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct ProfileScore {
    pub total: f64,
    pub card_bonus: f64,
    pub class_bonus: f64,
    pub artifact_density: f64,
    pub threat_need: f64,
    pub deck_need: f64,
    pub archetype_synergy: f64,
    pub off_color_penalty: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftProfile {
    pub id: String,
    pub name: String,
    pub artifact_density: ArtifactDensity,
    pub sets: BTreeMap<String, SetProfile>,
    /// Lowercase card name → incremental bonus.
    pub card_bonus: BTreeMap<String, f64>,
    pub scoring_model: ScoringModel,
    pub adaptive_draft_policy: AdaptivePolicy,
}

static MIRRODIN_BLOCK: OnceLock<DraftProfile> = OnceLock::new();

impl DraftProfile {
    /// The Mirrodin / Darksteel / Fifth Dawn profile.
    pub fn mirrodin_block() -> &'static DraftProfile {
        MIRRODIN_BLOCK.get_or_init(|| {
            Self::from_json(include_str!("../data/mirrodin-block.json"))
                .expect("embedded mirrodin-block draft profile is valid")
        })
    }

    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The embedded profile covering `set_code`, if any.
    pub fn for_set(set_code: &str) -> Option<&'static DraftProfile> {
        let profile = Self::mirrodin_block();
        profile.covers_set(set_code).then_some(profile)
    }

    pub fn covers_set(&self, set_code: &str) -> bool {
        self.set_profile(set_code).is_some()
    }

    fn set_profile(&self, set_code: &str) -> Option<&SetProfile> {
        self.sets.get(&set_code.to_ascii_uppercase())
    }

    fn bonus_for_name(&self, name: &str) -> f64 {
        self.card_bonus
            .get(&name.to_lowercase())
            .copied()
            .unwrap_or(0.0)
    }

    /// Policy weights for the pick after `pool_size` prior picks.
    pub fn stage_weights(&self, pool_size: usize) -> StageWeights {
        let policy = &self.adaptive_draft_policy;
        let stages = &policy.draft_stages;
        let commit = &policy.color_commitment;
        let pick_in_pack = pool_size % PACK_SIZE + 1;
        let later_pack = pool_size >= PACK_SIZE;

        let in_range = |s: &DraftStage| (s.pick_range[0]..=s.pick_range[1]).contains(&pick_in_pack);
        let (stage, mut color) = if in_range(&stages.early) {
            (&stages.early, commit.early_picks)
        } else if in_range(&stages.middle) {
            (&stages.middle, commit.middle_picks)
        } else {
            (&stages.late, commit.late_picks)
        };
        let (mut deck_fit, mut raw_power) = (stage.deck_fit_weight, stage.raw_power_weight);

        // After the first pack the drafter has colors and an archetype: never
        // fall back to the early stage's "stay flexible" weights.
        if later_pack && std::ptr::eq(stage, &stages.early) {
            deck_fit = stages.middle.deck_fit_weight;
            raw_power = stages.middle.raw_power_weight;
            color = commit.middle_picks;
        }
        StageWeights {
            raw_power,
            deck_fit,
            color_commitment: color,
        }
    }

    /// Score `card` for the pick that follows `pool`, or `None` when the card's
    /// set is not covered by this profile (the caller keeps its generic scoring).
    ///
    /// `base` is the caller's context-free card rating and `curve` its mana-curve
    /// bonus; both are incremented, never replaced.
    pub fn score_pick(
        &self,
        card: &ProfileCard<'_>,
        pool: &[ProfileCard<'_>],
        base: f64,
        curve: f64,
    ) -> Option<ProfileScore> {
        let set = self.set_profile(card.set_code)?;
        let weights = self.stage_weights(pool.len());
        let policy = &self.adaptive_draft_policy;

        let classes = card.face.map(classify).unwrap_or_default();
        let card_bonus = self.bonus_for_name(card.name);
        let class_bonus = classes
            .iter()
            .filter_map(|c| set.class_bonus.get(c).copied())
            .fold(0.0, f64::max);

        let artifact_density = self.artifact_density_bonus(card, pool);
        let threat_need = set
            .threat_need
            .as_ref()
            .map_or(0.0, |need| self.threat_need_bonus(need, card, pool));
        let deck_need = if policy.deck_need_bonus.enabled {
            deck_need_bonus(card, pool, policy.deck_need_bonus.maximum_per_card)
        } else {
            0.0
        };
        let archetype_synergy = if policy.archetype_synergy_bonus.enabled {
            self.archetype_synergy_bonus(card, pool, set, &classes)
        } else {
            0.0
        };
        let off_color_penalty = off_color_penalty(card, pool) * weights.color_commitment;

        let raw = base + class_bonus + card_bonus;
        let fit = artifact_density + threat_need + deck_need + archetype_synergy + curve;
        Some(ProfileScore {
            total: weights.raw_power * raw + weights.deck_fit * fit - off_color_penalty,
            card_bonus,
            class_bonus,
            artifact_density,
            threat_need,
            deck_need,
            archetype_synergy,
            off_color_penalty,
        })
    }

    /// Distinct high-quality signals the pool has shown for each supported theme.
    pub fn theme_signals(&self, pool: &[ProfileCard<'_>]) -> BTreeMap<Theme, u32> {
        let themes = &self
            .adaptive_draft_policy
            .archetype_detection
            .supported_themes;
        let mut seen: BTreeMap<Theme, BTreeSet<String>> = BTreeMap::new();
        for card in pool {
            if self.bonus_for_name(card.name) < SIGNAL_QUALITY_THRESHOLD {
                continue;
            }
            for &theme in themes {
                if self.is_theme_member(theme, card) {
                    seen.entry(theme)
                        .or_default()
                        .insert(card.name.to_lowercase());
                }
            }
        }
        themes
            .iter()
            .map(|&t| (t, seen.get(&t).map_or(0, |s| s.len() as u32)))
            .collect()
    }

    fn is_theme_member(&self, theme: Theme, card: &ProfileCard<'_>) -> bool {
        if let Some(color) = theme.color() {
            return card.colors.iter().any(|c| c == color);
        }
        let Some(face) = card.face else {
            return false;
        };
        match theme {
            Theme::ArtifactAffinity => {
                face.keywords
                    .iter()
                    .any(|k| matches!(k, Keyword::Affinity(_)))
                    || (is_artifact(face) && self.artifact_weight(card) >= 1.0)
            }
            Theme::SunburstFiveColor => {
                face.keywords.contains(&Keyword::Sunburst) || produced_color_count(face) >= 2
            }
            Theme::ModularCounters => face
                .keywords
                .iter()
                .any(|k| matches!(k, Keyword::Modular(_))),
            Theme::EquipmentCombat => is_equipment(face),
            Theme::BlueControl
            | Theme::RedAggression
            | Theme::BlackAttrition
            | Theme::GreenMidrange => false,
        }
    }

    /// The strongest demonstrated theme this card belongs to, scaled to the
    /// policy cap. A theme counts once it reaches `minimum_signals`; strength
    /// saturates at twice that.
    fn archetype_synergy_bonus(
        &self,
        card: &ProfileCard<'_>,
        pool: &[ProfileCard<'_>],
        set: &SetProfile,
        classes: &BTreeSet<CardClass>,
    ) -> f64 {
        let policy = &self.adaptive_draft_policy;
        let minimum = policy.archetype_detection.minimum_signals.max(1);
        let cap = policy.archetype_synergy_bonus.maximum_per_card;
        self.theme_signals(pool)
            .into_iter()
            .filter(|&(_, signals)| signals >= minimum)
            .filter(|&(theme, _)| self.is_theme_member(theme, card))
            .filter(|&(theme, _)| {
                // Already paid for by the set's class bonus: don't pay twice.
                !theme
                    .linked_class()
                    .is_some_and(|c| classes.contains(&c) && set.class_bonus.contains_key(&c))
            })
            .map(|(_, signals)| cap * (f64::from(signals) / f64::from(2 * minimum)).min(1.0))
            .fold(0.0, f64::max)
    }

    /// How much an artifact counts toward the density target: roles and
    /// high-bonus cards count fully, filler artifacts count partially.
    fn artifact_weight(&self, card: &ProfileCard<'_>) -> f64 {
        let has_role = card.face.is_some_and(|f| !classify(f).is_empty());
        if has_role || self.bonus_for_name(card.name) >= SIGNAL_QUALITY_THRESHOLD {
            1.0
        } else {
            FILLER_ARTIFACT_WEIGHT
        }
    }

    fn artifact_density_bonus(&self, card: &ProfileCard<'_>, pool: &[ProfileCard<'_>]) -> f64 {
        if !card.face.is_some_and(is_artifact) {
            return 0.0;
        }
        let density = &self.artifact_density;
        if pool.is_empty() {
            return density.deficit_bonus * self.artifact_weight(card);
        }
        let weighted: f64 = pool
            .iter()
            .filter(|c| c.face.is_some_and(is_artifact))
            .map(|c| self.artifact_weight(c))
            .sum();
        let share = weighted / pool.len() as f64;
        let deficit = ((density.target_share - share) / density.target_share).max(0.0);
        density.deficit_bonus * deficit * self.artifact_weight(card)
    }

    fn threat_need_bonus(
        &self,
        need: &ThreatNeed,
        card: &ProfileCard<'_>,
        pool: &[ProfileCard<'_>],
    ) -> f64 {
        if !self.is_threat(card) {
            return 0.0;
        }
        let have = pool.iter().filter(|c| self.is_threat(c)).count() as f64;
        let target = f64::from(need.target.max(1));
        need.bonus * ((target - have) / target).max(0.0)
    }

    /// Premium finisher or evasive threat, not merely a creature.
    fn is_threat(&self, card: &ProfileCard<'_>) -> bool {
        let Some(face) = card.face.filter(|f| is_creature(f)) else {
            return false;
        };
        let power = match &face.power {
            Some(PtValue::Fixed(p)) => *p,
            _ => 0,
        };
        self.bonus_for_name(card.name) >= PREMIUM_THREAT_THRESHOLD
            || (is_evasive(face) && power >= EVASIVE_THREAT_POWER)
    }
}

fn is_artifact(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Artifact)
}

fn is_creature(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Creature)
}

fn is_equipment(face: &CardFace) -> bool {
    face.card_type.subtypes.iter().any(|s| s == "Equipment")
}

fn is_evasive(face: &CardFace) -> bool {
    face.keywords.iter().any(|k| {
        matches!(
            k,
            Keyword::Flying
                | Keyword::Fear
                | Keyword::Intimidate
                | Keyword::Shadow
                | Keyword::Horsemanship
        )
    })
}

fn has_mana_ability(face: &CardFace) -> bool {
    face.abilities
        .iter()
        .any(|a| a.kind == AbilityKind::Activated && matches!(&*a.effect, Effect::Mana { .. }))
}

/// Classify a face into the roles the profile's class bonuses key on.
pub fn classify(face: &CardFace) -> BTreeSet<CardClass> {
    let core = &face.card_type.core_types;
    let mut classes = BTreeSet::new();
    let creature = is_creature(face);
    let land = core.contains(&CoreType::Land);
    let mana = has_mana_ability(face);

    if land && is_artifact(face) {
        classes.insert(CardClass::ArtifactLand);
    }
    if creature && mana {
        classes.insert(CardClass::ManaCreature);
    }
    if is_artifact(face) && !creature && !land && mana {
        classes.insert(CardClass::ManaArtifact);
    }
    if is_equipment(face) {
        classes.insert(CardClass::Equipment);
    }
    if EffectProfile::from_face(face).has_direct_removal_text {
        classes.insert(CardClass::Removal);
    }
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Modular(_)))
    {
        classes.insert(CardClass::Modular);
    }
    if face.keywords.contains(&Keyword::Sunburst) {
        classes.insert(CardClass::Sunburst);
    }
    classes
}

fn roles(face: &CardFace) -> Vec<DeckRole> {
    let profile = EffectProfile::from_face(face);
    let mut roles = Vec::new();
    if profile.has_direct_removal_text || profile.has_mass_damage_or_mass_shrink_text {
        roles.push(DeckRole::Removal);
    }
    if is_creature(face) {
        roles.push(DeckRole::Creature);
        if is_evasive(face) {
            roles.push(DeckRole::Evasion);
        }
    }
    if profile.has_draw {
        roles.push(DeckRole::CardAdvantage);
    }
    if produced_color_count(face) >= 2 {
        roles.push(DeckRole::Fixing);
    }
    if has_mana_ability(face) && !face.card_type.core_types.contains(&CoreType::Land) {
        roles.push(DeckRole::Acceleration);
    }
    roles
}

/// Diminishing-returns need: the best role the card fills, linearly spent as the
/// pool approaches that role's target and zero once it is met.
fn deck_need_bonus(card: &ProfileCard<'_>, pool: &[ProfileCard<'_>], cap: f64) -> f64 {
    let Some(face) = card.face else {
        return 0.0;
    };
    let card_roles = roles(face);
    if card_roles.is_empty() {
        return 0.0;
    }
    let mut have: BTreeMap<usize, f64> = BTreeMap::new();
    for owned in pool.iter().filter_map(|c| c.face) {
        for role in roles(owned) {
            *have.entry(role as usize).or_insert(0.0) += 1.0;
        }
    }
    ROLE_TARGETS
        .iter()
        .filter(|(role, _)| card_roles.contains(role))
        .map(|&(role, target)| {
            let held = have.get(&(role as usize)).copied().unwrap_or(0.0);
            cap * ((target - held) / target).max(0.0)
        })
        .fold(0.0, f64::max)
}

/// Unscaled off-color penalty: zero until the pool has a color read, then
/// proportional to the card's colored pips and to how concentrated the pool is.
fn off_color_penalty(card: &ProfileCard<'_>, pool: &[ProfileCard<'_>]) -> f64 {
    if card.colors.is_empty() {
        return 0.0;
    }
    let color_lists: Vec<&[String]> = pool.iter().map(|c| c.colors).collect();
    let dominant = dominant_colors(&color_lists, 3);
    if dominant.is_empty() || card.colors.iter().any(|c| dominant.contains(c)) {
        return 0.0;
    }
    let colored: Vec<&ProfileCard<'_>> = pool.iter().filter(|c| !c.colors.is_empty()).collect();
    if colored.is_empty() {
        return 0.0;
    }
    let on_color = colored
        .iter()
        .filter(|c| c.colors.iter().any(|x| dominant.contains(x)))
        .count();
    let concentration = on_color as f64 / colored.len() as f64;
    let pips = card
        .face
        .map_or(1, |f| f.mana_cost.count_colored_pips(None).max(1));
    let pip_scale = (f64::from(pips) / 2.0).min(1.0);
    MAX_OFF_COLOR_PENALTY * concentration * pip_scale
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::ability::{AbilityDefinition, ManaContribution, ManaProduction};
    use engine::types::card_type::CardType;
    use engine::types::mana::ManaColor;

    fn face(core: Vec<CoreType>, subtypes: &[&str]) -> CardFace {
        CardFace {
            card_type: CardType {
                core_types: core,
                subtypes: subtypes.iter().map(|s| s.to_string()).collect(),
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn tap_for(colors: Vec<ManaColor>) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors,
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
    }

    fn card<'a>(
        name: &'a str,
        set: &'a str,
        colors: &'a [String],
        f: &'a CardFace,
    ) -> ProfileCard<'a> {
        ProfileCard {
            name,
            set_code: set,
            colors,
            face: Some(f),
        }
    }

    #[test]
    fn embedded_profile_parses_with_the_requested_ratings() {
        let p = DraftProfile::mirrodin_block();
        assert_eq!(p.card_bonus["skullclamp"], 8.0);
        assert_eq!(p.card_bonus["iron myr"], 6.0);
        assert_eq!(p.sets["MRD"].class_bonus[&CardClass::ArtifactLand], 1.5);
        assert_eq!(p.sets["DST"].class_bonus[&CardClass::Modular], 0.75);
        assert_eq!(p.sets["5DN"].class_bonus[&CardClass::Sunburst], 1.0);
        assert_eq!(p.sets["5DN"].threat_need.as_ref().unwrap().target, 4);
        assert_eq!(p.artifact_density.target_share, 0.45);
        assert_eq!(
            p.adaptive_draft_policy.pack_order,
            vec!["MRD", "DST", "5DN"]
        );
        assert!(DraftProfile::for_set("mrd").is_some());
        assert!(DraftProfile::for_set("NEO").is_none());
    }

    #[test]
    fn stage_weights_follow_pick_number_and_skip_early_after_pack_one() {
        let p = DraftProfile::mirrodin_block();
        let early = p.stage_weights(0);
        assert_eq!(
            (early.raw_power, early.deck_fit, early.color_commitment),
            (1.0, 0.25, 0.0)
        );
        assert_eq!(p.stage_weights(4), early);
        let middle = p.stage_weights(5);
        assert_eq!(
            (middle.raw_power, middle.deck_fit, middle.color_commitment),
            (0.8, 0.7, 0.5)
        );
        assert_eq!(p.stage_weights(8), middle);
        let late = p.stage_weights(9);
        assert_eq!(
            (late.raw_power, late.deck_fit, late.color_commitment),
            (0.6, 1.0, 1.0)
        );
        assert_eq!(p.stage_weights(14), late);
        // Pick 1 of pack 2 behaves like the middle stage, not the early one.
        assert_eq!(p.stage_weights(15), middle);
        assert_eq!(p.stage_weights(30), middle);
        assert_eq!(p.stage_weights(24), late);
    }

    #[test]
    fn classify_recognises_each_role() {
        let mut myr = face(vec![CoreType::Artifact, CoreType::Creature], &["Myr"]);
        myr.abilities = vec![tap_for(vec![ManaColor::Red])];
        assert_eq!(classify(&myr), BTreeSet::from([CardClass::ManaCreature]));

        let mut talisman = face(vec![CoreType::Artifact], &[]);
        talisman.abilities = vec![tap_for(vec![ManaColor::Red])];
        assert_eq!(
            classify(&talisman),
            BTreeSet::from([CardClass::ManaArtifact])
        );

        let mut land = face(vec![CoreType::Artifact, CoreType::Land], &[]);
        land.abilities = vec![tap_for(vec![ManaColor::Red])];
        assert_eq!(classify(&land), BTreeSet::from([CardClass::ArtifactLand]));

        let sword = face(vec![CoreType::Artifact], &["Equipment"]);
        assert_eq!(classify(&sword), BTreeSet::from([CardClass::Equipment]));

        let mut modular = face(vec![CoreType::Artifact, CoreType::Creature], &[]);
        modular.keywords = vec![Keyword::Modular(2)];
        assert_eq!(classify(&modular), BTreeSet::from([CardClass::Modular]));

        let mut sunburst = face(vec![CoreType::Artifact], &[]);
        sunburst.keywords = vec![Keyword::Sunburst];
        assert_eq!(classify(&sunburst), BTreeSet::from([CardClass::Sunburst]));

        assert!(classify(&face(vec![CoreType::Instant], &[])).is_empty());
    }

    #[test]
    fn class_and_card_bonus_are_increments_on_the_base_rating() {
        let p = DraftProfile::mirrodin_block();
        let mut land = face(vec![CoreType::Artifact, CoreType::Land], &[]);
        land.abilities = vec![tap_for(vec![ManaColor::Red])];
        let c = card("Great Furnace", "MRD", &[], &land);
        let s = p.score_pick(&c, &[], 2.0, 0.0).unwrap();
        assert_eq!(s.card_bonus, 6.0);
        assert_eq!(s.class_bonus, 1.5);
        // Pick 1: raw weight 1.0, fit weight 0.25 (only the artifact-density term here).
        let expected = 2.0 + 6.0 + 1.5 + 0.25 * s.artifact_density;
        assert!((s.total - expected).abs() < 1e-9);
        // A different base moves the total one-for-one.
        let s2 = p.score_pick(&c, &[], 3.0, 0.0).unwrap();
        assert!((s2.total - s.total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn class_bonus_takes_the_max_not_the_sum() {
        let p = DraftProfile::mirrodin_block();
        let mut myr = face(vec![CoreType::Artifact, CoreType::Creature], &[]);
        myr.abilities = vec![tap_for(vec![ManaColor::Red])];
        myr.keywords = vec![Keyword::Modular(1)];
        let c = card("Test Myr", "DST", &[], &myr);
        let s = p.score_pick(&c, &[], 0.0, 0.0).unwrap();
        assert_eq!(s.class_bonus, 1.0); // mana_creature 1.0 beats modular 0.75, not 1.75
    }

    #[test]
    fn unknown_set_is_not_scored() {
        let p = DraftProfile::mirrodin_block();
        let f = face(vec![CoreType::Instant], &[]);
        assert!(p
            .score_pick(&card("Bolt", "LEA", &[], &f), &[], 1.0, 0.0)
            .is_none());
    }

    #[test]
    fn artifact_density_bonus_shrinks_as_the_pool_fills_with_artifacts() {
        let p = DraftProfile::mirrodin_block();
        let sword = face(vec![CoreType::Artifact], &["Equipment"]);
        let empty_pool_bonus = p
            .score_pick(&card("Bonesplitter", "MRD", &[], &sword), &[], 0.0, 0.0)
            .unwrap()
            .artifact_density;
        let pool: Vec<ProfileCard<'_>> = (0..6)
            .map(|_| card("Bonesplitter", "MRD", &[], &sword))
            .collect();
        let full_pool_bonus = p
            .score_pick(&card("Bonesplitter", "MRD", &[], &sword), &pool, 0.0, 0.0)
            .unwrap()
            .artifact_density;
        assert!(empty_pool_bonus > 0.0);
        assert_eq!(full_pool_bonus, 0.0);

        // Non-artifacts never earn it.
        let bear = face(vec![CoreType::Creature], &[]);
        let s = p
            .score_pick(&card("Bear", "MRD", &[], &bear), &[], 0.0, 0.0)
            .unwrap();
        assert_eq!(s.artifact_density, 0.0);
    }

    #[test]
    fn filler_artifacts_count_less_than_role_artifacts_toward_density() {
        let p = DraftProfile::mirrodin_block();
        let sword = face(vec![CoreType::Artifact], &["Equipment"]);
        let filler = face(vec![CoreType::Artifact], &[]);
        let bear = face(vec![CoreType::Creature], &[]);
        let mut role_pool = vec![card("Bonesplitter", "MRD", &[], &sword); 2];
        let mut filler_pool = vec![card("Mystery Rock", "MRD", &[], &filler); 2];
        for _ in 0..3 {
            role_pool.push(card("Bear", "MRD", &[], &bear));
            filler_pool.push(card("Bear", "MRD", &[], &bear));
        }
        let probe = card("Bonesplitter", "MRD", &[], &sword);
        let with_roles = p
            .score_pick(&probe, &role_pool, 0.0, 0.0)
            .unwrap()
            .artifact_density;
        let with_filler = p
            .score_pick(&probe, &filler_pool, 0.0, 0.0)
            .unwrap()
            .artifact_density;
        assert!(with_filler > with_roles);
    }

    #[test]
    fn threat_need_only_applies_in_5dn_and_only_to_real_threats() {
        let p = DraftProfile::mirrodin_block();
        let mut flyer = face(vec![CoreType::Creature], &[]);
        flyer.power = Some(PtValue::Fixed(4));
        flyer.toughness = Some(PtValue::Fixed(4));
        flyer.keywords = vec![Keyword::Flying];
        let mut bear = face(vec![CoreType::Creature], &[]);
        bear.power = Some(PtValue::Fixed(2));
        bear.toughness = Some(PtValue::Fixed(2));

        let in_5dn = p
            .score_pick(&card("Flyer", "5DN", &[], &flyer), &[], 0.0, 0.0)
            .unwrap();
        assert_eq!(in_5dn.threat_need, 1.5);
        let in_mrd = p
            .score_pick(&card("Flyer", "MRD", &[], &flyer), &[], 0.0, 0.0)
            .unwrap();
        assert_eq!(in_mrd.threat_need, 0.0);
        let plain = p
            .score_pick(&card("Bear", "5DN", &[], &bear), &[], 0.0, 0.0)
            .unwrap();
        assert_eq!(plain.threat_need, 0.0);

        // Four threats already drafted: the need is met.
        let pool = vec![card("Flyer", "5DN", &[], &flyer); 4];
        let met = p
            .score_pick(&card("Flyer", "5DN", &[], &flyer), &pool, 0.0, 0.0)
            .unwrap();
        assert_eq!(met.threat_need, 0.0);
    }

    #[test]
    fn deck_need_has_diminishing_returns() {
        let p = DraftProfile::mirrodin_block();
        let mut kill = face(vec![CoreType::Instant], &[]);
        kill.abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: engine::types::ability::TargetFilter::Any,
                cant_regenerate: false,
            },
        )];
        let probe = card("Terror", "MRD", &[], &kill);
        let need = |n: usize| {
            let pool = vec![card("Terror", "MRD", &[], &kill); n];
            p.score_pick(&probe, &pool, 0.0, 0.0).unwrap().deck_need
        };
        assert_eq!(need(0), 2.0);
        assert!(need(2) < need(0));
        assert!(need(2) > need(4));
        assert_eq!(need(5), 0.0);
    }

    #[test]
    fn archetype_synergy_requires_demonstrated_theme_and_skips_class_paid_themes() {
        let p = DraftProfile::mirrodin_block();
        let sword = face(vec![CoreType::Artifact], &["Equipment"]);
        let mut talisman = face(vec![CoreType::Artifact], &[]);
        talisman.abilities = vec![tap_for(vec![ManaColor::Red])];

        // Artifact-affinity signal needs distinct quality artifacts; one name repeated is one signal.
        let repeated = vec![card("Bonesplitter", "MRD", &[], &sword); 5];
        let probe = card("Talisman of Impulse", "MRD", &[], &talisman);
        assert_eq!(
            p.score_pick(&probe, &repeated, 0.0, 0.0)
                .unwrap()
                .archetype_synergy,
            0.0
        );

        let names = [
            "Bonesplitter",
            "Cranial Plating",
            "Grafted Wargear",
            "Skullclamp",
        ];
        let pool: Vec<ProfileCard<'_>> =
            names.iter().map(|n| card(n, "MRD", &[], &sword)).collect();
        let s = p.score_pick(&probe, &pool, 0.0, 0.0).unwrap();
        assert!(s.archetype_synergy > 0.0 && s.archetype_synergy <= 2.5);

        // The equipment theme is demonstrated too, but equipment already earns its class bonus.
        let sword_probe = card("Sword", "MRD", &[], &sword);
        let sw = p.score_pick(&sword_probe, &pool, 0.0, 0.0).unwrap();
        // Only the (unlinked) artifact-affinity theme may contribute for equipment.
        assert!(sw.archetype_synergy <= s.archetype_synergy + 1e-9);
        assert_eq!(p.theme_signals(&pool)[&Theme::EquipmentCombat], 4);
    }

    #[test]
    fn off_color_penalty_scales_with_stage_and_pips() {
        let p = DraftProfile::mirrodin_block();
        let red = vec!["R".to_string()];
        let blue = vec!["U".to_string()];
        let red_face = face(vec![CoreType::Creature], &[]);
        let mut blue_face = face(vec![CoreType::Creature], &[]);
        blue_face.mana_cost = engine::types::mana::parse_mtgjson_mana_cost("{1}{U}{U}");

        let mut pool: Vec<ProfileCard<'_>> = (0..4)
            .map(|_| card("Red", "MRD", &red, &red_face))
            .collect();
        let probe = card("Blue", "MRD", &blue, &blue_face);
        // Early (pool of 4): commitment 0 → no penalty.
        assert_eq!(
            p.score_pick(&probe, &pool, 0.0, 0.0)
                .unwrap()
                .off_color_penalty,
            0.0
        );
        // Middle (pool of 5..8).
        pool.push(card("Red", "MRD", &red, &red_face));
        let mid = p
            .score_pick(&probe, &pool, 0.0, 0.0)
            .unwrap()
            .off_color_penalty;
        // Late (pool of 9..14).
        pool.extend((0..4).map(|_| card("Red", "MRD", &red, &red_face)));
        let late = p
            .score_pick(&probe, &pool, 0.0, 0.0)
            .unwrap()
            .off_color_penalty;
        assert!(mid > 0.0 && late > mid);
        // On-color and colorless cards are never penalised.
        let on = card("Red2", "MRD", &red, &red_face);
        assert_eq!(
            p.score_pick(&on, &pool, 0.0, 0.0)
                .unwrap()
                .off_color_penalty,
            0.0
        );
        let none = card("Rock", "MRD", &[], &red_face);
        assert_eq!(
            p.score_pick(&none, &pool, 0.0, 0.0)
                .unwrap()
                .off_color_penalty,
            0.0
        );
    }

    #[test]
    fn early_picks_favour_power_and_late_picks_favour_fit() {
        let p = DraftProfile::mirrodin_block();
        let mut kill = face(vec![CoreType::Instant], &[]);
        kill.abilities = vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: engine::types::ability::TargetFilter::Any,
                cant_regenerate: false,
            },
        )];
        let c = card("Terror", "MRD", &[], &kill);
        let early = p.score_pick(&c, &[], 4.0, 1.0).unwrap();
        let filler = face(vec![CoreType::Creature], &[]);
        let pool = vec![card("Filler", "MRD", &[], &filler); 9];
        let late = p.score_pick(&c, &pool, 4.0, 1.0).unwrap();
        // Same card, same base: power-weighted early total exceeds the late one's raw share.
        assert!(early.total > 0.0 && late.total > 0.0);
        assert!(early.deck_need == late.deck_need || late.deck_need > 0.0);
        let w_early = p.stage_weights(0);
        let w_late = p.stage_weights(9);
        assert!(w_early.raw_power > w_late.raw_power);
        assert!(w_late.deck_fit > w_early.deck_fit);
    }
}

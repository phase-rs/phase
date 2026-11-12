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
//! identity classes (an artifact-creature mana source is not rewarded twice;
//! only an additional way to use the card, such as Channel, stacks on it), and a
//! theme whose mechanic already earned the card a class bonus (Modular,
//! Sunburst, Equipment, Spirit, Arcane, ...) contributes only the synergy in
//! excess of that bonus.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::OnceLock;

use nom::branch::alt;
use nom::bytes::complete::tag;
use nom::combinator::value;
use nom::sequence::pair;
use nom::{IResult, Parser};
use serde::{Deserialize, Serialize};

use engine::game::effects::effect::generic_effect_application_filter;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, ControllerRef, Effect,
    FilterProp, PtValue, StaticDefinition, TargetFilter, TypeFilter,
};
use engine::types::card::CardFace;
use engine::types::card_type::CoreType;
use engine::types::keywords::Keyword;
use engine::types::statics::StaticMode;
use engine::types::zones::Zone;

use crate::ability_chain::collect_chain_effects;
use crate::cast_facts::{collect_face_effects, is_direct_removal, EffectProfile};
use crate::draft_eval::{dominant_colors, produced_color_count};
use crate::features::mana_ramp::{chain_has_mana_effect, chain_searches_for_land};
use crate::policies::effect_classify::{pt_value_is_non_negative, quantity_is_non_negative};

/// Where in the draft a pick happens: which booster, and which pick within it.
///
/// Supplied by the draft session (which records each booster's real size) rather
/// than inferred from pool size, so packs of any configured size select the
/// right adaptive stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PackPosition {
    /// Zero-based booster index.
    pub pack_number: usize,
    /// One-based pick within the current booster.
    pub pick_in_pack: usize,
}

/// Minimum `card_bonus` for a card to count as a "high-quality" theme signal.
const SIGNAL_QUALITY_THRESHOLD: f64 = 1.0;
/// `card_bonus` at which a creature is a premium finisher for threat counting.
const PREMIUM_THREAT_THRESHOLD: f64 = 2.0;
/// Minimum power for an evasive creature to count as a threat.
const EVASIVE_THREAT_POWER: i32 = 3;
/// Highest mana value at which an evasive creature is a cheap Ninjutsu enabler.
const NINJUTSU_ENABLER_MAX_MANA_VALUE: u32 = 3;
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

/// Role a card plays for its set's class bonuses, derived from parsed card data
/// (never from a name), so it covers every card in the class.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CardClass {
    /// A land that is also an artifact (CR 205.2b: an object with more than one
    /// card type satisfies any effect that applies to any of them).
    ArtifactLand,
    /// A creature with an activated mana ability (CR 605.1a), anywhere in the
    /// ability's chain: Myr and other mana dorks.
    ManaCreature,
    /// A non-creature, non-land artifact with an activated mana ability
    /// (CR 605.1a): Talismans and other mana rocks.
    ManaArtifact,
    /// A permanent with the Equipment subtype (CR 301.5).
    Equipment,
    /// Removal-shaped effect (see [`is_removal`]): what `EffectProfile`
    /// classifies as removal (which also matches mass removal, `Counter` and
    /// `DiscardCard`), plus toughness reductions.
    Removal,
    /// Has the Modular keyword (CR 702.43).
    Modular,
    /// Has the Sunburst keyword (CR 702.44).
    Sunburst,
    /// A Spirit creature (CR 205.3m: Spirit is a creature type).
    Spirit,
    /// An instant or sorcery with the Arcane subtype (CR 205.3k).
    ArcaneSpell,
    /// Has the Soulshift keyword (CR 702.46).
    Soulshift,
    /// Has the Splice keyword (CR 702.47).
    Splice,
    /// A Ninja creature (CR 205.3m).
    Ninja,
    /// A creature that is evasive by its own abilities (see [`is_evasive`]).
    EvasiveThreat,
    /// A reliable way to connect, so a Ninjutsu activation (CR 702.49a: return
    /// an unblocked attacker) is available: a cheap evasive creature, or an
    /// effect that makes a creature evasive (see [`grants_evasion`]).
    NinjutsuEnabler,
    /// A card with a Channel ability (CR 207.2c ability word): activated from
    /// hand by discarding the card itself (see [`is_channel_ability`]).
    Channel,
    /// An instant whose effect chain raises a creature's power or toughness
    /// (see [`has_buff_effect`]), and that is not removal.
    CombatTrick,
    /// Makes mana of two or more colors, or searches the library for a land
    /// (see [`is_mana_fixing`]).
    ManaFixing,
}

impl CardClass {
    /// Whether the class is another way to use the card rather than another
    /// description of what it is when cast. A Channel card is discarded from
    /// hand for its Channel effect or cast as its printed self (CR 207.2c:
    /// Channel is an ability word), so its Channel bonus is not a duplicate of
    /// its Spirit or Removal bonus and stacks on top of it.
    fn is_additional_mode(self) -> bool {
        match self {
            CardClass::Channel => true,
            CardClass::ArtifactLand
            | CardClass::ManaCreature
            | CardClass::ManaArtifact
            | CardClass::Equipment
            | CardClass::Removal
            | CardClass::Modular
            | CardClass::Sunburst
            | CardClass::Spirit
            | CardClass::ArcaneSpell
            | CardClass::Soulshift
            | CardClass::Splice
            | CardClass::Ninja
            | CardClass::EvasiveThreat
            | CardClass::NinjutsuEnabler
            | CardClass::CombatTrick
            | CardClass::ManaFixing => false,
        }
    }
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
    RedGreenSpiritsArcane,
    WhiteBlackSpiritsSoulshift,
    BlueBlackNinjasTempo,
    BlueWhiteEvasionTempo,
    RedBlackAggression,
    GreenMidrangeSpirits,
    ArcaneSpliceControl,
    FiveColorGoodstuff,
}

impl Theme {
    /// The classes whose set bonus already rewards this theme's signature
    /// mechanics. A card paid by one of them gets only the theme's synergy in
    /// excess of that class bonus.
    fn linked_classes(self) -> &'static [CardClass] {
        match self {
            Theme::SunburstFiveColor => &[CardClass::Sunburst],
            Theme::ModularCounters => &[CardClass::Modular],
            Theme::EquipmentCombat => &[CardClass::Equipment],
            Theme::RedGreenSpiritsArcane => &[CardClass::Spirit, CardClass::ArcaneSpell],
            Theme::WhiteBlackSpiritsSoulshift => &[CardClass::Spirit, CardClass::Soulshift],
            Theme::BlueBlackNinjasTempo => &[CardClass::Ninja, CardClass::NinjutsuEnabler],
            Theme::BlueWhiteEvasionTempo => &[CardClass::EvasiveThreat],
            Theme::GreenMidrangeSpirits => &[CardClass::Spirit],
            Theme::ArcaneSpliceControl => &[CardClass::ArcaneSpell, CardClass::Splice],
            Theme::RedBlackAggression => &[CardClass::Removal, CardClass::EvasiveThreat],
            Theme::FiveColorGoodstuff => &[CardClass::ManaFixing],
            Theme::ArtifactAffinity
            | Theme::BlueControl
            | Theme::RedAggression
            | Theme::BlackAttrition
            | Theme::GreenMidrange => &[],
        }
    }

    /// The single color of a mono-color theme.
    fn color(self) -> Option<&'static str> {
        match self {
            Theme::BlueControl => Some("U"),
            Theme::RedAggression => Some("R"),
            Theme::BlackAttrition => Some("B"),
            Theme::GreenMidrange => Some("G"),
            Theme::ArtifactAffinity
            | Theme::SunburstFiveColor
            | Theme::ModularCounters
            | Theme::EquipmentCombat
            | Theme::RedGreenSpiritsArcane
            | Theme::WhiteBlackSpiritsSoulshift
            | Theme::BlueBlackNinjasTempo
            | Theme::BlueWhiteEvasionTempo
            | Theme::RedBlackAggression
            | Theme::GreenMidrangeSpirits
            | Theme::ArcaneSpliceControl
            | Theme::FiveColorGoodstuff => None,
        }
    }

    /// The colors a two-color theme lives in; a member card has at least one.
    fn color_pair(self) -> Option<[&'static str; 2]> {
        match self {
            Theme::RedGreenSpiritsArcane => Some(["R", "G"]),
            Theme::WhiteBlackSpiritsSoulshift => Some(["W", "B"]),
            Theme::BlueBlackNinjasTempo => Some(["U", "B"]),
            Theme::BlueWhiteEvasionTempo => Some(["U", "W"]),
            Theme::RedBlackAggression => Some(["R", "B"]),
            Theme::GreenMidrangeSpirits => Some(["G", "G"]),
            Theme::ArtifactAffinity
            | Theme::SunburstFiveColor
            | Theme::ModularCounters
            | Theme::EquipmentCombat
            | Theme::BlueControl
            | Theme::RedAggression
            | Theme::BlackAttrition
            | Theme::GreenMidrange
            | Theme::ArcaneSpliceControl
            | Theme::FiveColorGoodstuff => None,
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

/// How a set's role predicates (removal, evasion, fixing) read a card's
/// printed abilities.
///
/// The reading is a set-level fact because each profile's class bonuses,
/// role targets and card bonuses were tuned against one reading: switching a
/// set to another reading re-scores every card in it. A profile opts in per
/// set, and a set that does not keeps the reading its scores were tuned on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleReading {
    /// The reading the Mirrodin-block scores are tuned against. Removal is the
    /// shared direct-removal classifier, plus mass damage or a mass -N/-N for
    /// the deck role; fixing is mana of two or more colors; evasion is a
    /// creature's printed flying, fear, intimidate, shadow or horsemanship.
    #[default]
    Classic,
    /// Every printed removal, evasion and fixing shape:
    /// - removal also counts a toughness reduction on another creature
    ///   (CR 704.5f) from an effect or a static, and a static that stops
    ///   another creature from attacking or blocking, but only an effect that
    ///   can reach an opposing creature: not a bounce of the source or of
    ///   your own creature, nor damage only to a player or planeswalker;
    /// - evasion also counts menace, skulk and landwalk (CR 702.14b), CR
    ///   509.1b blocker restrictions, effects that make the source itself
    ///   evasive, and effects that make another creature evasive;
    /// - fixing also counts a search of its controller's library for a land.
    Full,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SetProfile {
    pub class_bonus: BTreeMap<CardClass, f64>,
    #[serde(default)]
    pub threat_need: Option<ThreatNeed>,
    /// How this set's role predicates read a card (see [`RoleReading`]).
    #[serde(default)]
    pub role_reading: RoleReading,
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
    /// Only formats with an artifact-density target carry an artifact policy.
    #[serde(default)]
    pub artifact_policy: Option<String>,
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

/// How a demonstrated theme's synergy combines with a set class bonus that
/// already rewards one of the theme's linked mechanics (see
/// [`Theme::linked_classes`]). Either way the flat class bonus is not paid
/// twice; the policies differ in whether a demonstrated pool still raises it.
///
/// Like [`RoleReading`], this is a profile-level fact because the profile's
/// class bonuses were tuned against one policy: switching a profile
/// re-scores every card whose classes overlap a theme.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SynergyOverlap {
    /// The policy the Mirrodin-block scores are tuned against: a card with a
    /// linked class its set rewards earns no synergy from that theme; the
    /// class bonus stands in for it.
    #[default]
    Suppress,
    /// The theme pays only its pool-dependent strength in excess of the
    /// class bonus the card was actually awarded through a linked class. A
    /// linked class that lost to a higher, unlinked class bonus was never
    /// paid, so it discounts nothing.
    Excess,
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
    /// How theme synergy combines with a linked class bonus (see
    /// [`SynergyOverlap`]).
    #[serde(default)]
    pub synergy_overlap: SynergyOverlap,
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

/// The set class bonus a card is paid.
///
/// A card's identity classes describe what the card is when cast; they can
/// overlap (an artifact-creature mana source), so only the highest of them is
/// paid, and every class that ties it is recorded. An additional-mode class
/// (see [`CardClass::is_additional_mode`]) describes another way to use the
/// card, so its bonus is paid on top of the identity award.
#[derive(Debug, Clone, Default)]
struct ClassAward {
    identity: f64,
    additional: f64,
    classes: BTreeSet<CardClass>,
}

impl ClassAward {
    fn new(set: &SetProfile, classes: &BTreeSet<CardClass>) -> Self {
        let bonus_of = |c: &CardClass| set.class_bonus.get(c).copied();
        let (additional, identity_classes): (Vec<CardClass>, Vec<CardClass>) = classes
            .iter()
            .copied()
            .partition(|c| c.is_additional_mode());
        let identity = identity_classes
            .iter()
            .filter_map(bonus_of)
            .fold(0.0, f64::max);
        let classes = identity_classes
            .into_iter()
            .filter(|c| identity > 0.0 && bonus_of(c) == Some(identity))
            .collect();
        Self {
            identity,
            additional: additional.iter().filter_map(bonus_of).sum(),
            classes,
        }
    }

    /// The total class bonus.
    fn bonus(&self) -> f64 {
        self.identity + self.additional
    }

    /// The identity bonus already paid through one of `linked` classes, or 0
    /// when it came from an unlinked class. A linked class that lost to a
    /// higher bonus was never paid, so it discounts nothing.
    fn paid_through(&self, linked: &[CardClass]) -> f64 {
        if linked.iter().any(|c| self.classes.contains(c)) {
            self.identity
        } else {
            0.0
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DraftProfile {
    pub id: String,
    pub name: String,
    /// Present only for formats whose guide sets an artifact-density target.
    #[serde(default)]
    pub artifact_density: Option<ArtifactDensity>,
    pub sets: BTreeMap<String, SetProfile>,
    /// Lowercase card name → incremental bonus.
    pub card_bonus: BTreeMap<String, f64>,
    pub scoring_model: ScoringModel,
    pub adaptive_draft_policy: AdaptivePolicy,
}

static MIRRODIN_BLOCK: OnceLock<DraftProfile> = OnceLock::new();
static KAMIGAWA_BLOCK: OnceLock<DraftProfile> = OnceLock::new();

impl DraftProfile {
    /// The Mirrodin / Darksteel / Fifth Dawn profile.
    pub fn mirrodin_block() -> &'static DraftProfile {
        MIRRODIN_BLOCK.get_or_init(|| {
            Self::from_json(include_str!("../data/mirrodin-block.json"))
                .expect("embedded mirrodin-block draft profile is valid")
        })
    }

    /// The Champions / Betrayers / Saviors of Kamigawa profile.
    pub fn kamigawa_block() -> &'static DraftProfile {
        KAMIGAWA_BLOCK.get_or_init(|| {
            Self::from_json(include_str!("../data/kamigawa-block.json"))
                .expect("embedded kamigawa-block draft profile is valid")
        })
    }

    /// Parse a profile from its JSON definition.
    pub fn from_json(json: &str) -> Result<Self, serde_json::Error> {
        serde_json::from_str(json)
    }

    /// The embedded profile covering `set_code`, if any.
    pub fn for_set(set_code: &str) -> Option<&'static DraftProfile> {
        [Self::mirrodin_block(), Self::kamigawa_block()]
            .into_iter()
            .find(|profile| profile.covers_set(set_code))
    }

    /// Whether this profile has a set entry for `set_code` (case-insensitive).
    pub fn covers_set(&self, set_code: &str) -> bool {
        self.set_profile(set_code).is_some()
    }

    /// The per-set class bonuses and threat need for `set_code`.
    fn set_profile(&self, set_code: &str) -> Option<&SetProfile> {
        self.sets.get(&set_code.to_ascii_uppercase())
    }

    /// The name-keyed `card_bonus` for a card, or 0 when none is configured.
    fn bonus_for_name(&self, name: &str) -> f64 {
        self.card_bonus
            .get(&name.to_lowercase())
            .copied()
            .unwrap_or(0.0)
    }

    /// Policy weights for the pick at `position`.
    pub fn stage_weights(&self, position: PackPosition) -> StageWeights {
        let policy = &self.adaptive_draft_policy;
        let stages = &policy.draft_stages;
        let commit = &policy.color_commitment;
        let pick_in_pack = position.pick_in_pack;
        let later_pack = position.pack_number > 0;

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

    /// Score `card` for the pick at `position` that follows `pool`, or `None` when the card's
    /// set is not covered by this profile (the caller keeps its generic scoring).
    ///
    /// `base` is the caller's context-free card rating and `curve` its mana-curve
    /// bonus; both are incremented, never replaced.
    pub fn score_pick(
        &self,
        card: &ProfileCard<'_>,
        pool: &[ProfileCard<'_>],
        position: PackPosition,
        base: f64,
        curve: f64,
    ) -> Option<ProfileScore> {
        let set = self.set_profile(card.set_code)?;
        let weights = self.stage_weights(position);
        let policy = &self.adaptive_draft_policy;

        let classes = card
            .face
            .map(|f| classify_with(f, set.role_reading))
            .unwrap_or_default();
        let card_bonus = self.bonus_for_name(card.name);
        let award = ClassAward::new(set, &classes);

        let artifact_density = self.artifact_density_bonus(card, pool);
        let threat_need = set
            .threat_need
            .as_ref()
            .map_or(0.0, |need| self.threat_need_bonus(need, card, pool));
        let deck_need = if policy.deck_need_bonus.enabled {
            self.deck_need_bonus(card, pool, policy.deck_need_bonus.maximum_per_card)
        } else {
            0.0
        };
        let archetype_synergy = if policy.archetype_synergy_bonus.enabled {
            self.archetype_synergy_bonus(card, pool, set, &classes, &award)
        } else {
            0.0
        };
        let off_color_penalty = off_color_penalty(card, pool) * weights.color_commitment;

        let class_bonus = award.bonus();
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
    /// Each face is classified once and reused for every theme.
    pub fn theme_signals(&self, pool: &[ProfileCard<'_>]) -> BTreeMap<Theme, u32> {
        let themes = &self
            .adaptive_draft_policy
            .archetype_detection
            .supported_themes;
        let mut seen: BTreeMap<Theme, BTreeSet<String>> = BTreeMap::new();
        for card in pool {
            let classes = self.classify_card(card);
            for &theme in themes {
                if self.is_theme_signal(theme, card, &classes)
                    && self.is_theme_member(theme, card, &classes)
                {
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

    /// Whether a card is a quality signal for `theme`.
    ///
    /// The name-keyed `card_bonus` list is the general gate, but it is
    /// deliberately selective. Fixing is the one theme whose membership is
    /// itself a quality judgement: a card that produces two colors or searches
    /// out a land is real fixing even when the list does not name it, so
    /// structural fixing qualifies without a named bonus. Without that, a
    /// block whose fixers are modest (Orochi Leafcaller, Tendo Ice Bridge)
    /// could never demonstrate the five-color theme the policy advertises.
    fn is_theme_signal(
        &self,
        theme: Theme,
        card: &ProfileCard<'_>,
        classes: &BTreeSet<CardClass>,
    ) -> bool {
        self.bonus_for_name(card.name) >= SIGNAL_QUALITY_THRESHOLD
            || (theme == Theme::FiveColorGoodstuff && classes.contains(&CardClass::ManaFixing))
    }

    fn is_theme_member(
        &self,
        theme: Theme,
        card: &ProfileCard<'_>,
        classes: &BTreeSet<CardClass>,
    ) -> bool {
        if let Some(color) = theme.color() {
            return card.colors.iter().any(|c| c == color);
        }
        let Some(face) = card.face else {
            return false;
        };
        let in_pair = theme
            .color_pair()
            .is_some_and(|pair| card.colors.iter().any(|c| pair.contains(&c.as_str())));
        match theme {
            Theme::RedGreenSpiritsArcane => {
                in_pair
                    && (classes.contains(&CardClass::Spirit)
                        || classes.contains(&CardClass::ArcaneSpell))
            }
            Theme::WhiteBlackSpiritsSoulshift => {
                in_pair
                    && (classes.contains(&CardClass::Spirit)
                        || classes.contains(&CardClass::Soulshift))
            }
            Theme::BlueBlackNinjasTempo => {
                in_pair
                    && (classes.contains(&CardClass::Ninja)
                        || classes.contains(&CardClass::NinjutsuEnabler))
            }
            Theme::BlueWhiteEvasionTempo => in_pair && classes.contains(&CardClass::EvasiveThreat),
            Theme::RedBlackAggression => {
                in_pair
                    && (classes.contains(&CardClass::Removal)
                        || classes.contains(&CardClass::EvasiveThreat))
            }
            Theme::GreenMidrangeSpirits => in_pair && classes.contains(&CardClass::Spirit),
            Theme::ArcaneSpliceControl => {
                classes.contains(&CardClass::ArcaneSpell) || classes.contains(&CardClass::Splice)
            }
            Theme::FiveColorGoodstuff => classes.contains(&CardClass::ManaFixing),
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
    /// saturates at twice that. A theme whose linked mechanic the card's
    /// class bonus already rewards is combined with that bonus under the
    /// profile's [`SynergyOverlap`] policy.
    fn archetype_synergy_bonus(
        &self,
        card: &ProfileCard<'_>,
        pool: &[ProfileCard<'_>],
        set: &SetProfile,
        classes: &BTreeSet<CardClass>,
        award: &ClassAward,
    ) -> f64 {
        let policy = &self.adaptive_draft_policy;
        let minimum = policy.archetype_detection.minimum_signals.max(1);
        let cap = policy.archetype_synergy_bonus.maximum_per_card;
        self.theme_signals(pool)
            .into_iter()
            .filter(|&(_, signals)| signals >= minimum)
            .filter(|&(theme, _)| self.is_theme_member(theme, card, classes))
            .filter_map(|(theme, signals)| {
                let strength = cap * (f64::from(signals) / f64::from(2 * minimum)).min(1.0);
                let linked = theme.linked_classes();
                match policy.synergy_overlap {
                    SynergyOverlap::Suppress => linked
                        .iter()
                        .all(|c| !(classes.contains(c) && set.class_bonus.contains_key(c)))
                        .then_some(strength),
                    SynergyOverlap::Excess => {
                        Some((strength - award.paid_through(linked)).max(0.0))
                    }
                }
            })
            .fold(0.0, f64::max)
    }

    /// Whether any of this profile's sets awards a class bonus for `class`.
    fn rewards_class(&self, class: CardClass) -> bool {
        self.sets
            .values()
            .any(|set| set.class_bonus.contains_key(&class))
    }

    /// How much an artifact counts toward the density target: artifacts with a
    /// role this profile rewards, and high-bonus cards, count fully; filler
    /// artifacts count partially. Classes only another profile rewards (a
    /// Kamigawa evasive threat, say) do not make a Mirrodin artifact a role.
    fn artifact_weight(&self, card: &ProfileCard<'_>) -> f64 {
        let has_role = self
            .classify_card(card)
            .into_iter()
            .any(|c| self.rewards_class(c));
        if has_role || self.bonus_for_name(card.name) >= SIGNAL_QUALITY_THRESHOLD {
            1.0
        } else {
            FILLER_ARTIFACT_WEIGHT
        }
    }

    /// Bonus for an artifact while the pool is below the artifact-density target.
    fn artifact_density_bonus(&self, card: &ProfileCard<'_>, pool: &[ProfileCard<'_>]) -> f64 {
        if !card.face.is_some_and(is_artifact) {
            return 0.0;
        }
        let Some(density) = &self.artifact_density else {
            return 0.0;
        };
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

    /// Bonus for a threat while the pool holds fewer threats than the set's target.
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

    /// Premium finisher or evasive threat, not merely a creature. A creature
    /// whose role is mana (a mana ability, multicolor mana, or a land search
    /// such as Sakura-Tribe Elder's) is never a finisher, however highly its
    /// pick quality is rated: the card bonus measures acceleration and fixing,
    /// not closing a game.
    fn is_threat(&self, card: &ProfileCard<'_>) -> bool {
        let Some(face) = card.face.filter(|f| is_creature(f)) else {
            return false;
        };
        let reading = self.reading(card.set_code);
        let classes = classify_with(face, reading);
        if classes.contains(&CardClass::ManaCreature) || classes.contains(&CardClass::ManaFixing) {
            return false;
        }
        let power = match &face.power {
            Some(PtValue::Fixed(p)) => *p,
            _ => 0,
        };
        self.bonus_for_name(card.name) >= PREMIUM_THREAT_THRESHOLD
            || (is_evasive(face, reading) && power >= EVASIVE_THREAT_POWER)
    }

    /// The role reading of `set_code`. A card from a set this profile does
    /// not cover keeps the default reading, so it is never silently re-scored.
    fn reading(&self, set_code: &str) -> RoleReading {
        self.set_profile(set_code)
            .map_or_else(RoleReading::default, |set| set.role_reading)
    }

    /// Classify a card under its own set's role reading.
    fn classify_card(&self, card: &ProfileCard<'_>) -> BTreeSet<CardClass> {
        let Some(face) = card.face else {
            return BTreeSet::new();
        };
        classify_with(face, self.reading(card.set_code))
    }

    /// The deck roles a card fills under its own set's role reading.
    fn roles_for(&self, card: &ProfileCard<'_>) -> Vec<DeckRole> {
        let Some(face) = card.face else {
            return Vec::new();
        };
        roles_with(face, self.reading(card.set_code))
    }

    /// Diminishing-returns need: the best role the card fills, linearly spent
    /// as the pool approaches that role's target and zero once it is met. Each
    /// face is read under its own set's role reading.
    fn deck_need_bonus(&self, card: &ProfileCard<'_>, pool: &[ProfileCard<'_>], cap: f64) -> f64 {
        let card_roles = self.roles_for(card);
        if card_roles.is_empty() {
            return 0.0;
        }
        let mut have: BTreeMap<usize, f64> = BTreeMap::new();
        for role in pool.iter().flat_map(|c| self.roles_for(c)) {
            *have.entry(role as usize).or_insert(0.0) += 1.0;
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
}

/// Whether the face has the artifact card type (CR 301).
fn is_artifact(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Artifact)
}

/// Whether the face has the creature card type (CR 302).
fn is_creature(face: &CardFace) -> bool {
    face.card_type.core_types.contains(&CoreType::Creature)
}

/// Whether the face has the Equipment artifact subtype (CR 301.5).
fn is_equipment(face: &CardFace) -> bool {
    face.card_type.subtypes.iter().any(|s| s == "Equipment")
}

/// Whether `keyword` is an evasion ability under `reading`: CR 702.9a Flying,
/// CR 702.36a Fear, CR 702.13a Intimidate, CR 702.28a Shadow and CR 702.31a
/// Horsemanship in either reading; the full reading adds CR 702.111a Menace,
/// CR 702.118a Skulk and CR 702.14b Landwalk. Each restricts which creatures
/// can block (CR 509.1b). Trample changes damage assignment, not blocking.
fn is_evasion_keyword(keyword: &Keyword, reading: RoleReading) -> bool {
    matches!(
        keyword,
        Keyword::Flying
            | Keyword::Fear
            | Keyword::Intimidate
            | Keyword::Shadow
            | Keyword::Horsemanship
    ) || (reading == RoleReading::Full
        && matches!(
            keyword,
            Keyword::Menace | Keyword::Skulk | Keyword::Landwalk(_)
        ))
}

/// CR 509.1b: `mode` restricts which creatures may block the affected
/// creature — "can't be blocked", "can't be blocked by X", and "can't be
/// blocked except by X" are all blocker restrictions on the attacker.
fn is_block_restriction(mode: &StaticMode) -> bool {
    matches!(
        mode,
        StaticMode::CantBeBlocked
            | StaticMode::CantBeBlockedBy { .. }
            | StaticMode::CantBeBlockedExceptBy { .. }
    )
}

/// Whether a continuous modification makes the affected creature evasive:
/// an evasion keyword, or a CR 509.1b blocker restriction.
fn is_evasion_modification(modification: &ContinuousModification) -> bool {
    match modification {
        ContinuousModification::AddKeyword { keyword } => {
            is_evasion_keyword(keyword, RoleReading::Full)
        }
        ContinuousModification::AddStaticMode { mode } => is_block_restriction(mode),
        // Exhaustive: every other modification changes a characteristic,
        // grants an ability, or rewrites text — none restricts blocking.
        ContinuousModification::CopyValues { .. }
        | ContinuousModification::CopyChosen
        | ContinuousModification::SetName { .. }
        | ContinuousModification::SetTextName { .. }
        | ContinuousModification::SubstituteTextWord { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAbility { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::GrantTrigger { .. }
        | ContinuousModification::GrantReplacement { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::SetDynamicPower { .. }
        | ContinuousModification::SetDynamicToughness { .. }
        | ContinuousModification::SetPowerDynamic { .. }
        | ContinuousModification::SetToughnessDynamic { .. }
        | ContinuousModification::AddDynamicPower { .. }
        | ContinuousModification::AddDynamicToughness { .. }
        | ContinuousModification::AddDynamicKeyword { .. }
        | ContinuousModification::AddKeywordWithDerivedCost { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor { .. }
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::GrantStaticAbility { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::SetChosenName
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::RetainAllOtherAbilitiesFromSource
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::AddCounterOnEnter { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => false,
    }
}

/// Whether a static definition applies to its own source: an absent
/// recipient, or the source named by itself.
fn static_affects_self(def: &StaticDefinition) -> bool {
    matches!(def.affected, None | Some(TargetFilter::SelfRef))
}

/// Whether a static's recipient becomes evasive: a CR 509.1b blocker
/// restriction, or a modification that adds one (see
/// [`is_evasion_modification`]).
fn static_makes_evasive(def: &StaticDefinition) -> bool {
    is_block_restriction(&def.mode) || def.modifications.iter().any(is_evasion_modification)
}

/// Whether a static definition makes a creature other than its source
/// evasive — the enabler shape ("enchanted creature can't be blocked by
/// Spirits", "creatures you control have flying").
fn static_grants_evasion(def: &StaticDefinition) -> bool {
    !static_affects_self(def) && static_makes_evasive(def)
}

/// Whether the face is evasive by its own abilities under `reading`: a
/// printed evasion keyword, or (full reading) a CR 509.1b blocker restriction
/// scoped to itself, or an effect that makes the source itself evasive (Kami
/// of Tattered Shoji: "Whenever you cast a Spirit or Arcane spell, this
/// creature gains flying until end of turn").
fn is_evasive(face: &CardFace, reading: RoleReading) -> bool {
    face.keywords.iter().any(|k| is_evasion_keyword(k, reading))
        || (reading == RoleReading::Full
            && (face
                .static_abilities
                .iter()
                .any(|s| static_affects_self(s) && static_makes_evasive(s))
                || collect_face_effects(face)
                    .into_iter()
                    .any(|e| effect_makes_evasive(e, EvasionRecipient::Source))))
}

/// Who an effect makes evasive (CR 509.1b).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvasionRecipient {
    /// The effect's own source ("this creature gains flying").
    Source,
    /// A creature other than the source ("target creature gains flying").
    Other,
}

/// Whether `effect` makes `recipient` evasive. CR 608.2c: where a
/// `GenericEffect`'s granted static lands is decided by
/// `generic_effect_application_filter`, the engine's authority for it — not
/// the outer target slot alone, which is empty on "this creature gains
/// flying" (Kami of Tattered Shoji). A grant with no recipient identifies no
/// attacker at all.
fn effect_makes_evasive(effect: &Effect, recipient: EvasionRecipient) -> bool {
    let Effect::GenericEffect {
        static_abilities,
        target,
        ..
    } = effect
    else {
        return false;
    };
    static_abilities.iter().any(|s| {
        let lands_on = match generic_effect_application_filter(target.as_ref(), s.affected.as_ref())
        {
            None => return false,
            Some(TargetFilter::SelfRef) => EvasionRecipient::Source,
            Some(_) => EvasionRecipient::Other,
        };
        lands_on == recipient && static_makes_evasive(s)
    })
}

/// Whether a spell, activated ability, trigger, or static on the face makes
/// another creature evasive: an attacker that connects is what a Ninjutsu
/// activation needs (CR 702.49a). Triggered abilities count — Guardian of
/// Solitude's "whenever you cast a Spirit or Arcane spell, target creature
/// gains flying" is exactly the enabler a Ninja deck wants. Only the full
/// reading counts enablers; the classic reading's evasion is a creature's own.
fn grants_evasion(face: &CardFace, reading: RoleReading) -> bool {
    reading == RoleReading::Full
        && (collect_face_effects(face)
            .into_iter()
            .any(|e| effect_makes_evasive(e, EvasionRecipient::Other))
            || face.static_abilities.iter().any(static_grants_evasion))
}

/// Whether a power/toughness value lowers the stat. The parser carries the
/// sign inside the value in two encodings the AI must read alike: a negative
/// `Fixed`, and a signed variable (`Variable("-X")`) or a negative-multiplier
/// `Quantity` — see [`crate::policies::effect_classify`].
fn is_negative(value: &PtValue) -> bool {
    !pt_value_is_non_negative(value)
}

/// Whether a power/toughness value is a fixed zero (no change).
fn is_zero(value: &PtValue) -> bool {
    matches!(value, PtValue::Fixed(0))
}

/// A toughness reduction that can land on an opposing creature: CR 704.5f
/// puts a creature with toughness 0 or less into its owner's graveyard, so
/// "-5/-5" is removal.
fn is_shrink_removal(effect: &Effect) -> bool {
    matches!(
        effect,
        Effect::Pump { toughness, target, .. } | Effect::PumpAll { toughness, target, .. }
            if is_negative(toughness) && may_be_opposing_creature(target)
    )
}

/// Whether a removal-shaped effect can reach an opposing creature on the
/// battlefield. A bounce of the source or of your own creature (CR 400.3:
/// it goes to its owner's hand) removes nothing from an opponent, and damage
/// only to a player (CR 120.3a: it causes life loss) or planeswalker touches
/// no creature. Counter and discard have no battlefield recipient, so they
/// keep the shared classifier's answer.
fn reaches_opposing_creature(effect: &Effect) -> bool {
    match effect {
        Effect::Destroy { target, .. }
        | Effect::DealDamage { target, .. }
        | Effect::Bounce { target, .. }
        | Effect::Fight { target, .. }
        | Effect::DestroyAll { target, .. }
        | Effect::DamageAll { target, .. } => may_be_opposing_creature(target),
        // An exile from the battlefield (CR 701.13a) or a graveyard move
        // from the battlefield (CR 704.5f: creature with 0 toughness) removes
        // an opposing creature. A move from another zone (library, graveyard,
        // hand) — e.g., Gifts Ungiven's "put the chosen cards into your
        // graveyard" — takes nothing off the battlefield and is not removal.
        // Only `origin == Some(Zone::Battlefield)` qualifies; `origin == None`
        // is ambiguous: if the target filter can only match battlefield
        // permanents (e.g., typed creature filter), we infer battlefield
        // origin; otherwise we conservatively return false.
        Effect::ChangeZone { origin, target, .. }
        | Effect::ChangeZoneAll { origin, target, .. } => {
            let origin_is_battlefield = match origin {
                Some(Zone::Battlefield) => true,
                None => target_filter_implies_battlefield(target),
                Some(_) => false,
            };
            origin_is_battlefield && may_be_opposing_creature(target)
        }
        _ => true,
    }
}

/// Whether `target` is a filter that can only match battlefield permanents.
/// Used to infer battlefield origin when `ChangeZone.origin` is `None`.
/// Returns true for typed creature/permanent filters with no non-battlefield
/// zone restriction; false for player, zone-specific, or unbound filters.
fn target_filter_implies_battlefield(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => {
            // A typed filter with a creature/permanent type filter and no
            // zone restriction implies battlefield (CR 109.2: objects on
            // the battlefield are permanents).
            // Exclude filters with an explicit InZone restriction to a
            // non-battlefield zone (e.g., "from a graveyard").
            if typed.properties.iter().any(
                |p| matches!(p, FilterProp::InZone { zone } if !matches!(zone, Zone::Battlefield)),
            ) {
                return false;
            }
            typed.type_filters.iter().any(type_admits_creature)
        }
        TargetFilter::Or { filters } => filters.iter().all(target_filter_implies_battlefield),
        TargetFilter::And { filters } => filters.iter().any(target_filter_implies_battlefield),
        // All other filters (Player, None, ParentTarget, etc.) don't imply
        // battlefield — they could match objects in other zones.
        _ => false,
    }
}

/// Whether a type-line constraint admits a creature. A constraint naming only
/// another card type (artifact, enchantment, land, planeswalker) is aimed at
/// that type, not at creatures, even where a permanent can carry both.
fn type_admits_creature(filter: &TypeFilter) -> bool {
    match filter {
        TypeFilter::Creature
        | TypeFilter::Permanent
        | TypeFilter::Card
        | TypeFilter::Any
        | TypeFilter::Subtype(_) => true,
        TypeFilter::Non(inner) => !matches!(**inner, TypeFilter::Creature | TypeFilter::Permanent),
        TypeFilter::AnyOf(filters) => filters.iter().any(type_admits_creature),
        TypeFilter::Land
        | TypeFilter::Artifact
        | TypeFilter::Enchantment
        | TypeFilter::Instant
        | TypeFilter::Sorcery
        | TypeFilter::Planeswalker
        | TypeFilter::Battle
        | TypeFilter::Kindred => false,
    }
}

/// Whether `filter` can name a creature an opponent controls. Object
/// references the AI cannot resolve statically (a parent target, an event
/// object, the enchanted creature) can be opposing, so they answer yes;
/// players, the source and objects the controller made or paid answer no.
///
/// Used for non-Sacrifice effects (Destroy, Exile, Bounce, etc.) where
/// `controller: null` on a creature target means "any creature" including
/// opponents'. For Sacrifice effects, use
/// `may_be_opposing_creature_for_sacrifice` instead.
fn may_be_opposing_creature(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Any => true,
        TargetFilter::Typed(typed) => {
            !matches!(typed.controller, Some(ControllerRef::You))
                && typed.type_filters.iter().all(type_admits_creature)
        }
        TargetFilter::Or { filters } => filters.iter().any(may_be_opposing_creature),
        // CR 109.3: every constraint of an `And` must hold for the match.
        TargetFilter::And { filters } => filters.iter().all(may_be_opposing_creature),
        TargetFilter::Not { .. }
        | TargetFilter::AttachedTo
        | TargetFilter::SpecificObject { .. }
        | TargetFilter::ParentTarget
        | TargetFilter::ParentTargetSlot { .. }
        | TargetFilter::EventTarget
        | TargetFilter::TriggeringSource
        | TargetFilter::TrackedSet { .. }
        | TargetFilter::TrackedSetFiltered { .. }
        | TargetFilter::LastZoneChanged
        | TargetFilter::ChosenCard
        | TargetFilter::HasChosenName
        | TargetFilter::Named { .. }
        | TargetFilter::ChosenDamageSource { .. }
        | TargetFilter::PostReplacementDamageSource
        | TargetFilter::PostReplacementDamageTarget => true,
        // The source, its pair, and objects its controller created or paid.
        TargetFilter::SelfRef
        | TargetFilter::GrantingObject { .. }
        | TargetFilter::SourceOrPaired
        | TargetFilter::OriginalSource
        | TargetFilter::ControllerAndControlledPermanents { .. }
        | TargetFilter::LastCreated
        | TargetFilter::CostPaidObject
        | TargetFilter::AmassedArmy
        // Objects off the battlefield.
        | TargetFilter::StackAbility { .. }
        | TargetFilter::StackSpell
        | TargetFilter::CastingSpell
        | TargetFilter::ExiledBySource
        | TargetFilter::ExiledCardByIndex { .. }
        | TargetFilter::LastRevealed
        // Players, and a player-or-planeswalker attack target.
        | TargetFilter::None
        | TargetFilter::Player
        | TargetFilter::Controller
        | TargetFilter::SourceController
        | TargetFilter::Opponent
        | TargetFilter::SpecificPlayer { .. }
        | TargetFilter::PlayerWhoChoseLabel { .. }
        | TargetFilter::PlayerMatching { .. }
        | TargetFilter::Neighbor { .. }
        | TargetFilter::ScopedPlayer
        | TargetFilter::TriggeringSpellController
        | TargetFilter::TriggeringSpellOwner
        | TargetFilter::TriggeringPlayer
        | TargetFilter::TriggeringSourceController
        | TargetFilter::AttackedPlayerOrPlaneswalker
        | TargetFilter::EventTargetController
        | TargetFilter::DeclaredPlayer { .. }
        | TargetFilter::ParentTargetController
        | TargetFilter::ParentTargetOwner
        | TargetFilter::AffectedObjectOwner
        | TargetFilter::SourceChosenPlayer
        | TargetFilter::OriginalController
        | TargetFilter::PostReplacementSourceController
        | TargetFilter::PostReplacementDamageTargetOwner
        | TargetFilter::DefendingPlayer
        | TargetFilter::Owner
        | TargetFilter::AllPlayers => false,
    }
}

/// Whether a Sacrifice effect's target can name an opponent's creature.
///
/// Per CR 701.21a and the engine's `resolve_sacrifice_scope`, a Sacrifice
/// effect with `controller: null` defaults to the ability's controller (the
/// player who controls the ability). Only explicit opponent/player scopes
/// (TargetPlayer, Opponent, DeclaredPlayer, etc.) can reach an opposing
/// creature.
///
/// Exception: a broad `Permanent` type filter with null controller is
/// treated as potentially opponent-reaching, because "sacrifice a permanent"
/// commonly appears on "each player" or "target player" effects (e.g.,
/// Crack the Earth), whereas "sacrifice a creature" with null controller
/// typically indicates a self-only sacrifice (e.g., Oni Possession).
fn may_be_opposing_creature_for_sacrifice(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => {
            let admits_creature = typed.type_filters.iter().all(type_admits_creature);
            let is_broad_permanent = typed
                .type_filters
                .iter()
                .any(|f| matches!(f, TypeFilter::Permanent));
            matches!(
                typed.controller,
                Some(ControllerRef::Opponent)
                    | Some(ControllerRef::TargetPlayer)
                    | Some(ControllerRef::TargetOpponent)
                    | Some(ControllerRef::DeclaredPlayer { .. })
                    | Some(ControllerRef::EventTargetController)
                    | Some(ControllerRef::ParentTargetController)
                    | Some(ControllerRef::SourceChosenPlayer)
                    | Some(ControllerRef::ScopedPlayer)
                    | Some(ControllerRef::DefendingPlayer)
                    | Some(ControllerRef::ChosenPlayer { .. })
            ) && admits_creature
                || (typed.controller.is_none() && is_broad_permanent && admits_creature)
        }
        TargetFilter::Or { filters } => filters.iter().any(may_be_opposing_creature_for_sacrifice),
        TargetFilter::And { filters } => filters.iter().all(may_be_opposing_creature_for_sacrifice),
        _ => false,
    }
}

/// Whether a continuous modification lowers the recipient's toughness.
/// CR 704.5f: a creature with toughness 0 or less is put into its owner's
/// graveyard. A dynamic reduction carries its sign in the quantity (see
/// [`quantity_is_non_negative`]).
fn modification_lowers_toughness(modification: &ContinuousModification) -> bool {
    matches!(
        modification,
        ContinuousModification::AddToughness { value } if *value < 0
    ) || matches!(
        modification,
        ContinuousModification::AddDynamicToughness { value } if !quantity_is_non_negative(value)
    )
}

/// A static that removes another creature: it lowers the recipient's
/// toughness (Kagemaro's Clutch: "Enchanted creature gets -X/-X", CR 704.5f),
/// or it stops the recipient from both attacking and blocking (Cage of Hands:
/// "Enchanted creature can't attack or block", CR 508.1c + CR 509.1b), either
/// as one combined mode or as a pair of statics on the same recipient.
fn has_removal_static(face: &CardFace) -> bool {
    let others = || {
        face.static_abilities
            .iter()
            .filter(|s| !static_affects_self(s))
    };
    let shrinks = others().any(|s| s.modifications.iter().any(modification_lowers_toughness));
    let locks = others().any(|s| {
        s.mode == StaticMode::CantAttackOrBlock
            || (s.mode == StaticMode::CantAttack
                && others().any(|b| b.mode == StaticMode::CantBlock && b.affected == s.affected))
    });
    shrinks || locks
}

/// Removal-shaped effect or static anywhere on the face, under `reading`.
///
/// The shared classifier `is_direct_removal` decides every ordinary effect.
/// The full reading narrows it to effects that can reach an opposing
/// creature (see [`reaches_opposing_creature`]) and widens it by toughness
/// reductions (see [`is_shrink_removal`]), removal statics (see
/// [`has_removal_static`]), and sacrifice effects that can target an
/// opposing creature (e.g., Exile into Darkness: "Target player sacrifices
/// a creature", CR 701.21a). Self-sacrifice costs (e.g., Sakura-Tribe Elder)
/// target `SelfRef` and are excluded by `may_be_opposing_creature_for_sacrifice`.
fn is_removal(face: &CardFace, reading: RoleReading) -> bool {
    let effects = collect_face_effects(face);
    match reading {
        RoleReading::Classic => effects.into_iter().any(is_direct_removal),
        RoleReading::Full => {
            effects.into_iter().any(|effect| {
                (is_direct_removal(effect) && reaches_opposing_creature(effect))
                    || is_shrink_removal(effect)
                    // Sacrifice that can reach an opposing creature (edicts).
                    // Self-sacrifice (SelfRef) is excluded by the target check.
                    // Use the sacrifice-specific checker which respects that
                    // `controller: null` defaults to the ability's controller.
                    || matches!(
                        effect,
                        Effect::Sacrifice { target, .. }
                            if may_be_opposing_creature_for_sacrifice(target)
                    )
            }) || has_removal_static(face)
        }
    }
}

/// Whether the face fills the Removal deck role. The classic reading also
/// counts mass damage and a mass -N/-N; the full reading's removal already
/// does.
fn is_removal_role(face: &CardFace, reading: RoleReading) -> bool {
    is_removal(face, reading)
        || (reading == RoleReading::Classic
            && EffectProfile::from_face(face).has_mass_damage_or_mass_shrink_text)
}

/// Whether the face makes mana of two or more colors or, in the full
/// reading, searches its controller's library for a land: the single fixing
/// predicate for both class and deck role.
fn is_mana_fixing(face: &CardFace, reading: RoleReading) -> bool {
    produced_color_count(face) >= 2
        || (reading == RoleReading::Full && face.abilities.iter().any(chain_searches_for_land))
}

/// Whether the face has an activated mana ability (CR 605.1a). The whole chain
/// is searched, not just the top-level effect: a mana ability can sit behind a
/// leading step whose sub-ability adds the mana.
fn has_mana_ability(face: &CardFace) -> bool {
    face.abilities
        .iter()
        .any(|a| a.kind == AbilityKind::Activated && chain_has_mana_effect(a))
}

/// Classify a face into the roles the profile's class bonuses key on, under
/// the full reading of its printed abilities (see [`RoleReading::Full`]).
pub fn classify(face: &CardFace) -> BTreeSet<CardClass> {
    classify_with(face, RoleReading::Full)
}

/// Classify a face under a set's role reading (see [`RoleReading`]).
fn classify_with(face: &CardFace, reading: RoleReading) -> BTreeSet<CardClass> {
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
    if is_removal(face, reading) {
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
    let has_subtype = |name: &str| {
        face.card_type
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case(name))
    };
    let spell = core.contains(&CoreType::Instant) || core.contains(&CoreType::Sorcery);
    if creature && has_subtype("Spirit") {
        classes.insert(CardClass::Spirit);
    }
    if spell && has_subtype("Arcane") {
        classes.insert(CardClass::ArcaneSpell);
    }
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Soulshift(_)))
    {
        classes.insert(CardClass::Soulshift);
    }
    if face
        .keywords
        .iter()
        .any(|k| matches!(k, Keyword::Splice { .. }))
    {
        classes.insert(CardClass::Splice);
    }
    if creature && has_subtype("Ninja") {
        classes.insert(CardClass::Ninja);
    }
    if creature && is_evasive(face, reading) {
        classes.insert(CardClass::EvasiveThreat);
        if face.mana_cost.mana_value() <= NINJUTSU_ENABLER_MAX_MANA_VALUE {
            classes.insert(CardClass::NinjutsuEnabler);
        }
    }
    if grants_evasion(face, reading) {
        classes.insert(CardClass::NinjutsuEnabler);
    }
    if has_channel_ability(face) {
        classes.insert(CardClass::Channel);
    }
    if core.contains(&CoreType::Instant)
        && !classes.contains(&CardClass::Removal)
        && has_buff_effect(face)
    {
        classes.insert(CardClass::CombatTrick);
    }
    if is_mana_fixing(face, reading) {
        classes.insert(CardClass::ManaFixing);
    }
    classes
}

/// Whether the face has a Channel ability, judged per ability.
fn has_channel_ability(face: &CardFace) -> bool {
    face.abilities.iter().any(is_channel_ability)
}

/// A Channel ability: "Channel — [Cost], Discard this card: [Effect]".
///
/// CR 207.2c: Channel is an ability word with no rules meaning, so the label
/// is the ability's only identity; the parser keeps it in the ability's
/// description. The ability must also activate from hand (CR 602.1) and pay
/// "Discard this card" as part of its cost (CR 602.1a). Cycling (CR 702.29a),
/// Transmute (CR 702.53a) and Bloodrush share that shape but not the label.
fn is_channel_ability(ability: &AbilityDefinition) -> bool {
    ability.kind == AbilityKind::Activated
        && ability.activation_zone == Some(Zone::Hand)
        && ability
            .cost
            .as_ref()
            .is_some_and(AbilityCost::consumes_source)
        && ability
            .description
            .as_deref()
            .is_some_and(has_channel_label)
}

/// Whether ability text opens with the Channel ability-word label.
fn has_channel_label(description: &str) -> bool {
    let lower = description.to_lowercase();
    let label: IResult<&str, ()> = value(
        (),
        pair(tag("channel"), alt((tag(" \u{2014} "), tag(" -- ")))),
    )
    .parse(lower.as_str());
    label.is_ok()
}

/// Whether any spell or ability chain on the face raises a creature's power or
/// toughness without lowering either: a combat trick, not a "-5/-5".
fn has_buff_effect(face: &CardFace) -> bool {
    face.abilities.iter().any(|a| {
        collect_chain_effects(a).iter().any(|e| {
            matches!(
                e,
                Effect::Pump { power, toughness, .. } | Effect::PumpAll { power, toughness, .. }
                    if !is_negative(power)
                        && !is_negative(toughness)
                        && !(is_zero(power) && is_zero(toughness))
            )
        })
    })
}

/// The deck roles a face fills under the full reading, for the deck-need
/// bonus. Test-only: scoring resolves each face's roles under its own set's
/// role reading through [`DraftProfile::roles_for`].
#[cfg(test)]
fn roles(face: &CardFace) -> Vec<DeckRole> {
    roles_with(face, RoleReading::Full)
}

/// The deck roles a face fills under `reading`. Each role uses the same
/// predicate as the class bonus that advertises it.
fn roles_with(face: &CardFace, reading: RoleReading) -> Vec<DeckRole> {
    let mut roles = Vec::new();
    if is_removal_role(face, reading) {
        roles.push(DeckRole::Removal);
    }
    let creature = is_creature(face);
    if creature {
        roles.push(DeckRole::Creature);
    }
    if (creature && is_evasive(face, reading)) || grants_evasion(face, reading) {
        roles.push(DeckRole::Evasion);
    }
    if EffectProfile::from_face(face).has_draw {
        roles.push(DeckRole::CardAdvantage);
    }
    if is_mana_fixing(face, reading) {
        roles.push(DeckRole::Fixing);
    }
    if has_mana_ability(face) && !face.card_type.core_types.contains(&CoreType::Land) {
        roles.push(DeckRole::Acceleration);
    }
    roles
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
    use engine::types::ability::{
        AbilityDefinition, Effect, ManaContribution, ManaProduction, StaticDefinition,
    };
    use engine::types::card_type::CardType;
    use engine::types::mana::ManaColor;
    use engine::types::statics::BlockExceptionKind;

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

    /// Position of the pick after `prior_picks` picks in 15-card packs.
    fn at(prior_picks: usize) -> PackPosition {
        PackPosition {
            pack_number: prior_picks / 15,
            pick_in_pack: prior_picks % 15 + 1,
        }
    }

    fn score15(
        p: &DraftProfile,
        c: &ProfileCard<'_>,
        pool: &[ProfileCard<'_>],
        base: f64,
        curve: f64,
    ) -> Option<ProfileScore> {
        p.score_pick(c, pool, at(pool.len()), base, curve)
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
        assert_eq!(p.artifact_density.as_ref().unwrap().target_share, 0.45);
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
        let early = p.stage_weights(at(0));
        assert_eq!(
            (early.raw_power, early.deck_fit, early.color_commitment),
            (1.0, 0.25, 0.0)
        );
        assert_eq!(p.stage_weights(at(4)), early);
        let middle = p.stage_weights(at(5));
        assert_eq!(
            (middle.raw_power, middle.deck_fit, middle.color_commitment),
            (0.8, 0.7, 0.5)
        );
        assert_eq!(p.stage_weights(at(8)), middle);
        let late = p.stage_weights(at(9));
        assert_eq!(
            (late.raw_power, late.deck_fit, late.color_commitment),
            (0.6, 1.0, 1.0)
        );
        assert_eq!(p.stage_weights(at(14)), late);
        // Pick 1 of pack 2 behaves like the middle stage, not the early one.
        assert_eq!(p.stage_weights(at(15)), middle);
        assert_eq!(p.stage_weights(at(30)), middle);
        assert_eq!(p.stage_weights(at(24)), late);
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
        let s = score15(p, &c, &[], 2.0, 0.0).unwrap();
        assert_eq!(s.card_bonus, 6.0);
        assert_eq!(s.class_bonus, 1.5);
        // Pick 1: raw weight 1.0, fit weight 0.25 (only the artifact-density term here).
        let expected = 2.0 + 6.0 + 1.5 + 0.25 * s.artifact_density;
        assert!((s.total - expected).abs() < 1e-9);
        // A different base moves the total one-for-one.
        let s2 = score15(p, &c, &[], 3.0, 0.0).unwrap();
        assert!((s2.total - s.total - 1.0).abs() < 1e-9);
    }

    #[test]
    fn class_bonus_takes_the_max_not_the_sum() {
        let p = DraftProfile::mirrodin_block();
        let mut myr = face(vec![CoreType::Artifact, CoreType::Creature], &[]);
        myr.abilities = vec![tap_for(vec![ManaColor::Red])];
        myr.keywords = vec![Keyword::Modular(1)];
        let c = card("Test Myr", "DST", &[], &myr);
        let s = score15(p, &c, &[], 0.0, 0.0).unwrap();
        assert_eq!(s.class_bonus, 1.0); // mana_creature 1.0 beats modular 0.75, not 1.75
    }

    #[test]
    fn unknown_set_is_not_scored() {
        let p = DraftProfile::mirrodin_block();
        let f = face(vec![CoreType::Instant], &[]);
        assert!(score15(p, &card("Bolt", "LEA", &[], &f), &[], 1.0, 0.0).is_none());
    }

    #[test]
    fn artifact_density_bonus_shrinks_as_the_pool_fills_with_artifacts() {
        let p = DraftProfile::mirrodin_block();
        let sword = face(vec![CoreType::Artifact], &["Equipment"]);
        let empty_pool_bonus = score15(p, &card("Bonesplitter", "MRD", &[], &sword), &[], 0.0, 0.0)
            .unwrap()
            .artifact_density;
        let pool: Vec<ProfileCard<'_>> = (0..6)
            .map(|_| card("Bonesplitter", "MRD", &[], &sword))
            .collect();
        let full_pool_bonus = score15(
            p,
            &card("Bonesplitter", "MRD", &[], &sword),
            &pool,
            0.0,
            0.0,
        )
        .unwrap()
        .artifact_density;
        assert!(empty_pool_bonus > 0.0);
        assert_eq!(full_pool_bonus, 0.0);

        // Non-artifacts never earn it.
        let bear = face(vec![CoreType::Creature], &[]);
        let s = score15(p, &card("Bear", "MRD", &[], &bear), &[], 0.0, 0.0).unwrap();
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
        let with_roles = score15(p, &probe, &role_pool, 0.0, 0.0)
            .unwrap()
            .artifact_density;
        let with_filler = score15(p, &probe, &filler_pool, 0.0, 0.0)
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

        let in_5dn = score15(p, &card("Flyer", "5DN", &[], &flyer), &[], 0.0, 0.0).unwrap();
        assert_eq!(in_5dn.threat_need, 1.5);
        let in_mrd = score15(p, &card("Flyer", "MRD", &[], &flyer), &[], 0.0, 0.0).unwrap();
        assert_eq!(in_mrd.threat_need, 0.0);
        let plain = score15(p, &card("Bear", "5DN", &[], &bear), &[], 0.0, 0.0).unwrap();
        assert_eq!(plain.threat_need, 0.0);

        // Four threats already drafted: the need is met.
        let pool = vec![card("Flyer", "5DN", &[], &flyer); 4];
        let met = score15(p, &card("Flyer", "5DN", &[], &flyer), &pool, 0.0, 0.0).unwrap();
        assert_eq!(met.threat_need, 0.0);
    }

    #[test]
    fn mana_myr_do_not_satisfy_5dn_finisher_threat_target() {
        let p = DraftProfile::mirrodin_block();
        // The MRD mana Myr are 1/1 Artifact Creature — Myr with "{T}: Add {C}."
        // for one color each, and the profile rates every one of them at or above
        // the premium-threat card bonus for acceleration and fixing.
        let myr = [
            ("Iron Myr", ManaColor::Red),
            ("Silver Myr", ManaColor::Blue),
            ("Leaden Myr", ManaColor::Black),
            ("Copper Myr", ManaColor::Green),
        ];
        let myr_faces: Vec<CardFace> = myr
            .iter()
            .map(|&(_, color)| {
                let mut f = face(vec![CoreType::Artifact, CoreType::Creature], &["Myr"]);
                f.power = Some(PtValue::Fixed(1));
                f.toughness = Some(PtValue::Fixed(1));
                f.mana_cost = engine::types::mana::ManaCost::generic(2);
                f.abilities = vec![tap_for(vec![color])];
                f
            })
            .collect();
        let pool: Vec<ProfileCard<'_>> = myr
            .iter()
            .zip(&myr_faces)
            .map(|(&(name, _), f)| card(name, "MRD", &[], f))
            .collect();
        for myr in &pool {
            assert!(p.bonus_for_name(myr.name) >= PREMIUM_THREAT_THRESHOLD);
            assert!(!p.is_threat(myr), "{} is not a finisher", myr.name);
        }

        // Hoverguard Sweepers: 5/6 flying, a real 5DN finisher.
        let mut sweepers = face(vec![CoreType::Creature], &["Drone"]);
        sweepers.power = Some(PtValue::Fixed(5));
        sweepers.toughness = Some(PtValue::Fixed(6));
        sweepers.mana_cost = engine::types::mana::parse_mtgjson_mana_cost("{6}{U}{U}");
        sweepers.keywords = vec![Keyword::Flying];
        let finisher = card("Hoverguard Sweepers", "5DN", &[], &sweepers);
        assert!(p.is_threat(&finisher));

        // Positive control: four real threats do satisfy the target.
        let threats = vec![finisher; 4];
        let sated = score15(p, &finisher, &threats, 0.0, 0.0).unwrap();
        assert_eq!(sated.threat_need, 0.0);

        let scored = score15(p, &finisher, &pool, 0.0, 0.0).unwrap();
        assert_eq!(
            scored.threat_need, 1.5,
            "a pool of four mana Myr still leaves the full threat bonus on an actual finisher"
        );
    }

    #[test]
    fn chained_mana_ability_classifies_as_mana_creature() {
        // The activated ability opens with a non-mana step and the mana arrives
        // in its sub-ability.
        let chained = AbilityDefinition::new(AbilityKind::Activated, Effect::NoOp)
            .sub_ability(tap_for(vec![ManaColor::Red]));
        let mut dork = face(vec![CoreType::Creature], &[]);
        dork.power = Some(PtValue::Fixed(4));
        dork.abilities = vec![chained];

        assert_eq!(classify(&dork), BTreeSet::from([CardClass::ManaCreature]));
        // However big, a mana creature is not a finisher.
        let p = DraftProfile::mirrodin_block();
        let mut flying_dork = dork.clone();
        flying_dork.keywords = vec![Keyword::Flying];
        assert!(!p.is_threat(&card("Chained Dork", "5DN", &[], &flying_dork)));
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
            score15(p, &probe, &pool, 0.0, 0.0).unwrap().deck_need
        };
        assert_eq!(need(0), 2.0);
        assert!(need(2) < need(0));
        assert!(need(2) > need(4));
        assert_eq!(need(5), 0.0);
    }

    #[test]
    fn archetype_synergy_requires_demonstrated_theme_and_nets_class_paid_themes() {
        let p = DraftProfile::mirrodin_block();
        let sword = face(vec![CoreType::Artifact], &["Equipment"]);
        let mut talisman = face(vec![CoreType::Artifact], &[]);
        talisman.abilities = vec![tap_for(vec![ManaColor::Red])];

        // Artifact-affinity signal needs distinct quality artifacts; one name repeated is one signal.
        let repeated = vec![card("Bonesplitter", "MRD", &[], &sword); 5];
        let probe = card("Talisman of Impulse", "MRD", &[], &talisman);
        assert_eq!(
            score15(p, &probe, &repeated, 0.0, 0.0)
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
        let s = score15(p, &probe, &pool, 0.0, 0.0).unwrap();
        assert!(s.archetype_synergy > 0.0 && s.archetype_synergy <= 2.5);

        // The equipment theme is demonstrated too, but equipment already earns its class
        // bonus, so that theme pays only its excess over it.
        let sword_probe = card("Sword", "MRD", &[], &sword);
        let sw = score15(p, &sword_probe, &pool, 0.0, 0.0).unwrap();
        // The unlinked artifact-affinity theme is the strongest one for equipment.
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
            score15(p, &probe, &pool, 0.0, 0.0)
                .unwrap()
                .off_color_penalty,
            0.0
        );
        // Middle (pool of 5..8).
        pool.push(card("Red", "MRD", &red, &red_face));
        let mid = score15(p, &probe, &pool, 0.0, 0.0)
            .unwrap()
            .off_color_penalty;
        // Late (pool of 9..14).
        pool.extend((0..4).map(|_| card("Red", "MRD", &red, &red_face)));
        let late = score15(p, &probe, &pool, 0.0, 0.0)
            .unwrap()
            .off_color_penalty;
        assert!(mid > 0.0 && late > mid);
        // On-color and colorless cards are never penalised.
        let on = card("Red2", "MRD", &red, &red_face);
        assert_eq!(
            score15(p, &on, &pool, 0.0, 0.0).unwrap().off_color_penalty,
            0.0
        );
        let none = card("Rock", "MRD", &[], &red_face);
        assert_eq!(
            score15(p, &none, &pool, 0.0, 0.0)
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
        let early = score15(p, &c, &[], 4.0, 1.0).unwrap();
        let filler = face(vec![CoreType::Creature], &[]);
        let pool = vec![card("Filler", "MRD", &[], &filler); 9];
        let late = score15(p, &c, &pool, 4.0, 1.0).unwrap();
        // Same card, same base: power-weighted early total exceeds the late one's raw share.
        assert!(early.total > 0.0 && late.total > 0.0);
        assert!(early.deck_need == late.deck_need || late.deck_need > 0.0);
        let w_early = p.stage_weights(at(0));
        let w_late = p.stage_weights(at(9));
        assert!(w_early.raw_power > w_late.raw_power);
        assert!(w_late.deck_fit > w_early.deck_fit);
    }

    #[test]
    fn kamigawa_profile_parses_and_covers_only_its_block() {
        let p = DraftProfile::kamigawa_block();
        assert!(p.artifact_density.is_none());
        assert_eq!(
            p.adaptive_draft_policy.pack_order,
            vec!["CHK", "BOK", "SOK"]
        );
        assert_eq!(p.sets["BOK"].class_bonus[&CardClass::Ninja], 0.8);
        assert_eq!(p.sets["SOK"].class_bonus[&CardClass::Channel], 0.45);
        assert_eq!(p.card_bonus["umezawa's jitte"], 20.0);
        for code in ["CHK", "bok", "SOK"] {
            assert!(DraftProfile::for_set(code).is_some_and(|p| p.id == "kamigawa-block-cbs"));
            assert_eq!(p.reading(code), RoleReading::Full, "{code}");
        }
        // Mirrodin's sets keep the reading their scores were tuned against.
        for code in ["MRD", "DST", "5DN"] {
            assert_eq!(
                DraftProfile::mirrodin_block().reading(code),
                RoleReading::Classic,
                "{code}"
            );
        }
        assert!(DraftProfile::for_set("MRD").is_some_and(|p| p.id == "mirrodin-block"));
        assert_eq!(
            p.adaptive_draft_policy.synergy_overlap,
            SynergyOverlap::Excess
        );
    }

    /// The two flip cards with a named bonus arrive from the published BOK
    /// booster sheets under their full "front // flipped" names, and the
    /// name-keyed lookup is exact.
    #[test]
    fn flip_card_bonuses_match_their_published_booster_names() {
        let p = DraftProfile::kamigawa_block();
        for name in [
            "Budoka Pupil // Ichiga, Who Topples Oaks",
            "Callow Jushi // Jaraku the Interloper",
        ] {
            let pick = ProfileCard {
                name,
                set_code: "BOK",
                colors: &[],
                face: None,
            };
            assert_eq!(first_pick(p, &pick).card_bonus, 1.0, "{name}");
        }
    }

    #[test]
    fn classify_recognises_kamigawa_classes_from_card_data() {
        let mut spirit = face(vec![CoreType::Creature], &["Spirit"]);
        spirit.keywords = vec![Keyword::Soulshift(3), Keyword::Flying];
        spirit.mana_cost = engine::types::mana::ManaCost::generic(3);
        let classes = classify(&spirit);
        for class in [
            CardClass::Spirit,
            CardClass::Soulshift,
            CardClass::EvasiveThreat,
            CardClass::NinjutsuEnabler,
        ] {
            assert!(classes.contains(&class), "{class:?}");
        }
        assert!(!classes.contains(&CardClass::Ninja));

        // A big flier is evasive but not a cheap ninjutsu enabler.
        let mut big = face(vec![CoreType::Creature], &[]);
        big.keywords = vec![Keyword::Flying];
        big.mana_cost = engine::types::mana::ManaCost::generic(6);
        let classes = classify(&big);
        assert!(classes.contains(&CardClass::EvasiveThreat));
        assert!(!classes.contains(&CardClass::NinjutsuEnabler));

        let ninja = face(vec![CoreType::Creature], &["Ninja"]);
        assert!(classify(&ninja).contains(&CardClass::Ninja));

        let mut arcane = face(vec![CoreType::Instant], &["Arcane"]);
        arcane.keywords = vec![Keyword::Splice {
            subtype: "Arcane".into(),
            cost: engine::types::mana::ManaCost::generic(1),
        }];
        let classes = classify(&arcane);
        assert!(classes.contains(&CardClass::ArcaneSpell));
        assert!(classes.contains(&CardClass::Splice));
        // Arcane on a creature is not an Arcane spell.
        assert!(!classify(&face(vec![CoreType::Creature], &["Arcane"]))
            .contains(&CardClass::ArcaneSpell));

        let mut dual = face(vec![CoreType::Land], &[]);
        dual.abilities = vec![tap_for(vec![ManaColor::Red, ManaColor::Green])];
        assert!(classify(&dual).contains(&CardClass::ManaFixing));
    }

    /// A face built through the production MTGJSON → parser → synthesis path.
    /// `card` supplies the card-specific `AtomicCard` fields.
    fn oracle_face(card: serde_json::Value) -> CardFace {
        let mut json = serde_json::json!({
            "colors": [],
            "colorIdentity": [],
            "layout": "normal",
            "manaValue": 0.0,
            "identifiers": {},
        });
        json.as_object_mut()
            .unwrap()
            .extend(card.as_object().unwrap().clone());
        let atomic: engine::database::mtgjson::AtomicCard =
            serde_json::from_value(json).expect("valid AtomicCard fixture");
        engine::database::synthesis::build_oracle_face(&atomic, None)
    }

    /// Sakura-Tribe Elder: a printed CHK fixer whose land search is the
    /// controller's own library.
    fn sakura_tribe_elder() -> CardFace {
        oracle_face(serde_json::json!({
            "name": "Sakura-Tribe Elder",
            "manaCost": "{1}{G}",
            "manaValue": 2.0,
            "colors": ["G"],
            "type": "Creature — Snake Shaman",
            "types": ["Creature"],
            "subtypes": ["Snake", "Shaman"],
            "power": "1",
            "toughness": "1",
            "text": "Sacrifice this creature: Search your library for a basic land card, put that card onto the battlefield tapped, then shuffle.",
        }))
    }

    /// Kodama's Reach: a printed CHK Arcane fixer.
    fn kodama_s_reach() -> CardFace {
        oracle_face(serde_json::json!({
            "name": "Kodama's Reach",
            "manaCost": "{2}{G}",
            "manaValue": 3.0,
            "colors": ["G"],
            "type": "Sorcery — Arcane",
            "types": ["Sorcery"],
            "subtypes": ["Arcane"],
            "text": "Search your library for up to two basic land cards, reveal those cards, put one onto the battlefield tapped and the other into your hand, then shuffle.",
        }))
    }

    #[test]
    fn channel_is_identified_per_ability() {
        let hand_discard_self = |f: &CardFace| {
            f.abilities.iter().any(|a| {
                a.activation_zone == Some(Zone::Hand)
                    && a.cost.as_ref().is_some_and(AbilityCost::consumes_source)
            })
        };

        let shinen = oracle_face(serde_json::json!({
            "name": "Shinen of Life's Roar",
            "manaCost": "{1}{G}",
            "manaValue": 2.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "1",
            "toughness": "2",
            "text": "All creatures able to block this creature do so.\nChannel — {2}{G}{G}, Discard this card: All creatures able to block target creature this turn do so.",
        }));
        assert!(classify(&shinen).contains(&CardClass::Channel));

        // Cycling on the same card does not hide its Channel ability, and the
        // Cycling ability alone (also "Discard this card", from hand) is not one.
        let cycler_text = "Channel — {1}{G}, Discard this card: Target creature gets +2/+2 until end of turn.\nCycling {2} ({2}, Discard this card: Draw a card.)";
        let cycler = oracle_face(serde_json::json!({
            "name": "Test Channel Cycler",
            "manaCost": "{2}{G}",
            "manaValue": 3.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "2",
            "toughness": "2",
            "keywords": ["Cycling"],
            "text": cycler_text,
        }));
        let cycling: Vec<&AbilityDefinition> = cycler
            .abilities
            .iter()
            .filter(|a| a.ability_tag == Some(engine::types::ability::AbilityTag::Cycling))
            .collect();
        assert_eq!(cycling.len(), 1, "the Cycling ability is synthesized");
        assert!(cycling[0].cost.as_ref().unwrap().consumes_source());
        assert!(!is_channel_ability(cycling[0]));
        assert!(classify(&cycler).contains(&CardClass::Channel));

        // Transmute and Bloodrush are hand-activated "Discard this card"
        // abilities too, but not Channel.
        let infiltrator = oracle_face(serde_json::json!({
            "name": "Dimir Infiltrator",
            "manaCost": "{U}{B}",
            "manaValue": 2.0,
            "colors": ["U", "B"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "1",
            "toughness": "3",
            "keywords": ["Transmute"],
            "text": "This creature can't be blocked.\nTransmute {1}{U}{B} ({1}{U}{B}, Discard this card: Search your library for a card with the same mana value as this card, reveal it, put it into your hand, then shuffle. Transmute only as a sorcery.)",
        }));
        assert!(
            hand_discard_self(&infiltrator),
            "positive guard: transmute ability exists"
        );
        assert!(!classify(&infiltrator).contains(&CardClass::Channel));

        let maaka = oracle_face(serde_json::json!({
            "name": "Rubblebelt Maaka",
            "manaCost": "{3}{R}",
            "manaValue": 4.0,
            "colors": ["R"],
            "type": "Creature — Cat",
            "types": ["Creature"],
            "subtypes": ["Cat"],
            "power": "3",
            "toughness": "3",
            "text": "Bloodrush — {R}, Discard this card: Target attacking creature gets +3/+3 until end of turn.",
        }));
        assert!(
            hand_discard_self(&maaka),
            "positive guard: bloodrush ability exists"
        );
        assert!(!classify(&maaka).contains(&CardClass::Channel));

        // A hand-activated ability that does not discard the card is not Channel,
        // even when labelled as one.
        let mut no_discard = tap_for(vec![ManaColor::Green]);
        no_discard.activation_zone = Some(Zone::Hand);
        no_discard.description = Some("Channel — {T}: Add {G}.".into());
        assert!(!is_channel_ability(&no_discard));
    }

    #[test]
    fn land_search_creature_rated_highly_is_not_a_finisher() {
        let p = DraftProfile::kamigawa_block();
        let elder = sakura_tribe_elder();
        let green = vec!["G".to_string()];
        let elder_card = card("Sakura-Tribe Elder", "CHK", &green, &elder);
        assert!(p.bonus_for_name(elder_card.name) >= PREMIUM_THREAT_THRESHOLD);
        assert!(classify(&elder).contains(&CardClass::ManaFixing));
        assert!(!p.is_threat(&elder_card));
        // The Elder keeps its full pick-quality bonus; only threat status is
        // withheld, so it earns no threat-need bonus even from an empty pool.
        let elder_score = score15(p, &elder_card, &[], 0.0, 0.0).unwrap();
        assert_eq!(elder_score.card_bonus, 2.0);
        assert_eq!(elder_score.threat_need, 0.0);

        let kodama = oracle_face(serde_json::json!({
            "name": "Kodama of the North Tree",
            "manaCost": "{2}{G}{G}{G}",
            "manaValue": 5.0,
            "colors": ["G"],
            "type": "Legendary Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "supertypes": ["Legendary"],
            "power": "6",
            "toughness": "4",
            "keywords": ["Shroud", "Trample"],
            "text": "Trample\nShroud (This creature can't be the target of spells or abilities.)",
        }));
        let finisher = card("Kodama of the North Tree", "CHK", &green, &kodama);
        assert!(
            p.is_threat(&finisher),
            "a premium finisher keeps threat status"
        );

        // A mana/fixing creature never consumes the finisher budget: the
        // threat count reads role, not pick quality, so copies of the Elder
        // cannot reduce or eliminate a real finisher's bonus.
        let elders = vec![elder_card; 4];
        let scored = score15(p, &finisher, &elders, 0.0, 0.0).unwrap();
        assert_eq!(scored.threat_need, 1.25);
        let mixed = vec![elder_card, finisher, elder_card, finisher];
        let mixed_score = score15(p, &finisher, &mixed, 0.0, 0.0).unwrap();
        assert_eq!(mixed_score.threat_need, 0.625, "two of four threats held");
        let finishers = vec![finisher; 4];
        let sated = score15(p, &finisher, &finishers, 0.0, 0.0).unwrap();
        assert_eq!(sated.threat_need, 0.0);
    }

    #[test]
    fn class_paid_theme_still_earns_its_excess_synergy() {
        let p = DraftProfile::kamigawa_block();
        let arcane_instant = face(vec![CoreType::Instant], &["Arcane"]);
        let arcane_sorcery = face(vec![CoreType::Sorcery], &["Arcane"]);
        let red = vec!["R".to_string()];
        let black = vec!["B".to_string()];
        let blue = vec!["U".to_string()];
        let probe = card("Petals of Insight", "CHK", &blue, &arcane_sorcery);

        // Distinct quality Arcane spells (card bonus >= 1.0).
        let mut pool = vec![
            card("Glacial Ray", "CHK", &red, &arcane_instant),
            // Rend Flesh is printed as an Instant in Champions.
            card("Rend Flesh", "CHK", &black, &arcane_instant),
        ];
        let below = score15(p, &probe, &pool, 0.0, 0.0).unwrap();
        assert_eq!(below.archetype_synergy, 0.0, "theme not yet demonstrated");

        // Horobi's Whisper is printed in Betrayers.
        pool.push(card("Horobi's Whisper", "BOK", &black, &arcane_instant));
        pool.push(card("Blind with Anger", "CHK", &red, &arcane_instant));
        assert_eq!(p.theme_signals(&pool)[&Theme::ArcaneSpliceControl], 4);
        let s = score15(p, &probe, &pool, 0.0, 0.0).unwrap();
        // Strength 2.5 * 4 / 6, less the Arcane class bonus (0.45) already paid.
        assert_eq!(s.class_bonus, 0.45);
        let expected = 2.5 * 4.0 / 6.0 - 0.45;
        assert!(
            (s.archetype_synergy - expected).abs() < 1e-9,
            "{}",
            s.archetype_synergy
        );
    }

    #[test]
    fn kamigawa_evasive_creature_gets_its_set_class_bonus_as_an_increment() {
        let p = DraftProfile::kamigawa_block();
        let mut flier = face(vec![CoreType::Creature], &[]);
        flier.keywords = vec![Keyword::Flying];
        flier.mana_cost = engine::types::mana::ManaCost::generic(5);
        let colors = vec!["U".to_string()];
        let c = card("Some Flier", "CHK", &colors, &flier);
        let s = score15(p, &c, &[], 2.0, 0.0).unwrap();
        assert_eq!(s.class_bonus, 0.55);
        assert_eq!(s.card_bonus, 0.0);
    }

    #[test]
    fn mirrodin_density_weights_only_roles_the_profile_rewards() {
        let p = DraftProfile::mirrodin_block();
        let ornithopter = oracle_face(serde_json::json!({
            "name": "Ornithopter",
            "manaCost": "{0}",
            "manaValue": 0.0,
            "colors": [],
            "type": "Artifact Creature — Thopter",
            "types": ["Artifact", "Creature"],
            "subtypes": ["Thopter"],
            "power": "0",
            "toughness": "2",
            "keywords": ["Flying"],
            "text": "Flying",
        }));
        let condor = oracle_face(serde_json::json!({
            "name": "Clockwork Condor",
            "manaCost": "{4}",
            "manaValue": 4.0,
            "colors": [],
            "type": "Artifact Creature — Bird",
            "types": ["Artifact", "Creature"],
            "subtypes": ["Bird"],
            "power": "0",
            "toughness": "0",
            "keywords": ["Flying"],
            "text": "Flying\nThis creature enters with three +1/+1 counters on it.\nWhenever this creature attacks or blocks, remove a +1/+1 counter from it at end of combat.",
        }));
        let filler = face(vec![CoreType::Artifact], &[]);
        let equipment = face(vec![CoreType::Artifact], &["Equipment"]);
        let density = |name: &str, f: &CardFace| {
            score15(p, &card(name, "MRD", &[], f), &[], 0.0, 0.0)
                .unwrap()
                .artifact_density
        };

        assert!(
            DraftProfile::kamigawa_block().rewards_class(CardClass::EvasiveThreat),
            "positive guard: the class is one another profile rewards"
        );
        assert!(!p.rewards_class(CardClass::EvasiveThreat));
        for (name, f) in [("Ornithopter", &ornithopter), ("Clockwork Condor", &condor)] {
            assert!(classify(f).contains(&CardClass::EvasiveThreat));
            assert!(p.bonus_for_name(name) < SIGNAL_QUALITY_THRESHOLD);
            assert_eq!(
                p.artifact_weight(&card(name, "MRD", &[], f)),
                FILLER_ARTIFACT_WEIGHT,
                "{name} earns no Mirrodin role"
            );
            assert_eq!(
                density(name, f),
                density("Mystery Rock", &filler),
                "{name} must count exactly like roleless filler"
            );
            assert_eq!(density(name, f), 0.5, "{name}");
        }
        // Positive control: an Equipment role earns the full weight.
        assert_eq!(density("Bonesplitter", &equipment), 1.0);

        // Nim Replica prints a "-1/-1" shrink. The full reading counts it as
        // removal, but Mirrodin keeps the classic reading, so the card keeps
        // its 0.5 filler weight and earns no class bonus: Mirrodin scoring is
        // unchanged by the Kamigawa reading.
        let nim = oracle_face(serde_json::json!({
            "name": "Nim Replica",
            "manaCost": "{3}",
            "manaValue": 3.0,
            "colors": [],
            "type": "Artifact Creature — Zombie",
            "types": ["Artifact", "Creature"],
            "subtypes": ["Zombie"],
            "power": "3",
            "toughness": "1",
            "text": "{2}{B}, Sacrifice this creature: Target creature gets -1/-1 until end of turn.",
        }));
        assert!(
            classify(&nim).contains(&CardClass::Removal),
            "positive guard: the shrink is removal-shaped"
        );
        assert!(!classify_with(&nim, RoleReading::Classic).contains(&CardClass::Removal));
        assert_eq!(
            p.artifact_weight(&card("Nim Replica", "MRD", &[], &nim)),
            FILLER_ARTIFACT_WEIGHT
        );
        let nim_score = score15(p, &card("Nim Replica", "MRD", &[], &nim), &[], 0.0, 0.0).unwrap();
        assert_eq!(nim_score.class_bonus, 0.0);
        assert_eq!(nim_score.artifact_density, 0.5);
    }

    #[test]
    fn printed_menace_and_unblockable_cards_are_evasive_roles() {
        let p = DraftProfile::kamigawa_block();
        let green = vec!["G".to_string()];
        let blue = vec!["U".to_string()];

        // Vine Kami: "Menace (This creature can't be blocked except by two or
        // more creatures.)" and Soulshift 6.
        let vine = oracle_face(serde_json::json!({
            "name": "Vine Kami",
            "manaCost": "{6}{G}",
            "manaValue": 7.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "4",
            "toughness": "4",
            "keywords": ["Menace", "Soulshift"],
            "text": "Menace (This creature can't be blocked except by two or more creatures.)\nSoulshift 6 (When this creature dies, you may return target Spirit card with mana value 6 or less from your graveyard to your hand.)",
        }));
        let vine_card = card("Vine Kami", "CHK", &green, &vine);
        assert!(classify(&vine).contains(&CardClass::EvasiveThreat));
        assert!(roles(&vine).contains(&DeckRole::Evasion));
        assert_eq!(
            score15(p, &vine_card, &[], 0.0, 0.0).unwrap().class_bonus,
            0.55
        );

        // Veil of Secrecy: "Target creature gains shroud until end of turn and
        // can't be blocked this turn." The shroud is not evasion; the
        // unblockable clause is what a Ninjutsu activation needs.
        let veil = oracle_face(serde_json::json!({
            "name": "Veil of Secrecy",
            "manaCost": "{1}{U}",
            "manaValue": 2.0,
            "colors": ["U"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "keywords": ["Splice"],
            "text": "Target creature gains shroud until end of turn and can't be blocked this turn. (A creature with shroud can't be the target of spells or abilities.)\nSplice onto Arcane—Return a blue creature you control to its owner's hand. (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)",
        }));
        let veil_card = card("Veil of Secrecy", "BOK", &blue, &veil);
        assert!(
            classify(&veil).contains(&CardClass::NinjutsuEnabler),
            "a spell that makes a creature unblockable is a Ninjutsu enabler"
        );
        assert!(roles(&veil).contains(&DeckRole::Evasion));
        assert_eq!(
            score15(p, &veil_card, &[], 0.0, 0.0).unwrap().class_bonus,
            0.65
        );
    }

    #[test]
    fn printed_toughness_reduction_is_removal_not_a_combat_trick() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        // Pull Under: "Target creature gets -5/-5 until end of turn."
        let pull_under = oracle_face(serde_json::json!({
            "name": "Pull Under",
            "manaCost": "{5}{B}",
            "manaValue": 6.0,
            "colors": ["B"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "text": "Target creature gets -5/-5 until end of turn.",
        }));
        let classes = classify(&pull_under);
        assert!(classes.contains(&CardClass::Removal));
        assert!(!classes.contains(&CardClass::CombatTrick));
        assert!(roles(&pull_under).contains(&DeckRole::Removal));
        assert_eq!(
            score15(
                p,
                &card("Pull Under", "CHK", &black, &pull_under),
                &[],
                0.0,
                0.0
            )
            .unwrap()
            .class_bonus,
            0.9
        );

        // Sickening Shoal: "Target creature gets -X/-X until end of turn."
        // The sign lives in the typed variable, so the reduction must read as
        // negative exactly like a fixed one.
        let shoal = oracle_face(serde_json::json!({
            "name": "Sickening Shoal",
            "manaCost": "{X}{B}{B}",
            "manaValue": 2.0,
            "colors": ["B"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "text": "You may exile a black card with mana value X from your hand rather than pay this spell's mana cost.\nTarget creature gets -X/-X until end of turn.",
        }));
        assert!(!has_buff_effect(&shoal), "a -X/-X is not a buff");
        assert!(classify(&shoal).contains(&CardClass::Removal));
        assert!(!classify(&shoal).contains(&CardClass::CombatTrick));
        assert!(roles(&shoal).contains(&DeckRole::Removal));
        assert_eq!(
            score15(
                p,
                &card("Sickening Shoal", "BOK", &black, &shoal),
                &[],
                0.0,
                0.0
            )
            .unwrap()
            .class_bonus,
            0.9
        );

        // Positive control: a real +2/+2 still reads as a buff and a trick.
        let trick = oracle_face(serde_json::json!({
            "name": "Test Arcane Trick",
            "manaCost": "{1}{G}",
            "manaValue": 2.0,
            "colors": ["G"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "text": "Target creature gets +2/+2 until end of turn.",
        }));
        assert!(has_buff_effect(&trick));
        assert!(classify(&trick).contains(&CardClass::CombatTrick));
    }

    #[test]
    fn printed_land_search_is_fixing_for_both_class_and_deck_role() {
        let p = DraftProfile::kamigawa_block();
        let green = vec!["G".to_string()];
        // Seek the Horizon's block printing is SOK, whose class table has no
        // fixing award: its contribution must come from the deck-need Fixing
        // role, not from a class bonus.
        let seek = oracle_face(serde_json::json!({
            "name": "Seek the Horizon",
            "manaCost": "{3}{G}",
            "manaValue": 4.0,
            "colors": ["G"],
            "type": "Sorcery",
            "types": ["Sorcery"],
            "subtypes": [],
            "text": "Search your library for up to three basic land cards, reveal them, put them into your hand, then shuffle.",
        }));
        assert!(is_mana_fixing(&seek, RoleReading::Full));
        assert!(classify(&seek).contains(&CardClass::ManaFixing));
        assert!(roles(&seek).contains(&DeckRole::Fixing));
        let seek_score = score15(
            p,
            &card("Seek the Horizon", "SOK", &green, &seek),
            &[],
            0.0,
            0.0,
        )
        .unwrap();
        assert_eq!(seek_score.class_bonus, 0.0, "SOK has no fixing award");
        assert_eq!(seek_score.deck_need, 2.0, "the Fixing role still applies");

        // Sakura-Tribe Elder is the printed CHK fixer whose class the set pays.
        let elder = sakura_tribe_elder();
        assert!(classify(&elder).contains(&CardClass::ManaFixing));
        assert_eq!(
            score15(
                p,
                &card("Sakura-Tribe Elder", "CHK", &green, &elder),
                &[],
                0.0,
                0.0
            )
            .unwrap()
            .class_bonus,
            0.25
        );

        // Kodama's Reach is also printed CHK fixing; its Arcane class outranks
        // the fixing bonus, so the Fixing role is what the deck-need term reads.
        let kodama = kodama_s_reach();
        assert!(classify(&kodama).contains(&CardClass::ManaFixing));
        assert!(roles(&kodama).contains(&DeckRole::Fixing));
        assert_eq!(
            score15(
                p,
                &card("Kodama's Reach", "CHK", &green, &kodama),
                &[],
                0.0,
                0.0
            )
            .unwrap()
            .class_bonus,
            0.45,
            "Arcane outranks the fixing award"
        );
    }

    #[test]
    fn printed_self_bounce_is_not_removal() {
        let p = DraftProfile::kamigawa_block();
        let green = vec!["G".to_string()];
        let haru = oracle_face(serde_json::json!({
            "name": "Haru-Onna",
            "manaCost": "{3}{G}",
            "manaValue": 4.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "2",
            "toughness": "1",
            "text": "When this creature enters, draw a card.\nWhenever you cast a Spirit or Arcane spell, you may return this creature to its owner's hand.",
        }));
        assert!(
            collect_face_effects(&haru).iter().any(|e| matches!(
                e,
                Effect::Bounce {
                    target: TargetFilter::SelfRef,
                    ..
                }
            )),
            "positive guard: the trigger is a typed self-bounce"
        );
        let classes = classify(&haru);
        assert!(!classes.contains(&CardClass::Removal));
        assert!(!roles(&haru).contains(&DeckRole::Removal));
        assert_eq!(
            score15(p, &card("Haru-Onna", "SOK", &green, &haru), &[], 0.0, 0.0)
                .unwrap()
                .class_bonus,
            0.5,
            "Spirit, not Removal"
        );
    }

    #[test]
    fn mirrodin_class_paid_theme_keeps_its_classic_suppression() {
        let p = DraftProfile::mirrodin_block();
        assert_eq!(
            p.adaptive_draft_policy.synergy_overlap,
            SynergyOverlap::Suppress
        );
        let none: Vec<String> = Vec::new();
        let blue = vec!["U".to_string()];
        let manta = oracle_face(serde_json::json!({
            "name": "Skyreach Manta",
            "manaCost": "{5}",
            "manaValue": 5.0,
            "type": "Artifact Creature — Fish",
            "types": ["Artifact", "Creature"],
            "subtypes": ["Fish"],
            "keywords": ["Sunburst", "Flying"],
            "power": "0",
            "toughness": "0",
            "text": "Sunburst (This creature enters with a +1/+1 counter on it for each color of mana spent to cast it.)\nFlying",
        }));
        let engineer = oracle_face(serde_json::json!({
            "name": "Vedalken Engineer",
            "manaCost": "{1}{U}",
            "manaValue": 2.0,
            "colors": ["U"],
            "type": "Creature — Vedalken Artificer",
            "types": ["Creature"],
            "subtypes": ["Vedalken", "Artificer"],
            "power": "1",
            "toughness": "1",
            "text": "{T}: Add two mana of any one color. Spend this mana only to cast artifact spells or activate abilities of artifacts.",
        }));
        let talisman = |name: &str, pair: &str| {
            oracle_face(serde_json::json!({
                "name": name,
                "manaCost": "{2}",
                "manaValue": 2.0,
                "type": "Artifact",
                "types": ["Artifact"],
                "text": format!("{{T}}: Add {{C}}.\n{{T}}: Add {pair}. This artifact deals 1 damage to you."),
            }))
        };
        let dominance = talisman("Talisman of Dominance", "{U} or {B}");
        let impulse = talisman("Talisman of Impulse", "{R} or {G}");
        let pool = [
            card("Vedalken Engineer", "MRD", &blue, &engineer),
            card("Talisman of Dominance", "MRD", &none, &dominance),
            card("Talisman of Impulse", "MRD", &none, &impulse),
        ];
        let manta_card = card("Skyreach Manta", "5DN", &none, &manta);

        // Positive guards: the pool demonstrates the Sunburst theme, the
        // artifact theme does not qualify, and the Manta is paid its
        // Sunburst class bonus.
        let signals = p.theme_signals(&pool);
        assert_eq!(signals[&Theme::SunburstFiveColor], 3);
        assert!(signals[&Theme::ArtifactAffinity] < 3);
        let scored = score15(p, &manta_card, &pool, 0.0, 0.0).unwrap();
        assert_eq!(scored.class_bonus, 1.0, "Sunburst");
        assert_eq!(
            scored.archetype_synergy, 0.0,
            "the Sunburst class bonus stands in for the Sunburst theme"
        );

        // The same pool under the excess policy would pay the 1.25 theme
        // strength beyond the 1.0 class bonus: the policy decides it.
        let mut excess = p.clone();
        excess.adaptive_draft_policy.synergy_overlap = SynergyOverlap::Excess;
        let scored = score15(&excess, &manta_card, &pool, 0.0, 0.0).unwrap();
        assert!((scored.archetype_synergy - 0.25).abs() < 1e-9);
    }

    #[test]
    fn removal_must_be_able_to_reach_an_opposing_creature() {
        let p = DraftProfile::kamigawa_block();
        let blue = vec!["U".to_string()];
        let red = vec!["R".to_string()];
        let twisted = oracle_face(serde_json::json!({
            "name": "Kami of Twisted Reflection",
            "manaCost": "{1}{U}{U}",
            "manaValue": 3.0,
            "colors": ["U"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "2",
            "toughness": "2",
            "text": "Sacrifice this creature: Return target creature you control to its owner's hand.",
        }));
        let vortex = oracle_face(serde_json::json!({
            "name": "Consuming Vortex",
            "manaCost": "{1}{U}",
            "manaValue": 2.0,
            "colors": ["U"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "keywords": ["Splice"],
            "text": "Return target creature to its owner's hand.\nSplice onto Arcane {3}{U} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)",
        }));
        let spike = oracle_face(serde_json::json!({
            "name": "Lava Spike",
            "manaCost": "{R}",
            "manaValue": 1.0,
            "colors": ["R"],
            "type": "Sorcery — Arcane",
            "types": ["Sorcery"],
            "subtypes": ["Arcane"],
            "text": "Lava Spike deals 3 damage to target player or planeswalker.",
        }));
        let ray = oracle_face(serde_json::json!({
            "name": "Glacial Ray",
            "manaCost": "{1}{R}",
            "manaValue": 2.0,
            "colors": ["R"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "keywords": ["Splice"],
            "text": "Glacial Ray deals 2 damage to any target.\nSplice onto Arcane {1}{R} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)",
        }));
        // Positive guards: each face exports the shared classifier's removal
        // shape, so only the recipient decides the outcome.
        for (name, f) in [
            ("Kami of Twisted Reflection", &twisted),
            ("Consuming Vortex", &vortex),
            ("Lava Spike", &spike),
            ("Glacial Ray", &ray),
        ] {
            assert!(
                collect_face_effects(f).into_iter().any(is_direct_removal),
                "{name}: positive guard"
            );
        }

        // Negatives: your own creature (CR 400.3) and a player or
        // planeswalker (CR 120.3a) are not an opposing creature.
        for (name, colors, f, bonus) in [
            ("Kami of Twisted Reflection", &blue, &twisted, 0.55),
            ("Lava Spike", &red, &spike, 0.45),
        ] {
            assert!(!classify(f).contains(&CardClass::Removal), "{name}");
            assert!(!roles(f).contains(&DeckRole::Removal), "{name}");
            assert!(
                classify_with(f, RoleReading::Classic).contains(&CardClass::Removal),
                "{name}: the classic reading keeps the shared classifier's answer"
            );
            let scored = first_pick(p, &card(name, "CHK", colors, f));
            assert_eq!(scored.class_bonus, bonus, "{name}: not the 0.9 Removal");
        }

        // Positives: the same effect families aimed at any creature.
        for (name, colors, f) in [
            ("Consuming Vortex", &blue, &vortex),
            ("Glacial Ray", &red, &ray),
        ] {
            assert!(classify(f).contains(&CardClass::Removal), "{name}");
            assert!(roles(f).contains(&DeckRole::Removal), "{name}");
            let scored = first_pick(p, &card(name, "CHK", colors, f));
            assert_eq!(scored.class_bonus, 0.9, "{name}");
        }

        // A removal-filled pool: own-creature bounce and player burn do not
        // consume the Removal need that real removal does.
        let ray_card = card("Glacial Ray", "CHK", &red, &ray);
        let need =
            |pool: &[ProfileCard<'_>]| score15(p, &ray_card, pool, 0.0, 0.0).unwrap().deck_need;
        let fake: Vec<_> = (0..5)
            .map(|i| {
                if i % 2 == 0 {
                    card("Lava Spike", "CHK", &red, &spike)
                } else {
                    card("Kami of Twisted Reflection", "CHK", &blue, &twisted)
                }
            })
            .collect();
        let real: Vec<_> = (0..5)
            .map(|_| card("Consuming Vortex", "CHK", &blue, &vortex))
            .collect();
        assert_eq!(need(&fake), 2.0, "the Removal role is still empty");
        assert_eq!(need(&real), 0.0, "five real removal spells meet the target");
    }

    #[test]
    fn printed_triggered_evasion_grant_is_a_ninjutsu_enabler() {
        let p = DraftProfile::kamigawa_block();
        let blue = vec!["U".to_string()];
        // Guardian of Solitude: "Whenever you cast a Spirit or Arcane spell,
        // target creature gains flying until end of turn."
        let guardian = oracle_face(serde_json::json!({
            "name": "Guardian of Solitude",
            "manaCost": "{1}{U}",
            "manaValue": 2.0,
            "colors": ["U"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "1",
            "toughness": "2",
            "text": "Whenever you cast a Spirit or Arcane spell, target creature gains flying until end of turn.",
        }));
        let classes = classify(&guardian);
        assert!(
            classes.contains(&CardClass::NinjutsuEnabler),
            "a triggered evasion grant is a Ninjutsu enabler"
        );
        assert!(!classes.contains(&CardClass::EvasiveThreat));
        assert!(roles(&guardian).contains(&DeckRole::Evasion));
        let scored = score15(
            p,
            &card("Guardian of Solitude", "CHK", &blue, &guardian),
            &[],
            0.0,
            0.0,
        )
        .unwrap();
        assert_eq!(scored.class_bonus, 0.55, "Spirit");
        assert_eq!(
            scored.deck_need, 2.0,
            "the Evasion role is empty in the pool"
        );
    }

    #[test]
    fn blocker_restrictions_are_evasion_in_both_paths() {
        let p = DraftProfile::kamigawa_block();
        let blue = vec!["U".to_string()];

        // Field of Reality: "Enchanted creature can't be blocked by Spirits."
        let field = oracle_face(serde_json::json!({
            "name": "Field of Reality",
            "manaCost": "{2}{U}",
            "manaValue": 3.0,
            "colors": ["U"],
            "type": "Enchantment — Aura",
            "types": ["Enchantment"],
            "subtypes": ["Aura"],
            "text": "Enchant creature\nEnchanted creature can't be blocked by Spirits.\n{1}{U}: Return this Aura to its owner's hand.",
        }));
        assert!(
            field
                .static_abilities
                .iter()
                .any(|s| matches!(s.mode, StaticMode::CantBeBlockedBy { .. })),
            "positive guard: the parser exports the blocker restriction"
        );
        let classes = classify(&field);
        assert!(classes.contains(&CardClass::NinjutsuEnabler));
        assert!(
            !classes.contains(&CardClass::EvasiveThreat),
            "an Aura is not itself an attacker"
        );
        assert!(
            !classes.contains(&CardClass::Removal),
            "returning the Aura itself is not removal"
        );
        assert!(roles(&field).contains(&DeckRole::Evasion));
        assert_eq!(
            score15(
                p,
                &card("Field of Reality", "CHK", &blue, &field),
                &[],
                0.0,
                0.0
            )
            .unwrap()
            .deck_need,
            2.0
        );

        // A self-scoped typed "can't be blocked except by ..." static is the
        // creature's own evasion.
        let mut guarded = face(vec![CoreType::Creature], &[]);
        guarded.static_abilities.push(
            StaticDefinition::new(StaticMode::CantBeBlockedExceptBy {
                kind: BlockExceptionKind::Quality(TargetFilter::Any),
            })
            .affected(TargetFilter::SelfRef),
        );
        assert!(classify(&guarded).contains(&CardClass::EvasiveThreat));

        // A granted typed blocker restriction is an enabler as it resolves.
        let grant = oracle_face(serde_json::json!({
            "name": "Test Blocker Restriction Grant",
            "manaCost": "{1}{U}",
            "manaValue": 2.0,
            "colors": ["U"],
            "type": "Instant",
            "types": ["Instant"],
            "text": "Target creature can't be blocked by Spirits until end of turn.",
        }));
        assert!(classify(&grant).contains(&CardClass::NinjutsuEnabler));
        assert!(roles(&grant).contains(&DeckRole::Evasion));
    }

    #[test]
    fn five_color_theme_reaches_three_signals_with_printed_fixers() {
        let p = DraftProfile::kamigawa_block();
        let green = vec!["G".to_string()];
        let kodama = kodama_s_reach();
        let elder = sakura_tribe_elder();
        let leafcaller = oracle_face(serde_json::json!({
            "name": "Orochi Leafcaller",
            "manaCost": "{G}",
            "manaValue": 1.0,
            "colors": ["G"],
            "type": "Creature — Snake Shaman",
            "types": ["Creature"],
            "subtypes": ["Snake", "Shaman"],
            "power": "1",
            "toughness": "1",
            "text": "{G}: Add one mana of any color.",
        }));
        // Structural fixing is a signal even without a named card bonus: the
        // selective list does not name Orochi Leafcaller.
        assert_eq!(p.bonus_for_name("Orochi Leafcaller"), 0.0);
        assert!(classify(&leafcaller).contains(&CardClass::ManaFixing));

        let two = vec![
            card("Kodama's Reach", "CHK", &green, &kodama),
            card("Sakura-Tribe Elder", "CHK", &green, &elder),
        ];
        assert_eq!(p.theme_signals(&two)[&Theme::FiveColorGoodstuff], 2);
        let probe = card("Kodama's Reach", "CHK", &green, &kodama);
        assert_eq!(
            score15(p, &probe, &two, 0.0, 0.0)
                .unwrap()
                .archetype_synergy,
            0.0,
            "two signals is below the policy's minimum of three"
        );

        let mut three = two.clone();
        three.push(card("Orochi Leafcaller", "CHK", &green, &leafcaller));
        assert_eq!(p.theme_signals(&three)[&Theme::FiveColorGoodstuff], 3);
        assert!(
            score15(p, &probe, &three, 0.0, 0.0)
                .unwrap()
                .archetype_synergy
                > 0.0,
            "the third printed fixer activates the five-color theme"
        );
    }

    #[test]
    fn unawarded_linked_class_does_not_discount_demonstrated_synergy() {
        let p = DraftProfile::kamigawa_block();
        let red = vec!["R".to_string()];
        let green = vec!["G".to_string()];

        // Ryusei, the Falling Star: a Dragon Spirit whose dies trigger deals 5
        // damage to each creature without flying. Its Removal class (0.9) beats
        // its Spirit class (0.55), so the Spirit bonus was never awarded.
        let ryusei = oracle_face(serde_json::json!({
            "name": "Ryusei, the Falling Star",
            "manaCost": "{5}{R}",
            "manaValue": 6.0,
            "colors": ["R"],
            "type": "Legendary Creature — Dragon Spirit",
            "types": ["Creature"],
            "subtypes": ["Dragon", "Spirit"],
            "power": "5",
            "toughness": "5",
            "keywords": ["Flying"],
            "text": "Flying\nWhen Ryusei dies, it deals 5 damage to each creature without flying.",
        }));
        let classes = classify(&ryusei);
        assert!(classes.contains(&CardClass::Removal));
        assert!(classes.contains(&CardClass::Spirit));

        // Three distinct quality red/green Spirits or Arcane spells demonstrate
        // the theme. Reported by the production Oracle parser.
        let burr = oracle_face(serde_json::json!({
            "name": "Burr Grafter",
            "manaCost": "{3}{G}",
            "manaValue": 4.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "2",
            "toughness": "2",
            "keywords": ["Soulshift"],
            "text": "Sacrifice this creature: Target creature gets +2/+2 until end of turn.\nSoulshift 3 (When this creature dies, you may return target Spirit card with mana value 3 or less from your graveyard to your hand.)",
        }));
        let kami = oracle_face(serde_json::json!({
            "name": "Kami of the Hunt",
            "manaCost": "{2}{G}",
            "manaValue": 3.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "2",
            "toughness": "2",
            "text": "Whenever you cast a Spirit or Arcane spell, this creature gets +1/+1 until end of turn.",
        }));
        let ray = oracle_face(serde_json::json!({
            "name": "Glacial Ray",
            "manaCost": "{1}{R}",
            "manaValue": 2.0,
            "colors": ["R"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "keywords": ["Splice"],
            "text": "Glacial Ray deals 2 damage to any target.\nSplice onto Arcane {1}{R} (As you cast an Arcane spell, you may reveal this card from your hand and pay its splice cost. If you do, add this card's effects to that spell.)",
        }));
        let pool = vec![
            card("Burr Grafter", "CHK", &green, &burr),
            card("Kami of the Hunt", "CHK", &green, &kami),
            card("Glacial Ray", "CHK", &red, &ray),
        ];
        assert_eq!(p.theme_signals(&pool)[&Theme::RedGreenSpiritsArcane], 3);

        let s = score15(
            p,
            &card("Ryusei, the Falling Star", "CHK", &red, &ryusei),
            &pool,
            0.0,
            0.0,
        )
        .unwrap();
        assert_eq!(s.class_bonus, 0.9, "Removal outranks Spirit");
        assert_eq!(
            s.archetype_synergy, 1.25,
            "the awarded Removal class is unlinked, so no Spirit bonus is deducted"
        );
    }

    #[test]
    fn aggression_theme_links_the_roles_it_admits() {
        assert_eq!(
            Theme::RedBlackAggression.linked_classes(),
            &[CardClass::Removal, CardClass::EvasiveThreat]
        );
    }

    /// The whole additive score of an empty-pool first pick with a zero base.
    fn first_pick(p: &DraftProfile, c: &ProfileCard<'_>) -> ProfileScore {
        score15(p, c, &[], 0.0, 0.0).unwrap()
    }

    #[test]
    fn mirrodin_keeps_its_classic_scores_for_printed_shrink_and_land_search() {
        let p = DraftProfile::mirrodin_block();
        let black = vec!["B".to_string()];
        let green = vec!["G".to_string()];
        // Echoing Decay (DST): "Target creature and all other creatures with
        // the same name as that creature get -2/-2 until end of turn."
        let decay = oracle_face(serde_json::json!({
            "name": "Echoing Decay",
            "manaCost": "{1}{B}",
            "manaValue": 2.0,
            "colors": ["B"],
            "type": "Instant",
            "types": ["Instant"],
            "text": "Target creature and all other creatures with the same name as that creature get -2/-2 until end of turn.",
        }));
        assert!(
            collect_face_effects(&decay)
                .iter()
                .any(|e| matches!(e, Effect::PumpAll { .. })),
            "positive guard: the export carries the filtered PumpAll"
        );
        assert!(
            classify(&decay).contains(&CardClass::Removal),
            "positive guard: the full reading counts the shrink as removal"
        );
        // Sylvan Scrying (MRD): "Search your library for a land card, reveal
        // it, put it into your hand, then shuffle."
        let scrying = oracle_face(serde_json::json!({
            "name": "Sylvan Scrying",
            "manaCost": "{1}{G}",
            "manaValue": 2.0,
            "colors": ["G"],
            "type": "Sorcery",
            "types": ["Sorcery"],
            "text": "Search your library for a land card, reveal it, put it into your hand, then shuffle.",
        }));
        assert!(
            roles(&scrying).contains(&DeckRole::Fixing),
            "positive guard: the full reading counts the land search as fixing"
        );

        // Mirrodin's sets keep the classic reading, so both cards keep their
        // pre-Kamigawa scores: no class and no deck role, so a zero-base
        // first pick totals exactly its named card bonus.
        for (name, set, colors, f) in [
            ("Echoing Decay", "DST", &black, &decay),
            ("Sylvan Scrying", "MRD", &green, &scrying),
        ] {
            assert_eq!(p.reading(set), RoleReading::Classic);
            assert!(classify_with(f, RoleReading::Classic).is_empty(), "{name}");
            assert!(roles_with(f, RoleReading::Classic).is_empty(), "{name}");
            assert_eq!(
                first_pick(p, &card(name, set, colors, f)),
                ProfileScore {
                    total: p.bonus_for_name(name),
                    card_bonus: p.bonus_for_name(name),
                    ..ProfileScore::default()
                },
                "{name} keeps its pre-change Mirrodin score"
            );
        }
    }

    #[test]
    fn channel_bonus_stacks_on_the_spirit_bonus_of_a_printed_sok_card() {
        let p = DraftProfile::kamigawa_block();
        let green = vec!["G".to_string()];
        // Shinen of Life's Roar (SOK), verbatim Oracle text.
        let shinen = oracle_face(serde_json::json!({
            "name": "Shinen of Life's Roar",
            "manaCost": "{1}{G}",
            "manaValue": 2.0,
            "colors": ["G"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "1",
            "toughness": "2",
            "text": "All creatures able to block this creature do so.\nChannel — {2}{G}{G}, Discard this card: All creatures able to block target creature this turn do so.",
        }));
        let classes = classify(&shinen);
        assert!(classes.contains(&CardClass::Spirit));
        assert!(classes.contains(&CardClass::Channel));

        let sok = &p.sets["SOK"];
        assert_eq!(sok.class_bonus[&CardClass::Spirit], 0.5);
        assert_eq!(sok.class_bonus[&CardClass::Channel], 0.45);
        let scored = first_pick(p, &card("Shinen of Life's Roar", "SOK", &green, &shinen));
        assert!(
            (scored.class_bonus - 0.95).abs() < 1e-9,
            "Spirit 0.5 plus Channel 0.45, not the 0.5 maximum: {}",
            scored.class_bonus
        );

        // The Spirit themes discount only the Spirit bonus they would
        // duplicate, never the Channel bonus.
        let award = ClassAward::new(sok, &classes);
        assert_eq!(award.paid_through(&[CardClass::Spirit]), 0.5);
        assert_eq!(award.paid_through(&[CardClass::Ninja]), 0.0);
    }

    #[test]
    fn printed_landwalk_is_evasion_in_the_full_reading() {
        let p = DraftProfile::kamigawa_block();
        let green = vec!["G".to_string()];
        let red = vec!["R".to_string()];
        let messenger = oracle_face(serde_json::json!({
            "name": "Jukai Messenger",
            "manaCost": "{G}",
            "manaValue": 1.0,
            "colors": ["G"],
            "type": "Creature — Human Monk",
            "types": ["Creature"],
            "subtypes": ["Human", "Monk"],
            "power": "1",
            "toughness": "1",
            "keywords": ["Forestwalk"],
            "text": "Forestwalk (This creature can't be blocked as long as defending player controls a Forest.)",
        }));
        let bruiser = oracle_face(serde_json::json!({
            "name": "Sokenzan Bruiser",
            "manaCost": "{4}{R}",
            "manaValue": 5.0,
            "colors": ["R"],
            "type": "Creature — Ogre Warrior",
            "types": ["Creature"],
            "subtypes": ["Ogre", "Warrior"],
            "power": "3",
            "toughness": "3",
            "keywords": ["Mountainwalk"],
            "text": "Mountainwalk (This creature can't be blocked as long as defending player controls a Mountain.)",
        }));
        let ayumi = oracle_face(serde_json::json!({
            "name": "Ayumi, the Last Visitor",
            "manaCost": "{3}{G}{G}",
            "manaValue": 5.0,
            "colors": ["G"],
            "type": "Legendary Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "supertypes": ["Legendary"],
            "power": "7",
            "toughness": "3",
            "keywords": ["Legendary landwalk"],
            "text": "Legendary landwalk (This creature can't be blocked as long as defending player controls a legendary land.)",
        }));
        for (name, f) in [
            ("Jukai Messenger", &messenger),
            ("Sokenzan Bruiser", &bruiser),
            ("Ayumi, the Last Visitor", &ayumi),
        ] {
            assert!(
                f.keywords.iter().any(|k| matches!(k, Keyword::Landwalk(_))),
                "positive guard: {name} exports a typed Landwalk"
            );
            assert!(classify(f).contains(&CardClass::EvasiveThreat), "{name}");
            assert!(roles(f).contains(&DeckRole::Evasion), "{name}");
            assert!(
                !classify_with(f, RoleReading::Classic).contains(&CardClass::EvasiveThreat),
                "{name}: the classic reading is unchanged"
            );
        }
        // The one-mana forestwalker is a cheap way to connect for Ninjutsu.
        assert!(classify(&messenger).contains(&CardClass::NinjutsuEnabler));
        assert!(!classify(&bruiser).contains(&CardClass::NinjutsuEnabler));
        // A 3-power landwalker is an evasive threat for the threat need.
        assert!(p.is_threat(&card("Sokenzan Bruiser", "CHK", &red, &bruiser)));
        assert_eq!(
            first_pick(p, &card("Jukai Messenger", "CHK", &green, &messenger)).class_bonus,
            0.55,
            "evasive threat"
        );
    }

    #[test]
    fn self_granted_evasion_is_own_evasion_not_an_enabler_for_another_attacker() {
        let p = DraftProfile::kamigawa_block();
        let white = vec!["W".to_string()];
        let blue = vec!["U".to_string()];
        // Kami of Tattered Shoji (BOK): the trigger's grant lands on itself.
        let shoji = oracle_face(serde_json::json!({
            "name": "Kami of Tattered Shoji",
            "manaCost": "{4}{W}",
            "manaValue": 5.0,
            "colors": ["W"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "2",
            "toughness": "5",
            "text": "Whenever you cast a Spirit or Arcane spell, this creature gains flying until end of turn.",
        }));
        let grant = collect_face_effects(&shoji)
            .into_iter()
            .find_map(|e| match e {
                Effect::GenericEffect {
                    static_abilities,
                    target,
                    ..
                } => Some((target.clone(), static_abilities[0].affected.clone())),
                _ => None,
            })
            .expect("positive guard: the trigger exports a GenericEffect grant");
        assert_eq!(grant, (None, Some(TargetFilter::SelfRef)));
        assert!(!grants_evasion(&shoji, RoleReading::Full));
        // CR 702.9a: the conditional flying is the Kami's own evasion.
        assert!(is_evasive(&shoji, RoleReading::Full));
        assert!(
            !is_evasive(&shoji, RoleReading::Classic),
            "the classic reading keeps printed keywords only"
        );
        let classes = classify(&shoji);
        assert!(classes.contains(&CardClass::EvasiveThreat));
        assert!(
            !classes.contains(&CardClass::NinjutsuEnabler),
            "five mana is over the enabler mana-value cap"
        );
        assert!(roles(&shoji).contains(&DeckRole::Evasion));
        let scored = first_pick(p, &card("Kami of Tattered Shoji", "BOK", &white, &shoji));
        assert_eq!(
            scored.class_bonus, 0.55,
            "evasive threat outranks the 0.4 Spirit award, not the 0.65 enabler"
        );

        // Positive control: Guardian of Solitude (CHK) grants flying to a
        // target creature, so it does enable another attacker.
        let guardian = oracle_face(serde_json::json!({
            "name": "Guardian of Solitude",
            "manaCost": "{1}{U}",
            "manaValue": 2.0,
            "colors": ["U"],
            "type": "Creature — Spirit",
            "types": ["Creature"],
            "subtypes": ["Spirit"],
            "power": "1",
            "toughness": "2",
            "text": "Whenever you cast a Spirit or Arcane spell, target creature gains flying until end of turn.",
        }));
        assert!(grants_evasion(&guardian, RoleReading::Full));
        assert!(classify(&guardian).contains(&CardClass::NinjutsuEnabler));
        assert_eq!(
            first_pick(p, &card("Guardian of Solitude", "BOK", &blue, &guardian)).class_bonus,
            0.65,
            "the same face under BOK earns the enabler award"
        );
    }

    #[test]
    fn printed_removal_auras_are_removal_through_their_statics() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        let white = vec!["W".to_string()];
        // Kagemaro's Clutch (SOK).
        let clutch = oracle_face(serde_json::json!({
            "name": "Kagemaro's Clutch",
            "manaCost": "{3}{B}",
            "manaValue": 4.0,
            "colors": ["B"],
            "type": "Enchantment — Aura",
            "types": ["Enchantment"],
            "subtypes": ["Aura"],
            "keywords": ["Enchant"],
            "text": "Enchant creature\nEnchanted creature gets -X/-X, where X is the number of cards in your hand.",
        }));
        assert!(
            clutch.static_abilities.iter().any(|s| s
                .modifications
                .iter()
                .any(|m| matches!(m, ContinuousModification::AddDynamicToughness { .. }))),
            "positive guard: the static exports AddDynamicToughness"
        );
        // Cage of Hands (CHK).
        let cage = oracle_face(serde_json::json!({
            "name": "Cage of Hands",
            "manaCost": "{2}{W}",
            "manaValue": 3.0,
            "colors": ["W"],
            "type": "Enchantment — Aura",
            "types": ["Enchantment"],
            "subtypes": ["Aura"],
            "keywords": ["Enchant"],
            "text": "Enchant creature\nEnchanted creature can't attack or block.\n{1}{W}: Return this Aura to its owner's hand.",
        }));
        assert!(
            cage.static_abilities.iter().any(|s| matches!(
                s.mode,
                StaticMode::CantAttack | StaticMode::CantAttackOrBlock
            )),
            "positive guard: the static exports the attack restriction"
        );
        // The Aura's own return ability is a self-bounce, not the removal.
        assert!(!collect_face_effects(&cage)
            .into_iter()
            .any(reaches_opposing_creature));

        for (name, set, colors, f, bonus) in [
            ("Kagemaro's Clutch", "SOK", &black, &clutch, 0.85),
            ("Cage of Hands", "CHK", &white, &cage, 0.9),
        ] {
            assert!(has_removal_static(f), "{name}");
            assert!(classify(f).contains(&CardClass::Removal), "{name}");
            assert!(roles(f).contains(&DeckRole::Removal), "{name}");
            let scored = first_pick(p, &card(name, set, colors, f));
            assert_eq!(scored.class_bonus, bonus, "{name}");
            assert_eq!(scored.deck_need, 2.0, "{name}: removal need is empty");
        }

        // A static that only stops blocking, or that lowers the source's own
        // toughness, removes nothing.
        let mut no_block = face(vec![CoreType::Enchantment], &["Aura"]);
        no_block
            .static_abilities
            .push(StaticDefinition::new(StaticMode::CantBlock).affected(TargetFilter::Any));
        assert!(!has_removal_static(&no_block));
        let mut self_shrink = face(vec![CoreType::Creature], &[]);
        self_shrink.static_abilities.push(
            StaticDefinition::new(StaticMode::Continuous)
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::AddToughness { value: -1 }]),
        );
        assert!(!has_removal_static(&self_shrink));
    }

    #[test]
    fn gifts_ungiven_library_to_graveyard_is_not_removal() {
        let p = DraftProfile::kamigawa_block();
        let blue = vec!["U".to_string()];
        // Gifts Ungiven: "Search your library for up to four cards with different
        // names and reveal them. Target opponent chooses two of those cards. Put
        // the chosen cards into your graveyard and the rest into your hand."
        // The parser emits a ChooseFromZone followed by a ChangeZone with
        // origin: None (library) -> Graveyard. This must not count as
        // battlefield removal.
        let gifts = oracle_face(serde_json::json!({
            "name": "Gifts Ungiven",
            "manaCost": "{3}{U}",
            "manaValue": 4.0,
            "colors": ["U"],
            "type": "Instant",
            "types": ["Instant"],
            "text": "Search your library for up to four cards with different names and reveal them. Target opponent chooses two of those cards. Put the chosen cards into your graveyard and the rest into your hand.",
        }));
        // Positive guard: the effect chain contains a ChangeZone with origin None.
        let has_lib_to_gy = collect_face_effects(&gifts).iter().any(|e| {
            matches!(
                e,
                Effect::ChangeZone {
                    origin: None,
                    destination: Zone::Graveyard,
                    ..
                }
            )
        });
        assert!(
            has_lib_to_gy,
            "positive guard: Gifts exports library->graveyard move"
        );
        // Full reading: not removal because origin is not Battlefield.
        assert!(!classify(&gifts).contains(&CardClass::Removal));
        assert!(!roles(&gifts).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Gifts Ungiven", "CHK", &blue, &gifts));
        assert_eq!(scored.class_bonus, 0.0, "Gifts Ungiven is not removal");

        // Positive control: Rend Flesh (CHK) — "Destroy target non-Spirit creature."
        // is removal under the full reading.
        let rend_flesh = oracle_face(serde_json::json!({
            "name": "Rend Flesh",
            "manaCost": "{2}{B}",
            "manaValue": 3.0,
            "colors": ["B"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "text": "Destroy target non-Spirit creature.",
        }));
        assert!(classify(&rend_flesh).contains(&CardClass::Removal));
        assert!(roles(&rend_flesh).contains(&DeckRole::Removal));
        let rf_score = first_pick(
            p,
            &card("Rend Flesh", "CHK", &["B".to_string()], &rend_flesh),
        );
        assert_eq!(rf_score.class_bonus, 0.9, "Rend Flesh is removal");
        assert_eq!(rf_score.deck_need, 2.0, "removal need is empty");
    }

    #[test]
    fn horobis_whisper_conditional_destroy_is_removal() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        // Horobi's Whisper (CHK): "If you control a Swamp, destroy target
        // nonblack creature." This is conditional removal — the condition is
        // checked at resolution (CR 608.2b). The parser emits a Destroy effect
        // with a condition; we classify it as removal because the effect shape
        // is removal when the condition is met.
        let whisper = oracle_face(serde_json::json!({
            "name": "Horobi's Whisper",
            "manaCost": "{2}{B}",
            "manaValue": 3.0,
            "colors": ["B"],
            "type": "Instant — Arcane",
            "types": ["Instant"],
            "subtypes": ["Arcane"],
            "text": "If you control a Swamp, destroy target nonblack creature.\nSplice onto Arcane—Exile four cards from your graveyard.",
        }));
        // Full reading: counts as removal (conditional destroy).
        assert!(classify(&whisper).contains(&CardClass::Removal));
        assert!(roles(&whisper).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Horobi's Whisper", "CHK", &black, &whisper));
        assert_eq!(scored.class_bonus, 0.9, "CHK removal bonus");
        assert_eq!(scored.deck_need, 2.0, "removal need is empty");

        // Negative control: Sakura-Tribe Elder's "Sacrifice this creature" is a
        // self-sacrifice *cost* (SelfRef), NOT an effect. Costs are not in the
        // effect chain collected by `collect_face_effects`, so the Elder has no
        // Sacrifice effect and is not classified as removal.
        let elder = sakura_tribe_elder();
        let has_self_sacrifice_cost = elder.abilities.iter().any(|a| {
            matches!(
                a.cost.as_ref(),
                Some(engine::types::ability::AbilityCost::Sacrifice(cost))
                    if matches!(cost.target, TargetFilter::SelfRef)
            )
        });
        assert!(
            has_self_sacrifice_cost,
            "positive guard: Elder has self-sacrifice cost"
        );
        assert!(!classify(&elder).contains(&CardClass::Removal));
        assert!(!roles(&elder).contains(&DeckRole::Removal));
        let elder_score = first_pick(
            p,
            &card("Sakura-Tribe Elder", "CHK", &["G".to_string()], &elder),
        );
        assert_eq!(
            elder_score.class_bonus, 0.25,
            "Elder gets fixing bonus, not removal"
        );
    }

    #[test]
    fn leveler_library_exile_is_not_removal() {
        let p = DraftProfile::kamigawa_block();
        // Leveler: "When this creature enters, exile all cards from your library."
        // This is a bulk exile from library, not battlefield — not removal.
        let leveler = oracle_face(serde_json::json!({
            "name": "Leveler",
            "manaCost": "{3}",
            "manaValue": 3.0,
            "colors": [],
            "type": "Artifact Creature — Construct",
            "types": ["Artifact", "Creature"],
            "subtypes": ["Construct"],
            "power": "10",
            "toughness": "10",
            "text": "When this creature enters, exile all cards from your library.",
        }));
        // Classic reading: should NOT be removal because origin is library, not battlefield.
        assert!(!classify(&leveler).contains(&CardClass::Removal));
        assert!(!roles(&leveler).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Leveler", "CHK", &[], &leveler));
        assert_eq!(scored.class_bonus, 0.0, "Leveler is not removal");
    }

    #[test]
    fn reciprocate_battlefield_exile_with_typed_target_is_removal() {
        let p = DraftProfile::kamigawa_block();
        let white = vec!["W".to_string()];
        // Reciprocate (CHK): "Exile target creature that dealt damage to you this
        // turn." The parser emits ChangeZone with origin None and a typed
        // creature target. Since the target filter implies battlefield, this is
        // removal.
        let reciprocate = oracle_face(serde_json::json!({
            "name": "Reciprocate",
            "manaCost": "{2}{W}",
            "manaValue": 3.0,
            "colors": ["W"],
            "type": "Instant",
            "types": ["Instant"],
            "text": "Exile target creature that dealt damage to you this turn.",
        }));
        // Positive guard: exports ChangeZone to Exile with typed creature target.
        let has_exile = collect_face_effects(&reciprocate).iter().any(|e| {
            matches!(
                e,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(_),
                    ..
                }
            )
        });
        assert!(has_exile, "Reciprocate exports battlefield exile");
        assert!(classify(&reciprocate).contains(&CardClass::Removal));
        assert!(roles(&reciprocate).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Reciprocate", "CHK", &white, &reciprocate));
        assert_eq!(scored.class_bonus, 0.9, "Reciprocate is removal");
    }

    #[test]
    fn final_judgment_mass_exile_is_removal() {
        let p = DraftProfile::kamigawa_block();
        let white = vec!["W".to_string()];
        // Final Judgment (BOK): "Exile all creatures." Uses ChangeZoneAll with
        // destination Exile. Must be classified as removal.
        let final_judgment = oracle_face(serde_json::json!({
            "name": "Final Judgment",
            "manaCost": "{4}{W}{W}",
            "manaValue": 6.0,
            "colors": ["W"],
            "type": "Sorcery",
            "types": ["Sorcery"],
            "text": "Exile all creatures.",
        }));
        // Positive guard: exports ChangeZoneAll to Exile.
        let has_mass_exile = collect_face_effects(&final_judgment).iter().any(|e| {
            matches!(
                e,
                Effect::ChangeZoneAll {
                    destination: Zone::Exile,
                    ..
                }
            )
        });
        assert!(has_mass_exile, "Final Judgment exports mass exile");
        assert!(classify(&final_judgment).contains(&CardClass::Removal));
        assert!(roles(&final_judgment).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Final Judgment", "BOK", &white, &final_judgment));
        assert_eq!(scored.class_bonus, 0.9, "Final Judgment is removal");
    }

    #[test]
    fn eradicate_battlefield_exile_is_removal_off_board_is_not() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        // Eradicate (BOK): "Exile target nonblack creature. Search its
        // controller's graveyard, hand, and library for all cards with the same
        // name as that creature and exile them. Then that player shuffles."
        // The first exile (battlefield) is removal; the subsequent exiles from
        // other zones are not. The classifier only sees the first effect for
        // the removal class.
        let eradicate = oracle_face(serde_json::json!({
            "name": "Eradicate",
            "manaCost": "{3}{B}",
            "manaValue": 4.0,
            "colors": ["B"],
            "type": "Instant",
            "types": ["Instant"],
            "text": "Exile target nonblack creature. Search its controller's graveyard, hand, and library for all cards with the same name as that creature and exile them. Then that player shuffles.",
        }));
        // Positive guard: exports at least one ChangeZone to Exile with typed target.
        let has_battlefield_exile = collect_face_effects(&eradicate).iter().any(|e| {
            matches!(
                e,
                Effect::ChangeZone {
                    destination: Zone::Exile,
                    target: TargetFilter::Typed(_),
                    ..
                }
            )
        });
        assert!(has_battlefield_exile, "Eradicate exports battlefield exile");
        // Classified as removal because the first effect reaches an opposing creature.
        assert!(classify(&eradicate).contains(&CardClass::Removal));
        assert!(roles(&eradicate).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Eradicate", "BOK", &black, &eradicate));
        assert_eq!(scored.class_bonus, 0.9, "Eradicate is removal");
    }

    #[test]
    fn oni_possession_self_sacrifice_is_not_removal() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        // Oni Possession (CHK): "Enchant creature. Enchanted creature gets
        // +3/+1. At the beginning of your upkeep, sacrifice a creature."
        // The sacrifice trigger forces the controller to sacrifice their own
        // creature. The Sacrifice effect has a typed creature target with no
        // controller restriction (defaults to ability's controller = you). This
        // must NOT be classified as opponent-targeting removal.
        let oni_possession = oracle_face(serde_json::json!({
            "name": "Oni Possession",
            "manaCost": "{3}{B}",
            "manaValue": 4.0,
            "colors": ["B"],
            "type": "Enchantment — Aura",
            "types": ["Enchantment"],
            "subtypes": ["Aura"],
            "text": "Enchant creature\nEnchanted creature gets +3/+1.\nAt the beginning of your upkeep, sacrifice a creature.",
        }));
        // Positive guard: exports a Sacrifice effect with typed creature target.
        let has_sacrifice = collect_face_effects(&oni_possession).iter().any(|e| {
            matches!(
                e,
                Effect::Sacrifice {
                    target: TargetFilter::Typed(_),
                    ..
                }
            )
        });
        assert!(has_sacrifice, "Oni Possession exports sacrifice effect");
        // But the target has no explicit opponent scope, so it's not removal.
        assert!(!classify(&oni_possession).contains(&CardClass::Removal));
        assert!(!roles(&oni_possession).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Oni Possession", "CHK", &black, &oni_possession));
        // Oni Possession is a Spirit (no, it's an Aura) — it gets no class bonus.
        assert_eq!(scored.class_bonus, 0.0, "Oni Possession is not removal");
    }

    #[test]
    fn painwracker_oni_conditional_self_sacrifice_is_not_removal() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        // Painwracker Oni (CHK): "At the beginning of your upkeep, if you
        // control an Ogre, sacrifice a creature." Same as Oni Possession — the
        // sacrifice targets the controller's own creatures.
        let painwracker = oracle_face(serde_json::json!({
            "name": "Painwracker Oni",
            "manaCost": "{4}{B}",
            "manaValue": 5.0,
            "colors": ["B"],
            "type": "Creature — Demon Spirit",
            "types": ["Creature"],
            "subtypes": ["Demon", "Spirit"],
            "power": "5",
            "toughness": "5",
            "text": "At the beginning of your upkeep, if you control an Ogre, sacrifice a creature.",
        }));
        let has_sacrifice = collect_face_effects(&painwracker).iter().any(|e| {
            matches!(
                e,
                Effect::Sacrifice {
                    target: TargetFilter::Typed(_),
                    ..
                }
            )
        });
        assert!(has_sacrifice, "Painwracker Oni exports sacrifice effect");
        assert!(!classify(&painwracker).contains(&CardClass::Removal));
        assert!(!roles(&painwracker).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Painwracker Oni", "CHK", &black, &painwracker));
        // It's a Spirit, so it gets the Spirit class bonus (0.55).
        assert_eq!(
            scored.class_bonus, 0.55,
            "Painwracker Oni gets Spirit bonus"
        );
    }

    #[test]
    fn crack_the_earth_each_player_sacrifices_is_removal() {
        let p = DraftProfile::kamigawa_block();
        let red = vec!["R".to_string()];
        // Crack the Earth (BOK): "Each player sacrifices a permanent of their
        // choice." This can reach an opponent's creature and is removal.
        // The effect targets each player, and each player sacrifices their
        // choice. The Sacrifice effect's target filter has an explicit player
        // scope (TargetPlayer/EachPlayer) that resolves to opponents.
        let crack = oracle_face(serde_json::json!({
            "name": "Crack the Earth",
            "manaCost": "{1}{R}",
            "manaValue": 2.0,
            "colors": ["R"],
            "type": "Sorcery",
            "types": ["Sorcery"],
            "text": "Each player sacrifices a permanent of their choice.",
        }));
        // Positive guard: exports a Sacrifice effect with a target that can be opposing.
        let has_sacrifice = collect_face_effects(&crack).iter().any(
            |e| matches!(e, Effect::Sacrifice { target, .. } if may_be_opposing_creature_for_sacrifice(target)),
        );
        eprintln!("Crack effects: {:?}", collect_face_effects(&crack));
        eprintln!("Crack has_sacrifice: {}", has_sacrifice);
        assert!(has_sacrifice, "Crack the Earth exports opposing sacrifice");
        assert!(classify(&crack).contains(&CardClass::Removal));
        assert!(roles(&crack).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Crack the Earth", "BOK", &red, &crack));
        assert_eq!(scored.class_bonus, 0.9, "Crack the Earth is removal");
    }

    #[test]
    fn exile_into_darkness_edict_is_removal() {
        let p = DraftProfile::kamigawa_block();
        let black = vec!["B".to_string()];
        // Exile into Darkness (SOK): "Target player sacrifices a creature of
        // their choice with mana value 3 or less." This is an edict — a
        // sacrifice effect targeting an opponent. The parser emits a Sacrifice
        // effect with a target that can reach an opposing creature.
        let exile = oracle_face(serde_json::json!({
            "name": "Exile into Darkness",
            "manaCost": "{3}{B}{B}",
            "manaValue": 5.0,
            "colors": ["B"],
            "type": "Sorcery",
            "types": ["Sorcery"],
            "text": "Target player sacrifices a creature of their choice with mana value 3 or less.\nAt the beginning of your upkeep, if you have more cards in hand than each opponent, you may return this card from your graveyard to your hand.",
        }));
        // Positive guard: exports a Sacrifice effect with a target that can be opposing.
        let has_sacrifice = collect_face_effects(&exile).iter().any(
            |e| matches!(e, Effect::Sacrifice { target, .. } if may_be_opposing_creature_for_sacrifice(target)),
        );
        assert!(
            has_sacrifice,
            "Exile into Darkness exports opposing sacrifice"
        );
        assert!(classify(&exile).contains(&CardClass::Removal));
        assert!(roles(&exile).contains(&DeckRole::Removal));
        let scored = first_pick(p, &card("Exile into Darkness", "SOK", &black, &exile));
        assert_eq!(scored.class_bonus, 0.85, "Exile into Darkness is removal");
    }
}

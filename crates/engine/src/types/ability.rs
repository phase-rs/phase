use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::card_type::{CardType, CoreType, Supertype};
use super::counter::{CounterMatch, CounterType};
use super::events::BendingType;
use super::game_state::{DistributionUnit, RetargetScope};
use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};
use super::mana::{ManaColor, ManaCost, ManaType};
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::replacements::ReplacementEvent;
use super::statics::StaticMode;
use super::triggers::TriggerMode;
use super::zones::Zone;
use crate::types::events::PlayerActionKind;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// CR 700.2: Who makes a choice during an effect's resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Chooser {
    /// The controller of the spell/ability makes the choice.
    #[default]
    Controller,
    /// An opponent of the controller makes the choice (CR 700.2).
    /// In 2-player, the single opponent. In multiplayer, controller chooses which opponent.
    Opponent,
}

/// CR 101.4: Who selects permanents in a multi-player category choice effect
/// (e.g., Cataclysm, Tragic Arrogance). Determines whether each player independently
/// chooses which of their permanents to keep, or the spell's controller decides for everyone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CategoryChooserScope {
    /// Each player chooses their own permanents in APNAP order (Cataclysm, Cataclysmic Gearhulk).
    #[default]
    EachPlayerSelf,
    /// The controller of the spell/ability chooses for all players (Tragic Arrogance).
    ControllerForAll,
}

/// Additional selection constraints for tracked-set card picks during resolution.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChooseFromZoneConstraint {
    /// The chosen cards must admit an injective assignment to distinct card types
    /// from the listed categories.
    DistinctCardTypes { categories: Vec<CoreType> },
}

/// Selection constraint applied to multi-card library searches at the
/// `WaitingFor::SearchChoice` step. Lives one abstraction up from `count` /
/// `up_to` so the engine can reject illegal combinations and the AI can
/// prune its candidate space without bespoke per-card knowledge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SearchSelectionConstraint {
    /// CR 107.1c: No restriction beyond `count` (and `up_to`).
    #[default]
    None,
    /// CR 608.2c: The spell's printed text dictates "with different names" —
    /// no two chosen cards may share a name. Used by Gifts Ungiven and the
    /// "for X cards with different names" tutor class.
    DistinctNames,
}

/// CR 608.2d: Who may choose to perform an optional effect during resolution.
/// Used with `AbilityDefinition::optional_for` to route the "you may" prompt to opponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpponentMayScope {
    /// "any opponent may" — each opponent in APNAP order gets the chance; first accept wins.
    AnyOpponent,
}

/// What kind of named choice the player must make at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChoiceType {
    CreatureType,
    Color,
    OddOrEven,
    BasicLandType,
    CardType,
    CardName,
    /// "Choose a number between X and Y" — generates string options "0", "1", ..., "Y".
    NumberRange {
        min: u8,
        max: u8,
    },
    /// "Choose left or right", "choose fame or fortune" — options come from the parser.
    Labeled {
        options: Vec<String>,
    },
    /// "Choose a land type" — includes basic + common nonbasic land types.
    LandType,
    /// "Choose an opponent" — selects one opponent player (CR 800.4a).
    Opponent,
    /// "Choose a player" — selects any player in the game.
    Player,
    /// "Choose two colors" — selects two distinct mana colors.
    TwoColors,
    /// "Choose a word" — names any English word (Un-set and silver-border cards).
    Word,
    /// "Choose an artist" — selects a Magic card artist name.
    Artist,
}

/// The five basic land types (CR 305.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BasicLandType {
    Plains,
    Island,
    Swamp,
    Mountain,
    Forest,
}

impl BasicLandType {
    /// The corresponding mana color for this basic land type.
    pub fn mana_color(self) -> ManaColor {
        match self {
            Self::Plains => ManaColor::White,
            Self::Island => ManaColor::Blue,
            Self::Swamp => ManaColor::Black,
            Self::Mountain => ManaColor::Red,
            Self::Forest => ManaColor::Green,
        }
    }

    /// All five basic land types in WUBRG order (CR 305.6).
    pub fn all() -> &'static [BasicLandType] {
        &[
            Self::Plains,
            Self::Island,
            Self::Swamp,
            Self::Mountain,
            Self::Forest,
        ]
    }

    /// The subtype string as it appears in card type lines.
    pub fn as_subtype_str(&self) -> &'static str {
        match self {
            Self::Plains => "Plains",
            Self::Island => "Island",
            Self::Swamp => "Swamp",
            Self::Mountain => "Mountain",
            Self::Forest => "Forest",
        }
    }
}

impl std::str::FromStr for BasicLandType {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "Plains" => Ok(Self::Plains),
            "Island" => Ok(Self::Island),
            "Swamp" => Ok(Self::Swamp),
            "Mountain" => Ok(Self::Mountain),
            "Forest" => Ok(Self::Forest),
            _ => Err(()),
        }
    }
}

/// Odd or even — used by cards like "choose odd or even."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Parity {
    Odd,
    Even,
}

/// A branch in a d20/d6/d4 result table (CR 706.2).
/// Each branch covers a contiguous range of die results and maps to an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DieResultBranch {
    pub min: u8,
    pub max: u8,
    pub effect: Box<AbilityDefinition>,
}

impl std::str::FromStr for Parity {
    type Err = ();
    fn from_str(s: &str) -> Result<Self, ()> {
        match s {
            "Odd" => Ok(Self::Odd),
            "Even" => Ok(Self::Even),
            _ => Err(()),
        }
    }
}

/// CR 615: Damage prevention scope.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreventionScope {
    /// Prevent all damage (combat + noncombat).
    #[default]
    AllDamage,
    /// Prevent only combat damage.
    CombatDamage,
}

/// CR 615: How much damage to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PreventionAmount {
    /// "Prevent the next N damage"
    Next(u32),
    /// "Prevent all damage"
    All,
}

/// Shield type for one-shot replacement effects that expire at cleanup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ShieldKind {
    #[default]
    None,
    /// CR 701.19a: Regeneration shield — consumed on use, expires at cleanup.
    Regeneration,
    /// CR 615: Prevention shield — absorbs/prevents damage, expires at cleanup.
    Prevention { amount: PreventionAmount },
}

impl ShieldKind {
    pub fn is_none(&self) -> bool {
        matches!(self, ShieldKind::None)
    }

    pub fn is_shield(&self) -> bool {
        !self.is_none()
    }
}

/// CR 601.2 vs CR 305.1: Distinguishes "cast" (spells only) from "play" (spells + lands).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CardPlayMode {
    /// CR 601.2: Cast a spell (cannot play lands this way).
    #[default]
    Cast,
    /// CR 305.1: Play a card — cast if it's a spell, play as a land if it's a land.
    Play,
}

impl fmt::Display for CardPlayMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CardPlayMode::Cast => write!(f, "Cast"),
            CardPlayMode::Play => write!(f, "Play"),
        }
    }
}

impl std::str::FromStr for CardPlayMode {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "Cast" => Ok(CardPlayMode::Cast),
            "Play" => Ok(CardPlayMode::Play),
            _ => Err(format!("Unknown CardPlayMode: {s}")),
        }
    }
}

/// CR 702.104a + CR 702.104b: The outcome of the Tribute choice the chosen opponent
/// made as the creature entered the battlefield. Persisted as a `ChosenAttribute` on
/// the Tribute creature so the companion "if tribute wasn't paid" trigger (CR
/// 702.104b) can read the decision. A typed enum rather than a `bool` so the absence
/// of any `TributeOutcome` remains distinguishable from an explicit `Declined`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TributeOutcome {
    /// The chosen opponent placed the Tribute +1/+1 counters (CR 702.104a).
    Paid,
    /// The chosen opponent declined (CR 702.104b: "if tribute wasn't paid").
    Declined,
}

/// A typed choice stored on a permanent (e.g., "choose a color" → Color(Red)).
/// The variant discriminant serves as the category key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ChosenAttribute {
    Color(ManaColor),
    CreatureType(String),
    BasicLandType(BasicLandType),
    CardType(CoreType),
    OddOrEven(Parity),
    CardName(String),
    /// Stores a chosen number (e.g., "choose a number" for Talion).
    Number(u8),
    /// Stores the chosen opponent/player ID (CR 800.4a).
    Player(PlayerId),
    /// Stores two chosen colors as a pair.
    TwoColors([ManaColor; 2]),
    /// CR 702.104a + CR 702.104b: Records whether the opponent chosen for the
    /// Tribute ETB replacement paid tribute or declined. Read by the companion
    /// `TriggerCondition::TributeNotPaid` evaluator.
    TributeOutcome(TributeOutcome),
}

impl ChosenAttribute {
    /// Which category of choice this represents.
    pub fn choice_type(&self) -> ChoiceType {
        match self {
            Self::Color(_) => ChoiceType::Color,
            Self::CreatureType(_) => ChoiceType::CreatureType,
            Self::BasicLandType(_) => ChoiceType::BasicLandType,
            Self::CardType(_) => ChoiceType::CardType,
            Self::OddOrEven(_) => ChoiceType::OddOrEven,
            Self::CardName(_) => ChoiceType::CardName,
            Self::Number(_) => ChoiceType::NumberRange { min: 0, max: 20 },
            // Player covers both Player and Opponent choice types
            Self::Player(_) => ChoiceType::Player,
            Self::TwoColors(_) => ChoiceType::TwoColors,
            // CR 702.104: Tribute outcome uses a dedicated prompt type rather than
            // a NamedChoice (two fixed labels: Paid / Declined). Classify under the
            // Labeled category so external listings (e.g., AI candidate generation)
            // can recognise it as a Yes/No-shaped prompt.
            Self::TributeOutcome(_) => ChoiceType::Labeled {
                options: vec!["Paid".to_string(), "Declined".to_string()],
            },
        }
    }

    /// Parse a player's string response into a typed ChosenAttribute.
    /// Returns None if the string doesn't match the expected choice type.
    pub fn from_choice(choice_type: ChoiceType, value: &str) -> Option<Self> {
        match ChoiceValue::from_choice(&choice_type, value)? {
            ChoiceValue::Color(color) => Some(Self::Color(color)),
            ChoiceValue::CreatureType(creature_type) => Some(Self::CreatureType(creature_type)),
            ChoiceValue::BasicLandType(land_type) => Some(Self::BasicLandType(land_type)),
            ChoiceValue::CardType(card_type) => Some(Self::CardType(card_type)),
            ChoiceValue::OddOrEven(parity) => Some(Self::OddOrEven(parity)),
            ChoiceValue::CardName(card_name) => Some(Self::CardName(card_name)),
            ChoiceValue::Player(id) => Some(Self::Player(id)),
            ChoiceValue::TwoColors(colors) => Some(Self::TwoColors(colors)),
            ChoiceValue::Number(n) => Some(Self::Number(n)),
            ChoiceValue::Label(_) | ChoiceValue::LandType(_) => None,
        }
    }
}

/// A typed value chosen at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum ChoiceValue {
    Color(ManaColor),
    CreatureType(String),
    BasicLandType(BasicLandType),
    CardType(CoreType),
    OddOrEven(Parity),
    CardName(String),
    Number(u8),
    Label(String),
    LandType(String),
    Player(PlayerId),
    TwoColors([ManaColor; 2]),
}

impl ChoiceValue {
    pub fn from_choice(choice_type: &ChoiceType, value: &str) -> Option<Self> {
        match choice_type {
            ChoiceType::Color => value.parse::<ManaColor>().ok().map(Self::Color),
            ChoiceType::CreatureType => Some(Self::CreatureType(value.to_string())),
            ChoiceType::BasicLandType => {
                value.parse::<BasicLandType>().ok().map(Self::BasicLandType)
            }
            ChoiceType::CardType => value.parse::<CoreType>().ok().map(Self::CardType),
            ChoiceType::OddOrEven => value.parse::<Parity>().ok().map(Self::OddOrEven),
            ChoiceType::CardName => Some(Self::CardName(value.to_string())),
            ChoiceType::NumberRange { .. } => value.parse::<u8>().ok().map(Self::Number),
            ChoiceType::Labeled { .. } => Some(Self::Label(value.to_string())),
            ChoiceType::LandType => Some(Self::LandType(value.to_string())),
            // CR 800.4a: Parse player ID from string.
            ChoiceType::Opponent | ChoiceType::Player => value
                .parse::<u8>()
                .ok()
                .map(|id| Self::Player(PlayerId(id))),
            ChoiceType::TwoColors => {
                let (a, b) = value.split_once(", ")?;
                let c1 = a.parse::<ManaColor>().ok()?;
                let c2 = b.parse::<ManaColor>().ok()?;
                Some(Self::TwoColors([c1, c2]))
            }
            ChoiceType::Word | ChoiceType::Artist => Some(Self::Label(value.to_string())),
        }
    }
}

/// How to specify a damage amount -- either a fixed integer or a variable reference.
/// Which category of chosen attribute to read as a subtype.
/// Used by `ContinuousModification::AddChosenSubtype` in layer evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ChosenSubtypeKind {
    CreatureType,
    BasicLandType,
}

/// Which players' zones to count across for zone-based quantity references.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CountScope {
    Controller,
    All,
    Opponents,
}

/// Which zone to count cards in (for `QuantityRef::ZoneCardCount`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneRef {
    Graveyard,
    Exile,
    Library,
    Hand,
}

/// Who gains life from a GainLife effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GainLifePlayer {
    /// The ability's controller (default).
    #[default]
    Controller,
    /// The controller of the targeted permanent.
    TargetedController,
    /// CR 115.2 + CR 601.2c + CR 119.3: An announced target player. The
    /// engine resolves this via `ResolvedAbility::target_player()`, which
    /// returns the first `TargetRef::Player` from `ability.targets` (and
    /// falls back to controller when no Player target was announced).
    /// Set by the parser/converter when the Oracle text is "target player
    /// gains N life" rather than "you gain N life" or "[permanent's]
    /// controller gains N life."
    TargetPlayer,
}

/// How much life is gained — a fixed amount or derived from the targeted permanent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value")]
pub enum LifeAmount {
    /// Gain a specific number of life.
    Fixed(i32),
    /// Gain life equal to the targeted permanent's power.
    TargetPower,
}

/// CR 701.10d-f: What aspect to double (counters, life total, or mana pool).
/// Used by `Effect::Double` per locked decision D-05.
/// DoublePT/DoublePTAll handle CR 701.10a-c (power/toughness) separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DoubleTarget {
    /// CR 701.10e: Double the number of a kind of counter on a permanent.
    /// None = all counter types on the permanent.
    Counters { counter_type: Option<String> },
    /// CR 701.10d: Double a player's life total.
    LifeTotal,
    /// CR 701.10f: Double the amount of a type of mana in a player's mana pool.
    /// None = all mana colors.
    ManaPool { color: Option<ManaColor> },
}

/// CR 701.10a: Which P/T characteristics to double.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DoublePTMode {
    Power,
    Toughness,
    PowerAndToughness,
}

/// Power/toughness value -- either a fixed integer or a variable reference (e.g. "*", "X").
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","value":2}` (new)
/// and plain strings like `"2"` or `"*"` (legacy card-data.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum PtValue {
    Fixed(i32),
    Variable(String),
    Quantity(QuantityExpr),
}

impl<'de> serde::Deserialize<'de> for PtValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(s) => {
                // Legacy format: plain string like "2", "*", "1+*"
                match s.parse::<i32>() {
                    Ok(n) => Ok(PtValue::Fixed(n)),
                    Err(_) => Ok(PtValue::Variable(s.clone())),
                }
            }
            serde_json::Value::Number(n) => Ok(PtValue::Fixed(n.as_i64().unwrap_or(0) as i32)),
            serde_json::Value::Object(_) => {
                // New tagged format: {"type":"Fixed","value":2}
                #[derive(serde::Deserialize)]
                #[serde(tag = "type")]
                enum PtValueHelper {
                    Fixed { value: i32 },
                    Variable { value: String },
                    Quantity { value: QuantityExpr },
                }
                let helper: PtValueHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    PtValueHelper::Fixed { value: n } => Ok(PtValue::Fixed(n)),
                    PtValueHelper::Variable { value: s } => Ok(PtValue::Variable(s)),
                    PtValueHelper::Quantity { value: q } => Ok(PtValue::Quantity(q)),
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected string, number, or object for PtValue",
            )),
        }
    }
}

/// CR 605.1a + CR 107.4a: Whether a mana-production effect is the *base* mana
/// addition (e.g. a basic Forest tapping for `{G}`) or an *additional* mana
/// addition that piggy-backs on another mana event (e.g. Fertile Ground's
/// "adds an additional one mana"). Typed enum — never a bool — so the parser
/// and resolver can dispatch on the contribution role without conflating it
/// with other dimensions of mana production.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum ManaContribution {
    /// The mana addition stands on its own (most mana abilities).
    #[default]
    Base,
    /// The mana addition is additive — it augments another mana event
    /// (Utopia Sprawl, Fertile Ground, Wild Growth, Carpet of Flowers).
    Additional,
}

fn default_mana_contribution() -> ManaContribution {
    ManaContribution::Base
}

fn is_default_mana_contribution(c: &ManaContribution) -> bool {
    matches!(c, ManaContribution::Base)
}

/// CR 700.5: Which color set a devotion quantity counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type", content = "value")]
pub enum DevotionColors {
    /// Count devotion to one or more fixed colors.
    Fixed(Vec<ManaColor>),
    /// Count devotion to the color chosen by the source's current choice effect
    /// or persisted chosen-color attribute.
    ChosenColor,
}

impl<'de> serde::Deserialize<'de> for DevotionColors {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::Array(_) => {
                let colors: Vec<ManaColor> =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(DevotionColors::Fixed(colors))
            }
            serde_json::Value::Object(_) => {
                #[derive(serde::Deserialize)]
                #[serde(tag = "type", content = "value")]
                enum DevotionColorsHelper {
                    Fixed(Vec<ManaColor>),
                    ChosenColor,
                }
                let helper: DevotionColorsHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    DevotionColorsHelper::Fixed(colors) => Ok(DevotionColors::Fixed(colors)),
                    DevotionColorsHelper::ChosenColor => Ok(DevotionColors::ChosenColor),
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected array or object for DevotionColors",
            )),
        }
    }
}

/// Mana production descriptor for `Effect::Mana`.
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","colors":["White"]}` (new)
/// and a plain array of `ManaColor` like `["White","Green"]` (legacy, pre-ManaProduction refactor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum ManaProduction {
    /// Produce an explicit fixed sequence of colored mana symbols (e.g. `{W}{U}`).
    Fixed {
        #[serde(default)]
        colors: Vec<ManaColor>,
        /// CR 605.1a: Whether this is base or additional (e.g. Wild Growth,
        /// Verdant Haven) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// Produce N colorless mana (e.g. `{C}`, `{C}{C}`).
    Colorless {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 106.1: Produce a mix of colorless and colored mana (e.g. `{C}{W}`, `{C}{C}{R}`).
    /// Used by Ravnica bounce lands (Karoo, Azorius Chancery) and similar.
    Mixed {
        colorless_count: u32,
        colors: Vec<ManaColor>,
    },
    /// Produce N mana of one chosen color from the provided set.
    AnyOneColor {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
        /// CR 605.1a: Whether this is base or additional (e.g. Fertile Ground) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// Produce N mana where each unit can be chosen independently from the provided set.
    AnyCombination {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
    },
    /// Produce N mana of a previously chosen color.
    ChosenColor {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// CR 605.1a: Whether this is base or additional (e.g. Utopia Sprawl) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// CR 106.7: Produce mana of any color that a land an opponent controls could produce.
    /// Colors are computed dynamically at resolution time by inspecting opponent lands.
    OpponentLandColors {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 106.7 + CR 106.1b: Produce N mana of any **type** (W/U/B/R/G/C) that a
    /// land matching `land_filter` could produce. Differs from
    /// `OpponentLandColors` in that the choice axis is the full type set
    /// including colorless (Reflecting Pool, Naga Vitalist, Incubation Druid,
    /// Cactus Preserve, Horizon of Progress). The `land_filter` controls which
    /// lands contribute (typically `ControllerRef::You`, but parameterized so
    /// future opponent-/player-scoped printings slot in without a new variant).
    /// Per CR 106.7 the union ignores cost-payability of the surveyed lands'
    /// mana abilities — only the resulting *type set* matters. CR 106.5 applies
    /// when the union is empty (e.g. no matching lands → no mana).
    AnyTypeProduceableBy {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        land_filter: TargetFilter,
    },
    /// CR 605.1a + CR 406.1: Produce one mana of any of the colors among the cards
    /// linked to `source` via `state.exile_links` (e.g. Pit of Offerings —
    /// "Add one mana of any of the exiled cards' colors"). Colors are computed
    /// dynamically at resolution time; if no linked card is colored the ability
    /// produces no mana (CR 106.5).
    ChoiceAmongExiledColors {
        #[serde(default)]
        source: LinkedExileScope,
    },
    /// CR 605.3b + CR 106.1a: Produce one of several fixed multi-mana
    /// combinations chosen by the controller. Each option is a complete
    /// pre-specified sequence of mana types (e.g. Shadowmoor/Eventide filter
    /// lands: `Add {W}{W}, {W}{U}, or {U}{U}` yields options
    /// `[[W,W], [W,U], [U,U]]`). Unlike `AnyOneColor` (pick one color, repeat
    /// N times) the choice axis here is which complete combination to produce.
    ChoiceAmongCombinations {
        #[serde(default)]
        options: Vec<Vec<ManaColor>>,
    },
    /// CR 903.4 + CR 903.4f + CR 106.5: Produce N mana of one color chosen
    /// from the controller's commander(s)' combined color identity. Colors
    /// are computed dynamically at resolution time via
    /// `commander_color_identity`. If the color identity is empty (no
    /// commander, or a colorless commander), CR 106.5 applies and the
    /// ability produces no mana. Used by Path of Ancestry and Study Hall.
    AnyInCommandersColorIdentity {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// CR 605.1a: Whether this is base or additional mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// CR 106.1 + CR 109.1: Produce one mana of each distinct color among
    /// permanents matching a filter. "Gold", "multicolor", and "colorless" are
    /// not colors (CR 105.1), so each of W/U/B/R/G contributes at most once.
    /// Used by Faeburrow Elder's "{T}: For each color among permanents you
    /// control, add one mana of that color." Mirrors the structure of
    /// `QuantityRef::DistinctColorsAmongPermanents`.
    DistinctColorsAmongPermanents { filter: TargetFilter },
    /// CR 603.7c + CR 106.3: Produce one mana of the same type as the mana
    /// produced by the triggering `ManaAdded` event. Used by `TapsForMana`
    /// triggers of the form "add one mana of any type that land produced"
    /// (Vorinclex, Voice of Hunger; Dictate of Karametra). Resolves from
    /// `state.current_trigger_event` at resolution time; emits no mana if the
    /// current trigger event is absent or not a `ManaAdded` event (CR 106.5).
    TriggerEventManaType,
}

/// CR 607.2a + CR 406.6 + CR 610.3: Which exile-link relation a mana ability reads
/// when computing legal colors. Typed enum — never a bool — so future extensions
/// (e.g. links to a different host than `~` itself) slot in cleanly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LinkedExileScope {
    /// Read `state.exile_links` keyed to this ability's source object
    /// (the activating permanent).
    #[default]
    ThisObject,
}

impl<'de> serde::Deserialize<'de> for ManaProduction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::Array(_) => {
                // Legacy format: plain Vec<ManaColor> like ["White", "Green"]
                let colors: Vec<ManaColor> =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(ManaProduction::Fixed {
                    colors,
                    contribution: ManaContribution::default(),
                })
            }
            serde_json::Value::Object(_) => {
                // New tagged format: {"type": "Fixed", "colors": [...]}
                #[derive(serde::Deserialize)]
                #[serde(tag = "type")]
                enum ManaProductionHelper {
                    Fixed {
                        #[serde(default)]
                        colors: Vec<ManaColor>,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    Colorless {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                    },
                    AnyOneColor {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_all_mana_colors")]
                        color_options: Vec<ManaColor>,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    AnyCombination {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_all_mana_colors")]
                        color_options: Vec<ManaColor>,
                    },
                    ChosenColor {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    OpponentLandColors {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                    },
                    AnyTypeProduceableBy {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        land_filter: TargetFilter,
                    },
                    ChoiceAmongExiledColors {
                        #[serde(default)]
                        source: LinkedExileScope,
                    },
                    ChoiceAmongCombinations {
                        #[serde(default)]
                        options: Vec<Vec<ManaColor>>,
                    },
                    Mixed {
                        colorless_count: u32,
                        colors: Vec<ManaColor>,
                    },
                    AnyInCommandersColorIdentity {
                        #[serde(default = "default_quantity_one")]
                        count: QuantityExpr,
                        #[serde(default = "default_mana_contribution")]
                        contribution: ManaContribution,
                    },
                    DistinctColorsAmongPermanents {
                        filter: TargetFilter,
                    },
                    TriggerEventManaType,
                }
                let helper: ManaProductionHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(match helper {
                    ManaProductionHelper::Fixed {
                        colors,
                        contribution,
                    } => ManaProduction::Fixed {
                        colors,
                        contribution,
                    },
                    ManaProductionHelper::Colorless { count } => {
                        ManaProduction::Colorless { count }
                    }
                    ManaProductionHelper::AnyOneColor {
                        count,
                        color_options,
                        contribution,
                    } => ManaProduction::AnyOneColor {
                        count,
                        color_options,
                        contribution,
                    },
                    ManaProductionHelper::AnyCombination {
                        count,
                        color_options,
                    } => ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                    ManaProductionHelper::ChosenColor {
                        count,
                        contribution,
                    } => ManaProduction::ChosenColor {
                        count,
                        contribution,
                    },
                    ManaProductionHelper::OpponentLandColors { count } => {
                        ManaProduction::OpponentLandColors { count }
                    }
                    ManaProductionHelper::AnyTypeProduceableBy { count, land_filter } => {
                        ManaProduction::AnyTypeProduceableBy { count, land_filter }
                    }
                    ManaProductionHelper::ChoiceAmongExiledColors { source } => {
                        ManaProduction::ChoiceAmongExiledColors { source }
                    }
                    ManaProductionHelper::ChoiceAmongCombinations { options } => {
                        ManaProduction::ChoiceAmongCombinations { options }
                    }
                    ManaProductionHelper::Mixed {
                        colorless_count,
                        colors,
                    } => ManaProduction::Mixed {
                        colorless_count,
                        colors,
                    },
                    ManaProductionHelper::AnyInCommandersColorIdentity {
                        count,
                        contribution,
                    } => ManaProduction::AnyInCommandersColorIdentity {
                        count,
                        contribution,
                    },
                    ManaProductionHelper::DistinctColorsAmongPermanents { filter } => {
                        ManaProduction::DistinctColorsAmongPermanents { filter }
                    }
                    ManaProductionHelper::TriggerEventManaType => {
                        ManaProduction::TriggerEventManaType
                    }
                })
            }
            _ => Err(serde::de::Error::custom(
                "expected array or object for ManaProduction",
            )),
        }
    }
}

/// Parse-time template for mana spend restrictions.
///
/// Unlike [`ManaRestriction`](super::mana::ManaRestriction) which carries concrete values
/// on a `ManaUnit`, this enum is stored on `Effect::Mana` and resolved at production time
/// by reading runtime state (e.g., chosen creature type from the source object).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaSpendRestriction {
    /// "Spend this mana only to cast creature spells."
    SpellType(String),
    /// "Spend this mana only to cast a creature spell of the chosen type."
    /// Resolved at runtime from the source's `chosen_creature_type()`.
    ChosenCreatureType,
    /// CR 106.12: "Spend this mana only to cast creature spells or activate abilities of creatures."
    /// Combined restriction with OR semantics: allowed for spells of the type OR ability
    /// activations on permanents of the type. The `String` is the card type (e.g., "Creature").
    SpellTypeOrAbilityActivation(String),
    /// "Spend this mana only to activate abilities."
    /// Cannot be used to cast spells; only for ability activation costs.
    ActivateOnly,
    /// "Spend this mana only on costs that include {X}."
    /// Only permits spending on spells or abilities with {X} in their cost.
    XCostOnly,
    /// "Spend this mana only to cast spells with flashback."
    SpellWithKeywordKind(KeywordKind),
    /// "Spend this mana only to cast spells with flashback from a graveyard."
    SpellWithKeywordKindFromZone { kind: KeywordKind, zone: Zone },
}

/// Duration for temporary effects.
///
/// Player-axis variants (`UntilNextTurnOf`, `UntilNextUntapStepOf`) are
/// parameterized by `PlayerScope` per the workspace "Parameterize, don't
/// proliferate" principle. `PlayerScope::Controller` recovers the legacy
/// "until your next turn" / "controller's next untap step" semantics; future
/// `Target` / `Opponent` / `AllPlayers` readings unblock cards whose duration
/// is bound to a non-controller player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Duration {
    /// CR 514.2: Effect expires at end of turn (cleanup step).
    UntilEndOfTurn,
    /// CR 514.2: Effect expires at end of combat phase.
    UntilEndOfCombat,
    /// CR 514.2 + CR 611.2a: Effect expires at the beginning of `player`'s
    /// next turn. `PlayerScope::Controller` corresponds to the legacy
    /// "until your next turn" reading.
    UntilNextTurnOf {
        player: PlayerScope,
    },
    /// CR 611.2a: Effect expires when the source object leaves the
    /// battlefield.
    UntilHostLeavesPlay,
    /// CR 502.3 + CR 611.2a: Effect expires at the beginning of `player`'s
    /// next untap step. `PlayerScope::Controller` corresponds to the legacy
    /// "controller's next untap step" reading used by exert / "doesn't
    /// untap" effects.
    UntilNextUntapStepOf {
        player: PlayerScope,
    },
    /// CR 611.2b: "for as long as [condition]" — effect persists while condition holds.
    ForAsLongAs {
        condition: StaticCondition,
    },
    Permanent,
}

// ---------------------------------------------------------------------------
// Game restriction system — composable runtime restrictions
// ---------------------------------------------------------------------------

/// A game-level restriction that modifies how rules are applied.
/// Stored in `GameState::restrictions` and evaluated by relevant game systems.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum GameRestriction {
    /// CR 614.16: Damage prevention effects are suppressed.
    DamagePreventionDisabled {
        source: ObjectId,
        expiry: RestrictionExpiry,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<RestrictionScope>,
    },
    /// CR 101.2 + CR 601.2a: A temporary effect restricts affected players to casting
    /// spells only from the listed zones until the restriction expires.
    CastOnlyFromZones {
        source: ObjectId,
        affected_players: RestrictionPlayerScope,
        allowed_zones: Vec<Zone>,
        expiry: RestrictionExpiry,
    },
    /// CR 101.2: A temporary effect prevents affected players from casting any spell
    /// until the restriction expires. E.g., Silence: "Your opponents can't cast spells this turn."
    CantCastSpells {
        source: ObjectId,
        affected_players: RestrictionPlayerScope,
        expiry: RestrictionExpiry,
    },
}

/// When a game restriction expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RestrictionExpiry {
    EndOfTurn,
    EndOfCombat,
    UntilPlayerNextTurn { player: PlayerId },
}

/// Limits the scope of a game restriction to specific sources or targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RestrictionScope {
    SourcesControlledBy(PlayerId),
    SpecificSource(ObjectId),
    DamageToTarget(ObjectId),
}

/// Identifies which players are affected by a temporary game restriction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RestrictionPlayerScope {
    AllPlayers,
    SpecificPlayer(PlayerId),
    OpponentsOfSourceController,
}

// ---------------------------------------------------------------------------
// Casting permissions — per-object casting grants
// ---------------------------------------------------------------------------

/// A permission granted to a `GameObject` allowing it to be cast under specific conditions.
/// Stored in `GameObject::casting_permissions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastingPermission {
    /// CR 715.5: After Adventure resolves to exile, creature face castable from exile.
    AdventureCreature,
    /// Card may be cast from exile for the specified cost by its owner.
    /// Building block for Airbending, Suspend, and similar "cast from exile" mechanics.
    /// `cast_transformed` causes the spell to resolve to its back face (CR 712.14a); used
    /// by Siege victory triggers (CR 310.11b: "cast it transformed without paying its mana cost").
    ExileWithAltCost {
        cost: ManaCost,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cast_transformed: bool,
        /// CR 702.85a: optional cast-time predicate gating whether the cast may
        /// proceed. `None` for the common case (Airbending, Suspend, Discover,
        /// etc.); `Some(...)` only for mechanics that must reject after X is
        /// chosen. Evaluated at cast finalization, before the spell moves to
        /// the stack — see `casting_costs::finalize_cast_with_phyrexian_choices`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<CastPermissionConstraint>,
    },
    /// CR 400.7i: Play from exile until duration expires (impulse draw).
    /// Building block for "exile top N, choose one, you may play it this turn" patterns.
    ///
    /// `granted_to` records the player the permission was granted to — i.e. the
    /// controller of the effect that created it (CR 611.2a/b). This is required
    /// to correctly expire `Duration::UntilNextTurnOf` permissions at the
    /// grantee's next untap step and `Duration::UntilEndOfTurn` permissions at
    /// cleanup (CR 514.2). Cast-permission checks (`has_exile_cast_permission`)
    /// do not consult this field — it governs duration pruning only.
    PlayFromExile {
        duration: Duration,
        granted_to: PlayerId,
    },
    /// CR 122.3: Cast from exile by paying {E} equal to the card's mana value.
    /// Building block for Amped Raptor and similar energy-based casting mechanics.
    ExileWithEnergyCost,
    /// CR 118.9 + CR 119.4: Cast from exile by paying a non-mana alternative
    /// cost in lieu of the spell's mana cost. Building block for "play it ...
    /// pay [non-mana cost] rather than paying its mana cost" patterns
    /// (Nashi, Moon Sage's Scion: pay life equal to the spell's mana value).
    ///
    /// `cost` is a full `AbilityCost` (with dynamic-quantity support via
    /// `QuantityExpr`), distinguishing this from `ExileWithAltCost { cost:
    /// ManaCost }` which can only carry a fixed mana cost.
    ExileWithAltAbilityCost {
        /// CR 117.1: the alternative cost to pay instead of the spell's mana
        /// cost. Resolved through the standard `pay_additional_cost` pipeline,
        /// so `AbilityCost::PayLife { amount: QuantityExpr }` and friends
        /// (with dynamic quantity refs like `SelfManaValue`, which reads the
        /// spell-being-cast's mana value at cost-payment time) work for free.
        cost: AbilityCost,
        /// CR 702.85a: optional cast-time predicate gating whether the cast
        /// may proceed. Mirrors `ExileWithAltCost.constraint`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<CastPermissionConstraint>,
    },
    /// CR 702.185a: Warp — card may be cast from exile at its normal mana cost,
    /// but only after the specified turn ends. Persists for as long as card remains exiled.
    WarpExile { castable_after_turn: u32 },
    /// CR 702.170a + CR 702.170d: Plot — card was exiled from its owner's hand via
    /// the plot special action (or granted this marker by another effect). On any
    /// turn after `turn_plotted`, the owner may cast it from exile without paying
    /// its mana cost during their own main phase while the stack is empty
    /// (sorcery-speed). The `turn_plotted` field is stamped from `state.turn_number`
    /// at resolution time by `grant_permission::resolve` (placeholder `0` at
    /// definition time); it is read by `has_exile_cast_permission` to gate the
    /// "later turn" check. Persists for as long as the card remains in exile
    /// (cleared by `zones::apply_zone_exit_cleanup` when the card leaves exile).
    Plotted { turn_plotted: u32 },
    /// CR 702.143a-d: Foretell — card was exiled from its owner's hand by the
    /// foretell special action, became a foretold card in exile, and may be
    /// cast on a later turn for its foretell cost. `turn_foretold` is stamped
    /// when the special action resolves; the permission is scoped to the exile
    /// zone and cleared when the object leaves exile.
    Foretold { cost: ManaCost, turn_foretold: u32 },
}

/// CR 611.2a + CR 108.3: Identifies which player a `CastingPermission` is granted
/// to at resolution time. Resolved to a concrete `PlayerId` by
/// `grant_permission::resolve` before the permission is written to the target
/// `GameObject`. Drives both `granted_to` binding and, for durations scoped to
/// that player (e.g., `UntilNextTurnOf`), the prune step in `layers.rs`.
///
/// Default is `AbilityController` for all pre-existing parser call sites.
/// Additional variants support compound-exile patterns where multiple objects
/// are granted distinct permissions tied to different players:
/// - `ObjectOwner` — Suspend Aggression: "its owner may play it". Each object
///   in the tracked set is granted to its own owner (CR 108.3).
/// - `ParentTargetController` — Expedited Inheritance: "its controller may
///   exile [N] ... They may play those cards". The grant is tied to the
///   parent effect's player target rather than the triggered-ability controller.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PermissionGrantee {
    /// CR 611.2a default — the controller of the effect that created the grant.
    #[default]
    AbilityController,
    /// CR 108.3 — each iterated object's owner (per-object binding).
    ObjectOwner,
    /// CR 109.4 — the player target of the parent effect in the chain.
    ParentTargetController,
}

/// Returns true when `grantee` is the default (`AbilityController`). Used as a
/// `skip_serializing_if` predicate so pre-existing card JSON stays unchanged.
pub fn is_default_grantee(g: &PermissionGrantee) -> bool {
    matches!(g, PermissionGrantee::AbilityController)
}

/// CR 702.85a: Typed cast-time predicates attached to an `ExileWithAltCost`
/// permission. Extend this enum when future mechanics need cast-time gating
/// that cannot be evaluated at permission-grant time (e.g., X-cost spells
/// whose mana value is only known after the caster chooses X).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CastPermissionConstraint {
    /// CR 702.85a: Cascade — "You may cast that card without paying its mana
    /// cost if the resulting spell's mana value is less than this spell's
    /// mana value." The resulting mana value is only determined after X,
    /// Kicker, and similar choices, so this check must run at cast
    /// finalization, not at offer time.
    ///
    /// `exiled_misses` is rejection-cleanup state: when the cast-time check
    /// fails, the original `WaitingFor::CascadeChoice` has already been
    /// cleared, so the misses ride inside the permission so the bottom-shuffle
    /// step can still reach them.
    CascadeResultingMvBelow {
        source_mv: u32,
        exiled_misses: Vec<super::identifiers::ObjectId>,
    },
}

/// When a delayed triggered ability fires (CR 603.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DelayedTriggerCondition {
    /// "at the beginning of the next [phase]"
    /// CR 603.7: fires on next PhaseChanged for that phase.
    AtNextPhase { phase: Phase },
    /// "at the beginning of your next [phase]"
    /// Fires only when the specified player is active.
    AtNextPhaseForPlayer { phase: Phase, player: PlayerId },
    /// "when [object] leaves the battlefield"
    WhenLeavesPlay {
        object_id: super::identifiers::ObjectId,
    },
    /// CR 603.7c: "when [object] dies" — fires on zone change to graveyard.
    /// Filter-based variant resolved at trigger check time (unlike WhenLeavesPlay
    /// which uses a specific object_id).
    WhenDies { filter: TargetFilter },
    /// CR 603.7c: "when [object] leaves the battlefield" — filter-based variant
    /// that fires on any zone change from battlefield.
    WhenLeavesPlayFiltered { filter: TargetFilter },
    /// CR 603.7c: "when [object] enters the battlefield" — fires on zone change
    /// to battlefield.
    WhenEntersBattlefield { filter: TargetFilter },
    /// "when [object] dies or is exiled" — fires on zone change to graveyard OR exile.
    /// Filter-based variant resolved at trigger check time.
    WhenDiesOrExiled { filter: TargetFilter },
    /// CR 603.7c: "Whenever [event] this turn" — fires each time the event occurs
    /// until end of turn. Reuses existing trigger matching infrastructure via embedded
    /// TriggerDefinition. The embedded trigger's `execute` field should be `None` —
    /// the actual effect lives in `DelayedTrigger.ability`.
    WheneverEvent { trigger: Box<TriggerDefinition> },
    /// CR 603.7: "When you next [event] this turn" — fires once on the next matching
    /// event, then is removed. One-shot variant of `WheneverEvent`.
    /// Uses existing trigger matching infrastructure to detect the event.
    WhenNextEvent { trigger: Box<TriggerDefinition> },
}

/// Specifies variable-count targeting for "any number of" effects.
/// CR 601.2c: Player chooses targets during resolution.
/// CR 115.1d: "Any number" means zero or more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MultiTargetSpec {
    pub min: usize,
    /// `None` means "any number" (unlimited). CR 115.1d.
    pub max: Option<QuantityExpr>,
}

impl MultiTargetSpec {
    pub fn fixed(min: usize, max: usize) -> Self {
        Self::bounded(min, QuantityExpr::Fixed { value: max as i32 })
    }

    pub fn up_to(max: QuantityExpr) -> Self {
        Self::bounded(0, max)
    }

    pub fn unlimited(min: usize) -> Self {
        Self { min, max: None }
    }

    pub fn bounded(min: usize, max: QuantityExpr) -> Self {
        Self {
            min,
            max: Some(max),
        }
    }
}

// ---------------------------------------------------------------------------
// TargetFilter -- replaces TargetSpec entirely
// ---------------------------------------------------------------------------

/// Type filter for card type matching in filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TypeFilter {
    Creature,
    Land,
    Artifact,
    Enchantment,
    Instant,
    Sorcery,
    Planeswalker,
    /// CR 310: Battle — a permanent type introduced in March of the Machine.
    Battle,
    Permanent,
    Card,
    Any,
    /// CR 205.4b: Negation — matches objects whose type does NOT match the inner filter.
    /// "noncreature" → `Non(Box::new(Creature))`, "non-Human" → `Non(Box::new(Subtype("Human")))`
    Non(Box<TypeFilter>),
    /// CR 205.3: Matches objects with a specific subtype (creature type, land type, etc.).
    /// String because MTG has 250+ creature subtypes (CR 205.3m) with new ones each set.
    Subtype(String),
    /// CR 608.2b: Disjunction — matches if ANY inner filter matches.
    /// "creature or enchantment" → `AnyOf(vec![Creature, Enchantment])`
    AnyOf(Vec<TypeFilter>),
}

/// Filter for damage type on trigger definitions.
/// CR 120.3: Combat damage is dealt during the combat damage step; all other damage is noncombat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum DamageKindFilter {
    /// Matches both combat and noncombat damage.
    #[default]
    Any,
    /// CR 120.2a: Only combat damage (dealt as a result of combat).
    CombatOnly,
    /// CR 120.2b: Only noncombat damage (dealt as an effect of a spell or ability).
    NoncombatOnly,
}

/// Controller reference for filter matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControllerRef {
    You,
    Opponent,
    /// CR 109.4 + CR 115.1: Filter controller is the player chosen as a target
    /// of the enclosing ability (e.g., "each creature target player controls").
    /// At resolution time, `filter_inner` reads the first `TargetRef::Player`
    /// from `ability.targets`. At target-selection time, `collect_target_slots`
    /// surfaces a companion `TargetFilter::Player` slot so the player is chosen
    /// as part of CR 601.2c / CR 603.3d target declaration.
    TargetPlayer,
    /// CR 508.1b + CR 603.4: Filter controller is the defending player for the
    /// source attacking creature in the current combat. Used by intervening-if
    /// quantity checks such as "defending player controls more lands than you."
    DefendingPlayer,
}

/// CR 301 / CR 303: Kinds of attachments to permanents.
/// Used by `FilterProp::HasAttachment` and `QuantityRef::AttachmentsOnLeavingObject`
/// to parameterize attachment-predicate checks.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AttachmentKind {
    /// CR 303.4: Aura — enchantment subtype that attaches via Enchant ability.
    Aura,
    /// CR 301.5: Equipment — artifact subtype that attaches via Equip ability.
    Equipment,
}

/// Qualities that can be shared across multi-target selections.
/// Used by `FilterProp::SharesQuality` for group constraint validation at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SharedQuality {
    /// CR 201.2: object names compared by shared name.
    Name,
    CreatureType,
    Color,
    CardType,
    /// CR 205.3i + CR 305.6: land subtypes, including the five basic land types.
    LandType,
}

/// Relationship required by `FilterProp::SharesQuality`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum SharedQualityRelation {
    #[default]
    Shares,
    DoesNotShare,
}

fn is_default_shared_quality_relation(value: &SharedQualityRelation) -> bool {
    matches!(value, SharedQualityRelation::Shares)
}

/// Individual filter properties that can be combined in a Typed filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilterProp {
    Token,
    Attacking,
    /// CR 509.1a: Matches creatures that are blocking.
    Blocking,
    /// CR 509.1g: Matches creatures currently blocking the filter source.
    /// Used for "creature(s) blocking it" source-relative quantities and filters.
    BlockingSource,
    /// CR 509.1h: Matches attacking creatures with no blockers assigned.
    Unblocked,
    Tapped,
    /// CR 302.6 / CR 110.5: Untapped status as targeting qualifier.
    Untapped,
    WithKeyword {
        value: Keyword,
    },
    HasKeywordKind {
        value: KeywordKind,
    },
    /// CR 702: Matches objects that do NOT have a given keyword ability.
    /// Used for "without flying", "without first strike", etc.
    WithoutKeyword {
        value: Keyword,
    },
    WithoutKeywordKind {
        value: KeywordKind,
    },
    CountersGE {
        counter_type: CounterType,
        count: QuantityExpr,
    },
    /// CR 122.1: Matches objects with at least one counter of any type on them.
    /// Used for "creature with one or more counters on it" phrases where the
    /// counter type is unspecified (Nils, Discipline Enforcer's attack-tax class).
    HasAnyCounter,
    /// Matches objects whose mana value satisfies `comparator` against `value`.
    /// CR 202.3. Replaces the legacy `CmcGE`/`CmcLE`/`CmcEQ` sibling cluster;
    /// the comparator axis is parameterized via the existing `Comparator` enum,
    /// unlocking GT/LT/NE comparisons without further variant proliferation.
    /// Supports both fixed and dynamic (e.g., X) thresholds via `QuantityExpr`.
    Cmc {
        comparator: Comparator,
        value: QuantityExpr,
    },
    InZone {
        zone: Zone,
    },
    Owned {
        controller: ControllerRef,
    },
    /// CR 702.143c-d: Matches cards in exile that have the foretold designation.
    /// This is a status of the exiled card, not equivalent to having the
    /// Foretell keyword or a generic exile casting permission.
    Foretold,
    EnchantedBy,
    EquippedBy,
    /// CR 301.5 + CR 303.4: True when the matched object's `attached_to` field
    /// equals the filter source's object ID. Inverse of `EnchantedBy`/`EquippedBy`,
    /// which check whether the source has an attachment. Used for "Aura and
    /// Equipment attached to ~" quantity clauses (Kellan, the Fae-Blooded) and
    /// for any compound filter whose subject is "attached to <self>".
    AttachedToSource,
    /// CR 301.5 + CR 303.4 + CR 613.4c: True when the matched object's
    /// `attached_to` field equals the *recipient* of the resolving effect (the
    /// per-object `id` in a layer's affected list). Distinct from
    /// `AttachedToSource` — for an Aura/Equipment static "Enchanted/Equipped
    /// creature gets +N/+M for each X attached to it", the pronoun "it" refers
    /// to the affected creature, not to the static's source object. The
    /// recipient is supplied through `FilterContext::recipient_id`; when
    /// recipient is unknown (no per-recipient context), this prop evaluates to
    /// false. Covers ~25 cards: Strong Back, Mantle of the Ancients,
    /// Auramancer's Guise, Champion of the Flame, Bruenor Battlehammer's
    /// "Each creature you control gets +2/+0 for each Equipment attached to
    /// it", and the broader "<subject> gets +N/+M for each Aura/Equipment
    /// attached to it" family.
    AttachedToRecipient,
    /// CR 303.4 + CR 301.5: Matches objects that have at least one attachment of the
    /// given kind whose controller matches `controller`. Unlike `EnchantedBy`/`EquippedBy`
    /// (which are source-relative — match when THIS source is attached to the object),
    /// this predicate is non-source-relative: it matches any object with a qualifying
    /// attachment. `controller = None` means "any controller".
    ///
    /// Covers:
    /// - "enchanted creature" when the ability source is not the Aura itself
    ///   (e.g. Hateful Eidolon's "Whenever an enchanted creature dies, ...").
    /// - "creature enchanted by an Aura you control" (Killian, Decisive Mentor).
    /// - "creature equipped by an Equipment you control" (future).
    HasAttachment {
        kind: AttachmentKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
    },
    /// CR 303.4 + CR 301.5: Disjunctive attachment predicate — matches objects that
    /// have at least one attachment whose subtype is in `kinds` and whose controller
    /// satisfies the optional `ControllerRef`. Generalizes `HasAttachment` to the
    /// "enchanted or equipped" compound subject class (Reyav, Master Smith;
    /// Dogmeat, Ever Loyal). Single-kind use is also valid (kinds.len() == 1) but
    /// `HasAttachment` is preferred for that case.
    HasAnyAttachmentOf {
        kinds: Vec<AttachmentKind>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
    },
    /// Matches any object that is NOT the trigger source (for "another creature" triggers).
    Another,
    /// CR 603.4 + CR 109.3: Matches any object that is NOT the object that caused
    /// the currently-evaluating trigger to fire (the "triggering object"). Distinct
    /// from `Another`, which excludes the *ability source*. Used for intervening-if
    /// count predicates like Valakut, the Molten Pinnacle's "if you control at
    /// least five other Mountains" — here "other" means "other than the newly-
    /// entered Mountain," not "other than Valakut." Resolves against
    /// `FilterContext::triggering_object_id`, populated at trigger-condition
    /// evaluation from the current `GameEvent`.
    OtherThanTriggerObject,
    /// Matches objects with a specific color (for "white creature", "red spell", etc.).
    HasColor {
        color: ManaColor,
    },
    /// Matches objects with power <= N (for "creature with power 2 or less").
    /// CR 208.1: Uses QuantityExpr to support both fixed and dynamic comparisons.
    PowerLE {
        value: QuantityExpr,
    },
    /// Matches objects with power >= N (for "creature with power 3 or greater").
    /// CR 208.1: Uses QuantityExpr to support both fixed and dynamic comparisons.
    PowerGE {
        value: QuantityExpr,
    },
    /// CR 509.1b: Matches objects whose power is strictly greater than the source object's power.
    /// Used for "creatures with greater power" blocking restrictions (relative comparison).
    PowerGTSource,
    /// Matches multicolored objects (2+ colors).
    Multicolored,
    /// CR 105.2c: Matches colorless objects (0 colors).
    Colorless,
    /// Matches objects with a specific supertype (Basic, Legendary, Snow).
    HasSupertype {
        value: Supertype,
    },
    /// Matches objects whose subtypes include the source object's chosen creature type.
    /// Used for "of the chosen type" patterns (Cavern of Souls, Metallic Mimic).
    IsChosenCreatureType,
    /// Matches objects whose colors include the source object's chosen color.
    /// Used for "of the chosen color" patterns (Hall of Triumph, Runed Stalactite).
    /// Reads `ChosenAttribute::Color` from the source permanent.
    IsChosenColor,
    /// Matches objects whose core type includes the source object's chosen card type.
    /// Used for "spells of the chosen type" patterns (Archon of Valor's Reach).
    /// Reads `ChosenAttribute::CardType` from the source permanent.
    IsChosenCardType,
    /// CR 205.2 + CR 608.2c: Matches objects by the transient "land or nonland"
    /// choice made earlier in the same resolving instruction sequence. Used by
    /// "of the chosen kind" library filters, where "Land" means cards with the
    /// land card type and "Nonland" means cards without it.
    IsChosenLandOrNonlandKind,
    /// CR 115.7: Matches stack entries that have exactly one target.
    /// Used for "with a single target" qualifiers on retarget effects.
    HasSingleTarget,
    /// CR 205.4b: Matches objects that do NOT have a specific color.
    /// Parallel to `HasColor` — used for "nonblack", "nonwhite" in negation stacks.
    NotColor {
        color: ManaColor,
    },
    /// CR 205.4a: Matches objects that do NOT have a specific supertype.
    /// Parallel to `HasSupertype` — used for "nonbasic", "nonlegendary" in negation stacks.
    NotSupertype {
        value: Supertype,
    },
    /// CR 701.60b: Matches suspected creatures.
    Suspected,
    /// CR 510.1c: Matches creatures whose toughness is greater than their power.
    ToughnessGTPower,
    /// CR 208.1: Matches objects with toughness <= N. Mirrors `PowerLE`.
    /// Used for "creature with toughness N or less" and as a building block
    /// for disjunctive P/T filters ("power or toughness N or less").
    ToughnessLE {
        value: QuantityExpr,
    },
    /// CR 208.1: Matches objects with toughness >= N. Mirrors `PowerGE`.
    ToughnessGE {
        value: QuantityExpr,
    },
    /// Disjunctive composite: the object matches if ANY inner prop matches.
    /// Used for natural-language OR within a property suffix — e.g.
    /// "creature with power or toughness N or less" decomposes to
    /// `AnyOf { [PowerLE(N), ToughnessLE(N)] }` on a `creature` typed filter,
    /// preserving the single-type constraint while expressing the OR
    /// semantics at the property layer. Nest by composing with other props.
    AnyOf {
        props: Vec<FilterProp>,
    },
    /// CR 700.9: A permanent is modified if it has one or more counters on it
    /// (CR 122), if it is equipped (CR 301.5), or if it is enchanted by an Aura
    /// that is controlled by that permanent's controller (CR 303.4).
    ///
    /// Modeled as a first-class typed predicate rather than an `AnyOf`
    /// composite because CR 700.9 names "modified" as a distinct concept and
    /// the three legs share a single runtime match arm. Parser dispatch emits
    /// `FilterProp::Modified` for "modified creature(s)" subjects, analogous
    /// to how `FilterProp::Suspected` models CR 701.60b's "suspected" status.
    Modified,
    /// CR 700.6: An object is historic if it has the legendary supertype, the
    /// artifact card type, or the Saga subtype.
    ///
    /// Modeled as a first-class typed predicate rather than an `AnyOf`
    /// composite because CR 700.6 names "historic" as a distinct concept and
    /// the three legs share a single runtime match arm. Parser dispatch emits
    /// `FilterProp::Historic` for "historic permanent" / "historic spell" /
    /// "historic card" subjects, mirroring `FilterProp::Modified` for
    /// CR 700.9's "modified" predicate.
    Historic,
    /// Matches objects whose name differs from all objects matching the inner filter
    /// that the evaluating controller controls on the battlefield.
    /// Used for "with a different name than each [type] you control" (e.g. Light-Paws).
    DifferentNameFrom {
        filter: Box<TargetFilter>,
    },
    /// CR 604.3: Matches objects whose current zone is any of the listed zones (OR semantics).
    /// Used for zone-based restrictions like "cards in graveyards and libraries".
    InAnyZone {
        zones: Vec<Zone>,
    },
    /// Multi-target group constraint — all selected targets must share at least
    /// one value of the named quality. Validated at resolution time, not per-object.
    /// Examples: "that share a creature type", "that share a color", "that share a card type".
    SharesQuality {
        quality: SharedQuality,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reference: Option<Box<TargetFilter>>,
        #[serde(default, skip_serializing_if = "is_default_shared_quality_relation")]
        relation: SharedQualityRelation,
    },
    /// CR 510.1: Object was dealt damage during this turn.
    /// Checks `damage_marked > 0` (damage persists until cleanup step).
    WasDealtDamageThisTurn,
    /// CR 400.7: Object entered the battlefield during this turn.
    /// Checks `entered_battlefield_turn == Some(current_turn)`.
    EnteredThisTurn,
    /// CR 508.1a: Creature was declared as an attacker this turn.
    /// Checks `creatures_attacked_this_turn` tracking set on GameState.
    AttackedThisTurn,
    /// CR 509.1a: Creature was declared as a blocker this turn.
    /// Checks `creatures_blocked_this_turn` tracking set on GameState.
    BlockedThisTurn,
    /// CR 508.1a + CR 509.1a: Creature attacked or blocked this turn.
    /// Compound check used by "that attacked or blocked this turn" Oracle text.
    AttackedOrBlockedThisTurn,
    /// CR 707.2: Matches face-down objects on the battlefield.
    /// Used for "face-down creature" trigger subjects.
    FaceDown,
    /// CR 115.9c: Matches stack entries whose targets ALL satisfy the given filter.
    /// Used for "that targets only ~", "that targets only a single creature you control", etc.
    /// Permissive at the per-object filter level; validated against the stack entry's actual
    /// targets by trigger matchers and retarget effects.
    TargetsOnly {
        filter: Box<TargetFilter>,
    },
    /// CR 115.9b: Matches stack entries that have at least one target satisfying the filter.
    /// Used for "that targets ~", "that targets you", etc. (.any() semantics).
    /// Contrast with TargetsOnly (CR 115.9c) which requires ALL targets to match (.all()).
    Targets {
        filter: Box<TargetFilter>,
    },
    /// CR 107.3 + CR 202.1: Matches spells/objects whose printed mana cost contains
    /// an `{X}` shard. Used for "spell with {X} in its mana cost" qualifier on
    /// spell-cast triggers (Lattice Library, Nev the Practical Dean, Owlin
    /// Spiralmancer, Brass Infiniscope, Elementalist's Palette).
    /// Evaluated against `SpellCastRecord.has_x_in_cost` in the spell-history
    /// filter path and against `cost_has_x(&obj.mana_cost)` for live objects.
    HasXInManaCost,
    /// CR 201.2: Matches objects whose card name equals the given name.
    /// Used for "cards named [X]" and "named [X]" filter patterns.
    /// Name comparison is exact per CR 201.2a (case-insensitive at evaluation).
    Named {
        name: String,
    },
    /// Matches objects with the same name as a previously-referenced card.
    /// Used for "search your library for a card with that name" patterns.
    SameName,
    /// CR 201.2: Matches objects whose name equals the name of the
    /// resolving ability's first object target. Used by chained sub-abilities
    /// where a prior step targeted/exiled a card and the next step references
    /// "cards with that name" — e.g., Deadly Cover-Up's "search ... for any
    /// number of cards with that name and exile them" (the "that name" is the
    /// name of the card exiled by the immediately preceding effect, which is
    /// inherited as the first target via `TargetFilter::ParentTarget`).
    ///
    /// Differs from `SameName` (which reads the source object's name): this
    /// reads from `ability.targets[0]` when that target is `TargetRef::Object`,
    /// looking up the name from `state.objects` (or `lki_cache` if the target
    /// has already left its zone).
    SameNameAsParentTarget,
    /// CR 201.2 + CR 201.2a: Matches objects whose name equals the name of any
    /// permanent currently on the battlefield. `controller` optionally narrows
    /// the pool of permanents whose names are considered (None = any controller,
    /// i.e. "shares a name with a permanent" unqualified). Used by "put a card
    /// onto the battlefield if it has the same name as a permanent" patterns
    /// (Mitotic Manipulation, and any analogous dig-with-name-match effect).
    NameMatchesAnyPermanent {
        controller: Option<ControllerRef>,
    },
    /// CR 508.1b: Matches attacking creatures whose defending player equals the
    /// filter's source controller ("creatures attacking you"). Distinct from
    /// `Attacking`, which matches any attacker regardless of defender.
    AttackingController,
    /// CR 903.3 + CR 903.3d: Matches permanents on the battlefield that are a
    /// commander. Reads `GameObject::is_commander`, set during deck construction
    /// per CR 903.3 (the legendary card designated as that deck's commander).
    /// Used for "commander(s) you control", "your commander" subject phrases
    /// and for "target commander" in commander-format effects (Codsworth, Falthis,
    /// Anara, Champions of Archery, etc.).
    IsCommander,
    Other {
        value: String,
    },
}

impl FilterProp {
    /// Returns true if `self` and `other` are the same enum variant (ignoring inner values).
    /// Used by `distribute_properties_to_or` to avoid duplicating property kinds.
    pub fn same_kind(&self, other: &Self) -> bool {
        std::mem::discriminant(self) == std::mem::discriminant(other)
    }
}

/// Named fields for the `TargetFilter::Typed` variant, extracted for builder ergonomics.
/// CR 205: `type_filters` holds all type constraints in conjunction (all must match).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypedFilter {
    /// CR 205: All type constraints that must match (conjunction).
    /// e.g. "noncreature, nonland permanent" → `[Permanent, Non(Creature), Non(Land)]`
    #[serde(default)]
    pub type_filters: Vec<TypeFilter>,
    #[serde(default)]
    pub controller: Option<ControllerRef>,
    #[serde(default)]
    pub properties: Vec<FilterProp>,
}

impl TypedFilter {
    pub fn new(card_type: TypeFilter) -> Self {
        Self {
            type_filters: vec![card_type],
            ..Self::default()
        }
    }
    pub fn creature() -> Self {
        Self::new(TypeFilter::Creature)
    }
    pub fn permanent() -> Self {
        Self::new(TypeFilter::Permanent)
    }
    pub fn land() -> Self {
        Self::new(TypeFilter::Land)
    }
    pub fn card() -> Self {
        Self::new(TypeFilter::Card)
    }
    /// Add an additional type constraint (conjunction).
    pub fn with_type(mut self, tf: TypeFilter) -> Self {
        self.type_filters.push(tf);
        self
    }
    pub fn controller(mut self, ctrl: ControllerRef) -> Self {
        self.controller = Some(ctrl);
        self
    }
    /// CR 205.3: Add a subtype constraint (e.g. "Human", "Zombie").
    pub fn subtype(mut self, sub: String) -> Self {
        self.type_filters.push(TypeFilter::Subtype(sub));
        self
    }
    pub fn properties(mut self, props: Vec<FilterProp>) -> Self {
        self.properties = props;
        self
    }

    /// Extract the first subtype from type_filters, if any.
    pub fn get_subtype(&self) -> Option<&str> {
        self.type_filters.iter().find_map(|tf| match tf {
            TypeFilter::Subtype(s) => Some(s.as_str()),
            _ => None,
        })
    }

    /// Extract the primary type filter (first non-Subtype, non-Non entry), if any.
    pub fn get_primary_type(&self) -> Option<&TypeFilter> {
        self.type_filters
            .iter()
            .find(|tf| !matches!(tf, TypeFilter::Subtype(_) | TypeFilter::Non(_)))
    }

    /// Whether this filter has any meaningful type constraint beyond Card/Any.
    pub fn has_meaningful_type_constraint(&self) -> bool {
        self.type_filters
            .iter()
            .any(|tf| !matches!(tf, TypeFilter::Card | TypeFilter::Any))
            || !self.properties.is_empty()
    }

    pub fn normalized(mut self) -> Self {
        self.type_filters = normalized_type_filters(self.type_filters);
        self.properties = normalized_filter_props(self.properties);
        self
    }
}

impl From<TypedFilter> for TargetFilter {
    fn from(f: TypedFilter) -> Self {
        TargetFilter::Typed(f)
    }
}

/// Typed target filter replacing all Forge filter strings and TargetSpec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetFilter {
    None,
    Any,
    Player,
    Controller,
    SelfRef,
    Typed(TypedFilter),
    Not {
        filter: Box<TargetFilter>,
    },
    Or {
        filters: Vec<TargetFilter>,
    },
    And {
        filters: Vec<TargetFilter>,
    },
    /// Matches non-mana activated or triggered abilities on the stack.
    /// Used by "counter target activated or triggered ability" effects.
    StackAbility,
    /// Matches spells on the stack (not activated/triggered abilities).
    /// CR 115.1a: Used by "becomes the target of a spell" triggers to filter source type.
    StackSpell,
    /// Matches a specific permanent by ObjectId.
    /// Used for duration-based statics that target a specific object
    /// (e.g., "that permanent loses all abilities for as long as ~").
    SpecificObject {
        id: ObjectId,
    },
    /// CR 113.10 + CR 702.16j: Matches a specific player by `PlayerId`.
    /// Used for duration-based statics scoped to a single player
    /// (e.g., Teferi's-Protection-style "you gain protection from everything").
    /// Registered at resolution time by `register_transient_effect` when the
    /// static's `affected` resolves to the ability's controller.
    SpecificPlayer {
        id: PlayerId,
    },
    /// Matches the permanent that the trigger source (Equipment/Aura) is attached to.
    /// Used for "equipped creature" / "enchanted creature" trigger subjects.
    AttachedTo,
    /// Resolves to the most recently created token(s) from Effect::Token.
    /// Used for "create X and [verb] it" patterns (e.g. "create a token and suspect it").
    LastCreated,
    /// Matches exactly the objects in a tracked set.
    /// CR 603.7: Delayed triggers act on specific objects from the originating effect.
    TrackedSet {
        id: super::identifiers::TrackedSetId,
    },
    /// CR 701.33 + CR 701.18: Intersection of a tracked set with a type filter.
    /// Matches objects that are BOTH members of the tracked set AND satisfy the
    /// inner type filter. Used to route "X cards revealed this way" downstream
    /// sub_abilities — the Dig resolver populates a tracked set with the cards
    /// kept (revealed) by the player's selection, and follow-up effects like
    /// "Put all land cards revealed this way onto the battlefield tapped" use
    /// this variant to restrict their targets to only the revealed subset that
    /// matches the type filter.
    ///
    /// Example: Zimone's Experiment produces `TrackedSetFiltered { id: 0
    /// /* sentinel resolved to the most recent tracked set */, filter:
    /// Typed(Land) }` for the land-routing sub_ability.
    TrackedSetFiltered {
        id: super::identifiers::TrackedSetId,
        filter: Box<TargetFilter>,
    },
    /// CR 610.3: Cards exiled by a specific source via "exile until ~ leaves" links.
    /// Resolves via relational `state.exile_links` lookup, not intrinsic object properties.
    ExiledBySource,
    /// CR 603.7c: Resolves to the controller of the spell/ability that triggered this.
    TriggeringSpellController,
    /// CR 603.7c: Resolves to the owner of the spell/ability that triggered this.
    TriggeringSpellOwner,
    /// CR 603.7c: Resolves to the player involved in the triggering event.
    TriggeringPlayer,
    /// CR 603.7c: Resolves to the source object of the triggering event.
    TriggeringSource,
    /// Resolves to the same target(s) as the parent ability.
    /// Used for anaphoric "it"/"that creature"/"that player" in compound effects
    /// (e.g., "tap target creature and put a stun counter on it").
    /// At resolution time, the sub_ability chain inherits parent targets automatically.
    ParentTarget,
    /// CR 608.2c: Resolves to the controller of the parent ability's target object.
    /// Used for "its controller" in compound effects (e.g., "counter target spell. Its controller
    /// loses 2 life."). At resolution time, looks up the controller of the first parent target.
    ParentTargetController,
    /// CR 615.5 + CR 609.7: Resolves to the controller of the *prevented event's*
    /// damage source. Used by prevention follow-up sentences such as "the source's
    /// controller draws cards equal to the damage prevented this way" (Swans of
    /// Bryn Argoll) and "deals damage to that source's controller" (Deflecting
    /// Palm class). At resolution time, looks up `state.post_replacement_event_source`
    /// and returns its controller.
    ///
    /// Distinct from `ParentTargetController` (which resolves via the parent
    /// ability's target slot) and `TriggeringSpellController` (which resolves
    /// via `state.current_trigger_event`, which is `None` during post-replacement
    /// resolution). Architectural twin of the quantity-side `last_effect_count`
    /// fallback at `replacement.rs:317` — both stash event context that lives
    /// outside the trigger window. The parser never emits this variant directly;
    /// the prevention follow-up call site rewrites `ParentTargetController`
    /// → `PostReplacementSourceController` via `each_target_filter_mut` after
    /// `parse_effect_chain` returns, so the surface phrase "the source's
    /// controller" can stay consolidated in `parse_target` for non-prevention
    /// callers.
    PostReplacementSourceController,
    /// CR 506.3d: Resolves to the player being attacked by the source creature.
    /// Looked up from `state.combat.attackers` using the trigger's source_id.
    DefendingPlayer,
    /// Matches objects whose name equals the source's ChosenAttribute::CardName.
    /// Used for "card with the chosen name" patterns.
    HasChosenName,
    /// CR 609.7a: Matches the object stored as the source's chosen damage source.
    /// Resolution-time prevention effects should resolve this to `SpecificObject`
    /// when the shield is created so global shields do not depend on a live source.
    ChosenDamageSource,
    /// Matches objects with a specific hardcoded name.
    /// Used for "card named [literal]" patterns.
    Named {
        name: String,
    },
    /// CR 400.3: Resolves to the owner of the object referenced by `source_id`.
    /// Used for "its owner's library/hand/graveyard" phrasings where the acting
    /// player must be the card's owner, not its current controller (e.g.,
    /// Nexus of Fate's shuffle-back replacement when the card is under opposing
    /// control via Mind Control).
    Owner,
}

/// CR 102 + CR 119 + CR 402: Player axis for player-scoped quantity references.
///
/// Parameterizes the player whose hand size, life total, or other per-player
/// scalar is being read. Replaces sibling enum variants like
/// `LifeTotal` / `TargetLifeTotal` / `OpponentLifeTotal` and
/// `HandSize` / `OpponentHandSize` with a single parameterized form.
///
/// Categorical-boundary check (per the "Parameterize, don't proliferate"
/// principle): every variant is a player-relativity choice within a single
/// CR section. Aggregate scopes (`Opponent`, `AllPlayers`) compose
/// `AggregateFunction` to express max/min/sum across a population — they do
/// not introduce a new abstraction layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayerScope {
    /// CR 109.5 / CR 113.6: The controller of the source ability or effect.
    Controller,
    /// CR 109.4 + CR 113.6 + CR 115.1: The first player target of the
    /// resolving ability (read from `ability.targets`).
    Target,
    /// CR 102.2 + CR 102.3: All opponents of the controller, aggregated by
    /// `aggregate`. Existing `OpponentLifeTotal` / `OpponentHandSize`
    /// semantics correspond to `aggregate = Max`.
    Opponent { aggregate: AggregateFunction },
    /// CR 102.1: All players in the game (controller + opponents),
    /// aggregated by `aggregate`. Reserved for future cards that read
    /// "the highest life total among players" or similar cross-player
    /// extrema that include the controller.
    AllPlayers { aggregate: AggregateFunction },
    /// CR 303.4m + CR 613.4c: The controller of the object currently
    /// receiving a layer effect. Used for Aura/Equipment statics such as
    /// "enchanted creature gets +1/+1 for each card in its controller's
    /// hand", where "its" refers to the enchanted creature, not the Aura.
    RecipientController,
}

/// Scope selector for object-axis quantities (Round Π-5). Picks WHICH object
/// to read from when a `QuantityRef` (and future per-object conditions) is
/// per-object. Mirrors `PlayerScope` for the player axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ObjectScope {
    /// CR 109.5 / CR 113.6: The source object of the resolving ability —
    /// "this creature", "~", "it" (when "it" anaphors back to the source).
    Source,
    /// CR 115.1: The first object target of the resolving ability — "that
    /// creature", "the creature", "it" (when "it" anaphors back to a target).
    Target,
    /// CR 613.4c + CR 115.10: The object currently receiving an effect.
    /// In layer evaluation this is the per-object recipient. Outside layers,
    /// it resolves to the first object target when present, then to the source.
    /// Used for recipient-relative "its colors" boosts such as Blessing of
    /// the Nephilim and Civic Saber.
    Recipient,
    /// CR 603.2: The object referenced by the current trigger event.
    EventSource,
}

/// Source set for counting distinct card types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CardTypeSetSource {
    /// CR 109.2a + CR 400.1: Cards in a specific zone, scoped by player.
    Zone { zone: ZoneRef, scope: CountScope },
    /// CR 607.2a + CR 406.6: Cards exiled by the source's linked ability.
    ExiledBySource,
    /// CR 109.2: Objects matching a battlefield-style filter.
    Objects { filter: TargetFilter },
}

/// CR 601.2h: Which cast object a mana-spent quantity reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CastManaObjectScope {
    /// The ability's source object, or the entering object in ETB replacement context.
    SelfObject,
    /// The spell object referenced by the current trigger event.
    TriggeringSpell,
}

/// CR 106.3 + CR 601.2h: What to measure about mana spent to cast a spell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastManaSpentMetric {
    /// Total amount of mana spent.
    Total,
    /// Number of distinct colors of mana spent.
    DistinctColors,
    /// Amount of mana whose source matched the filter at payment time.
    FromSource { source_filter: TargetFilter },
}

/// A dynamic game quantity — a runtime lookup into the game state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum QuantityRef {
    /// CR 402: Number of cards in `player`'s hand. `PlayerScope::Controller`
    /// is the default reading; `Target`, `Opponent { .. }`, and `AllPlayers`
    /// cover targeted-player and cross-player aggregate variants.
    HandSize { player: PlayerScope },
    /// CR 119: `player`'s current life total. See `HandSize` for player-axis
    /// semantics.
    LifeTotal { player: PlayerScope },
    /// Number of cards in the controller's graveyard.
    GraveyardSize,
    /// Controller's life total minus the format's starting life total.
    /// Used for "N or more life more than your starting life total" conditions.
    LifeAboveStarting,
    /// CR 103.4: The format's starting life total (20 for Standard, 40 for Commander, etc.).
    StartingLifeTotal,
    /// Count of objects on the battlefield matching a filter.
    /// Used for "for each creature you control" and similar patterns.
    ObjectCount { filter: TargetFilter },
    /// CR 201.2 + CR 603.4: Count of distinct names among objects matching a
    /// filter. Used for "seven or more lands with different names" (Field of
    /// the Dead) and similar distinct-name threshold predicates. Two objects
    /// with the same name count once.
    ObjectCountDistinctNames { filter: TargetFilter },
    /// Count of players matching a player-level filter.
    /// Used for "for each opponent who lost life this turn" and similar patterns.
    PlayerCount { filter: PlayerFilter },
    /// Count of counters of a given type on the source object.
    /// Used for "for each [counter type] counter on ~" patterns.
    ///
    /// For counters on a *player* (experience, poison, rad, ticket), use
    /// [`QuantityRef::PlayerCounter`] instead.
    /// CR 122.1: Count of counters on an object, parameterized by scope and
    /// type filter (Round Π-5). Replaces the 4-variant cluster
    /// `CountersOnSelf` / `CountersOnTarget` / `AnyCountersOnSelf` /
    /// `AnyCountersOnTarget`.
    ///
    /// - `scope = Source`: counts on the source permanent ("counter on ~").
    /// - `scope = Target`: counts on the first object target ("counter on
    ///   that creature").
    /// - `counter_type = Some(ct)`: only counters of type `ct`.
    /// - `counter_type = None`: total counters of any type (e.g., Gemstone
    ///   Mine "When there are no counters on ~"; Nils, Discipline Enforcer
    ///   "the number of counters on that creature" per Scryfall ruling).
    ///
    /// For counters on a *player* (experience, poison, rad, ticket), use
    /// [`QuantityRef::PlayerCounter`] instead.
    CountersOn {
        scope: ObjectScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<String>,
    },
    /// CR 122.1: Total counters across all objects matching a filter.
    /// Used for phrases like "the number of +1/+1 counters on lands you control"
    /// (`counter_type: Some("P1P1")`) and "counters among artifacts and creatures
    /// you control" (`counter_type: None`, sums across every counter type).
    CountersOnObjects {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<String>,
        filter: TargetFilter,
    },
    /// CR 122.1: Count of a named player-counter kind on a player (or summed across
    /// scoped players). Distinct from `CountersOnSelf` / `CountersOnTarget` /
    /// `CountersOnObjects`, which count counters on *objects* — player counters
    /// live on `Player`.
    ///
    /// Kind-specific CR references:
    /// - Poison: CR 122.1f + CR 704.5c (ten-or-more SBA).
    /// - Rad:    CR 122.1i + CR 728.
    /// - Experience and Ticket are covered only by the generic CR 122.1.
    ///
    /// Scope is currently limited to `Controller`, `Opponents`, and `All`.
    /// Targeted-player variants ("target opponent has N experience counters")
    /// are not yet represented; extending `CountScope` with a `TargetPlayer`
    /// arm is a future change when a card forces it.
    PlayerCounter {
        kind: PlayerCounterKind,
        scope: CountScope,
    },
    /// A variable reference (e.g. "X") resolved from spell payment or "that much" from prior effect.
    Variable { name: String },
    /// CR 208.1 + CR 113.6: Current power of an object, scoped via ObjectScope
    /// (Round Π-6). Replaces the `SelfPower` / `TargetPower` sibling pair.
    /// `Source` reads the source object's power (post-layer); `Target` reads
    /// the first object target's power.
    Power { scope: ObjectScope },
    /// CR 208.1 + CR 113.6: Current toughness of an object, scoped via
    /// ObjectScope (Round Π-6). Mirrors `Power`. Replaces the `SelfToughness`
    /// variant.
    Toughness { scope: ObjectScope },
    /// CR 202.3: Mana value of an object, scoped via ObjectScope.
    /// `Source` is the resolving ability's source; `Target` is the first object
    /// target. Used by source/target-relative mana-value filters such as
    /// "with the same mana value as that spell".
    ObjectManaValue { scope: ObjectScope },
    /// CR 105.1 + CR 105.2: Number of colors of an object, scoped via
    /// ObjectScope. Counts the object's current W/U/B/R/G color set; colorless
    /// objects return 0. `Recipient` preserves "for each of its colors" on
    /// static/layered boosts by binding to the affected object rather than the
    /// Aura, Equipment, or anthem source.
    ObjectColorCount { scope: ObjectScope },
    /// CR 201.1 + CR 201.2: Number of words in an object's current name,
    /// scoped via ObjectScope. `Recipient` preserves Aura/Equipment
    /// "enchanted/equipped creature gets +N/+N for each word in its name" by
    /// binding to the affected object rather than the Aura or Equipment source.
    ObjectNameWordCount { scope: ObjectScope },
    /// CR 107.4 + CR 202.1: Count colored mana symbols in an object's mana
    /// cost. Hybrid, monocolored hybrid, Phyrexian, hybrid Phyrexian, and
    /// colorless hybrid symbols count when they contain `color`, via
    /// `ManaCostShard::contributes_to`. `Recipient` preserves per-affected
    /// layer boosts such as "gets +1/+1 for each white mana symbol in its mana
    /// cost" by binding to the object currently receiving the effect.
    ManaSymbolsInManaCost {
        scope: ObjectScope,
        color: ManaColor,
    },
    /// CR 202.3: The mana value of the source object — i.e. the object passed
    /// as `source` to `resolve_quantity`. For an alt-cost cast (CR 118.9) this
    /// is the spell-being-cast, so "pay life equal to its mana value" reads
    /// the right value at cost-payment time. Distinct from
    /// `EventContextSourceManaValue` (which reads the triggering source via
    /// `current_trigger_event`); that ref returns 0 outside trigger
    /// resolution, whereas this one is correct any time the resolver has a
    /// `source_id` (cost payment, ability resolution, etc.).
    SelfManaValue,
    /// CR 107.3e: Aggregate query (max/min/sum) over a property of battlefield objects.
    Aggregate {
        function: AggregateFunction,
        property: ObjectProperty,
        filter: TargetFilter,
    },
    /// Card count in a specific zone of the first targeted player.
    /// Generalized for library, graveyard, exile, etc.
    /// Used for "half of target player's library" and similar patterns.
    TargetZoneCardCount { zone: ZoneRef },
    /// CR 700.5: Devotion to one or more colors.
    Devotion { colors: DevotionColors },
    /// CR 205.2a: Count distinct card types (CoreType) across a parameterized
    /// source set. Covers zone cards, linked-exile cards, and matching objects
    /// without proliferating card-type-count siblings.
    DistinctCardTypes { source: CardTypeSetSource },
    /// CR 406.6 + CR 607.1: Count of cards currently in exile that are linked to the source
    /// via its exile-linked ability. Used by "as long as there are N or more cards exiled
    /// with ~" conditional statics (Veteran Survivor, etc.) — composes with
    /// `StaticCondition::QuantityComparison` rather than requiring a dedicated variant.
    CardsExiledBySource,
    /// CR 604.3: Count cards in a zone matching optional type filters.
    /// Empty card_types means all cards. Multiple entries = OR (any match).
    /// "creature cards in your graveyard" → zone=Graveyard, card_types=[Creature], scope=Controller
    ZoneCardCount {
        zone: ZoneRef,
        card_types: Vec<TypeFilter>,
        scope: CountScope,
    },
    /// CR 305.6: Count distinct basic land types (Plains/Island/Swamp/Mountain/Forest)
    /// among lands controlled by the referenced player. Used by Domain.
    BasicLandTypeCount { controller: ControllerRef },
    /// CR 609.3: Count of objects moved by the preceding effect in the sub_ability chain.
    /// Only valid during sub-ability chain resolution; returns 0 outside that context.
    /// The caller (token resolver) is responsible for consuming the tracked set after use.
    TrackedSetSize,
    /// CR 400.7 + CR 608.2c: Number of cards exiled from a hand by the immediately
    /// preceding `Effect::ChangeZoneAll` resolution. Read by Deadly Cover-Up's
    /// "draws a card for each card exiled from their hand this way." The counter
    /// is tracked in `state.exiled_from_hand_this_resolution` and reset at the
    /// top of each player action and at the start of each top-level ability chain.
    ExiledFromHandThisResolution,
    /// CR 609.3: Numeric amount produced by the preceding effect in the sub_ability chain.
    /// Used for patterns where a sub_ability references the parent effect's numeric
    /// result (life lost, damage dealt, counters removed).
    PreviousEffectAmount,
    /// CR 118.4 + CR 119.3: Amount of life lost this turn, scoped by `player`
    /// per the workspace "Parameterize, don't proliferate" principle (Round Π-3).
    ///
    /// - `Controller`: controller's life lost this turn (`p.life_lost_this_turn`).
    ///   Used by "as long as you've lost life this turn".
    /// - `Opponent { aggregate: Sum }`: total life lost across opponents
    ///   (legacy `OpponentLifeLostThisTurn`). Used by "if an opponent lost life
    ///   this turn".
    /// - `AllPlayers { aggregate: Max }`: maximum life lost by any single player
    ///   (legacy `MaxLifeLostThisTurnAcrossPlayers`). Used by "if a player lost
    ///   N or more life this turn" (Y'shtola, Knight of the Ebon Legion).
    /// - `Target`: reserved — no current cards reference a targeted player's
    ///   life-lost-this-turn, but the slot exists for symmetry with `LifeTotal`
    ///   / `HandSize`.
    LifeLostThisTurn { player: PlayerScope },
    /// CR 700.8: Number of creatures in `player`'s party. A party consists of
    /// up to one Cleric, one Rogue, one Warrior, and one Wizard creature
    /// `player` controls; the resolver maximizes the count when creatures have
    /// multiple party-relevant types (CR 700.8b). The result is bounded
    /// `0..=4`. `PlayerScope::Controller` is the default reading
    /// ("your party"); `Target`/`Opponent { .. }`/`AllPlayers { .. }` cover
    /// targeted-player and cross-player aggregate variants per the same axis
    /// used by `LifeTotal`/`HandSize`.
    PartySize { player: PlayerScope },
    /// CR 702.179f: The controller's current speed, treating no speed as 0.
    Speed,
    /// CR 603.7c: Numeric value from the triggering event.
    /// Extracts amount/count from DamageDealt, LifeChanged, CardsDrawn, CounterAdded, etc.
    EventContextAmount,
    /// CR 603.7c: Power of the source object from the triggering event.
    /// Falls back to LKI cache for dies/leaves-battlefield triggers.
    EventContextSourcePower,
    /// CR 603.7c: Toughness of the source object from the triggering event.
    /// Falls back to LKI cache for dies/leaves-battlefield triggers.
    EventContextSourceToughness,
    /// CR 603.7c: Mana value of the source object from the triggering event.
    EventContextSourceManaValue,
    /// CR 202.3 + CR 118.8 + CR 701.21: Mana value of the object that was
    /// sacrificed, exiled, or otherwise paid as part of the cost of the
    /// resolving spell or ability. The mana value is captured at cost-payment
    /// time (CR 117.1) — before the object leaves the battlefield — and stored
    /// on `ResolvedAbility.cost_paid_object_mana_value`.
    ///
    /// Covers the "sac-for-mana-by-property" / "exile-for-mana-by-property"
    /// class: Food Chain ("1 plus the exiled creature's mana value"),
    /// Burnt Offering, Metamorphosis ("the sacrificed creature's mana value"),
    /// and any future cards that read the cost-paid object's mana value.
    ///
    /// Distinct from `EventContextSourceManaValue` (which reads the triggering
    /// event's source). A cost-paid object is not a triggering-event source —
    /// activation/spell-cast announcements (CR 117.1, CR 601.2) are not events
    /// in the triggered-ability sense.
    CostPaidObjectManaValue,
    /// CR 603.10a + CR 603.6e: Count of attachments of a given kind that were attached
    /// to the leaving-battlefield object at the moment it left, optionally filtered by
    /// attachment controller. Resolved via the triggering `ZoneChangeRecord`'s
    /// `attachments` snapshot (look-back semantics).
    ///
    /// Used for Hateful Eidolon ("draw a card for each Aura you controlled that was
    /// attached to it") — `kind: Aura`, `controller: Some(You)`.
    AttachmentsOnLeavingObject {
        kind: AttachmentKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
    },
    /// CR 107.3a + CR 601.2b + CR 603.2: The announced value of `{X}` for the
    /// triggering spell. Reads `GameObject::cost_x_paid` on the spell object
    /// referenced by `current_trigger_event` (populated during
    /// `determine_total_cost` and persisted through stack → battlefield).
    /// Used by triggers of the form "whenever you cast your first spell with
    /// {X} in its mana cost each turn, [do something with X]" (e.g. Nev the
    /// Practical Dean's "put X +1/+1 counters on Nev").
    EventContextSourceCostX,
    /// CR 117.1: Number of spells cast this turn by a specific player,
    /// optionally filtered by spell characteristics. `None` = all spells.
    /// Resolved against the controller (or scope_player in per-player iteration).
    SpellsCastThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
    /// Count of permanents matching filter that entered the battlefield
    /// under the controller's control this turn.
    EnteredThisTurn { filter: TargetFilter },
    /// CR 710.2: Number of crimes the controller has committed this turn.
    CrimesCommittedThisTurn,
    /// CR 119.4: Amount of life gained this turn, scoped by `player` per the
    /// workspace "Parameterize, don't proliferate" principle (Round Π-4 — mirrors
    /// `LifeLostThisTurn`'s Π-3 lift). `Controller` reads
    /// `p.life_gained_this_turn` directly; `Opponent { Sum }` totals across
    /// opponents (Needlebite Trap "if an opponent gained life this turn").
    LifeGainedThisTurn { player: PlayerScope },
    /// CR 500: Number of turns this player has taken so far in the game.
    /// Resolved against the controller/scope player.
    TurnsTaken,
    /// CR 400.7 + CR 700.4: Count this turn's zone-change records that match
    /// an origin/destination and the object's last-known characteristics.
    /// Used by Revolt, Morbid, and subtype-specific dies counts such as Zubera.
    ZoneChangeCountThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Zone>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Zone>,
        filter: TargetFilter,
    },
    /// A number chosen as the source entered the battlefield (e.g., Talion, the Kindly Lord).
    /// Resolved from the source object's `ChosenAttribute::Number`.
    ChosenNumber,
    /// CR 508.1a: Number of times the controller declared attackers this turn.
    /// Used for "if you attacked this turn" conditions.
    AttackedThisTurn,
    /// CR 603.4: Whether the controller descended this turn (permanent card entered graveyard).
    DescendedThisTurn,
    /// CR 117.1: Number of spells cast last turn (by any player).
    /// Used for werewolf transform conditions.
    SpellsCastLastTurn,
    /// CR 122.1 + CR 122.6: Count counters put this turn by `actor` on objects
    /// matching `target`. `counters` narrows the counter kind; `CounterMatch::Any`
    /// counts every counter type. This parameterizes the legacy boolean
    /// "counter added this turn" slot for Hornbeetle/Paladin-style dynamic counts
    /// without adding counter-type or recipient-filter sibling variants.
    CounterAddedThisTurn {
        actor: CountScope,
        counters: crate::types::counter::CounterMatch,
        target: TargetFilter,
    },
    /// CR 701.9 + CR 603.4: Whether any opponent of the controller discarded a
    /// card this turn. Resolves to 1 when at least one opponent is recorded in
    /// `players_who_discarded_card_this_turn`, else 0. Used by intervening-if
    /// clauses like "if an opponent discarded a card this turn" (Tinybones,
    /// Trinket Thief and similar).
    OpponentDiscardedCardThisTurn,
    /// CR 309.7: Number of dungeons the controller has completed.
    DungeonsCompleted,
    /// CR 107.3m: The value of X paid for the spell that produced the source
    /// object. Survives the stack → battlefield transition (stored on the
    /// GameObject as `cost_x_paid`), so ETB replacement effects ("enters with
    /// X counters") and ETB triggered abilities referring to X resolve against
    /// the actual paid amount. Distinct from `Variable { name: "X" }` which
    /// only resolves while the ability is on the stack with `chosen_x` set.
    CostXPaid,
    /// CR 702.33b + CR 702.33c: Number of kicker costs paid for the source
    /// spell. For multikicker, each repeated payment contributes one entry.
    KickerCount,
    /// CR 702.51c: Number of creatures that convoked the source spell or the
    /// spell that became the source permanent. Reads `GameObject::convoked_creatures`;
    /// ETB replacement contexts resolve against the entering object.
    ConvokedCreatureCount,
    /// CR 106.3 + CR 601.2h: Mana spent to cast a spell, parameterized by
    /// which cast object is being measured and which spent-mana metric is
    /// needed. Covers total amount, distinct colors, and source-qualified
    /// amounts without proliferating sibling variants.
    ManaSpentToCast {
        scope: CastManaObjectScope,
        metric: CastManaSpentMetric,
    },
    /// CR 903.4 + CR 903.4f: Number of distinct colors in the controller's
    /// commander(s)' combined color identity. Color identity is the union of
    /// every commander's mana-cost colors plus color indicator/CDA colors.
    /// Resolves to 0 when the controller has no commander (CR 903.4f: "that
    /// quality is undefined if that player doesn't have a commander"). Used
    /// by War Room's "pay life equal to the number of colors in your
    /// commanders' color identity" activation cost.
    ColorsInCommandersColorIdentity,
    /// CR 903.8: Number of times the controller has cast their commander(s)
    /// from the command zone this game. Used by commander storm effects such
    /// as "copy it for each time you've cast your commander from the command
    /// zone this game."
    CommanderCastFromCommandZoneCount,
    /// CR 106.1 + CR 109.1: Number of distinct colors among permanents matching
    /// a filter. "Gold", "multicolor", and "colorless" are not colors (CR 105.1),
    /// so each of W/U/B/R/G is counted at most once. Used by Faeburrow Elder's
    /// "+1/+1 for each color among permanents you control" CDA and its companion
    /// mana ability. Composes with `ObjectCount`-style filter predicates and is
    /// the dual to `ManaProduction::DistinctColorsAmongPermanents`.
    DistinctColorsAmongPermanents { filter: TargetFilter },
}

/// CR 107.1a: Rounding direction for fractional Oracle-text expressions.
/// Every "half X" phrase in Oracle text specifies whether to round up or
/// down; this enum records that choice verbatim so resolution is deterministic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum RoundingMode {
    Up,
    Down,
}

/// CR 107.3e: Aggregate function applied over a set of objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AggregateFunction {
    Max,
    Min,
    Sum,
}

/// A measurable property of a game object for aggregate queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectProperty {
    Power,
    Toughness,
    ManaValue,
}

/// CR 102.1 / CR 102.2 / CR 109.5: Relative player set for player filters that
/// compose with an independent condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayerRelation {
    /// The controller of the effect or quantity.
    Controller,
    /// All opponents of the controller.
    Opponent,
    /// All players in the game.
    All,
}

/// A filter matching players by game-state conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayerFilter {
    /// The controller of the effect or quantity.
    Controller,
    /// All opponents of the controller.
    Opponent,
    /// Each opponent who lost life this turn (life_lost_this_turn > 0).
    OpponentLostLife,
    /// Each opponent who gained life this turn (life_gained_this_turn > 0).
    OpponentGainedLife,
    /// All players.
    All,
    /// CR 702.179f: Each player whose speed is tied for the highest speed among players.
    HighestSpeed,
    /// "each player who [verb]ed a card this way" — scoped to players who owned objects
    /// that changed zones in the preceding effect (tracked via `last_zone_changed_ids`).
    ZoneChangedThisWay,
    /// CR 608.2c + CR 109.5: Players matching `relation` who performed `action`
    /// during the current top-level resolution. Used by "for each opponent who
    /// searched their library this way" and analogous player-action references.
    PerformedActionThisWay {
        relation: PlayerRelation,
        action: PlayerActionKind,
    },
    /// Each owner of a card currently exiled with the ability's source.
    /// Used by linked-exile follow-ups like Skyclave Apparition's leaves trigger.
    OwnersOfCardsExiledBySource,
    /// CR 113.3c + CR 603.2: The player identified by `state.current_trigger_event`. Used to route
    /// "they [verb]" effects on triggers whose subject is a player (e.g. Firemane
    /// Commando's "another player ... they draw a card").
    TriggeringPlayer,
    /// CR 120.3 + CR 603.2c: Each opponent other than the player identified by
    /// `state.current_trigger_event`. Used by "each other opponent" phrasing on
    /// damage triggers — the triggering opponent has already received the source's
    /// damage in the same chain (e.g. Hydra Omnivore's "Whenever ~ deals combat
    /// damage to an opponent, it deals that much damage to each other opponent.").
    /// The "other" anaphors back to the triggering opponent named in the trigger
    /// event clause. Falls back to plain `Opponent` semantics when no trigger
    /// event is in scope (i.e. only excludes the controller).
    OpponentOtherThanTriggering,
}

/// An expression that produces an integer for quantity comparisons.
/// Either a dynamic game-state lookup or a literal constant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum QuantityExpr {
    /// A dynamic quantity looked up from the current game state.
    Ref { qty: QuantityRef },
    /// A literal integer constant.
    Fixed { value: i32 },
    /// CR 107.1a: "Half X, rounded up/down" — divides the inner expression by
    /// 2 in the rounding direction specified by the Oracle text.
    HalfRounded {
        inner: Box<QuantityExpr>,
        rounding: RoundingMode,
    },
    /// CR 604.3: Base expression plus a fixed integer offset.
    /// "N plus the number of X" / "that number plus N" patterns.
    Offset {
        inner: Box<QuantityExpr>,
        offset: i32,
    },
    /// "Twice the number of X" / "N times X" / negation via factor: -1.
    Multiply {
        factor: i32,
        inner: Box<QuantityExpr>,
    },
    /// Sum of N independent quantity expressions. Used for conjunctive
    /// "for each X and each Y" patterns where the two filters span
    /// disjoint zones or axes that cannot be expressed as a single
    /// `TargetFilter::Or` (e.g., Alrund's "+1/+1 for each card in your
    /// hand and each foretold card you own in exile").
    Sum { exprs: Vec<QuantityExpr> },
    /// CR 107.1c + CR 608.2d: "Up to N" — the affected player chooses any
    /// integer in `0..=resolve(max)` at resolution time. Used by
    /// `Effect::Draw`, `Effect::Sacrifice`, `Effect::Discard`, and
    /// `Effect::SearchLibrary` for the "draw / sacrifice / discard / search
    /// for up to N" Oracle text class. The 4 specific resolvers peel this
    /// wrapper via `QuantityExpr::peel_up_to` and propagate the bool to
    /// their `WaitingFor::*Choice` runtime state.
    ///
    /// Layered above `QuantityExpr::Ref`/`Fixed`/`HalfRounded`/etc. so the
    /// upper bound itself can be a dynamic game-state quantity (e.g. "draw
    /// up to your hand size cards" → `UpTo { max: Ref { qty: HandSize } }`,
    /// "sacrifice up to half your creatures" → `UpTo { max: HalfRounded {
    /// inner: Ref { qty: ObjectCount {..} }, rounding: Down } }`).
    ///
    /// Generic quantity resolvers (`resolve_quantity`,
    /// `resolve_quantity_with_targets`, etc.) treat `UpTo` transparently —
    /// they resolve to the upper bound as if the `UpTo` wrapper were not
    /// present. This is safe because the only places where the
    /// "may pick fewer" semantics matter are the 4 specific effect
    /// resolvers, which extract the flag explicitly via `peel_up_to`.
    ///
    /// Invariant: `max` MUST NOT itself be `UpTo` — nesting is meaningless
    /// ("up to up to N" is just "up to N"). Always construct via the
    /// `QuantityExpr::up_to` helper which debug-asserts this invariant.
    UpTo { max: Box<QuantityExpr> },
}

impl QuantityExpr {
    /// Construct an `UpTo { max }` expression, debug-asserting the
    /// non-nesting invariant. Always use this rather than the raw struct
    /// literal.
    pub fn up_to(max: QuantityExpr) -> Self {
        debug_assert!(
            !matches!(max, QuantityExpr::UpTo { .. }),
            "QuantityExpr::UpTo cannot wrap another UpTo — \"up to up to N\" is meaningless",
        );
        QuantityExpr::UpTo { max: Box::new(max) }
    }

    /// Peel an outer `UpTo` wrapper, returning `(inner_max, true)` if this
    /// expression was an `UpTo`, otherwise `(self, false)`. The 4 effect
    /// resolvers (Draw, Sacrifice, Discard, SearchLibrary) all call this to
    /// derive the upper-bound expression and the "may pick fewer" flag they
    /// propagate to their `WaitingFor` state.
    pub fn peel_up_to(&self) -> (&QuantityExpr, bool) {
        match self {
            QuantityExpr::UpTo { max } => (max.as_ref(), true),
            other => (other, false),
        }
    }

    /// Returns true if this expression is an `UpTo` wrapper.
    pub fn is_up_to(&self) -> bool {
        matches!(self, QuantityExpr::UpTo { .. })
    }
}

/// Comparison operator used in static conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Comparator {
    GT,
    LT,
    GE,
    LE,
    EQ,
    NE,
}

impl Comparator {
    pub fn evaluate(self, lhs: i32, rhs: i32) -> bool {
        match self {
            Comparator::GT => lhs > rhs,
            Comparator::LT => lhs < rhs,
            Comparator::GE => lhs >= rhs,
            Comparator::LE => lhs <= rhs,
            Comparator::EQ => lhs == rhs,
            Comparator::NE => lhs != rhs,
        }
    }

    /// Return the logical negation of this comparator.
    /// Used when bridging `Not(QuantityComparison)` to `AbilityCondition::QuantityCheck`.
    pub fn negate(self) -> Self {
        match self {
            Comparator::GT => Comparator::LE,
            Comparator::LT => Comparator::GE,
            Comparator::GE => Comparator::LT,
            Comparator::LE => Comparator::GT,
            Comparator::EQ => Comparator::NE,
            Comparator::NE => Comparator::EQ,
        }
    }
}

/// CR 719.1: Condition that must be met for a Case to become solved.
/// Evaluated by the auto-solve trigger at end step (CR 719.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SolveCondition {
    /// "You control no suspected Skeletons" → count matching objects == 0
    ObjectCount {
        filter: TargetFilter,
        comparator: Comparator,
        threshold: u32,
    },
    /// Fallback for conditions the parser cannot decompose.
    Text { description: String },
}

/// CR 508.1h + CR 509.1d: How an `UnlessPay` combat-tax cost scales when multiple
/// creatures are covered by the restriction.
///
/// - `Flat`: the cost is paid once regardless of how many affected creatures there are
///   (e.g., Brainwash — a single enchanted creature, cost {3}).
/// - `PerAffectedCreature`: the cost is paid per creature that the restriction applies
///   to in the declared attack/block (e.g., Ghostly Prison — "pays {2} for each creature
///   they control that's attacking you").
/// - `PerQuantityRef`: the cost is paid once, scaled by the resolved value of the
///   dynamic `QuantityRef` (useful for "{X}" where X is defined as a game-state count
///   without a per-creature multiplier).
/// - `PerAffectedAndQuantityRef`: the cost is multiplied by the resolved quantity AND
///   paid once per affected creature (e.g., Sphere of Safety — "pays {X} for each of
///   those creatures, where X is the number of enchantments you control").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type", content = "data")]
pub enum UnlessPayScaling {
    #[default]
    Flat,
    PerAffectedCreature,
    PerQuantityRef {
        quantity: QuantityRef,
    },
    PerAffectedAndQuantityRef {
        quantity: QuantityRef,
    },
    /// CR 118.12a + CR 202.3e: Per-affected-creature cost where the scaling quantity
    /// is resolved against EACH affected creature independently (e.g., Nils, Discipline
    /// Enforcer — "pays {X}, where X is the number of counters on that creature" —
    /// each declared attacker pays base_cost × counters on itself).
    ///
    /// Distinct from `PerQuantityRef` (resolved once for all creatures) and
    /// `PerAffectedAndQuantityRef` (resolved once, then multiplied per creature). The
    /// quantity is resolved per-creature using that creature as the `TargetRef::Object`
    /// during resolution.
    PerAffectedWithRef {
        quantity: QuantityRef,
    },
}

impl UnlessPayScaling {
    pub fn is_flat(&self) -> bool {
        matches!(self, UnlessPayScaling::Flat)
    }
}

/// Condition for static ability applicability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum StaticCondition {
    DevotionGE {
        colors: Vec<ManaColor>,
        threshold: u32,
    },
    IsPresent {
        #[serde(default)]
        filter: Option<TargetFilter>,
    },
    /// True when the source object's chosen color matches the given color.
    /// Used for cards that choose a color on ETB and have color-conditional effects.
    ChosenColorIs {
        color: ManaColor,
    },
    /// True when a measurable quantity expression satisfies a comparison against another.
    /// Supports quantity-vs-quantity ("hand size > life total") and quantity-vs-constant
    /// ("life above starting >= 7") via `QuantityExpr::Fixed`.
    QuantityComparison {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
    },
    /// CR 702.178a: The relevant player has max speed.
    HasMaxSpeed,
    /// CR 702.178a + CR 702.179f: The relevant player's speed is at least the threshold.
    SpeedGE {
        threshold: u8,
    },
    /// True when ALL sub-conditions are satisfied.
    And {
        conditions: Vec<StaticCondition>,
    },
    /// True when ANY sub-condition is satisfied.
    Or {
        conditions: Vec<StaticCondition>,
    },
    /// True when the inner condition is NOT satisfied.
    /// Follows the existing And/Or combinator pattern.
    /// Used for "as long as ~ is untapped" → `Not(SourceIsTapped)`.
    Not {
        condition: Box<StaticCondition>,
    },
    /// CR 731.1: True when the game has the given day/night designation.
    DayNightIs {
        state: crate::types::game_state::DayNight,
    },
    /// CR 122.1: True when the source object has at least `minimum` (and at most `maximum`,
    /// if specified) counters matching `counters`. `CounterMatch::Any` sums across every
    /// counter type on the object (for Oracle text that refers to "a counter on it" with
    /// no type specified); `CounterMatch::OfType(ct)` matches only counters of that type.
    /// Used for level-up ranges (CR 711.2a + CR 711.2b) and counter-gated statics
    /// (e.g. Demon Wall: "as long as this creature has a counter on it").
    HasCounters {
        counters: CounterMatch,
        minimum: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u32>,
    },
    /// CR 716.2a: True when the source Class enchantment is at or above the given level.
    /// Class level is a dedicated field (not a counter), so proliferate does not interact.
    ClassLevelGE {
        level: u8,
    },
    /// CR 509.1b: True when the defending player controls a permanent matching the filter.
    /// Used for conditional evasion ("can't be blocked as long as defending player controls
    /// an artifact"). Distinct from `IsPresent` because it references the defending player,
    /// not the source's controller.
    DefendingPlayerControls {
        filter: TargetFilter,
    },
    /// CR 506.5: True when the source creature is the only attacking creature.
    SourceAttackingAlone,
    /// CR 508.1k + CR 506.4: True when the source creature is currently an attacking creature.
    /// CR 508.1k defines "attacking creature" (becomes one when declared, remains until removed
    /// or combat ends). CR 506.4 defines when a creature stops being an attacker.
    SourceIsAttacking,
    /// CR 509.1g + CR 506.4: True when the source creature is currently a blocking creature.
    /// CR 509.1g defines "blocking creature" (becomes one when declared, remains until removed
    /// or combat ends). CR 506.4 defines when a creature stops being a blocker.
    SourceIsBlocking,
    /// CR 509.1h: True when the source creature has been blocked this combat.
    /// Once a creature is blocked, it remains blocked for the rest of combat even
    /// if all its blockers leave — mirrors `AttackerInfo.blocked` (sticky flag).
    SourceIsBlocked,
    /// CR 725.1: True when the controller is the monarch.
    IsMonarch,
    /// CR 702.131a: True when the controller has the city's blessing (Ascend).
    HasCityBlessing,
    /// CR 309.7: True when the controller has completed at least one dungeon.
    /// Used by "as long as you've completed a dungeon" statics (Nadaar, etc.).
    CompletedADungeon,
    /// CR 701.27: True when any opponent has at least this many poison counters.
    OpponentPoisonAtLeast {
        count: u32,
    },
    /// CR 118.12a + CR 508.1d + CR 509.1c: "unless [player] pays [cost]" — an optional cost
    /// condition attached to a combat restriction (attack tax / block tax).
    ///
    /// `cost` is the base cost per activation of the condition. `scaling` determines how the
    /// total is computed when the static applies across multiple creatures — e.g.
    /// Ghostly Prison scales per affected creature, Sphere of Safety scales per enchantment
    /// the defender controls, and Brainwash scales flat.
    ///
    /// `layers::evaluate_condition` returns `false` for this variant (restriction active) —
    /// the per-attacker / per-blocker optional cost payment round-trip is performed at
    /// declaration time via `WaitingFor::CombatTaxPayment`, not inside the pure layer
    /// evaluator.
    UnlessPay {
        cost: ManaCost,
        #[serde(default, skip_serializing_if = "UnlessPayScaling::is_flat")]
        scaling: UnlessPayScaling,
    },
    /// Condition text that the parser could not yet decompose into a typed variant.
    /// Evaluated permissively (always true) so the static effect still applies.
    Unrecognized {
        text: String,
    },
    DuringYourTurn,
    /// CR 400.7: True when the source permanent entered the battlefield this turn.
    /// Used for "as long as this [permanent] entered this turn" conditional statics.
    SourceEnteredThisTurn,
    /// CR 701.54a: True when this creature is the ring-bearer for its controller.
    IsRingBearer,
    /// CR 701.54c: True when the controller's ring level is at least this value (0-indexed).
    RingLevelAtLeast {
        level: u8,
    },
    /// CR 903.3: True when the controller controls at least one of their commander(s).
    /// Used for Lieutenant mechanic ("if you control your commander").
    ControlsCommander,
    /// CR 611.2b: True when the source object is tapped.
    /// Used for "for as long as ~ remains tapped" duration conditions.
    SourceIsTapped,
    /// CR 702.62a + CR 611.2b: True when the source object's current controller
    /// equals the stored player. General-purpose "while you control this"
    /// predicate; the runtime-installed Suspend haste static uses
    /// `Duration::ForAsLongAs { SourceControllerEquals { player } }` so haste
    /// lapses the moment another player gains control of the suspended
    /// creature (Threaten, Mind Control, etc.).
    SourceControllerEquals {
        player: super::player::PlayerId,
    },
    /// CR 301.5a: True when at least one Equipment is attached to the source object.
    /// Used for "as long as ~ is equipped" statics (Auriok Steelshaper, etc.).
    SourceIsEquipped,
    /// CR 701.37: True when the source permanent is monstrous.
    /// Read from `GameObject::monstrous` (existing bool field).
    /// Used for "as long as this creature is monstrous" statics (Fleecemane Lion, etc.).
    SourceIsMonstrous,
    /// CR 301.5 + CR 303.4: True when the source Aura/Equipment is attached to a creature.
    /// All observed Oracle text uses "attached to a creature"; no filter parameter needed.
    /// Used for "as long as this Equipment is attached to a creature" statics (Pact Weapon, etc.).
    SourceAttachedToCreature,
    /// CR 608.2c: True when the source object matches the filter (type/subtype check).
    /// Used by leveler-style cards (e.g., Figure of Fable) where each activated ability
    /// gates on the source's current type. Bridges to `AbilityCondition::SourceMatchesFilter`.
    SourceMatchesFilter {
        filter: TargetFilter,
    },
    /// CR 113.6b: True when the source card is in the specified zone.
    /// Used for "as long as ~ is in your graveyard" / "this card is in your graveyard" conditions.
    SourceInZone {
        zone: crate::types::zones::Zone,
    },
    /// CR 708.2 + CR 707.2: True when the creature this Aura/Equipment is attached to is
    /// face-down. Resolves against the attached-to object's `face_down` status. Used by
    /// "as long as enchanted creature is face down" gated statics (Unable to Scream, etc.).
    EnchantedIsFaceDown,
    None,
}

// ---------------------------------------------------------------------------
// ParsedCondition — typed restriction conditions parsed at build time
// ---------------------------------------------------------------------------

/// CR 601.3 / CR 602.5: A fully typed condition for casting/activation restrictions.
/// Parsed at Oracle parse time to eliminate runtime reparsing.
/// `Option<ParsedCondition>` is used at storage sites — `None` means the parser
/// could not decompose the condition (permissive fallback: evaluates to `true`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ParsedCondition {
    SourceInZone {
        zone: Zone,
    },
    /// CR 508.1a: The source creature is currently attacking.
    SourceIsAttacking,
    SourceIsAttackingOrBlocking,
    /// CR 509.1h: The source creature is blocked.
    SourceIsBlocked,
    SourcePowerAtLeast {
        minimum: i32,
    },
    SourceHasCounterAtLeast {
        counter_type: CounterType,
        count: u32,
    },
    SourceHasNoCounter {
        counter_type: CounterType,
    },
    /// CR 302.6: Source entered the battlefield this turn.
    SourceEnteredThisTurn,
    SourceIsCreature,
    SourceUntappedAttachedTo {
        required_type: CoreType,
    },
    SourceLacksKeyword {
        keyword: Keyword,
    },
    SourceIsColor {
        color: ManaColor,
    },
    FirstSpellThisGame,
    OpponentSearchedLibraryThisTurn,
    BeenAttackedThisStep,
    ZoneCardCountAtLeast {
        zone: crate::types::zones::Zone,
        count: usize,
    },
    ZoneCardTypeCountAtLeast {
        zone: crate::types::zones::Zone,
        count: usize,
    },
    ZoneSubtypeCardCountAtLeast {
        zone: crate::types::zones::Zone,
        subtype: String,
        count: usize,
    },
    OpponentPoisonAtLeast {
        count: u32,
    },
    HandSizeExact {
        count: usize,
    },
    HandSizeOneOf {
        counts: Vec<usize>,
    },
    /// Compares a player-relative quantity against each opponent's quantity.
    /// The comparison must hold for ALL opponents.
    QuantityVsEachOpponent {
        lhs: QuantityRef,
        comparator: Comparator,
        rhs: QuantityRef,
    },
    CreaturesYouControlTotalPowerAtLeast {
        minimum: i32,
    },
    YouControlLandSubtypeAny {
        subtypes: Vec<String>,
    },
    YouControlSubtypeCountAtLeast {
        subtype: String,
        count: usize,
    },
    YouControlCoreTypeCountAtLeast {
        core_type: CoreType,
        count: usize,
    },
    YouControlColorPermanentCountAtLeast {
        color: ManaColor,
        count: usize,
    },
    YouControlSubtypeOrGraveyardCardSubtype {
        subtype: String,
    },
    YouControlLegendaryCreature,
    YouControlNamedPlaneswalker {
        name: String,
    },
    YouControlCreatureWithKeyword {
        keyword: Keyword,
    },
    YouControlCreatureWithPowerAtLeast {
        minimum: i32,
    },
    YouControlCreatureWithPt {
        power: i32,
        toughness: i32,
    },
    YouControlAnotherColorlessCreature,
    YouControlSnowPermanentCountAtLeast {
        count: usize,
    },
    YouControlDifferentPowerCreatureCountAtLeast {
        count: usize,
    },
    YouControlLandsWithSameNameAtLeast {
        count: usize,
    },
    YouControlNoCreatures,
    YouAttackedThisTurn,
    YouAttackedWithAtLeast {
        count: u32,
    },
    /// CR 602.5b + CR 109.2b: True when the player cast a spell matching
    /// `filter` this turn. `None` matches any spell.
    YouCastSpellThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
    YouCastNoncreatureSpellThisTurn,
    YouCastSpellCountAtLeast {
        count: u32,
    },
    YouGainedLifeThisTurn,
    YouCreatedTokenThisTurn,
    YouDiscardedCardThisTurn,
    YouSacrificedArtifactThisTurn,
    /// CR 700.4: A creature moved from battlefield to graveyard this turn.
    CreatureDiedThisTurn,
    YouHadCreatureEnterThisTurn,
    YouHadAngelOrBerserkerEnterThisTurn,
    YouHadArtifactEnterThisTurn,
    CardsLeftYourGraveyardThisTurnAtLeast {
        count: u32,
    },
    /// CR 602.5b: Count of non-eliminated players matching `filter` is at least `minimum`.
    /// e.g. "an opponent lost life this turn" → `filter: OpponentLostLife, minimum: 1`
    PlayerCountAtLeast {
        filter: PlayerFilter,
        minimum: usize,
    },
    /// CR 702.131a: True when the activating player has the city's blessing.
    HasCityBlessing,
    // -- Combinators --
    /// CR 601.3 / CR 602.5: All inner conditions must be true. Used for compound
    /// casting/activation restrictions like "Cast this spell only if you control a
    /// Forest and you've cast a creature spell this turn". Mirrors `AbilityCondition::And`
    /// and `TriggerCondition::And` so restriction-side conjunction composes uniformly.
    And {
        conditions: Vec<ParsedCondition>,
    },
    /// CR 601.3 / CR 602.5: Any inner condition must be true. Used for disjunctive
    /// casting/activation restrictions ("only if X or Y"). Mirrors `TriggerCondition::Or`
    /// for restriction-level conditions.
    Or {
        conditions: Vec<ParsedCondition>,
    },
    /// CR 601.3 / CR 602.5: True when the inner predicate is false. Used for
    /// "Cast this spell only if you don't ..." / "Activate only if it isn't ..." patterns.
    /// Mirrors `AbilityCondition::Not` and `TriggerCondition::Not` so restriction-side
    /// negation composes uniformly with `And`/`Or`.
    Not {
        condition: Box<ParsedCondition>,
    },
}

// ---------------------------------------------------------------------------
// PaymentCost — cost paid during effect resolution (not activation)
// ---------------------------------------------------------------------------

/// CR 118.1: A cost paid as part of an effect's resolution.
/// Distinct from AbilityCost (which gates activation before the colon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PaymentCost {
    Mana {
        cost: ManaCost,
    },
    /// CR 118.8 + CR 119.4: Pay life during effect resolution. Paying life IS losing
    /// life — the replacement pipeline and `CantLoseLife` locks apply. `amount` is a
    /// `QuantityExpr` so dynamic references (`"life equal to its power"`, `"X life"`,
    /// `"life equal to the number of ..."`) compose through the same cost resolver
    /// as a literal `"pay 2 life"`.
    Life {
        amount: QuantityExpr,
    },
    /// CR 118.3 + CR 702.179f: Pay speed during effect resolution.
    Speed {
        amount: QuantityExpr,
    },
    /// CR 107.14: Pay energy counters during effect resolution.
    /// Distinct from `AbilityCost::PayEnergy` (activation cost before the colon).
    Energy {
        amount: QuantityExpr,
    },
    /// CR 118.1 + CR 118.12: Non-resource cost instructions paid while a spell or
    /// ability resolves. Reuses the engine's existing `AbilityCost` taxonomy so
    /// resolution-time costs such as "discard a card" do not grow a parallel
    /// payment hierarchy.
    AbilityCost {
        cost: AbilityCost,
    },
}

// ---------------------------------------------------------------------------
// AbilityCost -- expanded typed variants
// ---------------------------------------------------------------------------

/// CR 702.49: Ninjutsu-family keyword variants that share the "activate to swap
/// a creature in combat" pattern. This enum is the *activation-family dispatch*
/// layer — it is used only by `activate_ninjutsu` and its supporting helpers
/// (`ninjutsu_timing_ok`, `returnable_creatures_for_variant`, etc.) to pick
/// the correct activation behavior. Sneak (CR 702.190a) is NOT an activated
/// ability and therefore does not appear here; see `CastVariantPaid` for the
/// trigger-tag layer that does include it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NinjutsuVariant {
    /// CR 702.49a: Return unblocked attacker, declare blockers or later.
    Ninjutsu,
    /// CR 702.49d: Commander ninjutsu — activate from hand or command zone.
    CommanderNinjutsu,
    /// CR 702.49 variant: Return any tapped creature you control.
    WebSlinging,
}

/// CR 702.49 + CR 702.190a + CR 603.4: Which alternative-cost cast/activation
/// variant was paid to put this permanent onto the battlefield. This is the
/// *trigger-tag / ability-condition* layer — separate from `NinjutsuVariant`
/// (activation-family dispatch) because it legitimately includes Sneak, which
/// is a cast alt-cost rather than an activated ability.
///
/// Populated by `casting::handle_cast_spell_as_sneak` (Sneak) and
/// `keywords::activate_ninjutsu` (Ninjutsu / CommanderNinjutsu / WebSlinging)
/// into `GameObject.cast_variant_paid` as the source permanent enters the
/// battlefield. Read by `TriggerCondition::CastVariantPaid` and
/// `AbilityCondition::CastVariantPaid` / `CastVariantPaidInstead`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastVariantPaid {
    /// CR 702.49a / CR 702.49d: Ninjutsu (incl. commander ninjutsu) cost was paid.
    Ninjutsu,
    /// CR 702.49d: Commander ninjutsu cost was paid (distinct from Ninjutsu for
    /// parser fidelity; triggers referencing "ninjutsu cost" match either).
    CommanderNinjutsu,
    /// CR 702.190a: Sneak alternative cast cost was paid from graveyard.
    Sneak,
    /// CR 702.49 variant: Web-slinging cost was paid.
    WebSlinging,
    /// CR 702.74a: Evoke alternative cast cost was paid from hand. Read by the
    /// synthesized intervening-if ETB sacrifice trigger.
    Evoke,
    /// CR 702.62a: Cast as the suspend "play it without paying its mana cost"
    /// resolution after the last time counter was removed. Tagged at stack
    /// resolution; the runtime-installed haste static (creature spells only)
    /// keys off this marker so a Threaten-style control swap correctly
    /// terminates haste via `StaticCondition::SourceControllerEquals`.
    Suspend,
    /// CR 702.138a + CR 702.138b: The spell that became this permanent was cast
    /// from a graveyard via its escape alternative cost. Read by the "unless it
    /// escaped" intervening-if on Phlage, Titan of Fire's Fury and any future
    /// escape-gated ETB trigger.
    Escape,
    /// CR 702.143c: The spell or permanent was a foretold card before it was
    /// cast. Read by "if this spell was foretold" instead/condition clauses.
    Foretell,
}

impl From<NinjutsuVariant> for CastVariantPaid {
    /// CR 702.49: Lift an activation-family variant into the cast-variant-paid tag
    /// used by trigger conditions. Sneak is intentionally NOT in `NinjutsuVariant`
    /// (it is a cast alt-cost, not an activation), so this conversion is total.
    fn from(v: NinjutsuVariant) -> Self {
        match v {
            NinjutsuVariant::Ninjutsu => CastVariantPaid::Ninjutsu,
            NinjutsuVariant::CommanderNinjutsu => CastVariantPaid::CommanderNinjutsu,
            NinjutsuVariant::WebSlinging => CastVariantPaid::WebSlinging,
        }
    }
}

/// CR 702.49: Identifies which dedicated engine path handles a RuntimeHandled ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeHandler {
    /// Handled by GameAction::ActivateNinjutsu path.
    NinjutsuFamily,
}

/// Cost to activate an ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AbilityCost {
    Mana {
        cost: ManaCost,
    },
    Tap,
    Untap,
    Loyalty {
        amount: i32,
    },
    Sacrifice {
        target: TargetFilter,
        /// Number of permanents to sacrifice (default 1).
        /// Used for "sacrifice two creatures" or "sacrifice three lands" costs.
        #[serde(default = "default_one")]
        count: u32,
    },
    /// CR 119.4: Pay life as an activation or additional cost. `amount` is a
    /// `QuantityExpr` so dynamic references (e.g.
    /// `QuantityRef::ColorsInCommandersColorIdentity` for War Room's "pay life
    /// equal to the number of colors in your commanders' color identity")
    /// resolve at activation time rather than forcing a static integer.
    PayLife {
        amount: QuantityExpr,
    },
    Discard {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default)]
        filter: Option<TargetFilter>,
        #[serde(default)]
        random: bool,
        /// When true, the source card itself is discarded (Channel's "Discard this card").
        #[serde(default)]
        self_ref: bool,
    },
    Exile {
        count: u32,
        #[serde(default)]
        zone: Option<Zone>,
        #[serde(default)]
        filter: Option<TargetFilter>,
    },
    /// CR 701.59a / CR 702.163a: Exile cards from your graveyard with total mana value
    /// at least N as a collect evidence cost.
    CollectEvidence {
        amount: u32,
    },
    TapCreatures {
        count: u32,
        filter: TargetFilter,
    },
    RemoveCounter {
        count: u32,
        counter_type: String,
        #[serde(default)]
        target: Option<TargetFilter>,
    },
    PayEnergy {
        amount: u32,
    },
    /// CR 118.3 + CR 702.179f: Pay speed as an activation or additional cost.
    PaySpeed {
        amount: QuantityExpr,
    },
    ReturnToHand {
        count: u32,
        #[serde(default)]
        filter: Option<TargetFilter>,
    },
    Mill {
        count: u32,
    },
    Exert,
    /// Blight N — put N -1/-1 counters on a creature you control.
    /// Used as both activated ability costs and optional additional casting costs.
    Blight {
        count: u32,
    },
    Reveal {
        count: u32,
        /// Filter on what must be revealed (e.g., "a Dragon card from your hand").
        /// None means reveal any card (self-reveal).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
    Composite {
        costs: Vec<AbilityCost>,
    },
    /// Waterbend {N}: pay N generic mana, allowing tap-to-pay with creatures/artifacts.
    Waterbend {
        cost: ManaCost,
    },
    /// CR 702.49: Pay mana and return a creature (variant-dependent) to put this card
    /// onto the battlefield tapped and attacking.
    NinjutsuFamily {
        variant: NinjutsuVariant,
        mana_cost: ManaCost,
    },
    Unimplemented {
        description: String,
    },
}

/// CR 118: Cost taxonomy — a structural classifier over `AbilityCost` variants.
///
/// Provides a single-authority view of what an ability "does" to pay itself
/// without forcing callers to destructure individual cost variants. Policies,
/// AI heuristics, and other consumers should ask
/// `ability.cost_categories().contains(&CostCategory::SacrificesPermanent)`
/// rather than match on `AbilityCost::Sacrifice { .. }` directly. This
/// preserves the "single authority for ability costs" invariant from CLAUDE.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CostCategory {
    ManaOnly,
    TapsSelf,
    UntapsSelf,
    SacrificesPermanent,
    PaysLife,
    PaysLoyalty,
    Discards,
    ExilesCards,
    TapsOtherCreatures,
    RemovesCounters,
    PaysEnergy,
    PaysSpeed,
    ReturnsToHand,
    Mills,
    PutsCounters,
    Reveals,
    Exerts,
    KeywordCost,
}

impl AbilityCost {
    /// CR 118: Classify this cost into one or more `CostCategory` buckets.
    ///
    /// `Composite` recurses, flattening every sub-cost. Variants that pay
    /// nothing real (`Unimplemented`) return an empty vec.
    pub fn categories(&self) -> Vec<CostCategory> {
        match self {
            AbilityCost::Mana { .. } => vec![CostCategory::ManaOnly],
            AbilityCost::Tap => vec![CostCategory::TapsSelf],
            AbilityCost::Untap => vec![CostCategory::UntapsSelf],
            AbilityCost::Loyalty { .. } => vec![CostCategory::PaysLoyalty],
            AbilityCost::Sacrifice { .. } => vec![CostCategory::SacrificesPermanent],
            AbilityCost::PayLife { .. } => vec![CostCategory::PaysLife],
            AbilityCost::Discard { .. } => vec![CostCategory::Discards],
            AbilityCost::Exile { .. } => vec![CostCategory::ExilesCards],
            AbilityCost::CollectEvidence { .. } => vec![CostCategory::ExilesCards],
            AbilityCost::TapCreatures { .. } => vec![CostCategory::TapsOtherCreatures],
            AbilityCost::RemoveCounter { .. } => vec![CostCategory::RemovesCounters],
            AbilityCost::PayEnergy { .. } => vec![CostCategory::PaysEnergy],
            AbilityCost::PaySpeed { .. } => vec![CostCategory::PaysSpeed],
            AbilityCost::ReturnToHand { .. } => vec![CostCategory::ReturnsToHand],
            AbilityCost::Mill { .. } => vec![CostCategory::Mills],
            AbilityCost::Exert => vec![CostCategory::Exerts],
            AbilityCost::Blight { .. } => vec![CostCategory::PutsCounters],
            AbilityCost::Reveal { .. } => vec![CostCategory::Reveals],
            AbilityCost::Composite { costs } => {
                let mut out = Vec::with_capacity(costs.len());
                for cost in costs {
                    for cat in cost.categories() {
                        if !out.contains(&cat) {
                            out.push(cat);
                        }
                    }
                }
                out
            }
            AbilityCost::Waterbend { .. } => vec![CostCategory::KeywordCost],
            AbilityCost::NinjutsuFamily { .. } => vec![CostCategory::KeywordCost],
            AbilityCost::Unimplemented { .. } => Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// AdditionalCost — models the different "as an additional cost" patterns
// ---------------------------------------------------------------------------

/// An additional cost that a player must decide on during casting.
///
/// This is the building block for all "as an additional cost to cast this spell"
/// patterns, including kicker, blight, and other future cost mechanics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum AdditionalCost {
    /// "you may [cost]" — player decides whether to pay.
    /// If paid, `SpellContext::additional_cost_paid` is set to true.
    Optional(AbilityCost),
    /// CR 702.33a-c + CR 601.2b/f: Kicker costs announced during spell
    /// casting. `costs.len() == 1` is ordinary kicker, `costs.len() == 2`
    /// represents "Kicker [cost 1] and/or [cost 2]" (CR 702.33b), and
    /// `repeatable == true` represents multikicker (CR 702.33c), where the
    /// single listed cost may be paid any number of times.
    Kicker {
        costs: Vec<AbilityCost>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repeatable: bool,
    },
    /// "[cost A] or [cost B]" — player must pay exactly one.
    /// Choosing the first cost sets `additional_cost_paid = true`.
    Choice(AbilityCost, AbilityCost),
    /// Mandatory additional cost (e.g., "As an additional cost, waterbend {5}").
    Required(AbilityCost),
}

/// Structured spell-casting options parsed from Oracle text.
/// These describe alternate ways a spell may be cast; runtime enforcement can
/// be added independently of parsing/export support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpellCastingOption {
    pub kind: SpellCastingOptionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AbilityCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<ParsedCondition>,
}

impl SpellCastingOption {
    pub fn alternative_cost(cost: AbilityCost) -> Self {
        Self {
            kind: SpellCastingOptionKind::AlternativeCost,
            cost: Some(cost),
            condition: None,
        }
    }

    pub fn free_cast() -> Self {
        Self {
            kind: SpellCastingOptionKind::CastWithoutManaCost,
            cost: None,
            condition: None,
        }
    }

    pub fn as_though_had_flash() -> Self {
        Self {
            kind: SpellCastingOptionKind::AsThoughHadFlash,
            cost: None,
            condition: None,
        }
    }

    pub fn cost(mut self, cost: AbilityCost) -> Self {
        self.cost = Some(cost);
        self
    }

    pub fn condition(mut self, condition: ParsedCondition) -> Self {
        self.condition = Some(condition);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpellCastingOptionKind {
    AlternativeCost,
    CastWithoutManaCost,
    AsThoughHadFlash,
    /// CR 715.3a: Cast the Adventure half of an Adventure card.
    CastAdventure,
}

// ---------------------------------------------------------------------------
// Unless Cost -- dynamic or static mana costs for "unless pays" effects
// ---------------------------------------------------------------------------

/// CR 118.12: Cost that may be static or resolved dynamically at payment time.
/// Used by counter-unless-pays, tax triggers (Esper Sentinel, Rhystic Study),
/// and ward costs (CR 702.21a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UnlessCost {
    /// Fixed mana cost (e.g., "unless that player pays {1}")
    Fixed { cost: ManaCost },
    /// Generic mana equal to a dynamic quantity (e.g., "where X is this creature's power")
    DynamicGeneric { quantity: QuantityExpr },
    /// CR 702.21a: Pay life as ward cost (e.g., "Ward—Pay 2 life")
    PayLife { amount: i32 },
    /// CR 702.21a: Discard a card as ward cost (e.g., "Ward—Discard a card")
    DiscardCard,
    /// CR 702.21a: Sacrifice N permanents matching a filter as ward cost.
    Sacrifice { count: u32, filter: TargetFilter },
    /// CR 118.12: Return N objects matching a filter to their owner's hand
    /// as an unless cost. Source zone defaults to battlefield; Harvest Wurm
    /// uses `from_zone: Some(Graveyard)`.
    ReturnToHand {
        count: u32,
        filter: TargetFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_zone: Option<crate::types::zones::Zone>,
    },
}

/// CR 118.12: "Effect unless [player] pays {cost}"
/// Wraps any effect with an opponent payment choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlessPayModifier {
    pub cost: UnlessCost,
    /// Who must pay — resolved via TargetFilter at trigger resolution time.
    /// Typically TargetFilter::TriggeringPlayer for "that player".
    pub payer: TargetFilter,
}

// ---------------------------------------------------------------------------
// Effect enum -- typed variants, zero HashMap
// ---------------------------------------------------------------------------

/// CR 701.24g: Specific position within a library for placement effects.
/// Top and Bottom use move_to_library_position; NthFromTop inserts at index n-1.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LibraryPosition {
    Top,
    Bottom,
    /// "second from the top", "third from the top", "seventh from the top"
    NthFromTop {
        n: u32,
    },
}

/// CR 120.3: Override for which object is the source of damage.
/// By default, the source is the ability's source object (`ability.source_id`).
/// `Target` means the first resolved target is the damage source (e.g.,
/// "Target creature deals damage to itself" — the creature, not the spell).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageSource {
    /// The first resolved object target is the damage source.
    Target,
    /// CR 120.3 + CR 603.7c: The triggering event's source object is the
    /// damage source.
    TriggeringSource,
}

/// A single conjured card entry: card name + quantity.
/// Used by `Effect::Conjure` to support multi-card conjure patterns
/// (e.g., "conjure a card named X and a card named Y into your hand").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConjureCard {
    pub name: String,
    #[serde(default = "default_quantity_one")]
    pub count: QuantityExpr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopyManaValueLimit {
    AmountSpentToCastSource,
}

/// The typed effect enum. Each variant corresponds to an effect handler.
/// Zero HashMap<String, String> fields.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
#[serde(tag = "type")]
pub enum Effect {
    /// CR 702.179a: A player starts their engines, setting speed to 1 if they have no speed.
    StartYourEngines {
        player_scope: PlayerFilter,
    },
    /// CR 702.179c-d: Increase the selected players' speed by the given amount.
    IncreaseSpeed {
        player_scope: PlayerFilter,
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
    },
    DealDamage {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 120.3: Override damage source. None = ability source (default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        damage_source: Option<DamageSource>,
    },
    /// CR 121.1: Draw a card.
    /// CR 115.1 + CR 601.2c: When `target` is `TargetFilter::Player` (or any
    /// other non-context-ref filter), the drawing player is chosen during spell
    /// announcement. The default `TargetFilter::Controller` preserves the
    /// historical "controller draws" semantics for `"draw a card"` /
    /// `"you draw a card"` patterns where no `target` field appears in the
    /// serialized AST.
    /// CR 121.1 + CR 608.2d: "Draw up to N cards" is encoded as
    /// `count: QuantityExpr::UpTo { max: <former count> }`. The drawing
    /// player chooses any 0..=resolve(max) at resolution time via the
    /// `engine_resolution_choices` flow.
    Draw {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
    },
    Pump {
        #[serde(default = "default_pt_value_zero")]
        power: PtValue,
        #[serde(default = "default_pt_value_zero")]
        toughness: PtValue,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Destroy {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 701.19a: When true, the destroyed permanent cannot be regenerated.
        #[serde(default)]
        cant_regenerate: bool,
    },
    /// CR 701.19a: Create a regeneration shield on the target permanent.
    Regenerate {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Counter {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// Static applied to counter's source, affecting the countered ability's source permanent.
        /// The `affected` filter is bound at resolution time to `SpecificObject(source_permanent_id)`.
        /// Used by cards like Tishana's Tidebinder ("loses all abilities for as long as ~").
        #[serde(default)]
        source_static: Option<StaticDefinition>,
        /// CR 118.12: "Counter target spell unless its controller pays {X}".
        /// When present, the spell's controller may pay the cost to prevent the counter.
        #[serde(default)]
        unless_payment: Option<UnlessCost>,
    },
    /// CR 701.6 + CR 405.1: Mass counter — counter every spell or ability on
    /// the stack matching `target`. Mirrors `Effect::DestroyAll` /
    /// `Effect::BounceAll` for the "counter all/each [filter] spells" /
    /// "counter all [filter] abilities" Oracle text class (Glen Elendra's
    /// Answer, Swift Silence, Kadena's Silencer, etc.). The class filter must
    /// pin the stack property (`InZone { Stack }` or `StackAbility`) — the
    /// resolver iterates the stack zone and counters each match. Since the
    /// effect is non-targeting (CR 115.1: "all" does not target), it never
    /// asks for player input and Ward / hexproof / shroud do not apply.
    CounterAll {
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    Token {
        name: String,
        #[serde(default = "default_pt_value_zero")]
        power: PtValue,
        #[serde(default = "default_pt_value_zero")]
        toughness: PtValue,
        #[serde(default)]
        types: Vec<String>,
        #[serde(default)]
        colors: Vec<ManaColor>,
        #[serde(default)]
        keywords: Vec<Keyword>,
        #[serde(default)]
        tapped: bool,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// The player who creates/owns the token(s).
        #[serde(default = "default_target_filter_controller")]
        owner: TargetFilter,
        /// CR 303.7: When a Role token or Aura token is created "attached to" a
        /// target, this field captures that attachment target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attach_to: Option<TargetFilter>,
        /// CR 508.4: Token enters the battlefield attacking (not declared as attacker).
        #[serde(default)]
        enters_attacking: bool,
        /// CR 205.4a: Supertypes for the token (Legendary, Snow, etc.).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        supertypes: Vec<super::card_type::Supertype>,
        /// Static abilities granted to the token (e.g., "This token can't block.").
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        static_abilities: Vec<StaticDefinition>,
        /// Counters placed on the token as it enters the battlefield.
        /// Each entry is (counter_type, count). Used by "The token enters with
        /// X +1/+1 counters on it" patterns.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(String, QuantityExpr)>,
    },
    GainLife {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        /// Who gains the life.
        #[serde(default)]
        player: GainLifePlayer,
    },
    LoseLife {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        /// CR 119.3 + CR 115.1d: Optional player target for directed life loss.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<TargetFilter>,
    },
    Tap {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Untap {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.26a: Tap all permanents matching the filter.
    TapAll {
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    /// CR 701.26b: Untap all permanents matching the filter.
    UntapAll {
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    AddCounter {
        counter_type: String,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    RemoveCounter {
        counter_type: String,
        #[serde(default = "default_one_i32")]
        count: i32,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Sacrifice {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 701.21a: Number of permanents to sacrifice. Defaults to 1 so every
        /// existing emission site — AST lowering, dungeon rooms, token graveyard
        /// upkeep, emblem cost handlers, Forge importer — keeps its original
        /// "sacrifice one" semantics without code changes. Dynamic quantities
        /// ("sacrifice half the permanents they control") resolve per-iteration
        /// via `resolve_quantity_with_targets`, which honors `player_scope`
        /// controller rebinding.
        /// CR 701.21a + CR 608.2d: "Sacrifice up to N permanents" is encoded
        /// as `count: QuantityExpr::UpTo { max: <former count> }`. Distinct
        /// from `optional: true` on the ability ("you may sacrifice").
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    DiscardCard {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Mill {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 701.17a: Destination zone for milled cards. Defaults to Graveyard.
        /// Set to Exile for "exile the top N cards" patterns that reuse Mill's
        /// top-of-library mechanics with a different destination.
        #[serde(default = "default_zone_graveyard")]
        destination: Zone,
    },
    /// CR 701.22a: Scry N — look at the top N cards, then put any number on
    /// the bottom in any order and the rest on top in any order.
    /// CR 115.1 + CR 601.2c: When `target` is `TargetFilter::Player` (or any
    /// other non-context-ref filter), the scrying player is chosen during
    /// spell announcement. The default `TargetFilter::Controller` preserves
    /// "you scry" / "scry N" semantics where no player is targeted.
    Scry {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
    },
    PumpAll {
        #[serde(default = "default_pt_value_zero")]
        power: PtValue,
        #[serde(default = "default_pt_value_zero")]
        toughness: PtValue,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    /// CR 120.3: Deal uniform damage to every matching object and optionally every
    /// matching player, as one simultaneous damage event from a single source.
    /// The object set (`target`) and player set (`player_filter`) resolve within
    /// the same effect resolution, so replacement effects (CR 614) and prevention
    /// shields (CR 615) that watch "the next damage dealt by [this source]"
    /// observe a single coherent event across both sets.
    DamageAll {
        amount: QuantityExpr,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
        /// CR 120.3: When `Some`, each non-eliminated player matching this filter
        /// is also dealt `amount` damage from the same source in the same event.
        /// Models "deals N damage to each opponent and each creature they control"
        /// as a single effect instead of two chained resolutions.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        player_filter: Option<PlayerFilter>,
    },
    /// CR 120.3: Deal damage to each player matching a filter, with per-player quantity.
    /// Unlike `DamageAll` (which iterates battlefield objects with a fixed amount),
    /// this iterates players and resolves `amount` per-player via `resolve_quantity_scoped()`.
    DamageEachPlayer {
        amount: QuantityExpr,
        player_filter: PlayerFilter,
    },
    DestroyAll {
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
        /// CR 701.19a: When true, destroyed permanents cannot be regenerated.
        #[serde(default)]
        cant_regenerate: bool,
    },
    ChangeZone {
        #[serde(default)]
        origin: Option<Zone>,
        destination: Zone,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 400.7: When true, route the object to its owner's library
        /// (not controller's). Used for "shuffle into its owner's library".
        #[serde(default)]
        owner_library: bool,
        /// CR 712.2: When true, the object enters the battlefield showing its back face.
        #[serde(default)]
        enter_transformed: bool,
        /// CR 110.2: When true, the object enters under the ability controller's control
        /// (not the object's owner). Used for "onto the battlefield under your control."
        #[serde(default)]
        under_your_control: bool,
        /// CR 614.1: When true, the object enters the battlefield tapped.
        /// Building block for "put onto the battlefield tapped" effects.
        #[serde(default)]
        enter_tapped: bool,
        /// CR 508.4: When true, the object enters the battlefield tapped and attacking.
        /// Not "declared as an attacker" — attack triggers do not fire.
        #[serde(default)]
        enters_attacking: bool,
        /// CR 608.2d: When true, the player may choose not to move
        /// ("put up to one land ...").
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        /// CR 122.1 + CR 614.1c: Counters placed on the moved object as it
        /// enters its destination zone. Each entry is `(counter_type, count)`.
        /// Mirrors `Effect::Token.enter_with_counters` and is used by patterns
        /// like "Put target creature card ... onto the battlefield ... with two
        /// additional +1/+1 counters on it" (Darkness Crystal) and "exile it
        /// with three egg counters on it" (Darigaaz Reincarnated).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(String, QuantityExpr)>,
    },
    ChangeZoneAll {
        #[serde(default)]
        origin: Option<Zone>,
        destination: Zone,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    /// CR 701.20e + CR 608.2c: Look at top N cards (shown only to the looking player),
    /// select some to keep per the effect's instructions, rest go elsewhere.
    Dig {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// Kept-card destination override (None = Hand).
        #[serde(default)]
        destination: Option<Zone>,
        /// How many cards to keep (None = 1).
        #[serde(default)]
        keep_count: Option<u32>,
        /// True = select 0..=keep_count ("up to N"), false = exactly keep_count.
        #[serde(default)]
        up_to: bool,
        /// Filter for keepable cards (Any = no filter).
        #[serde(default = "default_target_filter_any")]
        filter: TargetFilter,
        /// Where unchosen cards go (None = Graveyard, Some(Library) = bottom).
        #[serde(default)]
        rest_destination: Option<Zone>,
        /// CR 701.20a vs CR 701.16a: True = cards are revealed (public), false = looked at (private).
        #[serde(default)]
        reveal: bool,
    },
    GainControl {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    ControlNextTurn {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default)]
        grant_extra_turn_after: bool,
    },
    Attach {
        #[serde(
            default = "default_target_filter_self_ref",
            skip_serializing_if = "target_filter_is_self_ref"
        )]
        attachment: TargetFilter,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.25a: Surveil N — look at the top N cards, then put any number
    /// into the graveyard and the rest on top in any order.
    /// CR 115.1 + CR 601.2c: When `target` is `TargetFilter::Player` (or any
    /// other non-context-ref filter), the surveiling player is chosen during
    /// spell announcement. The default `TargetFilter::Controller` preserves
    /// "you surveil" / "surveil N" semantics where no player is targeted.
    Surveil {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
    },
    Fight {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 701.14a: The creature that fights. Defaults to SelfRef (the ability source).
        /// Set to AttachedTo for "enchanted/equipped creature fights" patterns.
        #[serde(default = "default_target_filter_self_ref")]
        subject: TargetFilter,
    },
    Bounce {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default)]
        destination: Option<Zone>,
    },
    /// CR 400.7 + CR 611.2c: Mass-bounce — return every permanent matching
    /// `target` to its owner's hand (default) or `destination` if set. Mirrors
    /// `Effect::DestroyAll` / `Effect::PumpAll` / `Effect::TapAll` for the
    /// "return all/each [filter]" Oracle text class (Evacuation, Devastation
    /// Tide, Upheaval, Sunderflock, Wash Out, Whelming Wave, Crush of
    /// Tentacles, Coastal Breach, etc.). The default destination is the
    /// owner's hand; `Some(Zone::Library)` covers top-of-library variants.
    BounceAll {
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<Zone>,
    },
    Explore,
    /// CR 701.44d: Simultaneous multi-permanent explore instruction.
    /// The resolver processes matching permanents one explore at a time in
    /// APNAP/controller-chosen order, reusing the single-permanent Explore resolver.
    ExploreAll {
        #[serde(default = "default_target_filter_any")]
        filter: TargetFilter,
    },
    /// CR 702.136: Investigate — create a Clue artifact token.
    Investigate,
    /// CR 702.104a: Tribute — "As this creature enters, an opponent of your choice may
    /// put N +1/+1 counters on it." The chosen opponent (persisted on the source as
    /// `ChosenAttribute::Player` by a preceding `Effect::Choose { Opponent, persist }`)
    /// is prompted pay-or-decline; the outcome is recorded on the source as
    /// `ChosenAttribute::TributeOutcome` so the companion "if tribute wasn't paid"
    /// trigger condition (CR 702.104b) can read it.
    Tribute {
        /// Number of +1/+1 counters placed if tribute is paid.
        count: u32,
    },
    /// CR 701.56a: Time travel — for each permanent you control with a time counter
    /// and each suspended card you own, you may add or remove a time counter.
    TimeTravel,
    /// CR 725.1: Become the monarch. Sets GameState::monarch to the controller.
    BecomeMonarch,
    Proliferate,
    /// CR 701.36a: Choose a creature token you control, then create a copy of it.
    Populate,
    /// CR 701.30: Clash with an opponent — reveal top cards, compare mana values.
    Clash,
    /// CR 701.38: Vote — each player chooses one of the listed options, starting
    /// with a specified player and proceeding in turn order. After all votes are
    /// collected, the resolver runs `per_choice_effect[i]` once for each vote
    /// cast for `choices[i]`. CR 701.38d: A player with multiple votes (granted
    /// by static "you may vote an additional time") makes those choices at the
    /// same time they would otherwise have voted.
    ///
    /// The starting player defaults to the ability's controller (the canonical
    /// "Council's dilemma — starting with you" pattern). `per_choice_effect[i]`
    /// resolves once per vote tallied for `choices[i]` (Tivit's "for each
    /// evidence vote, investigate" / "for each bribery vote, create a Treasure
    /// token"). Each per-vote sub-resolution inherits the source ability's
    /// controller and source object, identical to how `ForEach`-style replicate
    /// effects fan out.
    Vote {
        /// Lowercase choice identifiers ("evidence", "bribery", a creature
        /// type, etc.). Display capitalization is restored from the original
        /// Oracle text by the parser before serialization.
        choices: Vec<String>,
        /// One sub-effect per choice. `per_choice_effect[i]` resolves once for
        /// every vote cast for `choices[i]`. Length must equal `choices.len()`
        /// — the parser is the single authority and the resolver asserts this
        /// invariant.
        per_choice_effect: Vec<Box<AbilityDefinition>>,
        /// CR 101.4 + CR 701.38a: The first voter. `ControllerRef::You` covers
        /// "starting with you"; other refs cover "starting with the player to
        /// your left" / "the affected player" if those phrasings ever land.
        #[serde(default = "default_controller_ref_you")]
        starting_with: ControllerRef,
    },
    /// CR 613.4d: Switch a creature's power and toughness. Applied in layer 7d.
    SwitchPT {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    CopySpell {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 707.2 / CR 707.5: Create a token that's a copy of a permanent.
    /// Copies copiable characteristics (name, mana cost, color, types, P/T, abilities, keywords)
    /// from the target to a newly created token on the battlefield.
    CopyTokenOf {
        /// Filter for the object to copy. SelfRef = "copy of ~", Any/Typed = "copy of target..."
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 508.4: Token enters the battlefield attacking (not declared as attacker).
        #[serde(default)]
        enters_attacking: bool,
        /// Token enters the battlefield tapped.
        #[serde(default)]
        tapped: bool,
        /// CR 707.10: Number of copy-tokens to create. Defaults to one. Used by
        /// "create [N] of those tokens" continuations (Rite of Replication kicker,
        /// Krothuss, Adrix and Nev doubling, etc.) and by populate-style multi-copy.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// CR 707.2 + CR 702: "except it has [keyword(s)]" — extra keywords granted
        /// to each created copy token in addition to the copied characteristics.
        /// Twinflame ("…except it has haste") is the canonical example.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        extra_keywords: Vec<crate::types::keywords::Keyword>,
        /// CR 707.9 + CR 707.2: Non-keyword "except" exceptions applied to the
        /// synthesized token (e.g., Miirym, Sentinel Wyrm: "except the token
        /// isn't legendary"). Mirrors `BecomeCopy.additional_modifications` so
        /// the same building block (`become_copy_except.rs::parse_except_body`)
        /// produces the modifications for both forms. The token-copy resolver
        /// stamps each modification onto the synthesized token directly (see
        /// `game/effects/token_copy.rs`), since copiable values for tokens are
        /// baked in at creation rather than evaluated through the layer system.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_modifications: Vec<ContinuousModification>,
    },
    /// CR 707.2 / CR 613.1a: Become a copy of target permanent.
    /// Sets copiable characteristics at Layer 1.
    BecomeCopy {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<Duration>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_value_limit: Option<CopyManaValueLimit>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_modifications: Vec<ContinuousModification>,
    },
    ChooseCard {
        #[serde(default)]
        choices: Vec<String>,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    PutCounter {
        counter_type: String,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 122.1: Place counters on all objects matching a filter (no targeting).
    PutCounterAll {
        counter_type: String,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    MultiplyCounter {
        counter_type: String,
        #[serde(default = "default_two_i32")]
        multiplier: i32,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.10a: Double power/toughness of target creature.
    DoublePT {
        mode: DoublePTMode,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.10a: Double power/toughness of all matching creatures.
    DoublePTAll {
        mode: DoublePTMode,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 122.5 + CR 122.8: Put counters from source onto target. CR 122.5 is
    /// the general "move a counter" rule; CR 122.8 covers the specific case of
    /// triggered abilities that copy counters off a creature that left the
    /// battlefield (e.g. dies-trigger "put its counters on…").
    MoveCounters {
        /// Where counters are read from (SelfRef = ability source object).
        #[serde(default = "default_target_filter_self_ref")]
        source: TargetFilter,
        /// When Some, only move this counter type. When None, move all counters.
        #[serde(default)]
        counter_type: Option<String>,
        /// Where counters go.
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Animate {
        #[serde(default)]
        power: Option<i32>,
        #[serde(default)]
        toughness: Option<i32>,
        #[serde(default)]
        types: Vec<String>,
        /// CR 205.1a: Core types to remove from the permanent (e.g., Creature for Glimmer cycle).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remove_types: Vec<String>,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// Keywords to grant to the animated permanent (e.g., Haste for Earthbending).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        keywords: Vec<Keyword>,
    },
    /// Records that a player bent an element this turn and emits the corresponding event.
    RegisterBending {
        kind: BendingType,
    },
    /// Generic continuous effect application at resolution.
    GenericEffect {
        #[serde(default)]
        static_abilities: Vec<StaticDefinition>,
        #[serde(default)]
        duration: Option<Duration>,
        #[serde(default)]
        target: Option<TargetFilter>,
    },
    Cleanup {
        #[serde(default)]
        clear_remembered: bool,
        #[serde(default)]
        clear_chosen_player: bool,
        #[serde(default)]
        clear_chosen_color: bool,
        #[serde(default)]
        clear_chosen_type: bool,
        #[serde(default)]
        clear_chosen_card: bool,
        #[serde(default)]
        clear_imprinted: bool,
        #[serde(default)]
        clear_triggers: bool,
        #[serde(default)]
        clear_coin_flips: bool,
    },
    Mana {
        #[serde(default = "default_mana_production")]
        produced: ManaProduction,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        restrictions: Vec<ManaSpendRestriction>,
        /// CR 106.6: Properties granted to the spell this mana is spent on.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        grants: Vec<crate::types::mana::ManaSpellGrant>,
        /// When set, produced mana persists beyond normal phase-transition drains
        /// until the specified expiry condition is met (e.g., EndOfCombat for firebending).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expiry: Option<crate::types::mana::ManaExpiry>,
        /// CR 115.1 + CR 115.7: Spell-level player target for mana abilities whose
        /// produced amount references a player target (e.g., Jeska's Will mode 1
        /// "Add {R} for each card in target opponent's hand"). When set, the
        /// player target is surfaced as a target slot at cast time and
        /// `TargetZoneCardCount` and `LifeTotal { player: Target }` quantities
        /// resolve against it.
        /// `None` for the common case of mana abilities with no player target
        /// (Cabal Coffers, Reflecting Pool, fixed mana, etc.).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<TargetFilter>,
    },
    Discard {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 701.9a: When true, the discard is random (e.g., "discard a card at random").
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        random: bool,
        /// CR 701.9b + CR 608.2d: "Discard up to N cards" is encoded as
        /// `count: QuantityExpr::UpTo { max: <former count> }`.
        /// CR 608.2c: "discard N cards unless you discard a [type] card" — when set,
        /// the player may discard 1 card matching this filter instead of `count` cards.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unless_filter: Option<TargetFilter>,
        /// CR 701.9a + CR 608.2c: Restriction on which cards can satisfy the discard
        /// (e.g., Dokuchi Silencer's "discard a creature card"). Mirrors the
        /// `filter` slot on `AbilityCost::Discard` — when set, only cards matching
        /// this filter are legal to discard. `None` means any card in the
        /// discarding player's hand is legal (the default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
    Shuffle {
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
    },
    Transform {
        #[serde(default = "default_target_filter_self_ref")]
        target: TargetFilter,
    },
    /// Search a player's library for card(s) matching a filter.
    /// The destination is handled by the sub_ability chain (ChangeZone + Shuffle).
    SearchLibrary {
        /// What cards can be found.
        filter: TargetFilter,
        /// How many cards to find (usually 1). `QuantityExpr` so the count can be
        /// X (CR 107.3a) or a dynamic expression resolved at effect time.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// Whether to reveal the found card(s) to all players.
        #[serde(default)]
        reveal: bool,
        /// CR 701.23a: When set, search this player's library instead of the controller's.
        /// Used by Bribery, Acquire, Praetor's Grasp, etc.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_player: Option<TargetFilter>,
        // CR 107.1c + CR 701.23d: "search for up to N" / "search for any
        // number of" is encoded as `count: QuantityExpr::UpTo { max:
        // <former count> }`. Plain "search for N" leaves count as a
        // non-UpTo expression and the searcher must find exactly N (or as
        // many as possible if fewer exist).
        /// CR 608.2c: Printed-text restriction on the chosen set (e.g., "with
        /// different names"). Defaults to `None` so the existing card-data.json
        /// deserializes without churn; the parser populates it for tutors that
        /// carry the restriction.
        #[serde(
            default,
            skip_serializing_if = "is_default_search_selection_constraint"
        )]
        selection_constraint: SearchSelectionConstraint,
    },
    RevealHand {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default = "default_target_filter_any")]
        card_filter: TargetFilter,
        /// None = reveal entire hand. Some = reveal this many cards. CR 701.20a.
        #[serde(default)]
        count: Option<QuantityExpr>,
        /// CR 608.2d: "You may choose a [card] from it" makes the post-reveal
        /// card selection optional while the hand reveal itself remains mandatory.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        choice_optional: bool,
    },
    /// CR 701.20a: "You may reveal a [FILTER] card from your hand" — optional self-reveal
    /// from the controller's own hand. Distinct from `RevealHand` (target player, used for
    /// opponent-facing effects like Thoughtseize). If the controller's hand contains no
    /// card matching `filter`, or if the controller declines the prompt, `on_decline` runs.
    /// Used by reveal-lands (Port Town, Gilt-Leaf Palace, and the 10-Temple cycle) where
    /// the "if you don't" branch taps the source. Composable: `on_decline` is any
    /// `AbilityDefinition`, so symmetric "if you do, [effect]" variants reuse the same
    /// primitive simply by swapping accept and decline.
    RevealFromHand {
        #[serde(default = "default_target_filter_any")]
        filter: TargetFilter,
        /// The ability run when the controller cannot or chooses not to reveal a
        /// matching card. `None` = decline is a no-op.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        on_decline: Option<Box<AbilityDefinition>>,
    },
    /// CR 701.20: Reveal a specific object (resolved from `target`) to all players.
    /// Distinct from `RevealHand` (zone-wide) and `RevealTop` (library depth-N).
    /// Per CR 701.20b, revealing does not move the card.
    Reveal {
        #[serde(default = "default_target_filter_self_ref")]
        target: TargetFilter,
    },
    /// CR 701.20a: Reveal the top N card(s) of a player's library.
    RevealTop {
        /// The player whose library to reveal from.
        #[serde(default = "default_target_filter_any")]
        player: TargetFilter,
        /// Number of cards to reveal.
        #[serde(default = "default_one")]
        count: u32,
    },
    /// Exile the top N card(s) of a player's library.
    ExileTop {
        /// The player whose library to exile from.
        #[serde(default = "default_target_filter_any")]
        player: TargetFilter,
        /// Number of cards to exile.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// No-op effect that only establishes targeting for sub-abilities in the chain.
    /// Produced by Oracle text like "Choose target creature" where the sentence exists
    /// solely to designate a target referenced by subsequent sentences via "that creature".
    TargetOnly {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// Resolution-time named choice: "choose a creature type", "choose a color", etc.
    /// Sets WaitingFor::NamedChoice and stores the result in GameState::last_named_choice.
    Choose {
        choice_type: ChoiceType,
        /// When true, the chosen value is stored on the source object's chosen_attributes.
        /// Used for ETB choices that other abilities reference ("the chosen type/color").
        #[serde(default)]
        persist: bool,
    },
    /// CR 609.7a + CR 120.7: Choose a specific source of damage matching a
    /// source-object filter. This is object/source selection, not a named
    /// string choice, because the chosen source is an ObjectId and prevention
    /// shields need to recheck the source's properties when damage would be dealt.
    ChooseDamageSource {
        #[serde(default = "default_target_filter_any")]
        source_filter: TargetFilter,
    },
    /// CR 701.60a: Suspect target creature — it gains menace and "can't block."
    Suspect {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.50a: Target creature connives (draw a card, then discard a card;
    /// if a nonland card is discarded, put a +1/+1 counter on it).
    /// CR 701.50e: "Connive N" draws N, discards N, counters per nonland.
    Connive {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default = "default_one")]
        count: u32,
    },
    /// CR 702.26a: Target permanent phases out (treated as though it doesn't exist
    /// until its controller's next untap step).
    PhaseOut {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 702.26c: Target phased-out permanent phases in. Rare — used by cards
    /// that explicitly phase a previously-phased-out object back in (e.g.,
    /// Teferi's Veil / Wake of Destruction-style effects).
    PhaseIn {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 509.1g: Target creature must block this turn if able.
    ForceBlock {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 719.2: Solve the source Case — it becomes solved.
    SolveCase,
    /// CR 702.xxx: Prepare (Strixhaven) — mark the target creature as prepared.
    /// The target must be a creature with a prepare face (CardLayout::Prepare);
    /// on targets without a prepare face (e.g. Biblioplex Tomekeeper's Oracle
    /// "Target creature becomes prepared. (Only creatures with prepare spells
    /// can become prepared.)") the resolver is a no-op. Idempotent: if already
    /// prepared, no event fires. Assign when WotC publishes SOS CR update.
    BecomePrepared {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 702.xxx: Prepare (Strixhaven) — clear the prepared state on the
    /// target creature. Idempotent: if not prepared, no event fires. Assign
    /// when WotC publishes SOS CR update.
    BecomeUnprepared {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 716.2a: Set the class level on the source Class enchantment.
    SetClassLevel {
        level: u8,
    },
    /// CR 603.7: Creates a delayed triggered ability during resolution.
    /// The delayed trigger fires once at the specified condition, then is removed.
    CreateDelayedTrigger {
        /// When the delayed trigger fires.
        condition: DelayedTriggerCondition,
        /// The effect to execute when it fires.
        effect: Box<AbilityDefinition>,
        /// If true, resolve the effect against the tracked object set from the parent.
        #[serde(default)]
        uses_tracked_set: bool,
    },
    /// CR 614.1a + CR 514.2: Register a replacement effect on the parent
    /// ability's target object or player at resolution time. Used by riders like
    /// "If that creature would die this turn, exile it instead." attached to
    /// damage-dealing spells/abilities. The replacement is appended to the
    /// target object's `replacement_definitions`; `valid_card: SelfRef`
    /// inside the carried definition naturally binds to *that* object since
    /// SelfRef on a replacement resolves against the carrying object.
    /// Player-bound damage replacements are stored in GameState's pending
    /// damage replacements after context references are resolved.
    /// Cleanup at end-of-turn relies on `expires_at_eot: true` on the
    /// carried definition (CR 514.2).
    AddTargetReplacement {
        replacement: Box<ReplacementDefinition>,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 614.16: Apply a game-level restriction (e.g., disable damage prevention).
    AddRestriction {
        restriction: GameRestriction,
    },
    /// CR 601.2f: "The next spell you cast this turn costs {N} less to cast."
    /// Creates a one-shot pending cost reduction consumed when the player casts their next spell.
    ReduceNextSpellCost {
        amount: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_filter: Option<TargetFilter>,
    },
    /// CR 601.2f: "The next [type] spell you cast this turn [has keyword/can't be countered/etc.]."
    /// Creates a one-shot modifier applied when the player casts their next qualifying spell.
    GrantNextSpellAbility {
        modifier: crate::types::game_state::NextSpellModifier,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_filter: Option<TargetFilter>,
    },
    /// CR 614.1c: Register pending ETB counters for the triggering spell.
    /// Reads `current_trigger_event` (SpellCast) to identify the object, then adds
    /// counters to `pending_etb_counters` so they are applied when the object enters
    /// the battlefield. Used by "that creature enters with an additional +1/+1 counter".
    AddPendingETBCounters {
        counter_type: String,
        count: QuantityExpr,
    },
    /// CR 114.1 + CR 114.4: Create an emblem with the specified abilities in
    /// the command zone. Emblems persist for the rest of the game and cannot
    /// be removed. Per CR 114.4 their abilities (statics AND triggers)
    /// function in the command zone.
    CreateEmblem {
        #[serde(default)]
        statics: Vec<StaticDefinition>,
        /// CR 113.1c + CR 114.4: Triggered abilities hosted on the emblem.
        /// Each has `trigger_zones = [Zone::Command]` so the scan gate in
        /// `collect_matching_triggers` admits them.
        #[serde(default)]
        triggers: Vec<TriggerDefinition>,
    },
    /// CR 118.1: Pay a cost during effect resolution (mana or life).
    PayCost {
        cost: PaymentCost,
        /// CR 608.2c: Player who pays the resolution-time cost. Defaults to
        /// the ability controller; target-derived refs such as
        /// `ParentTargetController` cover "that spell's controller may pay".
        #[serde(default = "default_target_filter_controller")]
        payer: TargetFilter,
    },
    /// CR 601.2a + CR 118.9: Cast or play a card from a zone.
    /// Grants `ExileWithAltCost` casting permission on target cards (Discover pattern),
    /// or `ExileWithAltAbilityCost` when `alt_ability_cost` is `Some(_)`.
    CastFromZone {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default)]
        without_paying_mana_cost: bool,
        /// CR 601.2 vs CR 305.1: Cast (spells only) vs Play (spells + lands).
        #[serde(default)]
        mode: CardPlayMode,
        /// CR 712.14a + CR 310.11b: When true, the card is cast transformed — it
        /// resolves to its back face (used by Siege victory: "cast it transformed
        /// without paying its mana cost").
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cast_transformed: bool,
        /// CR 118.9 + CR 119.4: Optional non-mana alternative cost, paid in lieu
        /// of the spell's mana cost (e.g. Nashi's "pay life equal to its mana
        /// value rather than paying its mana cost"). When `Some(_)`, the
        /// resolver grants `CastingPermission::ExileWithAltAbilityCost` instead
        /// of `ExileWithAltCost`; the casting pipeline overrides the spell's
        /// mana cost to zero and routes this cost through the standard
        /// `pay_additional_cost` flow. Mutually-exclusive with
        /// `without_paying_mana_cost: true` — the spell either has no
        /// alternative cost (free) or this one (replacement).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        alt_ability_cost: Option<AbilityCost>,
    },
    /// CR 615: Prevent damage to a target.
    PreventDamage {
        amount: PreventionAmount,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default)]
        scope: PreventionScope,
        /// CR 615 + CR 614.1a: Optional filter restricting which damage *sources* are
        /// prevented. Resolved at effect resolution time against the source object's
        /// chosen attributes (e.g., `IsChosenColor` → reads `ChosenAttribute::Color`
        /// and builds a concrete `HasColor` filter on the shield).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        damage_source_filter: Option<TargetFilter>,
    },
    /// CR 104.3a: A player who meets this effect's condition loses the game.
    /// The affected player is determined by resolution context (controller's opponent
    /// if untargeted, or explicit target if targeted).
    LoseTheGame,
    /// CR 104.3a: The controller wins the game — all opponents lose.
    WinTheGame,
    /// CR 706: Roll a die with the given number of sides.
    /// If `results` is non-empty, execute the matching branch.
    RollDie {
        sides: u8,
        #[serde(default)]
        results: Vec<DieResultBranch>,
    },
    /// CR 705: Flip a coin. Optionally execute different effects on win/lose.
    FlipCoin {
        #[serde(default)]
        win_effect: Option<Box<AbilityDefinition>>,
        #[serde(default)]
        lose_effect: Option<Box<AbilityDefinition>>,
    },
    /// CR 705: Flip N coins. `win_effect` runs once per heads (win),
    /// `lose_effect` runs once per tails (loss). Generalization of `FlipCoin`
    /// for "flip N coins, for each heads …" patterns (Ral Zarek, Guest
    /// Lecturer). The one-flip degenerate case stays as `FlipCoin` — this
    /// variant is only emitted when `count > 1` or when the Oracle text
    /// explicitly binds a count.
    FlipCoins {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default)]
        win_effect: Option<Box<AbilityDefinition>>,
        #[serde(default)]
        lose_effect: Option<Box<AbilityDefinition>>,
    },
    /// CR 705: Flip coins until you lose a flip, then execute effect with win count.
    FlipCoinUntilLose {
        win_effect: Box<AbilityDefinition>,
    },
    /// CR 701.54a: The Ring tempts the controller. Increments ring level and prompts
    /// ring-bearer selection if the controller has creatures on the battlefield.
    RingTemptsYou,
    /// CR 701.49: Venture into the dungeon. Advances the player's venture marker
    /// or starts a new dungeon if none is active.
    VentureIntoDungeon,
    /// CR 701.49d: Venture into a specific dungeon (e.g., "venture into the Undercity").
    VentureInto {
        dungeon: crate::game::dungeon::DungeonId,
    },
    /// CR 725.2: Take the initiative. Grants initiative designation and triggers
    /// venture into the Undercity.
    TakeTheInitiative,
    /// Grant a casting permission to the target object (e.g., "cast from exile for {2}").
    /// Building block for Airbending, Foretell, Suspend, Hideaway, and similar mechanics.
    GrantCastingPermission {
        permission: CastingPermission,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 611.2a + CR 108.3: Which player the permission binds to at grant
        /// resolution. Defaults to `AbilityController` for every pre-existing
        /// parse site. `ObjectOwner` powers per-object iteration grants
        /// (Suspend Aggression). `ParentTargetController` binds to the parent
        /// effect's player target (Expedited Inheritance).
        #[serde(default, skip_serializing_if = "is_default_grantee")]
        grantee: PermissionGrantee,
    },
    /// Choose card(s) from a zone (typically exiled cards from a prior effect).
    /// Building block for impulse draw, cascade, hideaway, and similar exile-then-select patterns.
    /// The selection is from the tracked set of the parent effect's result.
    /// CR 700.2: The `chooser` field determines who makes the selection.
    ChooseFromZone {
        /// How many cards to choose.
        #[serde(default = "default_one")]
        count: u32,
        /// Which zone the cards are in (usually Exile).
        zone: Zone,
        /// Who makes the choice: controller (default) or opponent.
        #[serde(default)]
        chooser: Chooser,
        /// CR 609.3: When true, the chooser may select any number from 0..=count.
        #[serde(default)]
        up_to: bool,
        /// Additional validation rules for the chosen subset.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<ChooseFromZoneConstraint>,
    },
    /// CR 101.4 + CR 701.21a: Each player chooses one permanent per type category
    /// from among the permanents they control, then sacrifices the rest.
    /// Building block for Cataclysm, Tragic Arrogance, Cataclysmic Gearhulk.
    ChooseAndSacrificeRest {
        /// Which card type categories to choose from (e.g., [Artifact, Creature, Enchantment, Land]).
        categories: Vec<CoreType>,
        /// CR 101.4: Whether each player chooses independently or one player decides for all.
        #[serde(default)]
        chooser_scope: CategoryChooserScope,
    },
    /// CR 702.110b: Exploit — sacrifice a creature you control (optional).
    /// The controller may sacrifice any creature they control, including the exploiter itself.
    Exploit {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 122.1: Gain energy counters.
    GainEnergy {
        amount: QuantityExpr,
    },
    /// CR 122.1: Give player counters (poison, experience, rad, ticket, etc.).
    /// Poison counters route to the dedicated field via `Player::add_player_counters` (CR 104.3d).
    GivePlayerCounter {
        counter_kind: PlayerCounterKind,
        count: QuantityExpr,
        target: TargetFilter,
    },
    /// CR 122.1: Remove every counter of every kind from a player.
    /// Covers "target opponent loses all counters" (Suncleanser) and
    /// "each opponent loses all counters" (Final Act mode 5). Clears the
    /// dedicated poison field and every entry in `player_counters` for the
    /// resolving target player. When `player_scope` iterates (e.g., "each
    /// opponent"), the controller is rebound per iteration and `target`
    /// resolves to the iterating player via `TargetFilter::Controller`.
    LoseAllPlayerCounters {
        target: TargetFilter,
    },
    /// CR 702.84a: Exile cards from the top of your library one at a time until you
    /// exile a card matching the filter. The hit card is passed to the sub_ability chain
    /// as an injected target.
    ExileFromTopUntil {
        filter: TargetFilter,
    },
    /// CR 701.20a: Reveal cards from the top of a player's library one at a time
    /// until that player reveals a card matching the filter. The matching card
    /// goes to `kept_destination`, the rest go to `rest_destination`.
    RevealUntil {
        /// CR 109.5: Whose library is revealed. `Controller` for "you reveal..."
        /// (the activator), `ParentTargetController` for "its controller reveals..."
        /// or "that creature's controller reveals..." (Polymorph, Proteus Staff,
        /// Transmogrify), and any player-resolving filter for cards like
        /// "target opponent reveals..." (Telemin Performance, Mind Funeral).
        #[serde(default = "default_target_filter_controller")]
        player: TargetFilter,
        filter: TargetFilter,
        /// Where the matching card goes (Hand or Battlefield).
        kept_destination: Zone,
        /// Where non-matching revealed cards go (Library bottom or Graveyard).
        rest_destination: Zone,
        /// CR 110.5b: When true, the matching card enters the battlefield tapped.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter_tapped: bool,
    },
    /// CR 701.57a: Discover N — exile from top until nonland with MV ≤ N,
    /// cast free or put to hand, rest to bottom in random order.
    Discover {
        mana_value_limit: u32,
    },
    /// CR 702.85a: Cascade — when you cast a spell with cascade, exile cards from
    /// the top of your library until you exile a nonland card whose mana value is
    /// less than the cascade spell's mana value. You may cast that card without
    /// paying its mana cost. Then put all exiled non-cast cards on the bottom of
    /// your library in a random order.
    ///
    /// The source spell's mana value is read from `ability.source_id` at resolve
    /// time (the cascade spell is on the stack when cascade resolves per CR 702.85a),
    /// so no threshold parameter is stored on the variant itself.
    Cascade,
    /// CR 702.94a: Miracle trigger resolution — offers the player the chance to
    /// cast the source card from hand for its miracle cost. Carries the cost so
    /// the resolution handler can populate `WaitingFor::MiracleCastOffer`.
    MiracleCast {
        cost: super::mana::ManaCost,
    },
    /// CR 702.35a: Madness trigger resolution — offers the player the chance to
    /// cast the source card from exile for its madness cost.
    MadnessCast {
        cost: super::mana::ManaCost,
    },
    /// CR 701.24g: Put a card at a specific position in its owner's library.
    /// Unlike ChangeZone { destination: Library } which auto-shuffles (CR 401.3),
    /// this uses move_to_library_position for precise placement without shuffling.
    ///
    /// `count` carries the cardinality of the placement ("put **two** cards
    /// from your hand on top of your library in any order" — Cavalier of Gales,
    /// Brainstorm). Defaults to `Fixed(1)` for the dominant "put it on top" /
    /// "put that card on the bottom" forms; the JSON shape stays unchanged via
    /// `default_quantity_one`.
    PutAtLibraryPosition {
        target: TargetFilter,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        position: LibraryPosition,
    },
    /// CR 401.4: Target's owner puts it on top or bottom of their library (owner chooses).
    PutOnTopOrBottom {
        target: TargetFilter,
    },
    /// Deliver a gift to an opponent: draw a card, create a token, etc.
    /// Resolves for the opponent of the ability's controller (2-player: the single opponent).
    GiftDelivery {
        kind: crate::types::keywords::GiftKind,
    },
    /// CR 701.15a: Goad target creature — it must attack each combat if able and must
    /// attack a player other than the goading player if able. Duration is until the
    /// goading player's next turn (UntilNextTurnOf).
    Goad {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.35a: Detain target permanent — until the controller's next turn, that
    /// permanent can't attack or block and its activated abilities can't be activated.
    /// Follows the same per-player tracking pattern as Goad (detained_by on GameObject).
    Detain {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.12a: Exchange control of two target permanents. Each slot carries its
    /// own filter so patterns like "target X you control and target Y an opponent
    /// controls" (Oko, Political Trickery, Shrewd Negotiation) declare distinct
    /// legality per slot, while patterns like "two target X" (Switcheroo, Role
    /// Reversal) reuse the same filter on both slots. Resolution reads exactly two
    /// `TargetRef::Object` entries from `ability.targets`. CR 701.12b: same controller →
    /// no effect. All-or-nothing semantics.
    ExchangeControl {
        #[serde(default = "default_target_filter_any")]
        target_a: TargetFilter,
        #[serde(default = "default_target_filter_any")]
        target_b: TargetFilter,
    },
    /// CR 115.7: Change the target(s) of a spell or ability on the stack.
    /// `target` filters which stack entries are valid to select (e.g. "instant or sorcery spell").
    /// `scope` controls whether a single target or all targets are changed.
    /// `forced_to` is `Some` only when the new target is specified in Oracle text
    /// (e.g. "change the target of that spell to [target]").
    ChangeTargets {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        scope: RetargetScope,
        #[serde(default)]
        forced_to: Option<TargetFilter>,
    },
    /// CR 701.40a: Manifest — put the top card of a player's library onto the
    /// battlefield face down as a 2/2 creature with no text, no name, no
    /// subtypes, and no mana cost. `target` selects whose library is manifested
    /// from: `Controller` for "you manifest..." (Whisperwood Elemental,
    /// Qarsi High Priest) and `ParentTargetController` for "its controller
    /// manifests..." (Reality Shift). `count` determines how many cards to
    /// manifest.
    Manifest {
        target: TargetFilter,
        count: QuantityExpr,
    },
    /// CR 701.62a: Manifest dread — look at top 2 cards of library, manifest one,
    /// put the rest into graveyard. Uses interactive WaitingFor::ManifestDreadChoice.
    ManifestDread,
    /// CR 500.7: Take an extra turn after this one. The target determines who
    /// takes the extra turn (usually Controller for "take an extra turn").
    /// Extra turns are stored as a LIFO stack — most recently created taken first.
    ExtraTurn {
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
    },
    /// CR 614.10: "Skip your next turn." — the affected player's next N turns are skipped.
    /// Stored as a per-player counter in `GameState.turns_to_skip`; decremented during turn
    /// transition in `start_next_turn`. The target determines who skips (usually Controller).
    /// `count` is the number of turns to skip and defaults to 1 for backward compatibility
    /// with legacy `SkipNextTurn { target }` emissions.
    SkipNextTurn {
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 500.8: Add an additional combat phase (and optionally an additional main phase)
    /// after the current phase. Uses a LIFO stack on GameState.extra_phases.
    /// CR 500.10a: Only adds phases to the controller's own turn.
    AdditionalCombatPhase {
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
        /// If true, also adds an additional main phase after the combat phase.
        #[serde(default)]
        with_main_phase: bool,
    },
    /// CR 701.10d-f: Double counters on a permanent, a player's life total, or mana pool.
    /// Uses `DoubleTarget` enum per D-05 to distinguish the three variants.
    /// Existing DoublePT/DoublePTAll handle CR 701.10a-c (power/toughness).
    Double {
        target_kind: DoubleTarget,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// Marker for abilities whose resolution is handled by a dedicated engine path
    /// rather than the normal effect resolution pipeline.
    /// CR 702.49: NinjutsuFamily abilities are resolved via GameAction::ActivateNinjutsu.
    RuntimeHandled {
        handler: RuntimeHandler,
    },
    /// CR 701.53a: Incubate N — create an Incubator token with N +1/+1 counters on it.
    /// The Incubator is a colorless artifact with "{2}: Transform this artifact."
    /// Its back face is a 0/0 colorless Phyrexian artifact creature.
    Incubate {
        /// Number of +1/+1 counters to place on the Incubator token.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 701.47a: Amass [subtype] N — create or grow an Army creature token.
    /// If no Army exists, create a 0/0 black [subtype] Army creature token.
    /// Put N +1/+1 counters on the chosen Army. If it isn't a [subtype], it becomes one.
    Amass {
        /// The creature subtype to add (e.g., "Zombie", "Orc", "Phyrexian").
        subtype: String,
        /// Number of +1/+1 counters to place.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 701.37a: Monstrosity N — if not monstrous, put N +1/+1 counters and become monstrous.
    Monstrosity {
        /// Number of +1/+1 counters to place.
        count: QuantityExpr,
    },
    /// CR 701.39a: Bolster N — choose creature you control with least toughness,
    /// put N +1/+1 counters on it.
    Bolster {
        /// Number of +1/+1 counters to place.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 701.46a: Adapt N — if no +1/+1 counters, put N +1/+1 counters on this permanent.
    Adapt {
        /// Number of +1/+1 counters to place.
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 701.48a: Learn — you may discard a card to draw a card, or get a Lesson from outside the game.
    Learn,
    /// CR 702.166a: Forage — exile three cards from your graveyard or sacrifice a Food.
    Forage,
    /// CR 702.163a: Collect evidence N — exile cards with total mana value N or more from graveyard.
    CollectEvidence {
        #[serde(default = "default_one")]
        amount: u32,
    },
    /// Endure N — if this creature would die, instead remove N damage from it.
    Endure {
        amount: u32,
    },
    /// Blight N as an effect (target player blights N).
    BlightEffect {
        count: u32,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// Alchemy digital-only: randomly pick card(s) from library matching filter,
    /// put to destination (default hand). No reveal, no shuffle, no player choice.
    Seek {
        #[serde(default = "default_target_filter_any")]
        filter: TargetFilter,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// Where the sought card goes. Usually Hand, but some cards put onto Battlefield.
        #[serde(default = "default_zone_hand")]
        destination: Zone,
        #[serde(default)]
        enter_tapped: bool,
    },
    /// CR 119.5: Set a player's life total to a specific number.
    /// The player gains or loses the necessary amount of life to reach the target.
    SetLifeTotal {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        amount: QuantityExpr,
    },
    /// CR 730.1: Set the game's day/night designation.
    /// Triggers daybound/nightbound transformations on all relevant permanents.
    SetDayNight {
        to: crate::types::game_state::DayNight,
    },
    /// CR 110.2: Give control of target permanent to a specified recipient player.
    /// Unlike GainControl (controller takes), GiveControl transfers to a different player.
    GiveControl {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// The player who receives control (usually the targeted opponent).
        #[serde(default = "default_target_filter_any")]
        recipient: TargetFilter,
    },
    /// CR 506.4: Remove a creature from combat — it stops being an attacking,
    /// blocking, blocked, and/or unblocked creature.
    RemoveFromCombat {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// Digital-only keyword action (no CR entry): Conjure creates a card from outside
    /// the game and places it into a specified zone. Unlike tokens, conjured cards are
    /// "real" cards with full characteristics (mana value, types, abilities, etc.).
    Conjure {
        /// One or more (card_name, count) pairs for multi-card conjure patterns.
        cards: Vec<ConjureCard>,
        destination: Zone,
        #[serde(default)]
        tapped: bool,
    },
    /// CR 700.2 + CR 608.2d: Inline "you may [effect A] or [effect B]" —
    /// controller chooses at resolution which of the two (or more) branches to
    /// execute. Building block for optional binary-choice imperatives like
    /// Highway Robbery's "You may discard a card or sacrifice a land" and
    /// analogous "you may X or Y" patterns that are not expressed as a bulleted
    /// `Choose one —` modal block. Each branch is a full `AbilityDefinition`
    /// so it carries its own cost, target, and sub-ability chain. The outer
    /// imperative is already marked `optional: true` when "you may" is
    /// stripped; this effect represents the branching choice once the
    /// controller opts in.
    ///
    /// Resolution: controller picks exactly one branch by index; the chosen
    /// branch's effect resolves normally. Runtime resolver is not yet wired —
    /// the typed shape is emitted so the parser stops silently dropping the
    /// second branch; full runtime support can follow without changing the
    /// card data format.
    ChooseOneOf {
        /// The branches the controller may choose between. Each element is a
        /// self-contained ability (effect + optional cost + optional target).
        branches: Vec<AbilityDefinition>,
    },
    /// Semantic marker for effects the engine has not yet implemented a handler for.
    /// Carries zero HashMap -- architecturally distinct from the removed Effect::Other.
    Unimplemented {
        name: String,
        #[serde(default)]
        description: Option<String>,
    },
}

fn default_one() -> u32 {
    1
}

fn default_one_i32() -> i32 {
    1
}

fn default_quantity_one() -> QuantityExpr {
    QuantityExpr::Fixed { value: 1 }
}

fn is_default_search_selection_constraint(c: &SearchSelectionConstraint) -> bool {
    matches!(c, SearchSelectionConstraint::None)
}

fn default_zone_hand() -> Zone {
    Zone::Hand
}

fn default_zone_graveyard() -> Zone {
    Zone::Graveyard
}

fn default_pt_value_zero() -> PtValue {
    PtValue::Fixed(0)
}

fn default_mana_production() -> ManaProduction {
    ManaProduction::Fixed {
        colors: Vec::new(),
        contribution: ManaContribution::Base,
    }
}

fn default_all_mana_colors() -> Vec<ManaColor> {
    vec![
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ]
}

fn default_two_i32() -> i32 {
    2
}

pub(crate) fn default_target_filter_any() -> TargetFilter {
    TargetFilter::Any
}

fn default_target_filter_none() -> TargetFilter {
    TargetFilter::None
}

fn default_target_filter_controller() -> TargetFilter {
    TargetFilter::Controller
}

fn default_target_filter_self_ref() -> TargetFilter {
    TargetFilter::SelfRef
}

fn target_filter_is_self_ref(filter: &TargetFilter) -> bool {
    matches!(filter, TargetFilter::SelfRef)
}

fn normalize_and_filter(filters: Vec<TargetFilter>) -> TargetFilter {
    let mut typed_filters = Vec::new();
    let mut other_filters = Vec::new();

    for filter in filters.into_iter().map(TargetFilter::normalized) {
        match filter {
            TargetFilter::And { filters } => {
                for nested in filters {
                    collect_and_filter(nested, &mut typed_filters, &mut other_filters);
                }
            }
            filter => collect_and_filter(filter, &mut typed_filters, &mut other_filters),
        }
    }

    let mut normalized = Vec::with_capacity(typed_filters.len() + other_filters.len());
    normalized.extend(typed_filters.into_iter().map(TargetFilter::Typed));
    normalized.extend(other_filters);

    match normalized.len() {
        0 => TargetFilter::And {
            filters: normalized,
        },
        1 => normalized.pop().expect("length checked"),
        _ => TargetFilter::And {
            filters: normalized,
        },
    }
}

fn collect_and_filter(
    filter: TargetFilter,
    typed_filters: &mut Vec<TypedFilter>,
    other_filters: &mut Vec<TargetFilter>,
) {
    match filter {
        TargetFilter::Typed(filter) => merge_typed_filter(filter.normalized(), typed_filters),
        filter => other_filters.push(filter),
    }
}

fn merge_typed_filter(filter: TypedFilter, typed_filters: &mut Vec<TypedFilter>) {
    if let Some(existing) = typed_filters
        .iter_mut()
        .find(|existing| typed_filters_are_mergeable(existing, &filter))
    {
        merge_type_filter_vec(&mut existing.type_filters, filter.type_filters);
        merge_controller(&mut existing.controller, filter.controller);
        merge_filter_prop_vec(&mut existing.properties, filter.properties);
    } else {
        typed_filters.push(filter);
    }
}

fn typed_filters_are_mergeable(left: &TypedFilter, right: &TypedFilter) -> bool {
    match (&left.controller, &right.controller) {
        (Some(left), Some(right)) => left == right,
        _ => true,
    }
}

fn merge_controller(existing: &mut Option<ControllerRef>, incoming: Option<ControllerRef>) {
    if existing.is_none() {
        *existing = incoming;
    }
}

fn merge_type_filter_vec(existing: &mut Vec<TypeFilter>, incoming: Vec<TypeFilter>) {
    for filter in incoming {
        if !existing.contains(&filter) {
            existing.push(filter);
        }
    }
    *existing = normalized_type_filters(std::mem::take(existing));
}

fn merge_filter_prop_vec(existing: &mut Vec<FilterProp>, incoming: Vec<FilterProp>) {
    for prop in incoming {
        if !existing.contains(&prop) {
            existing.push(prop);
        }
    }
}

fn normalized_type_filters(filters: Vec<TypeFilter>) -> Vec<TypeFilter> {
    let mut normalized = Vec::with_capacity(filters.len());
    for filter in filters {
        if !normalized.contains(&filter) {
            normalized.push(filter);
        }
    }

    if normalized.iter().any(|filter| {
        matches!(
            filter,
            TypeFilter::Creature
                | TypeFilter::Land
                | TypeFilter::Artifact
                | TypeFilter::Enchantment
                | TypeFilter::Planeswalker
                | TypeFilter::Battle
        )
    }) {
        normalized.retain(|filter| !matches!(filter, TypeFilter::Permanent));
    }

    normalized
}

fn normalized_filter_props(props: Vec<FilterProp>) -> Vec<FilterProp> {
    let mut normalized = Vec::with_capacity(props.len());
    for prop in props.into_iter().map(normalized_filter_prop) {
        if !normalized.contains(&prop) {
            normalized.push(prop);
        }
    }
    normalized
}

fn normalized_filter_prop(prop: FilterProp) -> FilterProp {
    match prop {
        FilterProp::DifferentNameFrom { filter } => FilterProp::DifferentNameFrom {
            filter: Box::new(filter.normalized()),
        },
        FilterProp::SharesQuality {
            quality,
            reference,
            relation,
        } => FilterProp::SharesQuality {
            quality,
            reference: reference.map(|filter| Box::new(filter.normalized())),
            relation,
        },
        FilterProp::TargetsOnly { filter } => FilterProp::TargetsOnly {
            filter: Box::new(filter.normalized()),
        },
        FilterProp::Targets { filter } => FilterProp::Targets {
            filter: Box::new(filter.normalized()),
        },
        prop => prop,
    }
}

/// CR 701.38a + CR 101.4: Default starting voter for `Effect::Vote` is the
/// ability controller ("starting with you"). Defining this as a free function
/// (not an enum default) keeps the serde shape stable across schema upgrades.
fn default_controller_ref_you() -> ControllerRef {
    ControllerRef::You
}

impl TargetFilter {
    pub fn normalized(self) -> Self {
        match self {
            TargetFilter::Typed(filter) => TargetFilter::Typed(filter.normalized()),
            TargetFilter::Not { filter } => TargetFilter::Not {
                filter: Box::new(filter.normalized()),
            },
            TargetFilter::Or { filters } => TargetFilter::Or {
                filters: filters.into_iter().map(TargetFilter::normalized).collect(),
            },
            TargetFilter::And { filters } => normalize_and_filter(filters),
            TargetFilter::TrackedSetFiltered { id, filter } => TargetFilter::TrackedSetFiltered {
                id,
                filter: Box::new(filter.normalized()),
            },
            filter => filter,
        }
    }

    /// CR 115.1: Returns true for filters that are NOT player-chosen targets —
    /// context references (triggering event participants per CR 603.7c),
    /// parent target anaphora, and self-references resolve automatically
    /// without target selection.
    pub fn is_context_ref(&self) -> bool {
        matches!(
            self,
            TargetFilter::None
                | TargetFilter::SelfRef
                | TargetFilter::Controller
                | TargetFilter::TriggeringSpellController
                | TargetFilter::TriggeringSpellOwner
                | TargetFilter::TriggeringPlayer
                | TargetFilter::TriggeringSource
                | TargetFilter::DefendingPlayer
                | TargetFilter::AttachedTo
                | TargetFilter::ParentTarget
                | TargetFilter::ParentTargetController
                | TargetFilter::PostReplacementSourceController
        )
    }

    /// Extract the `InZone` zone from this filter's properties, if any.
    ///
    /// Recursively checks `Typed`, `Or`, `And`, and `Not` variants.
    /// Returns the first `InZone` zone found, or `None` if the filter
    /// has no zone constraint (defaulting to battlefield for counting).
    pub fn extract_in_zone(&self) -> Option<crate::types::zones::Zone> {
        match self {
            TargetFilter::Typed(tf) => tf.properties.iter().find_map(|p| match p {
                FilterProp::InZone { zone } => Some(*zone),
                _ => None,
            }),
            TargetFilter::Or { filters } | TargetFilter::And { filters } => {
                filters.iter().find_map(|f| f.extract_in_zone())
            }
            TargetFilter::Not { filter } => filter.extract_in_zone(),
            TargetFilter::ExiledBySource => Some(crate::types::zones::Zone::Exile),
            _ => None,
        }
    }

    /// CR 400.3 + CR 701.23: Returns the union of explicit zone constraints in this filter.
    /// Preserves the multi-zone semantics of `FilterProp::InAnyZone` (e.g.
    /// "search ... graveyard, hand, and library") that `extract_in_zone` collapses
    /// to a single zone. Falls back to the single `InZone` when only that variant
    /// is present. Returns an empty Vec when the filter imposes no zone constraint.
    pub fn extract_zones(&self) -> Vec<crate::types::zones::Zone> {
        let mut out = Vec::new();
        self.collect_zones(&mut out);
        out
    }

    fn collect_zones(&self, out: &mut Vec<crate::types::zones::Zone>) {
        match self {
            TargetFilter::Typed(tf) => {
                for p in &tf.properties {
                    match p {
                        FilterProp::InAnyZone { zones } => {
                            for z in zones {
                                if !out.contains(z) {
                                    out.push(*z);
                                }
                            }
                        }
                        FilterProp::InZone { zone } if !out.contains(zone) => {
                            out.push(*zone);
                        }
                        _ => {}
                    }
                }
            }
            TargetFilter::Or { filters } | TargetFilter::And { filters } => {
                for f in filters {
                    f.collect_zones(out);
                }
            }
            TargetFilter::Not { filter } => filter.collect_zones(out),
            _ => {}
        }
    }
}

impl fmt::Debug for Effect {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // JSON serialization instead of derived Debug — avoids stack overflow from
        // Effect ↔ AbilityDefinition mutual recursion and produces structured output
        // optimized for LLM consumption. Uses Serialize (not Debug) internally,
        // completely breaking the recursive Debug chain.
        // Respects the alternate flag: `{:#?}` → pretty JSON, `{:?}` → compact JSON.
        let json = if f.alternate() {
            serde_json::to_string_pretty(self)
        } else {
            serde_json::to_string(self)
        };
        match json {
            Ok(s) => f.write_str(&s),
            Err(_) => {
                let variant: &'static str = self.into();
                write!(f, "Effect::{variant} {{ .. }}")
            }
        }
    }
}

impl Effect {
    /// CR 115.1: Returns the target filter for effects that have a player-selectable
    /// `target` field. Returns `None` for effects with no target field, or whose
    /// targeting is handled through different mechanisms (filters, zones, etc.).
    ///
    /// Exhaustive match — no wildcards — so the compiler forces an update when new
    /// Effect variants are added.
    pub fn target_filter(&self) -> Option<&TargetFilter> {
        match self {
            // --- Effects with a `target: TargetFilter` field ---
            Effect::DealDamage { target, .. }
            | Effect::Draw { target, .. }
            | Effect::Scry { target, .. }
            | Effect::Surveil { target, .. }
            | Effect::Pump { target, .. }
            | Effect::Destroy { target, .. }
            | Effect::Regenerate { target, .. }
            | Effect::Counter { target, .. }
            | Effect::Tap { target, .. }
            | Effect::Untap { target, .. }
            | Effect::AddCounter { target, .. }
            | Effect::RemoveCounter { target, .. }
            | Effect::Sacrifice { target, .. }
            | Effect::DiscardCard { target, .. }
            | Effect::Mill { target, .. }
            | Effect::ChangeZone { target, .. }
            | Effect::GainControl { target, .. }
            | Effect::ControlNextTurn { target, .. }
            | Effect::Attach { target, .. }
            | Effect::Fight { target, .. }
            | Effect::Bounce { target, .. }
            | Effect::SwitchPT { target, .. }
            | Effect::CopySpell { target, .. }
            | Effect::CopyTokenOf { target, .. }
            | Effect::BecomeCopy { target, .. }
            | Effect::ChooseCard { target, .. }
            | Effect::PutCounter { target, .. }
            | Effect::MultiplyCounter { target, .. }
            | Effect::DoublePT { target, .. }
            | Effect::MoveCounters { target, .. }
            | Effect::Animate { target, .. }
            | Effect::Discard { target, .. }
            | Effect::Shuffle { target, .. }
            | Effect::Transform { target, .. }
            | Effect::RevealHand { target, .. }
            | Effect::Reveal { target, .. }
            | Effect::TargetOnly { target, .. }
            | Effect::Suspect { target, .. }
            | Effect::Connive { target, .. }
            | Effect::PhaseOut { target, .. }
            | Effect::PhaseIn { target, .. }
            | Effect::ForceBlock { target, .. }
            | Effect::BecomePrepared { target, .. }
            | Effect::BecomeUnprepared { target, .. }
            | Effect::CastFromZone { target, .. }
            | Effect::PreventDamage { target, .. }
            | Effect::Exploit { target, .. }
            | Effect::GivePlayerCounter { target, .. }
            | Effect::LoseAllPlayerCounters { target, .. }
            | Effect::PutAtLibraryPosition { target, .. }
            | Effect::PutOnTopOrBottom { target, .. }
            | Effect::Goad { target, .. }
            | Effect::Detain { target, .. }
            | Effect::ExtraTurn { target, .. }
            | Effect::SkipNextTurn { target, .. }
            | Effect::AdditionalCombatPhase { target, .. }
            | Effect::Double { target, .. }
            | Effect::BlightEffect { target, .. }
            | Effect::SetLifeTotal { target, .. }
            | Effect::GiveControl { target, .. }
            | Effect::RemoveFromCombat { target, .. } => Some(target),

            Effect::ExileTop { player, .. } => Some(player),

            // CR 111.2 + CR 601.2c: "Target player creates ..." token modes
            // (e.g. Ashling's Command mode 4, Brigid's Command, Prismari Command)
            // surface their token-creation target as the `owner` filter — the
            // player who creates the token is its owner. The default
            // `TargetFilter::Controller` preserves "you create ..." semantics.
            Effect::Token { owner, .. } => Some(owner),

            // GenericEffect and LoseLife have Option<TargetFilter>
            Effect::GenericEffect { target, .. } | Effect::LoseLife { target, .. } => {
                target.as_ref()
            }

            // CR 115.1 + CR 115.7: Mana abilities normally don't target, but a
            // few spell-only mana effects (Jeska's Will mode 1: "Add {R} for
            // each card in target opponent's hand") declare a player target so
            // the `TargetZoneCardCount` quantity in `produced` can resolve
            // against `ability.targets`. The optional `target` is `None` for
            // every classic mana ability (Cabal Coffers, Reflecting Pool, etc.).
            Effect::Mana { target, .. } => target.as_ref(),

            // --- Effects with no player-selectable target field ---
            // These use filters, zone-level operations, or have no targeting at all.
            Effect::StartYourEngines { .. }
            | Effect::IncreaseSpeed { .. }
            | Effect::GainLife { .. }
            | Effect::PumpAll { .. }
            | Effect::DamageAll { .. }
            | Effect::DamageEachPlayer { .. }
            | Effect::DestroyAll { .. }
            | Effect::TapAll { .. }
            | Effect::UntapAll { .. }
            | Effect::BounceAll { .. }
            | Effect::CounterAll { .. }
            | Effect::ChangeZoneAll { .. }
            | Effect::Dig { .. }
            | Effect::PutCounterAll { .. }
            | Effect::DoublePTAll { .. }
            | Effect::Explore
            | Effect::Investigate
            | Effect::Tribute { .. }
            | Effect::BecomeMonarch
            | Effect::Proliferate
            | Effect::Populate
            | Effect::Clash
            | Effect::Vote { .. }
            | Effect::Cleanup { .. }
            | Effect::RevealTop { .. }
            | Effect::Choose { .. }
            | Effect::ChooseDamageSource { .. }
            | Effect::SolveCase
            | Effect::SetClassLevel { .. }
            | Effect::CreateDelayedTrigger { .. }
            | Effect::AddTargetReplacement { .. }
            | Effect::AddRestriction { .. }
            | Effect::ReduceNextSpellCost { .. }
            | Effect::GrantNextSpellAbility { .. }
            | Effect::AddPendingETBCounters { .. }
            | Effect::CreateEmblem { .. }
            | Effect::PayCost { .. }
            | Effect::GrantCastingPermission { .. }
            | Effect::RegisterBending { .. }
            | Effect::ChooseFromZone { .. }
            | Effect::ChooseAndSacrificeRest { .. }
            | Effect::GainEnergy { .. }
            | Effect::ExileFromTopUntil { .. }
            | Effect::RevealUntil { .. }
            | Effect::Discover { .. }
            | Effect::Cascade
            | Effect::MiracleCast { .. }
            | Effect::MadnessCast { .. }
            | Effect::GiftDelivery { .. }
            | Effect::ExchangeControl { .. }
            | Effect::ChangeTargets { .. }
            | Effect::Manifest { .. }
            | Effect::ManifestDread
            | Effect::LoseTheGame
            | Effect::WinTheGame
            | Effect::RollDie { .. }
            | Effect::FlipCoin { .. }
            | Effect::FlipCoins { .. }
            | Effect::FlipCoinUntilLose { .. }
            | Effect::RingTemptsYou
            | Effect::VentureIntoDungeon
            | Effect::VentureInto { .. }
            | Effect::TakeTheInitiative
            | Effect::Incubate { .. }
            | Effect::Amass { .. }
            | Effect::Monstrosity { .. }
            | Effect::Bolster { .. }
            | Effect::Adapt { .. }
            | Effect::Learn
            | Effect::Forage
            | Effect::CollectEvidence { .. }
            | Effect::Endure { .. }
            | Effect::ExploreAll { .. }
            | Effect::Seek { .. }
            | Effect::SetDayNight { .. }
            | Effect::TimeTravel
            | Effect::RuntimeHandled { .. }
            | Effect::Conjure { .. }
            | Effect::ChooseOneOf { .. }
            | Effect::Unimplemented { .. }
            // CR 701.20a: RevealFromHand implicitly targets the controller's own hand;
            // it has no discrete `target` field for the generic targeting layer.
            | Effect::RevealFromHand { .. } => None,
            // CR 701.23a: SearchLibrary has an optional player target for opponent search.
            Effect::SearchLibrary { target_player, .. } => target_player.as_ref(),
        }
    }
}

/// Returns the human-readable variant name for an Effect.
/// Production API for GameEvent::EffectResolved api_type strings and logging.
pub fn effect_variant_name(effect: &Effect) -> &str {
    match effect {
        Effect::StartYourEngines { .. } => "StartYourEngines",
        Effect::IncreaseSpeed { .. } => "IncreaseSpeed",
        Effect::DealDamage { .. } => "DealDamage",
        Effect::Draw { .. } => "Draw",
        Effect::Pump { .. } => "Pump",
        Effect::Destroy { .. } => "Destroy",
        Effect::Regenerate { .. } => "Regenerate",
        Effect::Counter { .. } => "Counter",
        Effect::CounterAll { .. } => "CounterAll",
        Effect::Token { .. } => "Token",
        Effect::GainLife { .. } => "GainLife",
        Effect::LoseLife { .. } => "LoseLife",
        Effect::Tap { .. } => "Tap",
        Effect::Untap { .. } => "Untap",
        Effect::TapAll { .. } => "TapAll",
        Effect::UntapAll { .. } => "UntapAll",
        Effect::AddCounter { .. } => "AddCounter",
        Effect::RemoveCounter { .. } => "RemoveCounter",
        Effect::Sacrifice { .. } => "Sacrifice",
        Effect::DiscardCard { .. } => "DiscardCard",
        Effect::Mill { .. } => "Mill",
        Effect::Scry { .. } => "Scry",
        Effect::PumpAll { .. } => "PumpAll",
        Effect::DamageAll { .. } => "DamageAll",
        Effect::DamageEachPlayer { .. } => "DamageEachPlayer",
        Effect::DestroyAll { .. } => "DestroyAll",
        Effect::ChangeZone { .. } => "ChangeZone",
        Effect::ChangeZoneAll { .. } => "ChangeZoneAll",
        Effect::Dig { .. } => "Dig",
        Effect::GainControl { .. } => "GainControl",
        Effect::ControlNextTurn { .. } => "ControlNextTurn",
        Effect::Attach { .. } => "Attach",
        Effect::Surveil { .. } => "Surveil",
        Effect::Fight { .. } => "Fight",
        Effect::Bounce { .. } => "Bounce",
        Effect::BounceAll { .. } => "BounceAll",
        Effect::Explore => "Explore",
        Effect::ExploreAll { .. } => "ExploreAll",
        Effect::Investigate => "Investigate",
        Effect::Tribute { .. } => "Tribute",
        Effect::TimeTravel => "TimeTravel",
        Effect::BecomeMonarch => "BecomeMonarch",
        Effect::Proliferate => "Proliferate",
        Effect::Populate => "Populate",
        Effect::Clash => "Clash",
        Effect::Vote { .. } => "Vote",
        Effect::SwitchPT { .. } => "SwitchPT",
        Effect::CopySpell { .. } => "CopySpell",
        Effect::CopyTokenOf { .. } => "CopyTokenOf",
        Effect::BecomeCopy { .. } => "BecomeCopy",
        Effect::ChooseCard { .. } => "ChooseCard",
        Effect::PutCounter { .. } => "PutCounter",
        Effect::PutCounterAll { .. } => "PutCounterAll",
        Effect::MultiplyCounter { .. } => "MultiplyCounter",
        Effect::DoublePT { .. } => "DoublePT",
        Effect::DoublePTAll { .. } => "DoublePTAll",
        Effect::MoveCounters { .. } => "MoveCounters",
        Effect::Animate { .. } => "Animate",
        Effect::RegisterBending { .. } => "RegisterBending",
        Effect::GenericEffect { .. } => "Effect",
        Effect::Cleanup { .. } => "Cleanup",
        Effect::Mana { .. } => "Mana",
        Effect::Discard { .. } => "Discard",
        Effect::Shuffle { .. } => "Shuffle",
        Effect::Transform { .. } => "Transform",
        Effect::SearchLibrary { .. } => "SearchLibrary",
        Effect::RevealHand { .. } => "RevealHand",
        Effect::RevealFromHand { .. } => "RevealFromHand",
        Effect::Reveal { .. } => "Reveal",
        Effect::RevealTop { .. } => "RevealTop",
        Effect::ExileTop { .. } => "ExileTop",
        Effect::TargetOnly { .. } => "TargetOnly",
        Effect::Choose { .. } => "Choose",
        Effect::ChooseDamageSource { .. } => "ChooseDamageSource",
        Effect::Suspect { .. } => "Suspect",
        Effect::Connive { .. } => "Connive",
        Effect::PhaseOut { .. } => "PhaseOut",
        Effect::PhaseIn { .. } => "PhaseIn",
        Effect::ForceBlock { .. } => "ForceBlock",
        Effect::SolveCase => "SolveCase",
        Effect::BecomePrepared { .. } => "BecomePrepared",
        Effect::BecomeUnprepared { .. } => "BecomeUnprepared",
        Effect::SetClassLevel { .. } => "SetClassLevel",
        Effect::CreateDelayedTrigger { .. } => "CreateDelayedTrigger",
        Effect::AddTargetReplacement { .. } => "AddTargetReplacement",
        Effect::AddRestriction { .. } => "AddRestriction",
        Effect::ReduceNextSpellCost { .. } => "ReduceNextSpellCost",
        Effect::GrantNextSpellAbility { .. } => "GrantNextSpellAbility",
        Effect::AddPendingETBCounters { .. } => "AddPendingETBCounters",
        Effect::CreateEmblem { .. } => "CreateEmblem",
        Effect::PayCost { .. } => "PayCost",
        Effect::CastFromZone { .. } => "CastFromZone",
        Effect::PreventDamage { .. } => "PreventDamage",
        Effect::LoseTheGame => "LoseTheGame",
        Effect::WinTheGame => "WinTheGame",
        Effect::RollDie { .. } => "RollDie",
        Effect::FlipCoin { .. } => "FlipCoin",
        Effect::FlipCoins { .. } => "FlipCoins",
        Effect::FlipCoinUntilLose { .. } => "FlipCoinUntilLose",
        Effect::RingTemptsYou => "RingTemptsYou",
        Effect::VentureIntoDungeon => "VentureIntoDungeon",
        Effect::VentureInto { .. } => "VentureInto",
        Effect::TakeTheInitiative => "TakeTheInitiative",
        Effect::GrantCastingPermission { .. } => "GrantCastingPermission",
        Effect::ChooseFromZone { .. } => "ChooseFromZone",
        Effect::ChooseAndSacrificeRest { .. } => "ChooseAndSacrificeRest",
        Effect::Exploit { .. } => "Exploit",
        Effect::GainEnergy { .. } => "GainEnergy",
        Effect::GivePlayerCounter { .. } => "GivePlayerCounter",
        Effect::LoseAllPlayerCounters { .. } => "LoseAllPlayerCounters",
        Effect::ExileFromTopUntil { .. } => "ExileFromTopUntil",
        Effect::RevealUntil { .. } => "RevealUntil",
        Effect::Discover { .. } => "Discover",
        Effect::Cascade => "Cascade",
        Effect::MiracleCast { .. } => "MiracleCast",
        Effect::MadnessCast { .. } => "MadnessCast",
        Effect::PutAtLibraryPosition { .. } => "PutAtLibraryPosition",
        Effect::PutOnTopOrBottom { .. } => "PutOnTopOrBottom",
        Effect::GiftDelivery { .. } => "GiftDelivery",
        Effect::Goad { .. } => "Goad",
        Effect::Detain { .. } => "Detain",
        Effect::ExchangeControl { .. } => "ExchangeControl",
        Effect::ChangeTargets { .. } => "ChangeTargets",
        Effect::Incubate { .. } => "Incubate",
        Effect::Amass { .. } => "Amass",
        Effect::Monstrosity { .. } => "Monstrosity",
        Effect::Bolster { .. } => "Bolster",
        Effect::Adapt { .. } => "Adapt",
        Effect::Manifest { .. } => "Manifest",
        Effect::ManifestDread => "ManifestDread",
        Effect::ExtraTurn { .. } => "ExtraTurn",
        Effect::SkipNextTurn { .. } => "SkipNextTurn",
        Effect::AdditionalCombatPhase { .. } => "AdditionalCombatPhase",
        Effect::Double { .. } => "Double",
        Effect::RuntimeHandled { handler } => match handler {
            RuntimeHandler::NinjutsuFamily => "RuntimeHandled:NinjutsuFamily",
        },
        Effect::Learn => "Learn",
        Effect::Forage => "Forage",
        Effect::CollectEvidence { .. } => "CollectEvidence",
        Effect::Endure { .. } => "Endure",
        Effect::BlightEffect { .. } => "BlightEffect",
        Effect::Seek { .. } => "Seek",
        Effect::SetLifeTotal { .. } => "SetLifeTotal",
        Effect::SetDayNight { .. } => "SetDayNight",
        Effect::GiveControl { .. } => "GiveControl",
        Effect::RemoveFromCombat { .. } => "RemoveFromCombat",
        Effect::Conjure { .. } => "Conjure",
        Effect::ChooseOneOf { .. } => "ChooseOneOf",
        Effect::Unimplemented { name, .. } => name,
    }
}

// ---------------------------------------------------------------------------
// Effect kind — typed discriminant for GameEvent::EffectResolved
// ---------------------------------------------------------------------------

/// Typed tag carried by `GameEvent::EffectResolved`.
/// Replaces the former `api_type: String` field with a compile-time-checked enum.
/// Variants mirror `Effect` variants 1:1, plus a few engine-level emits (Equip)
/// and trigger-condition placeholders (Reveal, Transform, TurnFaceUp, DayTimeChange).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum EffectKind {
    StartYourEngines,
    IncreaseSpeed,
    DealDamage,
    Draw,
    Pump,
    Destroy,
    Counter,
    CounterAll,
    Token,
    GainLife,
    LoseLife,
    Tap,
    Untap,
    AddCounter,
    RemoveCounter,
    Sacrifice,
    DiscardCard,
    Mill,
    Scry,
    PumpAll,
    DamageAll,
    DamageEachPlayer,
    DestroyAll,
    TapAll,
    UntapAll,
    ChangeZone,
    ChangeZoneAll,
    Dig,
    GainControl,
    ControlNextTurn,
    Attach,
    AttachAll,
    Surveil,
    Fight,
    Bounce,
    BounceAll,
    Explore,
    ExploreAll,
    Investigate,
    Tribute,
    TimeTravel,
    BecomeMonarch,
    Proliferate,
    Populate,
    Clash,
    /// CR 701.38: Vote — interactive APNAP-ordered choice with per-choice tally effects.
    Vote,
    SwitchPT,
    CopySpell,
    CopyTokenOf,
    BecomeCopy,
    ChooseCard,
    PutCounter,
    PutCounterAll,
    MultiplyCounter,
    DoublePT,
    DoublePTAll,
    MoveCounters,
    Animate,
    RegisterBending,
    GenericEffect,
    Cleanup,
    Mana,
    Discard,
    Shuffle,
    SearchLibrary,
    ExileTop,
    TargetOnly,
    Choose,
    ChooseDamageSource,
    Suspect,
    Connive,
    PhaseOut,
    PhaseIn,
    ForceBlock,
    SolveCase,
    /// CR 702.xxx: Prepare (Strixhaven) — mark target creature as prepared.
    BecomePrepared,
    /// CR 702.xxx: Prepare (Strixhaven) — clear prepared state on target.
    BecomeUnprepared,
    SetClassLevel,
    CreateDelayedTrigger,
    AddTargetReplacement,
    AddRestriction,
    ReduceNextSpellCost,
    GrantNextSpellAbility,
    AddPendingETBCounters,
    CreateEmblem,
    PayCost,
    CastFromZone,
    PreventDamage,
    Regenerate,
    LoseTheGame,
    WinTheGame,
    RollDie,
    FlipCoin,
    FlipCoins,
    FlipCoinUntilLose,
    RingTemptsYou,
    VentureIntoDungeon,
    VentureInto,
    TakeTheInitiative,
    GrantCastingPermission,
    ChooseFromZone,
    ChooseAndSacrificeRest,
    Exploit,
    GainEnergy,
    GivePlayerCounter,
    LoseAllPlayerCounters,
    ExileFromTopUntil,
    RevealUntil,
    Discover,
    Cascade,
    MiracleCast,
    MadnessCast,
    PutAtLibraryPosition,
    PutOnTopOrBottom,
    GiftDelivery,
    Goad,
    Detain,
    ExchangeControl,
    ChangeTargets,
    Incubate,
    Amass,
    Monstrosity,
    Bolster,
    Adapt,
    Manifest,
    ManifestDread,
    ExtraTurn,
    SkipNextTurn,
    AdditionalCombatPhase,
    Double,
    RuntimeHandled,
    Learn,
    Forage,
    CollectEvidence,
    Endure,
    BlightEffect,
    Seek,
    SetLifeTotal,
    SetDayNight,
    GiveControl,
    RemoveFromCombat,
    Conjure,
    ChooseOneOf,
    Unimplemented,
    /// Engine-level equip action (not via an Effect handler).
    Equip,
    /// CR 702.122a: Engine-level crew action (not via an Effect handler).
    Crew,
    /// CR 702.184a: Engine-level station action (not via an Effect handler).
    Station,
    /// CR 702.171a: Engine-level saddle action (not via an Effect handler).
    Saddle,
    /// Trigger-condition placeholders — emitters not yet implemented.
    Reveal,
    Transform,
    TurnFaceUp,
    DayTimeChange,
}

impl From<&Effect> for EffectKind {
    fn from(effect: &Effect) -> Self {
        match effect {
            Effect::StartYourEngines { .. } => EffectKind::StartYourEngines,
            Effect::IncreaseSpeed { .. } => EffectKind::IncreaseSpeed,
            Effect::DealDamage { .. } => EffectKind::DealDamage,
            Effect::Draw { .. } => EffectKind::Draw,
            Effect::Pump { .. } => EffectKind::Pump,
            Effect::Destroy { .. } => EffectKind::Destroy,
            Effect::Regenerate { .. } => EffectKind::Regenerate,
            Effect::Counter { .. } => EffectKind::Counter,
            Effect::CounterAll { .. } => EffectKind::CounterAll,
            Effect::Token { .. } => EffectKind::Token,
            Effect::GainLife { .. } => EffectKind::GainLife,
            Effect::LoseLife { .. } => EffectKind::LoseLife,
            Effect::Tap { .. } => EffectKind::Tap,
            Effect::Untap { .. } => EffectKind::Untap,
            Effect::TapAll { .. } => EffectKind::TapAll,
            Effect::UntapAll { .. } => EffectKind::UntapAll,
            Effect::AddCounter { .. } => EffectKind::AddCounter,
            Effect::RemoveCounter { .. } => EffectKind::RemoveCounter,
            Effect::Sacrifice { .. } => EffectKind::Sacrifice,
            Effect::DiscardCard { .. } => EffectKind::DiscardCard,
            Effect::Mill { .. } => EffectKind::Mill,
            Effect::Scry { .. } => EffectKind::Scry,
            Effect::PumpAll { .. } => EffectKind::PumpAll,
            Effect::DamageAll { .. } => EffectKind::DamageAll,
            Effect::DamageEachPlayer { .. } => EffectKind::DamageEachPlayer,
            Effect::DestroyAll { .. } => EffectKind::DestroyAll,
            Effect::ChangeZone { .. } => EffectKind::ChangeZone,
            Effect::ChangeZoneAll { .. } => EffectKind::ChangeZoneAll,
            Effect::Dig { .. } => EffectKind::Dig,
            Effect::GainControl { .. } => EffectKind::GainControl,
            Effect::ControlNextTurn { .. } => EffectKind::ControlNextTurn,
            Effect::Attach { .. } => EffectKind::Attach,
            Effect::Surveil { .. } => EffectKind::Surveil,
            Effect::Fight { .. } => EffectKind::Fight,
            Effect::Bounce { .. } => EffectKind::Bounce,
            Effect::BounceAll { .. } => EffectKind::BounceAll,
            Effect::Explore => EffectKind::Explore,
            Effect::ExploreAll { .. } => EffectKind::ExploreAll,
            Effect::Investigate => EffectKind::Investigate,
            Effect::Tribute { .. } => EffectKind::Tribute,
            Effect::TimeTravel => EffectKind::TimeTravel,
            Effect::BecomeMonarch => EffectKind::BecomeMonarch,
            Effect::Proliferate => EffectKind::Proliferate,
            Effect::Populate => EffectKind::Populate,
            Effect::Clash => EffectKind::Clash,
            Effect::Vote { .. } => EffectKind::Vote,
            Effect::SwitchPT { .. } => EffectKind::SwitchPT,
            Effect::CopySpell { .. } => EffectKind::CopySpell,
            Effect::CopyTokenOf { .. } => EffectKind::CopyTokenOf,
            Effect::BecomeCopy { .. } => EffectKind::BecomeCopy,
            Effect::ChooseCard { .. } => EffectKind::ChooseCard,
            Effect::PutCounter { .. } => EffectKind::PutCounter,
            Effect::PutCounterAll { .. } => EffectKind::PutCounterAll,
            Effect::MultiplyCounter { .. } => EffectKind::MultiplyCounter,
            Effect::DoublePT { .. } => EffectKind::DoublePT,
            Effect::DoublePTAll { .. } => EffectKind::DoublePTAll,
            Effect::MoveCounters { .. } => EffectKind::MoveCounters,
            Effect::Animate { .. } => EffectKind::Animate,
            Effect::RegisterBending { .. } => EffectKind::RegisterBending,
            Effect::GenericEffect { .. } => EffectKind::GenericEffect,
            Effect::Cleanup { .. } => EffectKind::Cleanup,
            Effect::Mana { .. } => EffectKind::Mana,
            Effect::Discard { .. } => EffectKind::Discard,
            Effect::Shuffle { .. } => EffectKind::Shuffle,
            Effect::Transform { .. } => EffectKind::Transform,
            Effect::SearchLibrary { .. } => EffectKind::SearchLibrary,
            Effect::RevealHand { .. } => EffectKind::Reveal,
            Effect::RevealFromHand { .. } => EffectKind::Reveal,
            Effect::Reveal { .. } => EffectKind::Reveal,
            Effect::RevealTop { .. } => EffectKind::Reveal,
            Effect::ExileTop { .. } => EffectKind::ExileTop,
            Effect::TargetOnly { .. } => EffectKind::TargetOnly,
            Effect::Choose { .. } => EffectKind::Choose,
            Effect::ChooseDamageSource { .. } => EffectKind::ChooseDamageSource,
            Effect::Suspect { .. } => EffectKind::Suspect,
            Effect::Connive { .. } => EffectKind::Connive,
            Effect::PhaseOut { .. } => EffectKind::PhaseOut,
            Effect::PhaseIn { .. } => EffectKind::PhaseIn,
            Effect::ForceBlock { .. } => EffectKind::ForceBlock,
            Effect::SolveCase => EffectKind::SolveCase,
            Effect::BecomePrepared { .. } => EffectKind::BecomePrepared,
            Effect::BecomeUnprepared { .. } => EffectKind::BecomeUnprepared,
            Effect::SetClassLevel { .. } => EffectKind::SetClassLevel,
            Effect::CreateDelayedTrigger { .. } => EffectKind::CreateDelayedTrigger,
            Effect::AddTargetReplacement { .. } => EffectKind::AddTargetReplacement,
            Effect::AddRestriction { .. } => EffectKind::AddRestriction,
            Effect::ReduceNextSpellCost { .. } => EffectKind::ReduceNextSpellCost,
            Effect::GrantNextSpellAbility { .. } => EffectKind::GrantNextSpellAbility,
            Effect::AddPendingETBCounters { .. } => EffectKind::AddPendingETBCounters,
            Effect::CreateEmblem { .. } => EffectKind::CreateEmblem,
            Effect::PayCost { .. } => EffectKind::PayCost,
            Effect::CastFromZone { .. } => EffectKind::CastFromZone,
            Effect::PreventDamage { .. } => EffectKind::PreventDamage,
            Effect::LoseTheGame => EffectKind::LoseTheGame,
            Effect::WinTheGame => EffectKind::WinTheGame,
            Effect::RollDie { .. } => EffectKind::RollDie,
            Effect::FlipCoin { .. } => EffectKind::FlipCoin,
            Effect::FlipCoins { .. } => EffectKind::FlipCoins,
            Effect::FlipCoinUntilLose { .. } => EffectKind::FlipCoinUntilLose,
            Effect::RingTemptsYou => EffectKind::RingTemptsYou,
            Effect::VentureIntoDungeon => EffectKind::VentureIntoDungeon,
            Effect::VentureInto { .. } => EffectKind::VentureInto,
            Effect::TakeTheInitiative => EffectKind::TakeTheInitiative,
            Effect::GrantCastingPermission { .. } => EffectKind::GrantCastingPermission,
            Effect::ChooseFromZone { .. } => EffectKind::ChooseFromZone,
            Effect::ChooseAndSacrificeRest { .. } => EffectKind::ChooseAndSacrificeRest,
            Effect::Exploit { .. } => EffectKind::Exploit,
            Effect::GainEnergy { .. } => EffectKind::GainEnergy,
            Effect::GivePlayerCounter { .. } => EffectKind::GivePlayerCounter,
            Effect::LoseAllPlayerCounters { .. } => EffectKind::LoseAllPlayerCounters,
            Effect::ExileFromTopUntil { .. } => EffectKind::ExileFromTopUntil,
            Effect::RevealUntil { .. } => EffectKind::RevealUntil,
            Effect::Discover { .. } => EffectKind::Discover,
            Effect::Cascade => EffectKind::Cascade,
            Effect::MiracleCast { .. } => EffectKind::MiracleCast,
            Effect::MadnessCast { .. } => EffectKind::MadnessCast,
            Effect::PutAtLibraryPosition { .. } => EffectKind::PutAtLibraryPosition,
            Effect::PutOnTopOrBottom { .. } => EffectKind::PutOnTopOrBottom,
            Effect::GiftDelivery { .. } => EffectKind::GiftDelivery,
            Effect::Goad { .. } => EffectKind::Goad,
            Effect::Detain { .. } => EffectKind::Detain,
            Effect::ExchangeControl { .. } => EffectKind::ExchangeControl,
            Effect::ChangeTargets { .. } => EffectKind::ChangeTargets,
            Effect::Incubate { .. } => EffectKind::Incubate,
            Effect::Amass { .. } => EffectKind::Amass,
            Effect::Monstrosity { .. } => EffectKind::Monstrosity,
            Effect::Bolster { .. } => EffectKind::Bolster,
            Effect::Adapt { .. } => EffectKind::Adapt,
            Effect::Manifest { .. } => EffectKind::Manifest,
            Effect::ManifestDread => EffectKind::ManifestDread,
            Effect::ExtraTurn { .. } => EffectKind::ExtraTurn,
            Effect::SkipNextTurn { .. } => EffectKind::SkipNextTurn,
            Effect::AdditionalCombatPhase { .. } => EffectKind::AdditionalCombatPhase,
            Effect::Double { .. } => EffectKind::Double,
            Effect::RuntimeHandled { .. } => EffectKind::RuntimeHandled,
            Effect::Learn => EffectKind::Learn,
            Effect::Forage => EffectKind::Forage,
            Effect::CollectEvidence { .. } => EffectKind::CollectEvidence,
            Effect::Endure { .. } => EffectKind::Endure,
            Effect::BlightEffect { .. } => EffectKind::BlightEffect,
            Effect::Seek { .. } => EffectKind::Seek,
            Effect::SetLifeTotal { .. } => EffectKind::SetLifeTotal,
            Effect::SetDayNight { .. } => EffectKind::SetDayNight,
            Effect::GiveControl { .. } => EffectKind::GiveControl,
            Effect::RemoveFromCombat { .. } => EffectKind::RemoveFromCombat,
            Effect::Conjure { .. } => EffectKind::Conjure,
            Effect::ChooseOneOf { .. } => EffectKind::ChooseOneOf,
            Effect::Unimplemented { .. } => EffectKind::Unimplemented,
        }
    }
}

// ---------------------------------------------------------------------------
// Ability kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum AbilityKind {
    #[default]
    Spell,
    Activated,
    Database,
    /// Pre-game abilities: "If this card is in your opening hand, you may begin the game with..."
    /// Fired during game setup, not during normal stack resolution.
    BeginGame,
}

// ---------------------------------------------------------------------------
// Modal spell metadata
// ---------------------------------------------------------------------------

/// Metadata for modal spells ("Choose one —", "Choose two —", etc.).
///
/// Stored on the card data so the engine knows a spell is modal and how many
/// modes the player must choose. The `mode_count` field records the total
/// number of modes available; each mode corresponds to one `AbilityDefinition`
/// in the card's abilities array.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModalChoice {
    /// Minimum number of modes the player must choose.
    pub min_choices: usize,
    /// Maximum number of modes the player may choose.
    pub max_choices: usize,
    /// Total number of available modes.
    pub mode_count: usize,
    /// Short description of each mode (bullet text from Oracle).
    #[serde(default)]
    pub mode_descriptions: Vec<String>,
    /// Whether the same mode may be chosen multiple times.
    #[serde(default)]
    pub allow_repeat_modes: bool,
    /// Additional selection constraints parsed from modal reminder text.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub constraints: Vec<ModalSelectionConstraint>,
    /// Per-mode additional mana costs (Spree). Empty for standard modal spells.
    /// CR 702.172b: Chosen mode costs are additional costs, not part of the base mana cost.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mode_costs: Vec<ManaCost>,
    /// CR 702.42a: Entwine cost — when all modes are chosen, this additional cost is paid.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub entwine_cost: Option<ManaCost>,
}

/// Selection constraints attached to a modal choice header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ModalSelectionConstraint {
    DifferentTargetPlayers,
    /// CR 601.2b / CR 700.2a: A conditional casting-time modifier to the
    /// maximum number of modes the controller may choose.
    ConditionalMaxChoices {
        condition: ModalSelectionCondition,
        max_choices: usize,
        otherwise_max_choices: usize,
    },
    /// CR 700.2: Each mode may only be chosen once per turn for this source.
    /// Oracle text: "choose one that hasn't been chosen this turn"
    NoRepeatThisTurn,
    /// CR 700.2: Each mode may only be chosen once total for this source.
    /// Oracle text: "choose one that hasn't been chosen"
    NoRepeatThisGame,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ModalSelectionCondition {
    /// CR 903.3d: True when the spell's controller controls a commander permanent.
    ControlsCommander,
}

/// Structured activation-time restrictions parsed from Oracle text.
/// These describe when an activated ability may be activated; runtime
/// enforcement can be added independently of parsing/export support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ActivationRestriction {
    AsSorcery,
    AsInstant,
    DuringYourTurn,
    DuringYourUpkeep,
    DuringCombat,
    BeforeAttackersDeclared,
    BeforeCombatDamage,
    OnlyOnceEachTurn,
    OnlyOnce,
    MaxTimesEachTurn {
        count: u8,
    },
    RequiresCondition {
        condition: Option<ParsedCondition>,
    },
    /// CR 719.3c: This ability can only be activated while the source Case is solved.
    IsSolved,
    /// CR 716.4: Level N+1 ability can only activate when the source Class is at exactly this level.
    ClassLevelIs {
        level: u8,
    },
    /// CR 711.2a + CR 711.2b: Leveler counter range — ability can only be activated when
    /// the source has at least `minimum` level counters and at most `maximum` (if specified).
    LevelCounterRange {
        minimum: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u32>,
    },
    /// CR 721.2a: Counter-threshold gate — ability is present only while the source
    /// has `minimum` (and at most `maximum`, if specified) counters matching `counters`.
    /// Used for Spacecraft "N+ |" threshold lines (`counters = OfType(Generic("charge"))`)
    /// and any future pipe-delimited threshold layout that gates activated abilities.
    /// Generalization of `LevelCounterRange` across arbitrary counter types, mirroring
    /// `StaticCondition::HasCounters` and `TriggerCondition::HasCounters`.
    CounterThreshold {
        counters: CounterMatch,
        minimum: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u32>,
    },
    /// CR 702.62a: "If you could begin to cast this card by putting it onto the
    /// stack from your hand" — the activation is legal whenever the underlying
    /// card type's natural cast timing is legal. For a sorcery / permanent card,
    /// this is sorcery-speed; for an instant, it's instant-speed. Used by the
    /// synthesized Suspend hand-activated ability so future "cast-timing-mirroring"
    /// activations (Foretell, etc.) can reuse this primitive instead of
    /// special-casing card type at synthesis time.
    MatchesCardCastTiming,
}

/// Structured spell-casting restrictions parsed from Oracle text.
/// These describe when a spell may be cast. Runtime enforcement can
/// be added independently of parsing/export support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CastingRestriction {
    AsSorcery,
    DuringCombat,
    DuringOpponentsTurn,
    DuringYourTurn,
    DuringYourUpkeep,
    DuringOpponentsUpkeep,
    DuringAnyUpkeep,
    DuringYourEndStep,
    DuringOpponentsEndStep,
    DeclareAttackersStep,
    DeclareBlockersStep,
    BeforeAttackersDeclared,
    BeforeBlockersDeclared,
    BeforeCombatDamage,
    AfterCombat,
    RequiresCondition { condition: Option<ParsedCondition> },
}

/// CR 601.2f: Self-referential cost reduction on an activated ability.
/// "This ability costs {N} less to activate for each [condition]"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostReduction {
    /// Generic mana reduced per counted object (the {N} value).
    pub amount_per: u32,
    /// How many objects to count (e.g., legendary creatures you control).
    pub count: QuantityExpr,
}

// ---------------------------------------------------------------------------
// Definition types -- fully typed, zero HashMap
// ---------------------------------------------------------------------------

/// Parsed ability definition with typed effect. Zero remaining_params.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AbilityDefinition {
    pub kind: AbilityKind,
    pub effect: Box<Effect>,
    #[serde(default)]
    pub cost: Option<AbilityCost>,
    #[serde(default)]
    pub sub_ability: Option<Box<AbilityDefinition>>,
    /// CR 608.2c: Alternative branch executed when the condition on this ability is NOT met.
    /// Populated by "Otherwise, [effect]" Oracle text clauses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub else_ability: Option<Box<AbilityDefinition>>,
    #[serde(default)]
    pub duration: Option<Duration>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub target_prompt: Option<String>,
    #[serde(default)]
    pub sorcery_speed: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub activation_restrictions: Vec<ActivationRestriction>,
    /// CR 602.1: Zone from which this ability can be activated.
    /// `None` = battlefield (default). `Some(Zone::Hand)` for Channel, Cycling, etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_zone: Option<Zone>,
    /// Condition that must be met for this ability to execute during resolution.
    #[serde(default)]
    pub condition: Option<AbilityCondition>,
    /// When true, targeting is optional ("up to one"). Player may choose zero targets.
    #[serde(default)]
    pub optional_targeting: bool,
    /// CR 609.3: When true, the controller chooses whether to perform this effect ("You may X").
    #[serde(default)]
    pub optional: bool,
    /// CR 608.2d: When set, an opponent (not the controller) chooses whether to perform this
    /// optional effect. Requires `optional: true`. Opponents are prompted in APNAP order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_for: Option<OpponentMayScope>,
    /// Variable-count targeting: min/max targets the player can choose.
    /// When present, resolution enters MultiTargetSelection instead of immediate resolve.
    /// CR 601.2c + CR 115.1d.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_target: Option<MultiTargetSpec>,
    /// CR 601.2d: When set, the controller distributes this effect among chosen targets.
    /// Triggers WaitingFor::DistributeAmong during casting target selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribute: Option<DistributionUnit>,
    /// Modal metadata for activated/triggered abilities with "Choose one —" etc.
    /// When present, the ability pauses for mode selection before resolving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    /// The individual mode abilities for modal activated/triggered abilities.
    /// Each entry is one selectable mode. Only meaningful when `modal` is Some.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mode_abilities: Vec<AbilityDefinition>,
    /// CR 609.3: Repeat this ability N times, where N = resolve_quantity(repeat_for).
    /// Produced by "for each [X], [effect]" leading patterns.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_for: Option<QuantityExpr>,
    /// CR 601.2f: Self-referential cost reduction applied before activation.
    /// "This ability costs {N} less to activate for each [condition]"
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_reduction: Option<CostReduction>,
    /// When true, after this ability's effect resolves, moved/created objects are forwarded
    /// to the sub_ability: the moved object becomes sub's source_id, and the original source
    /// becomes a target. Used for "put onto the battlefield attached to [source]" patterns.
    #[serde(default)]
    pub forward_result: bool,
    /// Player scope for "each player/opponent [effect]" patterns.
    /// When set, the effect iterates over matching players (each becomes the acting player).
    /// Produced by "each opponent discards", "each player draws", etc.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_scope: Option<PlayerFilter>,
}

impl fmt::Debug for AbilityDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // JSON serialization instead of field-by-field Debug — avoids stack overflow
        // from Effect ↔ AbilityDefinition mutual recursion. Uses Serialize (not Debug)
        // internally, producing structured output optimized for LLM consumption.
        let json = if f.alternate() {
            serde_json::to_string_pretty(self)
        } else {
            serde_json::to_string(self)
        };
        match json {
            Ok(s) => f.write_str(&s),
            Err(_) => {
                let variant: &'static str = self.effect.as_ref().into();
                write!(f, "AbilityDefinition {{ effect: {variant}, .. }}")
            }
        }
    }
}

impl AbilityDefinition {
    /// Create a new `AbilityDefinition` with only the required fields; all optional
    /// fields default to `None` / `false`.
    pub fn new(kind: AbilityKind, effect: Effect) -> Self {
        Self {
            kind,
            effect: Box::new(effect),
            cost: None,
            sub_ability: None,
            else_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
            activation_restrictions: Vec::new(),
            activation_zone: None,
            condition: None,
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            distribute: None,
            modal: None,
            mode_abilities: Vec::new(),
            repeat_for: None,
            cost_reduction: None,
            forward_result: false,
            player_scope: None,
        }
    }

    pub fn player_scope(mut self, scope: PlayerFilter) -> Self {
        self.player_scope = Some(scope);
        self
    }

    pub fn multi_target(mut self, spec: MultiTargetSpec) -> Self {
        self.multi_target = Some(spec);
        self
    }

    pub fn distribute(mut self, unit: DistributionUnit) -> Self {
        self.distribute = Some(unit);
        self
    }

    pub fn cost(mut self, cost: AbilityCost) -> Self {
        self.cost = Some(cost);
        self
    }

    /// CR 118: Return the structural cost categories for this ability's cost,
    /// or an empty vec if the ability has no cost. Delegates to
    /// `AbilityCost::categories`.
    pub fn cost_categories(&self) -> Vec<CostCategory> {
        self.cost
            .as_ref()
            .map(AbilityCost::categories)
            .unwrap_or_default()
    }

    pub fn sub_ability(mut self, ability: AbilityDefinition) -> Self {
        self.sub_ability = Some(Box::new(ability));
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn description(mut self, desc: String) -> Self {
        self.description = Some(desc);
        self
    }

    pub fn target_prompt(mut self, prompt: String) -> Self {
        self.target_prompt = Some(prompt);
        self
    }

    /// CR 602.5d: "Activate only as a sorcery" — set both the display flag (for
    /// UI consumers) and push `ActivationRestriction::AsSorcery` so the legality
    /// gate in `game::restrictions::check_activation_restrictions` actually
    /// enforces the timing at runtime. Single authority for "this ability is
    /// sorcery speed" — covers Equip (CR 702.6a), Fortify (CR 702.67a),
    /// Reconfigure (CR 702.151a), Level Up (CR 702.87a), Scavenge (CR 702.97a),
    /// planeswalker loyalty (CR 606.3), and any future keyword that is required
    /// to be activated at sorcery speed.
    pub fn sorcery_speed(mut self) -> Self {
        self.sorcery_speed = true;
        if !self
            .activation_restrictions
            .contains(&ActivationRestriction::AsSorcery)
        {
            self.activation_restrictions
                .push(ActivationRestriction::AsSorcery);
        }
        self
    }

    pub fn activation_restrictions(mut self, restrictions: Vec<ActivationRestriction>) -> Self {
        self.activation_restrictions = restrictions;
        self
    }

    pub fn condition(mut self, condition: AbilityCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    pub fn optional_targeting(mut self) -> Self {
        self.optional_targeting = true;
        self
    }

    pub fn with_modal(
        mut self,
        modal: ModalChoice,
        mode_abilities: Vec<AbilityDefinition>,
    ) -> Self {
        self.modal = Some(modal);
        self.mode_abilities = mode_abilities;
        self
    }
}

/// Condition on an ability within a sub_ability chain.
/// Checked during resolve_ability_chain before executing the ability.
/// The condition is a pure predicate — it describes WHAT to check, not the outcome.
/// Casting-time facts needed for evaluation are stored in `SpellContext`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AbilityCondition {
    /// CR 702.33d + CR 702.33f + CR 608.2c: An optional additional cost was paid
    /// during casting. Parameterized for kicker variant gating:
    ///
    ///   - `variant: None`, `min_count: 1` (default) — "if it was kicked" / "if
    ///     the gift was promised" / "if its buyback cost was paid" / "if
    ///     evidence was collected" / "if it was bargained". Evaluates against
    ///     `SpellContext.additional_cost_paid` (the legacy single-bool flag,
    ///     used by all non-kicker optional-additional-cost mechanics).
    ///   - `variant: Some(KickerVariant)`, `min_count: 1` — "if it was kicked
    ///     with its [A]/[B] kicker" (CR 702.33f). Evaluates against
    ///     `SpellContext.kickers_paid` membership.
    ///   - `variant: None`, `min_count: N` (N >= 2) — "if it was kicked twice"
    ///     / "if it was kicked N times" (CR 702.33b/c). Evaluates against
    ///     `SpellContext.kickers_paid.len() >= N`.
    ///   - `kicker_cost: Some(_)` — parser-only cue for "with its {COST}
    ///     kicker" clauses. Database synthesis resolves this to `variant`
    ///     once the card's kicker declarations are visible.
    AdditionalCostPaid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<KickerVariant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kicker_cost: Option<ManaCost>,
        #[serde(
            default = "AbilityCondition::default_min_count",
            skip_serializing_if = "AbilityCondition::is_default_min_count"
        )]
        min_count: u32,
    },
    /// CR 608.2e: "Instead" clause — replaces the parent effect when the additional cost was paid.
    /// The resolver swaps the override sub's effect in place of the parent before resolution.
    AdditionalCostPaidInstead,
    /// CR 608.2c: "If you do" — sub_ability executes only if the parent optional effect was performed.
    IfYouDo,
    /// CR 603.12: "When you do" — reflexive trigger that always fires when the parent
    /// (non-optional) effect was performed. Unlike `IfYouDo` which gates on
    /// `optional_effect_performed`, this is unconditionally true for non-optional parents.
    WhenYouDo,
    /// CR 603.4: "If you cast it from [zone]" — sub_ability executes only if the spell
    /// was cast from the specified zone. Evaluated against SpellContext.cast_from_zone.
    CastFromZone { zone: Zone },
    /// CR 207.2c + CR 601.2: "if you cast this spell during your [phase/step]".
    /// `phases` is parameterized so grouped phrases like "main phase" can map to
    /// both concrete main phases without proliferating condition variants.
    CastDuringPhase { phases: Vec<Phase> },
    /// CR 601.2h + CR 608.2c: "if {C} was spent to cast this spell" gates
    /// resolution on the source object's recorded paid-mana colors.
    ManaColorSpent { color: ManaColor, minimum: u32 },
    /// CR 608.2c: "If it's a [type] card" — gates sub_ability on the last revealed card's type.
    /// Evaluated at resolution time by inspecting `state.last_revealed_ids[0]`.
    /// `additional_filter` holds optional extra filter properties (e.g., `IsChosenCreatureType`
    /// for "creature card of the chosen type"). For "if it's a nonland card" patterns,
    /// wrap with `AbilityCondition::Not`.
    RevealedHasCardType {
        card_type: CoreType,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        additional_filter: Option<FilterProp>,
    },
    /// CR 400.7 + CR 608.2c: True when the source permanent entered the battlefield
    /// this turn. For the "did not enter this turn" sense (e.g., Moon-Circuit Hacker
    /// "unless ~ entered this turn"), wrap with `AbilityCondition::Not`.
    SourceEnteredThisTurn,
    /// CR 702.49 + CR 603.4: True when the source permanent entered via a ninjutsu-family
    /// activation of the specified variant this turn.
    CastVariantPaid { variant: CastVariantPaid },
    /// CR 608.2e + CR 702.49 + CR 702.190a: "Instead" override gated on the source
    /// permanent having entered via a specified cast/activation variant this turn.
    /// Unlike AdditionalCostPaidInstead (which reads SpellContext.additional_cost_paid),
    /// this reads GameObject.cast_variant_paid from the game state.
    CastVariantPaidInstead { variant: CastVariantPaid },
    /// CR 608.2d: "If a player does" / "if they do" — gates sub_ability on whether
    /// any prompted opponent accepted an "any opponent may" optional effect.
    IfAPlayerDoes,
    /// CR 608.2c: General-purpose quantity comparison condition on effects.
    /// "if its power is N or greater" / "if its toughness is less than N" etc.
    /// Composes existing `QuantityExpr` and `Comparator` building blocks.
    QuantityCheck {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
    },
    /// CR 702.178a: The ability functions only while its controller has max speed.
    HasMaxSpeed,
    /// CR 725.1: "if you're the monarch" is true when the ability controller has the monarch designation.
    IsMonarch,
    /// CR 702.131c: "if you have the city's blessing" is true when the ability
    /// controller has the city's blessing designation.
    HasCityBlessing,
    /// CR 608.2e: "If [target] has [keyword], [override effect] instead"
    /// Checked at resolution time against the first resolved object target's keywords.
    /// Uses "Instead" override semantics: swaps the parent effect when condition is met.
    TargetHasKeywordInstead { keyword: Keyword },
    /// CR 400.7 + CR 608.2c: "If that creature was a [type]" — gates the sub_ability on
    /// whether the target (or its last-known information if `use_lki` is true) matches the filter.
    /// Present-tense ("is a") checks current state; past-tense ("was a") checks LKI per CR 400.7.
    /// For the "does NOT match" sense, wrap with `AbilityCondition::Not`.
    TargetMatchesFilter {
        filter: TargetFilter,
        #[serde(default)]
        use_lki: bool,
    },
    /// CR 608.2c: "If this creature/permanent is a [type]" — gates sub_ability on whether
    /// the ability's source object matches the filter. Used by leveler-style cards
    /// (e.g. Figure of Fable) where each activated ability gates on the source's current type.
    SourceMatchesFilter { filter: TargetFilter },
    /// CR 603.4 + CR 603.6 + CR 603.10: In a trigger-body condition, match the
    /// object from the current zone-change trigger event against a filter. ETB
    /// conditions check the live object in its destination zone; death/LTB
    /// conditions check the zone-change snapshot.
    ZoneChangeObjectMatchesFilter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Zone>,
        destination: Zone,
        filter: TargetFilter,
    },
    /// CR 608.2c + CR 614.1d: "if you control a [filter]" — gates sub_ability on whether
    /// the ability controller controls at least one battlefield permanent matching the
    /// filter (excluding the source itself). For the "controls NO matching permanent"
    /// sense (reveal-tribal land cycles like Fortified Beachhead / Temple of the Dragon
    /// Queen on_decline), wrap with `AbilityCondition::Not`.
    /// `filter` MUST have its `ControllerRef::You` pre-bound by the parser.
    ControllerControlsMatching { filter: TargetFilter },
    /// CR 608.2c: "If it's your turn" — gates sub_ability on whether the active player
    /// is the ability's controller. For "if it's not your turn", wrap with
    /// `AbilityCondition::Not`.
    IsYourTurn,
    /// CR 500.8 + CR 506.1 + CR 608.2c: "if it's the first combat phase of the turn".
    /// Gates a follow-up effect on whether this is the first combat phase started this turn.
    FirstCombatPhaseOfTurn,
    /// CR 608.2c: "If a [noun] was [verb]ed this way" — sub_ability executes only if
    /// the parent effect produced a zone change involving an object matching the filter.
    /// Evaluated by checking `state.last_zone_changed_ids` against the filter.
    /// Handles both optional-targeting parents (empty targets → empty IDs → false)
    /// and mandatory parents (type filter check on moved objects).
    ZoneChangedThisWay { filter: TargetFilter },
    /// CR 611.2b: "if this [permanent] is tapped" — checks the source's tapped status.
    /// For the untapped sense, wrap with `AbilityCondition::Not`.
    SourceIsTapped,
    /// CR 608.2c: General "instead" replacement — wraps any `AbilityCondition` with
    /// replacement semantics. When the inner condition is met at resolution, the sub's
    /// effect chain replaces the parent's entire effect chain. When not met, the base
    /// continuation chain (stored in `else_ability`) runs after the parent's own effect.
    ///
    /// Used for cross-line patterns like Delirium ("If [condition], instead [effect]")
    /// where the conditional replacement and the base effect are on separate Oracle lines.
    ConditionInstead { inner: Box<AbilityCondition> },
    /// CR 608.2c: Compound condition — all inner conditions must be true.
    /// Mirrors `TriggerCondition::And` for ability-level conditions.
    /// Used when multiple independent checks gate the same resolution
    /// (e.g., Revolt + mana value threshold on Fatal Push).
    And { conditions: Vec<AbilityCondition> },
    /// CR 608.2c: Compound condition — at least one inner condition must be true.
    /// Mirrors `TriggerCondition::Or` / `StaticCondition::Or` for ability-level
    /// conditions. Used when an intervening-if or sub-ability gate is satisfied
    /// by any of several independent checks.
    Or { conditions: Vec<AbilityCondition> },
    /// CR 608.2c: Logical negation — sub_ability executes when `condition` is false.
    /// Mirrors `TriggerCondition::Not` for ability-level conditions. Replaces the
    /// per-leaf `negated: bool` fields that existed on `RevealedHasCardType`,
    /// `TargetMatchesFilter`, `ControllerControlsMatching`, `IsYourTurn`, and
    /// `SourceIsTapped`, plus the dedicated negation variants
    /// `AdditionalCostNotPaid` and `SourceDidNotEnterThisTurn`. Used by
    /// "if you don't" / "unless you" / "if it isn't" sub-clause gates.
    Not { condition: Box<AbilityCondition> },
    /// CR 730.2a: True when it's neither day nor night (day_night is None).
    /// Used by Daybound/Nightbound ETB initialization: "If it's neither day nor night,
    /// it becomes day as this creature enters."
    DayNightIsNeither,
    /// CR 731.1: True when the game has the given day/night designation.
    DayNightIs {
        state: crate::types::game_state::DayNight,
    },
    /// CR 603.4: Intervening-if gate for "if this is the [Nth] time this ability has
    /// resolved this turn". Counter is keyed by `(source_id, ability_index)` and
    /// incremented at the top of `resolve_ability_chain` (depth 0). The condition is
    /// satisfied when, after the increment, the per-turn resolution count equals `n`.
    /// Cleared at end-of-turn cleanup alongside other per-turn counters.
    /// Used by Omnath, Locus of Creation and the broader nth-resolution class
    /// (Ashling the Pilgrim, Nissa Resurgent Animist, Teething Wurmlet, etc.).
    NthResolutionThisTurn { n: u32 },
}

impl AbilityCondition {
    /// Default `min_count` for `AdditionalCostPaid` is 1 (any single payment).
    /// Used by serde `#[serde(default = ...)]`.
    pub(crate) fn default_min_count() -> u32 {
        1
    }

    /// Skip-serialization predicate: omit `min_count` from JSON when it equals
    /// the default value (1). Keeps card-data.json compact for the common case.
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn is_default_min_count(value: &u32) -> bool {
        *value == 1
    }

    /// Construct the default-shape `AdditionalCostPaid` condition: any single
    /// optional-additional-cost payment was made. Equivalent to the legacy
    /// nullary `AdditionalCostPaid` variant; preserves call sites in
    /// `parser/oracle_effect/conditions.rs` (Gift, Buyback, Bargain, plain
    /// "if it was kicked"), `database/synthesis.rs` (Bargain), and
    /// `game/effects/change_zone.rs` (Collect Evidence).
    pub fn additional_cost_paid_any() -> Self {
        AbilityCondition::AdditionalCostPaid {
            variant: None,
            kicker_cost: None,
            min_count: 1,
        }
    }

    /// CR 702.33f: "if it was kicked with its [A/B] kicker" — gates on a
    /// specific kicker variant being paid.
    pub fn additional_cost_paid_kicker(variant: KickerVariant) -> Self {
        AbilityCondition::AdditionalCostPaid {
            variant: Some(variant),
            kicker_cost: None,
            min_count: 1,
        }
    }

    /// Parser-side representation for "if it was kicked with its {COST}
    /// kicker". Database synthesis maps the printed cost to its positional
    /// `KickerVariant` using the card's `AdditionalCost::Kicker` declaration.
    pub fn additional_cost_paid_kicker_cost(cost: ManaCost) -> Self {
        AbilityCondition::AdditionalCostPaid {
            variant: None,
            kicker_cost: Some(cost),
            min_count: 1,
        }
    }

    /// CR 702.33b/c: "if it was kicked N times" — gates on the total kicker
    /// payment count meeting a minimum.
    pub fn additional_cost_paid_n_times(min_count: u32) -> Self {
        AbilityCondition::AdditionalCostPaid {
            variant: None,
            kicker_cost: None,
            min_count,
        }
    }
}

/// CR 702.33f: Discriminator for which kicker cost was paid on a spell that has
/// more than one kicker cost ("Kicker {A} and/or {B}"). Per CR 702.33f, the
/// abilities that gate on a specific kicker reference the kickers by *position*
/// on the card ("its [A] kicker" / "its [B] kicker"), so the discriminator is
/// the position, not the cost text.
///
/// For single-kicker spells (CR 702.33a) and multikicker (CR 702.33c), only
/// `First` is meaningful — there is no second kicker cost. Multikicker count is
/// expressed via `Vec<KickerVariant>` length on `SpellContext.kickers_paid`
/// (each repeated payment pushes another `First` entry).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum KickerVariant {
    /// CR 702.33f: First kicker cost as listed on the card.
    First,
    /// CR 702.33f: Second kicker cost as listed on the card. Only present when
    /// the card has "Kicker {A} and/or {B}" (CR 702.33b).
    Second,
}

/// Casting-time facts that flow with a spell from casting through resolution.
/// Conditions in the sub_ability chain are evaluated against this context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SpellContext {
    /// Whether the spell's optional additional cost was paid during casting.
    #[serde(default)]
    pub additional_cost_paid: bool,
    /// CR 702.33d + CR 702.33f: The list of kicker payments declared during
    /// casting, in payment order. For "Kicker {A} and/or {B}" cards (CR 702.33b),
    /// each chosen kicker pushes a corresponding `KickerVariant` entry. For
    /// multikicker (CR 702.33c), each repeated payment pushes another `First`.
    /// Single-kicker spells push at most one `First` entry. Empty when no
    /// kicker was paid.
    ///
    /// Linked to (and a strict superset of the information in)
    /// `additional_cost_paid` for kicker-bearing spells:
    ///   - empty                       ⇔ kicker not paid
    ///   - non-empty                   ⇔ kicker paid
    ///   - contains `KickerVariant::X` ⇔ that specific kicker variant was paid
    ///   - `len() >= n`                ⇔ kicker was paid at least N times
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kickers_paid: Vec<KickerVariant>,
    /// Whether an optional "you may" effect was performed during resolution.
    /// Used by AbilityCondition::IfYouDo to gate dependent sub_abilities.
    #[serde(default)]
    pub optional_effect_performed: bool,
    /// CR 608.2d: The player who accepted an "any opponent may" optional effect.
    /// Used to resolve "that player" / "them" backreferences and target scoping.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub accepting_player: Option<PlayerId>,
    /// CR 603.4: The zone the spell was cast from. Propagated from casting through
    /// to ETB triggers so conditions like "if you cast it from your hand" can evaluate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_from_zone: Option<Zone>,
    /// CR 601.2: Phase at the time the spell was cast. Used by addendum-style
    /// conditions such as "if you cast this spell during your main phase".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_phase: Option<Phase>,
}

/// Intervening-if condition for triggered abilities.
/// Checked both when the trigger would fire and when it resolves on the stack.
///
/// Predicates are leaf conditions ("you gained life", "you descended").
/// `And`/`Or` compose multiple predicates for compound conditions
/// ("if you gained and lost life this turn").
///
/// Adding a new condition:
/// 1. Add a variant here with the predicate's natural subject baked in
/// 2. Add a match arm in `check_trigger_condition` (game/triggers.rs)
/// 3. Add parser support in `extract_if_condition` (parser/oracle_trigger.rs)
/// 4. Add any per-turn tracking fields to `Player` / `GameState` if needed
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TriggerCondition {
    // -- Predicates (leaf conditions) --
    /// "if you gained life this turn" / "if you've gained N or more life this turn"
    GainedLife { minimum: u32 },
    /// "if you lost life this turn"
    LostLife,
    /// "if you descended this turn" (a permanent card was put into your graveyard)
    Descended,
    /// Deprecated: Use `ControlCount { minimum, filter }` instead.
    /// Kept for backward compatibility with serialized card data.
    ControlCreatures { minimum: u32 },
    /// "if you control a [type]" — general control presence check.
    ControlsType { filter: TargetFilter },
    /// CR 603.4: "if no spells were cast last turn" — werewolf transform condition.
    NoSpellsCastLastTurn,
    /// CR 603.4: "if two or more spells were cast last turn" — werewolf reverse transform.
    TwoOrMoreSpellsCastLastTurn,
    /// CR 603.4: "if it's your turn" — intervening-if requiring the controller's turn.
    DuringYourTurn,
    /// CR 508.1a: "Whenever ~ and at least N other creatures attack."
    /// True when combat is active and at least `minimum` other creatures
    /// controlled by the same player are also attacking.
    MinCoAttackers { minimum: u32 },
    /// CR 719.2: Intervening-if for Case auto-solve.
    /// True when the source Case is unsolved AND its solve condition is met.
    SolveConditionMet,
    /// CR 716.2a: True when the source Class enchantment is at or above the given level.
    /// Used to gate continuous triggers that only become active at higher class levels.
    ClassLevelGE { level: u8 },

    /// "if you cast it" — zoneless cast check (unlike CastFromZone which requires a specific zone).
    /// CR 701.57a: Used by Discover ETB triggers.
    /// Negation ("if it wasn't cast") is expressed via `Not { Box::new(WasCast) }`.
    WasCast,
    /// CR 603.4 + CR 702.33d-f: Intervening-if for "if it was kicked" /
    /// "if it was kicked with its [A] kicker" / "if it was kicked twice".
    /// Evaluates the triggering zone-change object when present, otherwise the
    /// trigger source, using `GameObject::kickers_paid` recorded at cast time.
    AdditionalCostPaid {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<KickerVariant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kicker_cost: Option<ManaCost>,
        #[serde(
            default = "AbilityCondition::default_min_count",
            skip_serializing_if = "AbilityCondition::is_default_min_count"
        )]
        min_count: u32,
    },

    /// "if it's attacking" — true when the trigger source object is currently an attacker.
    /// CR 508.1: Used by ninjutsu ETB triggers (e.g., Thousand-Faced Shadow).
    SourceIsAttacking,

    /// CR 702.49 + CR 702.190a + CR 603.4 + CR 702.138b: "if its sneak/ninjutsu
    /// cost was paid this turn". True when the source permanent entered via the
    /// specified cast/activation variant this turn. Negation ("unless it escaped")
    /// is expressed via `Not { Box::new(CastVariantPaid { variant }) }`.
    CastVariantPaid { variant: CastVariantPaid },

    /// CR 601.2: "during each opponent's turn" — the trigger only fires when it is
    /// currently an opponent's turn. Used in conjunction with NthSpellThisTurn constraint.
    DuringOpponentsTurn,

    /// CR 700.4 + CR 120.1: "a creature dealt damage by ~ this turn dies" — death trigger
    /// gated on the dying creature having been dealt damage by the trigger source this turn.
    DealtDamageBySourceThisTurn,

    /// CR 400.7 + CR 603.10: "if it was a [type]" — true when the trigger source's
    /// last known information includes the specified core type. Used by the Glimmer cycle
    /// ("when this dies, if it was a creature, return it").
    WasType { card_type: CoreType },

    /// CR 603.4: "if you have N or more life" — intervening-if condition checking life total.
    LifeTotalGE { minimum: i32 },

    /// CR 603.4: "if you control N or more [type]" — generalized control count condition.
    /// Subsumes ControlCreatures for any permanent type (artifacts, enchantments, lands, etc.).
    ControlCount { minimum: u32, filter: TargetFilter },

    /// CR 603.8: "when you control no [type]" — state trigger condition.
    /// True when the controller controls no permanents matching the filter.
    ControlsNone { filter: TargetFilter },

    /// CR 603.4: "if you attacked this turn" — true when the controller declared attackers
    /// during this turn's combat phase.
    AttackedThisTurn,
    /// CR 500.8 + CR 506.1 + CR 603.4: "if it's the first combat phase of the turn".
    /// True during the first combat phase started this turn, including its steps.
    FirstCombatPhaseOfTurn,

    /// CR 603.4: "if you cast a [type] spell this turn" — true when the controller cast
    /// a spell matching the optional filter this turn.
    CastSpellThisTurn { filter: Option<TargetFilter> },

    /// Quantity comparison for trigger-side intervening-if checks.
    QuantityComparison {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
    },
    /// CR 702.178a: The trigger functions only while its controller has max speed.
    HasMaxSpeed,

    /// CR 725.1: "if you're the monarch" is true when the controller is the monarch.
    IsMonarch,
    /// CR 702.131a: "if you have the city's blessing" — true when the controller has Ascend.
    HasCityBlessing,
    /// CR 309.7: True when the controller has completed a dungeon.
    /// `specific: None` matches "have you completed any dungeon"; `specific: Some(d)`
    /// matches "have you completed `d`". Negation ("haven't completed Tomb of
    /// Annihilation") is expressed via `Not { Box::new(CompletedDungeon { specific }) }`.
    CompletedDungeon {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        specific: Option<crate::game::dungeon::DungeonId>,
    },
    /// CR 611.2b: "if this [permanent] is tapped" — checks the source's tapped status.
    /// Negation ("untapped") is expressed via `Not { Box::new(SourceIsTapped) }`.
    SourceIsTapped,
    /// CR 701.27g: "if this [permanent] is transformed" — checks the source's transformed status.
    /// A "transformed permanent" is a double-faced permanent on the battlefield with its
    /// back face up. Negation ("not transformed") is expressed via
    /// `Not { Box::new(SourceIsTransformed) }`.
    SourceIsTransformed,
    /// CR 708.2: "if this [permanent] is face-up" — checks the source's face-up status.
    /// Negation ("face-down") is expressed via `Not { Box::new(SourceIsFaceUp) }`.
    SourceIsFaceUp,
    /// CR 708.2: "if this [permanent] is face-down" — checks the source's face-down status.
    /// Negation ("face-up") is expressed via `Not { Box::new(SourceIsFaceDown) }`.
    SourceIsFaceDown,
    /// CR 113.6b: "if this card is in [zone]" — true when the trigger source is in the given zone.
    SourceInZone { zone: crate::types::zones::Zone },
    /// CR 122.1: "if you put a counter on a permanent this turn" — true when the controller
    /// added any counter to any permanent this turn.
    CounterAddedThisTurn,

    /// CR 603.4: "if an opponent lost life this turn" / "if that player lost life this turn"
    /// — checks whether a specific player reference lost life (this turn or last turn).
    LostLifeLastTurn,

    /// CR 509.1a + CR 603.4: "if defending player controls no [type]" — true when the
    /// defending player in the current combat controls no permanents matching the filter.
    DefendingPlayerControlsNone { filter: TargetFilter },

    /// CR 702.104a: "if tribute wasn't paid" — Tribute mechanic intervening-if.
    TributeNotPaid,
    /// CR 207.2c + CR 601.2: "if you cast this spell during your [phase/step]".
    /// `phases` is parameterized so grouped phrases like "main phase" can map to
    /// both concrete main phases without proliferating condition variants.
    CastDuringPhase { phases: Vec<Phase> },
    /// CR 207.2c: "if at least N mana of [color] was spent to cast this spell" — Adamant.
    ManaColorSpent { color: ManaColor, minimum: u32 },
    /// CR 601.2b: "if no mana was spent to cast it" / "if mana from a [source] was spent"
    ManaSpentCondition { text: String },
    /// CR 400.7: "if it had a +1/+1 counter on it" / "if it had counters on it"
    HadCounters { counter_type: Option<String> },
    /// CR 903.3: "if you control your commander" — Lieutenant mechanic.
    /// True when the controller controls at least one of their commander(s) on the battlefield.
    ControlsCommander,
    /// CR 702.112a: "if ~ is renowned" — true when the source has been made renowned.
    SourceIsRenowned,
    /// CR 711.2a + CR 711.2b: Level-up creature trigger gating — true when the source has at least
    /// `minimum` counters (and at most `maximum` if specified) matching `counters`.
    /// `CounterMatch::Any` sums across every counter type on the source; `OfType(ct)`
    /// restricts to a single type. Mirrors `StaticCondition::HasCounters`.
    HasCounters {
        counters: CounterMatch,
        minimum: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u32>,
    },

    /// CR 603.4 + CR 603.6 + CR 603.10: Intervening-if condition whose subject
    /// is the object from the triggering zone-change event rather than the
    /// permanent that owns the ability. ETB conditions check the live object in
    /// its destination zone; death/LTB conditions check the zone-change snapshot.
    ZoneChangeObjectMatchesFilter {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        origin: Option<Zone>,
        destination: Zone,
        filter: TargetFilter,
    },
    /// CR 603.4 + CR 611.2b: Source-bound intervening-if predicate expressed
    /// as a normal target filter evaluated against the trigger source.
    SourceMatchesFilter { filter: TargetFilter },

    /// CR 508.1 + CR 603.2c + CR 603.4: Intervening-if for "attacks with N or more creatures"
    /// triggers. Reads the triggering `AttackersDeclared` event and counts attackers whose
    /// controller matches `scope` relative to the trigger's controller:
    ///   - `ControllerRef::You` → attackers controlled by the trigger controller.
    ///   - `ControllerRef::Opponent` → attackers controlled by a player other than the
    ///     trigger controller. "Another player attacks with 2+" uses this scope; all
    ///     attackers in the batch share one attacking player (the active player per
    ///     CR 506.2), so counting "≠ trigger controller" is equivalent to counting the
    ///     triggering player's creatures.
    ///
    /// True when the count meets or exceeds `minimum`.
    AttackersDeclaredMin { scope: ControllerRef, minimum: u32 },
    /// CR 506.2 + CR 603.4: Intervening-if "if none of those creatures attacked you".
    /// Reads the triggering `AttackersDeclared` event's per-attacker `AttackTarget` tuples
    /// (CR 508.1b) and returns true iff no attacker in the batch targeted the trigger's
    /// controller directly (`AttackTarget::Player(trigger_controller)`).
    NoneOfAttackersTargetedYou,

    /// CR 121.1 + CR 504.1 + CR 603.4: "except the first one [you|they] draw in
    /// each of [your|their] draw steps" — the trigger fires for every card-draw
    /// EXCEPT the draw step's mandatory first draw. Used by Orcish Bowmasters
    /// ("Whenever an opponent draws a card except the first one they draw in
    /// each of their draw steps, ~ deals 1 damage to any target."). Reads the
    /// `nth_in_step` ordinal embedded in the `GameEvent::CardDrawn` event:
    /// returns `false` when the drawing player is the active player, the
    /// current phase is the draw step, AND `nth_in_step == 1`; otherwise `true`.
    ExceptFirstDrawInDrawStep,

    // -- Combinators --
    /// All conditions must be true ("if you gained and lost life this turn")
    And { conditions: Vec<TriggerCondition> },
    /// Any condition must be true
    Or { conditions: Vec<TriggerCondition> },
    /// CR 603.4 + CR 608.2c: Logical negation — the wrapped condition must
    /// evaluate to false. Used for "unless [phrase]" intervening-if patterns
    /// ("you lose 4 life unless you attacked this turn"
    /// → `Not { Box::new(AttackedThisTurn) }`) and predicate-side negation
    /// ("when ~ enters, if it isn't a [type]"). Mirrors the sibling wrapper
    /// variants `TargetFilter::Not`, `StaticCondition::Not`, and
    /// `AbilityCondition::Not` so trigger-side negation composes uniformly
    /// with `And`/`Or`. Replaces per-leaf `negated: bool` fields and the
    /// `NotYourTurn` / `WasNotCast` / `NotCompletedDungeon` sibling-pair variants.
    Not { condition: Box<TriggerCondition> },
}

/// Condition that gates whether a replacement effect applies.
/// Checked when determining if the replacement is a candidate for an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReplacementCondition {
    /// "unless you control a [subtype] or a [subtype]"
    /// Replacement is suppressed if the controller controls any permanent with a listed subtype.
    /// Used for check lands (Clifftop Retreat, Drowned Catacomb, etc.).
    UnlessControlsSubtype { subtypes: Vec<String> },
    /// "unless you control N or fewer other [type]"
    /// CR 614.1c — condition checked when determining replacement applicability.
    /// Replacement is suppressed if the controller controls N or fewer other permanents
    /// matching the filter (excluding the entering permanent itself).
    /// The filter MUST have `ControllerRef::You` and `FilterProp::Another` pre-set by the parser.
    /// Used for fast lands (Spirebluff Canal, Blackcleave Cliffs, etc.).
    UnlessControlsOtherLeq { count: u32, filter: TypedFilter },
    /// "unless you control a [type phrase]"
    /// CR 614.1d — General-purpose ETB replacement condition using existing TargetFilter evaluation.
    /// The filter MUST have `ControllerRef::You` pre-set by the parser.
    /// Covers: basic lands, legendary creatures, Mount/Vehicle, etc.
    UnlessControlsMatching { filter: TargetFilter },
    /// "unless you control N or more [type phrase]"
    /// CR 614.1d — Quantity-gated ETB replacement condition.
    /// The filter MUST have `ControllerRef::You` pre-set by the parser.
    /// Used for temple lands ("two or more other lands") and similar patterns.
    UnlessControlsCountMatching { minimum: u32, filter: TargetFilter },
    /// "unless a player has N or less life"
    /// CR 614.1d — Bond lands (Abandoned Campground, etc.)
    UnlessPlayerLifeAtMost { amount: u32 },
    /// "unless you have two or more opponents"
    /// CR 614.1d — Battlebond lands (Luxury Suite, etc.)
    UnlessMultipleOpponents,
    /// "unless it's your turn"
    /// CR 614.1d + CR 500 — Replacement suppressed when active player is the controller.
    UnlessYourTurn,
    /// General quantity-comparison condition for replacement effects.
    /// "unless <quantity condition>" — suppressed when the comparison is true.
    /// Reuses QuantityExpr + Comparator building blocks.
    /// `active_player_req` optionally gates the condition on whose turn it is:
    ///   - `None` → pure quantity check, no turn requirement
    ///   - `Some(You)` → must be controller's turn ("it's your Nth turn")
    ///   - `Some(Opponent)` → must be opponent's turn
    UnlessQuantity {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_player_req: Option<ControllerRef>,
    },
    /// General quantity-comparison gate for replacement effects.
    /// "as long as <quantity condition>" — replacement applies only while the comparison is true.
    /// Reuses QuantityExpr + Comparator building blocks.
    /// `active_player_req` optionally gates the condition on whose turn it is:
    ///   - `None` → pure quantity check, no turn requirement
    ///   - `Some(You)` → must be controller's turn
    ///   - `Some(Opponent)` → must be opponent's turn
    OnlyIfQuantity {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        active_player_req: Option<ControllerRef>,
    },
    /// CR 702.138c: "escapes with" — replacement applies only when the creature
    /// entered the battlefield via escape.
    CastViaEscape,
    /// CR 603.4: "if you cast it from [zone]" — replacement applies only when
    /// the source object was cast from the specified zone (e.g., Myojin's
    /// "enters with an indestructible counter on it if you cast it from your
    /// hand"). Evaluated against `GameObject.cast_from_zone`.
    CastFromZone { zone: Zone },
    /// CR 207.2c (Raid ability word) + CR 614.1c: "if you attacked this turn"
    /// — replacement applies only when the controller attacked with a
    /// creature earlier this turn. Evaluated against
    /// `state.creatures_attacked_this_turn` for the controller.
    YouAttackedThisTurn,
    /// CR 702.33d + CR 702.33f: "if was kicked" — replacement applies only when
    /// the source permanent's spell was cast with at least one kicker cost paid.
    /// Optional `variant` narrows to a specific kicker position (CR 702.33f:
    /// "with its [A]/[B] kicker"). Evaluated against
    /// `GameObject.kickers_paid` (populated at cast resolution from
    /// `SpellContext.kickers_paid`).
    CastViaKicker {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        variant: Option<KickerVariant>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kicker_cost: Option<ManaCost>,
    },
    /// "as long as ~ is tapped/untapped" — replacement applies only while the
    /// source object is in the required tapped state.
    SourceTappedState { tapped: bool },
    /// CR 120.1: "dealt damage this turn by a source you controlled" — replacement applies
    /// only to objects that were dealt damage this turn by a source controlled by the specified
    /// player. Checks `damage_dealt_this_turn` records in game state.
    DealtDamageThisTurnBySourceControlledBy { controller: ControllerRef },
    /// CR 500.7 + CR 614.10: Replacement applies only when the triggering
    /// event is an *extra* turn (granted by an effect, not a natural turn).
    /// Used by Stranglehold ("If a player would begin an extra turn...").
    /// Evaluated by `begin_turn_matcher` against `ProposedEvent::BeginTurn`.
    OnlyExtraTurn,
    /// CR 614.1a + CR 111.1: Gate a `CreateToken` replacement on whether the
    /// proposed event creates a token whose subtypes overlap a fixed set.
    /// Used by Xorn ("if you would create one or more Treasure tokens, …") and
    /// Academy Manufactor ("if you would create a Clue, Food, or Treasure
    /// token, …"). Subtype strings are matched case-insensitively against the
    /// proposed `TokenSpec.subtypes`. Substantive subtype canonicalization
    /// remains the parser's job.
    ///
    /// `subtypes` is `Vec<String>` to mirror the existing `TokenSpec.subtypes`
    /// shape; introducing a typed `Subtype` enum is a separate, broader refactor.
    TokenSubtypeMatches { subtypes: Vec<String> },
    /// CR 121.1 + CR 504.1 + CR 614.6: "except the first one you draw in each
    /// of your draw steps" — the replacement applies to every card-draw EXCEPT
    /// the draw step's mandatory first draw (the active player's CR 504.1
    /// turn-based action). Used by Alhammarret's Archive
    /// ("If you would draw a card except the first one you draw in each of
    /// your draw steps, draw two cards instead.").
    /// Evaluated against `ProposedEvent::Draw`: returns `false` (suppress) when
    /// the drawing player is the active player, the current phase is the draw
    /// step, AND the player has not yet drawn a card during this step
    /// (`cards_drawn_this_step == 0`); otherwise `true` (apply replacement).
    ExceptFirstDrawInDrawStep,
    /// "unless you revealed a [type] card" / "unless you paid {mana}"
    /// CR 614.1d — Generic condition text that the engine does not yet decompose further.
    /// Using this variant lets the replacement be recognized for coverage while deferring
    /// the condition evaluation.
    Unrecognized { text: String },
}

/// Rate-limiting constraint for triggered abilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TriggerConstraint {
    /// "This ability triggers only once each turn."
    OncePerTurn,
    /// "This ability triggers only once."
    OncePerGame,
    /// "This ability triggers only during your turn."
    OnlyDuringYourTurn,
    /// "Whenever you/an opponent casts your/their Nth [qualifier] spell each turn" —
    /// fires exactly when the caster's per-player spell count equals `n`.
    /// When `filter` is `Some`, only spells matching the filter are counted
    /// (e.g., `TypeFilter::Non(Creature)` for "noncreature spell").
    NthSpellThisTurn {
        n: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
    /// "Whenever you draw your Nth card each turn" — fires exactly when
    /// the controller's `cards_drawn_this_turn` equals `n`.
    NthDrawThisTurn { n: u32 },
    /// "At the beginning of each opponent's [phase]"
    OnlyDuringOpponentsTurn,
    /// CR 505.1: Trigger fires only during the controller's main phase
    /// (precombat or postcombat). Used by cards that print "during your
    /// main phase" as a trigger timing restriction.
    OnlyDuringYourMainPhase,
    /// CR 716.2a: "When this Class becomes level N" — fire only at the specified level.
    AtClassLevel { level: u8 },
    /// CR 603.4: "This ability triggers only the first N times each turn." — generalizes
    /// OncePerTurn to arbitrary limits. OncePerTurn remains for backward compatibility.
    MaxTimesPerTurn { max: u32 },
}

/// Filter for counter-related trigger modes (CounterAdded, CounterRemoved).
/// When set, the trigger only matches events for the specified counter type,
/// optionally requiring that the count crosses a threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterTriggerFilter {
    /// Only match events for this counter type.
    pub counter_type: crate::types::counter::CounterType,
    /// If set, only fire when the count crosses this threshold:
    /// previous_count < threshold <= new_count.
    /// Used by Saga chapter triggers (CR 714.2a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<u32>,
}

/// Trigger definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerDefinition {
    pub mode: TriggerMode,
    #[serde(default)]
    pub execute: Option<Box<AbilityDefinition>>,
    #[serde(default)]
    pub valid_card: Option<TargetFilter>,
    #[serde(default)]
    pub origin: Option<Zone>,
    /// CR 603.10a: Disjunctive source-zone filter for batched zone-change triggers
    /// like "one or more cards are put into exile from your library and/or your graveyard".
    /// When non-empty, the matcher requires `from_zone` to be in this set
    /// (and `origin` is ignored). Leave empty for single-zone triggers that use `origin`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub origin_zones: Vec<Zone>,
    #[serde(default)]
    pub destination: Option<Zone>,
    #[serde(default)]
    pub trigger_zones: Vec<Zone>,
    #[serde(default)]
    pub phase: Option<Phase>,
    #[serde(default)]
    pub optional: bool,
    /// CR 120.3: Filter for combat vs noncombat damage on damage triggers.
    #[serde(default)]
    pub damage_kind: DamageKindFilter,
    #[serde(default)]
    pub secondary: bool,
    #[serde(default)]
    pub valid_target: Option<TargetFilter>,
    #[serde(default)]
    pub valid_source: Option<TargetFilter>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub constraint: Option<TriggerConstraint>,
    #[serde(default)]
    pub condition: Option<TriggerCondition>,
    /// Optional filter for counter-related trigger modes (CR 714.2a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_filter: Option<CounterTriggerFilter>,
    /// CR 118.12: "Effect unless [player] pays {cost}" — tax trigger modifier.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless_pay: Option<UnlessPayModifier>,
    /// CR 603.2c: "One or more" triggers fire once per batch of simultaneous events.
    #[serde(default)]
    pub batched: bool,
    /// CR 700.14: Expend threshold — fires when cumulative mana spent on spells crosses N.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expend_threshold: Option<u32>,
    /// CR 508.3a: Filter for attack target type ("attacks a planeswalker").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_target_filter: Option<crate::types::triggers::AttackTargetFilter>,
    /// Typed player actions for PlayerPerformedAction trigger mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_actions: Option<Vec<PlayerActionKind>>,
}

impl TriggerDefinition {
    pub fn new(mode: TriggerMode) -> Self {
        Self {
            mode,
            execute: None,
            valid_card: None,
            origin: None,
            origin_zones: vec![],
            destination: None,
            trigger_zones: vec![],
            phase: None,
            optional: false,
            damage_kind: DamageKindFilter::Any,
            secondary: false,
            valid_target: None,
            valid_source: None,
            description: None,
            constraint: None,
            condition: None,
            counter_filter: None,
            unless_pay: None,
            batched: false,
            expend_threshold: None,
            attack_target_filter: None,
            player_actions: None,
        }
    }

    pub fn execute(mut self, ability: AbilityDefinition) -> Self {
        self.execute = Some(Box::new(ability));
        self
    }

    pub fn valid_card(mut self, filter: TargetFilter) -> Self {
        self.valid_card = Some(filter);
        self
    }

    pub fn origin(mut self, zone: Zone) -> Self {
        self.origin = Some(zone);
        self
    }

    pub fn destination(mut self, zone: Zone) -> Self {
        self.destination = Some(zone);
        self
    }

    pub fn trigger_zones(mut self, zones: Vec<Zone>) -> Self {
        self.trigger_zones = zones;
        self
    }

    pub fn phase(mut self, phase: Phase) -> Self {
        self.phase = Some(phase);
        self
    }

    pub fn optional(mut self) -> Self {
        self.optional = true;
        self
    }

    pub fn damage_kind(mut self, kind: DamageKindFilter) -> Self {
        self.damage_kind = kind;
        self
    }

    pub fn secondary(mut self) -> Self {
        self.secondary = true;
        self
    }

    pub fn valid_target(mut self, filter: TargetFilter) -> Self {
        self.valid_target = Some(filter);
        self
    }

    pub fn valid_source(mut self, filter: TargetFilter) -> Self {
        self.valid_source = Some(filter);
        self
    }

    pub fn description(mut self, desc: String) -> Self {
        self.description = Some(desc);
        self
    }

    pub fn constraint(mut self, constraint: TriggerConstraint) -> Self {
        self.constraint = Some(constraint);
        self
    }

    pub fn condition(mut self, condition: TriggerCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    pub fn counter_filter(mut self, filter: CounterTriggerFilter) -> Self {
        self.counter_filter = Some(filter);
        self
    }

    pub fn player_actions(mut self, actions: Vec<PlayerActionKind>) -> Self {
        self.player_actions = Some(actions);
        self
    }
}

/// Static ability definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StaticDefinition {
    #[serde(deserialize_with = "crate::types::statics::deserialize_static_mode_fwd")]
    pub mode: StaticMode,
    #[serde(default)]
    pub affected: Option<TargetFilter>,
    #[serde(default)]
    pub modifications: Vec<ContinuousModification>,
    #[serde(default)]
    pub condition: Option<StaticCondition>,
    #[serde(default)]
    pub affected_zone: Option<Zone>,
    #[serde(default)]
    pub effect_zone: Option<Zone>,
    /// CR 113.6 + CR 113.6b: Non-battlefield zones in which this static
    /// ability functions. An empty vec means the default — battlefield only
    /// (CR 113.6). A non-empty vec lists the non-battlefield zones the
    /// source must currently occupy for the static to contribute its
    /// continuous effects (e.g., Incarnation cycle — Anger, Filth, Brawn,
    /// Wonder, Valor — function from the graveyard). The battlefield is NOT
    /// implicitly included; callers that want both battlefield + graveyard
    /// must list both explicitly, matching `TriggerDefinition.trigger_zones`.
    #[serde(default)]
    pub active_zones: Vec<Zone>,
    #[serde(default)]
    pub characteristic_defining: bool,
    #[serde(default)]
    pub description: Option<String>,
}

impl StaticDefinition {
    pub fn new(mode: StaticMode) -> Self {
        Self {
            mode,
            affected: None,
            modifications: vec![],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones: vec![],
            characteristic_defining: false,
            description: None,
        }
    }

    pub fn continuous() -> Self {
        Self::new(StaticMode::Continuous)
    }

    pub fn affected(mut self, filter: TargetFilter) -> Self {
        self.affected = Some(filter);
        self
    }

    pub fn modifications(mut self, mods: Vec<ContinuousModification>) -> Self {
        self.modifications = mods;
        self
    }

    pub fn condition(mut self, cond: StaticCondition) -> Self {
        self.condition = Some(cond);
        self
    }

    pub fn affected_zone(mut self, zone: Zone) -> Self {
        self.affected_zone = Some(zone);
        self
    }

    pub fn effect_zone(mut self, zone: Zone) -> Self {
        self.effect_zone = Some(zone);
        self
    }

    /// CR 113.6 + CR 113.6b: Declare the non-battlefield zones this static
    /// functions in. Mirrors `TriggerDefinition::trigger_zones`. The
    /// battlefield is not implicitly included.
    pub fn active_zones(mut self, zones: Vec<Zone>) -> Self {
        self.active_zones = zones;
        self
    }

    pub fn cda(mut self) -> Self {
        self.characteristic_defining = true;
        self
    }

    pub fn description(mut self, desc: String) -> Self {
        self.description = Some(desc);
        self
    }
}

/// CR 614.1a: Damage modification formula for replacement effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DamageModification {
    /// amount * 2 (e.g. Furnace of Rath)
    Double,
    /// amount * 3 (e.g. Fiery Emancipation)
    Triple,
    /// amount + value (e.g. Torbran, +2)
    Plus { value: u32 },
    /// amount.saturating_sub(value) (e.g. Benevolent Unicorn, -1).
    /// CR 615.1 + CR 614.1a: Continuous prevention statics ("prevent that damage")
    /// emit `Minus { value: u32::MAX }` — saturating-subtraction yields 0 for any
    /// amount, and the replacement is not consumed (continuous, not shield-style).
    /// This is distinct from `ShieldKind::Prevention { All }` (one-shot consumed
    /// shield); the saturating-max sentinel covers the continuous case.
    Minus { value: u32 },
    /// CR 614.1a: Conditional — if amount < source's power, set amount = source's power.
    /// References the replacement source's (not the damage source's) current post-layer power.
    /// Used by Ojer Axonil: "deals damage equal to ~'s power instead."
    SetToSourcePower,
    /// CR 614.1a: Replace the damage amount with a fixed constant.
    /// "If [source] would deal damage to [target], it deals N damage to
    /// [target] instead." Distinct from `Plus`/`Minus` (arithmetic) and
    /// `SetToSourcePower` (dynamic) — this is a flat override of the
    /// event's amount with `value`.
    SetTo { value: u32 },
}

/// CR 614.1a: Quantity modification for replacement effects (tokens, counters).
/// Modeled after DamageModification but for non-damage quantities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum QuantityModification {
    /// count * 2 — Primal Vigor, Doubling Season, Parallel Lives, Anointed Procession
    Double,
    /// count + value — Hardened Scales (+1)
    Plus { value: u32 },
    /// count.saturating_sub(value) — Vizier of Remedies (-1)
    Minus { value: u32 },
}

/// CR 106.3 + CR 614.1a: Mana-production replacement payload.
/// Used by `ReplacementEvent::ProduceMana` to describe HOW the produced mana
/// should be modified before entering the player's mana pool.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManaModification {
    /// CR 614.1a: Replace with a specific mana type regardless of what was produced.
    /// e.g., Contamination ("produces {B} instead"), Pale Moon ("produces colorless instead").
    ReplaceWith { mana_type: ManaType },
}

/// CR 614.1a: Player axis for damage-recipient replacement filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DamageTargetPlayerScope {
    Any,
    Opponent,
    Specific(PlayerId),
}

/// CR 614.1a: Restricts which damage targets a replacement applies to.
/// Dedicated enum because `TargetRef` can be `Player` (not handled by `matches_target_filter`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum DamageTargetFilter {
    /// "to a creature" / "to that creature"
    CreatureOnly,
    /// "to a player" / "to that player" / "to an opponent"
    Player { player: DamageTargetPlayerScope },
    /// "to an opponent or a permanent an opponent controls" /
    /// "to that player or a permanent that player controls".
    PlayerOrPermanentsControlledBy { player: DamageTargetPlayerScope },
}

/// CR 614.1a: Restricts whether a damage replacement applies to combat, noncombat, or all damage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CombatDamageScope {
    CombatOnly,
    NoncombatOnly,
}

/// Whether a replacement effect is mandatory or offers the affected player a choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ReplacementMode {
    /// Always applies (default). Used for "enters tapped", "prevent damage", etc.
    #[default]
    Mandatory,
    /// Player may accept or decline. `execute` runs on accept; `decline` runs on decline.
    Optional {
        #[serde(default)]
        decline: Option<Box<AbilityDefinition>>,
    },
    /// CR 118.12 + CR 614.12a: Player may pay a cost as the replacement choice
    /// is made. The cost is paid before the permanent enters; `execute` runs on
    /// paid, and `decline` runs on decline or failed payment.
    MayCost {
        cost: AbilityCost,
        #[serde(default)]
        decline: Option<Box<AbilityDefinition>>,
    },
}

/// Replacement effect definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementDefinition {
    pub event: ReplacementEvent,
    #[serde(default)]
    pub execute: Option<Box<AbilityDefinition>>,
    #[serde(default)]
    pub mode: ReplacementMode,
    #[serde(default)]
    pub valid_card: Option<TargetFilter>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub condition: Option<ReplacementCondition>,
    /// CR 614.6: For Moved replacements, restricts which destination zone this replacement matches.
    /// E.g., `Some(Graveyard)` means "only replace zone changes TO the graveyard."
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination_zone: Option<Zone>,
    /// CR 614.1a: Damage modification formula (Double, Triple, Plus, Minus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage_modification: Option<DamageModification>,
    /// CR 614.1a: Restricts which damage source this replacement matches.
    /// Reuses existing TargetFilter infrastructure (SelfRef, Typed with ControllerRef/FilterProp).
    /// None = any source.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage_source_filter: Option<TargetFilter>,
    /// CR 614.1a: Restricts which damage target this replacement matches.
    /// Dedicated enum because TargetRef can be Player (not handled by matches_target_filter).
    /// None = any target.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage_target_filter: Option<DamageTargetFilter>,
    /// CR 614.1a: Restricts to combat-only or noncombat-only damage.
    /// None = all damage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub combat_scope: Option<CombatDamageScope>,
    /// Shield type for one-shot replacement effects that expire at cleanup.
    #[serde(default, skip_serializing_if = "ShieldKind::is_none")]
    pub shield_kind: ShieldKind,
    /// CR 614.1a: Quantity modification for token/counter replacements (Double, Plus, Minus).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantity_modification: Option<QuantityModification>,
    /// CR 614.1a: Restricts token replacement to specific owner scope.
    /// "under your control" → Some(You). None = any owner.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_owner_scope: Option<ControllerRef>,
    /// CR 614.1a: Restricts which player this replacement applies to.
    /// "an opponent would gain life" → Some(Opponent). None = applies to controller only.
    /// Parallel to `token_owner_scope` pattern.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_player: Option<ControllerRef>,
    /// Marks this replacement as consumed (one-shot). Skipped by find_applicable_replacements.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_consumed: bool,
    /// CR 514.2 + CR 614.1a: When true, this replacement is removed at the end-of-turn
    /// cleanup step regardless of whether it fired. Used by resolution-time replacement
    /// registrations like "If [target] would die this turn, exile it instead." (CR 614.1a).
    /// Orthogonal to `shield_kind`: shields imply EOT expiry via `is_shield()`, but this
    /// flag covers non-shield replacements (e.g., zone-redirection riders) that also need
    /// EOT cleanup. Cleanup logic ORs both signals.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub expires_at_eot: bool,
    /// CR 514.2 + CR 611.2a: Expiry for pending replacements whose lifetime is
    /// not just cleanup, e.g. "until your next turn".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expiry: Option<RestrictionExpiry>,
    /// CR 615.1a: Damage redirection target filter — when present, prevented damage is
    /// redirected to matching target instead (e.g., Pariah: "all damage that would be dealt
    /// to you is dealt to ~ instead" → SelfRef, meaning the enchanted permanent/source).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub redirect_target: Option<TargetFilter>,
    /// CR 106.3 + CR 614.1a: Mana modification for `ProduceMana` replacements.
    /// e.g., Contamination ("produces {B} instead") → `ReplaceWith { Black }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mana_modification: Option<ManaModification>,
    /// CR 614.1a + CR 111.1: Additional token creation appended to a
    /// `CreateToken` replacement. Covers the "those tokens plus a [Name] token"
    /// / "those tokens plus that many 1/1 green Squirrel creature tokens"
    /// pattern class (Chatterfang, Squirrel General; Donatello, Sol Invictus).
    /// The stored spec carries static characteristics only; `source_id` and
    /// `controller` are filled from the replacement source at apply time.
    /// Count equals the primary event's `count` at apply time, so additional
    /// replacements (e.g., Doubling Season) ordered before Chatterfang are
    /// reflected in the Squirrel count per CR 616.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_token_spec: Option<Box<crate::types::proposed_event::TokenSpec>>,
    /// CR 614.1a + CR 111.1: Ensure-all token-creation replacement. Used by
    /// Academy Manufactor ("If you would create a Clue, Food, or Treasure
    /// token, instead create one of each"). Each listed spec whose subtype
    /// is *not already present* in the proposed event's `TokenSpec.subtypes`
    /// is emitted as an additional `CreateToken` event via the same recursive
    /// `replace_event` path Chatterfang uses, preserving CR 616.1 ordering
    /// and idempotence (the `applied: HashSet<ReplacementId>` set on each
    /// spawned event blocks re-application of the same Manufactor).
    ///
    /// Distinct from `additional_token_spec` (which always appends): this
    /// field is *conditional* on subtype absence. The two fields are
    /// orthogonal — a single replacement may set either, but not both.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ensure_token_specs: Option<Vec<crate::types::proposed_event::TokenSpec>>,
}

impl ReplacementDefinition {
    /// Create a new replacement definition with only the required event field.
    /// All optional fields default to `None`/`Mandatory`.
    pub fn new(event: ReplacementEvent) -> Self {
        Self {
            event,
            execute: None,
            mode: ReplacementMode::Mandatory,
            valid_card: None,
            description: None,
            condition: None,
            destination_zone: None,
            damage_modification: None,
            damage_source_filter: None,
            damage_target_filter: None,
            combat_scope: None,
            shield_kind: ShieldKind::None,
            quantity_modification: None,
            token_owner_scope: None,
            valid_player: None,
            is_consumed: false,
            expires_at_eot: false,
            expiry: None,
            redirect_target: None,
            mana_modification: None,
            additional_token_spec: None,
            ensure_token_specs: None,
        }
    }

    pub fn execute(mut self, ability: AbilityDefinition) -> Self {
        self.execute = Some(Box::new(ability));
        self
    }

    pub fn mode(mut self, mode: ReplacementMode) -> Self {
        self.mode = mode;
        self
    }

    pub fn valid_card(mut self, filter: TargetFilter) -> Self {
        self.valid_card = Some(filter);
        self
    }

    pub fn description(mut self, desc: String) -> Self {
        self.description = Some(desc);
        self
    }

    pub fn condition(mut self, condition: ReplacementCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    pub fn destination_zone(mut self, zone: Zone) -> Self {
        self.destination_zone = Some(zone);
        self
    }

    pub fn damage_modification(mut self, modification: DamageModification) -> Self {
        self.damage_modification = Some(modification);
        self
    }

    pub fn damage_source_filter(mut self, filter: TargetFilter) -> Self {
        self.damage_source_filter = Some(filter);
        self
    }

    pub fn damage_target_filter(mut self, filter: DamageTargetFilter) -> Self {
        self.damage_target_filter = Some(filter);
        self
    }

    pub fn expiry(mut self, expiry: RestrictionExpiry) -> Self {
        self.expiry = Some(expiry);
        self
    }

    pub fn combat_scope(mut self, scope: CombatDamageScope) -> Self {
        self.combat_scope = Some(scope);
        self
    }

    /// CR 701.19a: Mark this replacement as a regeneration shield (one-shot, expires at cleanup).
    pub fn regeneration_shield(mut self) -> Self {
        self.shield_kind = ShieldKind::Regeneration;
        self
    }

    /// CR 615: Mark this replacement as a damage prevention shield.
    /// The shield absorbs or prevents damage, and is cleaned up at end of turn.
    pub fn prevention_shield(mut self, amount: PreventionAmount) -> Self {
        self.shield_kind = ShieldKind::Prevention { amount };
        self
    }

    pub fn quantity_modification(mut self, modification: QuantityModification) -> Self {
        self.quantity_modification = Some(modification);
        self
    }

    pub fn token_owner_scope(mut self, scope: ControllerRef) -> Self {
        self.token_owner_scope = Some(scope);
        self
    }

    /// CR 615.1a: Set the redirect target filter for damage redirection replacements.
    pub fn redirect_target(mut self, target: TargetFilter) -> Self {
        self.redirect_target = Some(target);
        self
    }

    /// CR 106.3 + CR 614.1a: Set the mana modification for `ProduceMana` replacements.
    pub fn mana_modification(mut self, modification: ManaModification) -> Self {
        self.mana_modification = Some(modification);
        self
    }

    /// CR 614.1a + CR 111.1: Attach an additional-token spec emitted alongside
    /// the primary `CreateToken` event (Chatterfang Squirrels, Donatello
    /// Mutagen). The stored spec's `source_id` / `controller` are placeholders
    /// overwritten with the replacement source at apply time.
    pub fn additional_token_spec(mut self, spec: crate::types::proposed_event::TokenSpec) -> Self {
        self.additional_token_spec = Some(Box::new(spec));
        self
    }

    /// CR 614.1a + CR 111.1: Attach the ensure-all token-spec list (Manufactor).
    /// At apply time, only specs whose subtype is missing from the proposed
    /// event's `TokenSpec.subtypes` are emitted.
    pub fn ensure_token_specs(
        mut self,
        specs: Vec<crate::types::proposed_event::TokenSpec>,
    ) -> Self {
        self.ensure_token_specs = Some(specs);
        self
    }
}

// ---------------------------------------------------------------------------
// ContinuousModification -- typed effect modifications for layers
// ---------------------------------------------------------------------------

/// What modification a continuous effect applies to an object.
/// Each variant knows its own layer implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopiableValues {
    pub name: String,
    pub mana_cost: ManaCost,
    pub color: Vec<ManaColor>,
    pub card_types: CardType,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub loyalty: Option<u32>,
    pub keywords: Vec<Keyword>,
    /// Ability-set fields are `Arc<Vec<_>>` so copy-effect propagation from
    /// source to target uses refcount sharing rather than deep clones.
    pub abilities: Arc<Vec<AbilityDefinition>>,
    pub trigger_definitions: Arc<Vec<TriggerDefinition>>,
    pub replacement_definitions: Arc<Vec<ReplacementDefinition>>,
    pub static_definitions: Arc<Vec<StaticDefinition>>,
}

/// What modification a continuous effect applies to an object.
/// Each variant knows its own layer implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContinuousModification {
    CopyValues {
        values: Box<CopiableValues>,
    },
    /// CR 707.9 + CR 707.2: Override the copy's name after `CopyValues` applies.
    /// Used by "enter as a copy, except its name is X" (e.g., Superior Spider-Man's
    /// Mind Swap). Applied in Layer 1 so the override is part of the copy's
    /// copiable values (per CR 707.9b).
    SetName {
        name: String,
    },
    AddPower {
        value: i32,
    },
    AddToughness {
        value: i32,
    },
    SetPower {
        value: i32,
    },
    SetToughness {
        value: i32,
    },
    AddKeyword {
        keyword: Keyword,
    },
    RemoveKeyword {
        keyword: Keyword,
    },
    GrantAbility {
        definition: Box<AbilityDefinition>,
    },
    /// CR 604.1: Grant a triggered ability to the affected object.
    /// Unlike GrantAbility (which pushes to obj.abilities), this pushes to
    /// obj.trigger_definitions so the trigger's event/condition metadata is
    /// preserved and the trigger fires correctly.
    GrantTrigger {
        trigger: Box<TriggerDefinition>,
    },
    RemoveAllAbilities,
    AddType {
        core_type: CoreType,
    },
    RemoveType {
        core_type: CoreType,
    },
    AddSubtype {
        subtype: String,
    },
    RemoveSubtype {
        subtype: String,
    },
    /// Set power to a dynamically computed value (CDA, layer 7a).
    SetDynamicPower {
        value: QuantityExpr,
    },
    /// Set toughness to a dynamically computed value (CDA, layer 7a).
    SetDynamicToughness {
        value: QuantityExpr,
    },
    /// CR 613.4b: Set base power to a dynamically computed value (layer 7b).
    /// Distinct from `SetDynamicPower` (layer 7a, CDA): this variant models
    /// one-shot or non-CDA set effects like Biomass Mutation's "base power
    /// and toughness X/X" where X is resolved at application time.
    SetPowerDynamic {
        value: QuantityExpr,
    },
    /// CR 613.4b: Set base toughness to a dynamically computed value (layer 7b).
    /// Distinct from `SetDynamicToughness` (layer 7a, CDA).
    SetToughnessDynamic {
        value: QuantityExpr,
    },
    /// CR 613.4c: Add dynamic +X to power (layer 7c), where X is computed at application time.
    AddDynamicPower {
        value: QuantityExpr,
    },
    /// CR 613.4c: Add dynamic +X to toughness (layer 7c), where X is computed at application time.
    AddDynamicToughness {
        value: QuantityExpr,
    },
    /// CR 702: Dynamic keyword where the parameter is computed at layer evaluation time.
    /// Used for "has annihilator X, where X is [quantity]".
    AddDynamicKeyword {
        kind: crate::types::keywords::DynamicKeywordKind,
        value: QuantityExpr,
    },
    /// Grants every creature type (Changeling CDA). Expanded at runtime
    /// using `GameState::all_creature_types`.
    AddAllCreatureTypes,
    /// CR 305.6 + CR 305.7: Adds all five basic land types in addition to
    /// existing types. Used by Prismatic Omen, Dryad of the Ilysian Grove.
    AddAllBasicLandTypes,
    /// Adds the source object's chosen subtype (creature type or basic land type).
    /// Resolved at layer evaluation time from the source's `chosen_attributes`.
    AddChosenSubtype {
        kind: ChosenSubtypeKind,
    },
    /// CR 105.3: Set the object's color to the chosen color.
    /// Reads from `chosen_attributes` at layer evaluation time.
    AddChosenColor,
    SetColor {
        colors: Vec<ManaColor>,
    },
    AddColor {
        color: ManaColor,
    },
    /// Grants a rule-modification static mode (e.g. MustBeBlocked, CantBeBlocked)
    /// to the affected object. Applied at layer 6 (ability-modifying).
    AddStaticMode {
        mode: StaticMode,
    },
    /// CR 613.4d: Switch power and toughness. Applied in layer 7d.
    SwitchPowerToughness,
    /// CR 510.1c: This creature assigns combat damage equal to its toughness
    /// rather than its power.
    AssignDamageFromToughness,
    /// CR 510.1c: This creature assigns combat damage as though it weren't blocked.
    AssignDamageAsThoughUnblocked,
    /// CR 510.1a: This creature assigns no combat damage.
    AssignNoCombatDamage,
    /// CR 613.2 (Layer 2): Change the controller of the affected object to the
    /// controller of the source permanent (e.g., Control Magic auras).
    ChangeController,
    /// CR 305.7: Sets a land's subtype to a basic land type, replacing old land
    /// subtypes and their associated mana abilities.
    SetBasicLandType {
        land_type: BasicLandType,
    },
    /// CR 707.9a: Retain a printed triggered ability from the source object's
    /// printed trigger list at the given index. Used by "becomes a copy of <X>,
    /// except it has this ability" patterns (Irma Part-Time Mutant, Cryptoplasm,
    /// Volrath's Shapeshifter), where "this ability" refers to the trigger
    /// containing the BecomeCopy effect — the trigger must persist on the copy
    /// so the cycle continues each turn.
    ///
    /// Applied at Layer 1 because CR 707.9a states the granted ability "becomes
    /// part of the copiable values for the copy". The runtime reads the source
    /// object's `base_trigger_definitions[source_trigger_index]` and pushes a
    /// clone onto the affected object's `trigger_definitions`.
    RetainPrintedTriggerFromSource {
        source_trigger_index: usize,
    },
    /// CR 205.4 + CR 707.9d: Add a supertype to the affected object's
    /// supertypes (e.g., Sarkhan, Soul Aflame: "it's legendary in addition
    /// to its other types"). Idempotent: pushing an already-present supertype
    /// is a no-op. Applied at Layer 4 (CR 613.1d) because supertypes are
    /// types per CR 205.4b.
    AddSupertype {
        supertype: Supertype,
    },
    /// CR 205.4 + CR 707.9b: Remove a supertype from the affected object
    /// (e.g., Miirym, Sentinel Wyrm: "except the token isn't legendary").
    /// Applied at Layer 4 (CR 613.1d). For tokens, the synthesized object's
    /// `base_card_types.supertypes` is also pruned at the resolver site
    /// because copiable values for tokens are baked in at creation
    /// (CR 707.2) rather than re-evaluated layer-by-layer.
    RemoveSupertype {
        supertype: Supertype,
    },
    /// CR 122.1 + CR 614.1c: Place counters on the entering / synthesized
    /// object as part of the copy resolution, optionally gated by the
    /// resolved core type of the entering object (Spark Double: "additional
    /// +1/+1 counter on it if it's a creature, additional loyalty counter
    /// if it's a planeswalker").
    ///
    /// This variant is NOT a continuous effect — it is consumed at
    /// resolution time by the BecomeCopy / CopyTokenOf resolvers and never
    /// reaches `apply_continuous_effect`. The placed counter then interacts
    /// with the layer system normally (Layer 7c/7d) via the CounterPT
    /// machinery already in place.
    AddCounterOnEnter {
        counter_type: String,
        count: QuantityExpr,
        /// `None` = unconditional. `Some(t)` gates the counter on the
        /// resolved object having `core_type == t` after copy values are
        /// applied (CR 707.9f-style "if the copy is or has certain
        /// characteristics").
        if_type: Option<CoreType>,
    },
}

// ---------------------------------------------------------------------------
// Target reference (unchanged)
// ---------------------------------------------------------------------------

/// Unified target reference for creatures and players.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum TargetRef {
    Object(ObjectId),
    Player(PlayerId),
}

// ---------------------------------------------------------------------------
// Keyword-action payloads
// ---------------------------------------------------------------------------

/// Typed payload for activated keyword abilities resolved from the stack.
///
/// CR 113.3b requires activated abilities to go on the stack. CR 113.7a
/// requires last-known-information carry-through when the source leaves
/// its zone. Cost-payment snapshots (e.g. `Station::snapshot_power`) are
/// captured at announcement time so resolution is safe even if the paid
/// creature leaves the battlefield.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum KeywordAction {
    /// CR 702.6a: attach source equipment to target creature.
    Equip {
        equipment_id: ObjectId,
        target_creature_id: ObjectId,
    },
    /// CR 702.122a: the Vehicle becomes an artifact creature until end of turn.
    /// Paid-creature ids are captured for logging and trigger context only.
    Crew {
        vehicle_id: ObjectId,
        paid_creature_ids: Vec<ObjectId>,
    },
    /// CR 702.171a: this permanent becomes saddled until end of turn.
    /// Paid-creature ids are captured for logging and trigger context only.
    Saddle {
        mount_id: ObjectId,
        paid_creature_ids: Vec<ObjectId>,
    },
    /// CR 702.184a: put charge counters on the Spacecraft equal to the tapped
    /// creature's power. The power value is snapshot at cost-payment time so
    /// resolution is stable under CR 113.7a even if the paid creature leaves
    /// the battlefield between announcement and resolution.
    Station {
        spacecraft_id: ObjectId,
        paid_creature_id: ObjectId,
        snapshot_power: i32,
    },
}

// ---------------------------------------------------------------------------
// Resolved ability -- simplified, zero HashMap
// ---------------------------------------------------------------------------

/// Runtime ability data passed to effect handlers at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAbility {
    pub effect: Effect,
    pub targets: Vec<TargetRef>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    /// The kind of ability this was (activated, triggered, static, etc.).
    /// Carried through from `AbilityDefinition` to allow resolution guards (e.g. skipping
    /// `BeginGame` abilities during normal stack resolution).
    #[serde(default)]
    pub kind: AbilityKind,
    #[serde(default)]
    pub sub_ability: Option<Box<ResolvedAbility>>,
    /// CR 608.2c: Alternative branch ("Otherwise") executed when condition is not met.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub else_ability: Option<Box<ResolvedAbility>>,
    #[serde(default)]
    pub duration: Option<Duration>,
    /// Condition that must be met for this ability to execute during resolution.
    #[serde(default)]
    pub condition: Option<AbilityCondition>,
    /// Casting-time facts for evaluating conditions during resolution.
    #[serde(default)]
    pub context: SpellContext,
    /// When true, targeting is optional ("up to one"). Player may choose zero targets.
    #[serde(default)]
    pub optional_targeting: bool,
    /// CR 609.3: Optional effect — controller prompted before execution.
    #[serde(default)]
    pub optional: bool,
    /// CR 608.2d: When set, an opponent chooses whether to perform this optional effect.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub optional_for: Option<OpponentMayScope>,
    /// Variable-count targeting preserved from the originating ability definition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_target: Option<MultiTargetSpec>,
    /// Human-readable description of this ability (from Oracle text / trigger line).
    /// Used by `OptionalEffectChoice` to tell the player what they're choosing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// CR 609.3: Repeat this ability N times (from "for each [X], [effect]").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_for: Option<QuantityExpr>,
    /// When true, moved/created objects from this effect are forwarded to the sub_ability.
    #[serde(default)]
    pub forward_result: bool,
    /// CR 118.12: "Effect unless [player] pays {cost}" — tax trigger modifier.
    /// When set, the payer is offered a choice before this effect executes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless_pay: Option<UnlessPayModifier>,
    /// CR 601.2d: Pre-assigned distribution from casting time ("divide N damage among").
    /// Each entry maps a target to its assigned portion. Read at resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribution: Option<Vec<(TargetRef, u32)>>,
    /// Player scope for "each player/opponent [effect]" patterns.
    /// When set, the effect iterates over matching players (each becomes the acting player).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_scope: Option<PlayerFilter>,
    /// CR 107.1b + CR 601.2f: The value of X chosen by the caster when this
    /// ability was cast/activated. `None` for abilities whose cost has no X.
    /// Read during resolution by `QuantityRef::Variable { name: "X" }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_x: Option<u32>,
    /// CR 117.1 + CR 202.3: Mana value of the object paid as part of this
    /// resolving ability's cost (sacrificed creature, exiled creature, etc.),
    /// captured at cost-payment time before the object leaves the battlefield.
    /// Read by `QuantityRef::CostPaidObjectManaValue` during resolution.
    /// `None` for abilities whose cost did not include a non-self
    /// sacrifice/exile of a permanent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_paid_object_mana_value: Option<u32>,
    /// CR 603.4: Index of the printed ability this resolution came from on the
    /// source object's ability list. Identifies "this ability" for per-turn
    /// resolution tracking (`AbilityCondition::NthResolutionThisTurn`). `None` for
    /// synthesized/runtime-only abilities (prowess, firebending) and activated
    /// abilities for which nth-resolution gating is not yet wired through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_index: Option<usize>,
}

impl ResolvedAbility {
    /// Build from a typed Effect. Simply stores the fields.
    pub fn new(
        effect: Effect,
        targets: Vec<TargetRef>,
        source_id: ObjectId,
        controller: PlayerId,
    ) -> Self {
        Self {
            effect,
            targets,
            source_id,
            controller,
            kind: AbilityKind::default(),
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            description: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            chosen_x: None,
            cost_paid_object_mana_value: None,
            ability_index: None,
        }
    }

    /// Propagate a chosen X value to this ability and every sub/else branch.
    /// CR 107.1b: X is one value per cast — the same for all effects produced.
    pub fn set_chosen_x_recursive(&mut self, value: u32) {
        self.chosen_x = Some(value);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_chosen_x_recursive(value);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_chosen_x_recursive(value);
        }
    }

    /// CR 117.1 + CR 202.3: Stamp the cost-paid object's mana value across this
    /// ability and every sub/else branch. Captured at cost-payment time (before
    /// the object leaves the battlefield) and read by
    /// `QuantityRef::CostPaidObjectManaValue` during resolution.
    pub fn set_cost_paid_object_mana_value_recursive(&mut self, value: u32) {
        self.cost_paid_object_mana_value = Some(value);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_cost_paid_object_mana_value_recursive(value);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_cost_paid_object_mana_value_recursive(value);
        }
    }

    pub fn kind(mut self, kind: AbilityKind) -> Self {
        self.kind = kind;
        self
    }

    pub fn sub_ability(mut self, ability: ResolvedAbility) -> Self {
        self.sub_ability = Some(Box::new(ability));
        self
    }

    pub fn else_ability(mut self, ability: ResolvedAbility) -> Self {
        self.else_ability = Some(Box::new(ability));
        self
    }

    pub fn duration(mut self, duration: Duration) -> Self {
        self.duration = Some(duration);
        self
    }

    pub fn condition(mut self, condition: AbilityCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    pub fn context(mut self, context: SpellContext) -> Self {
        self.context = context;
        self
    }

    /// Extract the first `TargetRef::Player` from targets, or default to controller.
    /// Used by effects that target a player (mill, discard, life loss, shuffle, etc.).
    pub fn target_player(&self) -> PlayerId {
        self.targets
            .iter()
            .find_map(|t| match t {
                TargetRef::Player(pid) => Some(*pid),
                _ => None,
            })
            .unwrap_or(self.controller)
    }
}

// ---------------------------------------------------------------------------
// Error types
// ---------------------------------------------------------------------------

/// Error type for effect handler failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum EffectError {
    #[error("missing required parameter: {0}")]
    MissingParam(String),
    #[error("invalid parameter value: {0}")]
    InvalidParam(String),
    #[error("player not found")]
    PlayerNotFound,
    #[error("object not found: {0:?}")]
    ObjectNotFound(ObjectId),
    #[error("sub-ability chain too deep")]
    ChainTooDeep,
    #[error("unregistered effect type: {0}")]
    Unregistered(String),
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn target_ref_object_variant() {
        let t = TargetRef::Object(ObjectId(5));
        assert_eq!(t, TargetRef::Object(ObjectId(5)));
        assert_ne!(t, TargetRef::Object(ObjectId(6)));
    }

    #[test]
    fn target_filter_normalized_merges_compatible_typed_conjunctions() {
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::permanent().properties(vec![
                    FilterProp::HasAttachment {
                        kind: AttachmentKind::Aura,
                        controller: None,
                    },
                ])),
                TargetFilter::Typed(TypedFilter::creature()),
                TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::You)),
            ],
        };

        assert_eq!(
            filter.normalized(),
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: vec![FilterProp::HasAttachment {
                    kind: AttachmentKind::Aura,
                    controller: None,
                }],
            })
        );
    }

    #[test]
    fn target_filter_normalized_preserves_conflicting_controller_conjunctions() {
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                TargetFilter::Typed(TypedFilter::permanent().controller(ControllerRef::Opponent)),
            ],
        };

        assert_eq!(
            filter.normalized(),
            TargetFilter::And {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
                    TargetFilter::Typed(
                        TypedFilter::permanent().controller(ControllerRef::Opponent)
                    ),
                ],
            }
        );
    }

    #[test]
    fn target_filter_normalized_recurses_through_nested_filter_props() {
        let filter =
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Targets {
                filter: Box::new(TargetFilter::And {
                    filters: vec![
                        TargetFilter::Typed(TypedFilter::permanent()),
                        TargetFilter::Typed(TypedFilter::creature()),
                    ],
                }),
            }]));

        assert_eq!(
            filter.normalized(),
            TargetFilter::Typed(TypedFilter::default().properties(vec![FilterProp::Targets {
                filter: Box::new(TargetFilter::Typed(TypedFilter::creature())),
            }]))
        );
    }

    /// CR 107.1c + CR 608.2d: `QuantityExpr::up_to(max)` constructs the
    /// wrapper variant; `peel_up_to` recovers (max, true).
    #[test]
    fn quantity_expr_up_to_constructor_and_peel_round_trip() {
        let inner = QuantityExpr::Fixed { value: 3 };
        let wrapped = QuantityExpr::up_to(inner.clone());
        assert!(wrapped.is_up_to());
        let (peeled, was_up_to) = wrapped.peel_up_to();
        assert!(was_up_to);
        assert_eq!(peeled, &inner);
    }

    /// CR 107.1c: `peel_up_to` on a non-UpTo expression returns the
    /// expression unchanged with `false`. Resolvers depend on this so they
    /// can call `peel_up_to` unconditionally.
    #[test]
    fn quantity_expr_peel_up_to_passes_through_non_up_to() {
        let expr = QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        };
        assert!(!expr.is_up_to());
        let (peeled, was_up_to) = expr.peel_up_to();
        assert!(!was_up_to);
        assert_eq!(peeled, &expr);
    }

    /// Demonstrates the new compositional power: "up to your hand size cards"
    /// composes `UpTo` over a dynamic `Ref` quantity, which was structurally
    /// inexpressible under the old `up_to: bool` field layout.
    #[test]
    fn quantity_expr_up_to_composes_with_ref_for_hand_size() {
        let expr = QuantityExpr::up_to(QuantityExpr::Ref {
            qty: QuantityRef::HandSize {
                player: PlayerScope::Controller,
            },
        });
        let (max, up_to) = expr.peel_up_to();
        assert!(up_to);
        match max {
            QuantityExpr::Ref {
                qty:
                    QuantityRef::HandSize {
                        player: PlayerScope::Controller,
                    },
            } => {}
            other => panic!("expected Ref {{ HandSize }}, got {other:?}"),
        }
    }

    /// Demonstrates the second new compositional axis: "up to half the
    /// creatures they control" stacks `UpTo` over `HalfRounded` over
    /// `ObjectCount`. Each layer is an existing primitive — the refactor
    /// only added the outer wrapper.
    #[test]
    fn quantity_expr_up_to_composes_with_half_rounded_object_count() {
        let creatures_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            ..Default::default()
        });
        let expr = QuantityExpr::up_to(QuantityExpr::HalfRounded {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: creatures_filter,
                },
            }),
            rounding: RoundingMode::Down,
        });
        let (max, up_to) = expr.peel_up_to();
        assert!(up_to);
        assert!(matches!(max, QuantityExpr::HalfRounded { .. }));
    }

    /// CR 107.1c: Nesting `UpTo` inside `UpTo` is meaningless ("up to up to N"
    /// is just "up to N"). The constructor `up_to()` debug-asserts against
    /// this. Wrapped only in `cfg(debug_assertions)` because `debug_assert!`
    /// is a no-op in release builds.
    #[test]
    #[cfg(debug_assertions)]
    #[should_panic(expected = "QuantityExpr::UpTo cannot wrap another UpTo")]
    fn quantity_expr_up_to_rejects_nested_up_to() {
        let inner = QuantityExpr::up_to(QuantityExpr::Fixed { value: 3 });
        let _ = QuantityExpr::up_to(inner);
    }

    #[test]
    fn target_ref_player_variant() {
        let t = TargetRef::Player(PlayerId(1));
        assert_eq!(t, TargetRef::Player(PlayerId(1)));
        assert_ne!(t, TargetRef::Player(PlayerId(0)));
    }

    #[test]
    fn target_ref_object_ne_player() {
        let obj = TargetRef::Object(ObjectId(0));
        let plr = TargetRef::Player(PlayerId(0));
        assert_ne!(obj, plr);
    }

    #[test]
    fn resolved_ability_serializes_and_roundtrips() {
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Object(ObjectId(10))],
            ObjectId(1),
            PlayerId(0),
        );
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn resolved_ability_with_sub_ability_roundtrips() {
        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
            vec![TargetRef::Player(PlayerId(1))],
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(sub);
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn effect_error_displays_meaningful_messages() {
        assert_eq!(
            EffectError::MissingParam("NumDmg".to_string()).to_string(),
            "missing required parameter: NumDmg"
        );
        assert_eq!(
            EffectError::InvalidParam("bad value".to_string()).to_string(),
            "invalid parameter value: bad value"
        );
        assert_eq!(EffectError::PlayerNotFound.to_string(), "player not found");
        assert_eq!(
            EffectError::ObjectNotFound(ObjectId(42)).to_string(),
            "object not found: ObjectId(42)"
        );
        assert_eq!(
            EffectError::ChainTooDeep.to_string(),
            "sub-ability chain too deep"
        );
        assert_eq!(
            EffectError::Unregistered("Foo".to_string()).to_string(),
            "unregistered effect type: Foo"
        );
    }

    #[test]
    fn untap_cost_serialization_roundtrip() {
        let cost = AbilityCost::Untap;
        let json = serde_json::to_string(&cost).unwrap();
        assert!(json.contains("\"type\":\"Untap\""));
        let deser: AbilityCost = serde_json::from_str(&json).unwrap();
        assert_eq!(deser, AbilityCost::Untap);
    }

    #[test]
    fn blight_cost_roundtrips() {
        let cost = AbilityCost::Blight { count: 2 };
        let json = serde_json::to_value(&cost).unwrap();
        assert_eq!(json["type"], "Blight");
        assert_eq!(json["count"], 2);
        let deserialized: AbilityCost = serde_json::from_value(json).unwrap();
        assert_eq!(deserialized, cost);
    }

    // --- Serde roundtrip tests for new typed definitions ---

    #[test]
    fn trigger_definition_roundtrip() {
        let trigger = TriggerDefinition {
            mode: TriggerMode::ChangesZone,
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            origin: Some(Zone::Battlefield),
            origin_zones: vec![],
            destination: Some(Zone::Graveyard),
            trigger_zones: vec![Zone::Battlefield],
            phase: None,
            optional: false,
            damage_kind: DamageKindFilter::Any,
            secondary: false,
            valid_target: None,
            valid_source: None,
            description: Some("When ~ dies, draw a card.".to_string()),
            constraint: None,
            condition: None,
            counter_filter: None,
            unless_pay: None,
            batched: false,
            expend_threshold: None,
            attack_target_filter: None,
            player_actions: None,
        };
        let json = serde_json::to_string(&trigger).unwrap();
        let deserialized: TriggerDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, deserialized);
    }

    #[test]
    fn static_definition_roundtrip() {
        let static_def = StaticDefinition {
            mode: StaticMode::Continuous,
            affected: Some(
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .into(),
            ),
            modifications: vec![
                ContinuousModification::AddPower { value: 1 },
                ContinuousModification::AddToughness { value: 1 },
            ],
            condition: None,
            affected_zone: None,
            effect_zone: None,
            active_zones: vec![],
            characteristic_defining: false,
            description: Some("Other creatures you control get +1/+1.".to_string()),
        };
        let json = serde_json::to_string(&static_def).unwrap();
        let deserialized: StaticDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(static_def, deserialized);
    }

    #[test]
    fn replacement_definition_roundtrip() {
        let replacement = ReplacementDefinition {
            execute: Some(Box::new(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: GainLifePlayer::Controller,
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(
                "If damage would be dealt to ~, prevent it and gain 1 life.".to_string(),
            ),
            ..ReplacementDefinition::new(ReplacementEvent::DamageDone)
        };
        let json = serde_json::to_string(&replacement).unwrap();
        let deserialized: ReplacementDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(replacement, deserialized);
    }

    #[test]
    fn target_filter_nested_roundtrip() {
        let filter = TargetFilter::And {
            filters: vec![
                TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .into(),
                TargetFilter::Not {
                    filter: Box::new(TargetFilter::SelfRef),
                },
            ],
        };
        let json = serde_json::to_string(&filter).unwrap();
        let deserialized: TargetFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, deserialized);
    }

    #[test]
    fn ability_definition_with_sub_ability_chain_roundtrip() {
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 3 },
                target: TargetFilter::Any,
                damage_source: None,
            },
        )
        .cost(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![],
                generic: 2,
            },
        })
        .sub_ability(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        ))
        .duration(Duration::UntilEndOfTurn)
        .description("Deal 3 damage, then draw a card.".to_string())
        .target_prompt("Choose a target".to_string())
        .sorcery_speed();
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: AbilityDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn ability_cost_expanded_variants_roundtrip() {
        let costs = vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 3,
                },
            },
            AbilityCost::Tap,
            AbilityCost::Loyalty { amount: -2 },
            AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: 2 },
            },
            AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: false,
            },
            AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TypedFilter::creature().into()),
            },
            AbilityCost::TapCreatures {
                count: 2,
                filter: TypedFilter::creature()
                    .controller(ControllerRef::You)
                    .into(),
            },
            AbilityCost::Sacrifice {
                target: TypedFilter::new(TypeFilter::Artifact).into(),
                count: 1,
            },
        ];
        let json = serde_json::to_string(&costs).unwrap();
        let deserialized: Vec<AbilityCost> = serde_json::from_str(&json).unwrap();
        assert_eq!(costs, deserialized);
    }

    #[test]
    fn continuous_modification_roundtrip() {
        let mods = vec![
            ContinuousModification::AddPower { value: 2 },
            ContinuousModification::AddToughness { value: 2 },
            ContinuousModification::SetPower { value: 0 },
            ContinuousModification::AddKeyword {
                keyword: Keyword::Flying,
            },
            ContinuousModification::RemoveKeyword {
                keyword: Keyword::Defender,
            },
            ContinuousModification::GrantAbility {
                definition: Box::new(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Unimplemented {
                        name: "Hexproof".to_string(),
                        description: None,
                    },
                )),
            },
            ContinuousModification::RemoveAllAbilities,
            ContinuousModification::AddType {
                core_type: CoreType::Artifact,
            },
            ContinuousModification::RemoveType {
                core_type: CoreType::Creature,
            },
            ContinuousModification::SetColor {
                colors: vec![ManaColor::Blue],
            },
            ContinuousModification::AddColor {
                color: ManaColor::Red,
            },
        ];
        let json = serde_json::to_string(&mods).unwrap();
        let deserialized: Vec<ContinuousModification> = serde_json::from_str(&json).unwrap();
        assert_eq!(mods, deserialized);
    }

    #[test]
    fn effect_unimplemented_variant_roundtrip() {
        let effect = Effect::Unimplemented {
            name: "Venture".to_string(),
            description: Some("Venture into the dungeon".to_string()),
        };
        let json = serde_json::to_string(&effect).unwrap();
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn effect_cleanup_typed_fields_roundtrip() {
        let effect = Effect::Cleanup {
            clear_remembered: true,
            clear_chosen_player: false,
            clear_chosen_color: true,
            clear_chosen_type: false,
            clear_chosen_card: false,
            clear_imprinted: true,
            clear_triggers: false,
            clear_coin_flips: false,
        };
        let json = serde_json::to_string(&effect).unwrap();
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn effect_mana_typed_roundtrip() {
        let effect = Effect::Mana {
            produced: ManaProduction::Fixed {
                colors: vec![ManaColor::Green, ManaColor::Green],
                contribution: ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        };
        let json = serde_json::to_string(&effect).unwrap();
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn effect_mana_legacy_vec_deserializes_as_fixed() {
        // Legacy format stored produced as Vec<ManaColor> e.g. `["White","Green"]`
        let legacy_json = r#"{"type":"Mana","produced":["White","Green"]}"#;
        let deserialized: Effect = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(
            deserialized,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::White, ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            }
        );
    }

    #[test]
    fn effect_generic_effect_typed_roundtrip() {
        let effect = Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected: Some(TargetFilter::SelfRef),
                modifications: vec![ContinuousModification::AddPower { value: 3 }],
                condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: vec![],
                characteristic_defining: false,
                description: None,
            }],
            duration: Some(Duration::UntilEndOfTurn),
            target: None,
        };
        let json = serde_json::to_string(&effect).unwrap();
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn static_condition_roundtrip() {
        let conditions = vec![
            StaticCondition::DevotionGE {
                colors: vec![ManaColor::White, ManaColor::Blue],
                threshold: 7,
            },
            StaticCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::LifeAboveStarting,
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 7 },
            },
            StaticCondition::IsPresent {
                filter: Some(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .into(),
                ),
            },
            StaticCondition::Unrecognized {
                text: "some complex condition".to_string(),
            },
            StaticCondition::ClassLevelGE { level: 2 },
            StaticCondition::None,
        ];
        let json = serde_json::to_string(&conditions).unwrap();
        let deserialized: Vec<StaticCondition> = serde_json::from_str(&json).unwrap();
        assert_eq!(conditions, deserialized);
    }

    #[test]
    fn duration_roundtrip() {
        let durations = vec![
            Duration::UntilEndOfTurn,
            Duration::UntilEndOfCombat,
            Duration::UntilNextTurnOf {
                player: PlayerScope::Controller,
            },
            Duration::UntilNextUntapStepOf {
                player: PlayerScope::Controller,
            },
            Duration::UntilHostLeavesPlay,
            Duration::Permanent,
        ];
        let json = serde_json::to_string(&durations).unwrap();
        let deserialized: Vec<Duration> = serde_json::from_str(&json).unwrap();
        assert_eq!(durations, deserialized);
    }

    #[test]
    fn pt_value_roundtrip() {
        let values = vec![
            PtValue::Fixed(4),
            PtValue::Variable("*".to_string()),
            PtValue::Variable("X".to_string()),
        ];
        let json = serde_json::to_string(&values).unwrap();
        let deserialized: Vec<PtValue> = serde_json::from_str(&json).unwrap();
        assert_eq!(values, deserialized);
    }

    #[test]
    fn effect_token_roundtrip() {
        let effect = Effect::Token {
            name: "Soldier".to_string(),
            power: PtValue::Fixed(1),
            toughness: PtValue::Variable("X".to_string()),
            types: vec!["Creature".to_string(), "Soldier".to_string()],
            colors: vec![ManaColor::White],
            keywords: vec![Keyword::Vigilance],
            tapped: true,
            count: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "the number of creatures you control".to_string(),
                },
            },
            owner: TargetFilter::Controller,
            attach_to: None,
            enters_attacking: false,
            supertypes: vec![],
            static_abilities: vec![],
            enter_with_counters: vec![],
        };
        let json = serde_json::to_string(&effect).unwrap();
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn filter_prop_roundtrip() {
        let props = vec![
            FilterProp::Token,
            FilterProp::Attacking,
            FilterProp::AttackingController,
            FilterProp::Blocking,
            FilterProp::BlockingSource,
            FilterProp::Unblocked,
            FilterProp::Tapped,
            FilterProp::Untapped,
            FilterProp::WithKeyword {
                value: Keyword::Flying,
            },
            FilterProp::HasKeywordKind {
                value: KeywordKind::Flashback,
            },
            FilterProp::WithoutKeyword {
                value: Keyword::Flying,
            },
            FilterProp::WithoutKeywordKind {
                value: KeywordKind::Cycling,
            },
            FilterProp::CountersGE {
                counter_type: CounterType::Plus1Plus1,
                count: QuantityExpr::Fixed { value: 3 },
            },
            FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            },
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
            FilterProp::Owned {
                controller: ControllerRef::Opponent,
            },
            FilterProp::EnchantedBy,
            FilterProp::EquippedBy,
            FilterProp::TargetsOnly {
                filter: Box::new(TargetFilter::SelfRef),
            },
            FilterProp::Other {
                value: "custom".to_string(),
            },
        ];
        let json = serde_json::to_string(&props).unwrap();
        let deserialized: Vec<FilterProp> = serde_json::from_str(&json).unwrap();
        assert_eq!(props, deserialized);
    }

    #[test]
    fn resolved_ability_no_hashmap_fields() {
        // Verify ResolvedAbility can be created and round-tripped without any HashMap fields
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 2 },
                target: TargetFilter::Controller,
            },
            vec![TargetRef::Player(PlayerId(0))],
            ObjectId(1),
            PlayerId(0),
        );
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn resolved_ability_duration_roundtrips() {
        let ability = ResolvedAbility::new(
            Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
            vec![TargetRef::Object(ObjectId(10))],
            ObjectId(1),
            PlayerId(0),
        )
        .duration(Duration::UntilHostLeavesPlay);
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
        assert_eq!(deserialized.duration, Some(Duration::UntilHostLeavesPlay));
    }

    #[test]
    fn parent_target_serde_roundtrip() {
        let filter = TargetFilter::ParentTarget;
        let json = serde_json::to_string(&filter).unwrap();
        let deserialized: TargetFilter = serde_json::from_str(&json).unwrap();
        assert_eq!(filter, deserialized);
    }

    #[test]
    fn change_zone_owner_library_serde_roundtrip() {
        let effect = Effect::ChangeZone {
            origin: Some(Zone::Battlefield),
            destination: Zone::Library,
            target: TargetFilter::Any,
            owner_library: true,
            enter_transformed: false,
            under_your_control: false,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
        };
        let json = serde_json::to_string(&effect).unwrap();
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn change_zone_owner_library_defaults_false() {
        // Backward compat: JSON without owner_library field should default to false
        let json = r#"{"type":"ChangeZone","destination":"Battlefield","target":{"type":"Any"}}"#;
        let effect: Effect = serde_json::from_str(json).unwrap();
        assert!(matches!(
            effect,
            Effect::ChangeZone {
                owner_library: false,
                enter_transformed: false,
                under_your_control: false,
                enter_tapped: false,
                enters_attacking: false,
                ..
            }
        ));
    }

    // CR 118: Cost taxonomy mapping — exhaustive per-variant coverage.
    mod cost_category {
        use super::*;

        #[test]
        fn mana_only() {
            let cost = AbilityCost::Mana {
                cost: ManaCost::zero(),
            };
            assert_eq!(cost.categories(), vec![CostCategory::ManaOnly]);
        }

        #[test]
        fn tap_self() {
            assert_eq!(AbilityCost::Tap.categories(), vec![CostCategory::TapsSelf]);
        }

        #[test]
        fn untap_self() {
            assert_eq!(
                AbilityCost::Untap.categories(),
                vec![CostCategory::UntapsSelf]
            );
        }

        #[test]
        fn loyalty() {
            assert_eq!(
                AbilityCost::Loyalty { amount: -2 }.categories(),
                vec![CostCategory::PaysLoyalty]
            );
        }

        #[test]
        fn sacrifice_permanent() {
            let cost = AbilityCost::Sacrifice {
                target: TargetFilter::Any,
                count: 1,
            };
            assert_eq!(cost.categories(), vec![CostCategory::SacrificesPermanent]);
        }

        #[test]
        fn pay_life() {
            assert_eq!(
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 2 },
                }
                .categories(),
                vec![CostCategory::PaysLife]
            );
        }

        #[test]
        fn discard() {
            let cost = AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter: None,
                random: false,
                self_ref: false,
            };
            assert_eq!(cost.categories(), vec![CostCategory::Discards]);
        }

        #[test]
        fn exile_cards() {
            let cost = AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: None,
            };
            assert_eq!(cost.categories(), vec![CostCategory::ExilesCards]);
        }

        #[test]
        fn collect_evidence() {
            assert_eq!(
                AbilityCost::CollectEvidence { amount: 4 }.categories(),
                vec![CostCategory::ExilesCards]
            );
        }

        #[test]
        fn tap_other_creatures() {
            let cost = AbilityCost::TapCreatures {
                count: 2,
                filter: TargetFilter::Any,
            };
            assert_eq!(cost.categories(), vec![CostCategory::TapsOtherCreatures]);
        }

        #[test]
        fn remove_counter() {
            let cost = AbilityCost::RemoveCounter {
                count: 1,
                counter_type: "+1/+1".to_string(),
                target: None,
            };
            assert_eq!(cost.categories(), vec![CostCategory::RemovesCounters]);
        }

        #[test]
        fn pay_energy() {
            assert_eq!(
                AbilityCost::PayEnergy { amount: 3 }.categories(),
                vec![CostCategory::PaysEnergy]
            );
        }

        #[test]
        fn pay_speed() {
            let cost = AbilityCost::PaySpeed {
                amount: QuantityExpr::Fixed { value: 1 },
            };
            assert_eq!(cost.categories(), vec![CostCategory::PaysSpeed]);
        }

        #[test]
        fn return_to_hand() {
            let cost = AbilityCost::ReturnToHand {
                count: 1,
                filter: None,
            };
            assert_eq!(cost.categories(), vec![CostCategory::ReturnsToHand]);
        }

        #[test]
        fn mill() {
            assert_eq!(
                AbilityCost::Mill { count: 3 }.categories(),
                vec![CostCategory::Mills]
            );
        }

        #[test]
        fn exert() {
            assert_eq!(AbilityCost::Exert.categories(), vec![CostCategory::Exerts]);
        }

        #[test]
        fn blight_puts_counters() {
            assert_eq!(
                AbilityCost::Blight { count: 1 }.categories(),
                vec![CostCategory::PutsCounters]
            );
        }

        #[test]
        fn reveal() {
            let cost = AbilityCost::Reveal {
                count: 1,
                filter: None,
            };
            assert_eq!(cost.categories(), vec![CostCategory::Reveals]);
        }

        #[test]
        fn waterbend_keyword_cost() {
            let cost = AbilityCost::Waterbend {
                cost: ManaCost::zero(),
            };
            assert_eq!(cost.categories(), vec![CostCategory::KeywordCost]);
        }

        #[test]
        fn ninjutsu_keyword_cost() {
            let cost = AbilityCost::NinjutsuFamily {
                variant: NinjutsuVariant::Ninjutsu,
                mana_cost: ManaCost::zero(),
            };
            assert_eq!(cost.categories(), vec![CostCategory::KeywordCost]);
        }

        #[test]
        fn unimplemented_returns_empty() {
            let cost = AbilityCost::Unimplemented {
                description: "foo".to_string(),
            };
            assert!(cost.categories().is_empty());
        }

        #[test]
        fn composite_flattens_and_dedupes() {
            // Tap + Sacrifice + Tap (duplicate) should yield [TapsSelf, SacrificesPermanent].
            let cost = AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Sacrifice {
                        target: TargetFilter::Any,
                        count: 1,
                    },
                    AbilityCost::Tap,
                ],
            };
            assert_eq!(
                cost.categories(),
                vec![CostCategory::TapsSelf, CostCategory::SacrificesPermanent]
            );
        }

        #[test]
        fn ability_definition_cost_categories_none_when_no_cost() {
            let def = AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            );
            assert!(def.cost_categories().is_empty());
        }

        #[test]
        fn ability_definition_cost_categories_delegates() {
            let def = AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Sacrifice {
                target: TargetFilter::Any,
                count: 1,
            });
            assert_eq!(
                def.cost_categories(),
                vec![CostCategory::SacrificesPermanent]
            );
        }
    }
}

#[cfg(test)]
mod modal_ability_tests {
    use super::*;

    #[test]
    fn ability_definition_supports_modal() {
        let mode1 = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let mode2 = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::GainLife {
                amount: QuantityExpr::Fixed { value: 3 },
                player: GainLifePlayer::Controller,
            },
        );
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["Draw a card.".to_string(), "Gain 3 life.".to_string()],
            ..Default::default()
        };
        let def = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Unimplemented {
                name: "modal_placeholder".to_string(),
                description: None,
            },
        )
        .with_modal(modal.clone(), vec![mode1, mode2]);

        assert!(def.modal.is_some());
        assert_eq!(def.mode_abilities.len(), 2);
    }
}

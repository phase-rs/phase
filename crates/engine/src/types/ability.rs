use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::card_type::CoreType;
use super::game_state::{DistributionUnit, RetargetScope};
use super::identifiers::ObjectId;
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost};
use super::phase::Phase;
use super::player::PlayerId;
use super::replacements::ReplacementEvent;
use super::statics::StaticMode;
use super::triggers::TriggerMode;
use super::zones::Zone;

// ---------------------------------------------------------------------------
// Supporting types
// ---------------------------------------------------------------------------

/// CR 700.2: Who makes a choice during an effect's resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Chooser {
    /// The controller of the spell/ability makes the choice.
    #[default]
    Controller,
    /// An opponent of the controller makes the choice (CR 700.2).
    /// In 2-player, the single opponent. In multiplayer, controller chooses which opponent.
    Opponent,
}

/// CR 608.2d: Who may choose to perform an optional effect during resolution.
/// Used with `AbilityDefinition::optional_for` to route the "you may" prompt to opponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum OpponentMayScope {
    /// "any opponent may" — each opponent in APNAP order gets the chance; first accept wins.
    AnyOpponent,
}

/// What kind of named choice the player must make at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
}

/// The five basic land types (CR 305.6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum Parity {
    Odd,
    Even,
}

/// A branch in a d20/d6/d4 result table (CR 706.2).
/// Each branch covers a contiguous range of die results and maps to an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum PreventionScope {
    /// Prevent all damage (combat + noncombat).
    #[default]
    AllDamage,
    /// Prevent only combat damage.
    CombatDamage,
}

/// CR 615: How much damage to prevent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum PreventionAmount {
    /// "Prevent the next N damage"
    Next(u32),
    /// "Prevent all damage"
    All,
}

/// Shield type for one-shot replacement effects that expire at cleanup.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
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

/// A typed choice stored on a permanent (e.g., "choose a color" → Color(Red)).
/// The variant discriminant serves as the category key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value")]
pub enum ChosenAttribute {
    Color(ManaColor),
    CreatureType(String),
    BasicLandType(BasicLandType),
    CardType(CoreType),
    OddOrEven(Parity),
    CardName(String),
    /// Stores the chosen opponent/player ID (CR 800.4a).
    Player(PlayerId),
    /// Stores two chosen colors as a pair.
    TwoColors([ManaColor; 2]),
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
            // Player covers both Player and Opponent choice types
            Self::Player(_) => ChoiceType::Player,
            Self::TwoColors(_) => ChoiceType::TwoColors,
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
            ChoiceValue::Number(_) | ChoiceValue::Label(_) | ChoiceValue::LandType(_) => None,
        }
    }
}

/// A typed value chosen at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
        }
    }
}

/// How to specify a damage amount -- either a fixed integer or a variable reference.
/// Which category of chosen attribute to read as a subtype.
/// Used by `ContinuousModification::AddChosenSubtype` in layer evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ChosenSubtypeKind {
    CreatureType,
    BasicLandType,
}

/// Which players' zones to count across for zone-based quantity references.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum CountScope {
    Controller,
    All,
    Opponents,
}

/// Which zone to count cards in (for `QuantityRef::ZoneCardCount`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ZoneRef {
    Graveyard,
    Exile,
    Library,
}

/// Who gains life from a GainLife effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum GainLifePlayer {
    /// The ability's controller (default).
    #[default]
    Controller,
    /// The controller of the targeted permanent.
    TargetedController,
}

/// How much life is gained — a fixed amount or derived from the targeted permanent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DoublePTMode {
    Power,
    Toughness,
    PowerAndToughness,
}

/// Power/toughness value -- either a fixed integer or a variable reference (e.g. "*", "X").
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","value":2}` (new)
/// and plain strings like `"2"` or `"*"` (legacy card-data.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
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

/// Mana production descriptor for `Effect::Mana`.
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","colors":["White"]}` (new)
/// and a plain array of `ManaColor` like `["White","Green"]` (legacy, pre-ManaProduction refactor).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ManaProduction {
    /// Produce an explicit fixed sequence of colored mana symbols (e.g. `{W}{U}`).
    Fixed {
        #[serde(default)]
        colors: Vec<ManaColor>,
    },
    /// Produce N colorless mana (e.g. `{C}`, `{C}{C}`).
    Colorless {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// Produce N mana of one chosen color from the provided set.
    AnyOneColor {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
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
    },
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
                Ok(ManaProduction::Fixed { colors })
            }
            serde_json::Value::Object(_) => {
                // New tagged format: {"type": "Fixed", "colors": [...]}
                #[derive(serde::Deserialize)]
                #[serde(tag = "type")]
                enum ManaProductionHelper {
                    Fixed {
                        #[serde(default)]
                        colors: Vec<ManaColor>,
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
                    },
                }
                let helper: ManaProductionHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                Ok(match helper {
                    ManaProductionHelper::Fixed { colors } => ManaProduction::Fixed { colors },
                    ManaProductionHelper::Colorless { count } => {
                        ManaProduction::Colorless { count }
                    }
                    ManaProductionHelper::AnyOneColor {
                        count,
                        color_options,
                    } => ManaProduction::AnyOneColor {
                        count,
                        color_options,
                    },
                    ManaProductionHelper::AnyCombination {
                        count,
                        color_options,
                    } => ManaProduction::AnyCombination {
                        count,
                        color_options,
                    },
                    ManaProductionHelper::ChosenColor { count } => {
                        ManaProduction::ChosenColor { count }
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

/// Duration for temporary effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Duration {
    UntilEndOfTurn,
    /// CR 514.2: Effect expires at end of combat phase.
    UntilEndOfCombat,
    UntilYourNextTurn,
    UntilHostLeavesPlay,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum GameRestriction {
    /// CR 614.16: Damage prevention effects are suppressed.
    DamagePreventionDisabled {
        source: ObjectId,
        expiry: RestrictionExpiry,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scope: Option<RestrictionScope>,
    },
}

/// When a game restriction expires.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum RestrictionExpiry {
    EndOfTurn,
    EndOfCombat,
}

/// Limits the scope of a game restriction to specific sources or targets.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "data")]
pub enum RestrictionScope {
    SourcesControlledBy(PlayerId),
    SpecificSource(ObjectId),
    DamageToTarget(ObjectId),
}

// ---------------------------------------------------------------------------
// Casting permissions — per-object casting grants
// ---------------------------------------------------------------------------

/// A permission granted to a `GameObject` allowing it to be cast under specific conditions.
/// Stored in `GameObject::casting_permissions`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum CastingPermission {
    /// CR 715.5: After Adventure resolves to exile, creature face castable from exile.
    AdventureCreature,
    /// Card may be cast from exile for the specified cost by its owner.
    /// Building block for Airbending, Foretell, Suspend, and similar "cast from exile" mechanics.
    ExileWithAltCost { cost: ManaCost },
    /// CR 400.7i: Play from exile until duration expires (impulse draw).
    /// Building block for "exile top N, choose one, you may play it this turn" patterns.
    PlayFromExile { duration: Duration },
    /// CR 122.3: Cast from exile by paying {E} equal to the card's mana value.
    /// Building block for Amped Raptor and similar energy-based casting mechanics.
    ExileWithEnergyCost,
    /// CR 702.185a: Warp — card may be cast from exile at its normal mana cost,
    /// but only after the specified turn ends. Persists for as long as card remains exiled.
    WarpExile { castable_after_turn: u32 },
}

/// When a delayed triggered ability fires (CR 603.7).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Building block for Earthbending return trigger and similar mechanics.
    WhenDiesOrExiled {
        object_id: super::identifiers::ObjectId,
    },
    /// CR 603.7c: "Whenever [event] this turn" — fires each time the event occurs
    /// until end of turn. Reuses existing trigger matching infrastructure via embedded
    /// TriggerDefinition. The embedded trigger's `execute` field should be `None` —
    /// the actual effect lives in `DelayedTrigger.ability`.
    WheneverEvent { trigger: Box<TriggerDefinition> },
}

/// Specifies variable-count targeting for "any number of" effects.
/// CR 601.2c: Player chooses targets during resolution.
/// CR 115.1d: "Any number" means zero or more.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct MultiTargetSpec {
    pub min: usize,
    /// `None` means "any number" (unlimited). CR 115.1d.
    pub max: Option<usize>,
}

// ---------------------------------------------------------------------------
// TargetFilter -- replaces TargetSpec entirely
// ---------------------------------------------------------------------------

/// Type filter for card type matching in filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize, JsonSchema)]
pub enum DamageKindFilter {
    /// Matches both combat and noncombat damage.
    #[default]
    Any,
    /// CR 120.1a: Only combat damage.
    CombatOnly,
    /// CR 120.1b: Only noncombat damage.
    NoncombatOnly,
}

/// Controller reference for filter matching.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum ControllerRef {
    You,
    Opponent,
}

/// Individual filter properties that can be combined in a Typed filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum FilterProp {
    Token,
    Attacking,
    /// CR 509.1h: Matches attacking creatures with no blockers assigned.
    Unblocked,
    Tapped,
    /// CR 302.6 / CR 110.5: Untapped status as targeting qualifier.
    Untapped,
    WithKeyword {
        value: String,
    },
    CountersGE {
        counter_type: String,
        count: u32,
    },
    /// Matches objects with converted mana cost >= N (for "mana value N or greater").
    /// CR 202.3: Uses QuantityExpr to support both fixed and dynamic comparisons.
    CmcGE {
        value: QuantityExpr,
    },
    /// Matches objects with converted mana cost <= N (for "mana value N or less").
    /// CR 202.3: Uses QuantityExpr to support both fixed and dynamic comparisons.
    CmcLE {
        value: QuantityExpr,
    },
    InZone {
        zone: Zone,
    },
    Owned {
        controller: ControllerRef,
    },
    EnchantedBy,
    EquippedBy,
    /// Matches any object that is NOT the trigger source (for "another creature" triggers).
    Another,
    /// Matches objects with a specific color (for "white creature", "red spell", etc.).
    HasColor {
        color: String,
    },
    /// Matches objects with power <= N (for "creature with power 2 or less").
    PowerLE {
        value: i32,
    },
    /// Matches objects with power >= N (for "creature with power 3 or greater").
    PowerGE {
        value: i32,
    },
    /// Matches multicolored objects (2+ colors).
    Multicolored,
    /// Matches objects with a specific supertype (Basic, Legendary, Snow).
    HasSupertype {
        value: String,
    },
    /// Matches objects whose subtypes include the source object's chosen creature type.
    /// Used for "of the chosen type" patterns (Cavern of Souls, Metallic Mimic).
    IsChosenCreatureType,
    /// CR 115.7: Matches stack entries that have exactly one target.
    /// Used for "with a single target" qualifiers on retarget effects.
    HasSingleTarget,
    /// CR 205.4b: Matches objects that do NOT have a specific color.
    /// Parallel to `HasColor` — used for "nonblack", "nonwhite" in negation stacks.
    NotColor {
        color: String,
    },
    /// CR 205.4a: Matches objects that do NOT have a specific supertype.
    /// Parallel to `HasSupertype` — used for "nonbasic", "nonlegendary" in negation stacks.
    NotSupertype {
        value: String,
    },
    /// CR 702.157a: Matches suspected creatures.
    Suspected,
    /// CR 510.1c: Matches creatures whose toughness is greater than their power.
    ToughnessGTPower,
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
    /// CR 700.5: Multi-target group constraint — all selected targets must share at least
    /// one value of the named quality. Validated at resolution time, not per-object.
    /// Examples: "that share a creature type", "that share a color", "that share a card type".
    SharesQuality {
        quality: String,
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
    /// Matches objects with converted mana cost == N (for "with mana value N" exact match).
    /// CR 202.3: Uses QuantityExpr to support both fixed and dynamic comparisons.
    CmcEQ {
        value: QuantityExpr,
    },
    /// Matches objects with the same name as a previously-referenced card.
    /// Used for "search your library for a card with that name" patterns.
    SameName,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

impl From<TypedFilter> for TargetFilter {
    fn from(f: TypedFilter) -> Self {
        TargetFilter::Typed(f)
    }
}

/// Typed target filter replacing all Forge filter strings and TargetSpec.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// CR 114.1a: Used by "becomes the target of a spell" triggers to filter source type.
    StackSpell,
    /// Matches a specific permanent by ObjectId.
    /// Used for duration-based statics that target a specific object
    /// (e.g., "that permanent loses all abilities for as long as ~").
    SpecificObject {
        id: ObjectId,
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
    /// CR 506.3d: Resolves to the player being attacked by the source creature.
    /// Looked up from `state.combat.attackers` using the trigger's source_id.
    DefendingPlayer,
}

/// A dynamic game quantity — a runtime lookup into the game state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum QuantityRef {
    /// Number of cards in the controller's hand.
    HandSize,
    /// Controller's current life total.
    LifeTotal,
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
    /// Count of players matching a player-level filter.
    /// Used for "for each opponent who lost life this turn" and similar patterns.
    PlayerCount { filter: PlayerFilter },
    /// Count of counters of a given type on the source object.
    /// Used for "for each [counter type] counter on ~" patterns.
    CountersOnSelf { counter_type: String },
    /// A variable reference (e.g. "X") resolved from spell payment or "that much" from prior effect.
    Variable { name: String },
    /// CR 208.3: The current power of the source object (post-layer).
    SelfPower,
    /// CR 208.3: The current toughness of the source object (post-layer).
    SelfToughness,
    /// CR 107.3e: Aggregate query (max/min/sum) over a property of battlefield objects.
    Aggregate {
        function: AggregateFunction,
        property: ObjectProperty,
        filter: TargetFilter,
    },
    /// The power of the targeted permanent. Used for "equal to target's power".
    TargetPower,
    /// CR 119.3 + CR 107.2: The life total of the targeted player.
    TargetLifeTotal,
    /// CR 700.5: Devotion to one or more colors.
    Devotion { colors: Vec<ManaColor> },
    /// CR 604.3: Count distinct card types (CoreType) across graveyards.
    /// Scope controls which players' graveyards are counted.
    /// Tarmogoyf: scope=All. "card types in your graveyard": scope=Controller.
    CardTypesInGraveyards { scope: CountScope },
    /// CR 604.3: Count cards in a zone matching optional type filters.
    /// Empty card_types means all cards. Multiple entries = OR (any match).
    /// "creature cards in your graveyard" → zone=Graveyard, card_types=[Creature], scope=Controller
    ZoneCardCount {
        zone: ZoneRef,
        card_types: Vec<TypeFilter>,
        scope: CountScope,
    },
    /// CR 305.6: Count distinct basic land types (Plains/Island/Swamp/Mountain/Forest)
    /// among lands the controller controls. Used by Domain.
    BasicLandTypeCount,
    /// CR 609.3: Count of objects moved by the preceding effect in the sub_ability chain.
    /// Only valid during sub-ability chain resolution; returns 0 outside that context.
    /// The caller (token resolver) is responsible for consuming the tracked set after use.
    TrackedSetSize,
    /// CR 118.4: Amount of life the controller has lost this turn.
    /// Used for "as long as you've lost life this turn" static conditions.
    LifeLostThisTurn,
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
    /// CR 117.1: Number of spells cast this turn by a specific player,
    /// optionally filtered by type. `None` = all spells.
    /// Resolved against the controller (or scope_player in per-player iteration).
    SpellsCastThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TypeFilter>,
    },
    /// Count of permanents matching filter that entered the battlefield
    /// under the controller's control this turn.
    EnteredThisTurn { filter: TargetFilter },
    /// CR 710.2: Number of crimes the controller has committed this turn.
    CrimesCommittedThisTurn,
    /// Amount of life the controller has gained this turn.
    LifeGainedThisTurn,
}

/// CR 107.2: Rounding direction for "half X" expressions in Magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum RoundingMode {
    Up,
    Down,
}

/// CR 107.3e: Aggregate function applied over a set of objects.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum AggregateFunction {
    Max,
    Min,
    Sum,
}

/// A measurable property of a game object for aggregate queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum ObjectProperty {
    Power,
    Toughness,
    ManaValue,
}

/// A filter matching players by game-state conditions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum PlayerFilter {
    /// All opponents of the controller.
    Opponent,
    /// Each opponent who lost life this turn (life_lost_this_turn > 0).
    OpponentLostLife,
    /// Each opponent who gained life this turn (life_gained_this_turn > 0).
    OpponentGainedLife,
    /// All players.
    All,
}

/// An expression that produces an integer for quantity comparisons.
/// Either a dynamic game-state lookup or a literal constant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum QuantityExpr {
    /// A dynamic quantity looked up from the current game state.
    Ref { qty: QuantityRef },
    /// A literal integer constant.
    Fixed { value: i32 },
    /// CR 107.2: "Half X, rounded up/down" — divides the inner expression by 2.
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
}

/// Comparison operator used in static conditions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Comparator {
    GT,
    LT,
    GE,
    LE,
    EQ,
}

impl Comparator {
    pub fn evaluate(self, lhs: i32, rhs: i32) -> bool {
        match self {
            Comparator::GT => lhs > rhs,
            Comparator::LT => lhs < rhs,
            Comparator::GE => lhs >= rhs,
            Comparator::LE => lhs <= rhs,
            Comparator::EQ => lhs == rhs,
        }
    }
}

/// CR 719.1: Condition that must be met for a Case to become solved.
/// Evaluated by the auto-solve trigger at end step (CR 719.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

/// Condition for static ability applicability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// True when ALL sub-conditions are satisfied.
    And {
        conditions: Vec<StaticCondition>,
    },
    /// True when ANY sub-condition is satisfied.
    Or {
        conditions: Vec<StaticCondition>,
    },
    /// CR 122.1: True when the source object has at least `minimum` (and at most `maximum`,
    /// if specified) counters of the given type. Used for level-up ranges (CR 710.3).
    HasCounters {
        counter_type: String,
        minimum: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        maximum: Option<u32>,
    },
    /// CR 716.6: True when the source Class enchantment is at or above the given level.
    /// Class level is a dedicated field (not a counter), so proliferate does not interact.
    ClassLevelGE {
        level: u8,
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
    /// CR 611.2b: True when the source object is tapped.
    /// Used for "for as long as ~ remains tapped" duration conditions.
    SourceIsTapped,
    /// CR 113.6b: True when the source card is in the specified zone.
    /// Used for "as long as ~ is in your graveyard" / "this card is in your graveyard" conditions.
    SourceInZone {
        zone: crate::types::zones::Zone,
    },
    None,
}

// ---------------------------------------------------------------------------
// PaymentCost — cost paid during effect resolution (not activation)
// ---------------------------------------------------------------------------

/// CR 118.1: A cost paid as part of an effect's resolution.
/// Distinct from AbilityCost (which gates activation before the colon).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum PaymentCost {
    Mana { cost: ManaCost },
    Life { amount: u32 },
}

// ---------------------------------------------------------------------------
// AbilityCost -- expanded typed variants
// ---------------------------------------------------------------------------

/// CR 702.49: Ninjutsu-family keyword variants that share the "swap creature in combat" pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum NinjutsuVariant {
    /// CR 702.49a: Return unblocked attacker, declare blockers or later.
    Ninjutsu,
    /// CR 702.49d: Commander ninjutsu — activate from hand or command zone.
    CommanderNinjutsu,
    /// CR 702.49 variant: Return unblocked attacker, declare blockers step only.
    Sneak,
    /// CR 702.49 variant: Return any tapped creature you control.
    WebSlinging,
}

/// CR 702.49: Identifies which dedicated engine path handles a RuntimeHandled ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum RuntimeHandler {
    /// Handled by GameAction::ActivateNinjutsu path.
    NinjutsuFamily,
}

/// Cost to activate an ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    },
    PayLife {
        amount: u32,
    },
    Discard {
        count: u32,
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
    /// "[cost A] or [cost B]" — player must pay exactly one.
    /// Choosing the first cost sets `additional_cost_paid = true`.
    Choice(AbilityCost, AbilityCost),
    /// Mandatory additional cost (e.g., "As an additional cost, waterbend {5}").
    Required(AbilityCost),
}

/// Structured spell-casting options parsed from Oracle text.
/// These describe alternate ways a spell may be cast; runtime enforcement can
/// be added independently of parsing/export support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct SpellCastingOption {
    pub kind: SpellCastingOptionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<AbilityCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<String>,
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

    pub fn condition(mut self, condition: impl Into<String>) -> Self {
        self.condition = Some(condition.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// CR 702.21a: Sacrifice a permanent as ward cost (e.g., "Ward—Sacrifice a permanent")
    SacrificeAPermanent,
}

/// CR 118.12: "Effect unless [player] pays {cost}"
/// Wraps any effect with an opponent payment choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DamageSource {
    /// The first resolved object target is the damage source.
    Target,
}

/// The typed effect enum. Each variant corresponds to an effect handler.
/// Zero HashMap<String, String> fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, strum::IntoStaticStr)]
#[serde(tag = "type")]
pub enum Effect {
    DealDamage {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 120.3: Override damage source. None = ability source (default).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        damage_source: Option<DamageSource>,
    },
    Draw {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
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
        /// CR 303.7: When a Role token or Aura token is created "attached to" a
        /// target, this field captures that attachment target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attach_to: Option<TargetFilter>,
        /// CR 508.4: Token enters the battlefield attacking (not declared as attacker).
        #[serde(default)]
        enters_attacking: bool,
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
    },
    Tap {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Untap {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    AddCounter {
        counter_type: String,
        #[serde(default = "default_one_i32")]
        count: i32,
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
    },
    Scry {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    PumpAll {
        #[serde(default = "default_pt_value_zero")]
        power: PtValue,
        #[serde(default = "default_pt_value_zero")]
        toughness: PtValue,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    DamageAll {
        amount: QuantityExpr,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
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
        /// CR 711.8: When true, the object enters the battlefield showing its back face.
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
        #[serde(default = "default_one")]
        count: u32,
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
    },
    GainControl {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Attach {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Surveil {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
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
    Explore,
    /// CR 702.136: Investigate — create a Clue artifact token.
    Investigate,
    /// CR 722: Become the monarch. Sets GameState::monarch to the controller.
    BecomeMonarch,
    Proliferate,
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
    },
    /// CR 707.2 / CR 613.1a: Become a copy of target permanent.
    /// Sets copiable characteristics at Layer 1.
    BecomeCopy {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<Duration>,
    },
    ChooseCard {
        #[serde(default)]
        choices: Vec<String>,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    PutCounter {
        counter_type: String,
        #[serde(default = "default_one_i32")]
        count: i32,
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
    /// CR 121.5: Put counters from source onto target.
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
        /// Whether this animation is an earthbending effect (emits GameEvent::Earthbend).
        /// Mirrors how grant_permission.rs uses ExileWithAltCost to detect airbending.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_earthbend: bool,
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
        /// When set, produced mana persists beyond normal phase-transition drains
        /// until the specified expiry condition is met (e.g., EndOfCombat for firebending).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        expiry: Option<crate::types::mana::ManaExpiry>,
    },
    Discard {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
        /// How many cards to find (usually 1).
        #[serde(default = "default_one")]
        count: u32,
        /// Whether to reveal the found card(s) to all players.
        #[serde(default)]
        reveal: bool,
    },
    RevealHand {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default = "default_target_filter_any")]
        card_filter: TargetFilter,
        /// None = reveal entire hand. Some = reveal this many cards. CR 701.20a.
        #[serde(default)]
        count: Option<QuantityExpr>,
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
    /// CR 702.157a: Suspect target creature — it gains menace and "can't block."
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
    /// CR 509.1g: Target creature must block this turn if able.
    ForceBlock {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 719.2: Solve the source Case — it becomes solved.
    SolveCase,
    /// CR 716.5: Set the class level on the source Class enchantment.
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
    /// CR 614.16: Apply a game-level restriction (e.g., disable damage prevention).
    AddRestriction {
        restriction: GameRestriction,
    },
    /// CR 114.1: Create an emblem with the specified static abilities in the command zone.
    /// Emblems persist for the rest of the game and cannot be removed.
    CreateEmblem {
        #[serde(default)]
        statics: Vec<StaticDefinition>,
    },
    /// CR 118.1: Pay a cost during effect resolution (mana or life).
    PayCost {
        cost: PaymentCost,
    },
    /// CR 601.2a + CR 118.9: Cast or play a card from a zone.
    /// Grants `ExileWithAltCost` casting permission on target cards (Discover pattern).
    CastFromZone {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default)]
        without_paying_mana_cost: bool,
        /// CR 601.2 vs CR 305.1: Cast (spells only) vs Play (spells + lands).
        #[serde(default)]
        mode: CardPlayMode,
    },
    /// CR 615: Prevent damage to a target.
    PreventDamage {
        amount: PreventionAmount,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default)]
        scope: PreventionScope,
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
    /// CR 705: Flip coins until you lose a flip, then execute effect with win count.
    FlipCoinUntilLose {
        win_effect: Box<AbilityDefinition>,
    },
    /// CR 701.54a: The Ring tempts the controller. Increments ring level and prompts
    /// ring-bearer selection if the controller has creatures on the battlefield.
    RingTemptsYou,
    /// Grant a casting permission to the target object (e.g., "cast from exile for {2}").
    /// Building block for Airbending, Foretell, Suspend, Hideaway, and similar mechanics.
    GrantCastingPermission {
        permission: CastingPermission,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
    },
    /// CR 702.110b: Exploit — sacrifice a creature you control (optional).
    /// The controller may sacrifice any creature they control, including the exploiter itself.
    Exploit {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 122.1: Gain energy counters. Amount is the number of {E} symbols in the Oracle text.
    GainEnergy {
        amount: u32,
    },
    /// CR 122.1: Give player counters (poison, experience, rad, ticket, etc.).
    /// Uses string-based counter kind for extensibility, paralleling CounterType::Generic(String).
    /// Poison counters have dedicated SBA rules (CR 104.3d / CR 122.1f).
    GivePlayerCounter {
        counter_kind: String,
        count: QuantityExpr,
        target: TargetFilter,
    },
    /// CR 702.84a: Exile cards from the top of your library one at a time until you
    /// exile a card matching the filter. The hit card is passed to the sub_ability chain
    /// as an injected target.
    ExileFromTopUntil {
        filter: TargetFilter,
    },
    /// CR 701.57a: Discover N — exile from top until nonland with MV ≤ N,
    /// cast free or put to hand, rest to bottom in random order.
    Discover {
        mana_value_limit: u32,
    },
    /// CR 701.24g: Put a card at a specific position in its owner's library.
    /// Unlike ChangeZone { destination: Library } which auto-shuffles (CR 401.3),
    /// this uses move_to_library_position for precise placement without shuffling.
    PutAtLibraryPosition {
        target: TargetFilter,
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
    /// goading player's next turn (UntilYourNextTurn).
    Goad {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 701.12a: Exchange control of two target permanents. Both targets come from
    /// ability.targets (two TargetRef::Object entries). If both have the same controller,
    /// the exchange does nothing (CR 701.12b). All-or-nothing semantics.
    ExchangeControl,
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

fn default_zone_hand() -> Zone {
    Zone::Hand
}

fn default_pt_value_zero() -> PtValue {
    PtValue::Fixed(0)
}

fn default_mana_production() -> ManaProduction {
    ManaProduction::Fixed { colors: Vec::new() }
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

/// Returns the human-readable variant name for an Effect.
/// Production API for GameEvent::EffectResolved api_type strings and logging.
pub fn effect_variant_name(effect: &Effect) -> &str {
    match effect {
        Effect::DealDamage { .. } => "DealDamage",
        Effect::Draw { .. } => "Draw",
        Effect::Pump { .. } => "Pump",
        Effect::Destroy { .. } => "Destroy",
        Effect::Regenerate { .. } => "Regenerate",
        Effect::Counter { .. } => "Counter",
        Effect::Token { .. } => "Token",
        Effect::GainLife { .. } => "GainLife",
        Effect::LoseLife { .. } => "LoseLife",
        Effect::Tap { .. } => "Tap",
        Effect::Untap { .. } => "Untap",
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
        Effect::Attach { .. } => "Attach",
        Effect::Surveil { .. } => "Surveil",
        Effect::Fight { .. } => "Fight",
        Effect::Bounce { .. } => "Bounce",
        Effect::Explore => "Explore",
        Effect::Investigate => "Investigate",
        Effect::BecomeMonarch => "BecomeMonarch",
        Effect::Proliferate => "Proliferate",
        Effect::CopySpell { .. } => "CopySpell",
        Effect::CopyTokenOf { .. } => "CopyTokenOf",
        Effect::BecomeCopy { .. } => "BecomeCopy",
        Effect::ChooseCard { .. } => "ChooseCard",
        Effect::PutCounter { .. } => "PutCounter",
        Effect::MultiplyCounter { .. } => "MultiplyCounter",
        Effect::DoublePT { .. } => "DoublePT",
        Effect::DoublePTAll { .. } => "DoublePTAll",
        Effect::MoveCounters { .. } => "MoveCounters",
        Effect::Animate { .. } => "Animate",
        Effect::GenericEffect { .. } => "Effect",
        Effect::Cleanup { .. } => "Cleanup",
        Effect::Mana { .. } => "Mana",
        Effect::Discard { .. } => "Discard",
        Effect::Shuffle { .. } => "Shuffle",
        Effect::Transform { .. } => "Transform",
        Effect::SearchLibrary { .. } => "SearchLibrary",
        Effect::RevealHand { .. } => "RevealHand",
        Effect::RevealTop { .. } => "RevealTop",
        Effect::TargetOnly { .. } => "TargetOnly",
        Effect::Choose { .. } => "Choose",
        Effect::Suspect { .. } => "Suspect",
        Effect::Connive { .. } => "Connive",
        Effect::PhaseOut { .. } => "PhaseOut",
        Effect::ForceBlock { .. } => "ForceBlock",
        Effect::SolveCase => "SolveCase",
        Effect::SetClassLevel { .. } => "SetClassLevel",
        Effect::CreateDelayedTrigger { .. } => "CreateDelayedTrigger",
        Effect::AddRestriction { .. } => "AddRestriction",
        Effect::CreateEmblem { .. } => "CreateEmblem",
        Effect::PayCost { .. } => "PayCost",
        Effect::CastFromZone { .. } => "CastFromZone",
        Effect::PreventDamage { .. } => "PreventDamage",
        Effect::LoseTheGame => "LoseTheGame",
        Effect::WinTheGame => "WinTheGame",
        Effect::RollDie { .. } => "RollDie",
        Effect::FlipCoin { .. } => "FlipCoin",
        Effect::FlipCoinUntilLose { .. } => "FlipCoinUntilLose",
        Effect::RingTemptsYou => "RingTemptsYou",
        Effect::GrantCastingPermission { .. } => "GrantCastingPermission",
        Effect::ChooseFromZone { .. } => "ChooseFromZone",
        Effect::Exploit { .. } => "Exploit",
        Effect::GainEnergy { .. } => "GainEnergy",
        Effect::GivePlayerCounter { .. } => "GivePlayerCounter",
        Effect::ExileFromTopUntil { .. } => "ExileFromTopUntil",
        Effect::Discover { .. } => "Discover",
        Effect::PutAtLibraryPosition { .. } => "PutAtLibraryPosition",
        Effect::PutOnTopOrBottom { .. } => "PutOnTopOrBottom",
        Effect::GiftDelivery { .. } => "GiftDelivery",
        Effect::Goad { .. } => "Goad",
        Effect::ExchangeControl => "ExchangeControl",
        Effect::ChangeTargets { .. } => "ChangeTargets",
        Effect::Amass { .. } => "Amass",
        Effect::Monstrosity { .. } => "Monstrosity",
        Effect::ManifestDread => "ManifestDread",
        Effect::ExtraTurn { .. } => "ExtraTurn",
        Effect::AdditionalCombatPhase { .. } => "AdditionalCombatPhase",
        Effect::Double { .. } => "Double",
        Effect::RuntimeHandled { handler } => match handler {
            RuntimeHandler::NinjutsuFamily => "RuntimeHandled:NinjutsuFamily",
        },
        Effect::Forage => "Forage",
        Effect::CollectEvidence { .. } => "CollectEvidence",
        Effect::Endure { .. } => "Endure",
        Effect::BlightEffect { .. } => "BlightEffect",
        Effect::Seek { .. } => "Seek",
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum EffectKind {
    DealDamage,
    Draw,
    Pump,
    Destroy,
    Counter,
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
    ChangeZone,
    ChangeZoneAll,
    Dig,
    GainControl,
    Attach,
    AttachAll,
    Surveil,
    Fight,
    Bounce,
    Explore,
    Investigate,
    BecomeMonarch,
    Proliferate,
    CopySpell,
    CopyTokenOf,
    BecomeCopy,
    ChooseCard,
    PutCounter,
    MultiplyCounter,
    DoublePT,
    DoublePTAll,
    MoveCounters,
    Animate,
    GenericEffect,
    Cleanup,
    Mana,
    Discard,
    Shuffle,
    SearchLibrary,
    TargetOnly,
    Choose,
    Suspect,
    Connive,
    PhaseOut,
    ForceBlock,
    SolveCase,
    SetClassLevel,
    CreateDelayedTrigger,
    AddRestriction,
    CreateEmblem,
    PayCost,
    CastFromZone,
    PreventDamage,
    Regenerate,
    LoseTheGame,
    WinTheGame,
    RollDie,
    FlipCoin,
    FlipCoinUntilLose,
    RingTemptsYou,
    GrantCastingPermission,
    ChooseFromZone,
    Exploit,
    GainEnergy,
    GivePlayerCounter,
    ExileFromTopUntil,
    Discover,
    PutAtLibraryPosition,
    PutOnTopOrBottom,
    GiftDelivery,
    Goad,
    ExchangeControl,
    ChangeTargets,
    Amass,
    Monstrosity,
    ManifestDread,
    ExtraTurn,
    AdditionalCombatPhase,
    Double,
    RuntimeHandled,
    Forage,
    CollectEvidence,
    Endure,
    BlightEffect,
    Seek,
    Unimplemented,
    /// Engine-level equip action (not via an Effect handler).
    Equip,
    /// Trigger-condition placeholders — emitters not yet implemented.
    Reveal,
    Transform,
    TurnFaceUp,
    DayTimeChange,
}

impl From<&Effect> for EffectKind {
    fn from(effect: &Effect) -> Self {
        match effect {
            Effect::DealDamage { .. } => EffectKind::DealDamage,
            Effect::Draw { .. } => EffectKind::Draw,
            Effect::Pump { .. } => EffectKind::Pump,
            Effect::Destroy { .. } => EffectKind::Destroy,
            Effect::Regenerate { .. } => EffectKind::Regenerate,
            Effect::Counter { .. } => EffectKind::Counter,
            Effect::Token { .. } => EffectKind::Token,
            Effect::GainLife { .. } => EffectKind::GainLife,
            Effect::LoseLife { .. } => EffectKind::LoseLife,
            Effect::Tap { .. } => EffectKind::Tap,
            Effect::Untap { .. } => EffectKind::Untap,
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
            Effect::Attach { .. } => EffectKind::Attach,
            Effect::Surveil { .. } => EffectKind::Surveil,
            Effect::Fight { .. } => EffectKind::Fight,
            Effect::Bounce { .. } => EffectKind::Bounce,
            Effect::Explore => EffectKind::Explore,
            Effect::Investigate => EffectKind::Investigate,
            Effect::BecomeMonarch => EffectKind::BecomeMonarch,
            Effect::Proliferate => EffectKind::Proliferate,
            Effect::CopySpell { .. } => EffectKind::CopySpell,
            Effect::CopyTokenOf { .. } => EffectKind::CopyTokenOf,
            Effect::BecomeCopy { .. } => EffectKind::BecomeCopy,
            Effect::ChooseCard { .. } => EffectKind::ChooseCard,
            Effect::PutCounter { .. } => EffectKind::PutCounter,
            Effect::MultiplyCounter { .. } => EffectKind::MultiplyCounter,
            Effect::DoublePT { .. } => EffectKind::DoublePT,
            Effect::DoublePTAll { .. } => EffectKind::DoublePTAll,
            Effect::MoveCounters { .. } => EffectKind::MoveCounters,
            Effect::Animate { .. } => EffectKind::Animate,
            Effect::GenericEffect { .. } => EffectKind::GenericEffect,
            Effect::Cleanup { .. } => EffectKind::Cleanup,
            Effect::Mana { .. } => EffectKind::Mana,
            Effect::Discard { .. } => EffectKind::Discard,
            Effect::Shuffle { .. } => EffectKind::Shuffle,
            Effect::Transform { .. } => EffectKind::Transform,
            Effect::SearchLibrary { .. } => EffectKind::SearchLibrary,
            Effect::RevealHand { .. } => EffectKind::Reveal,
            Effect::RevealTop { .. } => EffectKind::Reveal,
            Effect::TargetOnly { .. } => EffectKind::TargetOnly,
            Effect::Choose { .. } => EffectKind::Choose,
            Effect::Suspect { .. } => EffectKind::Suspect,
            Effect::Connive { .. } => EffectKind::Connive,
            Effect::PhaseOut { .. } => EffectKind::PhaseOut,
            Effect::ForceBlock { .. } => EffectKind::ForceBlock,
            Effect::SolveCase => EffectKind::SolveCase,
            Effect::SetClassLevel { .. } => EffectKind::SetClassLevel,
            Effect::CreateDelayedTrigger { .. } => EffectKind::CreateDelayedTrigger,
            Effect::AddRestriction { .. } => EffectKind::AddRestriction,
            Effect::CreateEmblem { .. } => EffectKind::CreateEmblem,
            Effect::PayCost { .. } => EffectKind::PayCost,
            Effect::CastFromZone { .. } => EffectKind::CastFromZone,
            Effect::PreventDamage { .. } => EffectKind::PreventDamage,
            Effect::LoseTheGame => EffectKind::LoseTheGame,
            Effect::WinTheGame => EffectKind::WinTheGame,
            Effect::RollDie { .. } => EffectKind::RollDie,
            Effect::FlipCoin { .. } => EffectKind::FlipCoin,
            Effect::FlipCoinUntilLose { .. } => EffectKind::FlipCoinUntilLose,
            Effect::RingTemptsYou => EffectKind::RingTemptsYou,
            Effect::GrantCastingPermission { .. } => EffectKind::GrantCastingPermission,
            Effect::ChooseFromZone { .. } => EffectKind::ChooseFromZone,
            Effect::Exploit { .. } => EffectKind::Exploit,
            Effect::GainEnergy { .. } => EffectKind::GainEnergy,
            Effect::GivePlayerCounter { .. } => EffectKind::GivePlayerCounter,
            Effect::ExileFromTopUntil { .. } => EffectKind::ExileFromTopUntil,
            Effect::Discover { .. } => EffectKind::Discover,
            Effect::PutAtLibraryPosition { .. } => EffectKind::PutAtLibraryPosition,
            Effect::PutOnTopOrBottom { .. } => EffectKind::PutOnTopOrBottom,
            Effect::GiftDelivery { .. } => EffectKind::GiftDelivery,
            Effect::Goad { .. } => EffectKind::Goad,
            Effect::ExchangeControl => EffectKind::ExchangeControl,
            Effect::ChangeTargets { .. } => EffectKind::ChangeTargets,
            Effect::Amass { .. } => EffectKind::Amass,
            Effect::Monstrosity { .. } => EffectKind::Monstrosity,
            Effect::ManifestDread => EffectKind::ManifestDread,
            Effect::ExtraTurn { .. } => EffectKind::ExtraTurn,
            Effect::AdditionalCombatPhase { .. } => EffectKind::AdditionalCombatPhase,
            Effect::Double { .. } => EffectKind::Double,
            Effect::RuntimeHandled { .. } => EffectKind::RuntimeHandled,
            Effect::Forage => EffectKind::Forage,
            Effect::CollectEvidence { .. } => EffectKind::CollectEvidence,
            Effect::Endure { .. } => EffectKind::Endure,
            Effect::BlightEffect { .. } => EffectKind::BlightEffect,
            Effect::Seek { .. } => EffectKind::Seek,
            Effect::Unimplemented { .. } => EffectKind::Unimplemented,
        }
    }
}

// ---------------------------------------------------------------------------
// Ability kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize, JsonSchema)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

/// Selection constraints attached to a modal choice header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ModalSelectionConstraint {
    DifferentTargetPlayers,
    /// CR 700.2: Each mode may only be chosen once per turn for this source.
    /// Oracle text: "choose one that hasn't been chosen this turn"
    NoRepeatThisTurn,
    /// CR 700.2: Each mode may only be chosen once total for this source.
    /// Oracle text: "choose one that hasn't been chosen"
    NoRepeatThisGame,
}

/// Structured activation-time restrictions parsed from Oracle text.
/// These describe when an activated ability may be activated; runtime
/// enforcement can be added independently of parsing/export support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
        text: String,
    },
    /// CR 719.4: This ability can only be activated while the source Case is solved.
    IsSolved,
    /// CR 716.4: Level N+1 ability can only activate when the source Class is at exactly this level.
    ClassLevelIs {
        level: u8,
    },
}

/// Structured spell-casting restrictions parsed from Oracle text.
/// These describe when a spell may be cast. Runtime enforcement can
/// be added independently of parsing/export support.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    RequiresCondition { text: String },
}

/// CR 601.2f: Self-referential cost reduction on an activated ability.
/// "This ability costs {N} less to activate for each [condition]"
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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

// Thread-local recursion depth counter for AbilityDefinition's Debug impl.
// Prevents stack overflow when formatting deeply nested ability trees (Effect ↔
// AbilityDefinition recursion). At depth > 2, recursive children are summarized
// as `AbilityDefinition { effect: <VariantName>, .. }` instead of fully expanded.
std::thread_local! {
    static ABILITY_DEBUG_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

const ABILITY_DEBUG_MAX_DEPTH: u32 = 2;

impl fmt::Debug for AbilityDefinition {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let depth = ABILITY_DEBUG_DEPTH.with(|d| {
            let current = d.get();
            d.set(current + 1);
            current
        });

        let result = if depth >= ABILITY_DEBUG_MAX_DEPTH {
            // Summarize: effect variant name + key flags, no recursive expansion.
            let variant: &'static str = self.effect.as_ref().into();
            write!(f, "AbilityDefinition {{ effect: {variant}")?;
            if self.sub_ability.is_some() {
                write!(f, ", sub_ability: Some(..)")?;
            }
            if self.else_ability.is_some() {
                write!(f, ", else_ability: Some(..)")?;
            }
            if self.condition.is_some() {
                write!(f, ", condition: Some(..)")?;
            }
            write!(f, ", .. }}")
        } else {
            // Full debug output — delegate to a debug_struct for all fields
            f.debug_struct("AbilityDefinition")
                .field("kind", &self.kind)
                .field("effect", &self.effect)
                .field("cost", &self.cost)
                .field("sub_ability", &self.sub_ability)
                .field("else_ability", &self.else_ability)
                .field("duration", &self.duration)
                .field("description", &self.description)
                .field("target_prompt", &self.target_prompt)
                .field("sorcery_speed", &self.sorcery_speed)
                .field("activation_restrictions", &self.activation_restrictions)
                .field("activation_zone", &self.activation_zone)
                .field("condition", &self.condition)
                .field("optional_targeting", &self.optional_targeting)
                .field("optional", &self.optional)
                .field("optional_for", &self.optional_for)
                .field("multi_target", &self.multi_target)
                .field("distribute", &self.distribute)
                .field("modal", &self.modal)
                .field("mode_abilities", &self.mode_abilities)
                .field("repeat_for", &self.repeat_for)
                .field("cost_reduction", &self.cost_reduction)
                .field("forward_result", &self.forward_result)
                .field("player_scope", &self.player_scope)
                .finish()
        };

        ABILITY_DEBUG_DEPTH.with(|d| d.set(depth));
        result
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

    pub fn sorcery_speed(mut self) -> Self {
        self.sorcery_speed = true;
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum AbilityCondition {
    /// CR 702.32b: Kicker — optional additional cost; paid/unpaid state stored in SpellContext.
    AdditionalCostPaid,
    /// CR 608.2e: "Instead" clause — replaces the parent effect when the additional cost was paid.
    /// The resolver swaps the override sub's effect in place of the parent before resolution.
    AdditionalCostPaidInstead,
    /// Negated additional cost: sub_ability executes only when the cost was NOT paid.
    /// Used by Gift "if the gift wasn't promised" pattern.
    AdditionalCostNotPaid,
    /// CR 608.2c: "If you do" — sub_ability executes only if the parent optional effect was performed.
    IfYouDo,
    /// CR 603.12: "When you do" — reflexive trigger that always fires when the parent
    /// (non-optional) effect was performed. Unlike `IfYouDo` which gates on
    /// `optional_effect_performed`, this is unconditionally true for non-optional parents.
    WhenYouDo,
    /// CR 603.4: "If you cast it from [zone]" — sub_ability executes only if the spell
    /// was cast from the specified zone. Evaluated against SpellContext.cast_from_zone.
    CastFromZone { zone: Zone },
    /// CR 608.2c: "If it's a [type] card" — gates sub_ability on the last revealed card's type.
    /// Evaluated at resolution time by inspecting `state.last_revealed_ids[0]`.
    /// `negated` handles "if it's a nonland card" patterns.
    RevealedHasCardType { card_type: CoreType, negated: bool },
    /// CR 400.7 + CR 608.2c: True when the source permanent did NOT enter the battlefield
    /// this turn. Used for "unless ~ entered this turn" exemptions (e.g., Moon-Circuit Hacker).
    SourceDidNotEnterThisTurn,
    /// CR 702.49 + CR 603.4: True when the source permanent entered via a ninjutsu-family
    /// activation of the specified variant this turn.
    NinjutsuVariantPaid { variant: NinjutsuVariant },
    /// CR 608.2e + CR 702.49: "Instead" override gated on the source permanent having
    /// entered via a ninjutsu-family variant this turn. Unlike AdditionalCostPaidInstead
    /// (which reads SpellContext.additional_cost_paid), this reads
    /// GameObject.ninjutsu_variant_paid from the game state.
    NinjutsuVariantPaidInstead { variant: NinjutsuVariant },
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
    /// CR 608.2e: "If [target] has [keyword], [override effect] instead"
    /// Checked at resolution time against the first resolved object target's keywords.
    /// Uses "Instead" override semantics: swaps the parent effect when condition is met.
    TargetHasKeywordInstead { keyword: Keyword },
}

/// Casting-time facts that flow with a spell from casting through resolution.
/// Conditions in the sub_ability chain are evaluated against this context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SpellContext {
    /// Whether the spell's optional additional cost was paid during casting.
    #[serde(default)]
    pub additional_cost_paid: bool,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TriggerCondition {
    // -- Predicates (leaf conditions) --
    /// "if you gained life this turn" / "if you've gained N or more life this turn"
    GainedLife { minimum: u32 },
    /// "if you lost life this turn"
    LostLife,
    /// "if you descended this turn" (a permanent card was put into your graveyard)
    Descended,
    /// "if you control N or more creatures"
    ControlCreatures { minimum: u32 },
    /// "if you control a [type]" — general control presence check.
    ControlsType { filter: TargetFilter },
    /// CR 603.4: "if no spells were cast last turn" — werewolf transform condition.
    NoSpellsCastLastTurn,
    /// CR 603.4: "if two or more spells were cast last turn" — werewolf reverse transform.
    TwoOrMoreSpellsCastLastTurn,
    /// CR 603.4: "if it's not your turn" / "if it isn't your turn"
    NotYourTurn,
    /// CR 508.1a: "Whenever ~ and at least N other creatures attack."
    /// True when combat is active and at least `minimum` other creatures
    /// controlled by the same player are also attacking.
    MinCoAttackers { minimum: u32 },
    /// CR 719.2: Intervening-if for Case auto-solve.
    /// True when the source Case is unsolved AND its solve condition is met.
    SolveConditionMet,
    /// CR 716.6: True when the source Class enchantment is at or above the given level.
    /// Used to gate continuous triggers that only become active at higher class levels.
    ClassLevelGE { level: u8 },

    /// "if you cast it" — zoneless cast check (unlike CastFromZone which requires a specific zone).
    /// CR 701.57a: Used by Discover ETB triggers.
    WasCast,

    /// "if it's attacking" — true when the trigger source object is currently an attacker.
    /// CR 508.1: Used by ninjutsu ETB triggers (e.g., Thousand-Faced Shadow).
    SourceIsAttacking,

    /// CR 702.49 + CR 603.4: "if its sneak/ninjutsu cost was paid this turn" — true when
    /// the source permanent entered via the specified ninjutsu-family variant this turn.
    NinjutsuVariantPaid { variant: NinjutsuVariant },

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

    // -- Combinators --
    /// All conditions must be true ("if you gained and lost life this turn")
    And { conditions: Vec<TriggerCondition> },
    /// Any condition must be true
    Or { conditions: Vec<TriggerCondition> },
}

/// Condition that gates whether a replacement effect applies.
/// Checked when determining if the replacement is a candidate for an event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// "unless a player has N or less life"
    /// CR 614.1d — Bond lands (Abandoned Campground, etc.)
    UnlessPlayerLifeAtMost { amount: u32 },
    /// "unless you have two or more opponents"
    /// CR 614.1d — Battlebond lands (Luxury Suite, etc.)
    UnlessMultipleOpponents,
    /// "unless you revealed a [type] card" / "unless you paid {mana}"
    /// CR 614.1d — Generic condition text that the engine does not yet decompose further.
    /// Using this variant lets the replacement be recognized for coverage while deferring
    /// the condition evaluation.
    Unrecognized { text: String },
}

/// Rate-limiting constraint for triggered abilities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
        filter: Option<TypedFilter>,
    },
    /// "Whenever you draw your Nth card each turn" — fires exactly when
    /// the controller's `cards_drawn_this_turn` equals `n`.
    NthDrawThisTurn { n: u32 },
    /// "At the beginning of each opponent's [phase]"
    OnlyDuringOpponentsTurn,
    /// CR 716.5: "When this Class becomes level N" — fire only at the specified level.
    AtClassLevel { level: u8 },
}

/// Filter for counter-related trigger modes (CounterAdded, CounterRemoved).
/// When set, the trigger only matches events for the specified counter type,
/// optionally requiring that the count crosses a threshold.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct CounterTriggerFilter {
    /// Only match events for this counter type.
    pub counter_type: crate::game::game_object::CounterType,
    /// If set, only fire when the count crosses this threshold:
    /// previous_count < threshold <= new_count.
    /// Used by Saga chapter triggers (CR 714.2a).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threshold: Option<u32>,
}

/// Trigger definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerDefinition {
    pub mode: TriggerMode,
    #[serde(default)]
    pub execute: Option<Box<AbilityDefinition>>,
    #[serde(default)]
    pub valid_card: Option<TargetFilter>,
    #[serde(default)]
    pub origin: Option<Zone>,
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
}

impl TriggerDefinition {
    pub fn new(mode: TriggerMode) -> Self {
        Self {
            mode,
            execute: None,
            valid_card: None,
            origin: None,
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
}

/// Static ability definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StaticDefinition {
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum DamageModification {
    /// amount * 2 (e.g. Furnace of Rath)
    Double,
    /// amount * 3 (e.g. Fiery Emancipation)
    Triple,
    /// amount + value (e.g. Torbran, +2)
    Plus { value: u32 },
    /// amount.saturating_sub(value) (e.g. Benevolent Unicorn, -1)
    Minus { value: u32 },
}

/// CR 614.1a: Quantity modification for replacement effects (tokens, counters).
/// Modeled after DamageModification but for non-damage quantities.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum QuantityModification {
    /// count * 2 — Primal Vigor, Doubling Season, Parallel Lives, Anointed Procession
    Double,
    /// count + value — Hardened Scales (+1)
    Plus { value: u32 },
    /// count.saturating_sub(value) — Vizier of Remedies (-1)
    Minus { value: u32 },
}

/// CR 614.1a: Restricts which damage targets a replacement applies to.
/// Dedicated enum because `TargetRef` can be `Player` (not handled by `matches_target_filter`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DamageTargetFilter {
    /// "to an opponent or a permanent an opponent controls"
    OpponentOrTheirPermanents,
    /// "to a creature" / "to that creature"
    CreatureOnly,
    /// "to a player" / "to that player"
    PlayerOnly,
}

/// CR 614.1a: Restricts whether a damage replacement applies to combat, noncombat, or all damage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum CombatDamageScope {
    CombatOnly,
    NoncombatOnly,
}

/// Whether a replacement effect is mandatory or offers the affected player a choice.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
}

/// Replacement effect definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Marks this replacement as consumed (one-shot). Skipped by find_applicable_replacements.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_consumed: bool,
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
            is_consumed: false,
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
}

// ---------------------------------------------------------------------------
// ContinuousModification -- typed effect modifications for layers
// ---------------------------------------------------------------------------

/// What modification a continuous effect applies to an object.
/// Each variant knows its own layer implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ContinuousModification {
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
    /// CR 613.4c: Add dynamic +X to power (layer 7c), where X is computed at application time.
    AddDynamicPower {
        value: QuantityExpr,
    },
    /// CR 613.4c: Add dynamic +X to toughness (layer 7c), where X is computed at application time.
    AddDynamicToughness {
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
    /// CR 510.1c: This creature assigns combat damage equal to its toughness
    /// rather than its power.
    AssignDamageFromToughness,
    /// CR 613.2 (Layer 2): Change the controller of the affected object to the
    /// controller of the source permanent (e.g., Control Magic auras).
    ChangeController,
    /// CR 305.7: Sets a land's subtype to a basic land type, replacing old land
    /// subtypes and their associated mana abilities.
    SetBasicLandType {
        land_type: BasicLandType,
    },
}

// ---------------------------------------------------------------------------
// Target reference (unchanged)
// ---------------------------------------------------------------------------

/// Unified target reference for creatures and players.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum TargetRef {
    Object(ObjectId),
    Player(PlayerId),
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
            description: None,
            repeat_for: None,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
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
                },
            ))),
            valid_card: Some(TargetFilter::SelfRef),
            origin: Some(Zone::Battlefield),
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
            AbilityCost::PayLife { amount: 2 },
            AbilityCost::Discard {
                count: 1,
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
            },
            restrictions: vec![],
            expiry: None,
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
                },
                restrictions: vec![],
                expiry: None,
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
            Duration::UntilYourNextTurn,
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
            attach_to: None,
            enters_attacking: false,
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
            FilterProp::Unblocked,
            FilterProp::Tapped,
            FilterProp::Untapped,
            FilterProp::WithKeyword {
                value: "Flying".to_string(),
            },
            FilterProp::CountersGE {
                counter_type: "+1/+1".to_string(),
                count: 3,
            },
            FilterProp::CmcGE {
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

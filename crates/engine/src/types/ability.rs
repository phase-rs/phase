use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::card_type::CoreType;
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
    /// CR 701.15: Regeneration shield — consumed on use, expires at cleanup.
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

/// A dynamically computed power/toughness value, evaluated at layer application time.
/// Used by CDA static abilities (layer 7a) where P/T depends on game state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum DynamicPTValue {
    /// Count distinct card types among cards in all graveyards, plus a fixed offset.
    /// Tarmogoyf: power = count + 0, toughness = count + 1.
    CardTypesInAllGraveyards {
        #[serde(default)]
        offset: i32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", content = "value")]
pub enum DamageAmount {
    Fixed(i32),
    Variable(String),
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

/// Power/toughness value -- either a fixed integer or a variable reference (e.g. "*", "X").
///
/// Custom Deserialize: accepts both the tagged format `{"type":"Fixed","value":2}` (new)
/// and plain strings like `"2"` or `"*"` (legacy card-data.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", content = "value")]
pub enum PtValue {
    Fixed(i32),
    Variable(String),
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
                #[serde(tag = "type", content = "value")]
                enum PtValueHelper {
                    Fixed(i32),
                    Variable(String),
                }
                let helper: PtValueHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    PtValueHelper::Fixed(n) => Ok(PtValue::Fixed(n)),
                    PtValueHelper::Variable(s) => Ok(PtValue::Variable(s)),
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected string, number, or object for PtValue",
            )),
        }
    }
}

/// Token count value -- either a fixed integer or a variable reference (e.g. "X").
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
#[serde(tag = "type", content = "value")]
pub enum CountValue {
    Fixed(u32),
    Variable(String),
    /// CR 609.3: Count of objects moved by the preceding effect in the sub_ability chain.
    /// Resolves to the size of the most recent tracked set recorded by `resolve_ability_chain`.
    TrackedSetSize,
}

impl<'de> serde::Deserialize<'de> for CountValue {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        match &value {
            serde_json::Value::String(s) => match s.parse::<u32>() {
                Ok(n) => Ok(CountValue::Fixed(n)),
                Err(_) => Ok(CountValue::Variable(s.clone())),
            },
            serde_json::Value::Number(n) => Ok(CountValue::Fixed(n.as_u64().unwrap_or(0) as u32)),
            serde_json::Value::Object(_) => {
                #[derive(serde::Deserialize)]
                #[serde(tag = "type", content = "value")]
                enum CountValueHelper {
                    Fixed(u32),
                    Variable(String),
                    TrackedSetSize,
                }
                let helper: CountValueHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    CountValueHelper::Fixed(n) => Ok(CountValue::Fixed(n)),
                    CountValueHelper::Variable(s) => Ok(CountValue::Variable(s)),
                    CountValueHelper::TrackedSetSize => Ok(CountValue::TrackedSetSize),
                }
            }
            _ => Err(serde::de::Error::custom(
                "expected string, number, or object for CountValue",
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
        #[serde(default = "default_count_value_one")]
        count: CountValue,
    },
    /// Produce N mana of one chosen color from the provided set.
    AnyOneColor {
        #[serde(default = "default_count_value_one")]
        count: CountValue,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
    },
    /// Produce N mana where each unit can be chosen independently from the provided set.
    AnyCombination {
        #[serde(default = "default_count_value_one")]
        count: CountValue,
        #[serde(default = "default_all_mana_colors")]
        color_options: Vec<ManaColor>,
    },
    /// Produce N mana of a previously chosen color.
    ChosenColor {
        #[serde(default = "default_count_value_one")]
        count: CountValue,
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
                        #[serde(default = "default_count_value_one")]
                        count: CountValue,
                    },
                    AnyOneColor {
                        #[serde(default = "default_count_value_one")]
                        count: CountValue,
                        #[serde(default = "default_all_mana_colors")]
                        color_options: Vec<ManaColor>,
                    },
                    AnyCombination {
                        #[serde(default = "default_count_value_one")]
                        count: CountValue,
                        #[serde(default = "default_all_mana_colors")]
                        color_options: Vec<ManaColor>,
                    },
                    ChosenColor {
                        #[serde(default = "default_count_value_one")]
                        count: CountValue,
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
}

/// Duration for temporary effects.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub enum Duration {
    UntilEndOfTurn,
    UntilYourNextTurn,
    UntilHostLeavesPlay,
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
    Permanent,
    Card,
    Any,
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
    Tapped,
    NonType {
        value: String,
    },
    WithKeyword {
        value: String,
    },
    CountersGE {
        counter_type: String,
        count: u32,
    },
    CmcGE {
        value: u32,
    },
    /// Matches objects with converted mana cost <= N (for "mana value N or less").
    CmcLE {
        value: u32,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TypedFilter {
    #[serde(default)]
    pub card_type: Option<TypeFilter>,
    #[serde(default)]
    pub subtype: Option<String>,
    #[serde(default)]
    pub controller: Option<ControllerRef>,
    #[serde(default)]
    pub properties: Vec<FilterProp>,
}

impl TypedFilter {
    pub fn new(card_type: TypeFilter) -> Self {
        Self {
            card_type: Some(card_type),
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
    pub fn controller(mut self, ctrl: ControllerRef) -> Self {
        self.controller = Some(ctrl);
        self
    }
    pub fn subtype(mut self, sub: String) -> Self {
        self.subtype = Some(sub);
        self
    }
    pub fn properties(mut self, props: Vec<FilterProp>) -> Self {
        self.properties = props;
        self
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
    /// The power of the targeted permanent. Used for "equal to target's power".
    TargetPower,
    /// CR 119.3 + CR 107.2: The life total of the targeted player.
    TargetLifeTotal,
}

/// CR 107.2: Rounding direction for "half X" expressions in Magic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum RoundingMode {
    Up,
    Down,
}

/// A filter matching players by game-state conditions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// CR 122.1: True when the source object has at least `minimum` counters of the given type.
    HasCounters {
        counter_type: String,
        minimum: u32,
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
    /// CR 701.52: True when this creature is the ring-bearer for its controller.
    IsRingBearer,
    /// CR 701.52: True when the controller's ring level is at least this value (0-indexed).
    RingLevelAtLeast {
        level: u8,
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
    /// CR 702.49a: Ninjutsu compound cost — pay mana and return an unblocked attacker.
    /// The return-attacker part is implicit in the ActivateNinjutsu action (player selects which).
    Ninjutsu {
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
// Effect enum -- typed variants, zero HashMap
// ---------------------------------------------------------------------------

/// The typed effect enum. Each variant corresponds to an effect handler.
/// Zero HashMap<String, String> fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum Effect {
    DealDamage {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
        /// CR 701.15: When true, the destroyed permanent cannot be regenerated.
        #[serde(default)]
        cant_regenerate: bool,
    },
    /// CR 701.15: Create a regeneration shield on the target permanent.
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
        unless_payment: Option<ManaCost>,
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
        #[serde(default = "default_count_value_one")]
        count: CountValue,
        /// CR 303.7: When a Role token or Aura token is created "attached to" a
        /// target, this field captures that attachment target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attach_to: Option<TargetFilter>,
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
        #[serde(default = "default_one")]
        count: u32,
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
        amount: DamageAmount,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    DestroyAll {
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
        /// CR 701.15: When true, destroyed permanents cannot be regenerated.
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
    },
    ChangeZoneAll {
        #[serde(default)]
        origin: Option<Zone>,
        destination: Zone,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
    },
    Dig {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default)]
        destination: Option<Zone>,
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
        #[serde(default = "default_one")]
        count: u32,
    },
    Fight {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
    },
    Discard {
        #[serde(default = "default_one")]
        count: u32,
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
    },
    /// CR 701.16a: Reveal the top N card(s) of a player's library.
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
    /// CR 702.162a: Target creature connives (draw a card, then discard a card;
    /// if a nonland card is discarded, put a +1/+1 counter on it).
    Connive {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
    /// Stub: parsed but not resolved. Runtime behavior is future work.
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
    /// CR 701.52: The Ring tempts the controller. Increments ring level and prompts
    /// ring-bearer selection if the controller has creatures on the battlefield.
    RingTemptsYou,
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

fn default_pt_value_zero() -> PtValue {
    PtValue::Fixed(0)
}

fn default_count_value_one() -> CountValue {
    CountValue::Fixed(1)
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
        Effect::BecomeCopy { .. } => "BecomeCopy",
        Effect::ChooseCard { .. } => "ChooseCard",
        Effect::PutCounter { .. } => "PutCounter",
        Effect::MultiplyCounter { .. } => "MultiplyCounter",
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
    BecomeCopy,
    ChooseCard,
    PutCounter,
    MultiplyCounter,
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
            Effect::BecomeCopy { .. } => EffectKind::BecomeCopy,
            Effect::ChooseCard { .. } => EffectKind::ChooseCard,
            Effect::PutCounter { .. } => EffectKind::PutCounter,
            Effect::MultiplyCounter { .. } => EffectKind::MultiplyCounter,
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

// ---------------------------------------------------------------------------
// Definition types -- fully typed, zero HashMap
// ---------------------------------------------------------------------------

/// Parsed ability definition with typed effect. Zero remaining_params.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AbilityDefinition {
    pub kind: AbilityKind,
    pub effect: Effect,
    #[serde(default)]
    pub cost: Option<AbilityCost>,
    #[serde(default)]
    pub sub_ability: Option<Box<AbilityDefinition>>,
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
    /// Condition that must be met for this ability to execute during resolution.
    #[serde(default)]
    pub condition: Option<AbilityCondition>,
    /// When true, targeting is optional ("up to one"). Player may choose zero targets.
    #[serde(default)]
    pub optional_targeting: bool,
    /// CR 609.3: When true, the controller chooses whether to perform this effect ("You may X").
    #[serde(default)]
    pub optional: bool,
    /// Variable-count targeting: min/max targets the player can choose.
    /// When present, resolution enters MultiTargetSelection instead of immediate resolve.
    /// CR 601.2c + CR 115.1d.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub multi_target: Option<MultiTargetSpec>,
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
}

impl AbilityDefinition {
    /// Create a new `AbilityDefinition` with only the required fields; all optional
    /// fields default to `None` / `false`.
    pub fn new(kind: AbilityKind, effect: Effect) -> Self {
        Self {
            kind,
            effect,
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
            activation_restrictions: Vec::new(),
            condition: None,
            optional_targeting: false,
            optional: false,
            multi_target: None,
            modal: None,
            mode_abilities: Vec::new(),
            repeat_for: None,
        }
    }

    pub fn multi_target(mut self, spec: MultiTargetSpec) -> Self {
        self.multi_target = Some(spec);
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
    /// CR 608.2c: "If you do" — sub_ability executes only if the parent optional effect was performed.
    IfYouDo,
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
    /// CR 719.2: Intervening-if for Case auto-solve.
    /// True when the source Case is unsolved AND its solve condition is met.
    SolveConditionMet,
    /// CR 716.6: True when the source Class enchantment is at or above the given level.
    /// Used to gate continuous triggers that only become active at higher class levels.
    ClassLevelGE { level: u8 },

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
    /// "Whenever you cast your Nth spell each turn" — fires exactly when
    /// the controller's `spells_cast_this_turn` equals `n`.
    NthSpellThisTurn { n: u32 },
    /// "Whenever you draw your Nth card each turn" — fires exactly when
    /// the controller's `cards_drawn_this_turn` equals `n`.
    NthDrawThisTurn { n: u32 },
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
    #[serde(default)]
    pub combat_damage: bool,
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
            combat_damage: false,
            secondary: false,
            valid_target: None,
            valid_source: None,
            description: None,
            constraint: None,
            condition: None,
            counter_filter: None,
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

    pub fn combat_damage(mut self) -> Self {
        self.combat_damage = true;
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

    /// CR 701.15: Mark this replacement as a regeneration shield (one-shot, expires at cleanup).
    pub fn regeneration_shield(mut self) -> Self {
        self.shield_kind = ShieldKind::Regeneration;
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
        value: DynamicPTValue,
    },
    /// Set toughness to a dynamically computed value (CDA, layer 7a).
    SetDynamicToughness {
        value: DynamicPTValue,
    },
    /// Grants every creature type (Changeling CDA). Expanded at runtime
    /// using `GameState::all_creature_types`.
    AddAllCreatureTypes,
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
    /// CR 609.3: Repeat this ability N times (from "for each [X], [effect]").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_for: Option<QuantityExpr>,
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
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            repeat_for: None,
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
            combat_damage: false,
            secondary: false,
            valid_target: None,
            valid_source: None,
            description: Some("When ~ dies, draw a card.".to_string()),
            constraint: None,
            condition: None,
            counter_filter: None,
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
    fn count_value_roundtrip() {
        let values = vec![
            CountValue::Fixed(3),
            CountValue::Variable("X".to_string()),
            CountValue::Variable("the number of creatures you control".to_string()),
            CountValue::TrackedSetSize,
        ];
        let json = serde_json::to_string(&values).unwrap();
        let deserialized: Vec<CountValue> = serde_json::from_str(&json).unwrap();
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
            count: CountValue::Variable("the number of creatures you control".to_string()),
            attach_to: None,
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
            FilterProp::Tapped,
            FilterProp::NonType {
                value: "Land".to_string(),
            },
            FilterProp::WithKeyword {
                value: "Flying".to_string(),
            },
            FilterProp::CountersGE {
                counter_type: "+1/+1".to_string(),
                count: 3,
            },
            FilterProp::CmcGE { value: 4 },
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
            FilterProp::Owned {
                controller: ControllerRef::Opponent,
            },
            FilterProp::EnchantedBy,
            FilterProp::EquippedBy,
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

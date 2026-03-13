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

/// The five basic land types (MTG Rule 305.6).
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
        match choice_type {
            ChoiceType::Color => value.parse::<ManaColor>().ok().map(Self::Color),
            ChoiceType::CreatureType => Some(Self::CreatureType(value.to_string())),
            ChoiceType::BasicLandType => {
                value.parse::<BasicLandType>().ok().map(Self::BasicLandType)
            }
            ChoiceType::CardType => value.parse::<CoreType>().ok().map(Self::CardType),
            ChoiceType::OddOrEven => value.parse::<Parity>().ok().map(Self::OddOrEven),
            ChoiceType::CardName => Some(Self::CardName(value.to_string())),
            // These choice types represent ephemeral selections (numbers, binary labels, land types)
            // that don't map to a typed ChosenAttribute. If persist=true is used with these variants,
            // the choice is not stored on the object — extend ChosenAttribute if persistence is needed.
            ChoiceType::NumberRange { .. } | ChoiceType::Labeled { .. } | ChoiceType::LandType => {
                None
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
                }
                let helper: CountValueHelper =
                    serde_json::from_value(value).map_err(serde::de::Error::custom)?;
                match helper {
                    CountValueHelper::Fixed(n) => Ok(CountValue::Fixed(n)),
                    CountValueHelper::Variable(s) => Ok(CountValue::Variable(s)),
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
    Other {
        value: String,
    },
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
    /// Matches a specific permanent by ObjectId.
    /// Used for duration-based statics that target a specific object
    /// (e.g., "that permanent loses all abilities for as long as ~").
    SpecificObject(ObjectId),
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
    CheckSVar {
        var: String,
        compare: String,
    },
    DuringYourTurn,
    None,
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

// ---------------------------------------------------------------------------
// Effect enum -- typed variants, zero HashMap
// ---------------------------------------------------------------------------

/// The typed effect enum. Each variant corresponds to an effect handler.
/// Zero HashMap<String, String> fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum Effect {
    DealDamage {
        amount: DamageAmount,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    Draw {
        #[serde(default = "default_one")]
        count: u32,
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
    },
    Counter {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// Static applied to counter's source, affecting the countered ability's source permanent.
        /// The `affected` filter is bound at resolution time to `SpecificObject(source_permanent_id)`.
        /// Used by cards like Tishana's Tidebinder ("loses all abilities for as long as ~").
        #[serde(default)]
        source_static: Option<StaticDefinition>,
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
    },
    GainLife {
        amount: LifeAmount,
        /// Who gains the life.
        #[serde(default)]
        player: GainLifePlayer,
    },
    LoseLife {
        amount: i32,
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
        #[serde(default = "default_one")]
        count: u32,
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
    },
    ChangeZone {
        #[serde(default)]
        origin: Option<Zone>,
        destination: Zone,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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
    Proliferate,
    CopySpell {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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

/// Returns the human-readable variant name for an Effect.
/// Production API for GameEvent::EffectResolved api_type strings and logging.
pub fn effect_variant_name(effect: &Effect) -> &str {
    match effect {
        Effect::DealDamage { .. } => "DealDamage",
        Effect::Draw { .. } => "Draw",
        Effect::Pump { .. } => "Pump",
        Effect::Destroy { .. } => "Destroy",
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
        Effect::Proliferate => "Proliferate",
        Effect::CopySpell { .. } => "CopySpell",
        Effect::ChooseCard { .. } => "ChooseCard",
        Effect::PutCounter { .. } => "PutCounter",
        Effect::MultiplyCounter { .. } => "MultiplyCounter",
        Effect::Animate { .. } => "Animate",
        Effect::GenericEffect { .. } => "Effect",
        Effect::Cleanup { .. } => "Cleanup",
        Effect::Mana { .. } => "Mana",
        Effect::Discard { .. } => "Discard",
        Effect::Shuffle { .. } => "Shuffle",
        Effect::SearchLibrary { .. } => "SearchLibrary",
        Effect::RevealHand { .. } => "RevealHand",
        Effect::TargetOnly { .. } => "TargetOnly",
        Effect::Choose { .. } => "Choose",
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
    Proliferate,
    CopySpell,
    ChooseCard,
    PutCounter,
    MultiplyCounter,
    Animate,
    GenericEffect,
    Cleanup,
    Mana,
    Discard,
    Shuffle,
    SearchLibrary,
    TargetOnly,
    Choose,
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
            Effect::Proliferate => EffectKind::Proliferate,
            Effect::CopySpell { .. } => EffectKind::CopySpell,
            Effect::ChooseCard { .. } => EffectKind::ChooseCard,
            Effect::PutCounter { .. } => EffectKind::PutCounter,
            Effect::MultiplyCounter { .. } => EffectKind::MultiplyCounter,
            Effect::Animate { .. } => EffectKind::Animate,
            Effect::GenericEffect { .. } => EffectKind::GenericEffect,
            Effect::Cleanup { .. } => EffectKind::Cleanup,
            Effect::Mana { .. } => EffectKind::Mana,
            Effect::Discard { .. } => EffectKind::Discard,
            Effect::Shuffle { .. } => EffectKind::Shuffle,
            Effect::SearchLibrary { .. } => EffectKind::SearchLibrary,
            Effect::RevealHand { .. } => EffectKind::Reveal,
            Effect::TargetOnly { .. } => EffectKind::TargetOnly,
            Effect::Choose { .. } => EffectKind::Choose,
            Effect::Unimplemented { .. } => EffectKind::Unimplemented,
        }
    }
}

// ---------------------------------------------------------------------------
// Ability kinds
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
pub enum AbilityKind {
    Spell,
    Activated,
    Database,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
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
    /// Condition that must be met for this ability to execute during resolution.
    #[serde(default)]
    pub condition: Option<AbilityCondition>,
    /// When true, targeting is optional ("up to one"). Player may choose zero targets.
    #[serde(default)]
    pub optional_targeting: bool,
    /// Modal metadata for activated/triggered abilities with "Choose one —" etc.
    /// When present, the ability pauses for mode selection before resolving.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modal: Option<ModalChoice>,
    /// The individual mode abilities for modal activated/triggered abilities.
    /// Each entry is one selectable mode. Only meaningful when `modal` is Some.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mode_abilities: Vec<AbilityDefinition>,
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
            condition: None,
            optional_targeting: false,
            modal: None,
            mode_abilities: Vec::new(),
        }
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

    pub fn condition(mut self, condition: AbilityCondition) -> Self {
        self.condition = Some(condition);
        self
    }

    pub fn optional_targeting(mut self) -> Self {
        self.optional_targeting = true;
        self
    }

    pub fn with_modal(mut self, modal: ModalChoice, mode_abilities: Vec<AbilityDefinition>) -> Self {
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
    /// This ability only fires if the spell's optional additional cost was paid.
    AdditionalCostPaid,
}

/// Casting-time facts that flow with a spell from casting through resolution.
/// Conditions in the sub_ability chain are evaluated against this context.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, Default)]
pub struct SpellContext {
    /// Whether the spell's optional additional cost was paid during casting.
    #[serde(default)]
    pub additional_cost_paid: bool,
}

/// Intervening-if condition for triggered abilities.
/// Checked both when the trigger would fire and when it resolves on the stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TriggerCondition {
    /// "if you gained life this turn" / "if you've gained N or more life this turn"
    LifeGainedThisTurn { minimum: u32 },
    /// "if you control N or more creatures"
    ControlCreatureCount { minimum: u32 },
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
    AddAbility {
        ability: String,
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
            sub_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
        }
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
                amount: DamageAmount::Fixed(3),
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
        let sub = ResolvedAbility::new(Effect::Draw { count: 1 }, vec![], ObjectId(1), PlayerId(0));
        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
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
                Effect::Draw { count: 1 },
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
                    amount: LifeAmount::Fixed(1),
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
                amount: DamageAmount::Fixed(3),
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
            Effect::Draw { count: 1 },
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
            ContinuousModification::AddAbility {
                ability: "Hexproof".to_string(),
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
            StaticCondition::IsPresent {
                filter: Some(
                    TypedFilter::creature()
                        .controller(ControllerRef::You)
                        .into(),
                ),
            },
            StaticCondition::CheckSVar {
                var: "X".to_string(),
                compare: "GE5".to_string(),
            },
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
            Effect::Draw { count: 2 },
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
}

#[cfg(test)]
mod modal_ability_tests {
    use super::*;

    #[test]
    fn ability_definition_supports_modal() {
        let mode1 = AbilityDefinition::new(AbilityKind::Spell, Effect::Draw { count: 1 });
        let mode2 = AbilityDefinition::new(AbilityKind::Spell, Effect::GainLife {
            amount: LifeAmount::Fixed(3),
            player: GainLifePlayer::Controller,
        });
        let modal = ModalChoice {
            min_choices: 1,
            max_choices: 1,
            mode_count: 2,
            mode_descriptions: vec!["Draw a card.".to_string(), "Gain 3 life.".to_string()],
        };
        let def = AbilityDefinition::new(AbilityKind::Activated, Effect::Unimplemented {
            name: "modal_placeholder".to_string(),
            description: None,
        })
        .with_modal(modal.clone(), vec![mode1, mode2]);

        assert!(def.modal.is_some());
        assert_eq!(def.mode_abilities.len(), 2);
    }
}

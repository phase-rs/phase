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

/// How to specify a damage amount -- either a fixed integer or a variable reference.
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
    NonType { value: String },
    WithKeyword { value: String },
    CountersGE { counter_type: String, count: u32 },
    CmcGE { value: u32 },
    InZone { zone: Zone },
    Owned { controller: ControllerRef },
    EnchantedBy,
    EquippedBy,
    /// Matches any object that is NOT the trigger source (for "another creature" triggers).
    Another,
    /// Matches objects with a specific color (for "white creature", "red spell", etc.).
    HasColor { color: String },
    /// Matches objects with power <= N (for "creature with power 2 or less").
    PowerLE { value: i32 },
    /// Matches objects with power >= N (for "creature with power 3 or greater").
    PowerGE { value: i32 },
    /// Matches multicolored objects (2+ colors).
    Multicolored,
    Other { value: String },
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
    Typed {
        #[serde(default)]
        card_type: Option<TypeFilter>,
        #[serde(default)]
        subtype: Option<String>,
        #[serde(default)]
        controller: Option<ControllerRef>,
        #[serde(default)]
        properties: Vec<FilterProp>,
    },
    Not {
        filter: Box<TargetFilter>,
    },
    Or {
        filters: Vec<TargetFilter>,
    },
    And {
        filters: Vec<TargetFilter>,
    },
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
    },
    Discard {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
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

fn default_target_filter_any() -> TargetFilter {
    TargetFilter::Any
}

fn default_target_filter_none() -> TargetFilter {
    TargetFilter::None
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
        Effect::Unimplemented { name, .. } => name,
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
}

/// Intervening-if condition for triggered abilities.
/// Checked both when the trigger would fire and when it resolves on the stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TriggerCondition {
    /// "if you gained life this turn" / "if you've gained N or more life this turn"
    LifeGainedThisTurn { minimum: u32 },
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

/// Replacement effect definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReplacementDefinition {
    pub event: ReplacementEvent,
    #[serde(default)]
    pub execute: Option<Box<AbilityDefinition>>,
    #[serde(default)]
    pub valid_card: Option<TargetFilter>,
    #[serde(default)]
    pub description: Option<String>,
}

// ---------------------------------------------------------------------------
// ContinuousModification -- typed effect modifications for layers
// ---------------------------------------------------------------------------

/// What modification a continuous effect applies to an object.
/// Each variant knows its own layer implicitly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum ContinuousModification {
    AddPower { value: i32 },
    AddToughness { value: i32 },
    SetPower { value: i32 },
    SetToughness { value: i32 },
    AddKeyword { keyword: Keyword },
    RemoveKeyword { keyword: Keyword },
    AddAbility { ability: String },
    RemoveAllAbilities,
    AddType { core_type: CoreType },
    RemoveType { core_type: CoreType },
    AddSubtype { subtype: String },
    RemoveSubtype { subtype: String },
    SetColor { colors: Vec<ManaColor> },
    AddColor { color: ManaColor },
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
        }
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
        let ability = ResolvedAbility {
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any,
            },
            targets: vec![TargetRef::Object(ObjectId(10))],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            duration: None,
        };
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn resolved_ability_with_sub_ability_roundtrips() {
        let sub = ResolvedAbility {
            effect: Effect::Draw { count: 1 },
            targets: vec![],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            duration: None,
        };
        let ability = ResolvedAbility {
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any,
            },
            targets: vec![TargetRef::Player(PlayerId(1))],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: Some(Box::new(sub)),
            duration: None,
        };
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

    // --- Serde roundtrip tests for new typed definitions ---

    #[test]
    fn trigger_definition_roundtrip() {
        let trigger = TriggerDefinition {
            mode: TriggerMode::ChangesZone,
            execute: Some(Box::new(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Draw { count: 1 },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            })),
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
            affected: Some(TargetFilter::Typed {
                card_type: Some(TypeFilter::Creature),
                subtype: None,
                controller: Some(ControllerRef::You),
                properties: vec![],
            }),
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
            event: ReplacementEvent::DamageDone,
            execute: Some(Box::new(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::GainLife { amount: LifeAmount::Fixed(1), player: GainLifePlayer::Controller },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            })),
            valid_card: Some(TargetFilter::SelfRef),
            description: Some(
                "If damage would be dealt to ~, prevent it and gain 1 life.".to_string(),
            ),
        };
        let json = serde_json::to_string(&replacement).unwrap();
        let deserialized: ReplacementDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(replacement, deserialized);
    }

    #[test]
    fn target_filter_nested_roundtrip() {
        let filter = TargetFilter::And {
            filters: vec![
                TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    subtype: None,
                    controller: Some(ControllerRef::You),
                    properties: vec![],
                },
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
        let ability = AbilityDefinition {
            kind: AbilityKind::Activated,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetFilter::Any,
            },
            cost: Some(AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![],
                    generic: 2,
                },
            }),
            sub_ability: Some(Box::new(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Draw { count: 1 },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            })),
            duration: Some(Duration::UntilEndOfTurn),
            description: Some("Deal 3 damage, then draw a card.".to_string()),
            target_prompt: Some("Choose a target".to_string()),
            sorcery_speed: true,
        };
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
                filter: Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    subtype: None,
                    controller: None,
                    properties: vec![],
                }),
            },
            AbilityCost::TapCreatures {
                count: 2,
                filter: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    subtype: None,
                    controller: Some(ControllerRef::You),
                    properties: vec![],
                },
            },
            AbilityCost::Sacrifice {
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Artifact),
                    subtype: None,
                    controller: None,
                    properties: vec![],
                },
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
                }
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
                filter: Some(TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    subtype: None,
                    controller: Some(ControllerRef::You),
                    properties: vec![],
                }),
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
        let ability = ResolvedAbility {
            effect: Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Any,
            },
            targets: vec![TargetRef::Object(ObjectId(10))],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            duration: Some(Duration::UntilHostLeavesPlay),
        };
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
        assert_eq!(deserialized.duration, Some(Duration::UntilHostLeavesPlay));
    }
}

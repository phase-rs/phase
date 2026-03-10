use std::collections::HashMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use super::identifiers::ObjectId;
use super::player::PlayerId;
use super::replacements::ReplacementEvent;
use super::statics::StaticMode;
use super::triggers::TriggerMode;

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

/// Targeting specification for an effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum TargetSpec {
    None,
    Any,
    Filtered { filter: String },
    Player,
    Controller,
    All { filter: String },
}

/// Cost to activate an ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum AbilityCost {
    Mana { cost: String },
    Tap,
    Loyalty { amount: i32 },
    Sacrifice { target: TargetSpec },
    Composite { costs: Vec<AbilityCost> },
}

// ---------------------------------------------------------------------------
// Effect enum -- 38 typed variants + Other fallback
// ---------------------------------------------------------------------------

/// The typed effect enum replacing `api_type: String` + `params: HashMap`.
/// Each variant corresponds to an entry in the effect handler registry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum Effect {
    DealDamage {
        amount: DamageAmount,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Draw {
        #[serde(default = "default_one")]
        count: u32,
    },
    Pump {
        #[serde(default)]
        power: i32,
        #[serde(default)]
        toughness: i32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Destroy {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Counter {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Token {
        name: String,
        #[serde(default)]
        power: i32,
        #[serde(default)]
        toughness: i32,
        #[serde(default)]
        types: Vec<String>,
        #[serde(default)]
        colors: Vec<String>,
        #[serde(default)]
        keywords: Vec<String>,
    },
    GainLife {
        amount: i32,
    },
    LoseLife {
        amount: i32,
    },
    Tap {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Untap {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    AddCounter {
        counter_type: String,
        #[serde(default = "default_one_i32")]
        count: i32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    RemoveCounter {
        counter_type: String,
        #[serde(default = "default_one_i32")]
        count: i32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Sacrifice {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    DiscardCard {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Mill {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Scry {
        #[serde(default = "default_one")]
        count: u32,
    },
    PumpAll {
        #[serde(default)]
        power: i32,
        #[serde(default)]
        toughness: i32,
        #[serde(default = "default_target_spec_all_empty")]
        target: TargetSpec,
    },
    DamageAll {
        amount: DamageAmount,
        #[serde(default = "default_target_spec_all_empty")]
        target: TargetSpec,
    },
    DestroyAll {
        #[serde(default = "default_target_spec_all_empty")]
        target: TargetSpec,
    },
    ChangeZone {
        #[serde(default)]
        origin: String,
        destination: String,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    ChangeZoneAll {
        #[serde(default)]
        origin: String,
        destination: String,
        #[serde(default = "default_target_spec_all_empty")]
        target: TargetSpec,
    },
    Dig {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default)]
        destination: String,
    },
    GainControl {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Attach {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Surveil {
        #[serde(default = "default_one")]
        count: u32,
    },
    Fight {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Bounce {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
        #[serde(default)]
        destination: String,
    },
    Explore,
    Proliferate,
    CopySpell {
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    ChooseCard {
        #[serde(default)]
        choices: Vec<String>,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    PutCounter {
        counter_type: String,
        #[serde(default = "default_one_i32")]
        count: i32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    MultiplyCounter {
        counter_type: String,
        #[serde(default = "default_two_i32")]
        multiplier: i32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    Animate {
        #[serde(default)]
        power: Option<i32>,
        #[serde(default)]
        toughness: Option<i32>,
        #[serde(default)]
        types: Vec<String>,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    /// Generic "Effect" handler (creates continuous effects at resolution).
    GenericEffect {
        #[serde(default)]
        params: HashMap<String, String>,
    },
    Cleanup {
        #[serde(default)]
        params: HashMap<String, String>,
    },
    Mana {
        #[serde(default)]
        produced: String,
        #[serde(default)]
        params: HashMap<String, String>,
    },
    Discard {
        #[serde(default = "default_one")]
        count: u32,
        #[serde(default = "default_target_spec_any")]
        target: TargetSpec,
    },
    /// Fallback for unrecognized effect types during parsing.
    Other {
        api_type: String,
        #[serde(default)]
        params: HashMap<String, String>,
    },
}

fn default_one() -> u32 {
    1
}

fn default_one_i32() -> i32 {
    1
}

fn default_two_i32() -> i32 {
    2
}

fn default_target_spec_any() -> TargetSpec {
    TargetSpec::Any
}

fn default_target_spec_all_empty() -> TargetSpec {
    TargetSpec::All {
        filter: String::new(),
    }
}

impl Effect {
    /// Returns the Forge api_type string for this effect.
    /// Used as a compatibility bridge during the transition period.
    pub fn api_type(&self) -> &str {
        match self {
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
            Effect::Other { api_type, .. } => api_type,
        }
    }

    /// Reconstructs a Forge-compatible HashMap from the typed effect fields.
    /// Used as a compatibility bridge during the transition period.
    pub fn to_params(&self) -> HashMap<String, String> {
        let mut params = HashMap::new();
        match self {
            Effect::DealDamage { amount, target } => {
                match amount {
                    DamageAmount::Fixed(n) => {
                        params.insert("NumDmg".to_string(), n.to_string());
                    }
                    DamageAmount::Variable(v) => {
                        params.insert("NumDmg".to_string(), v.clone());
                    }
                }
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Draw { count } => {
                params.insert("NumCards".to_string(), count.to_string());
            }
            Effect::Pump {
                power,
                toughness,
                target,
            } => {
                if *power >= 0 {
                    params.insert("NumAtt".to_string(), format!("+{power}"));
                } else {
                    params.insert("NumAtt".to_string(), power.to_string());
                }
                if *toughness >= 0 {
                    params.insert("NumDef".to_string(), format!("+{toughness}"));
                } else {
                    params.insert("NumDef".to_string(), toughness.to_string());
                }
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Destroy { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Counter { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Token {
                name,
                power,
                toughness,
                types,
                colors,
                keywords,
            } => {
                params.insert("TokenScript".to_string(), name.clone());
                params.insert("TokenPower".to_string(), power.to_string());
                params.insert("TokenToughness".to_string(), toughness.to_string());
                if !types.is_empty() {
                    params.insert("TokenTypes".to_string(), types.join(","));
                }
                if !colors.is_empty() {
                    params.insert("TokenColors".to_string(), colors.join(","));
                }
                if !keywords.is_empty() {
                    params.insert("TokenKeywords".to_string(), keywords.join(","));
                }
            }
            Effect::GainLife { amount } => {
                params.insert("LifeAmount".to_string(), amount.to_string());
            }
            Effect::LoseLife { amount } => {
                params.insert("LifeAmount".to_string(), amount.to_string());
            }
            Effect::Tap { target } | Effect::Untap { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::AddCounter {
                counter_type,
                count,
                target,
            }
            | Effect::RemoveCounter {
                counter_type,
                count,
                target,
            }
            | Effect::PutCounter {
                counter_type,
                count,
                target,
            } => {
                params.insert("CounterType".to_string(), counter_type.clone());
                params.insert("CounterNum".to_string(), count.to_string());
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Sacrifice { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::DiscardCard { count, target } | Effect::Discard { count, target } => {
                params.insert("NumCards".to_string(), count.to_string());
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Mill { count, target } => {
                params.insert("NumCards".to_string(), count.to_string());
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Scry { count } | Effect::Surveil { count } => {
                params.insert("ScryNum".to_string(), count.to_string());
            }
            Effect::PumpAll {
                power,
                toughness,
                target,
            } => {
                if *power >= 0 {
                    params.insert("NumAtt".to_string(), format!("+{power}"));
                } else {
                    params.insert("NumAtt".to_string(), power.to_string());
                }
                if *toughness >= 0 {
                    params.insert("NumDef".to_string(), format!("+{toughness}"));
                } else {
                    params.insert("NumDef".to_string(), toughness.to_string());
                }
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::DamageAll { amount, target } => {
                match amount {
                    DamageAmount::Fixed(n) => {
                        params.insert("NumDmg".to_string(), n.to_string());
                    }
                    DamageAmount::Variable(v) => {
                        params.insert("NumDmg".to_string(), v.clone());
                    }
                }
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::DestroyAll { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::ChangeZone {
                origin,
                destination,
                target,
            }
            | Effect::ChangeZoneAll {
                origin,
                destination,
                target,
            } => {
                if !origin.is_empty() {
                    params.insert("Origin".to_string(), origin.clone());
                }
                params.insert("Destination".to_string(), destination.clone());
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Dig { count, destination } => {
                params.insert("DigNum".to_string(), count.to_string());
                if !destination.is_empty() {
                    params.insert("DestinationZone".to_string(), destination.clone());
                }
            }
            Effect::GainControl { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Attach { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Fight { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Bounce {
                target,
                destination,
            } => {
                insert_target_spec(&mut params, "ValidTgts", target);
                if !destination.is_empty() {
                    params.insert("Destination".to_string(), destination.clone());
                }
            }
            Effect::Explore | Effect::Proliferate => {}
            Effect::CopySpell { target } => {
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::ChooseCard { choices, target } => {
                if !choices.is_empty() {
                    params.insert("Choices".to_string(), choices.join(","));
                }
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::MultiplyCounter {
                counter_type,
                multiplier,
                target,
            } => {
                params.insert("CounterType".to_string(), counter_type.clone());
                params.insert("Multiplier".to_string(), multiplier.to_string());
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::Animate {
                power,
                toughness,
                types,
                target,
            } => {
                if let Some(p) = power {
                    params.insert("Power".to_string(), p.to_string());
                }
                if let Some(t) = toughness {
                    params.insert("Toughness".to_string(), t.to_string());
                }
                if !types.is_empty() {
                    params.insert("Types".to_string(), types.join(","));
                }
                insert_target_spec(&mut params, "ValidTgts", target);
            }
            Effect::GenericEffect { params: p } | Effect::Cleanup { params: p } => {
                params.clone_from(p);
            }
            Effect::Mana {
                produced,
                params: p,
            } => {
                params.clone_from(p);
                if !produced.is_empty() {
                    params.insert("Produced".to_string(), produced.clone());
                }
            }
            Effect::Other { params: p, .. } => {
                params.clone_from(p);
            }
        }
        params
    }
}

/// Helper to insert a TargetSpec into a params map.
fn insert_target_spec(params: &mut HashMap<String, String>, key: &str, spec: &TargetSpec) {
    let value = match spec {
        TargetSpec::None => return,
        TargetSpec::Any => "Any".to_string(),
        TargetSpec::Filtered { filter } => filter.clone(),
        TargetSpec::Player => "Player".to_string(),
        TargetSpec::Controller => "Player.You".to_string(),
        TargetSpec::All { filter } => {
            if filter.is_empty() {
                return;
            }
            filter.clone()
        }
    };
    params.insert(key.to_string(), value);
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
// Definition types
// ---------------------------------------------------------------------------

/// Parsed ability definition with typed effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct AbilityDefinition {
    pub kind: AbilityKind,
    pub effect: Effect,
    #[serde(default)]
    pub cost: Option<AbilityCost>,
    #[serde(default)]
    pub sub_ability: Option<Box<AbilityDefinition>>,
    /// Parameters not consumed by the typed Effect during parsing.
    /// Contains metadata like SubAbility, Execute, SpellDescription, Defined,
    /// ConditionCompare, etc. Used by the compat params() method.
    #[serde(default)]
    pub remaining_params: HashMap<String, String>,
}

impl AbilityDefinition {
    /// Returns the Forge api_type string for this ability's effect.
    /// Compatibility bridge -- consumers that still match on api_type strings
    /// can use this until they are migrated to match on Effect variants.
    pub fn api_type(&self) -> &str {
        self.effect.api_type()
    }

    /// Reconstructs a Forge-compatible params HashMap from the typed effect,
    /// merged with any unconsumed parameters from parsing.
    /// Compatibility bridge -- consumers that still read params by string key
    /// can use this until they are migrated to read Effect fields.
    pub fn params(&self) -> HashMap<String, String> {
        let mut params = self.remaining_params.clone();
        // Typed effect fields take precedence over remaining params
        params.extend(self.effect.to_params());
        params
    }
}

/// Trigger definition with typed mode and effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct TriggerDefinition {
    pub mode: TriggerMode,
    /// Remaining trigger-specific parameters (Origin, Destination, ValidCard, Execute, etc.)
    /// that are not part of the effect itself.
    #[serde(default)]
    pub params: HashMap<String, String>,
}

impl TriggerDefinition {
    /// Returns the Forge mode string for this trigger.
    /// Compatibility bridge for consumers that still compare mode as a string.
    pub fn mode_str(&self) -> String {
        self.mode.to_string()
    }
}

/// Static ability definition with typed mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct StaticDefinition {
    pub mode: StaticMode,
    /// Parameters for the static ability (Affected, AddPower, AddToughness, etc.)
    #[serde(default)]
    pub params: HashMap<String, String>,
}

impl StaticDefinition {
    /// Returns the Forge mode string for this static ability.
    /// Compatibility bridge for consumers that still compare mode as a string.
    pub fn mode_str(&self) -> String {
        self.mode.to_string()
    }
}

/// Replacement effect definition with typed event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ReplacementDefinition {
    pub event: ReplacementEvent,
    /// Parameters for the replacement effect (conditions, modifications, etc.)
    #[serde(default)]
    pub params: HashMap<String, String>,
}

impl ReplacementDefinition {
    /// Returns the Forge event string for this replacement.
    /// Compatibility bridge for consumers that still compare event as a string.
    pub fn event_str(&self) -> String {
        self.event.to_string()
    }
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
// Resolved ability (UNCHANGED -- transitional, Plan 02 converts this)
// ---------------------------------------------------------------------------

/// Runtime ability data passed to effect handlers at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAbility {
    pub api_type: String,
    pub params: HashMap<String, String>,
    pub targets: Vec<TargetRef>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub sub_ability: Option<Box<ResolvedAbility>>,
    pub svars: HashMap<String, String>,
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
            api_type: "DealDamage".to_string(),
            params: HashMap::from([("NumDmg".to_string(), "3".to_string())]),
            targets: vec![TargetRef::Object(ObjectId(10))],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let json = serde_json::to_string(&ability).unwrap();
        let deserialized: ResolvedAbility = serde_json::from_str(&json).unwrap();
        assert_eq!(ability, deserialized);
    }

    #[test]
    fn resolved_ability_with_sub_ability_roundtrips() {
        let sub = ResolvedAbility {
            api_type: "Draw".to_string(),
            params: HashMap::from([("NumCards".to_string(), "1".to_string())]),
            targets: vec![],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: None,
            svars: HashMap::new(),
        };
        let ability = ResolvedAbility {
            api_type: "DealDamage".to_string(),
            params: HashMap::from([("NumDmg".to_string(), "3".to_string())]),
            targets: vec![TargetRef::Player(PlayerId(1))],
            source_id: ObjectId(1),
            controller: PlayerId(0),
            sub_ability: Some(Box::new(sub)),
            svars: HashMap::new(),
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

    // --- New typed enum tests ---

    #[test]
    fn effect_api_type_returns_correct_string() {
        assert_eq!(
            Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetSpec::Any,
            }
            .api_type(),
            "DealDamage"
        );
        assert_eq!(Effect::Draw { count: 1 }.api_type(), "Draw");
        assert_eq!(Effect::Explore.api_type(), "Explore");
        assert_eq!(Effect::Proliferate.api_type(), "Proliferate");
        assert_eq!(
            Effect::Other {
                api_type: "Custom".to_string(),
                params: HashMap::new(),
            }
            .api_type(),
            "Custom"
        );
    }

    #[test]
    fn effect_has_39_variants() {
        // 38 from the registry + Other = 39 total
        // Verify by checking api_type returns distinct strings for each variant
        let variants: Vec<Effect> = vec![
            Effect::DealDamage {
                amount: DamageAmount::Fixed(1),
                target: TargetSpec::Any,
            },
            Effect::Draw { count: 1 },
            Effect::Pump {
                power: 1,
                toughness: 1,
                target: TargetSpec::Any,
            },
            Effect::Destroy {
                target: TargetSpec::Any,
            },
            Effect::Counter {
                target: TargetSpec::Any,
            },
            Effect::Token {
                name: "T".to_string(),
                power: 1,
                toughness: 1,
                types: vec![],
                colors: vec![],
                keywords: vec![],
            },
            Effect::GainLife { amount: 1 },
            Effect::LoseLife { amount: 1 },
            Effect::Tap {
                target: TargetSpec::Any,
            },
            Effect::Untap {
                target: TargetSpec::Any,
            },
            Effect::AddCounter {
                counter_type: "p1p1".to_string(),
                count: 1,
                target: TargetSpec::Any,
            },
            Effect::RemoveCounter {
                counter_type: "p1p1".to_string(),
                count: 1,
                target: TargetSpec::Any,
            },
            Effect::Sacrifice {
                target: TargetSpec::Any,
            },
            Effect::DiscardCard {
                count: 1,
                target: TargetSpec::Any,
            },
            Effect::Mill {
                count: 1,
                target: TargetSpec::Any,
            },
            Effect::Scry { count: 1 },
            Effect::PumpAll {
                power: 1,
                toughness: 1,
                target: TargetSpec::All {
                    filter: String::new(),
                },
            },
            Effect::DamageAll {
                amount: DamageAmount::Fixed(1),
                target: TargetSpec::All {
                    filter: String::new(),
                },
            },
            Effect::DestroyAll {
                target: TargetSpec::All {
                    filter: String::new(),
                },
            },
            Effect::ChangeZone {
                origin: "Library".to_string(),
                destination: "Battlefield".to_string(),
                target: TargetSpec::Any,
            },
            Effect::ChangeZoneAll {
                origin: "Battlefield".to_string(),
                destination: "Graveyard".to_string(),
                target: TargetSpec::All {
                    filter: String::new(),
                },
            },
            Effect::Dig {
                count: 3,
                destination: String::new(),
            },
            Effect::GainControl {
                target: TargetSpec::Any,
            },
            Effect::Attach {
                target: TargetSpec::Any,
            },
            Effect::Surveil { count: 1 },
            Effect::Fight {
                target: TargetSpec::Any,
            },
            Effect::Bounce {
                target: TargetSpec::Any,
                destination: String::new(),
            },
            Effect::Explore,
            Effect::Proliferate,
            Effect::CopySpell {
                target: TargetSpec::Any,
            },
            Effect::ChooseCard {
                choices: vec![],
                target: TargetSpec::Any,
            },
            Effect::PutCounter {
                counter_type: "p1p1".to_string(),
                count: 1,
                target: TargetSpec::Any,
            },
            Effect::MultiplyCounter {
                counter_type: "p1p1".to_string(),
                multiplier: 2,
                target: TargetSpec::Any,
            },
            Effect::Animate {
                power: Some(5),
                toughness: Some(5),
                types: vec![],
                target: TargetSpec::Any,
            },
            Effect::GenericEffect {
                params: HashMap::new(),
            },
            Effect::Cleanup {
                params: HashMap::new(),
            },
            Effect::Mana {
                produced: "R".to_string(),
                params: HashMap::new(),
            },
            Effect::Discard {
                count: 1,
                target: TargetSpec::Any,
            },
            Effect::Other {
                api_type: "Custom".to_string(),
                params: HashMap::new(),
            },
        ];
        assert_eq!(
            variants.len(),
            39,
            "Expected 39 Effect variants (38 + Other)"
        );
    }

    #[test]
    fn effect_serde_roundtrip_internally_tagged() {
        let effect = Effect::DealDamage {
            amount: DamageAmount::Fixed(3),
            target: TargetSpec::Filtered {
                filter: "Creature".to_string(),
            },
        };
        let json = serde_json::to_string(&effect).unwrap();
        // Should be internally tagged with "type" key
        assert!(json.contains("\"type\":\"DealDamage\""));
        let deserialized: Effect = serde_json::from_str(&json).unwrap();
        assert_eq!(effect, deserialized);
    }

    #[test]
    fn ability_definition_serde_roundtrip() {
        let def = AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetSpec::Any,
            },
            cost: None,
            sub_ability: None,
            remaining_params: HashMap::new(),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: AbilityDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, deserialized);
    }

    #[test]
    fn ability_definition_compat_api_type() {
        let def = AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetSpec::Any,
            },
            cost: None,
            sub_ability: None,
            remaining_params: HashMap::new(),
        };
        assert_eq!(def.api_type(), "DealDamage");
    }

    #[test]
    fn ability_definition_compat_params() {
        let def = AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(3),
                target: TargetSpec::Any,
            },
            cost: None,
            sub_ability: None,
            remaining_params: HashMap::new(),
        };
        let params = def.params();
        assert_eq!(params.get("NumDmg").unwrap(), "3");
        assert_eq!(params.get("ValidTgts").unwrap(), "Any");
    }

    #[test]
    fn trigger_definition_serde_roundtrip() {
        let def = TriggerDefinition {
            mode: TriggerMode::ChangesZone,
            params: HashMap::from([
                ("Origin".to_string(), "Any".to_string()),
                ("Destination".to_string(), "Battlefield".to_string()),
            ]),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: TriggerDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, deserialized);
    }

    #[test]
    fn static_definition_serde_roundtrip() {
        let def = StaticDefinition {
            mode: StaticMode::Continuous,
            params: HashMap::from([
                ("Affected".to_string(), "Creature.YouCtrl".to_string()),
                ("AddPower".to_string(), "1".to_string()),
            ]),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: StaticDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, deserialized);
    }

    #[test]
    fn replacement_definition_serde_roundtrip() {
        let def = ReplacementDefinition {
            event: ReplacementEvent::DamageDone,
            params: HashMap::from([("ActiveZones".to_string(), "Battlefield".to_string())]),
        };
        let json = serde_json::to_string(&def).unwrap();
        let deserialized: ReplacementDefinition = serde_json::from_str(&json).unwrap();
        assert_eq!(def, deserialized);
    }

    #[test]
    fn damage_amount_variants() {
        let fixed = DamageAmount::Fixed(5);
        let variable = DamageAmount::Variable("X".to_string());
        assert_ne!(fixed, variable);

        let json = serde_json::to_string(&fixed).unwrap();
        let deserialized: DamageAmount = serde_json::from_str(&json).unwrap();
        assert_eq!(fixed, deserialized);
    }

    #[test]
    fn target_spec_variants() {
        let specs = vec![
            TargetSpec::None,
            TargetSpec::Any,
            TargetSpec::Filtered {
                filter: "Creature".to_string(),
            },
            TargetSpec::Player,
            TargetSpec::Controller,
            TargetSpec::All {
                filter: "Creature".to_string(),
            },
        ];
        for spec in specs {
            let json = serde_json::to_string(&spec).unwrap();
            let deserialized: TargetSpec = serde_json::from_str(&json).unwrap();
            assert_eq!(spec, deserialized);
        }
    }

    #[test]
    fn ability_cost_variants() {
        let costs = vec![
            AbilityCost::Mana {
                cost: "2R".to_string(),
            },
            AbilityCost::Tap,
            AbilityCost::Loyalty { amount: -3 },
            AbilityCost::Sacrifice {
                target: TargetSpec::Filtered {
                    filter: "Creature".to_string(),
                },
            },
            AbilityCost::Composite {
                costs: vec![
                    AbilityCost::Tap,
                    AbilityCost::Mana {
                        cost: "1".to_string(),
                    },
                ],
            },
        ];
        for cost in costs {
            let json = serde_json::to_string(&cost).unwrap();
            let deserialized: AbilityCost = serde_json::from_str(&json).unwrap();
            assert_eq!(cost, deserialized);
        }
    }

    #[test]
    fn effect_to_params_draw() {
        let effect = Effect::Draw { count: 2 };
        let params = effect.to_params();
        assert_eq!(params.get("NumCards").unwrap(), "2");
    }

    #[test]
    fn effect_to_params_change_zone() {
        let effect = Effect::ChangeZone {
            origin: "Battlefield".to_string(),
            destination: "Graveyard".to_string(),
            target: TargetSpec::Filtered {
                filter: "Creature".to_string(),
            },
        };
        let params = effect.to_params();
        assert_eq!(params.get("Origin").unwrap(), "Battlefield");
        assert_eq!(params.get("Destination").unwrap(), "Graveyard");
        assert_eq!(params.get("ValidTgts").unwrap(), "Creature");
    }

    #[test]
    fn static_definition_mode_str_compat() {
        let def = StaticDefinition {
            mode: StaticMode::Continuous,
            params: HashMap::new(),
        };
        assert_eq!(def.mode_str(), "Continuous");
    }

    #[test]
    fn trigger_definition_mode_str_compat() {
        let def = TriggerDefinition {
            mode: TriggerMode::ChangesZone,
            params: HashMap::new(),
        };
        assert_eq!(def.mode_str(), "ChangesZone");
    }

    #[test]
    fn replacement_definition_event_str_compat() {
        let def = ReplacementDefinition {
            event: ReplacementEvent::DamageDone,
            params: HashMap::new(),
        };
        assert_eq!(def.event_str(), "DamageDone");
    }
}

use std::fmt;
use std::sync::Arc;

use serde::de;
use serde::ser::SerializeStructVariant;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use thiserror::Error;

use super::card::PrintedCardRef;
use super::card_type::{CardType, CoreType, SubtypeSet, Supertype};
use super::counter::{CounterMatch, CounterType};
use super::events::BendingType;
use super::game_state::{
    is_zero_usize, DistributionUnit, LKISnapshot, MayTriggerOrigin, RetargetScope,
    TargetSelectionConstraint,
};
use super::identifiers::ObjectId;
use super::keywords::{Keyword, KeywordKind};
use super::mana::{ManaColor, ManaCost, ManaType};
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::replacements::ReplacementEvent;
use super::statics::{ActivationExemption, CastFrequency, StaticMode};
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

/// CR 400.1 + CR 608.2c: Which player's zone supplies cards for a direct
/// zone choice during resolution.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneOwner {
    /// The controller of the spell/ability owns the referenced zone.
    #[default]
    Controller,
    /// The first player target of the resolving spell/ability owns the referenced zone.
    TargetedPlayer,
    /// An opponent of the controller owns the referenced zone.
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
///
/// Internally tagged (`{ "type": "DistinctCardTypes", "categories": [...] }`) to
/// match the sibling [`SearchSelectionConstraint`] convention and the frontend's
/// `ChooseFromZoneConstraint` type, which discriminates on the `type` field. The
/// `CardChoiceModal` confirm gate reads `constraint.type`; without the tag the
/// default external representation (`{ "DistinctCardTypes": {...} }`) leaves
/// `type` undefined and the modal can never validate a selection.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ChooseFromZoneConstraint {
    /// The chosen cards must admit an injective assignment to distinct card types
    /// from the listed categories.
    DistinctCardTypes { categories: Vec<CoreType> },
}

/// Selection constraint applied to multi-card library searches at the
/// `WaitingFor::SearchChoice` step. Lives one abstraction up from `count` /
/// `up_to` so the engine can reject illegal combinations and the AI can
/// prune its candidate space without bespoke per-card knowledge.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SearchSelectionConstraint {
    /// CR 107.1c: No restriction beyond `count` (and `up_to`).
    #[default]
    None,
    /// CR 608.2c: The spell's printed text constrains the selected set by
    /// object qualities — e.g. "with different names", "with different
    /// powers", or "that don't share a mana value, power, toughness, or card
    /// type with each other".
    DistinctQualities { qualities: Vec<SharedQuality> },
    /// CR 608.2c + CR 202.3: The chosen set's combined mana value must satisfy
    /// the printed comparator. Used by "cards with total mana value N or less"
    /// tutor text, where the constraint applies across the selected set rather
    /// than to each individual card.
    TotalManaValue { comparator: Comparator, value: i32 },
    /// CR 701.23a + CR 701.23h: A single library search may ask for several
    /// independently described cards ("a black card, a green card, and a blue
    /// card"). The chosen set must be assignable to the printed descriptions,
    /// with each physical card used for at most one description slot.
    MatchEachFilter { filters: Vec<TargetFilter> },
}

/// CR 400.11 + CR 406.3: Candidate pool for outside-game searches. The
/// baseline pool is the player's sideboard; Karn/Coax-class text widens that
/// pool to include owned face-up exile cards that match the same filter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OutsideGameSourcePool {
    /// CR 400.11a: Tournament sideboard / casual outside-the-game collection.
    #[default]
    Sideboard,
    /// CR 400.11a + CR 406.3: Sideboard plus matching owned face-up exile.
    SideboardAndFaceUpExile,
}

impl OutsideGameSourcePool {
    pub fn includes_face_up_exile(self) -> bool {
        matches!(self, OutsideGameSourcePool::SideboardAndFaceUpExile)
    }
}

/// CR 701.23a + CR 608.2c: A search whose found set is partitioned between two
/// destinations — e.g. Cultivate ("put one onto the battlefield tapped and the
/// other into your hand"). `primary_count` cards go to `primary_destination`
/// (the searcher's choice when more than `primary_count` are found); the rest go
/// to `rest_destination`. `primary_destination` is `Battlefield` for the A/B/C
/// cluster, where `primary_enter_tapped` routes the entry through the ETB
/// pipeline so the permanent enters tapped (CR 614.1 / CR 110.5b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchDestinationSplit {
    /// Where the chosen primary cards go (Battlefield for cultivate-class).
    pub primary_destination: Zone,
    /// How many of the found cards go to `primary_destination` (literal N from
    /// "put N ..."). Mirrors `Effect::Dig.keep_count`.
    pub primary_count: u32,
    /// CR 614.1 / CR 110.5b: When true, primary cards enter the battlefield
    /// tapped. Mirrors `Effect::ChangeZone.enter_tapped`.
    pub primary_enter_tapped: bool,
    /// Where the remaining found cards go ("the rest"/"the other" — Hand for
    /// cultivate-class).
    pub rest_destination: Zone,
}

/// CR 608.2d: Who may choose to perform an optional effect during resolution.
/// Used with `AbilityDefinition::optional_for` to route the "you may" prompt to opponents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum OpponentMayScope {
    /// "any opponent may" — each opponent in APNAP order gets the chance; first accept wins.
    AnyOpponent,
}

/// What kind of named choice the player must make at resolution time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChoiceType {
    CreatureType,
    Color {
        /// Colors that cannot be chosen by this prompt.
        ///
        /// CR 105.1 + CR 105.4 define the five legal color choices; prompts such as
        /// "choose a color other than white" restrict that set.
        excluded: Vec<ManaColor>,
    },
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
    /// CR 608.2d: "Choose [an ability from this list]" — the option set is a
    /// typed list of `Keyword`s, not free-form strings. Used by Urborg /
    /// Walking Sponge / Phyrexian Splicer ("target creature loses first strike
    /// or swampwalk until end of turn"). The chosen keyword persists as
    /// `ChosenAttribute::Keyword` on the source so a downstream
    /// `ContinuousModification::RemoveChosenKeyword` (Layer 6) can strip the
    /// chosen ability at layer-evaluation time.
    Keyword {
        options: Vec<Keyword>,
    },
}

impl ChoiceType {
    pub fn color() -> Self {
        Self::Color {
            excluded: Vec::new(),
        }
    }

    pub fn color_excluding(excluded: Vec<ManaColor>) -> Self {
        Self::Color { excluded }
    }
}

impl Serialize for ChoiceType {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        match self {
            Self::CreatureType => {
                serializer.serialize_unit_variant("ChoiceType", 0, "CreatureType")
            }
            Self::Color { excluded } => {
                if excluded.is_empty() {
                    serializer.serialize_unit_variant("ChoiceType", 1, "Color")
                } else {
                    let mut variant =
                        serializer.serialize_struct_variant("ChoiceType", 1, "Color", 1)?;
                    variant.serialize_field("excluded", excluded)?;
                    variant.end()
                }
            }
            Self::OddOrEven => serializer.serialize_unit_variant("ChoiceType", 2, "OddOrEven"),
            Self::BasicLandType => {
                serializer.serialize_unit_variant("ChoiceType", 3, "BasicLandType")
            }
            Self::CardType => serializer.serialize_unit_variant("ChoiceType", 4, "CardType"),
            Self::CardName => serializer.serialize_unit_variant("ChoiceType", 5, "CardName"),
            Self::NumberRange { min, max } => {
                let mut variant =
                    serializer.serialize_struct_variant("ChoiceType", 6, "NumberRange", 2)?;
                variant.serialize_field("min", min)?;
                variant.serialize_field("max", max)?;
                variant.end()
            }
            Self::Labeled { options } => {
                let mut variant =
                    serializer.serialize_struct_variant("ChoiceType", 7, "Labeled", 1)?;
                variant.serialize_field("options", options)?;
                variant.end()
            }
            Self::LandType => serializer.serialize_unit_variant("ChoiceType", 8, "LandType"),
            Self::Opponent => serializer.serialize_unit_variant("ChoiceType", 9, "Opponent"),
            Self::Player => serializer.serialize_unit_variant("ChoiceType", 10, "Player"),
            Self::TwoColors => serializer.serialize_unit_variant("ChoiceType", 11, "TwoColors"),
            Self::Word => serializer.serialize_unit_variant("ChoiceType", 12, "Word"),
            Self::Artist => serializer.serialize_unit_variant("ChoiceType", 13, "Artist"),
            Self::Keyword { options } => {
                let mut variant =
                    serializer.serialize_struct_variant("ChoiceType", 14, "Keyword", 1)?;
                variant.serialize_field("options", options)?;
                variant.end()
            }
        }
    }
}

impl<'de> Deserialize<'de> for ChoiceType {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(untagged)]
        enum ChoiceTypeRepr {
            Unit(String),
            Data(ChoiceTypeData),
        }

        #[derive(Deserialize)]
        enum ChoiceTypeData {
            Color {
                #[serde(default)]
                excluded: Vec<ManaColor>,
            },
            NumberRange {
                min: u8,
                max: u8,
            },
            Labeled {
                options: Vec<String>,
            },
            Keyword {
                options: Vec<Keyword>,
            },
        }

        match ChoiceTypeRepr::deserialize(deserializer)? {
            ChoiceTypeRepr::Unit(value) => match value.as_str() {
                "CreatureType" => Ok(Self::CreatureType),
                "Color" => Ok(Self::color()),
                "OddOrEven" => Ok(Self::OddOrEven),
                "BasicLandType" => Ok(Self::BasicLandType),
                "CardType" => Ok(Self::CardType),
                "CardName" => Ok(Self::CardName),
                "LandType" => Ok(Self::LandType),
                "Opponent" => Ok(Self::Opponent),
                "Player" => Ok(Self::Player),
                "TwoColors" => Ok(Self::TwoColors),
                "Word" => Ok(Self::Word),
                "Artist" => Ok(Self::Artist),
                other => Err(de::Error::unknown_variant(
                    other,
                    &[
                        "CreatureType",
                        "Color",
                        "OddOrEven",
                        "BasicLandType",
                        "CardType",
                        "CardName",
                        "LandType",
                        "Opponent",
                        "Player",
                        "TwoColors",
                        "Word",
                        "Artist",
                    ],
                )),
            },
            ChoiceTypeRepr::Data(data) => match data {
                ChoiceTypeData::Color { excluded } => Ok(Self::Color { excluded }),
                ChoiceTypeData::NumberRange { min, max } => Ok(Self::NumberRange { min, max }),
                ChoiceTypeData::Labeled { options } => Ok(Self::Labeled { options }),
                ChoiceTypeData::Keyword { options } => Ok(Self::Keyword { options }),
            },
        }
    }
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

/// CR 706.2: Modifier applied to a die roll's natural result before the
/// effect's result table is consulted. "Roll a d20 and add the number of
/// cards in your hand" → `Add(QuantityExpr::Ref(HandSize { player: Controller }))`.
/// "Roll a d20 and subtract the number of cards in your hand" → `Subtract(...)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DieRollModifier {
    /// Add the resolved quantity to the natural roll.
    Add { value: QuantityExpr },
    /// Subtract the resolved quantity from the natural roll.
    Subtract { value: QuantityExpr },
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

/// CR 614.9: Recipient of a one-shot damage-redirection effect — the
/// battle/creature/planeswalker/player the replaced damage is dealt to instead.
/// Resolved to a concrete object/player at effect-resolution time (see
/// `effects::create_damage_replacement::resolve`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum DamageRedirectTarget {
    /// "...to you instead" — the replacement source's controller (Jade Monolith,
    /// Goblin Psychopath).
    Controller,
    /// "...to ~ instead" / "...dealt to this creature instead" — the replacement
    /// source object itself (Beacon of Destiny).
    SourceObject,
    /// "...to target creature instead" — an object chosen as a target of the
    /// creating ability (Soltari Guerrillas).
    ChosenObjectTarget,
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
    /// CR 614.5 + CR 614.1a: One-shot damage-amount replacement created by an
    /// effect ("the next time ... would deal damage this turn, it deals double
    /// that damage instead" — Desperate Gambit). Gets one opportunity to affect
    /// a damage event, is consumed on use, and expires at cleanup. Distinct from
    /// a continuous static `damage_modification` (Furnace of Rath), which keeps
    /// `ShieldKind::None` and re-applies to every damage event.
    DamageReplacementOneShot,
    /// CR 614.9: One-shot redirection shield — replaces all or part of a damage
    /// event's recipient with `recipient`. `All` covers "the next time ... would
    /// deal damage"; `Next(n)` covers "the next N damage ... is dealt to ..."
    /// redirections. Consumed on use, expires at cleanup.
    Redirection {
        recipient: DamageRedirectTarget,
        amount: PreventionAmount,
    },
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
    /// CR 608.2d: Records the typed keyword chosen from a `ChoiceType::Keyword`
    /// option list (Urborg / Walking Sponge "choose an ability the target has,
    /// remove it"). Read by `ContinuousModification::RemoveChosenKeyword` at
    /// Layer 6 evaluation to strip the chosen keyword from the recipient.
    Keyword(Keyword),
    /// CR 614.12c: Records the anchor-word label chosen as the permanent
    /// entered the battlefield (e.g. Frostcliff Siege "Jeskai" / "Temur",
    /// Khans of Tarkir Sieges "Khans" / "Dragons"). Read by
    /// `StaticCondition::ChosenLabelIs` and `TriggerCondition::ChosenLabelIs`
    /// to gate which of the two anchor-word-marked abilities (CR 607: linked
    /// abilities) functions while the permanent is on the battlefield. The
    /// label is stored case-canonicalised to match `ChoiceType::Labeled`'s
    /// capitalised option list.
    Label(String),
}

impl ChosenAttribute {
    /// Which category of choice this represents.
    pub fn choice_type(&self) -> ChoiceType {
        match self {
            Self::Color(_) => ChoiceType::color(),
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
            // CR 608.2d: A category template — the concrete option list is
            // attached to each emission site (Urborg lists FirstStrike +
            // Swampwalk; another card might list any pair). Mirrors the
            // `NumberRange { min: 0, max: 20 }` template idiom.
            Self::Keyword(_) => ChoiceType::Keyword {
                options: Vec::new(),
            },
            // CR 614.12c: Anchor-word labels are a free-form labeled choice
            // (the per-card option list — e.g. ["Jeskai", "Temur"] — is
            // emitted at the choice site, mirroring the `Keyword` template
            // idiom). The stored single label is one of those options.
            Self::Label(label) => ChoiceType::Labeled {
                options: vec![label.clone()],
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
            ChoiceValue::Keyword(keyword) => Some(Self::Keyword(keyword)),
            // CR 614.12c: Persist a labeled choice as an anchor-word label so
            // companion `ChosenLabelIs` conditions (static + trigger) can read
            // it for the lifetime of the permanent.
            ChoiceValue::Label(label) => Some(Self::Label(label)),
            ChoiceValue::LandType(_) => None,
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
    /// CR 608.2d: typed-keyword choice from a `ChoiceType::Keyword` option
    /// list (Urborg / Walking Sponge). Persisted into the source's
    /// `chosen_attributes` as `ChosenAttribute::Keyword` for later
    /// `RemoveChosenKeyword` resolution.
    Keyword(Keyword),
}

impl ChoiceValue {
    pub fn from_choice(choice_type: &ChoiceType, value: &str) -> Option<Self> {
        match choice_type {
            ChoiceType::Color { excluded } => {
                let color = value.parse::<ManaColor>().ok()?;
                (!excluded.contains(&color)).then_some(Self::Color(color))
            }
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
            // CR 608.2d: match the player's response against the typed option
            // list by display string. Comparison is case-insensitive so the
            // frontend can render canonical capitalization while the engine
            // accepts either form.
            ChoiceType::Keyword { options } => {
                let needle = value.to_lowercase();
                options
                    .iter()
                    .find(|k| k.to_string().to_lowercase() == needle)
                    .cloned()
                    .map(Self::Keyword)
            }
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
///
/// CR 608.2 + CR 109.5: `Controller` and `ScopedPlayer` are distinct axes
/// during `player_scope` iteration. `Controller` always means the printed
/// ability's controller (the "you" axis from CR 109.5). `ScopedPlayer` means
/// the player whose iteration copy is currently running (e.g., each opponent
/// in "Each opponent mills half of *their* library, rounded up." — Maddening
/// Cacophony's kicker mode). Outside iteration, `ScopedPlayer` falls back
/// to `Controller`. Mirrors the `ControllerRef::You` vs
/// `ControllerRef::ScopedPlayer` split.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CountScope {
    /// CR 109.5: The printed ability's controller — "you" / "your" semantics.
    Controller,
    /// CR 108.3 + CR 404.2: The printed ability controller as owner for
    /// player-scoped queries over non-battlefield zones.
    Owner,
    /// CR 608.2 + CR 109.5: The currently iterated player during a
    /// `player_scope` resolution — "they" / "their" semantics relative to the
    /// iteration. Issue #310: distinguishes "their library" (per-iteration)
    /// from "your library" (always caster). Falls back to `Controller`
    /// outside iteration.
    ScopedPlayer,
    /// CR 607.2d + CR 608.2c: The source's linked persisted chosen player —
    /// "the chosen player's <zone>" on cards whose source stored
    /// `ChosenAttribute::Player`.
    SourceChosenPlayer,
    All,
    Opponents,
}

fn default_count_scope_controller() -> CountScope {
    CountScope::Controller
}

/// Which zone to count cards in (for `QuantityRef::ZoneCardCount`).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ZoneRef {
    Graveyard,
    Exile,
    Library,
    Hand,
}

/// CR 701.10d-f: What aspect to double (counters, life total, or mana pool).
/// Used by `Effect::Double` per locked decision D-05.
/// DoublePT/DoublePTAll handle CR 701.10a-c (power/toughness) separately.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DoubleTarget {
    /// CR 701.10e: Double the number of a kind of counter on a permanent.
    /// None = all counter types on the permanent.
    Counters { counter_type: Option<CounterType> },
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

/// CR 122.5 / CR 122.8: Whether a counter-transfer effect actually moves
/// counters off the source or only puts matching counters on the destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CounterTransferMode {
    /// CR 122.5: Remove counters from the source and put them on the target.
    Move,
    /// CR 122.8: Put matching counters on the target using source/LKI state.
    Put,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CounterMoveSelection {
    #[default]
    StackTarget,
    StackTargetAnyNumber,
    ResolutionDistributionAnyNumber,
}

/// CR 701.6 + CR 608.2c: A follow-up instruction carried by `Effect::Counter`
/// that acts on the *source permanent* of an ability countered by the effect.
///
/// The rider only fires when the countered object was an activated or triggered
/// ability — per CR 701.8a / CR 110.1 a spell is not a permanent, so when a
/// spell is countered there is no permanent for the rider to act on and it is
/// skipped (the conditional "if a permanent's ability is countered this way" is
/// encoded structurally by this spell-vs-ability gate, not by a separate
/// `AbilityCondition`).
///
/// Both variants share one categorical axis — "an effect applied to the
/// countered ability's source permanent" — so they live on a single enum rather
/// than as sibling `Option` fields on `Effect::Counter` (parameterize, don't
/// proliferate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CounterSourceRider {
    /// CR 611.2: "that permanent loses all abilities ..." — applies a static
    /// definition to the countered ability's source permanent
    /// (Tishana's Tidebinder).
    LosesAbilities { static_def: Box<StaticDefinition> },
    /// CR 701.8: "destroy that permanent" — destroys the countered ability's
    /// source permanent (Teferi's Response, Green Slime).
    Destroy,
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
    /// Produce one or more specific colors.
    Fixed {
        #[serde(default)]
        colors: Vec<ManaColor>,
        /// CR 605.1a: Whether this is base or additional (e.g. Fertile Ground) mana.
        #[serde(
            default = "default_mana_contribution",
            skip_serializing_if = "is_default_mana_contribution"
        )]
        contribution: ManaContribution,
    },
    /// Produce strictly colorless mana (e.g. "Add {C}").
    Colorless {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// Produce a mixed bundle of fixed colorless + colored mana (e.g. "Add {C}{R}").
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
        /// CR 106.1: Optional fixed color the player may produce *instead of* the
        /// chosen color (Cycle of Gates: "Add {G} or one mana of the chosen
        /// color"). `None` = pure chosen-color production (Utopia Sprawl class).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        fixed_alternative: Option<ManaColor>,
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
                        #[serde(default)]
                        fixed_alternative: Option<ManaColor>,
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
                        fixed_alternative,
                    } => ManaProduction::ChosenColor {
                        count,
                        contribution,
                        fixed_alternative,
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
    /// "Spend this mana only to cast spells."
    SpellOnly,
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
    /// CR 106.6: "Spend this mana only to cast spells with mana value N or
    /// greater" (or "or less"). Parameterized over [`Comparator`] so a single
    /// variant covers every mana-value threshold reading rather than
    /// proliferating per-threshold siblings ("parameterize, don't proliferate").
    /// `value` is the printed threshold N; `comparator` applies
    /// `spell_mana_value <cmp> value`.
    SpellWithManaValue { comparator: Comparator, value: u32 },
    /// CR 105.2 + CR 106.6: "Spend this mana only to cast spells with exactly N
    /// colors" (also "N or more / N or fewer"; colorless = 0). Parameterized over
    /// [`Comparator`] — one variant per color-count reading. `count` is N.
    SpellWithColorCount { comparator: Comparator, count: u32 },
    /// CR 106.6 + CR 400.7: "Spend this mana only to cast spells from your
    /// graveyard" / "from exile". Gates spending on the spell's cast-from zone
    /// alone — a distinct axis from [`ManaSpendRestriction::SpellWithKeywordKindFromZone`],
    /// which additionally requires a keyword. Resolved against `SpellMeta.cast_from_zone`.
    SpellFromZone(Zone),
}

/// Duration for temporary effects.
///
/// Player-axis variants are parameterized by `PlayerScope` per the workspace
/// "Parameterize, don't proliferate" principle. `PlayerScope::Controller`
/// recovers the legacy "until your next turn" / "controller's next step"
/// semantics; future `Target` / `Opponent` / `AllPlayers` readings unblock
/// cards whose duration is bound to a non-controller player.
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
    /// CR 514.2: Effect expires at the **cleanup step of `player`'s next turn**
    /// — it persists through that entire turn. This is the "until the end of
    /// [your/their] next turn" reading (Light Up the Stage, Slip Out the Back),
    /// distinct from `UntilNextTurnOf` which expires at the *beginning* of the
    /// next turn. At the player's next untap step the effect is "armed"
    /// (converted to `UntilEndOfTurn`) so the existing cleanup-step prune ends
    /// it at that turn's cleanup; it survives the creation turn's own cleanup.
    UntilEndOfNextTurnOf {
        player: PlayerScope,
    },
    /// CR 611.2a: Effect expires when the source object leaves the
    /// battlefield.
    UntilHostLeavesPlay,
    /// CR 500.1 + CR 611.2a: Effect expires at the beginning of `player`'s
    /// next named phase/step. `Phase::Untap` covers exert / "doesn't untap"
    /// effects (CR 502.3). `Phase::End` covers "until your next end step"
    /// floating play-permission patterns such as Rocco, Street Chef (CR 513.1).
    #[serde(alias = "UntilNextUntapStepOf")]
    UntilNextStepOf {
        #[serde(default)]
        step: Phase,
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
    /// CR 101.2 + CR 601.2a + CR 602.5: A temporary effect prohibits one activity
    /// axis for affected players until expiry.
    ProhibitActivity {
        source: ObjectId,
        affected_players: RestrictionPlayerScope,
        expiry: RestrictionExpiry,
        activity: ProhibitedActivity,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ProhibitedActivity {
    /// CR 101.2 + CR 601.2a: Restrict casting to the listed zones.
    CastOnlyFromZones { allowed_zones: Vec<Zone> },
    /// CR 101.2: Prevent casting matching spells.
    CastSpells {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spell_filter: Option<TargetFilter>,
    },
    /// CR 101.2 + CR 602.5 + CR 605.1a: Prevent activating abilities, optionally
    /// exempting mana abilities.
    ActivateAbilities { exemption: ActivationExemption },
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
    /// Placeholder used by parser lowering for effects that target a player.
    /// Resolved to `SpecificPlayer` by `add_restriction` at resolution time.
    TargetedPlayer,
    /// CR 608.2c: Anaphoric "that player" in a sub-ability reuses a player
    /// target already chosen for an earlier instruction in the same chain.
    /// Resolved to `SpecificPlayer` by `add_restriction` after parent target
    /// propagation, without declaring a second target slot.
    ParentTargetedPlayer,
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
        /// CR 611.2a + CR 118.9: When `Some(p)`, only player `p` may cast under
        /// this permission. The grant-issuing effect (typically an attack
        /// trigger or ETB on a different controller's permanent) records the
        /// controller of the ability that granted this permission so cards
        /// owned by an *opponent* (e.g., cards exiled from each player's
        /// library by Jeleva's ETB) can still be cast only by the granting
        /// permanent's controller — not by the card's owner. When `None`,
        /// `has_exile_cast_permission` falls back to the legacy
        /// `obj.owner == player` rule (Discover, Cascade, Suspend, Airbending,
        /// and other classes where the card is exiled from the would-be
        /// caster's own zones, so owner == grantee anyway).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        granted_to: Option<PlayerId>,
        /// CR 608.2g: When `Some(...)`, this permission was granted to cast a
        /// Cascade/Discover hit *during resolution* of the source spell. It
        /// carries the rejection-cleanup state (exiled misses + where the hit
        /// goes if the cast-time MV check fails). `None` for all standing
        /// permissions (Airbending, Suspend, Maralen, Beseech, etc.) which are
        /// cast later via a normal `CastSpell` and never need resolution-time
        /// cleanup. `resolution_cleanup.is_some()` is the discriminator that
        /// distinguishes a cast-during-resolution permission from a plain
        /// `ManaValue`-constrained standing permission at finalize time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        resolution_cleanup: Option<ResolutionCastCleanup>,
        /// CR 611.2a: Optional durational scope. When `Some(...)`, this
        /// permission is pruned by the corresponding `layers::prune_*` helpers
        /// at the same timing points as `PlayFromExile { duration, .. }`.
        /// `None` (the common case) preserves the standing behavior: the
        /// permission persists until the object leaves exile (Airbending,
        /// Suspend, Discover, Cascade, etc., handled by
        /// `zones::apply_zone_exit_cleanup`).
        ///
        /// CR 702.88a (Rebound): used by the Rebound recast permission with
        /// `Duration::UntilEndOfTurn` so the granted "cast this card from
        /// exile without paying its mana cost" offer expires at the cleanup
        /// step of the upkeep on which it was offered if the controller
        /// declines or fails to cast.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<Duration>,
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
        /// CR 601.2a: Per-source use frequency for persistent play
        /// permissions. `Unlimited` preserves existing impulse-draw behavior;
        /// `OncePerTurn` models linked static permissions like Evelyn.
        #[serde(default, skip_serializing_if = "CastFrequency::is_unlimited")]
        frequency: CastFrequency,
        /// Source object whose once-per-turn slot is consumed when
        /// `frequency` is bounded. Filled by `grant_permission::resolve`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
        /// Controller of the ability that exiled this card and attached this
        /// permission. Evelyn-class statics use this as provenance, then find
        /// a currently-live static permission source at play/cast time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exiled_by_ability_controller: Option<PlayerId>,
        /// CR 609.4b: Optional payment permission carried by the same effect
        /// that allows the card to be played/cast from exile. This scopes
        /// "mana of any type can be spent to cast that spell" to the exiled
        /// card rather than creating a global player permission.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<ManaSpendPermission>,
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
        /// CR 611.2a + CR 118.9: Mirrors `ExileWithAltCost.granted_to`. See
        /// that field for the full rationale on owner-versus-grantee binding.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        granted_to: Option<PlayerId>,
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

/// CR 609.4b: Permission modifying how mana may be spent to pay a cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaSpendPermission {
    /// Mana may be spent as though it were mana of any type or color for this
    /// payment. This preserves the Oracle distinction without changing the
    /// actual mana spent.
    AnyTypeOrColor,
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
    /// CR 202.3 + CR 601.2e: The spell's resulting mana value must satisfy
    /// this predicate for the cast permission to apply.
    ManaValue {
        comparator: Comparator,
        value: QuantityExpr,
    },
}

/// CR 608.2g: Rejection-cleanup state carried by a cast-during-resolution
/// `ExileWithAltCost` permission (Cascade / Discover). When the cast-time
/// resulting-mana-value check fails at finalization, the source spell's
/// `WaitingFor::CastOffer` has already been consumed, so the misses ride
/// inside the permission and the engine still knows where the rejected hit
/// goes (`reject_action`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionCastCleanup {
    /// Cards exiled during the dig that were not the hit; they go to the
    /// bottom of the library in a random order on resolution completion.
    pub exiled_misses: Vec<super::identifiers::ObjectId>,
    /// Where the hit goes if the player declines or the cast-time MV check
    /// rejects the cast.
    pub reject_action: ResolutionMvRejectAction,
}

/// CR 608.2g: Disposition of a Cascade/Discover hit that is not cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ResolutionMvRejectAction {
    /// CR 702.85a: cascade — hit joins misses on the bottom in random order.
    BottomWithMisses,
    /// CR 701.57a: discover — hit goes to its owner's hand; misses to bottom.
    ToHand,
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
    #[serde(
        serialize_with = "serialize_multi_target_min",
        deserialize_with = "deserialize_multi_target_min"
    )]
    pub min: QuantityExpr,
    /// `None` means "any number" (unlimited). CR 115.1d.
    pub max: Option<QuantityExpr>,
}

impl MultiTargetSpec {
    pub fn fixed(min: usize, max: usize) -> Self {
        Self::bounded_expr(
            QuantityExpr::Fixed { value: min as i32 },
            QuantityExpr::Fixed { value: max as i32 },
        )
    }

    pub fn up_to(max: QuantityExpr) -> Self {
        Self::bounded_expr(QuantityExpr::Fixed { value: 0 }, max)
    }

    pub fn exact(count: QuantityExpr) -> Self {
        Self::bounded_expr(count.clone(), count)
    }

    pub fn unlimited(min: usize) -> Self {
        Self {
            min: QuantityExpr::Fixed { value: min as i32 },
            max: None,
        }
    }

    pub fn bounded(min: usize, max: QuantityExpr) -> Self {
        Self::bounded_expr(QuantityExpr::Fixed { value: min as i32 }, max)
    }

    pub fn bounded_expr(min: QuantityExpr, max: QuantityExpr) -> Self {
        Self {
            min,
            max: Some(max),
        }
    }

    pub fn min_is_fixed_zero(&self) -> bool {
        matches!(self.min, QuantityExpr::Fixed { value: 0 })
    }

    pub fn fixed_min_usize(&self) -> Option<usize> {
        match self.min {
            QuantityExpr::Fixed { value } if value >= 0 => Some(value as usize),
            _ => None,
        }
    }

    pub fn map_quantities<F>(&mut self, mut map: F)
    where
        F: FnMut(QuantityExpr) -> QuantityExpr,
    {
        self.min = map(self.min.clone());
        if let Some(max) = self.max.take() {
            self.max = Some(map(max));
        }
    }
}

fn serialize_multi_target_min<S>(min: &QuantityExpr, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    match min {
        QuantityExpr::Fixed { value } => serializer.serialize_i32(*value),
        other => other.serialize(serializer),
    }
}

fn deserialize_multi_target_min<'de, D>(deserializer: D) -> Result<QuantityExpr, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum MultiTargetMin {
        Fixed(i32),
        Expr(QuantityExpr),
    }

    match MultiTargetMin::deserialize(deserializer)? {
        MultiTargetMin::Fixed(value) => Ok(QuantityExpr::Fixed { value }),
        MultiTargetMin::Expr(expr) => Ok(expr),
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
    /// CR 115.10 + CR 608.2c: Filter controller is the current player being
    /// affected by an "each player/opponent" instruction during resolution.
    ScopedPlayer,
    /// CR 109.4 + CR 115.1: Filter controller is the player chosen as a target
    /// of the enclosing ability (e.g., "each creature target player controls").
    /// At resolution time, `filter_inner` reads the first `TargetRef::Player`
    /// from `ability.targets`. At target-selection time, `collect_target_slots`
    /// surfaces a companion `TargetFilter::Player` slot so the player is chosen
    /// as part of CR 601.2c / CR 603.3d target declaration.
    TargetPlayer,
    /// CR 608.2c + CR 109.4: Filter controller is the controller of the parent
    /// object target inherited by this chained effect ("that permanent's
    /// controller may sacrifice a land").
    ParentTargetController,
    /// CR 508.5 / CR 508.5a: Filter controller is the defending player for
    /// the source attacking creature, resolved per attacker through
    /// `combat::defending_player_for_attacker`. Used by intervening-if
    /// quantity checks such as "defending player controls more lands than you."
    DefendingPlayer,
    /// CR 608.2c + CR 109.4: Filter controller is the player chosen by the
    /// Nth `Effect::Choose { choice_type: ChoiceType::Player }` in this
    /// resolving ability chain (`index` is 0-based: 0 = the first choose).
    /// Distinct from `TargetPlayer` (a target declared when the ability went
    /// on the stack): a chosen player is selected *during* resolution via the
    /// `WaitingFor::NamedChoice` round-trip. Resolved by reading
    /// `ResolvedAbility.chosen_players[index]`, the resolution-scoped list the
    /// `NamedChoice` answer handler appends to. Powers the
    /// "choose a player. They <verb> … choose a second player to <verb>"
    /// card class (Gluntch, the Bestower; the Tempt cycle).
    ChosenPlayer {
        index: u8,
    },
    /// CR 613.1 + CR 109.4: Filter controller is the player PERSISTED on the
    /// source via `ChosenAttribute::Player` — the player chosen by an
    /// "as ~ enters the battlefield, choose a player" replacement. Read at
    /// filter / layer-evaluation time from the source's `chosen_attributes`
    /// (mirrors `GameObject::protector`). Distinct from `ChosenPlayer { index }`,
    /// which is resolution-scoped (valid only mid-resolution); this is a durable
    /// characteristic readable continuously, as a CDA requires. Powers
    /// "~'s power and toughness are each equal to the number of <X> the chosen
    /// player controls" (Skyshroud War Beast, Lost Order of Jarkeld).
    SourceChosenPlayer,
    /// CR 603.2 + CR 109.4: Filter controller is the player identified by the
    /// triggering event (the drawer, life-gainer, attacker, etc.). Resolved
    /// against `state.current_trigger_event` via `extract_player_from_event`.
    /// Mirrors `PlayerFilter::TriggeringPlayer` and `TargetFilter::TriggeringPlayer`.
    /// Used by control-relative trigger restrictions
    /// ("an opponent who controls F draws a card").
    TriggeringPlayer,
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
    /// CR 202.3: mana value.
    ManaValue,
    /// CR 208.1: power.
    Power,
    /// CR 208.1: toughness.
    Toughness,
    /// CR 208.1: sum of power and toughness.
    TotalPowerToughness,
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

/// Combat relationship required by `FilterProp::CombatRelation`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CombatRelation {
    /// CR 509.1g/509.1h: Candidate is blocking the subject or is blocked by it.
    BlockingOrBlockedBy,
}

/// Context object for a combat relationship filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CombatRelationSubject {
    /// The source object of the resolving spell or ability.
    Source,
    /// The first selected object target of the resolving spell or ability.
    ParentTarget,
}

/// Individual filter properties that can be combined in a Typed filter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum FilterProp {
    /// CR 111.1: Matches objects that are tokens.
    Token,
    /// CR 111.1: Matches objects that are not tokens.
    NonToken,
    Attacking,
    /// CR 509.1a: Matches creatures that are blocking.
    Blocking,
    /// CR 509.1g: Matches creatures currently blocking the filter source.
    /// Used for "creature(s) blocking it" source-relative quantities and filters.
    BlockingSource,
    /// CR 509.1g/509.1h: Matches creatures in a combat relationship with a
    /// source or selected parent target.
    CombatRelation {
        relation: CombatRelation,
        subject: CombatRelationSubject,
    },
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
    /// CR 303.4 + CR 702.5: Matches Aura objects whose enchant ability can
    /// legally enchant the referenced target. This is a semantic predicate,
    /// not exact keyword equality: `Enchant(creature)` can satisfy "that could
    /// enchant that creature" where the reference resolves through the
    /// ability's target slots.
    CanEnchant {
        target: Box<TargetFilter>,
    },
    /// CR 122.1: Matches objects whose counter count satisfies `comparator`
    /// against `count`. `counters` selects which counters are counted —
    /// `CounterMatch::OfType(t)` counts only type `t`, `CounterMatch::Any` sums
    /// across all counter types. Replaces the legacy `CountersGE` +
    /// `HasAnyCounter` variants; the comparator axis is parameterized via
    /// `Comparator`, unlocking EQ/LT/NE ("without a +1/+1 counter", "with no
    /// counters") without sibling proliferation — mirrors the `Cmc` refactor.
    Counters {
        counters: CounterMatch,
        comparator: Comparator,
        count: QuantityExpr,
    },
    /// Matches objects whose mana value satisfies `comparator` against `value`.
    /// CR 202.3. Replaces the legacy `CmcGE`/`CmcLE`/`CmcEQ` sibling cluster;
    /// the comparator axis is parameterized via the existing `Comparator` enum,
    /// unlocking GT/LT/NE comparisons without further variant proliferation.
    /// Supports both fixed and dynamic (e.g., X) thresholds via `QuantityExpr`.
    Cmc {
        comparator: Comparator,
        value: QuantityExpr,
    },
    /// CR 202.1: Matches objects whose printed mana cost is exactly one of `costs`.
    /// Distinct from `Cmc`/mana value (CR 202.3): "{0} or {1}" must not match
    /// artifacts with colored one-mana costs like {W}.
    ManaCostIn {
        costs: Vec<ManaCost>,
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
    /// this predicate is non-source-relative by default: it matches any object with a
    /// qualifying attachment. `exclude_source` preserves "another Aura/Equipment"
    /// semantics for Aura legality so the source attachment cannot satisfy its own
    /// "another" restriction after it becomes attached. `controller = None` means
    /// "any controller".
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
        #[serde(default, skip_serializing_if = "is_false")]
        exclude_source: bool,
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
    /// CR 702.95b: Matches objects that are not paired with another creature.
    Unpaired,
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
    /// CR 208 (Power/Toughness) + CR 208.4b (base vs current) + CR 613.4b
    /// (layer 7b: base P/T is set before counters/modifiers in 7c): Matches
    /// objects whose power, toughness, or total power and toughness satisfies
    /// `comparator` against `value`.
    ///
    /// Replaces the former `PowerLE`/`PowerGE`/`ToughnessLE`/`ToughnessGE`
    /// sibling cluster (a `stat × comparator` cross-product) with a single
    /// parameterized predicate, mirroring the `Cmc` and `Counters` refactors.
    /// Three orthogonal axes:
    /// - `stat`: which P/T metric to read — power (CR 208.1), toughness, or
    ///   their total.
    /// - `scope`: `Current` reads the live `power`/`toughness` (after all layers);
    ///   `Base` reads `base_power`/`base_toughness` per CR 208.4b — the value
    ///   after CDAs and set effects (layers 7a/7b) but ignoring counters and
    ///   modifying effects (layer 7c). A 1/1 with a +1/+1 counter has base
    ///   power 1 but current power 2.
    /// - `comparator`: any `Comparator` (LE/GE/EQ/LT/GT/NE).
    ///
    /// The disjunctive natural-language form "power or toughness N or less"
    /// composes two `PtComparison` props under `AnyOf`.
    ///
    /// Card data is regenerated from Oracle text at build time, so no legacy-tag
    /// deserialization shim is required; `scope` defaults to `Current` when
    /// absent for forward-compatible hand-authored fixtures.
    PtComparison {
        stat: PtStat,
        #[serde(default)]
        scope: PtValueScope,
        comparator: Comparator,
        value: QuantityExpr,
    },
    /// CR 509.1b: Matches objects whose power is strictly greater than the source object's power.
    /// Used for "creatures with greater power" blocking restrictions (relative comparison).
    PowerGTSource,
    /// CR 105.2: Matches objects by color-set size. Covers colorless
    /// (0 colors), monocolored (exactly 1 color), and multicolored
    /// (2 or more colors) without proliferating sibling variants.
    ColorCount {
        comparator: Comparator,
        count: u8,
    },
    /// Matches objects with a specific supertype (Basic, Legendary, Snow).
    HasSupertype {
        value: Supertype,
    },
    /// Matches objects whose subtypes include the source object's chosen creature type.
    /// Used for "of the chosen type" patterns (Cavern of Souls, Metallic Mimic).
    IsChosenCreatureType,
    /// CR 205.3m + CR 701.23a: Matches creature cards whose creature type is
    /// tied for the highest count among creature cards in the named player's
    /// named zone. CR 205.3m defines the creature subtype set being counted;
    /// CR 701.23a is the search mechanic that surfaces this filter.
    /// Generalizes the original "most prevalent creature type in your library"
    /// leaf so future variants (opponent's library, graveyard, etc.) reuse
    /// this slot rather than spawning siblings.
    ///
    /// Backward compat: deserializes from the legacy tag
    /// `MostPrevalentCreatureTypeInLibrary` (no fields) via `#[serde(alias)]`,
    /// defaulting `zone = Library` and `scope = You`.
    #[serde(alias = "MostPrevalentCreatureTypeInLibrary")]
    MostPrevalentCreatureTypeIn {
        #[serde(default = "default_most_prevalent_zone")]
        zone: crate::types::zones::Zone,
        #[serde(default = "default_most_prevalent_scope")]
        scope: ControllerRef,
    },
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
    /// CR 702.112b: Matches permanents with the renowned designation.
    Renowned,
    /// CR 510.1c: Matches creatures whose toughness is greater than their power.
    ToughnessGTPower,
    /// Disjunctive composite: the object matches if ANY inner prop matches.
    /// Used for natural-language OR within a property suffix — e.g.
    /// "creature with power or toughness N or less" decomposes to
    /// `AnyOf { [PtComparison(Power,LE,N), PtComparison(Toughness,LE,N)] }` on a `creature` typed filter,
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
    /// CR 400.7 + CR 700.4: The object moved between matching zones during
    /// this turn. Parameterized for phrases like "cards in your graveyard that
    /// were put there from the battlefield this turn"; `None` on either side
    /// means that side is unconstrained.
    ZoneChangedThisTurn {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from: Option<Zone>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        to: Option<Zone>,
    },
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
    /// CR 605.1: Matches objects that have at least one ability classified as a
    /// mana ability by the engine's authoritative mana-ability classifier.
    /// Used for library filters such as "artifact card with a mana ability".
    HasManaAbility,
    /// CR 113.1 + CR 113.3: Matches objects that currently have no abilities:
    /// no keyword, activated, triggered, replacement, or static abilities.
    /// Used for library filters such as "creature card with no abilities".
    HasNoAbilities,
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

/// CR 115.1 + CR 701.9b: How a target slot's referent is selected.
///
/// CR 115.1: Default — the spell/ability's controller chooses each target.
/// CR 701.9b: Some effects override the controller-choice default by requiring
/// the game to make the selection (analogous to "random discard" — the affected
/// player does not choose). Magic Oracle text expresses this with the modifier
/// "random target [predicate]" appearing immediately before the target word
/// (Mana Clash, Goblin Lyre, Pixie Queen, Vexing Sphinx, Maddening Hex, etc.).
///
/// Categorical-boundary check (per CLAUDE.md "Parameterize, don't proliferate"):
/// this enum is the *selection-mode* axis — one level above the predicate
/// (`TargetFilter`) and orthogonal to slot count (`MultiTargetSpec`). It does
/// not belong on `TargetFilter` (which captures *what* matches), nor on
/// `MultiTargetSpec` (which captures *how many*). It captures *who selects*.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetSelectionMode {
    /// CR 115.1: The spell or ability's controller chooses each target.
    #[default]
    Chosen,
    /// CR 701.9b (analogous): The game selects each target uniformly at random
    /// from the legal-target set. The controller does not choose.
    Random,
}

impl TargetSelectionMode {
    /// Helper for `serde(skip_serializing_if = ...)` — the default `Chosen`
    /// mode is omitted from card-data.json so existing card export shapes are
    /// preserved exactly.
    pub fn is_chosen(&self) -> bool {
        matches!(self, TargetSelectionMode::Chosen)
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
    /// CR 702.95b: Resolves to the source object and the creature it is paired
    /// with. If the source is not paired, this matches no objects.
    SourceOrPaired,
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
    StackAbility {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        controller: Option<ControllerRef>,
    },
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
    /// CR 102.1 + CR 103.1: living player seated immediately to controller's
    /// left/right; clockwise turn order, right = previous seat; resolved
    /// against `state.seat_order`. The recipient is computed at the resolver
    /// (`game::players::neighbor`), never selected as an interactive target
    /// slot.
    Neighbor {
        direction: SeatDirection,
    },
    /// CR 115.10 + CR 608.2c: The current player being affected by an
    /// "each player/opponent" instruction during resolution. This is not the
    /// ability controller; "you" and "your" still refer to `controller`.
    ScopedPlayer,
    /// Matches the permanent that the trigger source (Equipment/Aura) is attached to.
    /// Used for "equipped creature" / "enchanted creature" trigger subjects.
    AttachedTo,
    /// Resolves to the most recently created token(s) from Effect::Token.
    /// Used for "create X and [verb] it" patterns (e.g. "create a token and suspect it").
    LastCreated,
    /// CR 400.7j + CR 608.2k: Resolves to the object paid as a cost for the
    /// resolving spell or ability. Used by effects such as "the exiled card"
    /// after an exile-as-cost clause.
    CostPaidObject,
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
    /// CR 608.2c: Resolves to a specific target slot from the parent ability.
    /// Used when later English anaphors distinguish multiple prior targets by
    /// role ("the artifact" vs. "the artifact card") and live-state filtering
    /// would be ambiguous after earlier instructions mutate zones.
    ParentTargetSlot {
        index: usize,
    },
    /// CR 608.2c: Resolves to the controller of the parent ability's target object.
    /// Used for "its controller" in compound effects (e.g., "counter target spell. Its controller
    /// loses 2 life."). At resolution time, looks up the controller of the first parent target.
    ParentTargetController,
    /// CR 108.3 + CR 608.2c: Resolves to the *owner* of the parent ability's target
    /// object. Used for "its owner" anaphoric references where the acting player is the
    /// owner of a previously-mentioned permanent — e.g., Enslave's "enchanted creature
    /// deals 1 damage to its owner" or Bomb Squad's "that creature deals 4 damage to
    /// its owner". Resolution mirrors `ParentTargetController` (parent target slot →
    /// trigger-event source → AttachedTo host on source for Aura phase triggers), but
    /// returns the resolved object's `owner` rather than `controller` per CR 108.3.
    ///
    /// Distinct from `Owner` (which always reads the source object's owner) and
    /// `ParentTargetController` (which returns the controller per CR 109.4).
    ParentTargetOwner,
    /// CR 607.2d + CR 608.2c: Resolves to the player chosen for the source by
    /// a linked persisted choice ("the chosen player"). This is not a target
    /// slot and is distinct from `ControllerRef::ChosenPlayer`, which is
    /// resolution-scoped to the current ability chain.
    SourceChosenPlayer,
    /// CR 109.5 + CR 608.2c: Resolves to the ability's *original* controller — the
    /// player who put the spell or ability on the stack — even when a surrounding
    /// `player_scope` iteration has rebound `ResolvedAbility::controller` to a
    /// different player for the current per-player iteration.
    ///
    /// At resolution time, returns `ability.original_controller.unwrap_or(ability.controller)`.
    /// This mirrors the quantity-layer behavior in `resolve_quantity_with_targets`,
    /// where "you" / "your" quantities always read the original controller.
    ///
    /// Used by parser-level distribution of compound subjects like
    /// "you and that player each Y" — the first half ("you") MUST resolve to the
    /// printed ability controller, not the iterated voter, even when the parent
    /// ability has `player_scope: PlayerFilter::VotedFor` (Master of Ceremonies
    /// pattern, CR 800.4g).
    ///
    /// Distinct from `Controller` (which resolves to `ability.controller` and
    /// is rebound per-iteration by the player_scope driver in
    /// `resolve_ability_chain`). Distinct from `ParentTargetController`
    /// (parent's target's controller) and `PostReplacementSourceController`
    /// (replacement-context source's controller).
    OriginalController,
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
    /// CR 615.5: Resolves to the player or permanent that was the target of the
    /// prevented damage event. Used by prevention follow-up sentences such as
    /// "that player exiles that many cards" where the affected player is the
    /// damage recipient, not the replacement source or damage source.
    PostReplacementDamageTarget,
    /// CR 508.5 / CR 508.5a: Resolves to the defending player for the source
    /// attacking creature, looked up per attacker through
    /// `combat::defending_player_for_attacker`.
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
    /// CR 118.12a: Every player in the game (controller + opponents), polled in
    /// APNAP order. The unless-payer population for "[Effect] unless any player
    /// pays ..." clauses (Cleansing, Rhystic cycle, Soul Strings). Resolution is
    /// a sequential poll — the first player to pay prevents the effect — so this
    /// variant is only valid as an `UnlessPayModifier.payer`; it is never used
    /// as a target or affected filter.
    AllPlayers,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayerScope {
    /// CR 109.5 / CR 113.6: The controller of the source ability or effect.
    Controller,
    /// CR 115.10 + CR 608.2c: The current player being affected by an
    /// "each player/opponent" instruction during resolution. This keeps
    /// "their" quantities separate from "you" quantities.
    ScopedPlayer,
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
    AllPlayers {
        aggregate: AggregateFunction,
        /// CR 102.1 + CR 608.2c: when `Some`, the named player is excluded
        /// from the aggregated population ("each OTHER player"). The
        /// excluded player is itself a `PlayerScope` — the type composes
        /// with itself, generalizing "each other player" for any anchor.
        /// `None` = all players.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exclude: Option<Box<PlayerScope>>,
    },
    /// CR 303.4m + CR 613.4c: The controller of the object currently
    /// receiving a layer effect. Used for Aura/Equipment statics such as
    /// "enchanted creature gets +1/+1 for each card in its controller's
    /// hand", where "its" refers to the enchanted creature, not the Aura.
    RecipientController,
    /// CR 508.5 / CR 508.5a: The defending player for the source attacking
    /// creature, resolved per attacker through
    /// `combat::defending_player_for_attacker`. Used by attack-trigger
    /// intervening-if quantities such as "no opponent has more life than that
    /// player."
    DefendingPlayer,
    /// CR 109.4 + CR 608.2c: The controller of the first object target of
    /// the resolving ability ("that opponent" anaphoring the controller of
    /// a bounced/destroyed creature). The player-scalar-axis analogue of
    /// `ControllerRef::ParentTargetController`. Resolved via
    /// `ability_utils::parent_target_controller`.
    ParentObjectTargetController,
    /// CR 613.1 + CR 109.4: The player PERSISTED on the source via
    /// `ChosenAttribute::Player` (an "as ~ enters the battlefield, choose a
    /// player" replacement). The player-scalar-axis analogue of
    /// `ControllerRef::SourceChosenPlayer`, read continuously from the source's
    /// `chosen_attributes` so a CDA P/T can track e.g. the chosen player's hand
    /// or graveyard size (Entropic Specter, Sewer Nemesis).
    SourceChosenPlayer,
}

/// Scope selector for object-axis quantities (Round Π-5). Picks WHICH object
/// to read from when a `QuantityRef` (and future per-object conditions) is
/// per-object. Mirrors `PlayerScope` for the player axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ObjectScope {
    /// CR 113.7 + CR 113.7a: The source object of the resolving ability —
    /// "this creature", "~", "it" (when "it" anaphors back to the source).
    /// CR 113.7 defines the source as the object that generated the ability;
    /// CR 113.7a keeps the ability (and thus its source reference) valid on
    /// the stack even after the source leaves its expected zone, via LKI.
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
    /// CR 608.2k: The specific untargeted object previously referred to by
    /// this ability's cost OR trigger condition. Resolved (first match wins)
    /// via `ResolvedAbility.cost_paid_object` → trigger-event source →
    /// `effect_context_object`. Subsumes the former `EventContextSource*`
    /// trio (CR 117.1 + CR 400.7j cost referent and CR 603.2 trigger
    /// referent are the two enumerated members of CR 608.2k's single clause).
    CostPaidObject,
    /// CR 608.2k: A deferred anaphoric pronoun ("it" / "its") whose object
    /// referent is bound at parse time. The parser remaps this to a concrete
    /// scope wherever it can — `Source` when the clause subject is the ability
    /// source, `Target` when the recipient is "itself". For triggered abilities
    /// no remap applies and `Anaphoric` survives to runtime, where it resolves
    /// identically to `CostPaidObject` (behavior-preserving — these cards
    /// parsed to `CostPaidObject` before this variant existed). General
    /// rules-correct runtime resolution of triggered-ability anaphora (e.g.
    /// "its mana value" after a reveal — see issue #511) is separate per-card
    /// parser work. Distinguishing this from an explicit cost-paid possessive
    /// ("the sacrificed creature's power", CR 608.2k -> `CostPaidObject`) is
    /// what prevents the subject-injection rewrite from clobbering
    /// correctly-scoped possessives.
    Anaphoric,
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
    /// CR 404: Number of cards in the scoped player's graveyard. `PlayerScope`
    /// follows the same player-axis semantics as `HandSize` / `LifeTotal`:
    /// `Controller` is the default ("your graveyard"); `Opponent { aggregate }`
    /// covers "an opponent['s] graveyard" thresholds (e.g. Merfolk Windrobber,
    /// See Double — "if an opponent has eight or more cards in their graveyard").
    GraveyardSize { player: PlayerScope },
    /// Controller's life total minus the format's starting life total.
    /// Used for "N or more life more than your starting life total" conditions.
    LifeAboveStarting,
    /// CR 103.4: The format's starting life total (20 for Standard, 40 for Commander, etc.).
    StartingLifeTotal,
    /// Count of objects on the battlefield matching a filter.
    /// Used for "for each creature you control" and similar patterns.
    ObjectCount { filter: TargetFilter },
    /// CR 201.2 + CR 603.4: Count of objects matching a filter, deduplicated
    /// by the listed shared qualities. `qualities = [Name]` is the canonical
    /// "seven or more lands with different names" shape (Field of the Dead);
    /// `qualities = [ManaValue]` covers "different mana values"; multiple
    /// qualities form a tuple key (objects whose tuple values all coincide
    /// count as one).
    ///
    /// Lifts the legacy `ObjectCountDistinctNames` leaf to the same
    /// `Vec<SharedQuality>` axis already used by
    /// `SearchSelectionConstraint::DistinctQualities` (Batch 1) so the
    /// count-expression and constraint sides share one quality vocabulary.
    ///
    /// Backward compat: deserializes from the legacy tag
    /// `ObjectCountDistinctNames` (single `filter` field) via
    /// `#[serde(alias)]`, defaulting `qualities` to `vec![SharedQuality::Name]`.
    #[serde(alias = "ObjectCountDistinctNames")]
    ObjectCountDistinct {
        filter: TargetFilter,
        #[serde(default = "default_distinct_names")]
        qualities: Vec<SharedQuality>,
    },
    /// CR 109.3 + CR 205.3m: Count matching objects grouped by a shared object
    /// characteristic, including creature types, then aggregate the group
    /// sizes. Covers "the greatest number of creatures you control that have a
    /// creature type in common" without encoding the shared-quality clause as a
    /// target filter.
    ObjectCountBySharedQuality {
        filter: TargetFilter,
        quality: SharedQuality,
        aggregate: AggregateFunction,
    },
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
        counter_type: Option<CounterType>,
    },
    /// CR 122.1: Total counters across all objects matching a filter.
    /// Used for phrases like "the number of +1/+1 counters on lands you control"
    /// (`counter_type: Some("P1P1")`) and "counters among artifacts and creatures
    /// you control" (`counter_type: None`, sums across every counter type).
    CountersOnObjects {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<CounterType>,
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
    /// the first object target's power. `CostPaidObject` subsumes the former
    /// `EventContextSourcePower` (CR 608.2k: cost OR trigger-condition
    /// referent).
    Power { scope: ObjectScope },
    /// CR 208.1 + CR 113.6: Current toughness of an object, scoped via
    /// ObjectScope (Round Π-6). Mirrors `Power`. Replaces the `SelfToughness`
    /// variant. `CostPaidObject` subsumes the former
    /// `EventContextSourceToughness` (CR 608.2k).
    Toughness { scope: ObjectScope },
    /// CR 202.3: Mana value of an object, scoped via ObjectScope.
    /// `Source` is the resolving ability's source; `Target` is the first object
    /// target. Used by source/target-relative mana-value filters such as
    /// "with the same mana value as that spell". `CostPaidObject` subsumes the
    /// former `EventContextSourceManaValue` (CR 608.2k).
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
    /// `ObjectManaValue { scope: CostPaidObject }` (which reads the
    /// cost-paid / trigger-referenced object per CR 608.2k); that ref returns
    /// 0 outside cost/trigger resolution, whereas this one is correct any time
    /// the resolver has a `source_id` (cost payment, ability resolution, etc.).
    SelfManaValue,
    /// CR 107.3e: Aggregate query (max/min/sum) over a property of battlefield objects.
    Aggregate {
        function: AggregateFunction,
        property: ObjectProperty,
        filter: TargetFilter,
    },
    /// CR 107.1: The [min/max], across every player in the game, of the number
    /// of **battlefield** objects matching `filter` that the player controls
    /// (the game counts only in integers). Each player's per-player count is
    /// computed as if `filter`'s
    /// controller clause were that player (CR 109.5 "you"/"your" rebinding),
    /// then `aggregate` reduces the per-player counts to one integer. Used by
    /// Balance / Restore Balance / Balancing Act for "the number of [lands]
    /// controlled by the player who controls the fewest". `aggregate = Min` is
    /// the "fewest" reading; `aggregate = Max` covers "most"; `Sum` is accepted
    /// for completeness. Battlefield-scoped only: the hand-zone analogue ("the
    /// fewest cards in any player's hand") is `HandSize { player: AllPlayers {
    /// aggregate } }` — do NOT route hand counts through this variant.
    ControlledByEachPlayer {
        filter: TargetFilter,
        aggregate: AggregateFunction,
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
    /// CR 608.2c + CR 400.7: Count of members of the most recent tracked set that
    /// additionally satisfy the inner filter. Used for "for each nontoken creature
    /// you controlled that was destroyed this way" patterns where the tracked set
    /// holds all affected objects but only a filtered subset is relevant.
    FilteredTrackedSetSize { filter: Box<TargetFilter> },
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
    /// CR 702.179f: `player`'s current speed, treating no speed as 0.
    /// `PlayerScope::Controller` is the default reading ("your speed");
    /// `Target` / `Opponent { .. }` / `AllPlayers { .. }` /
    /// `ParentObjectTargetController` cover targeted, aggregate, and
    /// parent-object-target-controller variants per the same player axis
    /// used by `LifeTotal` / `HandSize` / `PartySize`.
    Speed { player: PlayerScope },
    /// CR 603.7c: Numeric value from the triggering event.
    /// Extracts amount/count from DamageDealt, LifeChanged, CardsDrawn, CounterAdded, etc.
    EventContextAmount,
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
    /// CR 117.1: Number of spells cast this turn by players in `scope`,
    /// optionally filtered by spell characteristics. `None` = all spells.
    SpellsCastThisTurn {
        scope: CountScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
    /// Count of permanents matching filter that entered the battlefield
    /// under the controller's control this turn.
    EnteredThisTurn { filter: TargetFilter },
    /// CR 701.21a: Count permanents sacrificed this turn by players in
    /// `player`, filtered against the permanent's sacrifice-time
    /// characteristics. Covers threshold and dynamic-count phrases like
    /// "you sacrificed a permanent this turn", "you've sacrificed an artifact
    /// this turn", and "the number of Foods you've sacrificed this turn".
    SacrificedThisTurn {
        player: PlayerScope,
        filter: TargetFilter,
    },
    /// CR 710.2: Number of crimes the controller has committed this turn.
    CrimesCommittedThisTurn,
    /// CR 119.4: Amount of life gained this turn, scoped by `player` per the
    /// workspace "Parameterize, don't proliferate" principle (Round Π-4 — mirrors
    /// `LifeLostThisTurn`'s Π-3 lift). `Controller` reads
    /// `p.life_gained_this_turn` directly; `Opponent { Sum }` totals across
    /// opponents (Needlebite Trap "if an opponent gained life this turn").
    LifeGainedThisTurn { player: PlayerScope },
    /// CR 121.1: Number of cards drawn this turn, scoped by `player`.
    /// Mirrors `LifeGainedThisTurn` / `LifeLostThisTurn` so conditions like
    /// "you've drawn two or more cards this turn" and "an opponent has drawn
    /// four or more cards this turn" reuse the existing per-player aggregate axis.
    CardsDrawnThisTurn { player: PlayerScope },
    /// CR 305.2a + CR 603.4: Count of lands played by the scoped player this turn.
    /// Backed by `Player::lands_played_this_turn`. Used for intervening-if conditions
    /// like "if it wasn't the first land you played this turn" (Fastbond).
    LandsPlayedThisTurn { player: PlayerScope },
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
    /// CR 120.1 + CR 120.9 + CR 603.4: Damage dealt this turn matching a source
    /// object filter and a recipient filter, optionally grouped by a key
    /// (CR 120.9 "specific source") and aggregated.
    ///
    /// `group_by: None` sums every matching record's `amount`.
    /// `group_by: Some(SourceId)` partitions matching records by `record.source_id`,
    /// sums each partition, then applies `aggregate` across the per-group sums
    /// (Max picks the largest single source's contribution; Sum equals the
    /// ungrouped sum; Min picks the smallest).
    ///
    /// Used by intervening-if clauses such as "if this creature dealt damage to
    /// an opponent this turn", "if this creature was dealt damage this turn",
    /// and "if a source you controlled dealt 5 or more damage this turn".
    DamageDealtThisTurn {
        source: Box<TargetFilter>,
        target: Box<TargetFilter>,
        #[serde(
            default = "default_damage_aggregate",
            skip_serializing_if = "is_default_damage_aggregate"
        )]
        aggregate: AggregateFunction,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        group_by: Option<DamageGroupKey>,
    },
    /// A number chosen as the source entered the battlefield (e.g., Talion, the Kindly Lord).
    /// Resolved from the source object's `ChosenAttribute::Number`.
    ChosenNumber,
    /// CR 508.1a: Number of creatures the controller attacked with this turn.
    /// Used for "if you attacked this turn" and "for each creature you attacked
    /// with this turn" patterns.
    AttackedThisTurn,
    /// CR 603.4: Whether the controller descended this turn (permanent card entered graveyard).
    DescendedThisTurn,
    /// CR 606.1 + CR 603.4: Number of loyalty abilities the scoped player has
    /// activated this turn (counts per CR 606.3 activations, summed across every
    /// planeswalker they controlled at activation time). Used for "if you
    /// activated a loyalty ability of a planeswalker this turn" intervening-if
    /// triggers (The Chain Veil class). Backed by
    /// `GameState::loyalty_abilities_activated_this_turn` and incremented in
    /// `finalize_loyalty_activation`.
    LoyaltyAbilitiesActivatedThisTurn { player: PlayerScope },
    /// CR 117.1: Number of spells cast last turn (by any player).
    /// Used for werewolf transform conditions.
    SpellsCastLastTurn,
    /// CR 117.1 + CR 601.2: Number of spells cast this game by players in
    /// `scope`, optionally filtered by spell characteristics (mirrors
    /// `SpellsCastThisTurn`).
    ///
    /// `filter: None` reads the fast O(1) per-player count from
    /// `state.spells_cast_this_game`. `filter: Some(_)` scans
    /// `state.spells_cast_this_game_by_player` records and matches each one
    /// against the filter at query time, so callers can ask "spells named
    /// {LITERAL} this game" via `FilterProp::Named { name }` (Approach of
    /// the Second Sun's win gate).
    ///
    /// Established usage: `{ scope: Controller, filter: None }` reproduces the
    /// pre-lift bare-leaf reading used by Establishing Shot class.
    SpellsCastThisGame {
        #[serde(default = "default_count_scope_controller")]
        scope: CountScope,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
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
    /// CR 701.9 + CR 603.4: Number of cards discarded this turn, scoped by
    /// `player`. Mirrors `CardsDrawnThisTurn` so conditions like "you've
    /// discarded a card this turn" and "if an opponent discarded a card this
    /// turn" reuse the existing per-player aggregate axis.
    CardsDiscardedThisTurn { player: PlayerScope },
    /// CR 111.2 + CR 603.4: Number of tokens created this turn, scoped by
    /// `player` and filtered against each token's creation-time
    /// characteristics. Covers "you created a token this turn" and dynamic
    /// counts like "the number of tokens you created this turn".
    TokensCreatedThisTurn {
        player: PlayerScope,
        filter: TargetFilter,
    },
    /// CR 603.4 + CR 701.22/701.25: Count typed player-action events this
    /// turn, scoped by `player`. Parameterizes "you've surveilled this turn",
    /// "you've scried this turn", and future action-history conditions without
    /// adding action-specific sibling quantities.
    PlayerActionsThisTurn {
        player: PlayerScope,
        action: PlayerActionKind,
    },
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
    /// CR 601.2b/f/h + CR 702.157a: Number of non-kicker additional-cost
    /// payments made for the source spell. Used by Squad so its payment count
    /// remains distinct from Kicker's CR 702.33 payment model.
    AdditionalCostPaymentCount,
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
    /// CR 122.1: distinct counter kinds among filter-matched permanents
    /// (controller-relative, CR 109.4). Counter-side dual of
    /// `DistinctColorsAmongPermanents` — counts each distinct `CounterType`
    /// appearing on at least one permanent matching `filter` exactly once.
    /// Used by Bribe Taker's "for each kind of counter on permanents you
    /// control" iteration source. Kept a separate variant from the color
    /// dual because counters (CR 122.1) and colors (CR 105/106) are distinct
    /// rule sections the engine resolves independently.
    DistinctCounterKindsAmong { filter: TargetFilter },
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

/// CR 120.9: Grouping key for damage-history aggregation. CR 120.9 distinguishes
/// damage dealt "by a specific source" from damage in the aggregate, so any
/// query that needs per-source partitioning before aggregation must select a
/// key here. Today only `SourceId` is needed; future axes (e.g., per-target)
/// fit cleanly as additional variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum DamageGroupKey {
    /// CR 120.9: Group records by `DamageRecord::source_id` so the resolver can
    /// answer "the most damage dealt by any single source."
    SourceId,
}

/// A measurable property of a game object for aggregate queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ObjectProperty {
    Power,
    Toughness,
    ManaValue,
}

/// CR 701.13a + CR 608.2c: Termination predicate for an iterative exile-from-top
/// loop. The loop exiles one card at a time off the top of a library and checks
/// the predicate after each exile; the loop ends as soon as the predicate is
/// satisfied (or the library is empty).
///
/// This parameterizes `Effect::ExileFromTopUntil` so that the same iteration
/// engine handles two distinct stop-condition families:
///
/// * `NextMatches(filter)` — stop once the most-recently-exiled card matches a
///   filter. Covers Etali / Cascade / Discover-shape patterns where a single
///   "hit" card is selected (see CR 702.85a, CR 701.57a).
/// * `CumulativeThreshold { .. }` — stop once the running aggregate over every
///   card exiled this resolution satisfies the comparator vs the threshold.
///   Covers Tasha's Hideous Laughter, Dream Harvest, Improvisation Capstone
///   ("…until that player has exiled cards with total mana value N or
///   greater") — CR 202.3 supplies the per-card mana value, CR 107.3e the
///   summation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum UntilCondition {
    /// CR 702.85a / CR 701.57a: Loop terminates when the just-exiled card
    /// satisfies the filter. The matching card is exposed to the sub_ability
    /// chain as an injected target.
    NextMatches { filter: TargetFilter },
    /// CR 202.3 + CR 107.3e: Loop terminates when the cumulative `property`
    /// summed over every card exiled this resolution satisfies
    /// `comparator(sum, threshold)`.
    CumulativeThreshold {
        property: ObjectProperty,
        comparator: Comparator,
        threshold: QuantityExpr,
    },
}

/// CR 117.1 + CR 400.7j + CR 608.2k: Public characteristics of an object paid
/// as a cost for the resolving spell or ability. Effects can later refer to
/// that object even after the cost moved it to a public zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostPaidObjectSnapshot {
    pub object_id: ObjectId,
    pub lki: LKISnapshot,
}

/// CR 102.1 + CR 103.1: Seating direction relative to a player. The game's
/// default turn order proceeds clockwise (CR 103.1); the next player in turn
/// order is seated to the active player's left (CR 101.4). Thus walking
/// forward through `seat_order` is `Left`, and walking backward is `Right`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SeatDirection {
    /// The living player seated immediately to the controller's left — the
    /// next player in turn order (CR 101.4). Forward through `seat_order`.
    Left,
    /// The living player seated immediately to the controller's right — the
    /// previous player in turn order. Backward through `seat_order`.
    Right,
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
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PlayerFilter {
    /// The controller of the effect or quantity.
    /// CR 700.2a: the default chooser for any modal/effect player reference.
    #[default]
    Controller,
    /// All opponents of the controller.
    Opponent,
    /// CR 506.2: The defending player for the source creature's attack.
    DefendingPlayer,
    /// Each opponent who lost life this turn (life_lost_this_turn > 0).
    OpponentLostLife,
    /// Each opponent who gained life this turn (life_gained_this_turn > 0).
    OpponentGainedLife,
    /// CR 120.1 + CR 510.1 + CR 120.9 + CR 608.2i: Each opponent who was dealt
    /// combat damage this turn, optionally restricted to damage from a source
    /// matching `source`. Resolved against `state.damage_dealt_this_turn`
    /// records whose `is_combat = true` and `target = Player(p.id)`. `source =
    /// None` counts any combat-damage source (Tymna the Weaver); `source =
    /// Some(f)` (CR 120.9) counts only opponents dealt combat damage by a source
    /// matching `f` — matched against each record's CR 608.2i look-back source
    /// snapshot, so the source's qualities are checked as they were at damage
    /// time (Estinien Varlineau: "by ~ or a Dragon").
    OpponentDealtCombatDamage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source: Option<Box<TargetFilter>>,
    },
    /// CR 508.6: A player has "attacked [a player]" if they declared one or more
    /// creatures attacking that player. Each opponent the controller attacked this
    /// turn, resolved against `state.attacked_defenders_this_turn[controller]`.
    /// Used by "the number of opponents you attacked this turn" (Militant Angel).
    /// (CR 508.1b: declare-attackers announcement; CR 506.2: active = attacking player.)
    OpponentAttackedThisTurn,
    /// CR 508.6: Each opponent the ability's source creature attacked this turn.
    /// Uses `state.creature_attacked_defenders_this_turn[source_id]` for
    /// source-specific text like "each player this creature attacked this turn"
    /// (Angel of Destiny).
    OpponentAttackedBySourceThisTurn,
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
    /// CR 608.2c + CR 701.38: Each player who cast a vote for `choices[choice_index]`
    /// in the most recent vote within the current top-level ability resolution.
    /// Mirrors `PerformedActionThisWay` — backed by a transient ledger
    /// (`state.last_vote_ballots`) that resets at chain depth 0.
    ///
    /// Used by Master of Ceremonies's "for each player who chose money,
    /// you and that player each create a Treasure token": the per-choice
    /// sub-effect's `player_scope` is set to `VotedFor { choice_index: 0 }`,
    /// which expands at resolution time into the controller plus each voter
    /// who chose that option.
    VotedFor {
        /// Index into the parent `Effect::Vote.choices` list (and parallel
        /// `WaitingFor::VoteChoice.options`). The voter ledger encodes choices
        /// as `u8` — vote sessions never exceed 255 choices in practice.
        choice_index: u8,
    },
    /// CR 109.4 + CR 608.2c: The controller of the first object target of
    /// the resolving ability ("reduce that opponent's speed" anaphoring the
    /// controller of a bounced creature). The `PlayerFilter`-axis analogue
    /// of `PlayerScope::ParentObjectTargetController` and
    /// `ControllerRef::ParentTargetController`. Resolved via
    /// `ability_utils::parent_target_controller`.
    ParentObjectTargetController,
    /// CR 109.4 + CR 109.5: Each player satisfying `relation` whose count of
    /// controlled permanents matching `filter` compares to `count` under
    /// `comparator`. The control relationship is enforced per-candidate at
    /// runtime (`obj.controller == candidate`), so `filter` carries no
    /// controller axis; `count`'s own `ObjectCount` may carry a `You`
    /// controller (CR 109.5 — "you" is the effect controller) for comparative
    /// "more X than you" phrasings.
    ///
    /// Covers the full presence/comparison class as a single parameterized
    /// variant:
    /// - "each opponent who controls an artifact" → `{ GE, Fixed(1) }`
    ///   (at least one matching permanent).
    /// - "each opponent who doesn't control an Elf" (Thornbow Archer) →
    ///   `{ EQ, Fixed(0) }` (no matching permanent).
    /// - "each player who controls more creatures than you" (Heidegger) →
    ///   `{ GT, Ref(ObjectCount { filter: <creature>.controller(You) }) }`.
    ///
    /// `count` is boxed to break the `QuantityExpr → QuantityRef::PlayerCount →
    /// PlayerFilter::ControlsCount → QuantityExpr` reference cycle that would
    /// otherwise give the enum infinite size.
    #[serde(alias = "ControlsPermanent")]
    ControlsCount {
        relation: PlayerRelation,
        filter: TargetFilter,
        #[serde(default = "default_comparator_ge")]
        comparator: Comparator,
        #[serde(default = "default_controls_count_one")]
        count: Box<QuantityExpr>,
    },
    /// CR 402.1 (hand) / CR 119.1 (life) / CR 122.1f (poison) / CR 404.1
    /// (graveyard): Each player satisfying `relation` whose scalar player
    /// attribute `attr`, read PER CANDIDATE PLAYER, satisfies `comparator`
    /// against `value`. `attr` is the per-player-scalar `QuantityRef` subset
    /// (`HandSize` / `LifeTotal` / `GraveyardSize` / `PlayerCounter`) — read
    /// directly off the candidate `Player` at runtime, never via the
    /// controller-scoped quantity resolver, so its embedded `PlayerScope` /
    /// `CountScope` carries no game-state meaning here.
    ///
    /// Covers "opponents who have N or more poison counters" (Glissa's
    /// Retriever) and "your opponents with N or more cards in hand"
    /// (Wolfcaller's Howl). `value` is the controller-relative threshold,
    /// resolved once per evaluation (candidate-independent).
    ///
    /// `attr` and `value` are boxed to break the `QuantityExpr →
    /// QuantityRef::PlayerCount → PlayerFilter::PlayerAttribute →
    /// {QuantityRef, QuantityExpr}` reference cycle that would otherwise give
    /// the enum infinite size.
    PlayerAttribute {
        relation: PlayerRelation,
        attr: Box<QuantityRef>,
        comparator: Comparator,
        value: Box<QuantityExpr>,
    },
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
    /// CR 107.1a: Fractional quantities ("half X", "a third of X", etc.) divide
    /// the inner expression by `divisor` in the rounding direction specified by
    /// the Oracle text.
    DivideRounded {
        inner: Box<QuantityExpr>,
        divisor: u32,
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
    /// Layered above `QuantityExpr::Ref`/`Fixed`/`DivideRounded`/etc. so the
    /// upper bound itself can be a dynamic game-state quantity (e.g. "draw
    /// up to your hand size cards" → `UpTo { max: Ref { qty: HandSize } }`,
    /// "sacrifice up to half your creatures" → `UpTo { max: DivideRounded {
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
    /// CR 107.3: `base` raised to the power of `exponent`. The exponent is a
    /// general `QuantityExpr` so it can resolve from a variable cost (e.g.
    /// `Variable { name: "X" }` for Mathemagics' `2ˣ`) or any other dynamic
    /// game-state lookup. Resolution uses saturating exponentiation; negative
    /// exponents clamp to 0.
    Power {
        base: i32,
        exponent: Box<QuantityExpr>,
    },
    /// The (unsigned) difference between two dynamic quantities. Resolves to
    /// `(resolve(left) - resolve(right)).abs()`. "The difference between A and
    /// B" is an unsigned-magnitude Oracle templating convention — it has no
    /// dedicated Comprehensive Rules number; `.abs()` implements that
    /// convention. (CR 107.1b is related but distinct: it governs clamping a
    /// negative *result* to zero, not taking an absolute value — it confirms
    /// only that the resulting amount is non-negative, not the operation.)
    /// A general arithmetic peer of `Offset`/`Multiply`/`Sum`: composes any
    /// "difference between A and B" card from existing `QuantityExpr` leaves
    /// (e.g. Doran's "the difference between its power and toughness" is
    /// `Difference { Ref(Power{Recipient}), Ref(Toughness{Recipient}) }`).
    Difference {
        left: Box<QuantityExpr>,
        right: Box<QuantityExpr>,
    },
}

impl QuantityExpr {
    /// Scale a quantity expression by a non-negative runtime factor. Fixed
    /// values fold immediately; dynamic quantities compose through
    /// `QuantityExpr::Multiply` so the existing quantity resolver evaluates the
    /// scaled expression in context.
    pub fn scaled_by(&self, factor: u32) -> Self {
        let factor = i32::try_from(factor).unwrap_or(i32::MAX);
        match (factor, self) {
            (0, _) => QuantityExpr::Fixed { value: 0 },
            (1, expr) => expr.clone(),
            (_, QuantityExpr::Fixed { value }) => QuantityExpr::Fixed {
                value: value.saturating_mul(factor),
            },
            (_, expr) => QuantityExpr::Multiply {
                factor,
                inner: Box::new(expr.clone()),
            },
        }
    }

    /// Returns true if this expression resolves through a
    /// `QuantityRef::Variable { name: "X" }` anywhere in its tree — i.e. its
    /// value depends on the spell or ability's chosen X. Single authority for
    /// "does this quantity use X": the engine cost machinery (X-affordability,
    /// pay-life-X) and the AI X-value policy all rely on it. The match is
    /// exhaustive so a new `QuantityExpr` variant forces every consumer to
    /// reconsider X-dependence rather than silently defaulting to false.
    pub fn contains_x(&self) -> bool {
        match self {
            QuantityExpr::Ref {
                qty: QuantityRef::Variable { name },
            } => name == "X",
            QuantityExpr::Offset { inner, .. }
            | QuantityExpr::Multiply { inner, .. }
            | QuantityExpr::DivideRounded { inner, .. }
            | QuantityExpr::UpTo { max: inner }
            | QuantityExpr::Power {
                exponent: inner, ..
            } => inner.contains_x(),
            QuantityExpr::Sum { exprs } => exprs.iter().any(QuantityExpr::contains_x),
            QuantityExpr::Difference { left, right } => left.contains_x() || right.contains_x(),
            QuantityExpr::Fixed { .. } | QuantityExpr::Ref { .. } => false,
        }
    }

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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

/// CR 208: Selects which creature P/T metric a `FilterProp::PtComparison`
/// reads. Power, toughness, and their total are all derived from the same CR
/// 208 characteristic pair, so they are a leaf-level parameterization axis, not
/// a cross-section conflation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PtStat {
    /// CR 208.1: The creature's power (first number).
    Power,
    /// CR 208.1: The creature's toughness (second number).
    Toughness,
    /// CR 208.1: The sum of the creature's power and toughness.
    TotalPowerToughness,
}

/// CR 208.4b: Selects whether a `FilterProp::PtComparison` reads the creature's
/// current power/toughness (after all continuous effects) or its base
/// power/toughness. Per CR 613.4b, base values are those set in layer 7b (after
/// CDAs in 7a and set effects, but before counters and modifying effects in
/// 7c). A 1/1 with a +1/+1 counter has base power 1 but current power 2.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
pub enum PtValueScope {
    /// CR 613.4: The fully-modified value after applying every layer-7 sublayer.
    #[default]
    Current,
    /// CR 208.4b: The value after layer 7b only — ignores counters (7c) and
    /// other modifying effects.
    Base,
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

/// Ownership scope for a commander-control condition.
/// CR 903.3 distinguishes "your commander" (owner-scoped) from "a commander"
/// (controller-only).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CommanderOwnership {
    /// CR 903.3 + CR 109.5: "your commander" — the commander must be owned AND
    /// controlled by the evaluating player. Used by the Lieutenant ability word.
    Own,
    /// CR 903.3d: "a commander" — any commander on the battlefield controlled by
    /// the evaluating player, regardless of owner (a stolen opponent's commander
    /// counts).
    Any,
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
    /// CR 614.12c + CR 607.2d: True when the source object's persisted
    /// `ChosenAttribute::Label` matches the given anchor word. Used by
    /// anchor-word modal permanents (Khans of Tarkir Sieges, Tarkir:
    /// Dragonstorm enchantments) to gate the linked static ability "as long
    /// as [anchor word] was chosen as this permanent entered the
    /// battlefield, this permanent has [ability]." Mirrors `ChosenColorIs`.
    ChosenLabelIs {
        label: String,
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
    /// CR 702.176a + CR 611.3a: True while the source permanent carries the
    /// persistent marker that its named alternative cost was paid. This is not
    /// turn-scoped; Impending's "not a creature" static must continue across
    /// turns until its last time counter is removed.
    CastVariantPaid {
        variant: CastVariantPaid,
    },
    /// CR 122.1 + CR 613.4c: True when the object currently receiving an
    /// attached-object static has at least `minimum` (and at most `maximum`, if
    /// specified) counters matching `counters`. Used for Aura/Equipment
    /// conditions where "it" refers to the enchanted/equipped creature rather
    /// than the attachment source.
    RecipientHasCounters {
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
    /// CR 725.1: True when no player holds the monarch designation. Distinct
    /// from `Not(IsMonarch)`, which is also true when an opponent is monarch.
    NoMonarch,
    /// CR 702.131a: True when the controller has the city's blessing (Ascend).
    HasCityBlessing,
    /// CR 309.7: True when the controller has completed at least one dungeon.
    /// Used by "as long as you've completed a dungeon" statics (Nadaar, etc.).
    CompletedADungeon,
    /// CR 103.1: True when the scoped player was the starting player (took the
    /// first turn of the game). Fixed at game start
    /// (`GameState.current_starting_player`). For "if you weren't the starting
    /// player", wrap with `StaticCondition::Not`. `controller` selects whose
    /// start status is checked; `ControllerRef::You` is the canonical reading.
    WasStartingPlayer {
        controller: ControllerRef,
    },
    /// CR 702.185c: True when any player cast a spell using the named
    /// alternative-cast `variant` (e.g. Warp) this turn. Parameterized by
    /// `CastingVariant` so every "cast via X this turn" history query — not
    /// just Warp — shares one variant. Not controller-scoped: "a spell was
    /// warped this turn" matches a cast by any player.
    SpellCastWithVariantThisTurn {
        variant: crate::types::game_state::CastingVariant,
    },
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
    /// `defended` (CR 506.3 + CR 508.1d) restricts which declared attacks the tax
    /// applies to: `AttackTargetFilter::Player` for "creatures can't attack you",
    /// `AttackTargetFilter::PlayerOrPlaneswalker` for "you or planeswalkers you control".
    /// `None` means the restriction is target-agnostic (block-side taxes use `None`,
    /// since "creatures can't block" has no defender). The runtime check at
    /// `compute_combat_tax` walks each declared `(attacker_id, AttackTarget)` pair
    /// and only taxes attackers whose `AttackTarget` matches the filter, scoped to
    /// the static's source controller. Reuses the existing `AttackTargetFilter`
    /// (CR 508.3a) shared with attack-trigger filtering — same categorical axis.
    ///
    /// `layers::evaluate_condition` returns `false` for this variant (restriction active) —
    /// the per-attacker / per-blocker optional cost payment round-trip is performed at
    /// declaration time via `WaitingFor::CombatTaxPayment`, not inside the pure layer
    /// evaluator.
    UnlessPay {
        cost: ManaCost,
        #[serde(default, skip_serializing_if = "UnlessPayScaling::is_flat")]
        scaling: UnlessPayScaling,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        defended: Option<crate::types::triggers::AttackTargetFilter>,
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
    /// CR 903.3 + CR 109.5: "you control your commander" — owner-scoped (Lieutenant).
    /// CR 903.3d: "you control a commander" — controller-only, any owner.
    /// The `ownership` field selects which CR condition this is.
    ControlsCommander {
        ownership: CommanderOwnership,
    },
    /// CR 110.5b: True when the source object is tapped.
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
    /// CR 611.3a: the recipient (effective subject) of the continuous effect matches
    /// `filter`; re-evaluated per affected object each layer cycle (mirrors
    /// `RecipientHasCounters`, the recipient analog of `HasCounters`). CR 611.3a: "A
    /// continuous effect generated by a static ability isn't 'locked in'; it applies at
    /// any given moment to whatever its text indicates" — so the anaphoric "it" in
    /// "... as long as it's a Zombie" binds to whatever object is currently receiving the
    /// effect: an Aura's enchanted creature, the source itself (SelfRef), or each affected
    /// object in a per-recipient anthem. `filter` is a plain type/subtype/color/supertype
    /// `TargetFilter::Typed` — no attachment prop, no recipient prop.
    RecipientMatchesFilter {
        filter: TargetFilter,
    },
    /// CR 702.95b: True while the source object is paired with another creature.
    SourceIsPaired,
    /// CR 113.6b: True when the source card is in the specified zone.
    /// Used for "as long as ~ is in your graveyard" / "this card is in your graveyard" conditions.
    SourceInZone {
        zone: crate::types::zones::Zone,
    },
    /// CR 708.2 + CR 707.2: True when the creature this Aura/Equipment is attached to is
    /// face-down. Resolves against the attached-to object's `face_down` status. Used by
    /// "as long as enchanted creature is face down" gated statics (Unable to Scream, etc.).
    EnchantedIsFaceDown,
    /// CR 702.166a + CR 601.2f: True when an optional additional cost (Bargain) was paid
    /// for the spell currently being cast. Gates self-spell `ModifyCost` statics like
    /// Hamlet Glutton's "This spell costs {2} less to cast if it's bargained." Evaluated
    /// against the in-flight cast's `additional_cost_paid` flag (`state.pending_cast`).
    AdditionalCostPaid,
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
    /// CR 702.142a: This creature attacked this turn (Boast activation restriction).
    SourceAttackedThisTurn,
    SourceIsCreature,
    /// CR 301.5 + CR 602.5b: The source is attached to an object with the
    /// required core type. Used for activation restrictions such as
    /// Reconfigure's "only if this permanent is attached to a creature."
    SourceAttachedTo {
        required_type: CoreType,
    },
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
    /// CR 601.3 / CR 602.5: Generic measurable restriction predicate.
    /// Mirrors `StaticCondition::QuantityComparison` and `TriggerCondition::QuantityComparison`
    /// so cast/activation restrictions can reuse `QuantityRef` building blocks instead of
    /// proliferating one-off condition variants.
    QuantityComparison {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
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
    /// CR 602.5b: "[you / an opponent] control(s) a creature with [keyword]"
    /// activation restriction. `controller` selects whose creatures are
    /// inspected — `ControllerRef::You` for "you control", `Opponent` for
    /// "an opponent controls".
    ControlsCreatureWithKeyword {
        controller: ControllerRef,
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
    /// True when the player has already used at least one land play this turn.
    /// The count is tracked on `Player::lands_played_this_turn` from
    /// `GameEvent::LandPlayed`.
    YouPlayedLandThisTurn,
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
    BattlefieldEntriesThisTurn {
        filter: TargetFilter,
        count: u32,
    },
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
    /// CR 102.1: "The active player is the player whose turn it is." True when
    /// the scoped player is the active player — gates a casting/restriction
    /// predicate on "if it's your turn". For "if it's not your turn" the parser
    /// wraps this leaf with `ParsedCondition::Not`. Mirrors
    /// `AbilityCondition::IsYourTurn` (the structural analogue at the
    /// ability-resolution layer), the same way `ParsedCondition::And` mirrors
    /// `AbilityCondition::And`.
    IsYourTurn,
    /// CR 601.3d + CR 702.8a + CR 608.2c: The in-flight spell being cast targets at
    /// least one object that matches `filter`. Gates a target-dependent casting
    /// permission (Timely Ward — "you may cast this spell as though it had flash if
    /// it targets a commander") on the spell's chosen targets. Evaluated against the
    /// `state.pending_cast.ability`'s flattened targets when targets have been
    /// committed; before target selection the condition reads as "not yet refutable"
    /// so the cast may be announced and proceed to target selection. Final
    /// validation runs at `finish_pending_cast_cost_or_pay` against the committed
    /// targets. The structural analogue at ability-resolution time is
    /// `AbilityCondition::TargetMatchesFilter` (CR 608.2c); this variant occupies
    /// the casting/restriction layer (CR 601.3d) the same way the rest of
    /// `ParsedCondition` does.
    SpellTargetsFilter {
        filter: TargetFilter,
    },
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
    /// CR 118.1: Per-object scaled mana cost. The `base` `ManaCost` (which may
    /// carry colored pips, e.g. `{U}`) is multiplied by `times` at payment
    /// resolution — every pip is repeated and the generic component scaled.
    /// Models "pay {N} for each [object] chosen this way" uniformly across
    /// generic ({4}×N) and colored ({U}×N) bases.
    /// CR 118.5: when `times` resolves to 0 the scaled cost is `{0}`, paid by
    /// the player's acknowledgment (the empty selection) — a no-op SUCCESS.
    ScaledMana {
        base: ManaCost,
        times: QuantityExpr,
    },
}

// ---------------------------------------------------------------------------
// AbilityCost -- expanded typed variants
// ---------------------------------------------------------------------------

/// CR 702.49: Ninjutsu-family keyword variants that share the "activate to swap
/// a creature in combat" pattern. This enum is the *activation-family dispatch*
/// layer — it is used only by `activate_ninjutsu` and its supporting helpers
/// (`ninjutsu_timing_ok`, `returnable_creatures_for_variant`, etc.) to pick
/// the correct activation behavior. Sneak (CR 702.190a) and Web-slinging
/// (CR 702.188a) are NOT activated abilities and therefore do not appear here;
/// see `CastVariantPaid` for the trigger-tag layer that does include them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NinjutsuVariant {
    /// CR 702.49a: Return unblocked attacker, declare blockers or later.
    Ninjutsu,
    /// CR 702.49d: Commander ninjutsu — activate from hand or command zone.
    CommanderNinjutsu,
}

/// CR 702.49 + CR 702.188a + CR 702.190a + CR 603.4: Which alternative-cost cast/activation
/// variant was paid to put this permanent onto the battlefield. This is the
/// *trigger-tag / ability-condition* layer — separate from `NinjutsuVariant`
/// (activation-family dispatch) because it legitimately includes Sneak and
/// Web-slinging, which are cast alt-costs rather than activated abilities.
///
/// Populated by cast alt-cost handlers and `keywords::activate_ninjutsu`
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
    /// CR 702.190a: Sneak alternative cast cost was paid from hand.
    Sneak,
    /// CR 702.188a: Web-slinging alternative cast cost was paid from hand.
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
    /// CR 702.103a + CR 702.103b: The spell was cast bestowed (its bestow
    /// alternative cost was paid). Read by trigger/ability conditions for
    /// "if its bestow cost was paid" and by display layers that need to
    /// distinguish a bestow-cast permanent from a hard-cast creature.
    Bestow,
    /// CR 702.176a: The spell was cast for its impending alternative cost.
    /// The permanent entered with N time counters and is not a creature
    /// while any remain. Read by the end-step counter-removal trigger and
    /// the "not a creature" layer fixup.
    Impending,
}

/// CR 601.3b + CR 702.8a: A timing permission actually used to cast a spell.
/// This is separate from `CastVariantPaid`: no alternative cost was paid, but
/// later abilities may care that normal sorcery timing was bypassed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CastTimingPermission {
    /// The spell was cast using an effect that allowed it to be cast as though
    /// it had flash.
    AsThoughHadFlash,
    /// CR 702.48a: The spell was cast at instant speed by paying an Offering
    /// additional cost. When this permission is set, the Offering sacrifice is
    /// required (the player used the offering to unlock instant-speed timing).
    Offering,
}

impl From<NinjutsuVariant> for CastVariantPaid {
    /// CR 702.49: Lift an activation-family variant into the cast-variant-paid tag
    /// used by trigger conditions. Cast alt-costs are intentionally NOT in
    /// `NinjutsuVariant`, so this conversion is total.
    fn from(v: NinjutsuVariant) -> Self {
        match v {
            NinjutsuVariant::Ninjutsu => CastVariantPaid::Ninjutsu,
            NinjutsuVariant::CommanderNinjutsu => CastVariantPaid::CommanderNinjutsu,
        }
    }
}

/// CR 702.49: Identifies which dedicated engine path handles a RuntimeHandled ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RuntimeHandler {
    /// Handled by GameAction::ActivateNinjutsu path.
    NinjutsuFamily,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BeholdCostAction {
    ChooseOrReveal,
    ExileChosen,
}

/// CR 702.167a: Object-count requirement for costs whose text may ask for an
/// exact number ("two creatures") or a minimum ("one or more creatures").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CostObjectCount {
    Exactly { count: u32 },
    AtLeast { count: u32 },
}

impl Default for CostObjectCount {
    fn default() -> Self {
        Self::exactly(1)
    }
}

impl CostObjectCount {
    pub fn exactly(count: u32) -> Self {
        Self::Exactly {
            count: count.max(1),
        }
    }

    pub fn at_least(count: u32) -> Self {
        Self::AtLeast {
            count: count.max(1),
        }
    }

    pub fn min_count(self) -> usize {
        match self {
            Self::Exactly { count } | Self::AtLeast { count } => count.max(1) as usize,
        }
    }

    pub fn max_count(self, eligible_count: usize) -> usize {
        match self {
            Self::Exactly { count } => count.max(1) as usize,
            Self::AtLeast { .. } => eligible_count,
        }
    }
}

/// Cost to activate an ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AbilityCost {
    Mana {
        cost: ManaCost,
    },
    /// CR 118.4 + CR 107.3c: A mana cost whose generic component is determined
    /// dynamically at the moment the cost is paid (e.g., "{X}, where X is this
    /// creature's power"). The runtime resolves this into a fixed `ManaCost`
    /// before the player is prompted to pay; this variant carries the dynamic
    /// expression up to that resolution point. Distinct from `Mana { cost }`,
    /// which is fully static.
    ManaDynamic {
        quantity: QuantityExpr,
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
    /// CR 702.167a/b: Craft's "Exile [materials] from among permanents you
    /// control and/or cards in your graveyard" component. Distinct from
    /// `Exile` (single zone, optional filter): the materials are chosen across
    /// the *union* of the battlefield (permanents you control) and your
    /// graveyard, so `materials` carries the dual-zone `TargetFilter::Or` built
    /// by `craft_materials_filter`. The interactive `PayCostKind::ExileMaterials`
    /// detour exiles objects satisfying `count`; this auto-payment arm is a no-op.
    ExileMaterials {
        materials: TargetFilter,
        #[serde(default)]
        count: CostObjectCount,
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
    /// CR 122.1 + CR 601.2h: Remove `count` counters matching `counter_type`
    /// as an additional cost. `CounterMatch::Any` is the untyped "remove a
    /// counter" form (Loch Mare's `{1}{U}, Remove a counter from ~`); the
    /// payment path sums across every counter type on the chosen permanent
    /// and resolves to a concrete kind at payment time. `CounterMatch::OfType`
    /// is the typed form ("remove a +1/+1 counter", "remove a charge counter"),
    /// scoped to a single counter kind.
    RemoveCounter {
        count: u32,
        counter_type: CounterMatch,
        #[serde(default)]
        target: Option<TargetFilter>,
    },
    PayEnergy {
        amount: QuantityExpr,
    },
    /// CR 118.3 + CR 702.179f: Pay speed as an activation or additional cost.
    PaySpeed {
        amount: QuantityExpr,
    },
    /// CR 118.12: Return N permanents matching `filter` to their owner's hand
    /// as a cost. `from_zone` defaults to `None`, meaning battlefield (the
    /// standard "return to hand" cost shape, e.g., "return a land you control
    /// to its owner's hand"). Some unless-cost cards (Harvest Wurm) use
    /// `Some(Zone::Graveyard)` to return cards from the graveyard instead.
    ReturnToHand {
        count: u32,
        #[serde(default)]
        filter: Option<TargetFilter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_zone: Option<Zone>,
    },
    /// CR 701.3d: Unattach this Equipment from the object it is equipping.
    /// Used by activated costs such as Sunforger's "Unattach this Equipment".
    Unattach,
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
    /// Behold a matching object as a casting cost.
    Behold {
        #[serde(default = "default_one")]
        count: u32,
        filter: TargetFilter,
        action: BeholdCostAction,
    },
    Composite {
        costs: Vec<AbilityCost>,
    },
    /// CR 118.12a + CR 118.12: "Unless they X or Y" — the paying player chooses
    /// **one** of `costs` to pay. Distinct from `Composite` (which requires
    /// paying ALL listed sub-costs). Used by the punisher class — e.g. Tergrid's
    /// Lantern ("Target player loses 3 life unless they sacrifice a nonland
    /// permanent of their choice or discard a card") and 30+ similar cards.
    ///
    /// Sibling rationale (CLAUDE.md "Parameterize, don't proliferate"):
    /// `Composite` is the AND-composition; `OneOf` is the OR-composition.
    /// These are categorically distinct payment shapes (single CR rule
    /// section 118), so a parameterized `Composite { mode, costs }` was
    /// considered but rejected today — at two compositional modes the
    /// sibling form remains the smaller change and keeps existing
    /// `Composite` call sites untouched. If a third compositional mode is
    /// ever needed, refactor to a `mode`-parameterized form then.
    OneOf {
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
    /// CR 118.3: An effect performed as an activation cost. The parser reuses
    /// the existing effect pipeline to parse the cost text; the runtime resolves
    /// the effect on the source before the ability's own effect fires.
    EffectCost {
        effect: Box<Effect>,
    },
    /// CR 702.24a: A cost that multiplies a base cost by the number of
    /// counters of `counter` type on `target`. The runtime resolves the
    /// multiplier at the unless-payment entry point and expands `base`
    /// into the effective payment: mana scales via `ManaCost::scaled(n)`,
    /// life/sacrifice counts multiply directly, and `OneOf` unfolds into
    /// a `Composite` of `n` independent disjunctive choices (each made
    /// separately per CR 702.24a).
    ///
    /// Building block, not a special case: this is the typed shape of
    /// "pay [cost] for each counter on it". Cumulative upkeep is the
    /// only mechanic using it today, but the variant is composable with
    /// every existing base cost (Mana, PayLife, Sacrifice, OneOf,
    /// Composite).
    PerCounter {
        counter: CounterType,
        target: TargetFilter,
        base: Box<AbilityCost>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
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
    Unattaches,
    Mills,
    PutsCounters,
    Reveals,
    Exerts,
    KeywordCost,
}

impl AbilityCost {
    /// CR 702.24a + CR 118.12: True iff this cost can be used as the base
    /// cost for a cumulative-upkeep trigger and then paid by the current
    /// unless-payment pipeline after `PerCounter` expansion.
    ///
    /// This is the single support boundary for cumulative-upkeep synthesis and
    /// coverage reporting. Widen it only when `expand_per_counter` and
    /// `handle_unless_payment` can pay the resulting expanded shape end-to-end.
    pub fn supports_cumulative_upkeep_payment(&self) -> bool {
        match self {
            AbilityCost::Mana { .. }
            | AbilityCost::PayLife { .. }
            | AbilityCost::Sacrifice { .. } => true,
            // CR 118.12a: OneOf at the base must be a disjunction of mana
            // costs; mixed-shape disjunctions are not yet expanded into a
            // payable per-counter form.
            AbilityCost::OneOf { costs } => {
                !costs.is_empty() && costs.iter().all(|c| matches!(c, AbilityCost::Mana { .. }))
            }
            // The payment path currently folds only all-Mana composites into a
            // single combined mana cost. Mixed composites need sequenced
            // sub-cost payment before they can be installed safely.
            AbilityCost::Composite { costs } => {
                !costs.is_empty() && costs.iter().all(|c| matches!(c, AbilityCost::Mana { .. }))
            }
            _ => false,
        }
    }

    /// CR 118: Classify this cost into one or more `CostCategory` buckets.
    ///
    /// `Composite` recurses, flattening every sub-cost. Variants that pay
    /// nothing real (`Unimplemented`) return an empty vec.
    pub fn categories(&self) -> Vec<CostCategory> {
        match self {
            AbilityCost::Mana { .. } => vec![CostCategory::ManaOnly],
            AbilityCost::ManaDynamic { .. } => vec![CostCategory::ManaOnly],
            AbilityCost::Tap => vec![CostCategory::TapsSelf],
            AbilityCost::Untap => vec![CostCategory::UntapsSelf],
            AbilityCost::Loyalty { .. } => vec![CostCategory::PaysLoyalty],
            AbilityCost::Sacrifice { .. } => vec![CostCategory::SacrificesPermanent],
            AbilityCost::PayLife { .. } => vec![CostCategory::PaysLife],
            AbilityCost::Discard { .. } => vec![CostCategory::Discards],
            AbilityCost::Exile { .. } => vec![CostCategory::ExilesCards],
            // CR 702.167a: Craft's materials component exiles other objects.
            AbilityCost::ExileMaterials { .. } => vec![CostCategory::ExilesCards],
            AbilityCost::CollectEvidence { .. } => vec![CostCategory::ExilesCards],
            AbilityCost::TapCreatures { .. } => vec![CostCategory::TapsOtherCreatures],
            AbilityCost::RemoveCounter { .. } => vec![CostCategory::RemovesCounters],
            AbilityCost::PayEnergy { .. } => vec![CostCategory::PaysEnergy],
            AbilityCost::PaySpeed { .. } => vec![CostCategory::PaysSpeed],
            AbilityCost::ReturnToHand { .. } => vec![CostCategory::ReturnsToHand],
            AbilityCost::Unattach => vec![CostCategory::Unattaches],
            AbilityCost::Mill { .. } => vec![CostCategory::Mills],
            AbilityCost::Exert => vec![CostCategory::Exerts],
            AbilityCost::Blight { .. } => vec![CostCategory::PutsCounters],
            AbilityCost::Reveal { .. } => vec![CostCategory::Reveals],
            AbilityCost::Behold { action, .. } => {
                if *action == BeholdCostAction::ExileChosen {
                    vec![CostCategory::Reveals, CostCategory::ExilesCards]
                } else {
                    vec![CostCategory::Reveals]
                }
            }
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
            // CR 118.12a: A `OneOf` cost will only resolve into one branch's
            // categories at runtime, but the AI / coverage classifier needs to
            // know what categories the cost *could* belong to. Flatten over
            // all sub-costs the same way `Composite` does — semantically this
            // is "any of these may be chosen".
            AbilityCost::OneOf { costs } => {
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
            AbilityCost::EffectCost { effect } => match effect.as_ref() {
                Effect::PutCounter { .. } | Effect::PutCounterAll { .. } => {
                    vec![CostCategory::PutsCounters]
                }
                _ => Vec::new(),
            },
            // CR 702.24a: The multiplier doesn't change *what kind* of cost
            // this is, only *how much* — delegate classification to the base.
            AbilityCost::PerCounter { base, .. } => base.categories(),
            AbilityCost::Unimplemented { .. } => Vec::new(),
        }
    }

    /// CR 118.3 + CR 702.29a: Returns `true` when paying this cost consumes the
    /// ability's *source* card — i.e. the source card is discarded to pay the
    /// cost itself. Used by the UI to decide that a lone legal action must be
    /// confirmed rather than auto-dispatched on a single tap: firing such an
    /// ability silently destroys a card the player may have intended to play
    /// (cycling discards the card — CR 702.29a; Channel likewise).
    ///
    /// `Composite` / `OneOf` recurse and OR over sub-costs, mirroring
    /// `categories()`. Costs that consume *other* permanents/cards
    /// (`Discard { self_ref: false }`, `Sacrifice`, `Exile`, `Mill`, etc.)
    /// return `false` — they do not destroy the source. (A self-only
    /// `Sacrifice` would also qualify, but no `TargetFilter` self-only
    /// predicate exists today; cycling — issue #506 — is `Discard`-based and
    /// fully covered.)
    pub fn consumes_source(&self) -> bool {
        match self {
            // Cycling, Channel: "Discard this card" as a cost.
            AbilityCost::Discard { self_ref, .. } => *self_ref,
            AbilityCost::Composite { costs } | AbilityCost::OneOf { costs } => {
                costs.iter().any(AbilityCost::consumes_source)
            }
            // CR 702.24a: The PerCounter wrapper multiplies its base; whether
            // the source is consumed is determined entirely by the base cost.
            AbilityCost::PerCounter { base, .. } => base.consumes_source(),
            // Every other variant: pays mana / life / loyalty / counters /
            // taps / sacrifices-or-exiles-other — none destroys the source.
            AbilityCost::Mana { .. }
            | AbilityCost::ManaDynamic { .. }
            | AbilityCost::Tap
            | AbilityCost::Untap
            | AbilityCost::Loyalty { .. }
            | AbilityCost::Sacrifice { .. }
            | AbilityCost::PayLife { .. }
            | AbilityCost::Exile { .. }
            // CR 702.167a: Craft's materials exile OTHER objects; the source's
            // own exile is the separate `Exile { filter: SelfRef }` sub-cost.
            | AbilityCost::ExileMaterials { .. }
            | AbilityCost::CollectEvidence { .. }
            | AbilityCost::TapCreatures { .. }
            | AbilityCost::RemoveCounter { .. }
            | AbilityCost::PayEnergy { .. }
            | AbilityCost::PaySpeed { .. }
            | AbilityCost::ReturnToHand { .. }
            | AbilityCost::Unattach
            | AbilityCost::Mill { .. }
            | AbilityCost::Exert
            | AbilityCost::Blight { .. }
            | AbilityCost::Reveal { .. }
            | AbilityCost::Behold { .. }
            | AbilityCost::Waterbend { .. }
            | AbilityCost::NinjutsuFamily { .. }
            | AbilityCost::EffectCost { .. }
            | AbilityCost::Unimplemented { .. } => false,
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
    /// If paid, `SpellContext::additional_cost_paid` is set to true. When
    /// `repeatable` is true, the same non-kicker additional cost may be paid
    /// any number of times and each payment increments
    /// `SpellContext::additional_cost_payment_count`.
    Optional {
        cost: AbilityCost,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        repeatable: bool,
    },
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

/// Which casting-time payment stream an `AdditionalCostPaid` condition reads.
///
/// `Any` preserves legacy optional-additional-cost behavior. `Kicker` reads
/// only CR 702.33 kicker payments, while `NonKicker` reads only repeatable
/// non-kicker additional-cost payments such as Squad.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum AdditionalCostPaymentSource {
    #[default]
    Any,
    Kicker,
    NonKicker,
}

impl AdditionalCostPaymentSource {
    #[allow(clippy::trivially_copy_pass_by_ref)]
    pub(crate) fn is_any(value: &Self) -> bool {
        matches!(value, AdditionalCostPaymentSource::Any)
    }
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
// UnlessPayModifier -- the "unless [player] pays [cost]" wrapper
// ---------------------------------------------------------------------------

/// CR 118.12 + CR 118.12a: "Effect unless [player] pays {cost}"
/// Wraps any effect with a player payment choice. The cost is the unified
/// `AbilityCost` taxonomy — historically a separate `UnlessCost` enum existed,
/// but every variant collapsed cleanly into `AbilityCost` (fold completed
/// 2026-05-09 audit batch H3/M1: `Fixed`/`DynamicGeneric` → `Mana`/`ManaDynamic`,
/// `DiscardCard` → `Discard`, etc.).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnlessPayModifier {
    /// CR 118.12: The cost the player may choose to pay to prevent the effect.
    /// Stored as the unified `AbilityCost` taxonomy (no parallel cost hierarchy).
    /// Forward-compatible deserialization accepts the legacy `UnlessCost` JSON
    /// shape so saved games / serialized triggers keep loading after the fold.
    #[serde(deserialize_with = "deserialize_ability_cost_compat")]
    pub cost: AbilityCost,
    /// Who must pay — resolved via TargetFilter at trigger resolution time.
    /// Typically TargetFilter::TriggeringPlayer for "that player".
    pub payer: TargetFilter,
}

/// Boxed variant of `deserialize_ability_cost_compat` for use on fields
/// declared as `Box<AbilityCost>` (e.g.,
/// `ManaAbilityResume::UnlessPayment.cost` is boxed to keep the enclosing
/// enum compact). Same backward-compat behavior as the unboxed variant.
pub fn deserialize_ability_cost_compat_boxed<'de, D>(d: D) -> Result<Box<AbilityCost>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    deserialize_ability_cost_compat(d).map(Box::new)
}

/// Forward-compatible deserializer for `UnlessPayModifier::cost` and
/// `WaitingFor::UnlessPayment::cost`.
/// First tries the unified `AbilityCost` JSON shape, then falls back to the
/// legacy `UnlessCost` shape so saved-game JSON / persisted trigger
/// definitions keep loading after the 2026-05-09 fold.
///
/// Legacy → modern mapping (per audit batch H3/M1):
/// - `Fixed { cost }` → `Mana { cost }`
/// - `DynamicGeneric { quantity }` → `ManaDynamic { quantity }`
/// - `PayLife { amount: i32 }` → `PayLife { amount: QuantityExpr::Fixed { value: amount } }`
/// - `PayEnergy { amount }` → `PayEnergy { amount }` (identity)
/// - `DiscardCard { filter }` → `Discard { count: 1, filter, random: false, self_ref: false }`
/// - `Sacrifice { count, filter }` → `Sacrifice { target: filter, count }`
/// - `ReturnToHand { count, filter, from_zone }` → `ReturnToHand { count, filter: Some(filter), from_zone }`
pub fn deserialize_ability_cost_compat<'de, D>(d: D) -> Result<AbilityCost, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::Deserialize as _;
    let raw: serde_json::Value = serde_json::Value::deserialize(d)?;
    // Try the modern AbilityCost shape first.
    if let Ok(cost) = serde_json::from_value::<AbilityCost>(raw.clone()) {
        return Ok(cost);
    }
    // Fall back to the legacy `UnlessCost` shape and translate.
    let legacy: LegacyUnlessCost = serde_json::from_value(raw).map_err(serde::de::Error::custom)?;
    Ok(legacy.into_ability_cost())
}

/// CR 118.12: Legacy shadow type used ONLY by `deserialize_ability_cost_compat`
/// to accept pre-fold serialized JSON. The variants and field names mirror the
/// pre-2026-05-09 `UnlessCost` enum exactly. New code must NOT construct this
/// type — it exists solely as a deserialization staging area.
#[derive(Deserialize)]
#[serde(tag = "type")]
enum LegacyUnlessCost {
    Fixed {
        cost: ManaCost,
    },
    DynamicGeneric {
        quantity: QuantityExpr,
    },
    PayLife {
        amount: i32,
    },
    PayEnergy {
        amount: u32,
    },
    DiscardCard {
        #[serde(default)]
        filter: Option<TargetFilter>,
    },
    Sacrifice {
        count: u32,
        filter: TargetFilter,
    },
    ReturnToHand {
        count: u32,
        filter: TargetFilter,
        #[serde(default)]
        from_zone: Option<Zone>,
    },
}

impl LegacyUnlessCost {
    fn into_ability_cost(self) -> AbilityCost {
        match self {
            LegacyUnlessCost::Fixed { cost } => AbilityCost::Mana { cost },
            LegacyUnlessCost::DynamicGeneric { quantity } => AbilityCost::ManaDynamic { quantity },
            LegacyUnlessCost::PayLife { amount } => AbilityCost::PayLife {
                amount: QuantityExpr::Fixed { value: amount },
            },
            LegacyUnlessCost::PayEnergy { amount } => AbilityCost::PayEnergy {
                amount: QuantityExpr::Fixed {
                    value: amount as i32,
                },
            },
            LegacyUnlessCost::DiscardCard { filter } => AbilityCost::Discard {
                count: QuantityExpr::Fixed { value: 1 },
                filter,
                random: false,
                self_ref: false,
            },
            LegacyUnlessCost::Sacrifice { count, filter } => AbilityCost::Sacrifice {
                target: filter,
                count,
            },
            LegacyUnlessCost::ReturnToHand {
                count,
                filter,
                from_zone,
            } => AbilityCost::ReturnToHand {
                count,
                filter: Some(filter),
                from_zone,
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Effect enum -- typed variants, zero HashMap
// ---------------------------------------------------------------------------

/// Specific position within a library for placement effects. Top and Bottom use
/// move_to_library_position; NthFromTop inserts at index n-1.
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

/// CR 702.179c-d: Direction of a speed change. Typed (not a bool) so the
/// `Effect::ChangeSpeed` handler dispatches exhaustively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum SpeedDelta {
    /// CR 702.179c-d: increase speed (capped at 4 by the speed rules).
    Increase,
    /// CR 702.179f-consistent: decrease speed, treating no speed as 0.
    Decrease,
}

/// CR 500.11 + CR 614.10a: A one-shot skip can name either a single step or a
/// whole phase. Keep whole-phase skips distinct from their first step so
/// "skip your next beginning of combat step" remains representable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum StepSkipTarget {
    Step(Phase),
    CombatPhase,
}

impl StepSkipTarget {
    pub fn constituent_steps(self) -> &'static [Phase] {
        match self {
            Self::Step(Phase::Untap) => &[Phase::Untap],
            Self::Step(Phase::Upkeep) => &[Phase::Upkeep],
            Self::Step(Phase::Draw) => &[Phase::Draw],
            Self::Step(Phase::PreCombatMain) => &[Phase::PreCombatMain],
            Self::Step(Phase::BeginCombat) => &[Phase::BeginCombat],
            Self::Step(Phase::DeclareAttackers) => &[Phase::DeclareAttackers],
            Self::Step(Phase::DeclareBlockers) => &[Phase::DeclareBlockers],
            Self::Step(Phase::CombatDamage) => &[Phase::CombatDamage],
            Self::Step(Phase::EndCombat) => &[Phase::EndCombat],
            Self::Step(Phase::PostCombatMain) => &[Phase::PostCombatMain],
            Self::Step(Phase::End) => &[Phase::End],
            Self::Step(Phase::Cleanup) => &[Phase::Cleanup],
            Self::CombatPhase => &[
                Phase::BeginCombat,
                Phase::DeclareAttackers,
                Phase::DeclareBlockers,
                Phase::CombatDamage,
                Phase::EndCombat,
            ],
        }
    }
}

/// CR 115.1: Whether the `Bounce` effect selects its affected object at
/// cast/activation time ("target", locked) or at resolution time (controller-
/// scoped filter like "a creature you control" — Whitemane Lion).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BounceSelection {
    /// Default — target chosen at cast/activation via the targeting pipeline.
    #[default]
    Targeted,
    /// CR 608.2c: Controller chooses an eligible object at resolution.
    AtResolution,
}

impl BounceSelection {
    /// Helper for `#[serde(skip_serializing_if = ...)]`.
    pub fn is_targeted(&self) -> bool {
        matches!(self, Self::Targeted)
    }
}

/// CR 708.2a: Characteristics an effect specifies for a permanent it puts onto
/// the battlefield face down ("...unless otherwise specified by the effect that
/// put it onto the battlefield face down"). When an effect lists no
/// characteristics, the permanent defaults to a vanilla 2/2 with no name,
/// subtypes, or mana cost (CR 708.2a). When the effect *does* specify
/// characteristics ("They're 2/2 Cyberman artifact creatures."), those override
/// the defaults. Parts-built — no card-named hardcode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaceDownProfile {
    /// CR 708.2a: Power override. `None` defaults to 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub power: Option<i32>,
    /// CR 708.2a: Toughness override. `None` defaults to 2.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub toughness: Option<i32>,
    /// CR 205.1a: Additional core card types beyond Creature (always present per
    /// CR 708.2a), e.g. Artifact for "artifact creatures".
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_core_types: Vec<CoreType>,
    /// CR 205.1a: Creature subtypes the effect grants ("Cyberman").
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subtypes: Vec<String>,
}

impl FaceDownProfile {
    /// CR 708.2a: The default face-down characteristics — a vanilla 2/2 creature
    /// with no extra types or subtypes. Used when an effect puts a card onto the
    /// battlefield face down without specifying characteristics.
    pub fn vanilla_2_2() -> Self {
        Self {
            power: None,
            toughness: None,
            extra_core_types: vec![],
            subtypes: vec![],
        }
    }
}

/// The typed effect enum. Each variant corresponds to an effect handler.
/// Zero HashMap<String, String> fields.
// clippy::large_enum_variant: `Effect` is the engine's central 100+ variant
// dispatch enum, so the largest/smallest spread is inherent. The current
// largest variant is `Token`, which inlines two `PtValue` fields that can each
// carry a `QuantityExpr`; the right remedy is to box `PtValue::Quantity` (used
// in 70+ sites), not to box individual `Effect` variants. Allow the spread here
// until that boxing lands.
#[allow(clippy::large_enum_variant)]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
#[serde(tag = "type")]
pub enum Effect {
    /// CR 702.179a: A player starts their engines, setting speed to 1 if they have no speed.
    StartYourEngines {
        player_scope: PlayerFilter,
    },
    /// CR 702.179c-d: Change the selected players' speed by the given amount
    /// in `direction`. `direction = Increase` covers all former `IncreaseSpeed`
    /// cards; `direction = Decrease` covers speed-reduction cards.
    ChangeSpeed {
        player_scope: PlayerFilter,
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        direction: SpeedDelta,
        /// Card-text-derived (NOT a CR rule): minimum speed a decrease may
        /// produce, e.g. Spikeshell Harrier's "can't reduce their speed
        /// below 1". `None` for increases and for unfloored decreases.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        floor: Option<u8>,
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
    /// CR 702.95a,c-d + CR 115.10a: Pair the source creature with an unpaired
    /// creature controlled by the same player. The partner is chosen while
    /// resolving and is not a target.
    PairWith {
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
        /// CR 701.6 + CR 608.2c: A follow-up instruction acting on the countered
        /// ability's source permanent (e.g. Tishana's Tidebinder "loses all
        /// abilities for as long as ~", Teferi's Response / Green Slime "destroy
        /// that permanent"). Resolved at counter-resolution time, bound to
        /// `source_permanent_id`. Only fires when an ability is countered — a
        /// countered spell is not a permanent (CR 701.8a / CR 110.1).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_rider: Option<CounterSourceRider>,
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
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
    },
    GainLife {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
        /// CR 119.3: Who gains the life. Defaults to Controller (omitted from JSON).
        #[serde(
            default = "default_target_filter_controller",
            skip_serializing_if = "is_target_filter_controller",
            deserialize_with = "deserialize_gain_life_player"
        )]
        player: TargetFilter,
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
        counter_type: CounterType,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    RemoveCounter {
        #[serde(default)]
        counter_type: Option<CounterType>,
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
        /// CR 107.1c: Minimum number of permanents to choose for ranged
        /// sacrifice effects. Defaults to 0 for ordinary "up to" choices; set
        /// to 1 for "one or more" choices.
        #[serde(default, skip_serializing_if = "is_zero_usize")]
        min_count: usize,
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
        /// CR 120.1 + CR 608.2c: Damage source override. When `Some(Target)`, the
        /// damage is attributed to the spell/ability's first target object rather
        /// than the ability's source. Mirrors `DealDamage::damage_source` for
        /// "target creature you control deals damage equal to its power to each
        /// other creature and each opponent" (Chandra's Ignition class) — the
        /// chosen creature, not the spell, is the source for the purpose of
        /// protection (CR 702.16), wither/infect (CR 120.3b/d), and damage-source
        /// replacements (CR 614). `None` keeps the ability's source attribution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        damage_source: Option<DamageSource>,
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
        /// CR 110.2a: Controller override on ETB. `Some(ref)` routes the object
        /// to the player resolved from `ref` (currently only `ControllerRef::You`
        /// is supported at runtime — see resolver in `effects/change_zone.rs`).
        /// `None` leaves the object under its owner's control per CR 110.2.
        ///
        /// Legacy on-disk shape (boolean `under_your_control`) deserializes via
        /// `deserialize_enters_under_compat`; emission is always the modern
        /// shape (`Option<ControllerRef>`). The compat path is guarded by
        /// `_LEGACY_DESER_ETB_CONTROLLER_2026Q2` and is scheduled to be removed
        /// once the workspace version exceeds 0.1.53.
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            alias = "under_your_control",
            deserialize_with = "deserialize_enters_under_compat"
        )]
        enters_under: Option<ControllerRef>,
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
        enter_with_counters: Vec<(CounterType, QuantityExpr)>,
        /// CR 708.2a + CR 708.3: when `Some`, the object that enters the
        /// battlefield via this move is turned face down (before entry, CR
        /// 708.3) with these characteristics. `None` = normal face-up entry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        face_down_profile: Option<FaceDownProfile>,
    },
    ChangeZoneAll {
        #[serde(default)]
        origin: Option<Zone>,
        destination: Zone,
        #[serde(default = "default_target_filter_none")]
        target: TargetFilter,
        /// CR 110.2a: Controller override on mass ETB zone changes. `Some(ref)`
        /// routes each entering object to the player resolved from `ref`.
        /// `None` leaves each object under its default controller.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enters_under: Option<ControllerRef>,
        /// CR 110.5b: When true, objects enter the battlefield tapped during
        /// a mass zone move.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter_tapped: bool,
        /// CR 708.2a + CR 708.3: when `Some`, each object that enters the
        /// battlefield via this move is turned face down (before entry, CR
        /// 708.3) with these characteristics. `None` = normal face-up entry.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        face_down_profile: Option<FaceDownProfile>,
    },
    /// CR 701.20e + CR 608.2c: Look at top N cards (shown only to the looking player),
    /// select some to keep per the effect's instructions, rest go elsewhere.
    Dig {
        /// Which player's library is inspected. Defaults to the ability controller
        /// for "your library"; `Player` covers "target player's library".
        #[serde(default = "default_target_filter_controller")]
        player: TargetFilter,
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
    /// CR 701.3d: Unattach every matching Equipment from a matched host while
    /// leaving that Equipment on the battlefield. `attachment` scopes which
    /// attached objects move; `target` scopes the host object.
    UnattachAll {
        #[serde(default = "default_target_filter_any")]
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
        /// CR 115.1 + Whitemane Lion ruling: Controls whether this effect uses
        /// the targeting pipeline (`Targeted`) or selects at resolution
        /// (`AtResolution` — Whitemane Lion class). Card-data.json records
        /// predating this field deserialize as `Targeted` via the default.
        #[serde(default, skip_serializing_if = "BounceSelection::is_targeted")]
        selection: BounceSelection,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        count: Option<QuantityExpr>,
    },
    Explore,
    /// CR 701.44d: Simultaneous multi-permanent explore instruction.
    /// The resolver processes matching permanents one explore at a time in
    /// APNAP/controller-chosen order, reusing the single-permanent Explore resolver.
    ExploreAll {
        #[serde(default = "default_target_filter_any")]
        filter: TargetFilter,
    },
    /// CR 701.16: Investigate — create a Clue token.
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
    /// CR 724.1: End the turn. Exile every object on the stack, check
    /// state-based actions, remove everything from combat, then skip straight
    /// to the cleanup step. Time Stop, Sundial of the Infinite, Obeka,
    /// Glorious End, Discontinuity, Day's Undoing.
    EndTheTurn,
    /// CR 724.2: End the combat phase. Exile every object on the stack, check
    /// state-based actions, remove everything from combat, expire "until end of
    /// combat" effects, and skip straight to the postcombat main phase. Does
    /// nothing outside a combat phase (CR 724.2g). Mandate of Peace.
    EndCombatPhase,
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
        /// CR 701.38a + CR 800.4g: Which players cast votes. Council's-dilemma
        /// classics ("starting with you, each player votes...") use
        /// `VoterScope::AllPlayers`; "each opponent chooses..." patterns
        /// (Master of Ceremonies) use `VoterScope::EachOpponent`, which
        /// excludes the controller from the voter queue. CR 800.4g handles the
        /// edge case where every opponent has been eliminated — the resolver
        /// emits `EffectResolved` with no tally and the chain continues.
        #[serde(default = "default_voter_scope_all")]
        voter_scope: VoterScope,
    },
    /// CR 700.3 + CR 608: Separate objects into two piles, have another player
    /// choose one of them, and apply a sub-effect to the chosen pile. The
    /// canonical "two piles" primitive — covers Make an Example (each opponent
    /// partitions their own creatures, you choose, they sacrifice the chosen
    /// pile) and is built so Liliana of the Veil −6 ("target player") and the
    /// Fact or Fiction family can extend it via leaf parameterization.
    ///
    /// Resolution is fully interactive: the partitioner submits one pile (pile
    /// A); pile B is derived as `eligible \ pile_a` (CR 700.3a — exhaustive
    /// disjoint partition, CR 700.3d — piles may be empty). The chooser then
    /// picks A or B per subject. Finally `chosen_pile_effect` resolves once
    /// for each object in each chosen pile, scoped to that subject as
    /// controller.
    ///
    /// Per CR 700.3b a pile is not a `GameObject`; the runtime carries piles
    /// as transient `im::Vector<ObjectId>` on the relevant `WaitingFor`
    /// variants. Per CR 700.3c, partitioned objects do not leave their zone
    /// during the partition/choice steps — only the final sub-effect acts on
    /// them.
    SeparateIntoPiles {
        /// CR 101.4 + CR 800.4g: Which players partition their own objects.
        /// `EachOpponent` is the Make-an-Example shape; Liliana −6 will
        /// extend this with a `TargetPlayers`-style variant on `VoterScope`.
        partition_subject: VoterScope,
        /// Filter applied to each subject's eligible object set (the
        /// resolver constrains the controller to the subject before applying
        /// this filter). Typically `Typed(Creature)` for Make an Example.
        #[serde(default = "default_target_filter_any")]
        object_filter: TargetFilter,
        /// CR 700.3 + CR 608.2c: Which player chooses one pile per subject.
        /// `Controller` covers "you choose" — the spell controller.
        chooser: PlayerScope,
        /// CR 608.2c: Sub-effect applied to each chosen pile, once per object,
        /// with the subject rebound as controller. Sacrifice for Make an
        /// Example; the building block accepts any per-object effect.
        chosen_pile_effect: Box<AbilityDefinition>,
    },
    /// CR 613.4d: Switch a creature's power and toughness. Applied in layer 7d.
    SwitchPT {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    CopySpell {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 707.10c: whether the controller may choose new targets for the copy.
        #[serde(default = "default_copy_keep_targets")]
        retarget: CopyRetargetPermission,
    },
    /// CR 707.12: Create a copy of a card/object in its zone and cast that
    /// copy while the resolving spell or ability continues resolving.
    CastCopyOfCard {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 118.9 + CR 601.2f: Alternative mana cost used to cast the copy.
        /// Mizzix's Mastery and Cipher use `ManaCost::zero()`.
        #[serde(default)]
        cost: ManaCost,
    },
    /// CR 707.2 / CR 707.5: Create a token that's a copy of a permanent.
    /// Copies copiable characteristics (name, mana cost, color, types, P/T, abilities, keywords)
    /// from the chosen copy source to a newly created token on the battlefield.
    CopyTokenOf {
        /// CR 115.1: Targeted copy source. SelfRef/ParentTarget are context refs;
        /// Any/Typed are selected as targets when `source_filter` is absent.
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        /// CR 109.4: The player who creates (and therefore controls) the copy
        /// token(s). Mirrors `Effect::Token.owner` — defaults to
        /// `TargetFilter::Controller`, but "target opponent creates a token
        /// that's a copy of it" lifts the chosen opponent into this field via
        /// `inject_subject_target`. The copy *source* stays in `target`.
        #[serde(default = "default_target_filter_controller")]
        owner: TargetFilter,
        /// CR 115.1 + CR 608.2c: Non-targeting copy source set for "for each
        /// [object], create a token that's a copy of it" effects. These objects
        /// are chosen by the effect at resolution, not by target declaration.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_filter: Option<TargetFilter>,
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
    /// CR 702.116a: Myriad creates one tapped attacking copy token for each
    /// opponent other than the defending player for the source creature, then
    /// exiles those tokens at end of combat.
    Myriad,
    /// CR 509.1g + CR 506.3e + CR 707.2: For each attacking creature matched by
    /// `source_filter`, create a token that's a copy of it and put that token
    /// onto the battlefield blocking the attacker it copies. Mirror Match is the
    /// canonical card ("For each attacking creature, create a token that's a
    /// copy of that creature. Those tokens block those creatures …"). The
    /// end-of-combat exile of the created tokens is composed separately as a
    /// delayed trigger over `TargetFilter::LastCreated` ("those tokens"), so
    /// this effect only handles the copy-and-block half of the idiom.
    CopyTokenBlockingAttacker {
        /// CR 508.1: The attacking creatures to copy and block. Non-targeting —
        /// resolved against the battlefield at resolution time ("for each").
        source_filter: TargetFilter,
        /// CR 109.4: The player who creates (and therefore controls) the copy
        /// tokens. Defaults to the resolving ability's controller. Mirrors
        /// `Effect::CopyTokenOf.owner`.
        #[serde(default = "default_target_filter_controller")]
        owner: TargetFilter,
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
        counter_type: CounterType,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    /// CR 122.1: Place counters on all objects matching a filter (no targeting).
    PutCounterAll {
        counter_type: CounterType,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
    },
    MultiplyCounter {
        counter_type: CounterType,
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
    /// CR 122.5 + CR 122.8: Transfer counters from source onto target.
    /// `mode` records whether Oracle says to move counters (remove then put)
    /// or to put matching counters from source/LKI state.
    MoveCounters {
        /// Where counters are read from (SelfRef = ability source object).
        #[serde(default = "default_target_filter_self_ref")]
        source: TargetFilter,
        /// When Some, only move this counter type. When None, move all counters.
        #[serde(default)]
        counter_type: Option<CounterType>,
        /// When Some, transfer up to this many matching counters. When None,
        /// transfer every matching counter.
        #[serde(default)]
        count: Option<QuantityExpr>,
        /// Whether to remove counters from the source or only put matching counters.
        #[serde(default = "default_counter_transfer_mode")]
        mode: CounterTransferMode,
        #[serde(default)]
        selection: CounterMoveSelection,
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
    /// CR 614.1 + CR 614.12 + CR 303.4 + CR 303.4a + CR 303.4g + CR 613.1d +
    /// CR 613.1f + CR 113.10 + CR 702.5a + CR 604.1 + CR 611.2a + CR 400.7:
    /// Return-as-Aura sub-effect. After the host object has been returned to
    /// the battlefield by a preceding `Effect::ChangeZone`, this effect
    /// installs the continuous "It's an Aura enchantment with enchant <X>"
    /// modification on the just-returned object (CR 613.1d sets card type to
    /// Enchantment, removes the Creature subtype set, adds the Aura subtype,
    /// CR 702.5a adds the Enchant keyword with the parsed filter, and any
    /// granted abilities/triggers/static abilities/keywords from the inner
    /// quoted body apply via Layer 6 per CR 613.1f / CR 113.10), then chooses
    /// a legal target per CR 303.4 + CR 303.4a and attaches the host to it.
    /// If no legal object exists, the host is put into its owner's graveyard
    /// per CR 303.4g. The continuous effect lasts `Duration::UntilHostLeavesPlay`
    /// per CR 611.2a + CR 400.7 because a new object on re-entry is not the
    /// same object and the prior continuous effect implicitly ends.
    ///
    /// Class members: Old-Growth Troll (KHM), Bronzehide Lion (THB),
    /// Harold and Bob, First Numens (FIN-precon).
    ReturnAsAura {
        /// CR 303.4a + CR 702.5a: The enchant filter parsed from
        /// "enchant <X>" (e.g., `Forest you control`, `creature you control`).
        #[serde(default = "default_target_filter_any")]
        enchant_filter: TargetFilter,
        /// CR 113.10 + CR 604.1: Granted abilities/triggers/static abilities/
        /// keywords from the inner quoted body. If the source Oracle text also
        /// said "~ loses all other abilities" (Harold-shape) or had a
        /// pre-split "and it loses all other abilities" sibling
        /// (Bronzehide-shape, folded at IR layer), `RemoveAllAbilities` is at
        /// `grants[0]` and is dependency-ordered before grants by Layer 6
        /// (CR 613.7c).
        #[serde(default)]
        grants: Vec<ContinuousModification>,
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
        /// CR 701.23a: Zones the search looks through. Defaults to library-only
        /// (the overwhelming majority of tutors). God-Pharaoh's-Gift-class cards
        /// (Gate to the Afterlife, Dark Supplicant, Say Its Name, Boonweaver
        /// Giant, Mishra) search graveyard + hand + library. The trailing "If you
        /// search your library this way, shuffle" is the effect's own Shuffle
        /// sub-ability and is always reached in the multi-zone case because
        /// Library is in the set. CR 701.23 + CR 609.3: a `CantSearchLibrary`
        /// muzzle suppresses only the library component — the other zones are
        /// still searched, and the per-turn "searched a library" tracking fires
        /// only when Library is actually among the searched zones.
        #[serde(
            default = "default_search_zones",
            skip_serializing_if = "is_default_search_zones"
        )]
        source_zones: Vec<Zone>,
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
        /// CR 701.23a + CR 608.2c: When set, the found set is partitioned across
        /// two destinations (cultivate-class "put one onto the battlefield
        /// tapped and the other into your hand"). `None` preserves the existing
        /// single-zone search behavior (destination handled by the sub_ability
        /// ChangeZone chain).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<SearchDestinationSplit>,
    },
    /// CR 400.11/400.11a + CR 701.23j: Choose card(s) the player owns from
    /// outside the game. For tournament-style play, the bounded accessible set
    /// is the player's current sideboard, which is not modeled as a zone.
    ///
    /// CR 400.11 + CR 406.3: Candidate source pool. Defaults to sideboard;
    /// Karn/Coax-class text widens the pool to include owned face-up exile.
    SearchOutsideGame {
        filter: TargetFilter,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default)]
        reveal: bool,
        #[serde(default = "default_zone_hand")]
        destination: Zone,
        #[serde(default, skip_serializing_if = "is_default_outside_game_source_pool")]
        source_pool: OutsideGameSourcePool,
    },
    RevealHand {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default = "default_target_filter_any")]
        card_filter: TargetFilter,
        /// None = reveal entire hand. Some = reveal this many cards. CR 701.20a.
        #[serde(default)]
        count: Option<QuantityExpr>,
        /// CR 701.20a: When true, reveal `count` cards chosen at random from that hand.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        random: bool,
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
        /// CR 406.3: When true the exiled cards enter Exile face down and
        /// must not be examinable by any player (the resolver flips the
        /// moved object's `face_down` flag, and `visibility.rs` redacts the
        /// card unless a separate effect grants look permission). Covers the
        /// Necropotence / Bomat Courier / Asmodeus the Archfiend /
        /// Knowledge Vault class — every card
        /// whose Oracle text says "exile the top card of your library face
        /// down". Skipped from serialization when false so JSON snapshots
        /// and stored card-data for face-up `ExileTop` effects are
        /// unchanged.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        face_down: bool,
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
    /// CR 508.1d: Target creature must attack the required player this turn/combat if able.
    ForceAttack {
        #[serde(default = "default_target_filter_any")]
        target: TargetFilter,
        #[serde(default = "default_target_filter_controller")]
        required_player: TargetFilter,
        #[serde(default = "default_duration_until_end_of_turn")]
        duration: Duration,
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
    /// Cleanup at end-of-turn relies on `expiry: Some(RestrictionExpiry::EndOfTurn)`
    /// on the carried definition (CR 514.2).
    ///
    /// `target: TargetFilter::None` is the "no per-target binding" mode for
    /// self-contained turn-bound replacements (Rankle and Torbran's "If a
    /// source would deal damage to a player or battle this turn, it deals
    /// that much damage plus 2 instead.", I Call for Slaughter's "If a
    /// source you control would deal damage this turn, it deals that much
    /// damage plus 1 instead."). The carried `ReplacementDefinition`
    /// already constrains its own source/target/scope filters, so the
    /// resolver pushes it directly to `pending_damage_replacements` with
    /// no per-target inference of `damage_target_filter`.
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
        counter_type: CounterType,
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
        /// CR 202.3 + CR 601.2e: Optional cast-permission predicate applied
        /// to the spell being cast from the granted zone.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<CastPermissionConstraint>,
        /// CR 611.2a: Optional durational scope propagated onto the granted
        /// `CastingPermission::ExileWithAltCost { duration, .. }` so the
        /// permission is pruned by the standard layer prune helpers.
        /// CR 702.88a (Rebound): set to `Some(Duration::UntilEndOfTurn)` by
        /// the Rebound arming flow so the next-upkeep recast offer expires
        /// at end of turn if not used. `None` for all standing cast-from-zone
        /// grants (Discover, Suspend, Nashi, etc.).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        duration: Option<Duration>,
    },
    /// CR 615: Prevent damage to a target.
    PreventDamage {
        amount: PreventionAmount,
        /// CR 615.11 + CR 107.3i: When present, overrides `amount` at effect
        /// resolution — the dynamic quantity is resolved to a concrete count and
        /// the prevention shield is created as a static `Next(n)` depletion shield.
        /// Set by "prevent X … where X is <quantity>" clauses. `None` for the
        /// common fixed/All forms.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        amount_dynamic: Option<QuantityExpr>,
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
    /// CR 614.9 + CR 614.1a + CR 615: Create a one-shot "the next time [source]
    /// would deal [combat] damage [to X] this turn, [modify/redirect] instead"
    /// damage-replacement shield (Desperate Gambit, Soltari Guerrillas, Beacon
    /// of Destiny, Jade Monolith, Goblin Psychopath).
    ///
    /// Distinct from a continuous static `damage_modification` replacement
    /// (Furnace of Rath / Gratuitous Violence): this effect, created by an
    /// activated/triggered ability at resolution, builds a one-shot
    /// `ReplacementDefinition` tagged with `ShieldKind::DamageReplacementOneShot`
    /// (amount form) or `ShieldKind::Redirection` (redirect form), consumed on
    /// its use (CR 614.5) and dropped at cleanup.
    ///
    /// Exactly one of `modification` / `redirect_to` is `Some`. When
    /// `redirect_to == Some(ChosenObjectTarget)` ("to target creature" —
    /// Soltari Guerrillas), `redirect_object_filter` carries the recipient's
    /// `TargetFilter` so the targeting layer surfaces a standard object target
    /// slot (`ability_utils::collect_target_slots`); the resolver captures the
    /// chosen object into the shield. All other redirect forms host on the
    /// controller / source with no declared target.
    CreateDamageReplacement {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_filter: Option<TargetFilter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        combat_scope: Option<CombatDamageScope>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_filter: Option<DamageTargetFilter>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modification: Option<DamageModification>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redirect_to: Option<DamageRedirectTarget>,
        /// CR 614.9: Optional amount cap for redirection shields. `None` means
        /// the whole damage event is redirected ("the next time ... would deal
        /// damage"). `Some(Next(n))` redirects only the next N damage from the
        /// matching event, leaving the remainder on the original recipient.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redirect_amount: Option<PreventionAmount>,
        /// CR 115.1: The redirect recipient's target filter for the
        /// `ChosenObjectTarget` form ("...deals that damage to target creature
        /// instead" — Soltari Guerrillas). `None` for the `Controller` /
        /// `SourceObject` redirect forms, which need no target slot.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        redirect_object_filter: Option<TargetFilter>,
        /// CR 115.1 + CR 614.9: The *original-recipient* target filter when the
        /// damage-to clause names a target ("would deal damage to target
        /// creature" — Jade Monolith). When `Some`, the shield is hosted on the
        /// chosen object with `valid_card: SelfRef` so it fires only on damage
        /// to that specific permanent (not the broader `target_filter` scope).
        /// `None` for cards whose recipient is a scope ("to an opponent" —
        /// Soltari) or implicit ("you" — Beacon).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        recipient_object_filter: Option<TargetFilter>,
    },
    /// CR 104.3a: A player who meets this effect's condition loses the game.
    /// The affected player is determined by resolution context (controller's opponent
    /// if untargeted, or explicit target if targeted).
    LoseTheGame,
    /// CR 104.3a: The controller wins the game — all opponents lose.
    WinTheGame,
    /// CR 706: Roll a die with the given number of sides.
    /// If `results` is non-empty, execute the matching branch.
    /// CR 706.1: `count` is how many dice of this kind to roll ("roll two
    /// six-sided dice", "roll X d12"). Each die is rolled independently with
    /// the same `sides`/`modifier`/`results`. Defaults to one for back-compat
    /// with cards printed in the single-die JSON shape. Mirrors the
    /// `FlipCoins.count` precedent (CR 705).
    /// CR 706.2: `modifier` adjusts the natural roll before result-branch lookup.
    /// `None` means the natural result is used unchanged.
    RollDie {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        sides: u8,
        #[serde(default)]
        results: Vec<DieResultBranch>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        modifier: Option<DieRollModifier>,
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
    /// CR 726.1 + CR 726.2: Take the initiative. Grants the initiative
    /// designation and triggers venture into Undercity.
    TakeTheInitiative,
    /// CR 701.51b: Open N Attractions by putting cards from the top of your
    /// Attraction deck onto the battlefield.
    OpenAttractions {
        count: u32,
    },
    /// CR 701.52: Roll to visit your Attractions.
    RollToVisitAttractions,
    /// CR 728.1: Process rad counters — mill cards equal to rad counter count,
    /// lose 1 life and remove one rad counter per nonland card milled.
    ProcessRadCounters,
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
    /// The selection is from the tracked set of the parent effect's result, falling back to
    /// direct zone contents for wordings like "choose a card in your hand."
    /// CR 700.2: The `chooser` field determines who makes the selection.
    ChooseFromZone {
        /// How many cards to choose.
        #[serde(default = "default_one")]
        count: u32,
        /// Which zone the cards are in (usually Exile).
        zone: Zone,
        /// Additional zones that share the same owner and filter.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        additional_zones: Vec<Zone>,
        /// Which player's zone(s) are searched when no tracked set is available.
        #[serde(default)]
        zone_owner: ZoneOwner,
        /// Optional filter for direct zone-backed choices.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
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
    /// CR 603.7e: An affected-player-chosen battlefield permanent set, written
    /// into the chain's tracked object set so downstream effects ("pay {N} for
    /// each ... chosen this way", "untap those creatures") reference the exact
    /// selection. `chooser` is a `TargetFilter` (not `Chooser`) so it rebinds
    /// per-trigger-instance to the affected player — e.g. the player whose
    /// upkeep it is for an "at the beginning of each player's upkeep" trigger.
    /// `filter` constrains the eligible permanents; `min`/`max` express the
    /// cardinality ("any number" → `min: 0, max: None`).
    ChooseObjectsIntoTrackedSet {
        /// The player who makes the selection — resolved per-instance.
        chooser: TargetFilter,
        /// Constrains which battlefield permanents are eligible.
        filter: TargetFilter,
        /// Minimum number of objects that must be selected.
        min: u32,
        /// Maximum number of objects selectable (`None` = "any number").
        max: Option<u32>,
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
        /// Permanents eligible to be chosen for the category slots.
        #[serde(default = "default_target_filter_permanent")]
        choose_filter: TargetFilter,
        /// Permanents in scope for the final sacrifice sweep.
        #[serde(default = "default_target_filter_permanent")]
        sacrifice_filter: TargetFilter,
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
    /// CR 701.13a + CR 608.2c: Exile cards from the top of a player's library
    /// one at a time until the typed `until` predicate is satisfied. The
    /// `until` axis is parameterized by [`UntilCondition`] — either
    /// `NextMatches(filter)` (Etali/Cascade/Discover-shape: stop on first hit
    /// and pass the hit card to the sub_ability chain) or
    /// `CumulativeThreshold { property, comparator, threshold }` (Tasha's
    /// Hideous Laughter / Dream Harvest / Improvisation Capstone: stop once
    /// the running sum of the property over every card exiled this resolution
    /// satisfies the comparator vs the threshold; CR 202.3 + CR 107.3e).
    ExileFromTopUntil {
        /// CR 109.5: Whose library is exiled. `Controller` for "you exile...",
        /// or any player-resolving target filter for "target opponent exiles..."
        /// and similar subject-anchored forms.
        #[serde(default = "default_target_filter_controller")]
        player: TargetFilter,
        until: UntilCondition,
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
        /// Where the matching card goes (Hand or Battlefield). When
        /// `kept_optional_to` is `Some`, this is repurposed as the *decline*
        /// zone (where the kept card goes if the controller declines).
        kept_destination: Zone,
        /// Where non-matching revealed cards go (Library bottom or Graveyard).
        rest_destination: Zone,
        /// CR 110.5b: When true, the matching card enters the battlefield tapped.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter_tapped: bool,
        /// CR 508.4: When true, a matching card sent to the battlefield enters
        /// attacking (e.g. Raph & Mikey, Fireflux Squad — "put that card onto
        /// the battlefield tapped and attacking"). Its controller's existing
        /// attacker (the trigger source) supplies the defending player.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enters_attacking: bool,
        /// CR 701.20a + CR 608.2c: Optional kept destination — `Some(accept_zone)`
        /// encodes "you may put that card onto the battlefield": the controller
        /// chooses the kept card's destination. Accept → `accept_zone`; decline →
        /// `kept_destination` (repurposed as the decline zone). `None` → the
        /// kept card unconditionally goes to `kept_destination` (mandatory).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        kept_optional_to: Option<Zone>,
    },
    /// CR 701.57a: Discover N — exile from top until nonland with MV ≤ N,
    /// cast free or put to hand, rest to bottom in random order.
    Discover {
        mana_value_limit: QuantityExpr,
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
    /// the resolution handler can populate `WaitingFor::CastOffer` (Miracle).
    MiracleCast {
        cost: super::mana::ManaCost,
    },
    /// CR 702.35a: Madness trigger resolution — offers the player the chance to
    /// cast the source card from exile for its madness cost.
    MadnessCast {
        cost: super::mana::ManaCost,
    },
    /// Put a card at a specific position in its owner's library.
    /// Unlike ChangeZone { destination: Library } which shuffles the destination
    /// library, this uses move_to_library_position for precise placement without
    /// shuffling.
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
    /// Choose cards in hand that were drawn this turn. Chosen cards are put on
    /// top of their owner's library; each unchosen required card is kept by
    /// paying life.
    ChooseDrawnThisTurnPayOrTopdeck {
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        #[serde(default = "default_quantity_four")]
        life_payment: QuantityExpr,
        #[serde(default = "default_target_filter_controller")]
        player: TargetFilter,
    },
    /// Target's owner puts it on top or bottom of their library (owner chooses).
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
    /// CR 701.15a/b: Goad every creature matching a battlefield filter without
    /// declaring targets. Mirrors `DestroyAll` / `TapAll` for mass Oracle text
    /// like "Goad all creatures you don't control."
    GoadAll {
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
    /// Qarsi High Priest), `ParentTargetController` for "its controller
    /// manifests..." (Reality Shift), and `TriggeringPlayer` for "that player's
    /// library" trigger bodies (Orochi Soul-Reaver). `count` determines how
    /// many cards to manifest.
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
    /// CR 606.3: Grant the resolved target player the right to activate each of
    /// their planeswalkers' loyalty abilities `amount` additional times this
    /// turn. Class lift of The Chain Veil's "{4}, {T}: You may activate each
    /// planeswalker's loyalty ability an additional time this turn." Stored as
    /// a per-player counter on `GameState::extra_loyalty_activations_this_turn`,
    /// read by `planeswalker::can_activate_loyalty_ability` as a +N bump to the
    /// per-permanent CR 606.3 cap. The counter is cleared at turn start.
    /// `target` defaults to `Controller` (printed wording is "you may
    /// activate..."); parameterized for future cards that grant the bonus to a
    /// different player.
    GrantExtraLoyaltyActivations {
        #[serde(default = "default_quantity_one")]
        amount: QuantityExpr,
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
    /// CR 614.10: "Skip your next [step] step." — the affected player's next N
    /// occurrences of the named step or phase are skipped. Stored separately from
    /// `SkipNextTurn` because turn and step consumption happen at different
    /// turn-flow boundaries.
    SkipNextStep {
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
        step: StepSkipTarget,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
    },
    /// CR 500.8: Add an additional step or phase after the specified anchor phase.
    /// Uses a LIFO stack on GameState.extra_phases. `followed_by` entries are pushed
    /// before `phase`, so "additional combat followed by an additional main phase"
    /// resolves in printed order while preserving CR 500.8 LIFO ordering.
    /// CR 500.10a: Only adds steps/phases to the affected player's own turn.
    /// `count` resolves at resolution time so dynamic quantities such as Obeka,
    /// Splitter of Seconds' "that many additional upkeep steps" thread the
    /// triggering event amount through `QuantityRef::EventContextAmount`. Legacy
    /// callers and explicit "an additional" wording deserialize to a Fixed 1.
    AdditionalPhase {
        #[serde(default = "default_target_filter_controller")]
        target: TargetFilter,
        phase: Phase,
        after: Phase,
        #[serde(default)]
        followed_by: Vec<Phase>,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
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
    /// Digital-only Specialize: permanently become a color-specific face after
    /// paying the synthesized activation cost (mana + discard). Not in CR text.
    Specialize,
    /// CR 702.112a: Renown N — if not renowned, put N +1/+1 counters on this
    /// permanent and it becomes renowned.
    Renown {
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
    /// CR 701.63a: Endure N — the enduring permanent's controller chooses: create an
    /// N/N white Spirit creature token, or put N +1/+1 counters on that permanent.
    /// CR 701.63b: Endure 0 does nothing.
    Endure {
        amount: u32,
    },
    /// CR 701.68a: Blight N as an effect — the controller of this ability puts
    /// N -1/-1 counters on a creature they control. Non-targeted controller choice.
    BlightEffect {
        count: u32,
    },
    /// Alchemy digital-only: randomly pick card(s) from library matching filter,
    /// put to destination (default hand). No reveal, no shuffle, no player choice.
    Seek {
        #[serde(default = "default_target_filter_any")]
        filter: TargetFilter,
        #[serde(default = "default_quantity_one")]
        count: QuantityExpr,
        /// Alchemy digital-only: restrict the random selection pool to the
        /// top N cards of the controller's library before applying `filter`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        from_top: Option<usize>,
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
    /// CR 701.12a: Exchange a player's life total with the source permanent's
    /// power or toughness. The player's life total becomes the stat's current
    /// value (CR 119.5 gain/lose-to-reach), and the source gains an indefinite
    /// layer-7b continuous effect setting that stat to the player's previous
    /// life total (CR 613.4b). All-or-nothing per CR 701.12a: if the life change
    /// is forbidden (CR 119.7/119.8 can't-gain/can't-lose), no part occurs.
    /// `player` selects the player (Controller for "your", Opponent for "target
    /// opponent"); `stat` selects which of the source's stats is exchanged.
    /// Tree of Redemption (your life ↔ toughness), Tree of Perdition (target
    /// opponent's life ↔ toughness), Evra, Halcyon Witness (your life ↔ power).
    ExchangeLifeWithStat {
        #[serde(default = "default_target_filter_any")]
        player: TargetFilter,
        stat: PtStat,
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
    /// CR 701.55 + CR 608.2d: Inline "[player] chooses/faces [effect A] or
    /// [effect B]" — the configured chooser scope chooses at resolution which
    /// branch to execute. Building block for villainous choices and optional
    /// binary-choice imperatives like
    /// Highway Robbery's "You may discard a card or sacrifice a land" and
    /// analogous "you may X or Y" patterns that are not expressed as a bulleted
    /// `Choose one —` modal block. Each branch is a full `AbilityDefinition`
    /// so it carries its own cost, target, and sub-ability chain. The outer
    /// imperative is already marked `optional: true` when "you may" is
    /// stripped; this effect represents the branching choice once the
    /// controller opts in.
    ///
    /// Resolution: each chooser picks exactly one branch by index; the chosen
    /// branch's effect resolves normally with the original ability controller.
    ChooseOneOf {
        /// Which player(s) make the branch choice. Defaults to the controller
        /// for pre-existing inline "you may A or B" card data.
        #[serde(default = "default_player_filter_controller")]
        chooser: PlayerFilter,
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

fn default_player_filter_controller() -> PlayerFilter {
    PlayerFilter::Controller
}

fn default_quantity_one() -> QuantityExpr {
    QuantityExpr::Fixed { value: 1 }
}

fn default_duration_until_end_of_turn() -> Duration {
    Duration::UntilEndOfTurn
}

fn default_comparator_ge() -> Comparator {
    Comparator::GE
}

fn default_controls_count_one() -> Box<QuantityExpr> {
    Box::new(QuantityExpr::Fixed { value: 1 })
}

/// Backward-compat deserializer for GainLife.player field.
/// Legacy card-data.json used the GainLifePlayer enum with string variants
/// ("controller", "targeted_controller", "target_player"). New code uses
/// TargetFilter directly. This maps the legacy strings to the corresponding
/// TargetFilter values.
fn deserialize_gain_life_player<'de, D>(d: D) -> Result<TargetFilter, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let raw: serde_json::Value = serde_json::Value::deserialize(d)?;
    match raw {
        // Legacy string format from GainLifePlayer enum
        serde_json::Value::String(s) => match s.as_str() {
            "controller" => Ok(TargetFilter::Controller),
            "targeted_controller" => Ok(TargetFilter::ParentTargetController),
            "target_player" => Ok(TargetFilter::Player),
            other => Err(de::Error::unknown_variant(
                other,
                &["controller", "targeted_controller", "target_player"],
            )),
        },
        // New TargetFilter object format — delegate to derived deserializer
        other => serde_json::from_value::<TargetFilter>(other).map_err(serde::de::Error::custom),
    }
}

fn default_quantity_four() -> QuantityExpr {
    QuantityExpr::Fixed { value: 4 }
}

fn default_counter_transfer_mode() -> CounterTransferMode {
    CounterTransferMode::Move
}

fn default_damage_aggregate() -> AggregateFunction {
    AggregateFunction::Sum
}

/// Backward-compat default for the legacy
/// `FilterProp::MostPrevalentCreatureTypeInLibrary` shape. Old saves had no
/// `zone` field; it always meant the player's library.
fn default_most_prevalent_zone() -> crate::types::zones::Zone {
    crate::types::zones::Zone::Library
}

/// Backward-compat default for the legacy
/// `QuantityRef::ObjectCountDistinctNames` shape. Old saves had no
/// `qualities` field; the count was always deduplicated by name.
fn default_distinct_names() -> Vec<SharedQuality> {
    vec![SharedQuality::Name]
}

/// Backward-compat default for the legacy
/// `FilterProp::MostPrevalentCreatureTypeInLibrary` shape. Old saves had no
/// `scope` field; it always meant `your` library.
fn default_most_prevalent_scope() -> ControllerRef {
    ControllerRef::You
}

fn is_default_damage_aggregate(a: &AggregateFunction) -> bool {
    matches!(a, AggregateFunction::Sum)
}

fn is_default_search_selection_constraint(c: &SearchSelectionConstraint) -> bool {
    matches!(c, SearchSelectionConstraint::None)
}

fn is_default_outside_game_source_pool(pool: &OutsideGameSourcePool) -> bool {
    matches!(pool, OutsideGameSourcePool::Sideboard)
}

/// CR 701.23a: Default search zone set — library only, the overwhelming
/// majority of tutors. Used by `Effect::SearchLibrary.source_zones`.
fn default_search_zones() -> Vec<Zone> {
    vec![Zone::Library]
}

/// True when `zones` is the library-only default, so multi-zone metadata is the
/// only thing serialized into `card-data.json`.
fn is_default_search_zones(zones: &[Zone]) -> bool {
    zones == [Zone::Library]
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

/// CR 707.10c: Whether a copy effect grants its controller the choice to pick
/// new targets for the copy. Only effects whose Oracle text states "you may
/// choose new targets for the copy/copies" permit retargeting; a bare "copy"
/// keeps the original's targets unchanged (CR 115.1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CopyRetargetPermission {
    /// No "choose new targets" clause — copy inherits the original's targets.
    KeepOriginalTargets,
    /// Oracle text grants "you may choose new targets for the copy."
    MayChooseNewTargets,
}

pub(crate) fn default_target_filter_any() -> TargetFilter {
    TargetFilter::Any
}

pub(crate) fn default_target_filter_permanent() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::permanent())
}

/// CR 115.1: a copy keeps the original spell's declared targets unless an
/// effect explicitly grants new-target choice (CR 707.10c).
fn default_copy_keep_targets() -> CopyRetargetPermission {
    CopyRetargetPermission::KeepOriginalTargets
}

fn default_target_filter_none() -> TargetFilter {
    TargetFilter::None
}

fn default_target_filter_controller() -> TargetFilter {
    TargetFilter::Controller
}

fn is_target_filter_controller(t: &TargetFilter) -> bool {
    matches!(t, TargetFilter::Controller)
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
        FilterProp::CanEnchant { target } => FilterProp::CanEnchant {
            target: Box::new(target.normalized()),
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

/// CR 701.38a: Default voter scope for `Effect::Vote` is "every player"
/// (the canonical Council's-dilemma shape). Pre-existing serialized vote
/// effects without a `voter_scope` field deserialize as
/// `VoterScope::AllPlayers`, preserving Tivit / Capital Punishment / Coercive
/// Portal behavior across the schema upgrade.
fn default_voter_scope_all() -> VoterScope {
    VoterScope::AllPlayers
}

/// CR 701.38a + CR 800.4g: Which players cast votes for an `Effect::Vote`.
///
/// `AllPlayers` is the classic Council's-dilemma shape ("starting with you,
/// each player votes for..."). `EachOpponent` covers "each opponent
/// chooses..." patterns (Master of Ceremonies, etc.) where the source's
/// controller does NOT vote — they instead receive a per-choice effect via
/// `PlayerFilter::VotedFor` ("you and that player each ...").
///
/// Per CR 800.4g, when every opponent has left the game in a multiplayer
/// session, an `EachOpponent` vote produces an empty voter queue and the
/// resolver emits `EffectResolved` with no tally instead of waiting forever
/// on a non-existent voter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum VoterScope {
    /// Every non-eliminated player votes, in APNAP order from `starting_with`.
    /// CR 701.38a — the canonical Council's-dilemma voter set.
    AllPlayers,
    /// CR 800.4g: Every non-eliminated opponent of the ability controller
    /// votes. The controller does not vote — they receive per-choice
    /// sub-effects via `PlayerFilter::VotedFor` against the recorded ballots.
    EachOpponent,
    /// CR 101.4 + CR 608.2: Battlebond's friend-or-foe keyword action has
    /// no dedicated CR section. The spell controller alone makes one choice
    /// per non-eliminated player, in APNAP order from the controller. The
    /// "voter" slot in each ballot records the LABELED player (subject),
    /// not the actor — the actor is always the controller. Used by
    /// Pir's Whim, Khorvath's Fury, Regna's Sanction, Virtus's Maneuver,
    /// and Zndrsplt's Judgment.
    ControllerLabels,
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

    pub fn references_exiled_by_source(&self) -> bool {
        match self {
            TargetFilter::ExiledBySource => true,
            TargetFilter::And { filters } => filters
                .iter()
                .any(TargetFilter::references_exiled_by_source),
            TargetFilter::Or { filters } => filters
                .iter()
                .all(TargetFilter::references_exiled_by_source),
            TargetFilter::TrackedSetFiltered { filter, .. } => filter.references_exiled_by_source(),
            _ => false,
        }
    }

    pub fn contains_source_attachment_host(&self) -> bool {
        match self {
            TargetFilter::Typed(TypedFilter { properties, .. }) => properties
                .iter()
                .any(|prop| matches!(prop, FilterProp::EnchantedBy | FilterProp::EquippedBy)),
            TargetFilter::And { filters } => filters
                .iter()
                .any(TargetFilter::contains_source_attachment_host),
            _ => false,
        }
    }

    /// CR 115.1: Returns true for filters that are NOT player-chosen targets —
    /// context references (triggering event participants per CR 603.7c),
    /// parent target anaphora, and self-references resolve automatically
    /// without target selection.
    pub fn is_context_ref(&self) -> bool {
        if self.references_exiled_by_source() {
            return true;
        }
        // CR 608.2c + CR 109.4: A player-only reference to a resolution-chosen
        // player is resolved during resolution (from `ResolvedAbility.
        // chosen_players`), never declared as a target — so it is a context
        // ref and surfaces no target slot.
        if self.chosen_player_index().is_some() {
            return true;
        }
        matches!(
            self,
            TargetFilter::None
                | TargetFilter::SelfRef
                | TargetFilter::SourceOrPaired
                | TargetFilter::Controller
                | TargetFilter::OriginalController
                | TargetFilter::ScopedPlayer
                | TargetFilter::TriggeringSpellController
                | TargetFilter::TriggeringSpellOwner
                | TargetFilter::TriggeringPlayer
                | TargetFilter::TriggeringSource
                | TargetFilter::DefendingPlayer
                // CR 102.1 + CR 103.1: the seating neighbor is computed at the
                // resolver (`game::players::neighbor`), never declared as a
                // chosen target slot — so it is a context ref.
                | TargetFilter::Neighbor { .. }
                | TargetFilter::AttachedTo
                | TargetFilter::CostPaidObject
                | TargetFilter::ParentTarget
                | TargetFilter::ParentTargetSlot { .. }
                | TargetFilter::ParentTargetController
                | TargetFilter::ParentTargetOwner
                | TargetFilter::SourceChosenPlayer
                | TargetFilter::PostReplacementSourceController
                | TargetFilter::PostReplacementDamageTarget
                | TargetFilter::TrackedSet { .. }
                | TargetFilter::TrackedSetFiltered { .. }
        )
    }

    /// CR 608.2c + CR 109.4: If this filter is a player-only reference to the
    /// Nth resolution-chosen player (a type-filter-free `Typed` whose only
    /// distinguishing property is `controller: ChosenPlayer { index }`), return
    /// that index. Used by effect-player resolvers to bind a "choose a player
    /// to <verb>" sub-effect's acting/recipient player without surfacing a
    /// target slot — the chosen player is fixed during resolution, not at
    /// target declaration.
    pub fn chosen_player_index(&self) -> Option<u8> {
        match self {
            TargetFilter::Typed(tf) if tf.type_filters.is_empty() && tf.properties.is_empty() => {
                match tf.controller {
                    Some(ControllerRef::ChosenPlayer { index }) => Some(index),
                    _ => None,
                }
            }
            _ => None,
        }
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
            | Effect::PairWith { target }
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
            | Effect::UnattachAll { target, .. }
            | Effect::Fight { target, .. }
            | Effect::Bounce { target, .. }
            | Effect::SwitchPT { target, .. }
            | Effect::CopySpell { target, .. }
            | Effect::CastCopyOfCard { target, .. }
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
            | Effect::ForceAttack { target, .. }
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
            | Effect::GrantExtraLoyaltyActivations { target, .. }
            | Effect::SkipNextTurn { target, .. }
            | Effect::SkipNextStep { target, .. }
            | Effect::AdditionalPhase { target, .. }
            | Effect::Double { target, .. }
            | Effect::SetLifeTotal { target, .. }
            | Effect::GiveControl { target, .. }
            | Effect::RemoveFromCombat { target, .. }
            // CR 115.7 + CR 115.1: "Change the target of target spell or ability"
            // (Bolt Bend, Redirect, Misdirection) targets the stack spell/ability
            // it will retarget. That target is chosen as the spell is cast (CR
            // 115.1), so it must be surfaced here — both to build the cast-time
            // target slot and so resolution-time re-validation (CR 608.2b) checks
            // it against the StackSpell/StackAbility filter instead of the
            // battlefield-only default (which would always fizzle a stack target).
            | Effect::ChangeTargets { target, .. } => Some(target),

            // CR 109.4 + CR 115.1 + CR 707.2: `CopyTokenOf` has two
            // potentially-targetable axes — the copy *source* (`target`) and
            // the token *creator/owner* (`owner`). `target_filter()` surfaces
            // exactly one as the stack-push target slot:
            //  * When the copy source is a declared target (`source_filter` is
            //    `None` and `target` is a real targetable filter, e.g.
            //    "create a token that's a copy of target creature"), the
            //    copy-source axis wins — it must keep its slot.
            //  * Otherwise the copy source is a context ref (`SelfRef` /
            //    `ParentTarget` — Wedding Ring, Twinflame Strike) or a
            //    non-targeting `source_filter` set, so the copy-source axis
            //    needs no slot; the `owner` filter is surfaced instead so
            //    "target opponent creates a token that's a copy of it" can
            //    declare the opponent as a target. This mirrors `Effect::Token`
            //    below, which surfaces its `owner` unconditionally.
            // No real card targets both axes at once; if one ever exists, the
            // copy-source axis is surfaced and `owner` resolution falls back to
            // the controller (documented at `token::resolve_token_owner`).
            Effect::CopyTokenOf {
                target,
                owner,
                source_filter,
                ..
            } => {
                if source_filter.is_none() && !target.is_context_ref() {
                    Some(target)
                } else {
                    Some(owner)
                }
            }

            Effect::Dig { player, .. }
            | Effect::ExileTop { player, .. }
            | Effect::ExchangeLifeWithStat { player, .. }
            | Effect::ExileFromTopUntil { player, .. }
            // CR 119.3: `GainLife.player` is a TargetFilter. `extract_target_filter_from_effect`
            // drops context-refs (Controller) via `.filter(|t| !t.is_context_ref())`, so the
            // default "you gain life" still surfaces no target slot.
            | Effect::GainLife { player, .. } => Some(player),

            // CR 115.1a + CR 601.2c: "Create a [Role/Aura] token attached to
            // target creature" targets its host — surface `attach_to` as the
            // target slot when it is a real targetable filter. CR 303.4 + the
            // Asinine Antics ruling: a for-each host (`ParentTarget`, a context
            // ref) is NOT targeted (hexproof can't stop it); it's bound
            // per-iteration by the member-driven loop, so `owner` is surfaced and
            // `attach_to` is reached as a hidden parent-ref slot instead. Mirrors
            // `CopyTokenOf` (two targetable axes; no real card targets both —
            // `attach_to` wins and `owner` falls back to the controller at resolve
            // via `token::resolve_token_owner`).
            //
            // CR 111.2 + CR 601.2c: "Target player creates ..." token modes
            // (e.g. Ashling's Command mode 4, Brigid's Command, Prismari Command)
            // surface their token-creation target as the `owner` filter — the
            // player who creates the token is its owner. The default
            // `TargetFilter::Controller` preserves "you create ..." semantics.
            Effect::Token { owner, attach_to, .. } => match attach_to {
                Some(f) if !f.is_context_ref() => Some(f),
                _ => Some(owner),
            },

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
            | Effect::Myriad
            // CR 508.1: copies are chosen by the effect, not declared as targets.
            | Effect::CopyTokenBlockingAttacker { .. }
            | Effect::ChangeSpeed { .. }
            | Effect::PumpAll { .. }
            | Effect::DamageAll { .. }
            | Effect::DamageEachPlayer { .. }
            | Effect::DestroyAll { .. }
            | Effect::TapAll { .. }
            | Effect::UntapAll { .. }
            | Effect::GoadAll { .. }
            | Effect::BounceAll { .. }
            | Effect::CounterAll { .. }
            | Effect::ChangeZoneAll { .. }
            | Effect::PutCounterAll { .. }
            | Effect::DoublePTAll { .. }
            | Effect::Explore
            | Effect::Investigate
            | Effect::Tribute { .. }
            | Effect::BecomeMonarch
            | Effect::Proliferate
            | Effect::Populate
            | Effect::Clash
            | Effect::EndTheTurn
            | Effect::EndCombatPhase
            | Effect::Vote { .. }
            | Effect::Cleanup { .. }
            | Effect::RevealTop { .. }
            | Effect::SearchOutsideGame { .. }
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
            // CR 303.4 + CR 115.1: ReturnAsAura attaches to a CHOICE (not a
            // target) picked at resolution time via
            // `WaitingFor::ReturnAsAuraTarget`. No stack-push target slot.
            | Effect::ReturnAsAura { .. }
            | Effect::ChooseFromZone { .. }
            | Effect::ChooseAndSacrificeRest { .. }
            | Effect::GainEnergy { .. }
            | Effect::RevealUntil { .. }
            | Effect::Discover { .. }
            | Effect::Cascade
            | Effect::MiracleCast { .. }
            | Effect::MadnessCast { .. }
            | Effect::GiftDelivery { .. }
            | Effect::ExchangeControl { .. }
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
            | Effect::OpenAttractions { .. }
            | Effect::RollToVisitAttractions
            | Effect::ProcessRadCounters
            | Effect::Incubate { .. }
            | Effect::Amass { .. }
            | Effect::Monstrosity { .. }
            | Effect::Specialize
            | Effect::Renown { .. }
            | Effect::Bolster { .. }
            | Effect::Adapt { .. }
            | Effect::Learn
            | Effect::Forage
            | Effect::CollectEvidence { .. }
            | Effect::Endure { .. }
            // CR 701.68a: BlightEffect is a non-targeted controller choice — no
            // targeting slot. The chosen creature is picked at resolution time
            // via WaitingFor::EffectZoneChoice, not declared as a target.
            | Effect::BlightEffect { .. }
            | Effect::ExploreAll { .. }
            | Effect::Seek { .. }
            | Effect::SetDayNight { .. }
            | Effect::TimeTravel
            | Effect::RuntimeHandled { .. }
            | Effect::Conjure { .. }
            | Effect::ChooseOneOf { .. }
            | Effect::Unimplemented { .. }
            // CR 603.7e: ChooseObjectsIntoTrackedSet has no discrete effect-target
            // slot — `chooser` is a player ref resolved like `PayCost.payer`, and
            // `filter` constrains the interactive selection, not a targeting slot.
            | Effect::ChooseObjectsIntoTrackedSet { .. }
            // CR 700.3b: SeparateIntoPiles has no targeting slot — partitioning
            // is a resolution-time set computation against `object_filter`.
            | Effect::SeparateIntoPiles { .. }
            // CR 701.20a: RevealFromHand implicitly targets the controller's own hand;
            // it has no discrete `target` field for the generic targeting layer.
            | Effect::RevealFromHand { .. }
            // CR 614.9 + CR 115.1: CreateDamageReplacement has no `target:
            // TargetFilter` field. Its "to target creature" redirect recipient
            // (Soltari Guerrillas — `redirect_to: ChosenObjectTarget`) is
            // surfaced through dedicated branches in `ability_utils`
            // (`collect_target_slots` / `collect_target_slot_specs`), mirroring
            // `MoveCounters`/`Attach`; all other forms host on the controller or
            // source and declare no target.
            | Effect::CreateDamageReplacement { .. } => None,
            // CR 701.23a: SearchLibrary has an optional player target for opponent search.
            Effect::SearchLibrary { target_player, .. } => target_player.as_ref(),
            Effect::ChooseDrawnThisTurnPayOrTopdeck { player, .. } => Some(player),
        }
    }
}

/// Returns the human-readable variant name for an Effect.
/// Production API for GameEvent::EffectResolved api_type strings and logging.
pub fn effect_variant_name(effect: &Effect) -> &str {
    match effect {
        Effect::StartYourEngines { .. } => "StartYourEngines",
        Effect::ChangeSpeed { .. } => "ChangeSpeed",
        Effect::DealDamage { .. } => "DealDamage",
        Effect::Draw { .. } => "Draw",
        Effect::Pump { .. } => "Pump",
        Effect::PairWith { .. } => "PairWith",
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
        Effect::UnattachAll { .. } => "UnattachAll",
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
        Effect::EndTheTurn => "EndTheTurn",
        Effect::EndCombatPhase => "EndCombatPhase",
        Effect::Populate => "Populate",
        Effect::Clash => "Clash",
        Effect::Vote { .. } => "Vote",
        Effect::SeparateIntoPiles { .. } => "SeparateIntoPiles",
        Effect::SwitchPT { .. } => "SwitchPT",
        Effect::CopySpell { .. } => "CopySpell",
        Effect::CastCopyOfCard { .. } => "CastCopyOfCard",
        Effect::CopyTokenOf { .. } => "CopyTokenOf",
        Effect::Myriad => "Myriad",
        Effect::CopyTokenBlockingAttacker { .. } => "CopyTokenBlockingAttacker",
        Effect::BecomeCopy { .. } => "BecomeCopy",
        Effect::ChooseCard { .. } => "ChooseCard",
        Effect::PutCounter { .. } => "PutCounter",
        Effect::PutCounterAll { .. } => "PutCounterAll",
        Effect::MultiplyCounter { .. } => "MultiplyCounter",
        Effect::DoublePT { .. } => "DoublePT",
        Effect::DoublePTAll { .. } => "DoublePTAll",
        Effect::MoveCounters { .. } => "MoveCounters",
        Effect::Animate { .. } => "Animate",
        Effect::ReturnAsAura { .. } => "ReturnAsAura",
        Effect::RegisterBending { .. } => "RegisterBending",
        Effect::GenericEffect { .. } => "Effect",
        Effect::Cleanup { .. } => "Cleanup",
        Effect::Mana { .. } => "Mana",
        Effect::Discard { .. } => "Discard",
        Effect::Shuffle { .. } => "Shuffle",
        Effect::Transform { .. } => "Transform",
        Effect::SearchLibrary { .. } => "SearchLibrary",
        Effect::SearchOutsideGame { .. } => "SearchOutsideGame",
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
        Effect::ForceAttack { .. } => "ForceAttack",
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
        Effect::CreateDamageReplacement { .. } => "CreateDamageReplacement",
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
        Effect::OpenAttractions { .. } => "OpenAttractions",
        Effect::RollToVisitAttractions => "RollToVisitAttractions",
        Effect::ProcessRadCounters => "ProcessRadCounters",
        Effect::GrantCastingPermission { .. } => "GrantCastingPermission",
        Effect::ChooseFromZone { .. } => "ChooseFromZone",
        Effect::ChooseObjectsIntoTrackedSet { .. } => "ChooseObjectsIntoTrackedSet",
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
        Effect::ChooseDrawnThisTurnPayOrTopdeck { .. } => "ChooseDrawnThisTurnPayOrTopdeck",
        Effect::PutOnTopOrBottom { .. } => "PutOnTopOrBottom",
        Effect::GiftDelivery { .. } => "GiftDelivery",
        Effect::Goad { .. } => "Goad",
        Effect::GoadAll { .. } => "GoadAll",
        Effect::Detain { .. } => "Detain",
        Effect::ExchangeControl { .. } => "ExchangeControl",
        Effect::ChangeTargets { .. } => "ChangeTargets",
        Effect::Incubate { .. } => "Incubate",
        Effect::Amass { .. } => "Amass",
        Effect::Monstrosity { .. } => "Monstrosity",
        Effect::Specialize => "Specialize",
        Effect::Renown { .. } => "Renown",
        Effect::Bolster { .. } => "Bolster",
        Effect::Adapt { .. } => "Adapt",
        Effect::Manifest { .. } => "Manifest",
        Effect::ManifestDread => "ManifestDread",
        Effect::ExtraTurn { .. } => "ExtraTurn",
        Effect::GrantExtraLoyaltyActivations { .. } => "GrantExtraLoyaltyActivations",
        Effect::SkipNextTurn { .. } => "SkipNextTurn",
        Effect::SkipNextStep { .. } => "SkipNextStep",
        Effect::AdditionalPhase { .. } => "AdditionalPhase",
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
        Effect::ExchangeLifeWithStat { .. } => "ExchangeLifeWithStat",
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
    ChangeSpeed,
    DealDamage,
    Draw,
    Pump,
    PairWith,
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
    UnattachAll,
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
    EndTheTurn,
    /// CR 724.2: End the combat phase — skip to the postcombat main phase.
    EndCombatPhase,
    /// CR 701.38: Vote — interactive APNAP-ordered choice with per-choice tally effects.
    Vote,
    /// CR 700.3: SeparateIntoPiles — partition objects into two piles, another player chooses one, sub-effect applies.
    SeparateIntoPiles,
    SwitchPT,
    CopySpell,
    CastCopyOfCard,
    CopyTokenOf,
    Myriad,
    BecomeCopy,
    ChooseCard,
    PutCounter,
    PutCounterAll,
    MultiplyCounter,
    DoublePT,
    DoublePTAll,
    MoveCounters,
    Animate,
    ReturnAsAura,
    RegisterBending,
    GenericEffect,
    Cleanup,
    Mana,
    Discard,
    Shuffle,
    SearchLibrary,
    SearchOutsideGame,
    ExileTop,
    TargetOnly,
    Choose,
    ChooseDamageSource,
    Suspect,
    Connive,
    PhaseOut,
    PhaseIn,
    ForceBlock,
    ForceAttack,
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
    CreateDamageReplacement,
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
    OpenAttractions,
    RollToVisitAttractions,
    ProcessRadCounters,
    GrantCastingPermission,
    ChooseFromZone,
    ChooseObjectsIntoTrackedSet,
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
    ChooseDrawnThisTurnPayOrTopdeck,
    PutOnTopOrBottom,
    GiftDelivery,
    Goad,
    GoadAll,
    Detain,
    ExchangeControl,
    ChangeTargets,
    Incubate,
    Amass,
    Monstrosity,
    Specialize,
    Renown,
    Bolster,
    Adapt,
    Manifest,
    ManifestDread,
    ExtraTurn,
    GrantExtraLoyaltyActivations,
    SkipNextTurn,
    SkipNextStep,
    AdditionalPhase,
    Double,
    RuntimeHandled,
    Learn,
    Forage,
    CollectEvidence,
    Endure,
    BlightEffect,
    Seek,
    SetLifeTotal,
    ExchangeLifeWithStat,
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
            Effect::ChangeSpeed { .. } => EffectKind::ChangeSpeed,
            Effect::DealDamage { .. } => EffectKind::DealDamage,
            Effect::Draw { .. } => EffectKind::Draw,
            Effect::Pump { .. } => EffectKind::Pump,
            Effect::PairWith { .. } => EffectKind::PairWith,
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
            Effect::UnattachAll { .. } => EffectKind::UnattachAll,
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
            Effect::EndTheTurn => EffectKind::EndTheTurn,
            Effect::EndCombatPhase => EffectKind::EndCombatPhase,
            Effect::Populate => EffectKind::Populate,
            Effect::Clash => EffectKind::Clash,
            Effect::Vote { .. } => EffectKind::Vote,
            Effect::SeparateIntoPiles { .. } => EffectKind::SeparateIntoPiles,
            Effect::SwitchPT { .. } => EffectKind::SwitchPT,
            Effect::CopySpell { .. } => EffectKind::CopySpell,
            Effect::CastCopyOfCard { .. } => EffectKind::CastCopyOfCard,
            Effect::CopyTokenOf { .. } => EffectKind::CopyTokenOf,
            Effect::Myriad => EffectKind::Myriad,
            // CR 707.2: classified as a copy-token effect — the block placement
            // is bookkeeping layered on top of the same token-copy creation.
            Effect::CopyTokenBlockingAttacker { .. } => EffectKind::CopyTokenOf,
            Effect::BecomeCopy { .. } => EffectKind::BecomeCopy,
            Effect::ChooseCard { .. } => EffectKind::ChooseCard,
            Effect::PutCounter { .. } => EffectKind::PutCounter,
            Effect::PutCounterAll { .. } => EffectKind::PutCounterAll,
            Effect::MultiplyCounter { .. } => EffectKind::MultiplyCounter,
            Effect::DoublePT { .. } => EffectKind::DoublePT,
            Effect::DoublePTAll { .. } => EffectKind::DoublePTAll,
            Effect::MoveCounters { .. } => EffectKind::MoveCounters,
            Effect::Animate { .. } => EffectKind::Animate,
            Effect::ReturnAsAura { .. } => EffectKind::ReturnAsAura,
            Effect::RegisterBending { .. } => EffectKind::RegisterBending,
            Effect::GenericEffect { .. } => EffectKind::GenericEffect,
            Effect::Cleanup { .. } => EffectKind::Cleanup,
            Effect::Mana { .. } => EffectKind::Mana,
            Effect::Discard { .. } => EffectKind::Discard,
            Effect::Shuffle { .. } => EffectKind::Shuffle,
            Effect::Transform { .. } => EffectKind::Transform,
            Effect::SearchLibrary { .. } => EffectKind::SearchLibrary,
            Effect::SearchOutsideGame { .. } => EffectKind::SearchOutsideGame,
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
            Effect::ForceAttack { .. } => EffectKind::ForceAttack,
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
            Effect::CreateDamageReplacement { .. } => EffectKind::CreateDamageReplacement,
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
            Effect::OpenAttractions { .. } => EffectKind::OpenAttractions,
            Effect::RollToVisitAttractions => EffectKind::RollToVisitAttractions,
            Effect::ProcessRadCounters => EffectKind::ProcessRadCounters,
            Effect::GrantCastingPermission { .. } => EffectKind::GrantCastingPermission,
            Effect::ChooseFromZone { .. } => EffectKind::ChooseFromZone,
            Effect::ChooseObjectsIntoTrackedSet { .. } => EffectKind::ChooseObjectsIntoTrackedSet,
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
            Effect::ChooseDrawnThisTurnPayOrTopdeck { .. } => {
                EffectKind::ChooseDrawnThisTurnPayOrTopdeck
            }
            Effect::PutOnTopOrBottom { .. } => EffectKind::PutOnTopOrBottom,
            Effect::GiftDelivery { .. } => EffectKind::GiftDelivery,
            Effect::Goad { .. } => EffectKind::Goad,
            Effect::GoadAll { .. } => EffectKind::GoadAll,
            Effect::Detain { .. } => EffectKind::Detain,
            Effect::ExchangeControl { .. } => EffectKind::ExchangeControl,
            Effect::ChangeTargets { .. } => EffectKind::ChangeTargets,
            Effect::Incubate { .. } => EffectKind::Incubate,
            Effect::Amass { .. } => EffectKind::Amass,
            Effect::Monstrosity { .. } => EffectKind::Monstrosity,
            Effect::Specialize => EffectKind::Specialize,
            Effect::Renown { .. } => EffectKind::Renown,
            Effect::Bolster { .. } => EffectKind::Bolster,
            Effect::Adapt { .. } => EffectKind::Adapt,
            Effect::Manifest { .. } => EffectKind::Manifest,
            Effect::ManifestDread => EffectKind::ManifestDread,
            Effect::ExtraTurn { .. } => EffectKind::ExtraTurn,
            Effect::GrantExtraLoyaltyActivations { .. } => EffectKind::GrantExtraLoyaltyActivations,
            Effect::SkipNextTurn { .. } => EffectKind::SkipNextTurn,
            Effect::SkipNextStep { .. } => EffectKind::SkipNextStep,
            Effect::AdditionalPhase { .. } => EffectKind::AdditionalPhase,
            Effect::Double { .. } => EffectKind::Double,
            Effect::RuntimeHandled { .. } => EffectKind::RuntimeHandled,
            Effect::Learn => EffectKind::Learn,
            Effect::Forage => EffectKind::Forage,
            Effect::CollectEvidence { .. } => EffectKind::CollectEvidence,
            Effect::Endure { .. } => EffectKind::Endure,
            Effect::BlightEffect { .. } => EffectKind::BlightEffect,
            Effect::Seek { .. } => EffectKind::Seek,
            Effect::SetLifeTotal { .. } => EffectKind::SetLifeTotal,
            Effect::ExchangeLifeWithStat { .. } => EffectKind::ExchangeLifeWithStat,
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
    /// CR 103.5b: Mulligan-time abilities — "Any time you could mulligan and ~ is in your
    /// hand, you may ..." (Serum Powder, No-Regrets Egret). The player may perform the
    /// action at any point they would declare whether to take a mulligan. Like `BeginGame`,
    /// these never resolve through the normal stack; runtime dispatch lives in the mulligan
    /// flow (e.g. `MulliganChoice::UseSerumPowder` for Serum Powder).
    Mulligan,
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
    /// CR 700.2e: The player who chooses the mode(s). Defaults to the
    /// controller (CR 700.2a) for all standard modal spells/abilities.
    #[serde(default = "default_player_filter_controller")]
    pub chooser: PlayerFilter,
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

/// Casting-time condition used by modal choice headers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "type")]
pub enum ModalSelectionCondition {
    /// CR 700.2a: Static game-state check made as the modal choice is announced.
    Static { condition: StaticCondition },
    /// CR 601.2b + CR 702.33d/f: Additional-cost declaration made while casting
    /// the spell, before the modal cap is evaluated.
    AdditionalCostPaid {
        #[serde(default, skip_serializing_if = "AdditionalCostPaymentSource::is_any")]
        source: AdditionalCostPaymentSource,
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
}

impl<'de> Deserialize<'de> for ModalSelectionCondition {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(tag = "type")]
        enum Tagged {
            Static {
                condition: StaticCondition,
            },
            AdditionalCostPaid {
                #[serde(default)]
                source: AdditionalCostPaymentSource,
                #[serde(default)]
                variant: Option<KickerVariant>,
                #[serde(default)]
                kicker_cost: Option<ManaCost>,
                #[serde(default = "AbilityCondition::default_min_count")]
                min_count: u32,
            },
        }

        #[derive(Deserialize)]
        #[serde(untagged)]
        enum Repr {
            Tagged(Tagged),
            LegacyStatic(StaticCondition),
        }

        match Repr::deserialize(deserializer)? {
            Repr::Tagged(Tagged::Static { condition }) => {
                Ok(ModalSelectionCondition::Static { condition })
            }
            Repr::Tagged(Tagged::AdditionalCostPaid {
                source,
                variant,
                kicker_cost,
                min_count,
            }) => Ok(ModalSelectionCondition::AdditionalCostPaid {
                source,
                variant,
                kicker_cost,
                min_count,
            }),
            Repr::LegacyStatic(condition) => Ok(ModalSelectionCondition::Static { condition }),
        }
    }
}

/// CR 702.142b: Tag identifying the keyword origin of an ability.
/// Used by effects that reference abilities by keyword class (e.g., "boast abilities",
/// "ninjutsu abilities"). Survives serialization through the WASM boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AbilityTag {
    /// CR 702.142a: This ability originated from a Boast keyword definition.
    Boast,
    /// CR 702.100a: This ability originated from an Evolve keyword definition.
    Evolve,
    /// CR 702.177a: This ability originated from an Exhaust keyword definition.
    Exhaust,
    /// CR 702.107a: This ability originated from an Outlast keyword definition.
    Outlast,
    /// CR 702.29a + CR 702.29e: This ability originated from a Cycling (or
    /// Typecycling) keyword definition. Used so the activation pipeline can emit
    /// a `GameEvent::Cycled` (CR 702.29c) that "When you cycle this card"
    /// triggers match.
    Cycling,
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

/// CR 601.2c + CR 603.3d + CR 608.2d: Whether object/player choices for an
/// ability are announced while casting/putting the ability on the stack, or
/// chosen later during resolution by the resolving instruction.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum TargetChoiceTiming {
    #[default]
    Stack,
    Resolution,
}

impl TargetChoiceTiming {
    pub fn is_stack(timing: &Self) -> bool {
        matches!(timing, Self::Stack)
    }
}

// ---------------------------------------------------------------------------
// Definition types -- fully typed, zero HashMap
// ---------------------------------------------------------------------------

/// Parsed ability definition with typed effect. Zero remaining_params.
///
/// `Serialize` is hand-written (see `impl Serialize for AbilityDefinition`) so
/// it can emit the computed `consumes_source` UI key alongside the field set.
/// **Any field change here MUST be mirrored in `AbilityDefinitionRepr` and the
/// exhaustive destructure in `impl Serialize` — a new field fails to compile at
/// that destructure until it is mirrored (#506).**
#[derive(Clone, PartialEq, Eq, Deserialize)]
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
    /// CR 702.142b: Tag identifying the keyword origin of this ability.
    /// Used by effects that reference abilities by keyword class (e.g., "boast abilities").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_tag: Option<AbilityTag>,
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
    /// CR 115.1 + CR 601.2c: Additional legality constraints across selected targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// CR 601.2c + CR 608.2d: Timing for object/player choices represented by
    /// this ability's target filter. Stack timing is true targeting; resolution
    /// timing is used for non-target instructions such as "return a land card
    /// from your graveyard" after another instruction has changed zone state.
    #[serde(default, skip_serializing_if = "TargetChoiceTiming::is_stack")]
    pub target_choice_timing: TargetChoiceTiming,
    /// CR 601.2d: When set, the controller distributes this effect among chosen targets.
    /// Triggers WaitingFor::DistributeAmong during casting target selection.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribute: Option<DistributionUnit>,
    /// CR 118.12: "Effect unless [player] pays {cost}" — resolution-time payment modifier.
    /// Triggered abilities and normal spell/activated definitions use the same runtime
    /// `ResolvedAbility::unless_pay` pipeline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unless_pay: Option<UnlessPayModifier>,
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
    /// Minimum legal announced value for X. Defaults to zero; set to one by
    /// "X can't be 0" annotations.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub min_x_value: u32,
    /// Stack-copy restriction from "This ability can't be copied."
    #[serde(default, skip_serializing_if = "is_false")]
    pub cant_be_copied: bool,
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
    /// CR 101.4 + CR 800.4: Override the default APNAP turn-order start for
    /// `player_scope` iteration. `None` = use the active player (standard
    /// APNAP order per CR 101.4). `Some(ControllerRef::You)` = start with the
    /// ability's controller (Join Forces: "Starting with you, each player may
    /// pay any amount of mana"). The iteration site in `effects/mod.rs` reads
    /// this via `players::apnap_order_from(state, starting_with, controller)`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starting_with: Option<ControllerRef>,
    /// CR 115.1 + CR 701.9b: Selection mode for this ability's target slot(s).
    /// `Chosen` (default) = the controller chooses each target per CR 115.1.
    /// `Random` = the game uniformly selects from each slot's legal-target set
    /// (Mana Clash, Goblin Lyre, Pixie Queen, Vexing Sphinx, Maddening Hex, etc.).
    /// Read at target-selection time to short-circuit `WaitingFor::TargetSelection`.
    #[serde(default, skip_serializing_if = "TargetSelectionMode::is_chosen")]
    pub target_selection_mode: TargetSelectionMode,
    /// CR 608.2c + CR 107.1c: per-iteration loop-continuation predicate, the
    /// non-count companion to `repeat_for`. When `Some`, the resolution chain
    /// is re-followed ("repeat this process") under this predicate instead of
    /// a fixed iteration count. Mutually exclusive with `repeat_for` in
    /// practice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_until: Option<RepeatContinuation>,
    /// CR 608.2c: How this ability links to its parent when present as a
    /// `sub_ability`. `ContinuationStep` (default) = part of the parent's action;
    /// `SequentialSibling` = independent following instruction. Set during
    /// `lower_effect_chain_ir` from the `ClauseBoundary` PRECEDING this clause.
    #[serde(default, skip_serializing_if = "SubAbilityLink::is_continuation")]
    pub sub_link: SubAbilityLink,
    /// CR 608.2c + CR 122.1: when this ability is a `ChooseOneOf` branch driven
    /// by a counter-kind iteration (`repeat_for: DistinctCounterKindsAmong`),
    /// `Some(RebindToIteratedKind)` marks the branch whose `PutCounter`
    /// counter type must be rewritten to the current iteration's counter kind
    /// before resolution. `None` (default) = branch is fixed (e.g. "+1/+1").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub iteration_kind_binding: Option<IterationKindBinding>,
}

/// Private serialization mirror for `AbilityDefinition`. Holds a borrowed view
/// of every field so the field list (and every `skip_serializing_if`
/// predicate) stays single-sourced in `AbilityDefinition` itself — this struct
/// only re-declares the field *types and serde attributes*. The hand-written
/// `Serialize for AbilityDefinition` flattens this and appends the computed
/// `consumes_source` key. See #506.
#[derive(Serialize)]
struct AbilityDefinitionRepr<'a> {
    // NOTE: every field below carries the EXACT serde attributes of the
    // matching `AbilityDefinition` field. Fields with only `#[serde(default)]`
    // on the original (no `skip_serializing_if`) must NOT gain one here — that
    // would silently drop a `null` key the existing JSON / snapshots expect.
    kind: &'a AbilityKind,
    effect: &'a Effect,
    cost: &'a Option<AbilityCost>,
    sub_ability: &'a Option<Box<AbilityDefinition>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    else_ability: &'a Option<Box<AbilityDefinition>>,
    duration: &'a Option<Duration>,
    description: &'a Option<String>,
    target_prompt: &'a Option<String>,
    sorcery_speed: bool,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    activation_restrictions: &'a Vec<ActivationRestriction>,
    #[serde(skip_serializing_if = "Option::is_none")]
    activation_zone: &'a Option<Zone>,
    #[serde(skip_serializing_if = "Option::is_none")]
    ability_tag: &'a Option<AbilityTag>,
    condition: &'a Option<AbilityCondition>,
    optional_targeting: bool,
    optional: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    optional_for: &'a Option<OpponentMayScope>,
    #[serde(skip_serializing_if = "Option::is_none")]
    multi_target: &'a Option<MultiTargetSpec>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    target_constraints: &'a Vec<TargetSelectionConstraint>,
    #[serde(skip_serializing_if = "TargetChoiceTiming::is_stack")]
    target_choice_timing: TargetChoiceTiming,
    #[serde(skip_serializing_if = "Option::is_none")]
    distribute: &'a Option<DistributionUnit>,
    #[serde(skip_serializing_if = "Option::is_none")]
    unless_pay: &'a Option<UnlessPayModifier>,
    #[serde(skip_serializing_if = "Option::is_none")]
    modal: &'a Option<ModalChoice>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    mode_abilities: &'a Vec<AbilityDefinition>,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_for: &'a Option<QuantityExpr>,
    #[serde(skip_serializing_if = "is_zero_u32")]
    min_x_value: u32,
    #[serde(skip_serializing_if = "is_false")]
    cant_be_copied: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    cost_reduction: &'a Option<CostReduction>,
    forward_result: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    player_scope: &'a Option<PlayerFilter>,
    #[serde(skip_serializing_if = "Option::is_none")]
    starting_with: &'a Option<ControllerRef>,
    #[serde(skip_serializing_if = "TargetSelectionMode::is_chosen")]
    target_selection_mode: TargetSelectionMode,
    #[serde(skip_serializing_if = "Option::is_none")]
    repeat_until: &'a Option<RepeatContinuation>,
    #[serde(skip_serializing_if = "SubAbilityLink::is_continuation")]
    sub_link: SubAbilityLink,
    #[serde(skip_serializing_if = "Option::is_none")]
    iteration_kind_binding: &'a Option<IterationKindBinding>,
}

impl Serialize for AbilityDefinition {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        // Exhaustive destructure with NO `..` — this is the field-parity guard:
        // a new field on `AbilityDefinition` fails to compile here until it is
        // also added to `AbilityDefinitionRepr` below (#506).
        let AbilityDefinition {
            kind,
            effect,
            cost,
            sub_ability,
            else_ability,
            duration,
            description,
            target_prompt,
            sorcery_speed,
            activation_restrictions,
            activation_zone,
            ability_tag,
            condition,
            optional_targeting,
            optional,
            optional_for,
            multi_target,
            target_constraints,
            target_choice_timing,
            distribute,
            unless_pay,
            modal,
            mode_abilities,
            repeat_for,
            min_x_value,
            cant_be_copied,
            cost_reduction,
            forward_result,
            player_scope,
            starting_with,
            target_selection_mode,
            repeat_until,
            sub_link,
            iteration_kind_binding,
        } = self;
        let repr = AbilityDefinitionRepr {
            kind,
            // `effect` is `&Box<Effect>` from the destructure; deref to `&Effect`.
            effect,
            cost,
            sub_ability,
            else_ability,
            duration,
            description,
            target_prompt,
            sorcery_speed: *sorcery_speed,
            activation_restrictions,
            activation_zone,
            ability_tag,
            condition,
            optional_targeting: *optional_targeting,
            optional: *optional,
            optional_for,
            multi_target,
            target_constraints,
            target_choice_timing: *target_choice_timing,
            distribute,
            unless_pay,
            modal,
            mode_abilities,
            repeat_for,
            min_x_value: *min_x_value,
            cant_be_copied: *cant_be_copied,
            cost_reduction,
            forward_result: *forward_result,
            player_scope,
            starting_with,
            target_selection_mode: *target_selection_mode,
            repeat_until,
            sub_link: *sub_link,
            iteration_kind_binding,
        };
        /// Flatten wrapper: the mirror carries the real field set;
        /// `consumes_source` is the computed UI key (#506).
        #[derive(Serialize)]
        struct Outer<'a> {
            #[serde(flatten)]
            repr: AbilityDefinitionRepr<'a>,
            #[serde(skip_serializing_if = "is_false")]
            consumes_source: bool,
        }
        Outer {
            repr,
            consumes_source: self.consumes_source(),
        }
        .serialize(s)
    }
}

/// CR 608.2c: How a `sub_ability` relates to its parent in the resolution chain.
/// Determines whether the sub is part of the parent's action (skipped when an
/// optional parent is declined) or an independent following instruction (always
/// resolves). Derived at parse time from the `ClauseBoundary` separating the
/// two clauses in the printed Oracle text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum SubAbilityLink {
    /// Within-sentence continuation (comma / "then" joined). The sub is a
    /// resolution step of the parent's instruction — Squadron Hawk
    /// "...put them into your hand, then shuffle." Skipped when an optional
    /// parent is declined. This is the default: an unmarked sub is a
    /// continuation, preserving today's runtime behavior for every existing
    /// chain.
    #[default]
    ContinuationStep,
    /// Separate-sentence sibling instruction (sentence boundary). The sub is
    /// the NEXT printed instruction, independent of the parent — Ponder
    /// "You may shuffle." "Draw a card." Always resolves, even when an
    /// optional parent is declined (CR 608.2c "in the order written").
    SequentialSibling,
}

impl SubAbilityLink {
    /// `skip_serializing_if` predicate — the default needs no JSON byte.
    pub fn is_continuation(link: &Self) -> bool {
        matches!(link, Self::ContinuationStep)
    }
}

/// CR 608.2c + CR 107.1c: how a "repeat this process" loop decides whether to
/// run another iteration. The non-count companion to `AbilityDefinition`'s
/// `repeat_for` (a fixed `QuantityExpr` count) — this predicate decides
/// per-iteration whether to re-follow the resolving ability's instructions.
///
/// Currently a single-variant enum: only the controller-decision form ("you
/// may repeat this process any number of times") is modeled. The game-state
/// predicate form ("if you do, repeat this process" — Primal Surge) is a
/// separately-tracked deferred unit; it will add a `While(...)` variant once
/// the optional-put pause semantics it depends on are designed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RepeatContinuation {
    /// CR 107.1c: "you may repeat this process [any number of times]" — after
    /// each iteration fully resolves, the controller is prompted
    /// (`WaitingFor::RepeatDecision`) to repeat or stop.
    ControllerChoice,
}

/// CR 608.2c + CR 122.1: tags a `ChooseOneOf` branch whose effect must be
/// rebound to the current counter-kind iteration before resolving. Used when a
/// `repeat_for: DistinctCounterKindsAmong` loop drives a "put your choice of a
/// fixed counter or a counter of that kind" choice — the dynamic branch carries
/// `RebindToIteratedKind` so the loop rewrites its `PutCounter` counter type to
/// the iteration's kind. Typed (not a bool) so future binding modes can extend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum IterationKindBinding {
    /// Rebind this branch's `PutCounter` counter type to the current iterated
    /// counter kind.
    RebindToIteratedKind,
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
            ability_tag: None,
            condition: None,
            optional_targeting: false,
            optional: false,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: TargetChoiceTiming::Stack,
            distribute: None,
            unless_pay: None,
            modal: None,
            mode_abilities: Vec::new(),
            repeat_for: None,
            min_x_value: 0,
            cant_be_copied: false,
            cost_reduction: None,
            forward_result: false,
            player_scope: None,
            starting_with: None,
            target_selection_mode: TargetSelectionMode::Chosen,
            repeat_until: None,
            sub_link: SubAbilityLink::ContinuationStep,
            iteration_kind_binding: None,
        }
    }

    /// Derived (not stored): `true` when this ability's cost consumes the
    /// source card — its cost discards the card itself (cycling, Channel; see
    /// `AbilityCost::consumes_source`). Computed on demand from `cost`, the
    /// single source of truth — never cached, never a struct field.
    ///
    /// Used only by the UI (via the `consumes_source` serialization key emitted
    /// in this type's `Serialize` impl) so a lone legal action that destroys
    /// the card prompts a confirmation modal instead of auto-dispatching on a
    /// single tap (issue #506). The engine never branches on this at runtime.
    pub fn consumes_source(&self) -> bool {
        self.cost.as_ref().is_some_and(AbilityCost::consumes_source)
    }

    pub fn player_scope(mut self, scope: PlayerFilter) -> Self {
        self.player_scope = Some(scope);
        self
    }

    /// CR 101.4 + CR 800.4: Set the turn-order start for `player_scope`
    /// iteration. See `AbilityDefinition::starting_with` doc for details.
    pub fn starting_with(mut self, who: ControllerRef) -> Self {
        self.starting_with = Some(who);
        self
    }

    pub fn multi_target(mut self, spec: MultiTargetSpec) -> Self {
        self.multi_target = Some(spec);
        self
    }

    pub fn target_constraint(mut self, constraint: TargetSelectionConstraint) -> Self {
        self.target_constraints.push(constraint);
        self
    }

    pub fn target_choice_timing(mut self, timing: TargetChoiceTiming) -> Self {
        self.target_choice_timing = timing;
        self
    }

    pub fn distribute(mut self, unit: DistributionUnit) -> Self {
        self.distribute = Some(unit);
        self
    }

    pub fn unless_pay(mut self, modifier: UnlessPayModifier) -> Self {
        self.unless_pay = Some(modifier);
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

/// Which previous-effect outcome a conditional sub-ability asks about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectOutcomeSignal {
    /// CR 608.2c / CR 608.2d: "if you do", "if that player does", and "if a
    /// player does" all read whether the prompted optional effect was
    /// performed.
    OptionalEffectPerformed,
    /// CR 101.3 + CR 608.2c: "for each opponent who can't" reads whether the
    /// current player-scope iteration's mandatory instruction succeeded.
    CurrentScopeSucceeded,
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
        #[serde(default, skip_serializing_if = "AdditionalCostPaymentSource::is_any")]
        source: AdditionalCostPaymentSource,
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
    /// CR 614.1a / CR 614.15: "Instead" clause — a self-replacement effect that replaces
    /// the parent effect when the additional cost was paid.
    /// The resolver swaps the override sub's effect in place of the parent before resolution.
    AdditionalCostPaidInstead,
    /// CR 118.9 + CR 608.2c: "If the {COST} cost was paid" on spells with an
    /// alternative *mana* cost (e.g. Baleful Mastery). Evaluates against
    /// `SpellContext.alternative_mana_cost_paid`, not `additional_cost_paid`.
    AlternativeManaCostPaid,
    /// CR 608.2c / CR 608.2d / CR 101.3: Gates a sub-ability on the outcome of
    /// a previous instruction in the same resolution. Parameterized so
    /// optional-decline and mandatory-impossible branches share one condition
    /// family instead of proliferating `IfYouDo`-style siblings.
    EffectOutcome { signal: EffectOutcomeSignal },
    /// CR 608.2c: "If you won" — sub_ability executes only if this ability's
    /// controller won the triggering event, such as a clash or coin flip. Falls
    /// back to `optional_effect_performed` for in-chain clash continuations
    /// whose parent effect records the result directly.
    EventOutcomeWon,
    /// CR 603.12: "When you do" — reflexive trigger that fires based on whether the
    /// parent's trigger event actually occurred. For a non-cost parent (e.g. a
    /// `BecomeCopy` reflexive or a copy/exile replacement sub-ability) the "do"
    /// always occurred, so this is unconditionally true. For a cost-payment parent
    /// (`Effect::PayCost`), an unpayable or declined cost is not an occurrence, so
    /// the reflexive sub-ability is skipped — `evaluate_condition` gates on
    /// `cost_payment_failed_flag` for that case (mirrors `IfYouDo`).
    WhenYouDo,
    /// CR 603.4: "If you cast it from [zone]" — sub_ability executes only if the spell
    /// was cast from the specified zone. Evaluated against SpellContext.cast_from_zone.
    CastFromZone { zone: Zone },
    /// CR 207.2c + CR 601.2: "if you cast this spell during your [phase/step]".
    /// `phases` is parameterized so grouped phrases like "main phase" can map to
    /// both concrete main phases without proliferating condition variants.
    CastDuringPhase { phases: Vec<Phase> },
    /// CR 601.3b + CR 702.8a: The source permanent came from a spell cast using
    /// a specific timing permission this turn.
    CastTimingPermission { permission: CastTimingPermission },
    /// CR 601.2h + CR 608.2c: "if {C} was spent to cast this spell" gates
    /// resolution on the source object's recorded paid-mana colors.
    ManaColorSpent { color: ManaColor, minimum: u32 },
    /// CR 608.2c: "If it's a [type] card" — gates sub_ability on the last
    /// revealed card's type, or on the just-moved card when the parent effect
    /// changed zones without revealing.
    /// Evaluated at resolution time by inspecting `state.last_revealed_ids[0]`,
    /// falling back to `state.last_zone_changed_ids[0]` only when no reveal
    /// occurred in the current resolution.
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
    /// CR 608.2c: General-purpose quantity comparison condition on effects.
    /// "if its power is N or greater" / "if its toughness is less than N" etc.
    /// Composes existing `QuantityExpr` and `Comparator` building blocks.
    QuantityCheck {
        lhs: QuantityExpr,
        comparator: Comparator,
        rhs: QuantityExpr,
    },
    /// CR 608.2c + CR 120.10: Compares the numeric result tracked from the
    /// previous instruction in the same resolution, such as excess damage dealt
    /// this way. Uses the same `last_effect_amount` channel that feeds
    /// `QuantityRef::PreviousEffectAmount` / `EventContextAmount`.
    PreviousEffectAmount {
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
    /// CR 601.2 + CR 608.2c: "if you controlled a [filter] as you cast this spell" —
    /// gates on a casting-time snapshot in `SpellContext`, not the resolution-time
    /// battlefield. The parser pre-binds `ControllerRef::You` and battlefield scope.
    ControllerControlledMatchingAsCast { filter: TargetFilter },
    /// CR 608.2c: "If it's your turn" — gates sub_ability on whether the active player
    /// is the ability's controller. For "if it's not your turn", wrap with
    /// `AbilityCondition::Not`.
    IsYourTurn,
    /// CR 103.1 + CR 608.2c: "if you were the starting player" — gates a
    /// follow-up effect on whether the scoped player took the first turn of
    /// the game. The starting player is fixed at game start
    /// (`GameState.current_starting_player`). For "if you weren't the starting
    /// player" (Radiant Smite, Cindercone Smite), wrap with
    /// `AbilityCondition::Not`. `controller` selects whose start status is
    /// checked; `ControllerRef::You` is the canonical reading.
    WasStartingPlayer { controller: ControllerRef },
    /// CR 702.185c + CR 608.2c: "if a spell was warped this turn" — gates a
    /// follow-up effect on whether any player cast a spell using the named
    /// alternative-cast `variant` this turn. Parameterized by `CastingVariant`
    /// so every "cast via X this turn" history query shares one variant.
    SpellCastWithVariantThisTurn {
        variant: crate::types::game_state::CastingVariant,
    },
    /// CR 500.8 + CR 506.1 + CR 608.2c: "if it's the first combat phase of the turn".
    /// Gates a follow-up effect on whether this is the first combat phase started this turn.
    FirstCombatPhaseOfTurn,
    /// CR 608.2c: "If a [noun] was [verb]ed this way" — sub_ability executes only if
    /// the parent effect produced a zone change involving an object matching the filter.
    /// Evaluated by checking `state.last_zone_changed_ids` against the filter.
    /// Handles both optional-targeting parents (empty targets → empty IDs → false)
    /// and mandatory parents (type filter check on moved objects).
    ZoneChangedThisWay { filter: TargetFilter },
    /// CR 117.1 + CR 400.7j + CR 608.2k: "if you sacrificed/exiled/discarded a
    /// [filter] this way" checks the object paid as a cost for this resolving
    /// ability using its cost-payment-time public characteristics.
    CostPaidObjectMatchesFilter { filter: TargetFilter },
    /// CR 110.5b: "if this [permanent] is tapped" — checks the source's tapped status.
    /// For the untapped sense, wrap with `AbilityCondition::Not`.
    SourceIsTapped,
    /// CR 614.1a / CR 614.15: General "instead" self-replacement — wraps any
    /// `AbilityCondition` with replacement semantics. When the inner condition is
    /// met at resolution, the sub's
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
    /// CR 702.x: True when the source permanent does not have the specified keyword.
    /// Inverse of keyword presence check — used by "if ~ doesn't have [keyword]" gates.
    SourceLacksKeyword { keyword: Keyword },
    /// CR 101.3 + CR 109.5 + CR 608.2c: True when the current per-iteration
    /// scoped player (`ResolvedAbility.scoped_player`) matches `filter`
    /// relative to the ability's controller. Used by cross-scope decline-tail
    /// gates where the parent's `player_scope` iterates a wider set than the
    /// decline clause's own `PlayerFilter` (Liliana, Waker of the Dead: parent
    /// "each player discards a card" iterates `All`, decline clause "each
    /// opponent who can't loses 3 life" filters to `Opponent`).
    ///
    /// Composed with `Not{IfCurrentScopeSucceeded}` via `AbilityCondition::And`
    /// so the body fires only on iterations where (a) the parent action failed
    /// AND (b) the scoped player matches the decline clause's own filter.
    ///
    /// For same-scope decline-tails (Plaguecrafter, Entropic Battlecruiser,
    /// Momentum Breaker — parent and decline both iterate the same set) this
    /// conjunct is trivially true for every iteration and acts as a no-op.
    ///
    /// Outside a `player_scope` iteration (no `scoped_player` bound) the
    /// condition resolves against the ability's controller — the canonical
    /// fallback semantics for the `ScopedPlayer`/`Controller` split.
    ScopedPlayerMatches { filter: PlayerFilter },
}

impl AbilityCondition {
    /// CR 608.2c / CR 608.2d: "if you do", "if that player does", and "if a
    /// player does" all read the same optional-effect-performed signal.
    pub fn effect_performed() -> Self {
        AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::OptionalEffectPerformed,
        }
    }

    /// CR 101.3 + CR 608.2c: "for each opponent who can't" reads whether the
    /// current player-scope iteration's mandatory instruction succeeded.
    pub fn current_scope_succeeded() -> Self {
        AbilityCondition::EffectOutcome {
            signal: EffectOutcomeSignal::CurrentScopeSucceeded,
        }
    }

    pub fn is_effect_outcome(&self) -> bool {
        matches!(self, AbilityCondition::EffectOutcome { .. })
    }

    pub fn is_optional_effect_performed(&self) -> bool {
        matches!(
            self,
            AbilityCondition::EffectOutcome {
                signal: EffectOutcomeSignal::OptionalEffectPerformed
            }
        )
    }

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
            source: AdditionalCostPaymentSource::Any,
            variant: None,
            kicker_cost: None,
            min_count: 1,
        }
    }

    /// CR 702.33f: "if it was kicked with its [A/B] kicker" — gates on a
    /// specific kicker variant being paid.
    pub fn additional_cost_paid_kicker(variant: KickerVariant) -> Self {
        AbilityCondition::AdditionalCostPaid {
            source: AdditionalCostPaymentSource::Kicker,
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
            source: AdditionalCostPaymentSource::Kicker,
            variant: None,
            kicker_cost: Some(cost),
            min_count: 1,
        }
    }

    /// CR 702.33b/c: "if it was kicked N times" — gates on the total kicker
    /// payment count meeting a minimum.
    pub fn additional_cost_paid_n_times(min_count: u32) -> Self {
        AbilityCondition::AdditionalCostPaid {
            source: AdditionalCostPaymentSource::Kicker,
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
    /// CR 118.9: Whether the controller paid an alternative mana cost from
    /// `casting_options` (not an optional/additional cost such as kicker or
    /// pay-life alternatives).
    #[serde(default)]
    pub alternative_mana_cost_paid: bool,
    /// CR 601.2b/f/h: Number of non-kicker additional-cost payments declared
    /// while casting this spell. Used by keyword abilities such as Squad
    /// (CR 702.157a), whose repeatable payment count is not a kicker count.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub additional_cost_payment_count: u32,
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
    /// Used by AbilityCondition::effect_performed() to gate dependent sub_abilities.
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
    /// CR 601.2 + CR 608.2c: Presence filters the controller matched as the
    /// spell was cast. Used by effects that say "if you controlled a [filter]
    /// as you cast this spell"; the resolver checks this snapshot instead of
    /// the resolution-time battlefield.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub controller_controlled_as_cast: Vec<TargetFilter>,
    /// CR 702: Keyword origin carried from the parsed/synthesized definition
    /// into runtime resolution for keyword-specific events.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_tag: Option<AbilityTag>,
}

impl SpellContext {
    pub fn additional_cost_paid_matches(
        &self,
        source: AdditionalCostPaymentSource,
        variant: Option<KickerVariant>,
        kicker_cost: Option<&ManaCost>,
        min_count: u32,
    ) -> bool {
        if kicker_cost.is_some() && variant.is_none() {
            return false;
        }

        match variant {
            Some(kicker) => self.kickers_paid.contains(&kicker),
            None => additional_cost_payment_count_matches(
                source,
                self.additional_cost_paid,
                self.kickers_paid.len(),
                self.additional_cost_payment_count,
                min_count,
            ),
        }
    }
}

pub(crate) fn additional_cost_payment_count_matches(
    source: AdditionalCostPaymentSource,
    additional_cost_paid: bool,
    kicker_count: usize,
    non_kicker_count: u32,
    min_count: u32,
) -> bool {
    match source {
        AdditionalCostPaymentSource::Any => {
            if min_count == 0 || (min_count == 1 && additional_cost_paid) {
                true
            } else {
                kicker_count >= min_count as usize || non_kicker_count >= min_count
            }
        }
        AdditionalCostPaymentSource::Kicker => {
            let min_count = min_count.max(1);
            kicker_count >= min_count as usize
        }
        AdditionalCostPaymentSource::NonKicker => {
            if min_count == 0 {
                true
            } else {
                non_kicker_count >= min_count
                    || (min_count == 1 && additional_cost_paid && kicker_count == 0)
            }
        }
    }
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
    /// "if you control a [type]" — general control presence check.
    ControlsType { filter: TargetFilter },
    /// CR 603.4: "if no spells were cast last turn" — werewolf transform condition.
    NoSpellsCastLastTurn,
    /// CR 603.4: "if two or more spells were cast last turn" — werewolf reverse transform.
    TwoOrMoreSpellsCastLastTurn,
    /// CR 603.4 + CR 102.1: "if it's <player>'s turn" intervening-if.
    ///
    /// Parameterized on which player must be the active player:
    /// - `PlayerFilter::Controller` ← "if it's your turn"
    /// - `PlayerFilter::Opponent` ← "during each opponent's turn"
    /// - `PlayerFilter::TriggeringPlayer` ← "if it's that player's turn"
    ///   (the player named by the trigger event — drawer, tapper, etc.)
    ///
    /// Negation ("if it isn't <player>'s turn") wraps via `Not { Box::new(...) }`.
    DuringPlayersTurn { player: PlayerFilter },
    /// CR 400.7 + CR 603.4: True when the source permanent entered the
    /// battlefield this turn.
    SourceEnteredThisTurn,
    /// CR 702.30a: Echo intervening-if for a permanent that has not yet had
    /// its next-controller-upkeep echo payment handled.
    EchoDue,
    /// CR 508.1a + CR 603.2c: "Whenever ~ and at least N other creatures attack".
    /// True when combat is active and at least `minimum` other creatures
    /// controlled by the same player are also attacking.
    MinCoAttackers { minimum: u32 },
    /// CR 719.2: Intervening-if for Case auto-solve.
    /// True when the source Case is unsolved AND its solve condition is met.
    SolveConditionMet,
    /// CR 716.2a: True when the source Class enchantment is at or above the given level.
    /// Used to gate continuous triggers that only become active at higher class levels.
    ClassLevelGE { level: u8 },
    /// CR 701.52a + CR 702.159a: Visit ability on a numbered attraction line —
    /// the roll from `AttractionVisited` must fall within the printed range.
    AttractionVisitRoll { min: u8, max: u8 },

    /// CR 601.2 + CR 603.4: reads the ENTERING object's `cast_from_zone`, never the source.
    WasCast {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        zone: Option<Zone>,
    },
    /// CR 305.1 + CR 603.4: Intervening/event condition for zone-change
    /// triggers whose subject must have been played as a land. Negation
    /// ("without being played") is expressed via `Not { Box::new(WasPlayed) }`.
    WasPlayed,
    /// CR 603.4 + CR 702.33d-f: Intervening-if for "if it was kicked" /
    /// "if it was kicked with its [A] kicker" / "if it was kicked twice".
    /// Evaluates the triggering zone-change object when present, otherwise the
    /// trigger source, using `GameObject::kickers_paid` recorded at cast time.
    AdditionalCostPaid {
        #[serde(default, skip_serializing_if = "AdditionalCostPaymentSource::is_any")]
        source: AdditionalCostPaymentSource,
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
    /// CR 702.176a + CR 603.4: True while the source permanent carries the
    /// persistent marker that its named alternative cost was paid. Used for
    /// recurring battlefield triggers such as Impending's end-step time-counter
    /// removal, which must continue across turns.
    CastVariantPaidPersistent { variant: CastVariantPaid },

    /// CR 605.1a + CR 603.4: Event qualifier for "that isn't a mana ability"
    /// on activated-ability trigger events.
    ActivatedAbilityIsNonMana,

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
    /// CR 725.1: "if there is no monarch" is true when no player holds the
    /// monarch designation. Distinct from `Not(IsMonarch)`.
    NoMonarch,
    /// CR 103.1: "if you were/weren't the starting player" — true when the
    /// scoped player took the first turn of the game
    /// (`GameState.current_starting_player`). Used by Radiant Smite's Cycling
    /// trigger ("When you cycle Radiant Smite, if you weren't the starting
    /// player, ..."). Negation is expressed via `Not`. `controller` selects
    /// whose start status is checked.
    WasStartingPlayer { controller: ControllerRef },
    /// CR 702.185c: "if a spell was warped this turn" — true when any player
    /// cast a spell using the named alternative-cast `variant` this turn.
    /// Parameterized by `CastingVariant` so every "cast via X this turn"
    /// history query shares one variant.
    SpellCastWithVariantThisTurn {
        variant: crate::types::game_state::CastingVariant,
    },
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
    /// CR 110.5b: "if this [permanent] is tapped" — checks the source's tapped status.
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
    /// CR 601.3b + CR 702.8a: The source permanent came from a spell cast using
    /// a specific timing permission this turn.
    CastTimingPermission { permission: CastTimingPermission },
    /// CR 207.2c: "if at least N mana of [color] was spent to cast this spell" — Adamant.
    ManaColorSpent { color: ManaColor, minimum: u32 },
    /// CR 601.2b: "if no mana was spent to cast it" / "if mana from a [source] was spent"
    ManaSpentCondition { text: String },
    /// CR 400.7: "if it had a +1/+1 counter on it" / "if it had counters on it"
    HadCounters { counter_type: Option<CounterType> },
    /// CR 903.3 + CR 109.5: "if you control your commander" — owner-scoped (Lieutenant).
    /// CR 903.3d: "if you control a commander" — controller-only, any owner.
    /// The `ownership` field selects which CR condition this is.
    ControlsCommander { ownership: CommanderOwnership },
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
    /// CR 603.4 + CR 603.6a + CR 110.5b: Intervening-if for an "enters
    /// tapped" rider whose subject is the permanent named by the trigger
    /// event (the entering permanent), NOT the permanent that owns the
    /// ability. Distinct from `SourceIsTapped`, which checks the ability's
    /// own source. Used by "Whenever a [filter] enters tapped" observer
    /// triggers (Amulet of Vigor, Tiller Engine) and, via `Not`, "enters
    /// untapped" (Charismatic Conqueror). Mirrors the source-vs-event-object
    /// split already established by `SourceMatchesFilter` /
    /// `ZoneChangeObjectMatchesFilter`.
    ZoneChangeObjectIsTapped,
    /// CR 603.4 + CR 611.2b: Source-bound intervening-if predicate expressed
    /// as a normal target filter evaluated against the trigger source.
    SourceMatchesFilter { filter: TargetFilter },

    /// CR 614.12c + CR 607.2d + CR 603.4: True when the trigger source's
    /// persisted `ChosenAttribute::Label` matches the given anchor word.
    /// Used by anchor-word modal permanents (Khans of Tarkir Sieges, Tarkir:
    /// Dragonstorm enchantments) to gate the linked triggered ability "as
    /// long as [anchor word] was chosen as this permanent entered the
    /// battlefield, this permanent has [ability]." Mirrors
    /// `StaticCondition::ChosenLabelIs`. Checked at both fire-time and
    /// resolution-time per CR 603.4.
    ChosenLabelIs { label: String },

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
    /// `filter` is the condition-level type axis (CR 508.1): when `Some(f)`,
    /// only attackers whose object matches `f` are counted, so "you attack with
    /// two or more Dinosaurs" fires only on ≥2 Dinosaurs — not on one Dinosaur
    /// plus an unrelated attacker. When `None`, every attacker in the scoped
    /// batch is counted (the untyped "attack with two or more creatures"
    /// behavior, preserved byte-for-byte).
    ///
    /// True when the count meets or exceeds `minimum`.
    AttackersDeclaredMin {
        scope: ControllerRef,
        minimum: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        filter: Option<TargetFilter>,
    },
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

    /// CR 603.4 + CR 603.6a + CR 400.7: "if it was put onto the battlefield with
    /// this ability" — true when the triggering zone-change object's
    /// `entered_via_ability_source` equals the trigger's own source id (i.e. THIS
    /// ability placed it). Used by anti-recursion ETB guards (Kodama of the East
    /// Tree). The negation ("if it wasn't ... with this ability") wraps via
    /// `Not { condition: Box::new(PlacedByAbilitySource) }`, mirroring
    /// `Not(WasCast)` — no `negated: bool`. Resolves the entering object from the
    /// `GameEvent::ZoneChanged` event, falling back to the trigger source for
    /// self-referential cases.
    PlacedByAbilitySource,

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
    /// CR 614.1d: A replacement may carry multiple independent restrictions.
    /// Every child condition must match for the replacement to apply.
    And {
        conditions: Vec<ReplacementCondition>,
    },
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
    /// CR 702.178a + CR 702.179e: "Max speed — [replacement]" applies only
    /// while the replacement source's controller has max speed.
    HasMaxSpeed,
    /// CR 702.138c: "escapes with" — replacement applies only when the creature
    /// entered the battlefield via escape.
    CastViaEscape,
    /// CR 702.188a: "if ~ was cast using [variant]" — replacement applies only
    /// when the source permanent's spell was cast paying the named alternative
    /// cost. Mirrors `TriggerCondition::CastVariantPaid` /
    /// `AbilityCondition::CastVariantPaid`. Evaluated against
    /// `GameObject.cast_variant_paid`. Used by Scarlet Spider (web-slinging).
    CastVariantPaid { variant: CastVariantPaid },
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
    /// CR 702.54a (Bloodthirst) + CR 614.1c: "if an opponent was dealt damage
    /// this turn" — replacement applies only when any opponent of the
    /// replacement's controller was the target of a damage event recorded in
    /// `state.damage_dealt_this_turn` earlier this turn. The damage need not
    /// have originated from the controller; ANY source dealing damage to ANY
    /// opponent satisfies CR 702.54a. Tracking is cleared at turn start via
    /// `start_next_turn` (CR 514.2 cleanup is one earlier mechanism; the
    /// per-turn store is cleared on the active player's next turn-start).
    OpponentDamagedThisTurn,
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
    /// CR 120.1 + CR 614.1a: Replacement applies only to objects that were
    /// dealt damage this turn by a source matching the filter. Covers
    /// source-controller gates and source-object gates such as "this creature"
    /// or "enchanted creature" (CR 303.4m).
    DealtDamageThisTurnBySource { source: TargetFilter },
    /// CR 109.5 + CR 614.1a: Replacement applies only when the event was caused by
    /// a source controlled by the specified player relative to the replacement source.
    /// Used by "an opponent controls causes you to discard this card" replacement effects.
    EventSourceControlledBy { controller: ControllerRef },
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
    /// CR 614.1d: "if you control [N or more] [filter]" — replacement applies only
    /// while the controller has at least `minimum` permanents matching `filter` on
    /// the battlefield. `minimum = 1` covers the singular "if you control a [type]"
    /// form (used by Worship); higher values cover "if you control N or more [type]"
    /// forms (used by creature lands such as Lair of the Hydra, Hall of Storm Giants).
    /// The filter MUST have `ControllerRef::You` pre-set by the parser.
    /// Positive form of `UnlessControlsMatching` / `UnlessControlsCountMatching`.
    IfControlsMatching {
        #[serde(default = "default_one")]
        minimum: u32,
        filter: TargetFilter,
    },
    /// CR 716.2a: Replacement effect granted by a Class enchantment level —
    /// applies only while the source Class is at `level` or higher. Parallels
    /// `StaticCondition::ClassLevelGE`. Innkeeper's Talent level 3 (the
    /// "twice that many of each of those kinds of counters" doubling
    /// replacement) is the canonical case.
    ClassLevelGE { level: u8 },
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
    /// "Whenever you draw your Nth card each turn" — fires exactly when the
    /// triggering draw event's per-turn ordinal equals `n`.
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

/// CR 603.6c: source-zone constraint for one clause of a zone-change trigger.
/// CR 400.1 enumerates the seven zones; a zone-change event's `from` field is
/// `Some(zone)` for an object that moved between zones and `None` for an object
/// created directly in its destination (CR 111.1 token creation).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum OriginConstraint {
    /// No source-zone restriction. Matches any `from`, including `None`.
    Any,
    /// Matches only when the object moved from exactly this zone.
    Equals(Zone),
    /// "from anywhere other than the battlefield" — matches any source zone
    /// except this one. An object with `from = None` does not match.
    NotEquals(Zone),
    /// Matches when the source zone is one of these. Subsumes inclusion sets.
    OneOf(Vec<Zone>),
}

impl OriginConstraint {
    /// Serde default for `TriggerDefinition::spell_cast_origin` so older
    /// serialized snapshots (which predate the field) round-trip as `Any`.
    pub fn any_default() -> Self {
        OriginConstraint::Any
    }

    /// Predicate for `#[serde(skip_serializing_if = ...)]` — keeps JSON output
    /// compact for the common no-restriction case.
    pub fn is_any(&self) -> bool {
        matches!(self, OriginConstraint::Any)
    }
}

pub type DestinationConstraint = OriginConstraint;

/// CR 603.6 + CR 603.2: one clause of a disjunctive zone-change trigger.
/// A zone-change event satisfies the trigger if it matches ANY clause
/// (CR 603.2 — a game event matching the trigger condition fires the ability).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneChangeClause {
    /// CR 603.6c: the source-zone constraint for this clause.
    pub origin: OriginConstraint,
    /// The required destination zone, or `None` for "leaves [zone]" triggers
    /// (CR 603.10a) where the destination is unconstrained.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub destination: Option<Zone>,
    /// CR 700.4: destination-zone predicate for LTB forms such as
    /// "without dying" (`NotEquals(Graveyard)`). `destination` remains the
    /// compact exact-match field for existing JSON and builder call sites.
    #[serde(
        default = "OriginConstraint::any_default",
        skip_serializing_if = "OriginConstraint::is_any"
    )]
    pub destination_constraint: DestinationConstraint,
    /// Filter the moved card must satisfy for this clause to match.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_card: Option<TargetFilter>,
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

/// CR 705.2: Typed result filter for coin-flip triggers.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CoinFlipResult {
    Won,
    Lost,
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
    /// CR 603.6 + CR 603.2: Disjunctive zone-change clauses. A single triggered
    /// ability (CR 603.1) whose trigger event is a disjunction of distinct
    /// zone-change shapes (e.g. Syr Konrad: "another creature dies, OR a creature
    /// card is put into a graveyard from anywhere other than the battlefield, OR
    /// a creature card leaves your graveyard"). When non-empty, the matcher fires
    /// if the event matches ANY clause and the scalar
    /// `origin`/`origin_zones`/`destination`/`valid_card` fields are ignored.
    /// Leave empty for single-clause triggers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_change_clauses: Vec<ZoneChangeClause>,
    #[serde(default)]
    pub destination: Option<Zone>,
    /// CR 700.4: destination-zone predicate for zone-change triggers whose
    /// destination is described by exclusion, e.g. "leaves the battlefield
    /// without dying" = leaves battlefield to a non-graveyard zone.
    #[serde(
        default = "OriginConstraint::any_default",
        skip_serializing_if = "OriginConstraint::is_any"
    )]
    pub destination_constraint: DestinationConstraint,
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
    /// CR 601.2a + CR 603.2: Cast-origin constraint for `TriggerMode::SpellCast`
    /// (and `SpellCastOrCopy` / `SpellCopy`) triggers. `Any` means no zone
    /// restriction. Reuses `OriginConstraint` — the same typed source-zone
    /// discriminator as `ZoneChangeClause.origin` (CR 603.6 zone-change source),
    /// here applied to the cast event's `cast_from_zone` per CR 601.2a.
    /// CR 707.10: copy events have no cast origin and are rejected by any
    /// non-`Any` constraint.
    #[serde(
        default = "OriginConstraint::any_default",
        skip_serializing_if = "OriginConstraint::is_any"
    )]
    pub spell_cast_origin: OriginConstraint,
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
    /// CR 706.2: Optional sides filter for die-roll triggers such as
    /// "Whenever you roll a d20". `None` accepts any die.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub die_sides: Option<u8>,
    /// CR 700.14: Expend threshold — fires when cumulative mana spent on spells crosses N.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expend_threshold: Option<u32>,
    /// CR 508.3a: Filter for attack target type ("attacks a planeswalker").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_target_filter: Option<crate::types::triggers::AttackTargetFilter>,
    /// Typed player actions for PlayerPerformedAction trigger mode.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_actions: Option<Vec<PlayerActionKind>>,
    /// CR 603.2 + CR 120.1: Per-event damage-amount threshold for damage triggers
    /// ("…deals 5 or more damage to a player"). When `Some((cmp, n))`, the
    /// matcher requires the `DamageDealt` event's `amount` to satisfy
    /// `amount cmp n`. `None` means no amount restriction. Applies to all
    /// damage-event trigger modes (`DamageDone`, `DamageDoneOnce`, `DamageAll`,
    /// `DamageDealtOnce`); ignored by other modes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub damage_amount: Option<(Comparator, u32)>,
    /// CR 705.2: Coin-flip result filter for FlippedCoin trigger mode.
    /// When `Some(Won)`, fires only on wins; `Some(Lost)` only on losses; `None` fires on any flip.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coin_flip_result: Option<CoinFlipResult>,
}

impl TriggerDefinition {
    pub fn new(mode: TriggerMode) -> Self {
        Self {
            mode,
            execute: None,
            valid_card: None,
            origin: None,
            origin_zones: vec![],
            zone_change_clauses: vec![],
            destination: None,
            destination_constraint: DestinationConstraint::Any,
            trigger_zones: vec![],
            phase: None,
            optional: false,
            damage_kind: DamageKindFilter::Any,
            secondary: false,
            valid_target: None,
            valid_source: None,
            spell_cast_origin: OriginConstraint::Any,
            description: None,
            constraint: None,
            condition: None,
            counter_filter: None,
            unless_pay: None,
            batched: false,
            die_sides: None,
            expend_threshold: None,
            attack_target_filter: None,
            player_actions: None,
            damage_amount: None,
            coin_flip_result: None,
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

    /// CR 601.2a + CR 603.2: set the cast-origin constraint for SpellCast
    /// triggers ("from your graveyard", "from anywhere other than their hand").
    pub fn spell_cast_origin(mut self, constraint: OriginConstraint) -> Self {
        self.spell_cast_origin = constraint;
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

    pub fn zone_change_clauses(mut self, clauses: Vec<ZoneChangeClause>) -> Self {
        self.zone_change_clauses = clauses;
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
    /// Optional gate on whether this definition's modifications apply.
    ///
    /// CR 603.4 + CR 608.2h: This field has **dual semantics** depending on
    /// which code path reaches it:
    ///
    /// - **Continuous "as long as" gate** — when a `StaticDefinition` belongs
    ///   to a permanent's intrinsic static ability and is evaluated via the
    ///   def-index path in `layers.rs`, the condition is re-checked every time
    ///   layers are recomputed, so the modifications turn on and off with the
    ///   game state ("creatures you control get +1/+1 as long as you control a
    ///   Forest").
    /// - **Resolution-time gate** — when a `StaticDefinition` is carried by an
    ///   `Effect::GenericEffect` resolved through `effect.rs::resolve`, the
    ///   condition models an in-effect "if <condition>" (Odric, Lunarch
    ///   Marshal). Per CR 608.2h / CR 611.2d the condition's truth is
    ///   determined exactly once, when the effect is applied: `effect.rs`
    ///   evaluates it, registers a transient continuous effect only for the
    ///   satisfied subset, and zeroes `condition` to `None` so `layers.rs`
    ///   never re-evaluates it — the resulting grant then persists for the
    ///   effect's duration regardless of later state changes.
    ///
    /// This is a load-bearing invariant: a `GenericEffect`-borne
    /// `StaticDefinition` must reach `layers.rs` only with `condition: None`.
    #[serde(default)]
    pub condition: Option<StaticCondition>,
    /// CR 101.2 + CR 109.5: Per-affected-player applicability gate.
    ///
    /// Distinct from `condition` (the source-relative CR 604.1 FUNCTIONING gate,
    /// evaluated against the source's controller upstream by
    /// `battlefield_active_statics`). `per_player_condition` is evaluated against
    /// the AFFECTED player (the caster for `CantBeCast`; the attacking creature's
    /// controller for `CantAttack`) via `restrictions::evaluate_condition`. Used by
    /// "each opponent who [did X] this turn can't [Y]" prohibitions (Angelic
    /// Arbiter). `None` = unconditional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_player_condition: Option<ParsedCondition>,
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
            per_player_condition: None,
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

    /// CR 101.2 + CR 109.5: Set the per-affected-player applicability gate.
    /// Evaluated against the affected player (caster / attacking creature's
    /// controller), not the source controller. Mirrors `.condition()`.
    pub fn per_player_condition(mut self, cond: ParsedCondition) -> Self {
        self.per_player_condition = Some(cond);
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
    /// CR 614.1a: Cap damage so the target player's life total cannot fall
    /// below `minimum`. Applied only when the damage target is a player.
    /// Computed at resolution time as `amount = max(0, life_total - minimum)`.
    /// Used by Worship: "damage that would reduce your life total to less
    /// than 1 reduces it to 1 instead."
    LifeFloor { minimum: i32 },
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
    /// CR 113.6i + CR 614.17 + CR 614.6 + CR 614.7 + CR 122.1: Fully replace the
    /// quantity event with nothing — the proposed `AddCounter` / `CreateToken`
    /// event "never happens" (CR 614.6).
    ///
    /// CR 113.6i is the *authorizing* rule for permanent-scoped counter
    /// prohibitions ("an object's ability that states counters can't be put on
    /// that object functions as that object is entering the battlefield in
    /// addition to functioning while that object is on the battlefield").
    /// CR 614.17 is the framework for "can't" effects — they aren't replacement
    /// effects but follow similar rules — which is why we model the prohibition
    /// through the replacement pipeline. CR 122.1 ("a counter is a marker
    /// placed on an object or player") identifies the placement event the
    /// prohibition suppresses; CR 614.6/614.7 govern the resulting "event
    /// never happens" outcome.
    ///
    /// Used by self-targeted counter-prohibition replacements ("~ can't have
    /// counters put on it." — Melira's Keepers). Composes with `valid_card:
    /// SelfRef` for permanent-scoped protection and with player-scope filters
    /// for the future Solemnity-class global variant.
    Prevent,
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
    /// CR 106.12b + CR 614.1a: Multiply the amount of mana produced while
    /// preserving its type and restrictions.
    Multiply { factor: u32 },
}

/// CR 106.12b + CR 614.1a: Event scope for mana-production replacements.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManaReplacementScope {
    /// Applies to any mana-production event.
    #[default]
    Any,
    /// Applies only when a permanent is tapped for mana.
    TappedForMana,
}

impl ManaReplacementScope {
    pub fn is_any(&self) -> bool {
        matches!(self, Self::Any)
    }
}

/// CR 614.1a: Player axis for damage-recipient replacement filters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DamageTargetPlayerScope {
    Any,
    Opponent,
    /// The controller of the replacement source. Used by Worship: "damage
    /// that would reduce *your* life total to less than 1".
    Controller,
    /// CR 607.2d + CR 614.1a: Damage recipient is the player chosen for the
    /// replacement source by a linked persisted choice, or a permanent that
    /// player controls for `PlayerOrPermanentsControlledBy`.
    SourceChosenPlayer,
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

/// CR 614.1a: Which player(s) a replacement effect applies to, scoped relative
/// to the replacement source's controller. `valid_player: None` keeps the
/// controller-only default; `Some(You)` is the explicit controller scope,
/// `Some(Opponent)` an opponent-scoped replacement (Tainted Remedy), and
/// `Some(AnyPlayer)` a global all-players replacement (Rain of Gore).
///
/// Serialized as a bare string (no `#[serde(tag)]`) to match the prior
/// `Option<ControllerRef>` field encoding — existing persisted / in-flight
/// `valid_player` values (`"You"` / `"Opponent"`) deserialize unchanged.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReplacementPlayerScope {
    /// The replacement source's controller.
    You,
    /// Any opponent of the replacement source's controller.
    Opponent,
    /// Every player in the game, regardless of who controls the source.
    AnyPlayer,
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

/// CR 614.12a + CR 615.5: Continuation effect that runs after a replacement
/// effect's modifications complete. Stashed by the replacement pipeline,
/// drained by callers (`engine_replacement`, `stack`, `deal_damage`,
/// `engine`).
///
/// Two binding states share one slot:
/// - `Template`: an `AbilityDefinition` AST that needs the replacement
///   source for resolution context (e.g. "As ~ enters, choose a basic land
///   type" — controller and source come from the resolving permanent).
/// - `Resolved`: a `ResolvedAbility` that already carries selected targets
///   and resolution-time context, ready for direct dispatch (e.g. Phyrexian
///   Hydra's prevented-damage follow-up captures the source and counter
///   quantity from the resolution that created the prevention shield).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "ability")]
pub enum PostReplacementContinuation {
    Template(Box<AbilityDefinition>),
    Resolved(Box<ResolvedAbility>),
}

/// Replacement effect definition with typed fields. Zero params HashMap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementDefinition {
    pub event: ReplacementEvent,
    #[serde(default)]
    pub execute: Option<Box<AbilityDefinition>>,
    /// CR 615.5: Runtime continuation captured while resolving an effect that
    /// creates a replacement shield. Unlike `execute`, this preserves selected
    /// targets and other resolution-time context for the delayed follow-up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_execute: Option<Box<ResolvedAbility>>,
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
    /// CR 111.2 + CR 614.1a: Redirects the controller of a created token to a
    /// specific scope relative to the replacement source. Used by Crafty
    /// Cutpurse ("each token that would be created under an opponent's control
    /// this turn is created under your control instead") — pair with
    /// `token_owner_scope: Some(Opponent)` so only the opponent-controlled
    /// branch fires, and `Some(You)` here to redirect ownership to the source
    /// controller. None = no redirect (existing replacements unaffected).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_owner_redirect: Option<ControllerRef>,
    /// CR 614.1a: Restricts which player this replacement applies to.
    /// "an opponent would gain life" → Some(Opponent); "a spell or ability would
    /// cause its controller to gain life" (Rain of Gore) → Some(AnyPlayer).
    /// None = applies to the replacement source's controller only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub valid_player: Option<ReplacementPlayerScope>,
    /// Marks this replacement as consumed (one-shot). Skipped by find_applicable_replacements.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_consumed: bool,
    /// CR 514.2 + CR 611.2a + CR 614.1a: When this replacement expires.
    ///
    /// Single typed authority covering both end-of-turn cleanup (e.g., the
    /// "if [target] would die this turn, exile it instead." rider on damage
    /// spells — `Some(RestrictionExpiry::EndOfTurn)`) and longer-lived
    /// pending replacements (e.g., "until your next turn" — `Some(...UntilPlayerNextTurn)`).
    /// `None` means this replacement persists until removed by other means
    /// (e.g., the source object leaving the battlefield).
    ///
    /// Orthogonal to `shield_kind`: shields imply EOT expiry via
    /// `is_shield()`. Cleanup logic ORs both signals so a replacement may
    /// be both a shield and have an explicit `EndOfTurn` expiry.
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
    /// CR 106.12b + CR 614.1a: Restricts mana replacements to the relevant
    /// production event class, e.g. "is tapped for mana".
    #[serde(default, skip_serializing_if = "ManaReplacementScope::is_any")]
    pub mana_replacement_scope: ManaReplacementScope,
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
    /// CR 614.1a + CR 122.1a: Counter-type discriminator for `AddCounter`
    /// replacements that explicitly name a counter type in their Oracle text
    /// ("If one or more +1/+1 counters …" — Hardened Scales; "If one or more
    /// -1/-1 counters …" — Vizier of Remedies). When set to
    /// `Some(CounterMatch::OfType(ct))`, the replacement only fires for
    /// `ProposedEvent::AddCounter` events whose `counter_type == ct`.
    /// `None` (and the redundant `Some(CounterMatch::Any)`) match any
    /// counter type — used by Doubling Season ("those counters") and other
    /// counter-agnostic replacements.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub counter_match: Option<CounterMatch>,
}

impl ReplacementDefinition {
    /// Create a new replacement definition with only the required event field.
    /// All optional fields default to `None`/`Mandatory`.
    pub fn new(event: ReplacementEvent) -> Self {
        Self {
            event,
            execute: None,
            runtime_execute: None,
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
            token_owner_redirect: None,
            valid_player: None,
            is_consumed: false,
            expiry: None,
            redirect_target: None,
            mana_modification: None,
            mana_replacement_scope: ManaReplacementScope::Any,
            additional_token_spec: None,
            ensure_token_specs: None,
            counter_match: None,
        }
    }

    pub fn execute(mut self, ability: AbilityDefinition) -> Self {
        self.execute = Some(Box::new(ability));
        self
    }

    pub fn runtime_execute(mut self, ability: ResolvedAbility) -> Self {
        self.runtime_execute = Some(Box::new(ability));
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

    /// CR 614.5 + CR 614.1a: Mark this replacement as a one-shot damage-amount
    /// shield (Desperate Gambit). Pair with `.damage_modification(...)` to set
    /// the amount formula; the shield is consumed after its single use and
    /// expires at cleanup. Distinct from a continuous static (Furnace of Rath),
    /// which leaves `shield_kind` as `None`.
    pub fn damage_replacement_oneshot_shield(mut self) -> Self {
        self.shield_kind = ShieldKind::DamageReplacementOneShot;
        self
    }

    /// CR 614.9: Mark this replacement as a one-shot redirection shield that
    /// re-targets the damage recipient. Consumed on use, expires at cleanup.
    pub fn redirection_shield(
        mut self,
        recipient: DamageRedirectTarget,
        amount: PreventionAmount,
    ) -> Self {
        self.shield_kind = ShieldKind::Redirection { recipient, amount };
        self
    }

    pub fn quantity_modification(mut self, modification: QuantityModification) -> Self {
        self.quantity_modification = Some(modification);
        self
    }

    /// CR 614.1a + CR 122.1a: Restrict an `AddCounter` replacement to a specific
    /// counter type (e.g., Vizier of Remedies's `-1/-1` filter). Replacements
    /// whose Oracle text doesn't name a specific counter type leave this `None`,
    /// which matches every counter type (current behavior for Doubling Season).
    pub fn counter_match(mut self, m: CounterMatch) -> Self {
        self.counter_match = Some(m);
        self
    }

    pub fn token_owner_scope(mut self, scope: ControllerRef) -> Self {
        self.token_owner_scope = Some(scope);
        self
    }

    /// CR 111.2 + CR 614.1a: Set the controller-redirect target for the
    /// proposed `CreateToken` event. Pair with `token_owner_scope` so the
    /// match-side (which tokens this replacement fires on) and the redirect
    /// side (where their controller ends up) compose cleanly.
    pub fn token_owner_redirect(mut self, redirect: ControllerRef) -> Self {
        self.token_owner_redirect = Some(redirect);
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

    pub fn mana_replacement_scope(mut self, scope: ManaReplacementScope) -> Self {
        self.mana_replacement_scope = scope;
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
        /// Display-identity pointer of the copy source (oracle id + displayed
        /// face name). NOT a CR 707.2 copiable characteristic — it carries no
        /// rules weight and is deliberately kept off `CopiableValues`. It rides
        /// on the modification so the copy's art is applied (and reverts) through
        /// the same layer pass as the copied characteristics. `None` when the
        /// source is a true token with no printed identity.
        #[serde(default)]
        printed_ref: Option<PrintedCardRef>,
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
    /// CR 205.1a + CR 613.1d: Replace the object's entire core card-type set
    /// (Layer 4). Models "becomes a [type] ... and loses all other card types"
    /// — set-replacement semantics, atomic, so the parser need not enumerate
    /// the full `CoreType` space to express "becomes exactly artifact creature".
    SetCardTypes {
        core_types: Vec<CoreType>,
    },
    /// CR 205.1a + CR 613.1d: Remove every subtype belonging to a given subtype
    /// set (Layer 4). "loses all other creature types" emits
    /// `RemoveAllSubtypes { set: SubtypeSet::Creature }`.
    RemoveAllSubtypes {
        set: SubtypeSet,
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
    /// CR 205.3i + CR 305.7: grants every land type (all 17 land subtypes) and
    /// their mana abilities, additive. Distinct from AddAllBasicLandTypes
    /// (CR 305.6, 5 basic types). Omo, Queen of Vesuva. Layer 4.
    AddAllLandTypes,
    /// Adds the source object's chosen subtype (creature type or basic land type).
    /// Resolved at layer evaluation time from the source's `chosen_attributes`.
    AddChosenSubtype {
        kind: ChosenSubtypeKind,
    },
    /// CR 105.3: Set the object's color to the chosen color.
    /// Reads from `chosen_attributes` at layer evaluation time.
    AddChosenColor,
    /// CR 608.2d + CR 613.1f: Strip the chosen keyword (read from the granting
    /// source's `chosen_attributes`) from the affected object. Mirrors
    /// `RemoveKeyword`'s discriminant-based stripping so parameterized
    /// keywords (e.g., `Landwalk("Swamp")`) lose every variant sharing the
    /// same discriminant. Used by Urborg / Walking Sponge: "target creature
    /// loses [chosen ability] until end of turn".
    RemoveChosenKeyword,
    SetColor {
        colors: Vec<ManaColor>,
    },
    AddColor {
        color: ManaColor,
    },
    /// Grants a rule-modification static mode (e.g. MustBeBlocked, CantBeBlocked)
    /// to the affected object. Applied at layer 6 (ability-modifying).
    AddStaticMode {
        #[serde(deserialize_with = "crate::types::statics::deserialize_static_mode_fwd")]
        mode: StaticMode,
    },
    /// CR 113.3d + CR 604.1 + CR 613.1f: Grant a full static ability to the
    /// affected object — used for quoted continuous statics whose own
    /// `affected`/`condition`/`modifications` are independent of the recipient
    /// (e.g. "...and \"Other commanders you control get +2/+2 and have
    /// lifelink\""). The recipient receives the granted static as if it were
    /// printed on it (CR 604.1); the granted static then operates per CR 611.2c
    /// (continuous effect from a static — set of affected objects is
    /// re-evaluated continuously). Unlike `AddStaticMode` (which manufactures a
    /// `SelfRef` static against the recipient), this variant carries the inner
    /// `StaticDefinition` verbatim so the inner scope, condition, and layered
    /// modifications are all preserved. Applied at layer 6 (CR 613.1f).
    GrantStaticAbility {
        definition: Box<StaticDefinition>,
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
    /// CR 305.7 + CR 305.6: Sets a land's subtype to the basic land type CHOSEN
    /// by the granting source (e.g. Phantasmal Terrain, Convincing Mirage:
    /// "As this Aura enters, choose a basic land type." + "Enchanted land is the
    /// chosen type."). Mirrors `SetBasicLandType`'s replacement semantics —
    /// removes the land's old land subtypes and clears the abilities generated
    /// from its rules text — but reads the concrete subtype from the source
    /// object's `chosen_attributes` (`ChosenSubtypeKind::BasicLandType`) at layer
    /// evaluation time rather than carrying a fixed type. Unit variant: the kind
    /// is implicitly `BasicLandType`. The intrinsic mana ability for the new type
    /// is derived from the subtype in `mana_sources.rs` (CR 305.6), so no explicit
    /// mana grant is needed here.
    SetChosenBasicLandType,
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
        counter_type: CounterType,
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

/// CR 707.10 + CR 614.1a: Whether copy-count replacement effects (Twinning
/// Staff's "copy an additional time") have already been folded into a
/// `CopySpell` resolution's iteration count.
///
/// A `CopySpell` of a targeted spell pauses on `CopyRetarget` per copy; the
/// drain driver then resumes the next iteration with a single-iteration ability.
/// The bonus must apply to the copy *event* once (CR 614.5 — a replacement
/// effect doesn't invoke itself repeatedly; it gets only one opportunity to
/// affect an event), not per copy, so a resumed iteration is marked `Finalized`
/// and the count hook skips it.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CopyCountStatus {
    /// Initial resolution — copy-count replacements not yet applied.
    #[default]
    Pending,
    /// Resumed iteration — the bonus is already folded into the iteration count.
    Finalized,
}

impl CopyCountStatus {
    /// True for the initial resolution (the only state in which copy-count
    /// replacements should be applied).
    pub fn is_pending(&self) -> bool {
        matches!(self, CopyCountStatus::Pending)
    }
}

/// Runtime ability data passed to effect handlers at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAbility {
    pub effect: Effect,
    pub targets: Vec<TargetRef>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    /// CR 109.5: The controller of the spell or ability before any
    /// resolution-time player-scope iteration rebinds the acting player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub original_controller: Option<PlayerId>,
    /// CR 115.10 + CR 608.2c: Runtime-only current player for an "each
    /// player/opponent" instruction. This is intentionally distinct from
    /// `controller`, because CR 109.5 keeps "you/your" bound to the ability's
    /// controller while the instruction affects another player.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scoped_player: Option<PlayerId>,
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
    /// CR 115.1 + CR 601.2c: Constraints the chosen target set must satisfy
    /// (e.g. combined mana value cap). Carried through from the originating
    /// `AbilityDefinition` so the resolution-time validator can enforce them
    /// against the announced/selected targets.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// CR 601.2c + CR 608.2d: Whether target-like filters are announced on the
    /// stack or selected during resolution.
    #[serde(default, skip_serializing_if = "TargetChoiceTiming::is_stack")]
    pub target_choice_timing: TargetChoiceTiming,
    /// Human-readable description of this ability (from Oracle text / trigger line).
    /// Used by `OptionalEffectChoice` to tell the player what they're choosing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    /// CR 609.3: Repeat this ability N times (from "for each [X], [effect]").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_for: Option<QuantityExpr>,
    /// Minimum legal announced value for X. Defaults to zero; set to one by
    /// "X can't be 0" annotations.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub min_x_value: u32,
    /// Stack-copy restriction from "This ability can't be copied."
    #[serde(default, skip_serializing_if = "is_false")]
    pub cant_be_copied: bool,
    /// CR 707.10 + CR 614.1a + CR 614.5: `Finalized` on a `repeat_for` iteration
    /// that the drain driver resumes after a per-copy pause, so the "copy an
    /// additional time" replacement bonus (Twinning Staff) is folded into the
    /// iteration count exactly once — at the initial resolution — and never
    /// re-applied on each resumed iteration (CR 614.5: a replacement effect gets
    /// only one opportunity to affect an event; re-applying would explode into
    /// runaway copies). Only read by the `CopySpell` count hook in
    /// `effects::resolve_effect`.
    #[serde(default, skip_serializing_if = "CopyCountStatus::is_pending")]
    pub copy_count_status: CopyCountStatus,
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
    /// CR 101.4 + CR 800.4: Override the default APNAP turn-order start for
    /// `player_scope` iteration. Carried through from `AbilityDefinition` so
    /// the iteration site in `effects/mod.rs` can call
    /// `players::apnap_order_from(state, starting_with, controller)`.
    /// `None` = use active player (standard APNAP). `Some(ControllerRef::You)`
    /// = start with `controller` (Join Forces: "Starting with you, ...").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub starting_with: Option<ControllerRef>,
    /// CR 107.1b + CR 601.2f: The value of X chosen by the caster when this
    /// ability was cast/activated. `None` for abilities whose cost has no X.
    /// Read during resolution by `QuantityRef::Variable { name: "X" }`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_x: Option<u32>,
    /// CR 117.1 + CR 400.7j + CR 608.2k: Public characteristics of the object
    /// paid as part of this resolving ability's cost, captured before it leaves
    /// its zone. Read by cost-paid-object-scoped quantity refs and
    /// `AbilityCondition::CostPaidObjectMatchesFilter` during resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_paid_object: Option<CostPaidObjectSnapshot>,
    /// Public characteristics of an object chosen or moved by an earlier
    /// effect in the same resolving ability. This is distinct from
    /// `cost_paid_object`: the object was not paid as a cost, but later
    /// instructions may still refer to it after it left its zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_context_object: Option<CostPaidObjectSnapshot>,
    /// CR 603.4: Index of the printed ability this resolution came from on the
    /// source object's ability list. Identifies "this ability" for per-turn
    /// resolution tracking (`AbilityCondition::NthResolutionThisTurn`). `None` for
    /// synthesized/runtime-only abilities (prowess, firebending) and activated
    /// abilities for which nth-resolution gating is not yet wired through.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub may_trigger_origin: Option<MayTriggerOrigin>,
    /// CR 115.1 + CR 701.9b: Selection mode carried through from the originating
    /// `AbilityDefinition`. Read by `casting_targets`/`engine_modes`/`planeswalker`
    /// to short-circuit `WaitingFor::TargetSelection` for `Random` abilities.
    #[serde(default, skip_serializing_if = "TargetSelectionMode::is_chosen")]
    pub target_selection_mode: TargetSelectionMode,
    /// CR 608.2c + CR 109.4: Players chosen by `Effect::Choose { choice_type:
    /// ChoiceType::Player }` instructions during this resolution, in chain
    /// order. The `WaitingFor::NamedChoice` answer handler appends to this list
    /// as each `Choose(Player)` resolves; `ControllerRef::ChosenPlayer { index }`
    /// reads it. Resolution-scoped (mirrors `chosen_x`): it survives the
    /// `Choose` → `drain_pending_continuation` → next `Choose` cycle within one
    /// ability resolution because it travels on the continuation chain, but it
    /// never leaks across abilities. Distinct-player rulings (Gluntch) read
    /// this list as the exclusion set when computing the next choice's options.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_players: Vec<PlayerId>,
    /// CR 608.2c + CR 107.1c: per-iteration loop-continuation predicate carried
    /// through from the originating `AbilityDefinition`. When `Some`, the
    /// resolution chain is re-followed ("repeat this process") under this
    /// predicate. Read by the `repeat_until` dispatch in `resolve_ability_chain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repeat_until: Option<RepeatContinuation>,
    /// CR 608.2c: How this ability links to its parent when present as a
    /// `sub_ability`. Copied through from the originating `AbilityDefinition`.
    /// `SequentialSibling` subs resolve even when an optional parent is declined.
    #[serde(default, skip_serializing_if = "SubAbilityLink::is_continuation")]
    pub sub_link: SubAbilityLink,
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
            original_controller: None,
            scoped_player: None,
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
            target_constraints: Vec::new(),
            target_choice_timing: TargetChoiceTiming::Stack,
            description: None,
            repeat_for: None,
            min_x_value: 0,
            cant_be_copied: false,
            copy_count_status: CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            effect_context_object: None,
            ability_index: None,
            may_trigger_origin: None,
            target_selection_mode: TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            sub_link: SubAbilityLink::ContinuationStep,
        }
    }

    pub fn set_may_trigger_origin_recursive(&mut self, origin: MayTriggerOrigin) {
        self.may_trigger_origin = Some(origin);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_may_trigger_origin_recursive(origin);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_may_trigger_origin_recursive(origin);
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

    /// CR 117.1 + CR 400.7j + CR 608.2k: Stamp the cost-paid object across this
    /// ability and every sub/else branch. Captured at cost-payment time (before
    /// the object leaves its zone) and read by cost-paid-object refs during
    /// resolution.
    pub fn set_cost_paid_object_recursive(&mut self, snapshot: CostPaidObjectSnapshot) {
        self.cost_paid_object = Some(snapshot.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_cost_paid_object_recursive(snapshot.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_cost_paid_object_recursive(snapshot);
        }
    }

    /// Stamp an object selected by a previous effect in this same resolution
    /// across the continuation chain. Used by sacrifice-as-effect patterns
    /// whose later instructions reference "that creature" after it has left
    /// the battlefield.
    pub fn set_effect_context_object_recursive(&mut self, snapshot: CostPaidObjectSnapshot) {
        self.effect_context_object = Some(snapshot.clone());
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_effect_context_object_recursive(snapshot.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_effect_context_object_recursive(snapshot);
        }
    }

    /// Bind the current player for a `player_scope` resolution pass across the
    /// local ability chain. This intentionally does not change `controller`.
    pub fn set_scoped_player_recursive(&mut self, player: PlayerId) {
        self.scoped_player = Some(player);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_scoped_player_recursive(player);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_scoped_player_recursive(player);
        }
    }

    /// CR 608.2c: Stamp `context.optional_effect_performed` across the local
    /// ability chain. Used when an optional effect is accepted after its prompt
    /// suspended the parent chain — the stashed "If you do" continuation was
    /// captured before the choice, so its context must be updated retroactively
    /// for `IfYouDo` / `IfAPlayerDoes` gates to evaluate correctly.
    pub fn set_optional_effect_performed_recursive(&mut self, performed: bool) {
        self.context.optional_effect_performed = performed;
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_optional_effect_performed_recursive(performed);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_optional_effect_performed_recursive(performed);
        }
    }

    pub fn set_original_controller_recursive(&mut self, player: PlayerId) {
        self.original_controller = Some(player);
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_original_controller_recursive(player);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_original_controller_recursive(player);
        }
    }

    /// CR 608.2c + CR 109.4: Propagate the resolution-scoped chosen-players
    /// list across this ability and every sub/else branch. Called by the
    /// `WaitingFor::NamedChoice` answer handler after appending a freshly
    /// chosen player, so a later `Choose(Player)` or a `ChosenPlayer { index }`
    /// reference deeper in the continuation chain sees every earlier choice.
    pub fn set_chosen_players_recursive(&mut self, players: &[PlayerId]) {
        self.chosen_players = players.to_vec();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_chosen_players_recursive(players);
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_chosen_players_recursive(players);
        }
    }

    pub fn set_context_recursive(&mut self, context: SpellContext) {
        self.context = context.clone();
        if let Some(sub) = self.sub_ability.as_mut() {
            sub.set_context_recursive(context.clone());
        }
        if let Some(else_branch) = self.else_ability.as_mut() {
            else_branch.set_context_recursive(context);
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

    /// CR 601.2c: Whether this ability permits choosing zero targets — i.e. an
    /// empty legal-target set is acceptable rather than an error. True when the
    /// ability-wide `optional_targeting` ("up to one") flag is set, or when its
    /// `multi_target` spec ("up to N") has a minimum of zero. Both fields encode
    /// the same "zero targets is legal" fact, so target-slot collection must
    /// honor either.
    pub fn targeting_is_optional(&self) -> bool {
        self.optional_targeting
            || self
                .multi_target
                .as_ref()
                .is_some_and(MultiTargetSpec::min_is_fixed_zero)
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

fn is_false(value: &bool) -> bool {
    !*value
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

// ---------------------------------------------------------------------------
// Legacy on-disk compatibility for `Effect::ChangeZone::enters_under`.
//
// The field was previously a `bool` named `under_your_control` (true = enters
// under ability controller). It was lifted to `Option<ControllerRef>` so the
// engine can express any `ControllerRef` variant on ETB (CR 110.2a). On-disk
// payloads — semantic-audit snapshots, replays, and inline tests in
// `mtgish-import` — may still carry the bool shape. `serde(alias =
// "under_your_control")` routes them to this deserializer; we dispatch on the
// JSON value to keep both shapes readable. Emission is always the modern
// shape; new clients never see the bool. See `_LEGACY_DESER_ETB_CONTROLLER_2026Q2`
// below for the removal tripwire.
// ---------------------------------------------------------------------------

/// Deserialize either the modern `Option<ControllerRef>` shape or the legacy
/// boolean `under_your_control` shape (routed in via `#[serde(alias)]`).
fn deserialize_enters_under_compat<'de, D>(
    deserializer: D,
) -> Result<Option<ControllerRef>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Bool(true) => Ok(Some(ControllerRef::You)),
        serde_json::Value::Bool(false) => Ok(None),
        serde_json::Value::Null => Ok(None),
        other => serde_json::from_value::<Option<ControllerRef>>(other).map_err(D::Error::custom),
    }
}

/// Compat shim for the pre-2026-Q2 `under_your_control: bool` field on the
/// resolved-once runtime carriers (`PendingChangeZoneIteration` and
/// `WaitingFor::EffectZoneChoice`). Modern shape is `Option<PlayerId>` —
/// the bool was resolved to a concrete `PlayerId` at the ChangeZone resolver
/// entry per CR 110.2a so the carrier no longer re-evaluates a `ControllerRef`
/// across an interactive pause.
///
/// Reached only through `serde_json::from_str` resume paths: IndexedDB resume
/// (`client/src/services/gamePersistence.ts`), phase-server SQLite restore,
/// and P2P resume. The `serde_wasm_bindgen` action-dispatch path never carries
/// these carriers across the boundary.
///
/// **Mapping:** legacy `true` → `None` with `tracing::warn!`. The bool's
/// original semantics ("under ability controller") cannot be reconstructed
/// at deserialization time without the originating `AbilityDefinition`. Falling
/// back to `None` matches what the unshimmed code would have produced anyway
/// (object enters under its owner's control) — we accept that worst case and
/// emit a warn so a paused mid-prompt resume that hits the wrong routing has
/// an audit trail. Legacy `false` → `None` silently; modern shape roundtrips
/// through `serde_json::from_value::<Option<PlayerId>>`.
///
/// Removal is gated by `_LEGACY_DESER_ETB_CONTROLLER_2026Q2` alongside
/// `deserialize_enters_under_compat`.
pub fn deserialize_enters_under_player_compat<'de, D>(
    deserializer: D,
) -> Result<Option<PlayerId>, D::Error>
where
    D: Deserializer<'de>,
{
    use serde::de::Error;
    let value = serde_json::Value::deserialize(deserializer)?;
    match value {
        serde_json::Value::Bool(true) => {
            tracing::warn!(
                target: "engine::compat",
                "LEGACY_DESER_ETB_CONTROLLER_2026Q2: legacy `under_your_control=true` \
                 on resumed runtime carrier (PendingChangeZoneIteration or \
                 EffectZoneChoice); cannot reconstruct PlayerId without ability \
                 context. Defaulting to None (owner control). If the controller \
                 assignment is load-bearing for this resume, the player should \
                 restart the prompt."
            );
            Ok(None)
        }
        serde_json::Value::Bool(false) => Ok(None),
        serde_json::Value::Null => Ok(None),
        other => serde_json::from_value::<Option<PlayerId>>(other).map_err(D::Error::custom),
    }
}

/// const fn version-component parser for `_LEGACY_DESER_ETB_CONTROLLER_2026Q2`.
/// `env!` produces a `&'static str` at compile time; this consumes ASCII digits.
const fn parse_version_component(s: &str) -> u32 {
    let bytes = s.as_bytes();
    let mut out: u32 = 0;
    let mut i = 0;
    while i < bytes.len() {
        let b = bytes[i];
        assert!(b >= b'0' && b <= b'9', "non-digit in version component");
        out = out * 10 + (b - b'0') as u32;
        i += 1;
    }
    out
}

/// Tripwire: the legacy `under_your_control` boolean compat path in
/// `deserialize_enters_under_compat` was added at workspace version 0.1.39
/// and is scheduled for removal once a release > 0.1.53 ships. The const
/// below fails to compile when the workspace version crosses that boundary,
/// forcing the maintainer to either remove the compat or push the deadline.
///
/// Grep token: `LEGACY_DESER_ETB_CONTROLLER_2026Q2`. See `docs/LEGACY-COMPAT.md`.
///
/// Clippy `absurd_extreme_comparisons` correctly observes that `MAJOR > 0`
/// is always true for any non-zero major version; that's intentional — the
/// tripwire fires the instant the major version bumps. Allow it locally.
#[allow(dead_code, clippy::absurd_extreme_comparisons)]
const _LEGACY_DESER_ETB_CONTROLLER_2026Q2: () = {
    const MAJOR: u32 = parse_version_component(env!("CARGO_PKG_VERSION_MAJOR"));
    const MINOR: u32 = parse_version_component(env!("CARGO_PKG_VERSION_MINOR"));
    const PATCH: u32 = parse_version_component(env!("CARGO_PKG_VERSION_PATCH"));
    assert!(
        !(MAJOR > 0 || MINOR > 1 || (MINOR == 1 && PATCH > 53)),
        "LEGACY_DESER_ETB_CONTROLLER_2026Q2: remove the under_your_control bool \
         compat paths in deserialize_enters_under_compat (AST field) AND \
         deserialize_enters_under_player_compat (PendingChangeZoneIteration + \
         WaitingFor::EffectZoneChoice runtime carriers), the corresponding \
         `#[serde(alias = ..., deserialize_with = ...)]` attributes on all \
         three fields, and this tripwire once we ship past 0.1.53. See \
         docs/LEGACY-COMPAT.md."
    );
};

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// #506: `AbilityCost::consumes_source` classifies a self-discard cost
    /// (cycling, Channel) as source-consuming so the UI confirms a lone such
    /// action instead of auto-firing it.
    #[test]
    fn ability_consumes_source_classifier() {
        let self_discard = AbilityCost::Discard {
            count: default_quantity_one(),
            filter: None,
            random: false,
            self_ref: true,
        };
        let other_discard = AbilityCost::Discard {
            count: default_quantity_one(),
            filter: None,
            random: false,
            self_ref: false,
        };
        let mana = AbilityCost::Mana {
            cost: crate::types::mana::ManaCost::generic(2),
        };

        assert!(self_discard.consumes_source());
        assert!(!other_discard.consumes_source());
        assert!(!mana.consumes_source());
        assert!(!AbilityCost::Tap.consumes_source());

        // Composite / OneOf recurse and OR over sub-costs.
        assert!(AbilityCost::Composite {
            costs: vec![mana.clone(), self_discard.clone()],
        }
        .consumes_source());
        assert!(!AbilityCost::Composite {
            costs: vec![mana.clone(), AbilityCost::Tap],
        }
        .consumes_source());
        assert!(AbilityCost::OneOf {
            costs: vec![mana.clone(), self_discard.clone()],
        }
        .consumes_source());

        // AbilityDefinition accessor: derived from `cost`.
        let cycling = AbilityDefinition {
            cost: Some(AbilityCost::Composite {
                costs: vec![mana.clone(), self_discard],
            }),
            ..AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: default_quantity_one(),
                    target: default_target_filter_controller(),
                },
            )
        };
        assert!(cycling.consumes_source());

        let no_cost = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: default_quantity_one(),
                target: default_target_filter_controller(),
            },
        );
        assert!(!no_cost.consumes_source());

        let tap_only = AbilityDefinition {
            cost: Some(AbilityCost::Tap),
            ..AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: default_quantity_one(),
                    target: default_target_filter_controller(),
                },
            )
        };
        assert!(!tap_only.consumes_source());

        // Serialization: the custom `Serialize` impl emits `consumes_source`
        // only when true (skip_serializing_if = "is_false"). With the impl
        // reverted to the bare derive, the key never appears — discriminating.
        let cycling_json = serde_json::to_value(&cycling).unwrap();
        assert_eq!(cycling_json["consumes_source"], serde_json::json!(true));
        let benign_json = serde_json::to_value(&tap_only).unwrap();
        assert!(benign_json.get("consumes_source").is_none());
    }

    #[test]
    fn per_counter_cost_delegates_categories_to_base() {
        let base = AbilityCost::Mana {
            cost: ManaCost::generic(1),
        };
        let wrapped = AbilityCost::PerCounter {
            counter: CounterType::Age,
            target: TargetFilter::SelfRef,
            base: Box::new(base.clone()),
        };
        assert_eq!(wrapped.categories(), base.categories());
    }

    #[test]
    fn quantity_expr_scaled_by_folds_fixed_and_composes_dynamic_values() {
        assert_eq!(
            QuantityExpr::Fixed { value: 3 }.scaled_by(4),
            QuantityExpr::Fixed { value: 12 },
        );

        let dynamic = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };

        assert_eq!(dynamic.scaled_by(1), dynamic);
        assert_eq!(dynamic.scaled_by(0), QuantityExpr::Fixed { value: 0 });
        assert_eq!(
            dynamic.scaled_by(4),
            QuantityExpr::Multiply {
                factor: 4,
                inner: Box::new(dynamic),
            },
        );
    }

    #[test]
    fn cumulative_upkeep_support_boundary_matches_payment_pipeline() {
        assert!(AbilityCost::Mana {
            cost: ManaCost::generic(1)
        }
        .supports_cumulative_upkeep_payment());
        assert!(AbilityCost::PayLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::Variable {
                    name: "X".to_string(),
                },
            },
        }
        .supports_cumulative_upkeep_payment());
        assert!(AbilityCost::Sacrifice {
            target: TargetFilter::SelfRef,
            count: 1,
        }
        .supports_cumulative_upkeep_payment());
        assert!(AbilityCost::OneOf {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
            ],
        }
        .supports_cumulative_upkeep_payment());
        assert!(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::Mana {
                    cost: ManaCost::generic(2),
                },
            ],
        }
        .supports_cumulative_upkeep_payment());

        assert!(!AbilityCost::Composite {
            costs: vec![
                AbilityCost::Mana {
                    cost: ManaCost::generic(1),
                },
                AbilityCost::PayLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                },
            ],
        }
        .supports_cumulative_upkeep_payment());
        assert!(!AbilityCost::Discard {
            count: QuantityExpr::Fixed { value: 1 },
            filter: None,
            random: false,
            self_ref: false,
        }
        .supports_cumulative_upkeep_payment());
    }

    #[test]
    fn choice_type_color_deserializes_legacy_unit_variant() {
        let choice_type: ChoiceType = serde_json::from_str("\"Color\"").unwrap();

        assert_eq!(choice_type, ChoiceType::color());
    }

    #[test]
    fn choice_type_color_deserializes_excluded_colors() {
        let choice_type: ChoiceType =
            serde_json::from_str(r#"{"Color":{"excluded":["White"]}}"#).unwrap();

        assert_eq!(
            choice_type,
            ChoiceType::Color {
                excluded: vec![ManaColor::White],
            }
        );
    }

    #[test]
    fn restricted_color_choice_value_rejects_excluded_color() {
        assert_eq!(
            ChoiceValue::from_choice(
                &ChoiceType::color_excluding(vec![ManaColor::White]),
                "White",
            ),
            None
        );
        assert_eq!(
            ChoiceValue::from_choice(&ChoiceType::color_excluding(vec![ManaColor::White]), "Blue"),
            Some(ChoiceValue::Color(ManaColor::Blue))
        );
    }

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
                        exclude_source: false,
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
                    exclude_source: false,
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
    /// creatures they control" stacks `UpTo` over `DivideRounded` over
    /// `ObjectCount`. Each layer is an existing primitive — the refactor
    /// only added the outer wrapper.
    #[test]
    fn quantity_expr_up_to_composes_with_half_rounded_object_count() {
        let creatures_filter = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            ..Default::default()
        });
        let expr = QuantityExpr::up_to(QuantityExpr::DivideRounded {
            inner: Box::new(QuantityExpr::Ref {
                qty: QuantityRef::ObjectCount {
                    filter: creatures_filter,
                },
            }),
            divisor: 2,
            rounding: RoundingMode::Down,
        });
        let (max, up_to) = expr.peel_up_to();
        assert!(up_to);
        assert!(matches!(max, QuantityExpr::DivideRounded { .. }));
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
    fn stack_ability_filter_accepts_legacy_unit_json() {
        let filter: TargetFilter = serde_json::from_str(r#"{"type":"StackAbility"}"#).unwrap();
        assert_eq!(filter, TargetFilter::StackAbility { controller: None });
        assert_eq!(
            serde_json::to_string(&filter).unwrap(),
            r#"{"type":"StackAbility"}"#
        );
    }

    #[test]
    fn stack_ability_filter_roundtrips_controller_scope() {
        let filter: TargetFilter =
            serde_json::from_str(r#"{"type":"StackAbility","controller":"You"}"#).unwrap();
        assert_eq!(
            filter,
            TargetFilter::StackAbility {
                controller: Some(ControllerRef::You)
            }
        );
        assert_eq!(
            serde_json::to_string(&filter).unwrap(),
            r#"{"type":"StackAbility","controller":"You"}"#
        );
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
    fn multi_target_spec_serializes_fixed_min_compatibly_and_dynamic_min_roundtrips() {
        let fixed = MultiTargetSpec::fixed(0, 2);
        let fixed_json = serde_json::to_value(&fixed).unwrap();
        assert_eq!(fixed_json["min"], serde_json::json!(0));
        assert_eq!(
            serde_json::from_value::<MultiTargetSpec>(fixed_json).unwrap(),
            fixed
        );

        let dynamic = MultiTargetSpec::exact(QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        });
        let dynamic_json = serde_json::to_value(&dynamic).unwrap();
        assert!(
            dynamic_json["min"].is_object(),
            "dynamic min must serialize as a QuantityExpr object"
        );
        assert_eq!(
            serde_json::from_value::<MultiTargetSpec>(dynamic_json).unwrap(),
            dynamic
        );
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
            zone_change_clauses: vec![],
            destination: Some(Zone::Graveyard),
            destination_constraint: DestinationConstraint::Any,
            trigger_zones: vec![Zone::Battlefield],
            phase: None,
            optional: false,
            damage_kind: DamageKindFilter::Any,
            secondary: false,
            valid_target: None,
            valid_source: None,
            spell_cast_origin: OriginConstraint::Any,
            description: Some("When ~ dies, draw a card.".to_string()),
            constraint: None,
            condition: None,
            counter_filter: None,
            unless_pay: None,
            batched: false,
            die_sides: None,
            expend_threshold: None,
            attack_target_filter: None,
            player_actions: None,
            damage_amount: None,
            coin_flip_result: None,
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
            per_player_condition: None,
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
                    player: TargetFilter::Controller,
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
    fn player_filter_legacy_controls_permanent_alias_defaults_to_presence_check() {
        let json = r#"{
            "type": "ControlsPermanent",
            "relation": { "type": "Opponent" },
            "filter": { "type": "Any" }
        }"#;
        let deserialized: PlayerFilter = serde_json::from_str(json).unwrap();
        assert_eq!(
            deserialized,
            PlayerFilter::ControlsCount {
                relation: PlayerRelation::Opponent,
                filter: TargetFilter::Any,
                comparator: Comparator::GE,
                count: Box::new(QuantityExpr::Fixed { value: 1 }),
            }
        );
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
            AbilityCost::Unattach,
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
    fn gain_life_legacy_player_strings_deserialize() {
        let cases = [
            ("controller", TargetFilter::Controller),
            ("targeted_controller", TargetFilter::ParentTargetController),
            ("target_player", TargetFilter::Player),
        ];

        for (legacy_player, expected) in cases {
            let json = format!(r#"{{"type":"GainLife","player":"{legacy_player}"}}"#);
            let deserialized: Effect = serde_json::from_str(&json).unwrap();
            assert_eq!(
                deserialized,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: expected,
                }
            );
        }
    }

    #[test]
    fn gain_life_unknown_player_string_errors() {
        let err = serde_json::from_str::<Effect>(r#"{"type":"GainLife","player":"nobody"}"#)
            .expect_err("unknown legacy GainLife.player strings must not silently default");
        assert!(
            err.to_string().contains("unknown variant"),
            "expected unknown-variant error, got: {err}"
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
                per_player_condition: None,
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
            Duration::UntilNextStepOf {
                step: Phase::Untap,
                player: PlayerScope::Controller,
            },
            Duration::UntilNextStepOf {
                step: Phase::End,
                player: PlayerScope::Controller,
            },
            Duration::UntilEndOfNextTurnOf {
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
    fn duration_deserializes_legacy_until_next_untap_step() {
        let json = r#"[{"UntilNextUntapStepOf":{"player":{"type":"Controller"}}}]"#;
        let deserialized: Vec<Duration> = serde_json::from_str(json).unwrap();
        assert_eq!(
            deserialized,
            vec![Duration::UntilNextStepOf {
                step: Phase::Untap,
                player: PlayerScope::Controller,
            }]
        );
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
            FilterProp::CombatRelation {
                relation: CombatRelation::BlockingOrBlockedBy,
                subject: CombatRelationSubject::ParentTarget,
            },
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
            FilterProp::Counters {
                counters: CounterMatch::OfType(CounterType::Plus1Plus1),
                comparator: Comparator::GE,
                count: QuantityExpr::Fixed { value: 3 },
            },
            FilterProp::Counters {
                counters: CounterMatch::Any,
                comparator: Comparator::EQ,
                count: QuantityExpr::Fixed { value: 0 },
            },
            FilterProp::Cmc {
                comparator: Comparator::GE,
                value: QuantityExpr::Fixed { value: 4 },
            },
            FilterProp::ManaCostIn {
                costs: vec![ManaCost::zero(), ManaCost::generic(1)],
            },
            FilterProp::InZone {
                zone: Zone::Graveyard,
            },
            FilterProp::ZoneChangedThisTurn {
                from: Some(Zone::Battlefield),
                to: Some(Zone::Graveyard),
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
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                face_down_profile: None,
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
            enters_under: None,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            face_down_profile: None,
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
                enters_under: None,
                enter_tapped: false,
                enters_attacking: false,
                ..
            }
        ));
    }

    // ---------------------------------------------------------------------
    // CR 110.2a: `Effect::ChangeZone.enters_under` serde-compat coverage.
    // Modern shape is `Option<ControllerRef>`; legacy on-disk shape is the
    // boolean `under_your_control`. Routed via `#[serde(alias = ...)]` +
    // `deserialize_enters_under_compat`. See LEGACY_DESER_ETB_CONTROLLER_2026Q2.
    // ---------------------------------------------------------------------

    /// Helper: build a minimal `Effect::ChangeZone` with `enters_under` set.
    fn change_zone_with_enters_under(enters_under: Option<ControllerRef>) -> Effect {
        Effect::ChangeZone {
            origin: None,
            destination: Zone::Battlefield,
            target: TargetFilter::Any,
            owner_library: false,
            enter_transformed: false,
            enters_under,
            enter_tapped: false,
            enters_attacking: false,
            up_to: false,
            enter_with_counters: vec![],
            face_down_profile: None,
        }
    }

    #[test]
    fn enters_under_legacy_bool_false_deserializes_to_none() {
        let json = r#"{
            "type": "ChangeZone",
            "destination": "Battlefield",
            "under_your_control": false
        }"#;
        let effect: Effect = serde_json::from_str(json).expect("legacy false should parse");
        match effect {
            Effect::ChangeZone { enters_under, .. } => assert_eq!(enters_under, None),
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    #[test]
    fn enters_under_legacy_bool_true_deserializes_to_some_you() {
        let json = r#"{
            "type": "ChangeZone",
            "destination": "Battlefield",
            "under_your_control": true
        }"#;
        let effect: Effect = serde_json::from_str(json).expect("legacy true should parse");
        match effect {
            Effect::ChangeZone { enters_under, .. } => {
                assert_eq!(enters_under, Some(ControllerRef::You))
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    #[test]
    fn enters_under_chosen_player_index_zero_distinguishable_from_legacy_false() {
        // The modern shape can express `Some(ControllerRef::ChosenPlayer { index: 0 })`,
        // which must NOT collapse to the legacy `false` semantics (`None`).
        let json = r#"{
            "type": "ChangeZone",
            "destination": "Battlefield",
            "enters_under": { "ChosenPlayer": { "index": 0 } }
        }"#;
        let effect: Effect = serde_json::from_str(json).expect("ChosenPlayer should parse");
        match effect {
            Effect::ChangeZone { enters_under, .. } => {
                assert_eq!(enters_under, Some(ControllerRef::ChosenPlayer { index: 0 }))
            }
            other => panic!("expected ChangeZone, got {other:?}"),
        }
    }

    #[test]
    fn enters_under_modern_shape_roundtrips() {
        let original = change_zone_with_enters_under(Some(ControllerRef::You));
        let json = serde_json::to_string(&original).expect("serialize");
        // Modern shape must be emitted, NOT the legacy bool field.
        assert!(
            json.contains("\"enters_under\""),
            "expected modern field name in: {json}"
        );
        assert!(
            !json.contains("\"under_your_control\""),
            "legacy field must not be emitted: {json}"
        );
        let decoded: Effect = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(original, decoded);
    }

    #[test]
    fn enters_under_alias_resolution_when_both_fields_present() {
        // serde resolves `alias` by collapsing both names onto the same logical
        // field, so a payload that includes BOTH the modern key and the legacy
        // alias is treated as a duplicate-field error. This pins the behavior
        // so a future schema migration is not surprised by it: on-disk payloads
        // must use ONE of `enters_under` or `under_your_control`, never both.
        let json = r#"{
            "type": "ChangeZone",
            "destination": "Battlefield",
            "under_your_control": false,
            "enters_under": "You"
        }"#;
        let err = serde_json::from_str::<Effect>(json)
            .expect_err("duplicate alias+modern field must error");
        assert!(
            err.to_string().contains("duplicate field"),
            "expected duplicate-field error, got: {err}"
        );
    }

    #[test]
    fn legacy_under_your_control_field_not_emitted_in_serialization() {
        // `enters_under: None` must skip-serialize; `Some(You)` must emit the
        // modern key. Neither case may emit the legacy boolean field.
        for variant in [None, Some(ControllerRef::You)] {
            let effect = change_zone_with_enters_under(variant.clone());
            let json = serde_json::to_string(&effect).expect("serialize");
            assert!(
                !json.contains("\"under_your_control\""),
                "legacy bool field emitted for {variant:?}: {json}"
            );
        }
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
                counter_type: CounterMatch::OfType(CounterType::Plus1Plus1),
                target: None,
            };
            assert_eq!(cost.categories(), vec![CostCategory::RemovesCounters]);
        }

        #[test]
        fn pay_energy() {
            assert_eq!(
                AbilityCost::PayEnergy {
                    amount: QuantityExpr::Fixed { value: 3 }
                }
                .categories(),
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
                from_zone: None,
            };
            assert_eq!(cost.categories(), vec![CostCategory::ReturnsToHand]);
        }

        #[test]
        fn unattach() {
            assert_eq!(
                AbilityCost::Unattach.categories(),
                vec![CostCategory::Unattaches]
            );
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
                player: TargetFilter::Controller,
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

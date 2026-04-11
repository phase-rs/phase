use std::collections::{BTreeSet, HashMap, HashSet};
use std::sync::Arc;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use super::ability::{
    AbilityCost, AbilityDefinition, AdditionalCost, ChoiceType, ChoiceValue,
    ContinuousModification, DelayedTriggerCondition, Duration, GameRestriction, ModalChoice,
    ResolvedAbility, StaticCondition, TargetFilter, TargetRef, TriggerCondition, UnlessCost,
};
use super::card::CardFace;
use super::card_type::{CoreType, Supertype};
use super::counter::CounterType;
use super::events::GameEvent;
use super::format::FormatConfig;
use super::identifiers::{CardId, ObjectId, TrackedSetId};
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaCost, ManaType};
use super::match_config::{MatchConfig, MatchPhase, MatchScore};
use super::phase::Phase;
use super::player::{Player, PlayerId};
use super::proposed_event::{ProposedEvent, ReplacementId};
use super::zones::Zone;

use crate::game::combat::CombatState;
use crate::game::deck_loading::DeckEntry;

use crate::game::game_object::GameObject;

fn default_rng() -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(0)
}

fn default_game_number() -> u8 {
    1
}

fn default_remaining_one() -> u32 {
    1
}

/// Serde module for `HashMap<(ObjectId, usize), u32>` — JSON requires string keys,
/// so we serialize the tuple as `"objectId_index"` (e.g. `"42_0"`).
mod tuple_key_map {
    use super::*;
    use serde::de::{self, MapAccess, Visitor};
    use serde::ser::SerializeMap;
    use serde::{Deserializer, Serializer};
    use std::fmt;

    pub fn serialize<S>(
        map: &HashMap<(ObjectId, usize), u32>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut ser_map = serializer.serialize_map(Some(map.len()))?;
        for ((oid, idx), val) in map {
            ser_map.serialize_entry(&format!("{}_{}", oid.0, idx), val)?;
        }
        ser_map.end()
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<HashMap<(ObjectId, usize), u32>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct TupleKeyVisitor;

        impl<'de> Visitor<'de> for TupleKeyVisitor {
            type Value = HashMap<(ObjectId, usize), u32>;

            fn expecting(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
                formatter.write_str("a map with \"objectId_index\" string keys")
            }

            fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let mut map = HashMap::new();
                while let Some((key, val)) = access.next_entry::<String, u32>()? {
                    let (oid_str, idx_str) = key
                        .split_once('_')
                        .ok_or_else(|| de::Error::custom(format!("invalid tuple key: {key}")))?;
                    let oid = oid_str
                        .parse::<u64>()
                        .map(ObjectId)
                        .map_err(de::Error::custom)?;
                    let idx = idx_str.parse::<usize>().map_err(de::Error::custom)?;
                    map.insert((oid, idx), val);
                }
                Ok(map)
            }
        }

        deserializer.deserialize_map(TupleKeyVisitor)
    }
}

/// Tracks whether the game is in day or night state (CR 730).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DayNight {
    Day,
    Night,
}

/// CR 702.51a / Waterbend: Determines tap-to-pay behavior during mana payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ConvokeMode {
    /// CR 702.51a: Creature's color determines mana produced.
    Convoke,
    /// Waterbend: always produces {1} colorless, emits Waterbend event.
    Waterbend,
}

/// CR 400.7: Snapshot of an object's characteristics at the time it left a public zone.
/// Used for event-context resolution when the object is no longer in its original zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LKISnapshot {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub mana_value: u32,
    pub controller: PlayerId,
    pub owner: PlayerId,
    /// CR 400.7: Core types as they last existed on the battlefield.
    /// Used by `TriggerCondition::WasType` for "if it was a creature" patterns.
    #[serde(default)]
    pub card_types: Vec<CoreType>,
    /// CR 400.7: Counters as they last existed on the object.
    /// Used by `TriggerCondition::HadCounters` for "if it had counters on it" patterns.
    #[serde(default)]
    pub counters: HashMap<CounterType, u32>,
}

/// Snapshot of a spell's characteristics at cast time for per-turn history queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpellCastRecord {
    pub core_types: Vec<CoreType>,
    pub supertypes: Vec<Supertype>,
    pub subtypes: Vec<String>,
    pub keywords: Vec<Keyword>,
    pub colors: Vec<ManaColor>,
    pub mana_value: u32,
}

/// CR 601.2f: A pending one-shot cost reduction for the next spell a player casts.
/// Created by effects like "the next spell you cast this turn costs {N} less to cast."
/// Consumed (removed) when the player casts their next spell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSpellCostReduction {
    pub player: PlayerId,
    /// Generic mana reduction amount.
    pub amount: u32,
    /// Optional filter for which spells this applies to (None = any spell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spell_filter: Option<TargetFilter>,
}

/// CR 601.2f: Describes a one-shot modification applied to the next qualifying spell a player
/// casts. Created by effects like "the next spell you cast this turn has convoke" or "the next
/// creature spell you cast this turn can't be countered."
/// Consumed (removed) when the player casts their next qualifying spell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingNextSpellModifier {
    pub player: PlayerId,
    /// What modification to apply to the next spell.
    pub modifier: NextSpellModifier,
    /// Optional filter for which spells this applies to (None = any spell).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spell_filter: Option<TargetFilter>,
}

/// CR 601.2f: The kind of modification to apply to the next qualifying spell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum NextSpellModifier {
    /// "The next spell you cast this turn can't be countered."
    CantBeCountered,
    /// "The next spell you cast this turn has [keyword]."
    HasKeyword { keyword: Keyword },
    /// "The next spell you cast this turn can be cast as though it had flash."
    CastAsThoughFlash,
}

/// CR 400.7: Snapshot of an object's properties at the time of a zone change,
/// enabling data-driven filtered counting at resolution time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneChangeRecord {
    pub object_id: ObjectId,
    pub name: String,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub controller: PlayerId,
    pub owner: PlayerId,
    pub from_zone: Zone,
    pub to_zone: Zone,
}

/// CR 403.3: Snapshot of an object's properties at the time it enters the battlefield,
/// enabling data-driven ETB condition queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BattlefieldEntryRecord {
    pub object_id: ObjectId,
    pub name: String,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub controller: PlayerId,
}

/// CR 120.1: Snapshot of a damage event for "was dealt damage by" queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageRecord {
    pub source_id: ObjectId,
    pub target: TargetRef,
    pub amount: u32,
    #[serde(default)]
    pub is_combat: bool,
}

/// CR 607.2a + CR 406.6: Tracks the link between an exiling source and the exiled card.
/// When the source leaves the battlefield, the exiled card returns (CR 610.3a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExileLink {
    pub exiled_id: ObjectId,
    pub source_id: ObjectId,
    /// CR 610.3a: The zone the exiled object occupied before being exiled.
    pub return_zone: Zone,
}

/// Tracks commander damage dealt to a specific player by a specific commander.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommanderDamageEntry {
    pub player: PlayerId,
    pub commander: ObjectId,
    pub damage: u32,
}

/// CR 603.7: A delayed triggered ability created during resolution of a spell or ability.
/// Fires once at the specified condition, then is removed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DelayedTrigger {
    /// When this trigger fires.
    pub condition: DelayedTriggerCondition,
    /// The ability to execute when it fires.
    pub ability: ResolvedAbility,
    /// CR 603.7d: Controller (the player who created it).
    pub controller: PlayerId,
    /// Source permanent that created this delayed trigger.
    pub source_id: ObjectId,
    /// Whether this trigger fires once and is removed (most delayed triggers).
    /// CR 603.7c.
    pub one_shot: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCast {
    pub object_id: ObjectId,
    pub card_id: CardId,
    pub ability: ResolvedAbility,
    pub cost: ManaCost,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_cost: Option<AbilityCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_ability_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// How this spell was cast — threads through the casting pipeline to finalize_cast.
    #[serde(default)]
    pub casting_variant: CastingVariant,
    /// CR 601.2d: When set, after target selection the caster must distribute this
    /// resource (damage, counters, life) among the chosen targets via DistributeAmong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribute: Option<DistributionUnit>,
}

impl PendingCast {
    pub fn new(
        object_id: ObjectId,
        card_id: CardId,
        ability: ResolvedAbility,
        cost: ManaCost,
    ) -> Self {
        Self {
            object_id,
            card_id,
            ability,
            cost,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            distribute: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CollectEvidenceResume {
    Casting {
        pending_cast: Box<PendingCast>,
    },
    Effect {
        pending_ability: Box<ResolvedAbility>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaAbilityResume {
    Priority,
    ManaPayment {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    UnlessPayment {
        cost: UnlessCost,
        pending_effect: Box<ResolvedAbility>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_description: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingManaAbility {
    pub player: PlayerId,
    pub source_id: ObjectId,
    pub ability_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_override: Option<ManaType>,
    pub resume: ManaAbilityResume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetSelectionSlot {
    pub legal_targets: Vec<TargetRef>,
    #[serde(default)]
    pub optional: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct TargetSelectionProgress {
    #[serde(default)]
    pub current_slot: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub selected_slots: Vec<Option<TargetRef>>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_legal_targets: Vec<TargetRef>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PublicStateDirty {
    pub all_objects_dirty: bool,
    pub dirty_objects: HashSet<ObjectId>,
    pub all_players_dirty: bool,
    pub dirty_players: HashSet<PlayerId>,
    pub battlefield_display_dirty: bool,
    pub mana_display_dirty: bool,
}

impl PublicStateDirty {
    pub fn all_dirty() -> Self {
        Self {
            all_objects_dirty: true,
            dirty_objects: HashSet::new(),
            all_players_dirty: true,
            dirty_players: HashSet::new(),
            battlefield_display_dirty: true,
            mana_display_dirty: true,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TargetSelectionConstraint {
    DifferentTargetPlayers,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlayerDeckPool {
    pub player: PlayerId,
    pub registered_main: Vec<DeckEntry>,
    pub registered_sideboard: Vec<DeckEntry>,
    pub current_main: Vec<DeckEntry>,
    pub current_sideboard: Vec<DeckEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WaitingFor {
    Priority {
        player: PlayerId,
    },
    MulliganDecision {
        player: PlayerId,
        mulligan_count: u8,
    },
    MulliganBottomCards {
        player: PlayerId,
        count: u8,
    },
    ManaPayment {
        player: PlayerId,
        /// CR 702.51a / Waterbend: When present, the player can tap untapped
        /// creatures/artifacts to pay mana. Summoning sickness does not apply (CR 302.6).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    TargetSelection {
        player: PlayerId,
        pending_cast: Box<PendingCast>,
        target_slots: Vec<TargetSelectionSlot>,
        #[serde(default)]
        selection: TargetSelectionProgress,
    },
    DeclareAttackers {
        player: PlayerId,
        valid_attacker_ids: Vec<ObjectId>,
        #[serde(default)]
        valid_attack_targets: Vec<crate::game::combat::AttackTarget>,
    },
    DeclareBlockers {
        player: PlayerId,
        valid_blocker_ids: Vec<ObjectId>,
        #[serde(default)]
        valid_block_targets: HashMap<ObjectId, Vec<ObjectId>>,
    },
    GameOver {
        winner: Option<PlayerId>,
    },
    ReplacementChoice {
        player: PlayerId,
        candidate_count: usize,
        #[serde(default)]
        candidate_descriptions: Vec<String>,
    },
    /// CR 707.9: Player chooses a permanent to copy as part of an "enter as a copy of"
    /// replacement effect. This is a choice, not targeting (hexproof/shroud don't apply).
    CopyTargetChoice {
        player: PlayerId,
        /// The permanent that just entered the battlefield (the clone).
        source_id: ObjectId,
        /// Legal permanents on the battlefield that can be copied.
        valid_targets: Vec<ObjectId>,
    },
    /// CR 701.44d: Player chooses which of their remaining permanents explores next.
    ExploreChoice {
        player: PlayerId,
        source_id: ObjectId,
        choosable: Vec<ObjectId>,
        remaining: Vec<ObjectId>,
        pending_effect: Box<ResolvedAbility>,
    },
    EquipTarget {
        player: PlayerId,
        equipment_id: ObjectId,
        valid_targets: Vec<ObjectId>,
    },
    /// CR 702.122a: Player must tap creatures with total power >= crew_power.
    CrewVehicle {
        player: PlayerId,
        vehicle_id: ObjectId,
        /// The crew N value from the keyword.
        crew_power: u32,
        /// Untapped creatures the player controls (excluding the Vehicle itself).
        eligible_creatures: Vec<ObjectId>,
    },
    ScryChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    /// CR 701.20e: Waiting for the player to choose which looked-at cards to keep.
    DigChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        keep_count: usize,
        /// True = select 0..=keep_count ("up to N"), false = exactly keep_count.
        #[serde(default)]
        up_to: bool,
        /// Cards that pass the filter — frontend greys out others.
        #[serde(default)]
        selectable_cards: Vec<ObjectId>,
        /// Where kept cards go (None = Hand).
        #[serde(default)]
        kept_destination: Option<Zone>,
        /// Where unchosen cards go (None = Graveyard, Some(Library) = bottom).
        #[serde(default)]
        rest_destination: Option<Zone>,
        /// Source ability's object ID for filter context.
        #[serde(default)]
        source_id: Option<ObjectId>,
    },
    SurveilChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    RevealChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        #[serde(default = "super::ability::default_target_filter_any")]
        filter: TargetFilter,
    },
    /// Player is choosing card(s) from a filtered library search.
    SearchChoice {
        player: PlayerId,
        /// Object IDs of legal choices (pre-filtered from library).
        cards: Vec<ObjectId>,
        /// How many cards to select.
        count: usize,
        /// Whether the chosen cards should be revealed before the continuation resolves.
        #[serde(default)]
        reveal: bool,
    },
    /// CR 700.2: Player selects card(s) from a tracked set (e.g., exiled cards).
    /// Chosen/unchosen cards flow into sub-abilities via pending_continuation,
    /// unlike DigChoice which moves to fixed zones.
    ChooseFromZoneChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        count: usize,
        source_id: ObjectId,
    },
    /// CR 701.50a: Player chooses card(s) to discard for connive.
    /// After discarding, nonland discards add +1/+1 counters to the conniving creature.
    ConniveDiscard {
        player: PlayerId,
        conniver_id: ObjectId,
        source_id: ObjectId,
        cards: Vec<ObjectId>,
        count: usize,
    },
    /// CR 701.9b: Player chooses card(s) to discard during effect resolution.
    /// Used when an effect says "discard a card" without "at random."
    DiscardChoice {
        player: PlayerId,
        count: usize,
        cards: Vec<ObjectId>,
        source_id: ObjectId,
        effect_kind: crate::types::ability::EffectKind,
        /// CR 701.9b: When true, the player may discard 0..=count cards ("discard up to N").
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        /// CR 608.2c: "discard N unless you discard a [type]" — when set,
        /// the player may discard 1 card matching this filter instead of `count`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unless_filter: Option<crate::types::ability::TargetFilter>,
    },
    /// CR 608.2d: Player chooses object(s) from a zone during effect resolution.
    /// Generalizes the DiscardChoice pattern to sacrifice-from-battlefield and hand-to-battlefield.
    EffectZoneChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        count: usize,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        source_id: ObjectId,
        effect_kind: crate::types::ability::EffectKind,
        /// Source zone of eligible objects (Battlefield for sacrifice, Hand for put-onto-BF).
        zone: Zone,
        /// Destination zone for ChangeZone effects. None for Sacrifice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<Zone>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter_tapped: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter_transformed: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        under_your_control: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enters_attacking: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        owner_library: bool,
    },
    /// CR 701.48a: Learn — player chooses to rummage (discard→draw) or skip.
    /// `hand_cards` lists cards eligible for discard.
    LearnChoice {
        player: PlayerId,
        hand_cards: Vec<ObjectId>,
    },
    /// CR 701.62a: Player chooses one of the top 2 revealed cards to manifest face-down.
    /// The unchosen card goes to graveyard. Cards are visible only to the manifesting player.
    ManifestDreadChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    TriggerTargetSelection {
        player: PlayerId,
        target_slots: Vec<TargetSelectionSlot>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        target_constraints: Vec<TargetSelectionConstraint>,
        #[serde(default)]
        selection: TargetSelectionProgress,
        /// Source permanent that owns this trigger (for UI context).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
        /// Human-readable description of the trigger (from Oracle text).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    BetweenGamesSideboard {
        player: PlayerId,
        game_number: u8,
        score: MatchScore,
    },
    BetweenGamesChoosePlayDraw {
        player: PlayerId,
        game_number: u8,
        score: MatchScore,
    },
    /// Player must choose from a named set of options (creature type, color, etc.).
    NamedChoice {
        player: PlayerId,
        choice_type: ChoiceType,
        options: Vec<String>,
        /// The object that originated this choice (for persisting to chosen_attributes).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
    },
    /// Player must choose modes for a modal spell (e.g. "Choose one —").
    ModeChoice {
        player: PlayerId,
        modal: ModalChoice,
        pending_cast: Box<PendingCast>,
    },
    /// Player must choose which cards to discard down to maximum hand size (cleanup step).
    DiscardToHandSize {
        player: PlayerId,
        /// How many cards must be discarded.
        count: usize,
        /// The ObjectIds of all cards in the player's hand (the chooseable set).
        cards: Vec<ObjectId>,
    },
    /// Player must decide on an additional casting cost (e.g. kicker, blight, "or pay").
    OptionalCostChoice {
        player: PlayerId,
        cost: AdditionalCost,
        pending_cast: Box<PendingCast>,
    },
    /// CR 601.2b: Defiler cycle — player may pay life to reduce mana cost of a colored
    /// permanent spell. Presented when a controlled Defiler matches the spell's color.
    DefilerPayment {
        player: PlayerId,
        /// Life cost if accepted (e.g. 2)
        life_cost: u32,
        /// Mana cost reduction if life is paid (e.g. {G})
        mana_reduction: ManaCost,
        pending_cast: Box<PendingCast>,
    },
    /// CR 715.3a: Player chooses creature face vs Adventure half when casting
    /// an Adventure card from hand (or exile with permission).
    AdventureCastChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
    },
    /// CR 712.12: Player chooses which face of an MDFC to play as a land
    /// when both faces have the Land type.
    ModalFaceChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
    },
    /// Player chooses between normal cast and Warp cast from hand.
    /// Warp is a custom keyword: cast for warp cost, exile at next end step,
    /// then may cast from exile later. Only presented when both costs are affordable.
    WarpCostChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
        /// The card's normal mana cost (for display in the choice modal).
        normal_cost: ManaCost,
        /// The Warp keyword's alternative mana cost (for display in the choice modal).
        warp_cost: ManaCost,
    },
    /// CR 601.2c: Player chooses any number of legal targets from a set.
    /// Used for "exile any number of" and similar variable-count targeting.
    MultiTargetSelection {
        player: PlayerId,
        legal_targets: Vec<ObjectId>,
        min_targets: usize,
        max_targets: usize,
        /// The pending ability to execute with selected targets injected.
        pending_ability: Box<ResolvedAbility>,
    },
    /// Player must choose modes for a modal activated or triggered ability.
    /// Unlike ModeChoice (which is casting-specific via PendingCast), this variant
    /// is decoupled from PendingCast and carries the mode ability definitions directly.
    AbilityModeChoice {
        player: PlayerId,
        modal: ModalChoice,
        /// The source object that owns this ability.
        source_id: ObjectId,
        /// The individual mode abilities the player can choose from.
        mode_abilities: Vec<AbilityDefinition>,
        /// Whether this is an activated ability (needs stack push) or triggered
        /// (already on stack, needs effect replacement).
        #[serde(default)]
        is_activated: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ability_index: Option<usize>,
        /// For activated abilities: the cost to pay after mode selection.
        /// CR 602.2a: Announce → choose modes → choose targets → pay costs.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ability_cost: Option<AbilityCost>,
        /// Mode indices unavailable due to NoRepeatThisTurn/NoRepeatThisGame constraints.
        /// CR 700.2: Engine computes which modes have been previously chosen; frontend uses this to disable them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unavailable_modes: Vec<usize>,
    },
    /// CR 608.2d: Player must choose whether to perform an optional effect ("You may X").
    OptionalEffectChoice {
        player: PlayerId,
        source_id: ObjectId,
        /// Human-readable description of the effect (e.g. "draw a card").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
    /// CR 608.2d + CR 101.4: An opponent may choose to perform an optional effect.
    /// Prompts opponents in APNAP order. First accept wins; remaining are not prompted.
    OpponentMayChoice {
        player: PlayerId,
        source_id: ObjectId,
        /// Human-readable description of the effect.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Opponents still to prompt after current `player` (APNAP order).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining: Vec<PlayerId>,
    },
    /// CR 118.12: Opponent must decide whether to pay a cost to prevent an effect.
    /// Used by "counter unless pays {X}" (Mana Leak), tax triggers (Esper Sentinel),
    /// and ward costs (CR 702.21a).
    UnlessPayment {
        player: PlayerId,
        cost: UnlessCost,
        /// The effect to execute if the player declines to pay.
        pending_effect: Box<ResolvedAbility>,
        /// Human-readable description for the frontend (e.g., "counter target spell", "draw a card").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_description: Option<String>,
    },
    /// CR 702.21a: Player must choose a card to discard as ward cost payment.
    WardDiscardChoice {
        player: PlayerId,
        /// Eligible cards in hand.
        cards: Vec<ObjectId>,
        /// The counter effect to prevent if the discard succeeds.
        pending_effect: Box<ResolvedAbility>,
    },
    /// CR 702.21a: Player must choose a permanent to sacrifice as ward cost payment.
    WardSacrificeChoice {
        player: PlayerId,
        /// Eligible permanents on the battlefield.
        permanents: Vec<ObjectId>,
        /// The counter effect to prevent if the sacrifice succeeds.
        pending_effect: Box<ResolvedAbility>,
        /// Number of permanents remaining to sacrifice (for "sacrifice two permanents" etc.)
        #[serde(default = "default_remaining_one")]
        remaining: u32,
    },
    /// CR 701.54: Player must choose which creature becomes their ring-bearer.
    ChooseRingBearer {
        player: PlayerId,
        candidates: Vec<ObjectId>,
    },
    /// CR 701.49a: Player chooses which dungeon to venture into (no active dungeon).
    ChooseDungeon {
        player: PlayerId,
        options: Vec<crate::game::dungeon::DungeonId>,
    },
    /// CR 309.5a: Player at a branching room chooses which room to advance to.
    ChooseDungeonRoom {
        player: PlayerId,
        dungeon: crate::game::dungeon::DungeonId,
        options: Vec<u8>,
        option_names: Vec<String>,
    },
    /// CR 601.2b: Player must choose a card to discard as part of an additional casting cost.
    /// After selection, the card is discarded and casting continues via `pay_and_push`.
    DiscardForCost {
        player: PlayerId,
        /// How many cards to discard.
        count: usize,
        /// Eligible cards in hand (excludes the spell being cast).
        cards: Vec<ObjectId>,
        /// The pending cast to resume after the discard is complete.
        pending_cast: Box<PendingCast>,
    },
    /// CR 118.3 / CR 601.2b: Player must choose permanent(s) to sacrifice as cost.
    SacrificeForCost {
        player: PlayerId,
        /// How many permanents to sacrifice (usually 1; covers "sacrifice two creatures").
        count: usize,
        /// Pre-filtered eligible permanents on the battlefield.
        permanents: Vec<ObjectId>,
        /// The pending cast to resume after the sacrifice is complete.
        pending_cast: Box<PendingCast>,
    },
    /// CR 702.34a / CR 601.2b: Player must choose untapped creatures to tap as a spell cost
    /// (e.g., "Flashback—Tap three untapped white creatures you control").
    TapCreaturesForSpellCost {
        player: PlayerId,
        count: usize,
        creatures: Vec<ObjectId>,
        pending_cast: Box<PendingCast>,
    },
    /// CR 118.3 / CR 605.3b: Player must choose untapped creatures to pay a mana ability cost.
    TapCreaturesForManaAbility {
        player: PlayerId,
        count: usize,
        creatures: Vec<ObjectId>,
        pending_mana_ability: Box<PendingManaAbility>,
    },
    /// CR 702.138a: Player must choose cards to exile from graveyard as escape cost.
    ExileFromGraveyardForCost {
        player: PlayerId,
        /// How many cards to exile.
        count: usize,
        /// Eligible graveyard cards — excludes the escape card itself.
        cards: Vec<ObjectId>,
        /// The pending cast to resume after the exile is complete.
        pending_cast: Box<PendingCast>,
    },
    /// CR 701.59a / CR 702.163a: Choose graveyard cards with combined mana value
    /// at least the required threshold, then resume casting or effect resolution.
    CollectEvidenceChoice {
        player: PlayerId,
        minimum_mana_value: u32,
        cards: Vec<ObjectId>,
        resume: Box<CollectEvidenceResume>,
    },
    /// CR 702.180a: Harmonize allows tapping up to one untapped creature to reduce cost by its power.
    /// CR 702.180b: Creature chosen as you choose to pay the harmonize cost (CR 601.2b).
    /// CR 302.6: Summoning sickness does not restrict tapping for costs (only {T} abilities).
    HarmonizeTapChoice {
        player: PlayerId,
        /// Untapped creatures the player controls with power > 0.
        eligible_creatures: Vec<ObjectId>,
        /// The pending cast to resume after the tap choice.
        pending_cast: Box<PendingCast>,
    },
    /// CR 701.57a: Player chooses to cast the discovered card or put it to hand.
    DiscoverChoice {
        player: PlayerId,
        /// The nonland card that was hit.
        hit_card: ObjectId,
        /// Cards exiled as misses (go to bottom in random order).
        exiled_misses: Vec<ObjectId>,
    },
    /// CR 401.4: Owner chooses to put a permanent on top or bottom of their library.
    TopOrBottomChoice {
        player: PlayerId,
        object_id: ObjectId,
    },
    /// CR 701.36a: Choose a creature token you control to create a copy of.
    PopulateChoice {
        player: PlayerId,
        source_id: ObjectId,
        valid_tokens: Vec<ObjectId>,
    },
    /// CR 701.30c: After a clash, each player puts their revealed card on top or
    /// bottom of their library. Choices are made in APNAP order. `remaining` holds
    /// the next player/card pairs still awaiting a choice.
    ClashCardPlacement {
        player: PlayerId,
        card: ObjectId,
        remaining: Vec<(PlayerId, ObjectId)>,
    },
    /// CR 702.139a: Before the game begins, reveal companion from outside the game.
    CompanionReveal {
        player: PlayerId,
        /// Eligible companion cards from sideboard: (card_name, sideboard_index).
        eligible_companions: Vec<(String, usize)>,
    },
    /// CR 704.5j: Player chooses which legendary permanent to keep.
    /// The rest are put into their owners' graveyards (not destroyed — indestructible does not apply).
    ChooseLegend {
        player: PlayerId,
        legend_name: String,
        candidates: Vec<ObjectId>,
    },
    /// CR 701.34a: Player chooses any number of permanents and/or players that have
    /// counters on them, then adds one counter of each kind already there.
    ProliferateChoice {
        player: PlayerId,
        /// Eligible permanents (with counters) and players (with poison/energy).
        eligible: Vec<TargetRef>,
    },
    /// CR 101.4 + CR 701.21a: Player selects one permanent per type category
    /// from among those they (or another player) control, then the rest are sacrificed.
    /// Used by Cataclysm, Tragic Arrogance, Cataclysmic Gearhulk.
    CategoryChoice {
        player: PlayerId,
        /// Whose permanents are being chosen from (may differ from `player` for Tragic Arrogance).
        target_player: PlayerId,
        /// Type categories to fill (e.g., [Artifact, Creature, Enchantment, Land]).
        categories: Vec<CoreType>,
        /// For each category, the eligible permanent IDs (battlefield objects matching that type).
        eligible_per_category: Vec<Vec<ObjectId>>,
        source_id: ObjectId,
        /// Players still to choose after the current one (APNAP order).
        remaining_players: Vec<PlayerId>,
        /// Permanents chosen by previous players — protected from sacrifice.
        all_kept: Vec<ObjectId>,
    },
    /// CR 707.10c: When a spell is copied, the controller may choose new targets.
    /// Each slot shows the current target and legal alternatives.
    CopyRetarget {
        player: PlayerId,
        copy_id: ObjectId,
        target_slots: Vec<CopyTargetSlot>,
    },
    /// CR 510.1c: Attacker with multiple blockers — controller divides damage as they choose.
    /// CR 702.19b/c: Trample requires lethal to each blocker before excess to defending player.
    AssignCombatDamage {
        player: PlayerId,
        attacker_id: ObjectId,
        total_damage: u32,
        blockers: Vec<DamageSlot>,
        /// Available combat-damage assignment modes for this attacker.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        assignment_modes: Vec<CombatDamageAssignmentMode>,
        /// CR 702.19: Which trample variant applies (None = no trample).
        trample: Option<crate::game::combat::TrampleKind>,
        defending_player: PlayerId,
        #[serde(default = "crate::game::combat::default_attack_target")]
        attack_target: crate::game::combat::AttackTarget,
        /// CR 702.19c: PW loyalty threshold for trample-over-PW spillover.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pw_loyalty: Option<u32>,
        /// CR 702.19c: PW controller as additional damage target.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pw_controller: Option<PlayerId>,
    },
    /// CR 601.2d: Distribute N among targets at casting time ("divide N damage among").
    /// Infrastructure ready: handler in engine.rs, AI candidates, continuation match.
    /// TODO: Wire trigger in casting.rs when a "divide/distribute" ability is being cast.
    /// Requires parser support for "divide N damage among" Oracle text patterns.
    DistributeAmong {
        player: PlayerId,
        total: u32,
        targets: Vec<TargetRef>,
        unit: DistributionUnit,
    },
    /// CR 115.7: Change the target(s) of a spell or ability on the stack.
    /// Infrastructure ready: handler in engine.rs, AI candidates, continuation match.
    /// TODO: Add Effect::ChangeTargets variant + resolver in effects/change_targets.rs.
    /// Requires parser support for "change the target of" Oracle text patterns.
    RetargetChoice {
        player: PlayerId,
        stack_entry_index: usize,
        scope: RetargetScope,
        current_targets: Vec<TargetRef>,
        legal_new_targets: Vec<TargetRef>,
    },
}

/// CR 707.10c: A target slot on a copied spell, showing current target and alternatives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyTargetSlot {
    pub current: TargetRef,
    pub legal_alternatives: Vec<TargetRef>,
}

/// CR 510.1c: Optional combat-damage assignment mode for attackers with text like
/// "you may have this creature assign its combat damage as though it weren't blocked."
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum CombatDamageAssignmentMode {
    #[default]
    Normal,
    AsThoughUnblocked,
}

/// CR 510.1c: A blocker with its lethal damage threshold for UI display.
/// `lethal_minimum` is only enforced as a hard constraint for trample (CR 702.19b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageSlot {
    pub blocker_id: ObjectId,
    /// Lethal damage threshold. CR 702.2c: With deathtouch, lethal = 1.
    /// Informational for non-trample; enforced for trample (CR 702.19b).
    pub lethal_minimum: u32,
}

/// CR 601.2d: What is being distributed (damage, counters, life).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum DistributionUnit {
    Damage,
    /// CR 601.2d: Even split — engine auto-computes `total / num_targets` (rounded down).
    /// No player choice needed; bypasses `WaitingFor::DistributeAmong`.
    EvenSplitDamage,
    Counters(String),
    Life,
}

/// CR 115.7: Scope of retargeting — single target, all targets, or forced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum RetargetScope {
    Single,
    All,
    ForcedTo(TargetRef),
}

impl WaitingFor {
    /// Extract the player who must act, if any.
    pub fn acting_player(&self) -> Option<PlayerId> {
        match self {
            WaitingFor::Priority { player }
            | WaitingFor::MulliganDecision { player, .. }
            | WaitingFor::MulliganBottomCards { player, .. }
            | WaitingFor::ManaPayment { player, .. }
            | WaitingFor::TargetSelection { player, .. }
            | WaitingFor::DeclareAttackers { player, .. }
            | WaitingFor::DeclareBlockers { player, .. }
            | WaitingFor::ReplacementChoice { player, .. }
            | WaitingFor::CopyTargetChoice { player, .. }
            | WaitingFor::ExploreChoice { player, .. }
            | WaitingFor::EquipTarget { player, .. }
            | WaitingFor::CrewVehicle { player, .. }
            | WaitingFor::ScryChoice { player, .. }
            | WaitingFor::DigChoice { player, .. }
            | WaitingFor::SurveilChoice { player, .. }
            | WaitingFor::RevealChoice { player, .. }
            | WaitingFor::SearchChoice { player, .. }
            | WaitingFor::ChooseFromZoneChoice { player, .. }
            | WaitingFor::LearnChoice { player, .. }
            | WaitingFor::ManifestDreadChoice { player, .. }
            | WaitingFor::EffectZoneChoice { player, .. }
            | WaitingFor::TriggerTargetSelection { player, .. }
            | WaitingFor::BetweenGamesSideboard { player, .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { player, .. }
            | WaitingFor::NamedChoice { player, .. }
            | WaitingFor::ModeChoice { player, .. }
            | WaitingFor::DiscardToHandSize { player, .. }
            | WaitingFor::OptionalCostChoice { player, .. }
            | WaitingFor::DefilerPayment { player, .. }
            | WaitingFor::AbilityModeChoice { player, .. }
            | WaitingFor::MultiTargetSelection { player, .. }
            | WaitingFor::AdventureCastChoice { player, .. }
            | WaitingFor::ModalFaceChoice { player, .. }
            | WaitingFor::WarpCostChoice { player, .. }
            | WaitingFor::ChooseRingBearer { player, .. }
            | WaitingFor::ChooseDungeon { player, .. }
            | WaitingFor::ChooseDungeonRoom { player, .. }
            | WaitingFor::DiscardForCost { player, .. }
            | WaitingFor::SacrificeForCost { player, .. }
            | WaitingFor::TapCreaturesForSpellCost { player, .. }
            | WaitingFor::TapCreaturesForManaAbility { player, .. }
            | WaitingFor::ExileFromGraveyardForCost { player, .. }
            | WaitingFor::CollectEvidenceChoice { player, .. }
            | WaitingFor::HarmonizeTapChoice { player, .. }
            | WaitingFor::OptionalEffectChoice { player, .. }
            | WaitingFor::OpponentMayChoice { player, .. }
            | WaitingFor::UnlessPayment { player, .. }
            | WaitingFor::DiscoverChoice { player, .. }
            | WaitingFor::TopOrBottomChoice { player, .. }
            | WaitingFor::PopulateChoice { player, .. }
            | WaitingFor::ClashCardPlacement { player, .. }
            | WaitingFor::CompanionReveal { player, .. }
            | WaitingFor::ChooseLegend { player, .. }
            | WaitingFor::ProliferateChoice { player, .. }
            | WaitingFor::CategoryChoice { player, .. }
            | WaitingFor::CopyRetarget { player, .. }
            | WaitingFor::AssignCombatDamage { player, .. }
            | WaitingFor::DistributeAmong { player, .. }
            | WaitingFor::RetargetChoice { player, .. }
            | WaitingFor::WardDiscardChoice { player, .. }
            | WaitingFor::WardSacrificeChoice { player, .. }
            | WaitingFor::ConniveDiscard { player, .. }
            | WaitingFor::DiscardChoice { player, .. } => Some(*player),
            WaitingFor::GameOver { .. } => None,
        }
    }

    /// Whether this state is part of the casting flow and can be backed out of
    /// with `CancelCast`. This is true for every state that carries a `pending_cast`.
    pub fn has_pending_cast(&self) -> bool {
        match self {
            WaitingFor::ManaPayment { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::ModeChoice { .. }
            | WaitingFor::OptionalCostChoice { .. }
            | WaitingFor::DefilerPayment { .. }
            | WaitingFor::DiscardForCost { .. }
            | WaitingFor::SacrificeForCost { .. }
            | WaitingFor::TapCreaturesForSpellCost { .. }
            | WaitingFor::TapCreaturesForManaAbility { .. }
            | WaitingFor::ExileFromGraveyardForCost { .. }
            | WaitingFor::HarmonizeTapChoice { .. } => true,
            WaitingFor::CollectEvidenceChoice { resume, .. } => {
                matches!(resume.as_ref(), CollectEvidenceResume::Casting { .. })
            }
            _ => false,
        }
    }
}

/// What the frontend requests for auto-pass (no internal state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoPassRequest {
    UntilStackEmpty,
    UntilEndOfTurn,
}

/// What the engine stores for auto-pass (includes captured state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoPassMode {
    /// Auto-pass while stack is non-empty. Clears when stack empties or grows
    /// beyond `initial_stack_len` (the stack size when the flag was set).
    UntilStackEmpty { initial_stack_len: usize },
    /// Auto-pass through all priority/combat stops until the flagged player's turn starts.
    UntilEndOfTurn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ActionResult {
    pub events: Vec<GameEvent>,
    pub waiting_for: WaitingFor,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub log_entries: Vec<super::log::GameLogEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackEntry {
    pub id: ObjectId,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub kind: StackEntryKind,
}

impl StackEntry {
    /// Access the resolved ability for this stack entry (immutable).
    /// Returns `None` for permanent spells with no spell-level effect.
    pub fn ability(&self) -> Option<&ResolvedAbility> {
        match &self.kind {
            StackEntryKind::Spell { ability, .. } => ability.as_ref(),
            StackEntryKind::ActivatedAbility { ability, .. }
            | StackEntryKind::TriggeredAbility { ability, .. } => Some(ability),
        }
    }

    /// Access the resolved ability for this stack entry (mutable).
    /// Returns `None` for permanent spells with no spell-level effect.
    pub fn ability_mut(&mut self) -> Option<&mut ResolvedAbility> {
        match &mut self.kind {
            StackEntryKind::Spell { ability, .. } => ability.as_mut(),
            StackEntryKind::ActivatedAbility { ability, .. }
            | StackEntryKind::TriggeredAbility { ability, .. } => Some(ability),
        }
    }
}

/// How a spell was cast — determines zone routing and post-resolution behavior.
/// Replaces individual boolean flags (cast_as_adventure, cast_as_warp) with a
/// single enum that captures the casting context.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum CastingVariant {
    /// Normal spell cast — no special resolution behavior.
    #[default]
    Normal,
    /// CR 715.4: Cast as the Adventure half. On resolution, exiled with
    /// AdventureCreature permission and creature face restored.
    Adventure,
    /// CR 702.185a: Cast via Warp alternative cost from hand. On resolution,
    /// creates a delayed trigger to exile at end step with WarpExile permission.
    Warp,
    /// CR 702.138: Cast from graveyard via Escape. On resolution, goes to
    /// appropriate zone normally (unlike Flashback which exiles).
    Escape,
    /// CR 702.180a: Cast from graveyard for harmonize cost. On resolution, exiled
    /// instead of going anywhere else (unlike Escape which returns to graveyard).
    Harmonize,
    /// CR 702.34a: Cast from graveyard for flashback cost. On resolution (or
    /// whenever leaving the stack for any reason), exiled instead of going anywhere else.
    Flashback,
    /// CR 601.2a: Cast from graveyard via a static permission source (e.g. Lurrus).
    /// Stores the granting permanent's ObjectId for once-per-turn tracking.
    /// CR 400.7: Zone change creates new ObjectId, naturally resetting permission.
    GraveyardPermission {
        source: ObjectId,
        /// When true, casting consumes this source's once-per-turn permission.
        once_per_turn: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum StackEntryKind {
    Spell {
        card_id: CardId,
        /// The spell's on-resolution ability. `None` for permanent spells with no
        /// spell-level effect (creatures, artifacts, etc.) — they simply enter the
        /// battlefield on resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ability: Option<ResolvedAbility>,
        /// How this spell was cast — determines resolution behavior (zone routing,
        /// exile permissions, delayed triggers).
        #[serde(default)]
        casting_variant: CastingVariant,
    },
    ActivatedAbility {
        source_id: ObjectId,
        ability: ResolvedAbility,
    },
    TriggeredAbility {
        source_id: ObjectId,
        ability: ResolvedAbility,
        #[serde(default)]
        condition: Option<TriggerCondition>,
        /// CR 603.7c: The event that caused this trigger, for event-context resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
        /// Human-readable trigger description from the Oracle text.
        /// Used by the frontend to distinguish triggers from the same source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GameState {
    pub turn_number: u32,
    pub active_player: PlayerId,
    pub phase: Phase,
    pub players: Vec<Player>,
    pub priority_player: PlayerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_decision_controller: Option<PlayerId>,

    // Central object store
    pub objects: HashMap<ObjectId, GameObject>,
    pub next_object_id: u64,

    // Shared zones
    pub battlefield: Vec<ObjectId>,
    pub stack: Vec<StackEntry>,
    pub exile: Vec<ObjectId>,

    /// Objects in the command zone (commanders, emblems).
    #[serde(default)]
    pub command_zone: Vec<ObjectId>,

    // RNG
    pub rng_seed: u64,
    #[serde(skip, default = "default_rng")]
    pub rng: ChaCha20Rng,

    // Combat
    pub combat: Option<CombatState>,

    // Game flow
    pub waiting_for: WaitingFor,
    /// Derived: true when waiting_for is part of the casting flow and can be
    /// backed out with CancelCast. Computed during derive_display_state so the
    /// frontend doesn't need to maintain a parallel list of casting states.
    #[serde(skip_deserializing, default)]
    pub has_pending_cast: bool,
    pub lands_played_this_turn: u8,
    pub max_lands_per_turn: u8,
    pub priority_pass_count: u8,

    // Replacement effects
    pub pending_replacement: Option<PendingReplacement>,
    /// Transient: effect to resolve after a replacement choice's zone change completes.
    /// Set by `continue_replacement` for Optional replacements, consumed by the caller.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_effect: Option<Box<crate::types::ability::AbilityDefinition>>,

    /// Transient: post-resolution context for a permanent spell whose ETB replacement
    /// needs a player choice (NeedsChoice). Consumed by `handle_replacement_choice`
    /// after the zone change completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_spell_resolution: Option<PendingSpellResolution>,

    // Layer system
    pub layers_dirty: bool,
    pub next_timestamp: u64,
    #[serde(skip, default = "PublicStateDirty::all_dirty")]
    pub public_state_dirty: PublicStateDirty,
    #[serde(skip, default)]
    pub state_revision: u64,

    // Runtime continuous effects (from resolved spells/abilities, not printed card text)
    #[serde(default)]
    pub transient_continuous_effects: Vec<TransientContinuousEffect>,
    #[serde(default)]
    pub next_continuous_effect_id: u64,

    // Day/night tracking
    #[serde(default)]
    pub day_night: Option<DayNight>,
    #[serde(default)]
    pub spells_cast_this_turn: u8,
    /// CR 603.4: Snapshot of `spells_cast_this_turn` from the previous turn.
    /// Used by werewolf "if no/two or more spells were cast last turn" conditions.
    #[serde(default)]
    pub spells_cast_last_turn: Option<u8>,

    /// Objects whose casting/activation was cancelled this priority window.
    /// Prevents the AI from looping cast→cancel→recast on the same spell or ability.
    /// Cleared on PassPriority or PlayLand.
    #[serde(default)]
    pub cancelled_casts: Vec<ObjectId>,

    // Triggered ability targeting
    #[serde(default)]
    pub pending_trigger: Option<crate::game::triggers::PendingTrigger>,

    // CR 607.2a + CR 406.5: Exile tracking for "until leaves" linked abilities.
    #[serde(default)]
    pub exile_links: Vec<ExileLink>,

    /// CR 603.7: Delayed triggered abilities waiting to fire.
    #[serde(default)]
    pub delayed_triggers: Vec<DelayedTrigger>,

    /// CR 603.7: Object sets tracked for delayed triggers ("those cards", "that creature").
    #[serde(default)]
    pub tracked_object_sets: HashMap<TrackedSetId, Vec<ObjectId>>,

    #[serde(default)]
    pub next_tracked_set_id: u64,

    // Commander support
    #[serde(default)]
    pub commander_cast_count: HashMap<ObjectId, u32>,

    /// CR 500.7: Extra turns granted by effects, stored as a LIFO stack.
    /// Most recently created extra turn is taken first (pop from end).
    #[serde(default)]
    pub extra_turns: Vec<PlayerId>,

    /// CR 614.10: Per-player count of turns to skip. When a player would begin their
    /// turn with a non-zero counter, the turn is skipped and the counter is decremented.
    #[serde(default)]
    pub turns_to_skip: Vec<u32>,
    #[serde(default)]
    pub scheduled_turn_controls: Vec<ScheduledTurnControl>,

    /// CR 500.8: Extra phases granted by effects, stored as a LIFO stack.
    /// Most recently created phase occurs first (pop from end).
    /// Consumed by `advance_phase()` — popped when transitioning between phases.
    #[serde(default)]
    pub extra_phases: Vec<Phase>,

    // N-player support
    #[serde(default)]
    pub seat_order: Vec<PlayerId>,
    #[serde(default = "FormatConfig::standard")]
    pub format_config: FormatConfig,
    #[serde(default)]
    pub eliminated_players: Vec<PlayerId>,
    #[serde(default)]
    pub commander_damage: Vec<CommanderDamageEntry>,
    #[serde(default)]
    pub priority_passes: BTreeSet<PlayerId>,
    /// Per-player auto-pass flags. When set, the engine auto-passes for this player.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub auto_pass: HashMap<PlayerId, AutoPassMode>,

    /// CR 605.3: Lands manually tapped for mana via TapLandForMana this priority window.
    /// Per-player map enables multiplayer correctness (e.g., UnlessPayment opponent tapping).
    /// Cleared on priority pass, cast, non-mana action, or phase transition.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lands_tapped_for_mana: HashMap<PlayerId, Vec<ObjectId>>,

    #[serde(default)]
    pub match_config: MatchConfig,
    #[serde(default)]
    pub match_phase: MatchPhase,
    #[serde(default)]
    pub match_score: MatchScore,
    #[serde(default = "default_game_number")]
    pub game_number: u8,
    #[serde(default)]
    pub current_starting_player: PlayerId,
    #[serde(default)]
    pub next_game_chooser: Option<PlayerId>,
    #[serde(default)]
    pub deck_pools: Vec<PlayerDeckPool>,
    #[serde(default)]
    pub sideboard_submitted: Vec<PlayerId>,

    // Trigger constraint tracking: (object_id, trigger_index) pairs that have fired
    #[serde(default)]
    pub triggers_fired_this_turn: HashSet<(ObjectId, usize)>,
    /// CR 603.4: Per-trigger fire counts for MaxTimesPerTurn constraint.
    /// Tracks how many times each (object_id, trigger_index) has fired this turn.
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "tuple_key_map"
    )]
    pub trigger_fire_counts_this_turn: HashMap<(ObjectId, usize), u32>,
    #[serde(default)]
    pub triggers_fired_this_game: HashSet<(ObjectId, usize)>,
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "tuple_key_map"
    )]
    pub activated_abilities_this_turn: HashMap<(ObjectId, usize), u32>,
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "tuple_key_map"
    )]
    pub activated_abilities_this_game: HashMap<(ObjectId, usize), u32>,
    /// CR 601.2a: Tracks which graveyard-cast permission sources have been
    /// used this turn. Keyed by the granting permanent's ObjectId.
    /// CR 400.7: Zone change creates new ObjectId, naturally resetting.
    #[serde(default)]
    pub graveyard_cast_permissions_used: HashSet<ObjectId>,
    #[serde(default)]
    pub spells_cast_this_game: HashMap<PlayerId, u32>,
    /// Per-player spell cast history this turn.
    /// Each entry records the spell's relevant characteristics at cast time,
    /// enabling data-driven filtered counting at resolution.
    #[serde(default)]
    pub spells_cast_this_turn_by_player: HashMap<PlayerId, Vec<SpellCastRecord>>,
    #[serde(default)]
    pub players_who_searched_library_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_attacked_this_step: HashSet<PlayerId>,
    #[serde(default)]
    pub players_attacked_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub attacking_creatures_this_turn: HashMap<PlayerId, u32>,
    /// CR 508.1a: Object IDs of creatures declared as attackers this turn.
    /// Persists after combat ends for post-combat filtering.
    #[serde(default)]
    pub creatures_attacked_this_turn: HashSet<ObjectId>,
    /// CR 509.1a: Object IDs of creatures declared as blockers this turn.
    /// Persists after combat ends for post-combat filtering.
    #[serde(default)]
    pub creatures_blocked_this_turn: HashSet<ObjectId>,
    #[serde(default)]
    pub players_who_created_token_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_added_counter_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_discarded_card_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub players_who_sacrificed_artifact_this_turn: HashSet<PlayerId>,
    /// CR 400.7: Zone-change snapshots this turn, enabling data-driven condition queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_changes_this_turn: Vec<ZoneChangeRecord>,
    /// CR 403.3: Battlefield entry snapshots this turn, enabling data-driven ETB queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub battlefield_entries_this_turn: Vec<BattlefieldEntryRecord>,
    /// CR 120.1: Damage records this turn for "was dealt damage by" condition queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub damage_dealt_this_turn: Vec<DamageRecord>,
    /// CR 700.14: Cumulative mana spent on spells this turn per player (for Expend triggers).
    #[serde(default)]
    pub mana_spent_on_spells_this_turn: HashMap<PlayerId, u32>,
    /// CR 601.2f: One-shot cost reductions for the next spell cast.
    /// Consumed when the player casts their next qualifying spell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_spell_cost_reductions: Vec<PendingSpellCostReduction>,
    /// CR 601.2f: One-shot ability modifiers for the next spell cast.
    /// Consumed when the player casts their next qualifying spell.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_next_spell_modifiers: Vec<PendingNextSpellModifier>,
    /// CR 614.1c: Pending ETB counters for objects that haven't entered yet.
    /// Added by delayed triggers like "that creature enters with an additional +1/+1 counter".
    /// Consumed when the object enters the battlefield. Each entry: (object_id, counter_type, count).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_etb_counters: Vec<(ObjectId, String, u32)>,

    /// Modal modes chosen this turn per source: (ObjectId, mode_index).
    /// CR 700.2: "choose one that hasn't been chosen this turn"
    /// Note: ObjectId-keyed — zone changes create new ObjectId per CR 400.7, naturally resetting tracking.
    #[serde(default)]
    pub modal_modes_chosen_this_turn: HashSet<(ObjectId, usize)>,
    /// Modal modes chosen this game per source: (ObjectId, mode_index).
    /// CR 700.2: "choose one that hasn't been chosen" (game-scoped)
    /// Note: ObjectId-keyed — zone changes create new ObjectId per CR 400.7, naturally resetting tracking.
    #[serde(default)]
    pub modal_modes_chosen_this_game: HashSet<(ObjectId, usize)>,

    /// Cards currently revealed to all players (e.g. during a RevealHand effect).
    /// `filter_state_for_player` skips hiding these cards.
    #[serde(default)]
    pub revealed_cards: HashSet<ObjectId>,

    // Pending ability continuation after a player choice (Scry/Dig/Surveil).
    // When resolve_ability_chain pauses mid-chain for a choice state, the remaining
    // sub-ability is stored here and executed after the player responds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continuation: Option<Box<crate::types::ability::ResolvedAbility>>,

    /// Pending optional effect ability chain, awaiting player accept/decline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_optional_effect: Option<Box<crate::types::ability::ResolvedAbility>>,

    /// The most recently chosen named value (creature type, color, etc.).
    /// Set by the NamedChoice handler, consumed by continuation effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_named_choice: Option<ChoiceValue>,

    /// All creature subtypes seen across loaded cards. Used by Changeling CDA
    /// to grant every creature type at runtime.
    #[serde(default)]
    pub all_creature_types: Vec<String>,

    /// All card names from the loaded card database, used to validate
    /// "name a card" choices. Skipped in serialization to avoid sending 30k+ names.
    /// Wrapped in `Arc` so `GameState::clone()` during AI search is O(1) — avoids
    /// deep-copying 34k+ strings on every candidate evaluation.
    #[serde(skip)]
    pub all_card_names: Arc<[String]>,

    /// Card face data from the loaded card database, keyed by lowercase name.
    /// Used by the Conjure effect handler to create full cards at runtime.
    /// Skipped in serialization — repopulated by `rehydrate_game_from_card_db`.
    /// Wrapped in `Arc` so `GameState::clone()` during AI search is O(1).
    #[serde(skip)]
    pub card_face_registry: Arc<HashMap<String, CardFace>>,

    /// Display names for log resolution. Set by server; WASM leaves empty (defaults to "Player N").
    /// Skipped in serialization — runtime context only.
    #[serde(skip)]
    pub log_player_names: Vec<String>,

    /// Object IDs from the most recently resolved Effect::Token.
    /// Consumed by sub_abilities referencing "it"/"them" via TargetFilter::LastCreated.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_created_token_ids: Vec<ObjectId>,

    /// ObjectIds of cards revealed by the most recent RevealTop or reveal-Dig effect.
    /// Used by AbilityCondition::RevealedHasCardType and sub_ability target injection.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_revealed_ids: Vec<ObjectId>,

    /// ObjectIds of objects moved by the most recent zone-change effect.
    /// Used by AbilityCondition::ZoneChangedThisWay to gate sub_abilities on
    /// whether the parent effect moved an object matching a type filter.
    /// Cleared at depth 0 in resolve_ability_chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_zone_changed_ids: Vec<ObjectId>,

    /// CR 609.3: Numeric result from the preceding effect in a sub_ability chain.
    /// Set after resolve_effect for effects producing a numeric result (LoseLife, DealDamage).
    /// Read by QuantityRef::PreviousEffectAmount ("gain life equal to the life lost this way").
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_amount: Option<i32>,

    /// Count from the most recent interactive effect resolution (e.g., number of cards
    /// actually discarded in a DiscardChoice). Used as fallback for EventContextAmount
    /// in sub_ability continuations where current_trigger_event has no amount.
    /// Cleared at the top of apply() (once per player action).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_count: Option<i32>,

    /// CR 722: The current monarch, if any. At the beginning of the monarch's end step,
    /// the monarch draws a card. When a creature deals combat damage to the monarch,
    /// the creature's controller becomes the monarch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monarch: Option<PlayerId>,

    /// CR 702.131a: Players who have the city's blessing (from Ascend).
    /// Once gained, the city's blessing is permanent for the rest of the game.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub city_blessing: HashSet<PlayerId>,

    /// Active game-level restrictions (e.g., damage prevention disabled).
    /// Checked by relevant game systems; expired entries cleaned up at phase transitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restrictions: Vec<GameRestriction>,

    /// CR 615.3: Game-state-level damage prevention shields from fog-like spells.
    /// Instant/sorcery prevention effects (e.g., Fog: "prevent all combat damage") can't
    /// attach shields to their source (it moves to graveyard on resolution). Instead,
    /// shields are stored here and checked during damage application in `deal_damage.rs`.
    /// Cleaned up at end of turn during cleanup step.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_damage_prevention: Vec<crate::types::ability::ReplacementDefinition>,

    /// Transient: set by stack.rs before resolving a triggered ability, cleared after.
    /// Used by event-context TargetFilter variants to resolve trigger event data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_trigger_event: Option<GameEvent>,

    /// CR 400.7: Last Known Information cache.
    /// Populated before zone changes for objects leaving the battlefield.
    /// Cleared on phase/step transitions via `advance_phase()`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lki_cache: HashMap<ObjectId, LKISnapshot>,

    /// Transient: set by PayCost resolver when payment fails.
    /// Gates IfYouDo sub-abilities. Reset in DecideOptionalEffect handler.
    #[serde(skip)]
    pub cost_payment_failed_flag: bool,

    /// Pending cast info saved when entering ManaPayment state (X-cost or convoke).
    /// Consumed by the (ManaPayment, PassPriority) handler to finalize the cast.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_cast: Option<Box<PendingCast>>,

    /// CR 701.54: Per-player ring level (0-3, 4 levels total).
    #[serde(default)]
    pub ring_level: HashMap<PlayerId, u8>,
    /// CR 701.54: Per-player ring-bearer (the creature the Ring is on).
    #[serde(default)]
    pub ring_bearer: HashMap<PlayerId, Option<ObjectId>>,

    /// CR 309 / CR 701.49: Per-player dungeon venture progress.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub dungeon_progress: HashMap<PlayerId, crate::game::dungeon::DungeonProgress>,
    /// CR 725: The initiative designation (like monarch — one player at a time).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub initiative: Option<PlayerId>,
}

/// A runtime-generated continuous effect stored at state level.
///
/// Unlike `StaticDefinition` (which represents intrinsic/printed card text),
/// transient effects are created by resolving spells and abilities at runtime
/// (e.g., "target creature gets +3/+3 until end of turn"). They participate
/// in layer evaluation alongside intrinsic statics but have explicit lifetimes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TransientContinuousEffect {
    pub id: u64,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub timestamp: u64,
    pub duration: Duration,
    pub affected: TargetFilter,
    pub modifications: Vec<ContinuousModification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub condition: Option<StaticCondition>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingReplacement {
    pub proposed: ProposedEvent,
    pub candidates: Vec<ReplacementId>,
    pub depth: u16,
    /// When true, the replacement is Optional — index 0 = accept, index 1 = decline.
    /// `candidates` has exactly one entry (the real replacement); decline is synthetic.
    #[serde(default)]
    pub is_optional: bool,
}

/// Context stored when a permanent spell's ETB replacement needs a player choice
/// (e.g., Clone choosing a copy target). After the replacement resolves, the
/// post-resolution work (aura attachment, warp triggers, etc.) uses this context.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingSpellResolution {
    pub object_id: ObjectId,
    pub controller: PlayerId,
    pub casting_variant: CastingVariant,
    pub cast_from_zone: Option<crate::types::zones::Zone>,
    pub spell_targets: Vec<crate::types::ability::TargetRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTurnControl {
    pub target_player: PlayerId,
    pub controller: PlayerId,
    #[serde(default)]
    pub grant_extra_turn_after: bool,
}

impl GameState {
    /// Create a new game with the given format configuration and player count.
    pub fn new(config: FormatConfig, player_count: u8, seed: u64) -> Self {
        let players: Vec<Player> = (0..player_count)
            .map(|i| Player {
                id: PlayerId(i),
                life: config.starting_life,
                ..Player::default()
            })
            .collect();
        let seat_order: Vec<PlayerId> = (0..player_count).map(PlayerId).collect();

        GameState {
            turn_number: 0,
            active_player: PlayerId(0),
            phase: Phase::Untap,
            players,
            priority_player: PlayerId(0),
            turn_decision_controller: None,
            objects: HashMap::new(),
            next_object_id: 1,
            battlefield: Vec::new(),
            stack: Vec::new(),
            exile: Vec::new(),
            command_zone: Vec::new(),
            rng_seed: seed,
            rng: ChaCha20Rng::seed_from_u64(seed),
            combat: None,
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            has_pending_cast: false,
            lands_played_this_turn: 0,
            max_lands_per_turn: 1,
            priority_pass_count: 0,
            pending_replacement: None,
            post_replacement_effect: None,
            pending_spell_resolution: None,
            layers_dirty: true,
            next_timestamp: 1,
            public_state_dirty: PublicStateDirty::all_dirty(),
            state_revision: 0,
            transient_continuous_effects: Vec::new(),
            next_continuous_effect_id: 1,
            day_night: None,
            spells_cast_this_turn: 0,
            spells_cast_last_turn: None,
            pending_trigger: None,
            exile_links: Vec::new(),
            delayed_triggers: Vec::new(),
            tracked_object_sets: HashMap::new(),
            next_tracked_set_id: 1,
            commander_cast_count: HashMap::new(),
            extra_turns: Vec::new(),
            turns_to_skip: vec![0; player_count as usize],
            scheduled_turn_controls: Vec::new(),
            extra_phases: Vec::new(),
            seat_order,
            format_config: config,
            eliminated_players: Vec::new(),
            commander_damage: Vec::new(),
            priority_passes: BTreeSet::new(),
            auto_pass: HashMap::new(),
            lands_tapped_for_mana: HashMap::new(),
            match_config: MatchConfig::default(),
            match_phase: MatchPhase::InGame,
            match_score: MatchScore::default(),
            game_number: default_game_number(),
            current_starting_player: PlayerId(0),
            next_game_chooser: None,
            deck_pools: Vec::new(),
            sideboard_submitted: Vec::new(),
            triggers_fired_this_turn: HashSet::new(),
            trigger_fire_counts_this_turn: HashMap::new(),
            triggers_fired_this_game: HashSet::new(),
            activated_abilities_this_turn: HashMap::new(),
            activated_abilities_this_game: HashMap::new(),
            graveyard_cast_permissions_used: HashSet::new(),
            spells_cast_this_game: HashMap::new(),
            spells_cast_this_turn_by_player: HashMap::new(),
            players_who_searched_library_this_turn: HashSet::new(),
            players_attacked_this_step: HashSet::new(),
            players_attacked_this_turn: HashSet::new(),
            attacking_creatures_this_turn: HashMap::new(),
            creatures_attacked_this_turn: HashSet::new(),
            creatures_blocked_this_turn: HashSet::new(),
            players_who_created_token_this_turn: HashSet::new(),
            players_who_added_counter_this_turn: HashSet::new(),
            players_who_discarded_card_this_turn: HashSet::new(),
            players_who_sacrificed_artifact_this_turn: HashSet::new(),
            zone_changes_this_turn: Vec::new(),
            battlefield_entries_this_turn: Vec::new(),
            damage_dealt_this_turn: Vec::new(),
            mana_spent_on_spells_this_turn: HashMap::new(),
            pending_spell_cost_reductions: Vec::new(),
            pending_next_spell_modifiers: Vec::new(),
            pending_etb_counters: Vec::new(),
            modal_modes_chosen_this_turn: HashSet::new(),
            modal_modes_chosen_this_game: HashSet::new(),
            revealed_cards: HashSet::new(),
            pending_continuation: None,
            pending_optional_effect: None,
            last_named_choice: None,
            all_creature_types: Vec::new(),
            all_card_names: Arc::from([]),
            card_face_registry: Arc::new(HashMap::new()),
            log_player_names: Vec::new(),
            last_created_token_ids: Vec::new(),
            last_revealed_ids: Vec::new(),
            last_zone_changed_ids: Vec::new(),
            last_effect_amount: None,
            last_effect_count: None,
            monarch: None,
            city_blessing: HashSet::new(),
            restrictions: Vec::new(),
            pending_damage_prevention: Vec::new(),
            current_trigger_event: None,
            lki_cache: HashMap::new(),
            cost_payment_failed_flag: false,
            pending_cast: None,
            ring_level: HashMap::new(),
            ring_bearer: HashMap::new(),
            dungeon_progress: HashMap::new(),
            initiative: None,
            cancelled_casts: Vec::new(),
        }
    }

    /// Create a standard 2-player game (backward-compatible).
    pub fn new_two_player(seed: u64) -> Self {
        Self::new(FormatConfig::standard(), 2, seed)
    }

    /// Returns the current timestamp and increments for next use.
    pub fn next_timestamp(&mut self) -> u64 {
        let ts = self.next_timestamp;
        self.next_timestamp += 1;
        ts
    }

    /// Register a transient continuous effect and mark layers dirty.
    pub fn add_transient_continuous_effect(
        &mut self,
        source_id: ObjectId,
        controller: PlayerId,
        duration: Duration,
        affected: TargetFilter,
        modifications: Vec<ContinuousModification>,
        condition: Option<StaticCondition>,
    ) -> u64 {
        let id = self.next_continuous_effect_id;
        self.next_continuous_effect_id += 1;
        let timestamp = self.next_timestamp();
        self.transient_continuous_effects
            .push(TransientContinuousEffect {
                id,
                source_id,
                controller,
                timestamp,
                duration,
                affected,
                modifications,
                condition,
            });
        self.layers_dirty = true;
        id
    }
}

impl Default for GameState {
    fn default() -> Self {
        Self::new_two_player(0)
    }
}

// Reconstruct RNG from seed on deserialization
impl PartialEq for GameState {
    fn eq(&self, other: &Self) -> bool {
        self.turn_number == other.turn_number
            && self.active_player == other.active_player
            && self.phase == other.phase
            && self.players == other.players
            && self.priority_player == other.priority_player
            && self.turn_decision_controller == other.turn_decision_controller
            && self.objects.len() == other.objects.len()
            && self.next_object_id == other.next_object_id
            && self.battlefield == other.battlefield
            && self.stack == other.stack
            && self.exile == other.exile
            && self.command_zone == other.command_zone
            && self.rng_seed == other.rng_seed
            && self.combat == other.combat
            && self.waiting_for == other.waiting_for
            && self.lands_played_this_turn == other.lands_played_this_turn
            && self.max_lands_per_turn == other.max_lands_per_turn
            && self.priority_pass_count == other.priority_pass_count
            && self.pending_replacement == other.pending_replacement
            && self.pending_spell_resolution == other.pending_spell_resolution
            && self.layers_dirty == other.layers_dirty
            && self.next_timestamp == other.next_timestamp
            && self.public_state_dirty == other.public_state_dirty
            && self.state_revision == other.state_revision
            && self.day_night == other.day_night
            && self.spells_cast_this_turn == other.spells_cast_this_turn
            && self.spells_cast_last_turn == other.spells_cast_last_turn
            && self.pending_trigger == other.pending_trigger
            && self.exile_links == other.exile_links
            && self.delayed_triggers == other.delayed_triggers
            && self.tracked_object_sets == other.tracked_object_sets
            && self.next_tracked_set_id == other.next_tracked_set_id
            && self.commander_cast_count == other.commander_cast_count
            && self.extra_turns == other.extra_turns
            && self.turns_to_skip == other.turns_to_skip
            && self.scheduled_turn_controls == other.scheduled_turn_controls
            && self.extra_phases == other.extra_phases
            && self.seat_order == other.seat_order
            && self.format_config == other.format_config
            && self.eliminated_players == other.eliminated_players
            && self.commander_damage == other.commander_damage
            && self.priority_passes == other.priority_passes
            && self.auto_pass == other.auto_pass
            && self.lands_tapped_for_mana == other.lands_tapped_for_mana
            && self.match_config == other.match_config
            && self.match_phase == other.match_phase
            && self.match_score == other.match_score
            && self.game_number == other.game_number
            && self.current_starting_player == other.current_starting_player
            && self.next_game_chooser == other.next_game_chooser
            && self.deck_pools == other.deck_pools
            && self.sideboard_submitted == other.sideboard_submitted
            && self.triggers_fired_this_turn == other.triggers_fired_this_turn
            && self.trigger_fire_counts_this_turn == other.trigger_fire_counts_this_turn
            && self.triggers_fired_this_game == other.triggers_fired_this_game
            && self.activated_abilities_this_turn == other.activated_abilities_this_turn
            && self.activated_abilities_this_game == other.activated_abilities_this_game
            && self.graveyard_cast_permissions_used == other.graveyard_cast_permissions_used
            && self.spells_cast_this_game == other.spells_cast_this_game
            && self.spells_cast_this_turn_by_player == other.spells_cast_this_turn_by_player
            && self.players_who_searched_library_this_turn
                == other.players_who_searched_library_this_turn
            && self.players_attacked_this_step == other.players_attacked_this_step
            && self.players_attacked_this_turn == other.players_attacked_this_turn
            && self.attacking_creatures_this_turn == other.attacking_creatures_this_turn
            && self.creatures_attacked_this_turn == other.creatures_attacked_this_turn
            && self.creatures_blocked_this_turn == other.creatures_blocked_this_turn
            && self.players_who_created_token_this_turn == other.players_who_created_token_this_turn
            && self.players_who_added_counter_this_turn == other.players_who_added_counter_this_turn
            && self.players_who_discarded_card_this_turn
                == other.players_who_discarded_card_this_turn
            && self.players_who_sacrificed_artifact_this_turn
                == other.players_who_sacrificed_artifact_this_turn
            && self.zone_changes_this_turn == other.zone_changes_this_turn
            && self.battlefield_entries_this_turn == other.battlefield_entries_this_turn
            && self.damage_dealt_this_turn == other.damage_dealt_this_turn
            && self.pending_spell_cost_reductions == other.pending_spell_cost_reductions
            && self.pending_next_spell_modifiers == other.pending_next_spell_modifiers
            && self.pending_etb_counters == other.pending_etb_counters
            && self.modal_modes_chosen_this_turn == other.modal_modes_chosen_this_turn
            && self.modal_modes_chosen_this_game == other.modal_modes_chosen_this_game
            && self.pending_continuation == other.pending_continuation
            && self.pending_cast == other.pending_cast
            && self.last_named_choice == other.last_named_choice
            && self.last_revealed_ids == other.last_revealed_ids
            && self.last_zone_changed_ids == other.last_zone_changed_ids
            && self.last_effect_count == other.last_effect_count
            && self.lki_cache == other.lki_cache
            && self.city_blessing == other.city_blessing
    }
}

impl Eq for GameState {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_creates_two_player_game() {
        let state = GameState::default();
        assert_eq!(state.players.len(), 2);
    }

    #[test]
    fn default_starts_at_turn_zero() {
        let state = GameState::default();
        assert_eq!(state.turn_number, 0);
    }

    #[test]
    fn default_starts_in_untap_phase() {
        let state = GameState::default();
        assert_eq!(state.phase, Phase::Untap);
    }

    #[test]
    fn default_players_have_20_life() {
        let state = GameState::default();
        for player in &state.players {
            assert_eq!(player.life, 20);
        }
    }

    #[test]
    fn default_players_have_distinct_ids() {
        let state = GameState::default();
        assert_ne!(state.players[0].id, state.players[1].id);
    }

    #[test]
    fn game_state_has_central_object_store() {
        let state = GameState::default();
        assert!(state.objects.is_empty());
        assert_eq!(state.next_object_id, 1);
    }

    #[test]
    fn game_state_has_shared_zone_collections() {
        let state = GameState::default();
        assert!(state.battlefield.is_empty());
        assert!(state.stack.is_empty());
        assert!(state.exile.is_empty());
    }

    #[test]
    fn game_state_has_seeded_rng() {
        let state1 = GameState::new_two_player(42);
        let state2 = GameState::new_two_player(42);
        assert_eq!(state1.rng_seed, state2.rng_seed);
        assert_eq!(state1.rng_seed, 42);
    }

    #[test]
    fn game_state_has_waiting_for() {
        let state = GameState::default();
        assert_eq!(
            state.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        );
    }

    #[test]
    fn game_state_has_land_tracking() {
        let state = GameState::default();
        assert_eq!(state.lands_played_this_turn, 0);
        assert_eq!(state.max_lands_per_turn, 1);
    }

    #[test]
    fn new_two_player_creates_game_with_seed() {
        let state = GameState::new_two_player(12345);
        assert_eq!(state.rng_seed, 12345);
        assert_eq!(state.players.len(), 2);
    }

    #[test]
    fn game_state_serializes_and_roundtrips() {
        let state = GameState::default();
        let serialized = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&serialized).unwrap();
        // Reconstruct RNG from seed since it's skipped in serde
        deserialized.rng = ChaCha20Rng::seed_from_u64(deserialized.rng_seed);
        assert_eq!(state, deserialized);
    }

    #[test]
    #[allow(clippy::vec_init_then_push)]
    fn waiting_for_variants_exist() {
        fn dummy_pending() -> Box<PendingCast> {
            Box::new(PendingCast {
                object_id: ObjectId(1),
                card_id: CardId(1),
                ability: ResolvedAbility::new(
                    crate::types::ability::Effect::Unimplemented {
                        name: "Dummy".to_string(),
                        description: None,
                    },
                    vec![],
                    ObjectId(1),
                    PlayerId(0),
                ),
                cost: ManaCost::NoCost,
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: vec![],
                casting_variant: CastingVariant::Normal,
                distribute: None,
            })
        }

        // Use push to avoid large stack frame from vec! macro expansion.
        let mut variants: Vec<Box<WaitingFor>> = Vec::new();
        variants.push(Box::new(WaitingFor::Priority {
            player: PlayerId(0),
        }));
        variants.push(Box::new(WaitingFor::MulliganDecision {
            player: PlayerId(0),
            mulligan_count: 1,
        }));
        variants.push(Box::new(WaitingFor::MulliganBottomCards {
            player: PlayerId(0),
            count: 2,
        }));
        variants.push(Box::new(WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        }));
        variants.push(Box::new(WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![],
            valid_attack_targets: vec![],
        }));
        variants.push(Box::new(WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![],
            valid_block_targets: HashMap::new(),
        }));
        variants.push(Box::new(WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        }));
        variants.push(Box::new(WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 2,
            candidate_descriptions: vec![],
        }));
        variants.push(Box::new(WaitingFor::ExploreChoice {
            player: PlayerId(0),
            source_id: ObjectId(1),
            choosable: vec![ObjectId(2)],
            remaining: vec![ObjectId(2)],
            pending_effect: Box::new(ResolvedAbility::new(
                crate::types::ability::Effect::Unimplemented {
                    name: "Dummy".to_string(),
                    description: None,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            )),
        }));
        variants.push(Box::new(WaitingFor::EquipTarget {
            player: PlayerId(0),
            equipment_id: ObjectId(1),
            valid_targets: vec![],
        }));
        variants.push(Box::new(WaitingFor::ScryChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
        }));
        variants.push(Box::new(WaitingFor::DigChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
            keep_count: 1,
            up_to: false,
            selectable_cards: vec![ObjectId(1)],
            kept_destination: None,
            rest_destination: None,
            source_id: None,
        }));
        variants.push(Box::new(WaitingFor::SurveilChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
        }));
        variants.push(Box::new(WaitingFor::ChooseFromZoneChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
            count: 1,
            source_id: ObjectId(100),
        }));
        variants.push(Box::new(WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(ObjectId(1))],
                optional: false,
            }],
            target_constraints: vec![],
            selection: TargetSelectionProgress::default(),
            source_id: None,
            description: None,
        }));
        variants.push(Box::new(WaitingFor::ModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 3,
                ..Default::default()
            },
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::DiscardToHandSize {
            player: PlayerId(0),
            count: 2,
            cards: vec![ObjectId(1), ObjectId(2)],
        }));
        variants.push(Box::new(WaitingFor::OptionalCostChoice {
            player: PlayerId(0),
            cost: AdditionalCost::Optional(crate::types::ability::AbilityCost::Blight { count: 1 }),
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::AbilityModeChoice {
            player: PlayerId(0),
            modal: ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 2,
                ..Default::default()
            },
            source_id: ObjectId(1),
            mode_abilities: vec![],
            is_activated: true,
            ability_index: Some(0),
            ability_cost: None,
            unavailable_modes: vec![],
        }));
        variants.push(Box::new(WaitingFor::DiscardForCost {
            player: PlayerId(0),
            count: 1,
            cards: vec![ObjectId(1)],
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::SacrificeForCost {
            player: PlayerId(0),
            count: 1,
            permanents: vec![ObjectId(1)],
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::HarmonizeTapChoice {
            player: PlayerId(0),
            eligible_creatures: vec![ObjectId(1)],
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::ConniveDiscard {
            player: PlayerId(0),
            conniver_id: ObjectId(1),
            source_id: ObjectId(1),
            cards: vec![ObjectId(2)],
            count: 1,
        }));
        variants.push(Box::new(WaitingFor::DiscardChoice {
            player: PlayerId(0),
            count: 1,
            cards: vec![ObjectId(1)],
            source_id: ObjectId(100),
            effect_kind: crate::types::ability::EffectKind::Discard,
            up_to: false,
            unless_filter: None,
        }));
        variants.push(Box::new(WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
            count: 1,
            up_to: false,
            source_id: ObjectId(100),
            effect_kind: crate::types::ability::EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: false,
            enter_transformed: false,
            under_your_control: false,
            enters_attacking: false,
            owner_library: false,
        }));
        variants.push(Box::new(WaitingFor::DefilerPayment {
            player: PlayerId(0),
            life_cost: 2,
            mana_reduction: ManaCost::zero(),
            pending_cast: dummy_pending(),
        }));
        assert_eq!(variants.len(), 26);
    }

    #[test]
    fn stack_entry_kind_spell() {
        let entry = StackEntry {
            id: ObjectId(1),
            source_id: ObjectId(2),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(100),
                ability: None,
                casting_variant: CastingVariant::Normal,
            },
        };
        assert_eq!(entry.id, ObjectId(1));
        assert_eq!(entry.source_id, ObjectId(2));
        assert!(entry.ability().is_none());
    }

    #[test]
    fn action_result_contains_events_and_waiting_for() {
        let result = ActionResult {
            events: vec![GameEvent::GameStarted],
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            log_entries: vec![],
        };
        assert_eq!(result.events.len(), 1);
    }

    #[test]
    fn players_have_per_player_zones() {
        let state = GameState::default();
        for player in &state.players {
            assert!(player.library.is_empty());
            assert!(player.hand.is_empty());
            assert!(player.graveyard.is_empty());
        }
    }

    #[test]
    fn day_night_starts_none() {
        let state = GameState::default();
        assert_eq!(state.day_night, None);
    }

    #[test]
    fn spells_cast_this_turn_starts_zero() {
        let state = GameState::default();
        assert_eq!(state.spells_cast_this_turn, 0);
    }

    #[test]
    fn day_night_enum_variants() {
        assert_ne!(DayNight::Day, DayNight::Night);
    }

    #[test]
    fn day_night_changed_event_roundtrips() {
        let event = GameEvent::DayNightChanged {
            new_state: "Night".to_string(),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn exile_link_roundtrips() {
        let link = ExileLink {
            exiled_id: ObjectId(10),
            source_id: ObjectId(5),
            return_zone: Zone::Battlefield,
        };
        let json = serde_json::to_string(&link).unwrap();
        let deserialized: ExileLink = serde_json::from_str(&json).unwrap();
        assert_eq!(link, deserialized);
    }

    #[test]
    fn trigger_target_selection_roundtrips() {
        use crate::types::ability::TargetRef;
        let wf = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Object(ObjectId(1)),
                    TargetRef::Object(ObjectId(2)),
                ],
                optional: false,
            }],
            target_constraints: vec![],
            selection: TargetSelectionProgress::default(),
            source_id: Some(ObjectId(10)),
            description: Some("test trigger description".to_string()),
        };
        let json = serde_json::to_string(&wf).unwrap();
        let deserialized: WaitingFor = serde_json::from_str(&json).unwrap();
        assert_eq!(wf, deserialized);
        // Verify tag format
        assert!(json.contains("\"TriggerTargetSelection\""));
    }

    #[test]
    fn effect_zone_choice_roundtrips() {
        let wf = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1), ObjectId(2)],
            count: 1,
            up_to: true,
            source_id: ObjectId(10),
            effect_kind: crate::types::ability::EffectKind::ChangeZone,
            zone: Zone::Hand,
            destination: Some(Zone::Battlefield),
            enter_tapped: true,
            enter_transformed: false,
            under_your_control: true,
            enters_attacking: false,
            owner_library: false,
        };
        let json = serde_json::to_string(&wf).unwrap();
        let deserialized: WaitingFor = serde_json::from_str(&json).unwrap();
        assert_eq!(wf, deserialized);
        assert!(json.contains("\"EffectZoneChoice\""));
    }

    #[test]
    fn pending_trigger_roundtrips() {
        use crate::game::triggers::PendingTrigger;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};

        let trigger = PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 42,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        };
        let json = serde_json::to_string(&trigger).unwrap();
        let deserialized: PendingTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, deserialized);
    }

    #[test]
    fn game_state_with_pending_trigger_and_exile_links() {
        use crate::game::triggers::PendingTrigger;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};

        let mut state = GameState::new_two_player(42);
        state.exile_links.push(ExileLink {
            exiled_id: ObjectId(10),
            source_id: ObjectId(5),
            return_zone: Zone::Battlefield,
        });
        state.pending_trigger = Some(PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
        });

        let json = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&json).unwrap();
        deserialized.rng = rand_chacha::ChaCha20Rng::seed_from_u64(deserialized.rng_seed);
        assert_eq!(state, deserialized);
    }

    #[test]
    fn new_two_player_initializes_pending_trigger_and_exile_links() {
        let state = GameState::new_two_player(0);
        assert!(state.pending_trigger.is_none());
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn new_with_standard_config_matches_new_two_player() {
        let from_new = GameState::new(crate::types::format::FormatConfig::standard(), 2, 42);
        let from_legacy = GameState::new_two_player(42);
        assert_eq!(from_new.players.len(), from_legacy.players.len());
        assert_eq!(from_new.players[0].life, from_legacy.players[0].life);
        assert_eq!(from_new.players[1].life, from_legacy.players[1].life);
        assert_eq!(from_new.rng_seed, from_legacy.rng_seed);
        assert_eq!(from_new, from_legacy);
    }

    #[test]
    fn new_with_commander_config_creates_four_players_with_40_life() {
        let state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 0);
        assert_eq!(state.players.len(), 4);
        for player in &state.players {
            assert_eq!(player.life, 40);
        }
        assert_eq!(
            state.seat_order,
            vec![PlayerId(0), PlayerId(1), PlayerId(2), PlayerId(3)]
        );
    }

    #[test]
    fn new_initializes_seat_order() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        assert_eq!(state.seat_order, vec![PlayerId(0), PlayerId(1)]);
    }

    #[test]
    fn new_initializes_eliminated_players_empty() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        assert!(state.eliminated_players.is_empty());
    }

    #[test]
    fn new_initializes_commander_damage_empty() {
        let state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 0);
        assert!(state.commander_damage.is_empty());
    }

    #[test]
    fn new_initializes_priority_passes_empty() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        assert!(state.priority_passes.is_empty());
    }

    #[test]
    fn player_is_eliminated_defaults_to_false() {
        let state = GameState::new(crate::types::format::FormatConfig::standard(), 2, 0);
        for player in &state.players {
            assert!(!player.is_eliminated);
        }
    }

    #[test]
    fn new_two_player_has_seat_order_and_format_config() {
        let state = GameState::new_two_player(0);
        assert_eq!(state.seat_order, vec![PlayerId(0), PlayerId(1)]);
        assert_eq!(
            state.format_config,
            crate::types::format::FormatConfig::standard()
        );
    }

    #[test]
    fn game_state_with_new_fields_serializes_and_roundtrips() {
        let state = GameState::new(crate::types::format::FormatConfig::commander(), 4, 42);
        let serialized = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&serialized).unwrap();
        deserialized.rng = ChaCha20Rng::seed_from_u64(deserialized.rng_seed);
        assert_eq!(state, deserialized);
    }
}

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use super::ability::{
    default_target_filter_permanent, AbilityCost, AbilityDefinition, AdditionalCost,
    AdditionalCostInstance, AdditionalCostInstancePayment, AttackSubject, BeholdCostAction,
    CastVariantPaid, CategoryChooserScope, ChoiceType, ChoiceValue, ChooseFromZoneConstraint,
    ChosenAttribute, Comparator, ContinuousModification, CostPaidObjectSnapshot,
    CounterCostSelection, DelayedTriggerCondition, Duration, EffectKind, GameRestriction,
    KeywordAction, KickerVariant, LibraryPosition, ModalChoice, QuantityExpr, ResolvedAbility,
    SearchDestinationSplit, SearchSelectionConstraint, StaticCondition, TargetFilter, TargetRef,
    ThisWayCause, TriggerCondition, TriggerDefinition,
};
use super::attribution::ObjectAttribution;
use super::card::CardFace;
use super::card_type::{CoreType, Supertype};
use super::counter::{counter_map_serde, CounterMatch, CounterType};
use super::events::{GameEvent, PlayerActionKind};
use super::format::FormatConfig;
use super::identifiers::{CardId, ObjectId, TrackedSetId};
use super::keywords::{Keyword, KeywordKind};
use super::mana::{ManaColor, ManaCost, ManaType, StepEndManaAction};
use super::match_config::{MatchConfig, MatchPhase, MatchScore};
use super::phase::Phase;
use super::player::{Player, PlayerCounterKind, PlayerId};
use super::proposed_event::{CopyTokenSpec, ProposedEvent, ReplacementId, TokenSpec};
use super::zones::EtbTapState;
use super::zones::{ExileCostSourceZone, Zone};

use crate::game::bracket_estimate::CommanderBracketTier;
use crate::game::combat::{AttackTarget, CombatState};
use crate::game::deck_loading::DeckEntry;

use crate::game::game_object::{AttachTarget, GameObject};

fn default_rng() -> ChaCha20Rng {
    ChaCha20Rng::seed_from_u64(0)
}

fn default_game_number() -> u8 {
    1
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

pub(crate) fn is_zero_usize(value: &usize) -> bool {
    *value == 0
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
    /// CR 702.126a: Improvise — tap an untapped artifact to pay one generic mana.
    Improvise,
    /// CR 702.66a: Delve — exile a card from your graveyard to pay one generic
    /// mana. Unlike the others, the "source" is a graveyard card that is exiled
    /// (not a battlefield permanent that is tapped).
    Delve,
}

/// CR 702.132a: Tracks the once-per-cast Assist offer/decision on a `PendingCast`.
/// A typed enum (not a bool) so the offered-once guard and the committed
/// contribution share one field. `Committed` defers the helper's actual mana
/// spend to `finalize_cast`, so cancelling the cast never leaks tapped lands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AssistState {
    /// The Assist offer has not been made for this cast.
    #[default]
    NotOffered,
    /// The offer was made and the caster declined (or contributed nothing).
    Offered,
    /// The caster chose `helper`, who will pay `generic` of the spell's generic
    /// mana. The caster's owed cost is reduced by `generic` now; the helper's
    /// sources are tapped only at `finalize_cast` (the non-cancellable commit).
    Committed { helper: PlayerId, generic: u32 },
}

/// CR 400.7: Snapshot of an object's characteristics at the time it left a public zone.
/// Used for event-context resolution when the object is no longer in its original zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LKISnapshot {
    pub name: String,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    /// CR 208.4b + CR 613.4b: Base power as it last existed in the public zone
    /// (layer-7b value). Threaded so that an LKI snapshot converted to a
    /// `ZoneChangeRecord` (see `matches_target_filter_on_lki_snapshot`)
    /// evaluates `PtComparison { scope: Base }` against the base value rather
    /// than defaulting to 0.
    #[serde(default)]
    pub base_power: Option<i32>,
    /// CR 208.4b + CR 613.4b: Base toughness as it last existed in the public zone.
    #[serde(default)]
    pub base_toughness: Option<i32>,
    pub mana_value: u32,
    pub controller: PlayerId,
    pub owner: PlayerId,
    /// CR 400.7: Core types as they last existed on the battlefield.
    /// Used by `TriggerCondition::WasType` for "if it was a creature" patterns.
    #[serde(default)]
    pub card_types: Vec<CoreType>,
    /// CR 400.7: Subtypes as they last existed in the public zone.
    #[serde(default)]
    pub subtypes: Vec<String>,
    /// CR 400.7: Supertypes as they last existed in the public zone.
    #[serde(default)]
    pub supertypes: Vec<Supertype>,
    /// CR 400.7: Keywords as they last existed in the public zone.
    #[serde(default)]
    pub keywords: Vec<Keyword>,
    /// CR 400.7: Colors as they last existed in the public zone.
    #[serde(default)]
    pub colors: Vec<ManaColor>,
    /// CR 400.7: Persisted choices as they last existed in the public zone.
    /// Source-linked abilities use this after the source leaves before a
    /// linked "the chosen player" instruction resolves.
    #[serde(default)]
    pub chosen_attributes: Vec<ChosenAttribute>,
    /// CR 400.7: Counters as they last existed on the object.
    /// Used by `TriggerCondition::HadCounters` for "if it had counters on it" patterns.
    #[serde(default, with = "counter_map_serde")]
    pub counters: HashMap<CounterType, u32>,
}

/// CR 106.3 + CR 601.2h: Snapshot of the source of one mana spent to cast a spell.
///
/// Mana remembers the source that produced it, and source-qualified Oracle text
/// ("mana from a Treasure", "mana from an artifact source") needs the source's
/// characteristics as they existed when the mana was paid, not a post-hoc lookup
/// after the source may have left the battlefield or changed characteristics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaSpentSourceSnapshot {
    pub source_id: ObjectId,
    pub lki: LKISnapshot,
}

/// Snapshot of a spell's characteristics at cast time for per-turn history queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpellCastRecord {
    /// CR 201.2: Card name captured at cast time so name-filtered history
    /// queries (e.g. Approach of the Second Sun's "another spell named
    /// {LITERAL} this game") can resolve against `FilterProp::Named { name }`
    /// without rehydrating the cast object.
    /// `#[serde(default)]` keeps the field optional for serialized snapshots
    /// predating this addition — those records won't match name filters.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub core_types: Vec<CoreType>,
    pub supertypes: Vec<Supertype>,
    pub subtypes: Vec<String>,
    pub keywords: Vec<Keyword>,
    pub colors: Vec<ManaColor>,
    pub mana_value: u32,
    /// CR 107.3 + CR 601.2b: Whether the spell's printed mana cost contains an `{X}`
    /// shard. Captured at cast-time so later filtered counting (CR 117.1) can
    /// match "spell with {X} in its mana cost" predicates without re-inspecting
    /// the underlying object (which may have left the stack).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub has_x_in_cost: bool,
    /// CR 400.1 + CR 601.2a: Zone the spell was cast from, captured at cast-time
    /// so per-turn spell-history conditions can answer "from your hand" after
    /// the spell has moved on from the stack. Per CR 601.2a every cast spell
    /// is moved "from where it is" to the stack, so this field is always
    /// populated. Older serialized snapshots emitted this as `Option<Zone>`
    /// (with `null` for the default); the custom deserializer accepts both
    /// shapes and falls back to `Zone::Hand` (the dominant origin per
    /// CR 601.2a) when the field is missing or `null`.
    #[serde(
        default = "default_spell_cast_record_from_zone",
        deserialize_with = "deserialize_spell_cast_record_from_zone"
    )]
    pub from_zone: Zone,
    /// CR 702.185c: The alternative-cast variant chosen when this spell was
    /// cast (Warp, etc.), captured at cast-time so per-turn spell-history
    /// conditions ("a spell was warped this turn") can answer after the spell
    /// has left the stack. `#[serde(default)]` yields `CastingVariant::Normal`
    /// for serialized snapshots predating this field.
    #[serde(default)]
    pub cast_variant: CastingVariant,
}

/// Snapshot of a land play's cast-capable origin for per-turn history queries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LandPlayRecord {
    /// CR 305.2a + CR 601.2a: Zone the land was played from, captured at play
    /// time so end-step conditions can answer "played a land from outside your
    /// hand" after the land has moved or left the battlefield.
    pub from_zone: Zone,
}

/// CR 601.2a: Default origin zone for `SpellCastRecord.from_zone`. Hand is the
/// overwhelmingly common cast origin, so it's the safe default for snapshots
/// that pre-date the non-Option migration.
fn default_spell_cast_record_from_zone() -> Zone {
    Zone::Hand
}

impl Default for SpellCastRecord {
    fn default() -> Self {
        Self {
            name: String::new(),
            core_types: Vec::new(),
            supertypes: Vec::new(),
            subtypes: Vec::new(),
            keywords: Vec::new(),
            colors: Vec::new(),
            mana_value: 0,
            has_x_in_cost: false,
            from_zone: Zone::Hand,
            cast_variant: CastingVariant::Normal,
        }
    }
}

/// Backwards-compatible deserializer for `SpellCastRecord.from_zone`. Accepts
/// the modern non-Option encoding (`"Hand"`, `"Battlefield"`, …), the legacy
/// `Option<Zone>` encoding (`null` → `Zone::Hand`), and absent fields (handled
/// by `#[serde(default = …)]` upstream of this hook).
fn deserialize_spell_cast_record_from_zone<'de, D>(de: D) -> Result<Zone, D::Error>
where
    D: serde::Deserializer<'de>,
{
    Ok(Option::<Zone>::deserialize(de)?.unwrap_or_else(default_spell_cast_record_from_zone))
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
    /// CR 118.9a: "The next [filter] spell you cast this turn can be cast without
    /// paying its mana cost." Additional costs still apply (CR 118.8).
    WithoutPayingManaCost,
}

/// CR 400.7: Snapshot of an object's properties at the time of a zone change,
/// enabling data-driven filtered counting at resolution time and event-time
/// trigger-filter evaluation (CR 603.10) after the object has moved zones.
///
/// Fields are captured at move-time so that subsequent filter evaluations
/// (e.g. "whenever a creature with power 4 or greater dies") can read the
/// event-time characteristics instead of chasing the object to its new zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneChangeRecord {
    pub object_id: ObjectId,
    pub name: String,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub keywords: Vec<Keyword>,
    /// CR 603.10a: Trigger definitions as they last existed on the object.
    /// Runtime-granted leaves-the-battlefield keyword triggers can be removed
    /// from the live object before the look-back trigger scan, so the zone-change
    /// record carries the exact LKI trigger multiset.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub trigger_definitions: Vec<TriggerDefinition>,
    /// CR 208.1: Power as of the zone change.
    pub power: Option<i32>,
    /// CR 208.1: Toughness as of the zone change.
    pub toughness: Option<i32>,
    /// CR 208.4b + CR 613.4b: Base power as of the zone change (the layer-7b
    /// value, ignoring +1/+1 counters and non-setting P/T modifiers in layer
    /// 7c). Read by `PtComparison` filters with `scope = Base` on the look-back
    /// (leaves-the-battlefield / dies) path so base-vs-current is honored after
    /// the object has left the battlefield (CR 603.10a).
    #[serde(default)]
    pub base_power: Option<i32>,
    /// CR 208.4b + CR 613.4b: Base toughness as of the zone change.
    #[serde(default)]
    pub base_toughness: Option<i32>,
    /// CR 105.1 / CR 202.2: Colors as of the zone change.
    pub colors: Vec<ManaColor>,
    /// CR 202.3: Mana value as of the zone change.
    pub mana_value: u32,
    pub controller: PlayerId,
    pub owner: PlayerId,
    /// CR 603.6a + CR 111.1: `None` when the object was created directly in the
    /// destination zone without existing in a prior zone (e.g. token creation
    /// on the battlefield, emblem creation in the command zone). For normal
    /// zone moves this carries the origin zone.
    pub from_zone: Option<Zone>,
    pub to_zone: Zone,
    /// CR 603.10a + CR 603.6e: Snapshot of attachments on the object at the moment
    /// of the zone change. Required by look-back triggers of the form
    /// "for each Aura you controlled that was attached to it" (Hateful Eidolon),
    /// since Aura attachments are cleared by SBA immediately after the creature
    /// leaves the battlefield.
    #[serde(default)]
    pub attachments: Vec<AttachmentSnapshot>,
    /// CR 603.10a + CR 607.2a: Snapshot of cards linked as "exiled with" this
    /// object at the moment it left the battlefield. Leaves-the-battlefield
    /// triggers resolve later through `current_trigger_event`, after
    /// `TrackedBySource` links have been pruned per CR 400.7, so linked-exile
    /// follow-ups (Skyclave Apparition) must read this look-back snapshot
    /// instead of the live `state.exile_links`.
    #[serde(default)]
    pub linked_exile_snapshot: Vec<LinkedExileSnapshot>,
    /// CR 111.1: Token identity at the moment of the zone change. Token-ness is a
    /// stable property of the object (not ephemeral battlefield state), so filters
    /// like "whenever a creature token dies" (Grismold) evaluate against this
    /// snapshot after the object has left the battlefield.
    #[serde(default)]
    pub is_token: bool,
    /// CR 506.4 + CR 603.10a: Combat status immediately before the object left
    /// its zone. Leaving combat clears live combat maps, so LTB filters such as
    /// "attacking creatures die" and "if it wasn't blocking" must read this
    /// snapshot rather than current combat state.
    #[serde(default)]
    pub combat_status: ZoneChangeCombatStatus,
    /// CR 603.10a: ObjectIds that left the battlefield in the SAME simultaneous
    /// event as this object (every permanent destroyed by one board wipe, every
    /// creature destroyed together by a single state-based-action check, etc.),
    /// excluding this object. Populated only by producers of a simultaneous
    /// departure batch via `zones::mark_simultaneous_departures`; empty for a
    /// lone departure or for departures that are separate sequential instructions
    /// of one resolution. A leaves-the-battlefield / dies observer listed here
    /// observes this departure via last-known information (CR 603.10a's worked
    /// example); a creature that left in an earlier, separate event is not listed
    /// and therefore does not cross-observe. This is the authority for
    /// simultaneity — trigger collection must not infer it from the shape of the
    /// accumulated event vector.
    #[serde(default)]
    pub co_departed: Vec<ObjectId>,
}

/// CR 506.4 / CR 508.1k / CR 509.1g / CR 509.1h: Combat role snapshot for an
/// object leaving its current zone.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ZoneChangeCombatStatus {
    #[serde(default)]
    pub attacking: bool,
    #[serde(default)]
    pub blocking: bool,
    #[serde(default)]
    pub blocked: bool,
    #[serde(default)]
    pub defending_player: Option<PlayerId>,
}

/// CR 508.1a: Snapshot of a creature's public characteristics when it was
/// declared as an attacker.
///
/// Later "you attacked with <quality> this turn" checks resolve after combat,
/// after the attacker may have changed zones or ceased to exist, so they must
/// read declaration-time characteristics instead of live battlefield state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttackDeclarationRecord {
    pub object_id: ObjectId,
    pub lki: LKISnapshot,
    /// CR 111.1: Token identity at declaration time.
    #[serde(default)]
    pub is_token: bool,
    /// CR 903.3d: Commander identity at declaration time.
    #[serde(default)]
    pub is_commander: bool,
}

/// CR 603.10a: Snapshot of a single attachment on a leaving-battlefield object
/// at the instant before the zone change. Controller/kind are captured so that
/// post-LTB resolvers can filter ("each Aura you controlled") without chasing
/// the attachment object, which may itself be in a different zone by then.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttachmentSnapshot {
    pub object_id: ObjectId,
    pub controller: PlayerId,
    pub kind: crate::types::ability::AttachmentKind,
}

/// CR 603.10a + CR 607.2a: Snapshot of a single card linked as "exiled with"
/// a source at the instant before that source leaves the battlefield.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkedExileSnapshot {
    pub exiled_id: ObjectId,
    pub owner: PlayerId,
    pub mana_value: u32,
}

#[cfg(test)]
impl ZoneChangeRecord {
    /// Minimal skeleton for tests. Non-transition fields default to empty/zero;
    /// override specific fields with struct update syntax:
    ///   `ZoneChangeRecord { core_types: vec![..], ..ZoneChangeRecord::test_minimal(id, from, to) }`
    ///
    /// Production code must use `GameObject::snapshot_for_zone_change` — the
    /// authoritative constructor that copies from a live object.
    pub fn test_minimal(object_id: ObjectId, from: Option<Zone>, to: Zone) -> Self {
        Self {
            object_id,
            name: String::new(),
            core_types: Vec::new(),
            subtypes: Vec::new(),
            supertypes: Vec::new(),
            keywords: Vec::new(),
            trigger_definitions: Vec::new(),
            power: None,
            toughness: None,
            base_power: None,
            base_toughness: None,
            colors: Vec::new(),
            mana_value: 0,
            controller: PlayerId(0),
            owner: PlayerId(0),
            from_zone: from,
            to_zone: to,
            attachments: Vec::new(),
            linked_exile_snapshot: Vec::new(),
            is_token: false,
            combat_status: ZoneChangeCombatStatus::default(),
            co_departed: Vec::new(),
        }
    }
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
    #[serde(default)]
    pub colors: Vec<ManaColor>,
    pub controller: PlayerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoMayChoice {
    Accept,
    Decline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MayTriggerOrigin {
    Printed { trigger_index: usize },
    Keyword { keyword: KeywordKind },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct MayTriggerAutoChoiceKey {
    pub player: PlayerId,
    pub source_id: ObjectId,
    pub origin: MayTriggerOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MayTriggerAutoChoiceRecord {
    pub key: MayTriggerAutoChoiceKey,
    pub choice: AutoMayChoice,
}

/// CR 609.7a: A source of damage chosen while creating a prevention or
/// replacement effect. The original filter is retained so property-based
/// choices such as "red source of your choice" recheck source qualities when
/// damage would be dealt (CR 609.7b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChosenDamageSource {
    pub source_id: ObjectId,
    pub source_filter: TargetFilter,
}

/// CR 120.1: Snapshot of a damage event for "was dealt damage by" queries.
///
/// CR 608.2i + CR 608.2h: source characteristics snapshot at damage time
/// (look-back; criteria need not still hold). Queries such as "opponents who
/// were dealt combat damage by ~ or a Dragon this turn" (Estinien Varlineau)
/// must match the source's qualities *as they were when damage was dealt* — the
/// source may have since changed type, left the battlefield, or been removed.
/// The `source_*` snapshot fields mirror `CounterAddedRecord`'s event-time
/// characteristic capture and feed `matches_target_filter_on_damage_record_source`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageRecord {
    pub source_id: ObjectId,
    #[serde(default)]
    pub source_controller: PlayerId,
    pub target: TargetRef,
    #[serde(default)]
    pub target_controller: PlayerId,
    pub amount: u32,
    #[serde(default)]
    pub is_combat: bool,
    // CR 608.2i + CR 608.2h: source characteristics snapshot at damage time
    // (look-back; criteria need not still hold).
    #[serde(default)]
    pub source_name: String,
    #[serde(default)]
    pub source_core_types: Vec<CoreType>,
    #[serde(default)]
    pub source_subtypes: Vec<String>,
    #[serde(default)]
    pub source_supertypes: Vec<Supertype>,
    #[serde(default)]
    pub source_keywords: Vec<Keyword>,
    #[serde(default)]
    pub source_power: Option<i32>,
    #[serde(default)]
    pub source_toughness: Option<i32>,
    #[serde(default)]
    pub source_colors: Vec<ManaColor>,
    #[serde(default)]
    pub source_mana_value: u32,
    #[serde(default)]
    pub source_controller_snapshot: PlayerId,
    #[serde(default)]
    pub source_owner: PlayerId,
    /// CR 608.2i + CR 608.2h: the source's zone at damage time. Non-combat
    /// damage from a spell originates from the Stack, so a zone-discriminating
    /// look-back source filter ("by a permanent") must evaluate against the
    /// recorded zone, not an assumed battlefield. Defaults to `Battlefield`
    /// (the common combat-damage case) for legacy records and test fixtures.
    #[serde(default = "default_source_zone")]
    pub source_zone: Zone,
}

/// CR 608.2i: Default damage-source zone. Combat damage — the overwhelmingly
/// common recorded case — comes from the battlefield, so legacy serialized
/// records and `..Default::default()` test fixtures default to it.
fn default_source_zone() -> Zone {
    Zone::Battlefield
}

impl Default for DamageRecord {
    /// A non-combat, zero-amount record from/to player 0 with an empty source
    /// snapshot. Production damage recording (`deal_damage.rs`) always fills
    /// every field explicitly; this default exists so test and synthesis
    /// fixtures that only care about a few fields can spread `..Default::default()`
    /// for the CR 608.2i source-snapshot fields they don't exercise.
    fn default() -> Self {
        Self {
            source_id: ObjectId(0),
            source_controller: PlayerId(0),
            target: TargetRef::Player(PlayerId(0)),
            target_controller: PlayerId(0),
            amount: 0,
            is_combat: false,
            source_name: String::new(),
            source_core_types: Vec::new(),
            source_subtypes: Vec::new(),
            source_supertypes: Vec::new(),
            source_keywords: Vec::new(),
            source_power: None,
            source_toughness: None,
            source_colors: Vec::new(),
            source_mana_value: 0,
            source_controller_snapshot: PlayerId(0),
            source_owner: PlayerId(0),
            source_zone: Zone::Battlefield,
        }
    }
}

/// CR 122.1 + CR 122.6: Snapshot of counters put on an object this turn.
///
/// Captures both the player who put the counters and the recipient object's
/// event-time characteristics, so dynamic quantities can later answer
/// "for each +1/+1 counter you've put on creatures under your control this turn"
/// even if the recipient has changed zones or characteristics.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterAddedRecord {
    pub actor: PlayerId,
    pub object_id: ObjectId,
    pub counter_type: CounterType,
    pub count: u32,
    pub name: String,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub keywords: Vec<Keyword>,
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    pub colors: Vec<ManaColor>,
    pub mana_value: u32,
    pub controller: PlayerId,
    pub owner: PlayerId,
    #[serde(default, with = "counter_map_serde")]
    pub counters: HashMap<CounterType, u32>,
}

/// CR 607.2a + CR 406.6: Tracks the link between an exiling source and the exiled card.
/// When the source leaves the battlefield, the exiled card returns (CR 610.3a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ExileLinkKind {
    /// CR 610.3a: Return the exiled object when the source leaves the battlefield.
    UntilSourceLeaves { return_zone: Zone },
    /// Track cards "exiled with" a source without creating an automatic return.
    TrackedBySource,
    /// CR 702.xxx: Paradigm (Strixhaven) — this exile entry marks the card as a
    /// paradigm source. The identified `player` is the one for whom Paradigm
    /// armed at first resolution; at the start of each of that player's first
    /// main phases, a turn-based offer lets them cast a copy of this card
    /// without paying its mana cost (CR 601.2h + CR 707.10). The exiled card
    /// itself stays in exile across turns — the offer produces a token spell
    /// copy on the stack (CR 707.10f), not a re-cast of the original. Assign
    /// when WotC publishes SOS CR update.
    ParadigmSource { player: PlayerId },
    /// CR 702.99b: Cipher — the exiled card (`exiled_id`) is *encoded* on the
    /// creature (`source_id`). While the card stays in exile and the creature
    /// stays on the battlefield, the creature has "Whenever this creature deals
    /// combat damage to a player, its controller may cast a copy of the encoded
    /// card without paying its mana cost" (CR 702.99c). The link is pruned
    /// automatically when the card leaves exile (`zones.rs` exile-exit) or the
    /// creature leaves the battlefield (`zones.rs` battlefield-exit, since this
    /// is not an `UntilSourceLeaves` link) — exactly CR 702.99c's lifetime.
    Cipher,
    /// CR 702.55b: Haunt — the exiled card (`exiled_id`) "haunts" the creature
    /// (`source_id`) targeted by its haunt ability. The link drives the card's
    /// haunt-payoff trigger, which fires from the exile zone when the haunted
    /// creature dies (CR 702.55c). Unlike `Cipher`, this link is **preserved**
    /// when the haunted creature leaves the battlefield (`zones.rs` battlefield
    /// exit) — the haunted creature's death is exactly when the payoff must read
    /// the link. The card "haunts the creature it haunts regardless of whether
    /// or not that object is still a creature" (CR 702.55b), so the link is
    /// pruned only when the haunting card itself leaves exile (`zones.rs`
    /// exile-exit), not when the creature changes or dies.
    Haunt,
    /// CR 702.75a: Hideaway — the card (`exiled_id`) was exiled face down by the
    /// permanent (`source_id`). Like `TrackedBySource` it tracks the card so the
    /// companion "you may play the exiled card" ability (`TargetFilter::
    /// ExiledBySource`, which is kind-agnostic) can later find it — but it
    /// additionally grants a *look-permission*: the player who controls the
    /// exiling permanent "may look at this card in the exile zone". Visibility
    /// keys the controller's face-down look-through on this kind specifically, so
    /// plain `TrackedBySource` face-down exiles that grant no such permission
    /// (Bomat Courier's "(You can't look at it.)", Necropotence, Asmodeus) stay
    /// redacted. Pruned on exile-exit / source-exit like `Cipher` (not an
    /// `UntilSourceLeaves` link, so no automatic return).
    HideawayLookable,
    /// CR 702.167c: Craft material — the card (`exiled_id`) was exiled to pay the
    /// craft activation cost of the permanent (`source_id`) that returns to the
    /// battlefield transformed. "An ability of a permanent may refer to the
    /// exiled cards used to craft it." Unlike `TrackedBySource`, this link is
    /// **preserved** when the craft source leaves the battlefield — the source
    /// self-exiles mid-activation (CR 702.167a) and returns with the SAME
    /// ObjectId, so the link must survive its battlefield exit for the returned
    /// permanent to read it. Unlike `UntilSourceLeaves` it triggers NO automatic
    /// return (the materials stay in exile). Read by the kind-agnostic
    /// `ExiledBySource` / `CardsExiledBySource` consumers; pruned only when a
    /// material itself leaves exile (`zones.rs` exile-exit).
    CraftMaterial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExileLink {
    pub exiled_id: ObjectId,
    pub source_id: ObjectId,
    pub kind: ExileLinkKind,
}

/// CR 702.xxx: Paradigm (Strixhaven) first-resolution record.
///
/// Stored in `GameState::paradigm_primed`. Each entry gates "first" against
/// the `(player, card_name)` pair: subsequent resolutions of the same card
/// name by the same player never re-arm Paradigm (the reminder text says
/// "After you **first** resolve a spell with this name"). Name, not ObjectId,
/// is the key per reminder wording — a different physical card with the same
/// printed name still counts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ParadigmPrime {
    pub player: PlayerId,
    pub card_name: String,
}

/// Tracks commander damage dealt to a specific player by a specific commander.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommanderDamageEntry {
    pub player: PlayerId,
    pub commander: ObjectId,
    pub damage: u32,
}

/// Resume state for an ability chain paused mid-resolution.
///
/// When `resolve_ability_chain` cannot advance because an effect entered an
/// interactive state (scry/surveil/dig, search, discard-to-hand-size,
/// replacement-choice, etc.) or because a damage replacement proposal needs
/// a player choice, the remainder of the chain is stashed here and replayed
/// once the choice resolves.
///
/// `parent_kind` carries the outer effect's `EffectKind` when that parent
/// normally emits an `EffectResolved { kind, source_id }` at the tail of its
/// resolver — but the pause path returned early before it could fire. The
/// drain step (see `drain_pending_continuation`) resolves the chain and then
/// emits the parent event, so trigger matchers keyed on the parent kind
/// (e.g. `match_fight` on `EffectKind::Fight` in `trigger_matchers.rs`) fire
/// on the pause path as well. `None` means the chain has no distinct parent
/// event — each chain node emits its own `EffectResolved` and that is the
/// correct observable behavior.
///
/// The chain and its parent-kind metadata are coupled in one type so they
/// cannot go out of sync; two parallel `Option`s would let one be set
/// without the other and break the "pause emits the same event as
/// non-pause" invariant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingContinuation {
    pub chain: Box<ResolvedAbility>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_kind: Option<EffectKind>,
}

impl PendingContinuation {
    /// Construct a continuation with no parent-kind emission. Used for chains
    /// whose per-node `EffectResolved` events are the full observable story
    /// (targeted damage continuations, Learn rummage, Bolster, Clash, etc.).
    pub fn new(chain: Box<ResolvedAbility>) -> Self {
        Self {
            chain,
            parent_kind: None,
        }
    }

    /// Construct a continuation whose drain must re-emit the outer effect's
    /// `EffectResolved { kind, source_id }` once the chain completes. The
    /// `source_id` used for emission is read from `chain.source_id` at drain
    /// time, matching the non-pause path.
    pub fn with_parent_kind(chain: Box<ResolvedAbility>, parent_kind: EffectKind) -> Self {
        Self {
            chain,
            parent_kind: Some(parent_kind),
        }
    }
}

/// CR 609.3 + CR 109.5: Resume state for a `repeat_for` iteration loop paused
/// when the inner effect entered an interactive `WaitingFor` state.
///
/// When `resolve_ability_chain` is executing the iteration loop for a
/// `repeat_for` quantity (e.g., Winds of Abandon overloaded, where each
/// exiled creature's controller searches their library), the inner effect can
/// transition to `WaitingFor::SearchChoice` (or any other player-choice
/// state). Without resumption, only the first iteration would ever run — the
/// loop breaks at the first paused iteration and the remaining iterations are
/// silently dropped.
///
/// This struct stashes everything needed to re-enter the loop after the
/// current iteration's player choice (and any chained sub-ability) drains:
/// - `ability` — the effective per-iteration ability (parent of the loop's
///   `effect`); cloned with `sub_ability = None` because the sub-ability is
///   already wired through `pending_continuation` for the current iteration.
/// - `tracked_members` — the tracked-set members snapshotted at loop entry
///   (used by `effect_refs_parent_target` rebinding). Empty when no rebind
///   is required.
/// - `next_iteration` — index of the iteration that should run next when the
///   resume fires.
/// - `total_iterations` — original loop bound, used to detect completion.
///
/// Drained by `drain_pending_continuation` after the per-iteration
/// `pending_continuation` chain fully drains. Each resumed iteration may
/// itself pause and re-stash this struct (recursive drive).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRepeatIteration {
    pub ability: Box<crate::types::ability::ResolvedAbility>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_members: Vec<ObjectId>,
    /// CR 122.1 + CR 608.2c: the per-iteration counter kinds snapshotted at
    /// loop entry for a `repeat_for: DistinctCounterKindsAmong` loop. Indexed
    /// by iteration number; each resumed iteration rebinds its tagged
    /// `ChooseOneOf` branch to `iterated_counter_kinds[iteration]`. Empty when
    /// the loop is not counter-kind-driven.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub iterated_counter_kinds: Vec<crate::types::counter::CounterType>,
    pub next_iteration: usize,
    pub total_iterations: usize,
}

/// CR 705.1 + CR 614.1a: Discriminates which multi-flip resolver paused for a
/// Krark's Thumb keep-1 choice, carrying the loop position needed to re-enter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingCoinFlipKind {
    /// `Effect::FlipCoin` — a single logical flip.
    Single,
    /// `Effect::FlipCoins { count }` — `remaining` flips still to perform after
    /// the one currently paused for a keep choice.
    FlipN { remaining: u32 },
    /// `Effect::FlipCoinUntilLose` — `wins_so_far` flips won before the one
    /// currently paused for a keep choice.
    UntilLose { wins_so_far: u32 },
}

/// CR 705.1 + CR 614.1a: Full resolution context + loop position for a
/// multi-flip resolver paused mid-loop for a Krark's Thumb keep-1 choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCoinFlip {
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub targets: Vec<TargetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win_effect: Option<Box<AbilityDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lose_effect: Option<Box<AbilityDefinition>>,
    pub kind: PendingCoinFlipKind,
}

/// CR 614.12b + CR 614.1c + CR 614.13: Resume state for a multi-target
/// `ChangeZone` resolution loop paused when one of the moving objects
/// triggered a per-permanent replacement choice (shock-land "pay 2 life?",
/// check-land reveal prompt, Sutured Ghoul / Vesuva copy-as-enters, any
/// `MayCost`/`Optional` replacement on entering the battlefield).
///
/// The loop in `change_zone::resolve` (and the analogous `EffectZoneChoice`
/// multi-card loop in `engine_resolution_choices`) calls `execute_zone_move`
/// per object. When one returns `ZoneMoveResult::NeedsChoice(player)`, the
/// handler must set `waiting_for = ReplacementChoice` and return — leaving
/// the remaining objects unmoved. Without this resume primitive, those
/// remaining objects are silently dropped (issue #535: Skyshroud Claim
/// chooses two shock lands; only the first ever entered the battlefield).
///
/// The struct stashes the per-iteration context (`ChangeZoneIterationCtx`)
/// plus the unprocessed object ids; `drain_pending_change_zone_iteration`
/// (in `effects/mod.rs`) re-enters the loop after each `ReplacementChoice`
/// resolves. Drained BEFORE `pending_repeat_iteration` because the outer
/// `repeat_for` loop may have stashed a chain that contains this inner
/// ChangeZone iteration.
///
/// Mirrors `PendingRepeatIteration`'s stash-and-drain shape; the only new
/// fields are the captured ChangeZone parameters needed to resume identically
/// to the live `resolve` path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingChangeZoneIteration {
    pub remaining: Vec<ObjectId>,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub origin: Option<crate::types::zones::Zone>,
    pub destination: crate::types::zones::Zone,
    pub enter_transformed: bool,
    #[serde(
        default,
        with = "crate::types::zones::etb_tap_bool_compat",
        skip_serializing_if = "EtbTapState::is_unspecified"
    )]
    pub enter_tapped: EtbTapState,
    /// CR 110.2a: Resolved-once controller override on ETB. `Some(pid)`
    /// routes the object to `pid`. `None` leaves the object under its
    /// owner's control. Resolved from `Effect::ChangeZone.enters_under`
    /// at resolver entry, so the carrier never re-evaluates a `ControllerRef`
    /// across an interactive pause.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enters_under_player: Option<PlayerId>,
    pub enters_attacking: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enter_with_counters: Vec<(crate::types::counter::CounterType, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration: Option<crate::types::ability::Duration>,
    pub track_exiled_by_source: bool,
    /// CR 608.2c: Optional mass-move count carried by `ChangeZoneAll` resume
    /// paths so a paused Aura host choice still leaves "that many" chained
    /// effects with the same count the uninterrupted mass path records.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub moved_count: Option<i32>,
    /// CR 708.2a + CR 708.3: face-down entry profile carried across a
    /// replacement-ordering / as-enters pause so a paused-then-resumed
    /// face-down return (Yedora's dying creatures → face-down Forest lands)
    /// still applies the profile on resume. `None` = normal face-up entry.
    /// Mirrors the `enter_tapped`/`enter_transformed`/`enters_under_player`
    /// carry-through pattern on this same struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_down_profile: Option<crate::types::ability::FaceDownProfile>,
    pub effect_kind: crate::types::ability::EffectKind,
}

/// CR 707.2 + CR 614.1a + CR 616.1: Resume state for `CopyTokenOf` when a
/// copy-token `CreateToken` event pauses for replacement ordering/optional
/// choice. The currently-paused source is stored in `pending_replacement`; this
/// record carries already-created token ids and the remaining copy sources so
/// `handle_replacement_choice` can continue the same resolver after the chosen
/// replacement applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCopyTokenBatch {
    pub owner: PlayerId,
    pub copy: Box<CopyTokenSpec>,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCopyTokenResolution {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_ids: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "VecDeque::is_empty")]
    pub remaining: VecDeque<PendingCopyTokenBatch>,
    pub effect_kind: EffectKind,
    pub source_id: ObjectId,
}

/// CR 608.2c + CR 107.1c: Resume state for a "repeat this process" loop
/// (`RepeatContinuation`) paused when an iteration's process entered an
/// interactive `WaitingFor` state.
///
/// The loop in `resolve_ability_chain` cannot set the repeat prompt while a
/// player choice from the iteration is still unresolved. It stashes this
/// struct and `drain_pending_continuation` re-checks it once the choice (and
/// any chained continuation) drains.
///
/// - `ability` — the loop ability, retaining `repeat_until` so the drain knows
///   which continuation mode to apply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRepeatUntil {
    pub ability: Box<crate::types::ability::ResolvedAbility>,
}

/// CR 701.55d: Remaining players queued to face the same resolution-time
/// branch choice after the current chosen branch finishes resolving.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingChooseOneOf {
    pub controller: PlayerId,
    pub source_id: ObjectId,
    pub branches: Vec<AbilityDefinition>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub parent_targets: Vec<TargetRef>,
    #[serde(default)]
    pub context: super::ability::SpellContext,
    pub remaining_players: Vec<PlayerId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterMoveChoice {
    pub destination_id: ObjectId,
    pub counter_type: CounterType,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CounterCostChoice {
    pub object_id: ObjectId,
    pub counter_type: CounterType,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCounterMove {
    pub actor: PlayerId,
    pub source_id: ObjectId,
    pub destination_id: ObjectId,
    pub counter_type: CounterType,
    pub remove_count: u32,
    pub add_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCounterMoveQueue {
    pub remaining: Vec<PendingCounterMove>,
    pub effect_kind: EffectKind,
    pub source_id: ObjectId,
}

/// CR 603.10a + CR 616.1: The not-yet-delivered tail of a simultaneous
/// zone-move batch, parked when a per-object `Moved` replacement surfaces a
/// replacement choice mid-batch (e.g. two simultaneously-applicable
/// graveyard→exile redirects — Rest in Peace + Leyline of the Void — racing on
/// the same object). Drained by `zone_pipeline::drain_pending_batch_deliveries`
/// from the replacement-choice resume path after the chosen event delivers; the
/// drain re-parks when the next object surfaces its own choice.
///
/// Shared by every batch flow that delivers many objects to one destination
/// through the pipeline (mill: library→graveyard/exile/hand; mass bounce:
/// battlefield→hand/library; reveal-until library-bottom placement). Serializes
/// as a plain struct (the type name never appears on the wire), so the rename
/// from the original mill-only `PendingMillDeliveries` is wire-transparent; the
/// field-name alias on the holding `GameState` field carries the only readable
/// name change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBatchDeliveries {
    /// Objects whose per-object zone move has not yet been delivered.
    pub remaining: Vec<ObjectId>,
    /// The batch destination zone (graveyard for mill by default; hand for mass
    /// bounce; exile/library for variants).
    pub destination: Zone,
    /// CR 400.7 attribution source for the rebuilt tail requests. `None` means
    /// each object anchors itself (the mill idiom,
    /// `ZoneMoveRequest::effect(obj, dest, obj)`); `Some` carries a shared
    /// ability source (the seek idiom) so battlefield entries record
    /// `entered_via_ability_source` and exile links key off the right source
    /// across the pause boundary. Batch-uniform by the same design that makes
    /// `destination` batch-wide (single-destination batches; per-card
    /// heterogeneity is a flagged design extension, not forced in).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<ObjectId>,
    /// CR 614.1c tap-state re-seeded on each rebuilt tail request (the seek
    /// `enter_tapped` mod survives the pause boundary).
    #[serde(default, skip_serializing_if = "EtbTapState::is_unspecified")]
    pub enter_tapped: EtbTapState,
    /// Exile-link tracking re-seeded on each rebuilt tail request.
    #[serde(default)]
    pub exile_tracking: ZoneDeliveryExileTracking,
    /// Library placement re-seeded on each rebuilt tail request. `None` means a
    /// plain library move, which uses the delivery tail's normal library shuffle;
    /// `Some(Bottom/Top/NthFromTop)` preserves explicit placement batches such as
    /// reveal-until rest piles across CR 616.1 pauses.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_placement: Option<LibraryPosition>,
    /// Post-batch cleanup that MUST run exactly once after every object in the
    /// batch has been delivered (including across a CR 616.1 pause/resume). The
    /// batch caller stashes it when the batch pauses mid-pile; the drain path
    /// (`zone_pipeline::drain_pending_batch_deliveries`) runs it the moment the
    /// tail empties without re-parking. `None` for batch flows whose only effect
    /// is the moves themselves (mill, mass bounce). See [`BatchCompletion`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<BatchCompletion>,
}

/// CR 701.25a / manifest dread: the post-loop cleanup a rest-pile batch must run
/// once its graveyard pile has been delivered. These flows partition a looked-at
/// pile into a graveyard "rest" pile (delivered through the simultaneous-move
/// batch so per-card `Moved` redirects fire — Rest in Peace / Leyline of the Void
/// class) and a "kept" remainder whose placement/marker cleanup happens after the
/// whole pile lands. Because a per-card redirect can pause the batch (two
/// simultaneous redirects on one card need a CR 616.1 ordering choice), the
/// cleanup cannot run inline at the end of the loop — it would run before the
/// paused tail finished, then never again. Stashing it as typed data on
/// [`PendingBatchDeliveries`] (not a closure) lets the drain run it exactly once
/// on true completion, mirroring the `PendingCounterPostAction` continuation
/// pattern.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchCompletion {
    /// CR 701.25a: After the surveil rest pile reaches the graveyard, the kept
    /// cards rest on top of the player's library in the chosen order
    /// (`top_cards[0]` becomes the topmost card).
    SurveilKeepOnTop {
        player: PlayerId,
        top_cards: Vec<ObjectId>,
    },
    /// Manifest dread: after the non-manifested cards reach the graveyard, clear
    /// the reveal markers on every looked-at card.
    ManifestDreadCleanup {
        player: PlayerId,
        revealed: Vec<ObjectId>,
    },
    /// CR 303.4f / CR 616.1 + CR 701.20b: A reveal-until / dig kept card routed
    /// onto the battlefield paused on an as-enters choice (aura host pick or a
    /// replacement-ordering prompt) before the unkept "rest pile" was moved.
    /// Defer the rest-pile move + reveal-marker cleanup onto the parked batch
    /// tail so it runs exactly once after the kept card's entry resolves —
    /// otherwise the rest cards strand in the library (the early-`return` bug).
    RevealRestPile {
        /// The player whose continuation drains after the pile lands.
        player: PlayerId,
        /// Unkept cards to move once the kept card finishes entering.
        rest_cards: Vec<ObjectId>,
        /// Where the rest pile goes (`Library` => bottom in a reposition, else
        /// the destination zone).
        rest_destination: Zone,
        /// CR 701.20b: reveal markers to clear once the cards have moved (the
        /// kept card plus the misses).
        clear_markers: Vec<ObjectId>,
        /// Dig only: `Some(kept)` publishes the kept cards as a fresh tracked set
        /// and wires them as the continuation's targets (Zimone's Experiment
        /// class). `None` for reveal-until, which has no tracked-set sub-ability.
        publish_tracked_set: Option<Vec<ObjectId>>,
        /// `Some(source_id)` emits `EffectResolved { RevealUntil, source_id }`
        /// before draining the continuation — the direct `reveal_until::resolve`
        /// path (no kept-choice) emits it inline at the end, so the deferred path
        /// must too. `None` for the kept-choice / dig paths, which emit their own
        /// `EffectResolved` before the pause (or rely on the continuation).
        emit_reveal_until_resolved: Option<ObjectId>,
    },
    /// CR 610.3 + CR 614.1c: An "exile until ~ leaves" return (Banisher Priest /
    /// Fiend Hunter / Oblivion Ring class) routed its exiled cards back to the
    /// battlefield through the simultaneous-move batch so the delivery tail seeds
    /// enters-with-counters statics. A returned creature can pause on an
    /// as-enters / aura-host choice; defer the exile-link bookkeeping cleanup
    /// (`UntilSourceLeaves` links are spent once their card returns) onto the
    /// parked batch tail so the links are dropped exactly once after the whole
    /// return pile lands — not before a paused card finishes returning.
    RemoveExileLinks {
        /// The exiled-card ids whose `UntilSourceLeaves` links are consumed by
        /// this return and must be retained out of `state.exile_links`.
        returned_ids: Vec<ObjectId>,
    },
    /// CR 702.49 + CR 616.1: A ninja entering via ninjutsu paused on a
    /// battlefield-entry replacement-ordering choice (two co-played external
    /// enter-tapped effects — Authority of the Consuls + Imposing Sovereign
    /// class collide on the entry's tap field). The post-entry ninjutsu work —
    /// the CR 702.49 cast-variant provenance tag, the CR 702.49c
    /// tapped-and-attacking combat placement (no `AttackersDeclared`), and the
    /// CR 702.49a `NinjutsuActivated` trigger event — cannot run before the
    /// entry delivers; defer it onto the parked batch tail so the drain runs
    /// it exactly once after the entry resolves.
    NinjutsuPlacement {
        player: PlayerId,
        ninjutsu_obj_id: ObjectId,
        cast_variant: CastVariantPaid,
        defending_player: PlayerId,
        attack_target: AttackTarget,
    },
    /// CR 701.51 + CR 616.1: An Attraction being opened paused on a
    /// battlefield-entry replacement-ordering choice (Kismet / Frozen Aether
    /// class enter-tapped effects). Defer the paused Attraction's open
    /// bookkeeping (`in_attraction_deck` clear + `AttractionOpened`) and the
    /// remaining opens of the same instruction onto the parked batch tail —
    /// the remaining opens may themselves pause and re-defer through this same
    /// completion.
    AttractionOpenRemainder {
        player: PlayerId,
        object_id: ObjectId,
        remaining: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEffectResolved {
    pub kind: EffectKind,
    pub source_id: ObjectId,
    #[serde(default)]
    pub resolution_event: PendingEffectResolutionEvent,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub post_actions: Vec<PendingCounterPostAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_action: Option<PendingPlayerAction>,
}

impl PendingEffectResolved {
    pub fn new(kind: EffectKind, source_id: ObjectId) -> Self {
        Self {
            kind,
            source_id,
            resolution_event: PendingEffectResolutionEvent::Emit,
            post_actions: Vec::new(),
            player_action: None,
        }
    }

    pub fn with_post_actions(
        kind: EffectKind,
        source_id: ObjectId,
        post_actions: Vec<PendingCounterPostAction>,
    ) -> Self {
        Self {
            kind,
            source_id,
            resolution_event: PendingEffectResolutionEvent::Emit,
            post_actions,
            player_action: None,
        }
    }

    pub fn with_post_actions_without_effect(
        kind: EffectKind,
        source_id: ObjectId,
        post_actions: Vec<PendingCounterPostAction>,
    ) -> Self {
        Self {
            kind,
            source_id,
            resolution_event: PendingEffectResolutionEvent::Suppress,
            post_actions,
            player_action: None,
        }
    }

    pub fn with_player_action(
        kind: EffectKind,
        source_id: ObjectId,
        player_id: PlayerId,
        action: PlayerActionKind,
    ) -> Self {
        Self {
            kind,
            source_id,
            resolution_event: PendingEffectResolutionEvent::Emit,
            post_actions: Vec::new(),
            player_action: Some(PendingPlayerAction { player_id, action }),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PendingEffectResolutionEvent {
    #[default]
    Emit,
    Suppress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingPlayerAction {
    pub player_id: PlayerId,
    pub action: PlayerActionKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingCounterPostAction {
    EmitEffectResolved {
        kind: EffectKind,
        source_id: ObjectId,
    },
    RecordPlayerAction {
        player_id: PlayerId,
        action: PlayerActionKind,
    },
    AddSubtype {
        object_id: ObjectId,
        subtype: String,
    },
    InjectPredefinedTokenAbilities {
        object_id: ObjectId,
    },
    FinalizeTokenEntry {
        object_id: ObjectId,
        name: String,
        attach_to: Option<AttachTarget>,
        sacrifice_at: Option<Duration>,
        source_id: ObjectId,
        controller: PlayerId,
    },
    ContinueTokenCreation {
        owner: PlayerId,
        spec: Box<TokenSpec>,
        enter_tapped: EtbTapState,
        remaining_count: u32,
    },
    FinalizeCopyTokenEntry {
        object_id: ObjectId,
        name: String,
        enters_attacking: bool,
        source_id: ObjectId,
        controller: PlayerId,
    },
    ContinueCopyTokenCreation {
        owner: PlayerId,
        copy: Box<CopyTokenSpec>,
        enter_tapped: EtbTapState,
        enter_with_counters: Vec<(CounterType, u32)>,
        remaining_count: u32,
    },
    ApplyCopyTokenModificationsAndFinalize {
        object_id: ObjectId,
        name: String,
        enters_attacking: bool,
        source_id: ObjectId,
        controller: PlayerId,
        remaining_modifications: Vec<ContinuousModification>,
    },
    ClearPendingEtbCounters {
        object_id: ObjectId,
    },
    ContinueZoneDeliveryTail {
        object_id: ObjectId,
        from: Zone,
        to: Zone,
        cause: Option<ObjectId>,
        source_id: Option<ObjectId>,
        duration: Option<Duration>,
        exile_tracking: ZoneDeliveryExileTracking,
        /// Who drains `post_replacement_continuation` when this deferred tail
        /// finally runs (CR 614.12a). `#[serde(default)]` = `DeliveryTail`,
        /// matching every record minted before the field existed.
        #[serde(default)]
        drain: PostReplacementDrainOwner,
    },
    RecordStationed {
        spacecraft_id: ObjectId,
        creature_id: ObjectId,
        counters_added: u32,
    },
    MarkMonstrous {
        object_id: ObjectId,
    },
    MarkRenowned {
        object_id: ObjectId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum ZoneDeliveryExileTracking {
    #[default]
    None,
    TrackBySource,
}

/// CR 614.12a + CR 616.1: Which layer drains `post_replacement_continuation`
/// after a post-replacement zone delivery (Phase-B divergence reconciliation,
/// PLAN §7). The replacement-choice resume path historically drained the
/// continuation in its own epilogue — WITH the spell-resolution ctx and with
/// `post_replacement_source` cleared for zone changes — while the shared
/// delivery tail drains it ctx-less without the clear. Parameterizing the tail
/// (instead of keeping two divergent delivery copies) lets the resume path
/// route through the shared `deliver` machinery while its epilogue keeps
/// exclusive ownership of the drain.
/// CR 730.3e (second clause): the card-component routing override for a TOKEN
/// merged permanent leaving the battlefield under a card-scoped (`NonToken`)
/// `Moved` redirect. The token survivor and token components are put into
/// `default_dest` (the pre-replacement appropriate zone); the card components
/// are "moved by the replacement effect" to `card_dest`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedCardComponentRoute {
    /// Where the token survivor + token components go — the pre-replacement
    /// appropriate zone (a card-scoped redirect did not match the token
    /// survivor, so its own move is unredirected).
    pub default_dest: Zone,
    /// Where the card components go — the destination the card-scoped redirect
    /// resolved to in the single component-aware consult.
    pub card_dest: Zone,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum PostReplacementDrainOwner {
    /// The shared delivery tail (`apply_zone_delivery_tail`) drains the
    /// continuation ctx-less — every direct pipeline delivery (effect moves,
    /// stack resolution, land play, destroy/sacrifice lowering).
    #[default]
    DeliveryTail,
    /// The caller's epilogue owns the drain; the tail skips it. Used by
    /// `engine_replacement::handle_replacement_choice`, whose post-`Execute`
    /// epilogue drains with the spell-resolution ctx and clears
    /// `post_replacement_source` for zone changes (CR 614.12a ordering:
    /// `apply_pending_spell_resolution` runs before the drain there).
    CallerEpilogue,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingCounterAddition {
    Object {
        actor: PlayerId,
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
    },
    Player {
        actor: PlayerId,
        player_id: PlayerId,
        counter_kind: PlayerCounterKind,
        count: u32,
    },
    Energy {
        actor: PlayerId,
        player_id: PlayerId,
        count: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCounterAdditionQueue {
    pub remaining: Vec<PendingCounterAddition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<PendingEffectResolved>,
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

/// CR 702.50a: A rest-of-game Epic effect, created when an Epic spell resolves.
/// Held in `GameState::epic_effects` (never purged) and used to (a) lock its
/// controller out of casting spells (CR 702.50b) and (b) synthesize an
/// `Effect::EpicCopy` triggered ability at the beginning of each of the
/// controller's upkeeps that copies the spell minus its epic ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EpicEffect {
    /// The player who controlled the resolved Epic spell — locked from casting
    /// and the recipient of the recurring upkeep copies.
    pub controller: PlayerId,
    /// The resolved Epic card (now in the graveyard) whose characteristics each
    /// upkeep copy clones. `None`-equivalent handling lives in the resolver:
    /// if the object has left the game the copy is a no-op (last-known-info).
    pub prototype_id: ObjectId,
    /// Snapshot of the Epic spell's resolved ability, replayed as the body of
    /// each upkeep copy.
    pub spell: Box<ResolvedAbility>,
}

fn default_copy_retarget_effect_kind() -> EffectKind {
    EffectKind::CopySpell
}

/// CR 601.2g-h: Whether the engine may auto-pay an unambiguous spell mana cost
/// or must pause after announcement so the player can activate mana abilities
/// manually before committing payment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastPaymentMode {
    #[default]
    Auto,
    Manual,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCast {
    pub object_id: ObjectId,
    pub card_id: CardId,
    pub ability: ResolvedAbility,
    pub cost: ManaCost,
    /// CR 601.2f: The tax-inclusive base mana cost captured at announcement,
    /// BEFORE any cost reductions/increases or {X} concretization. Lets the
    /// full concrete cost be recomputed from scratch for any chosen X with
    /// floors applied LAST (`concrete_cost_for_x`). `None` for activated /
    /// mana-ability casts and for legacy/in-flight saved games — those paths
    /// fall back to flooring the already-reduced `cost`. `NoCost` is a real
    /// base, so `Option` is the only safe sentinel.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_cost: Option<ManaCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_cost: Option<AbilityCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_ability_index: Option<usize>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// How this spell was cast — threads through the casting pipeline to finalize_cast.
    #[serde(default)]
    pub casting_variant: CastingVariant,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_timing_permission: Option<crate::types::ability::CastTimingPermission>,
    /// CR 601.2d: When set, after target selection the caster must distribute this
    /// resource (damage, counters, life) among the chosen targets via DistributeAmong.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub distribute: Option<DistributionUnit>,
    /// CR 601.2a + CR 601.2i: Zone the spell was in before announcement. The spell
    /// moves to the stack at announcement time; if the cast is aborted at any step
    /// (modal/target/cost), the object is returned to this zone and all choices
    /// are reversed. Defaults to `Zone::Hand` — the common case — so legacy
    /// `PendingCast::new` callers (mana abilities, activated abilities) don't
    /// need updating.
    #[serde(default = "default_origin_zone")]
    pub origin_zone: Zone,
    /// CR 601.2b + CR 702.33b/c: Additional-cost declaration still being
    /// walked after one sub-cost has been accepted. Used for independent
    /// kicker costs and multikicker loops.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost_flow: Option<AdditionalCost>,
    /// CR 601.2f/h: Required additional cost to pay after a multi-step
    /// optional additional-cost flow completes. Used when a target-dependent
    /// static imposes a required non-mana cost on a spell that is also walking
    /// Kicker/Multikicker choices in `additional_cost_flow`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_required_additional_cost: Option<AbilityCost>,
    /// CR 601.2b/f + CR 113.2c: Queue of independent non-kicker additional-cost
    /// keyword instances still being announced for this cast. Kicker keeps its
    /// existing `additional_cost_flow` path because it already records
    /// per-variant payments in `SpellContext::kickers_paid`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_cost_queue: Vec<AdditionalCostInstance>,
    /// CR 601.2b + CR 702.48c: Source of the currently pending additional-cost
    /// component. This disambiguates same-shaped costs when a later object
    /// selection resumes payment.
    #[serde(default)]
    pub additional_cost_source: SpellCostSource,
    /// CR 601.2b + CR 700.2a: Modal spells with kicker-dependent mode caps
    /// announce kicker intent before choosing modes, but pay those costs later
    /// in the normal cost-payment step.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_modal_choice: Option<ModalChoice>,
    /// CR 601.2b/c + CR 702.33g: Spells with kicker-dependent target sets
    /// announce kicker intent before targets, then pay declared kicker costs
    /// during the normal cost-payment step after targets are chosen.
    #[serde(default)]
    pub deferred_target_selection: bool,
    /// CR 700.2 + CR 601.2b: Indices of the modes chosen during the cast's
    /// modal step, sorted ascending to match `build_chained_resolved` /
    /// `build_target_slots_labelled`. Persisted so a deferred target-selection
    /// step (after X or an additional cost) can re-build per-slot mode labels
    /// for the targeting UI. Empty for non-modal casts.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_modes: Vec<usize>,
    /// CR 601.2b: Set to `true` once an optional additional cost (e.g. Casualty)
    /// that was deferred before target selection has been decided (paid or declined).
    /// Guards `finish_pending_cast_cost_or_pay` from re-presenting the same cost
    /// after the player selects targets.
    #[serde(default)]
    pub additional_cost_decided: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_kickers_to_pay: Vec<KickerVariant>,
    /// CR 702.33f: Non-repeatable kicker options the player has declined in
    /// the current casting announcement. Paid options are tracked on
    /// `ability.context.kickers_paid`; this list only prevents re-prompting
    /// declined sibling kickers.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declined_kickers: Vec<KickerVariant>,
    /// CR 702.51c: Creatures tapped to pay this spell's convoke cost.
    /// Collected during `WaitingFor::ManaPayment` and copied onto the spell
    /// object when the cast is finalized so "creatures that convoked it"
    /// quantities can resolve later.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convoked_creatures: Vec<ObjectId>,
    /// CR 601.2i + CR 722.3c: Optional source permanent to re-mark as
    /// prepared if this cast is cancelled and rolled back. Used by the
    /// prepared-copy special action to restore pre-cast state.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cancel_restore_prepared_source: Option<ObjectId>,
    #[serde(default)]
    pub payment_mode: CastPaymentMode,
    /// CR 702.132a: Assist offer/decision for this cast. `NotOffered` until the
    /// "choose another player" step is presented (so re-entering
    /// `enter_payment_step` doesn't re-offer); `Committed` carries the helper and
    /// the generic amount they will pay at `finalize_cast`.
    #[serde(default)]
    pub assist_state: AssistState,
}

fn default_origin_zone() -> Zone {
    Zone::Hand
}

/// CR 601.2h + CR 616.1: Resume paying a discard cost after a replacement choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDiscardForCostResume {
    pub player: PlayerId,
    pub pending: PendingCast,
    pub chosen: Vec<ObjectId>,
    /// Index into `chosen` whose discard was paused; that discard completes
    /// during `handle_replacement_choice` before this resume runs.
    pub paused_at_index: usize,
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
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
        }
    }

    pub fn with_payment_mode(mut self, payment_mode: CastPaymentMode) -> Self {
        self.payment_mode = payment_mode;
        self
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
        /// CR 118.12: Carried-through cost from `WaitingFor::UnlessPayment`.
        /// See the matching `WaitingFor::UnlessPayment.cost` doc-comment for
        /// the legacy-shape deserialization contract. Boxed so the
        /// enclosing `ManaAbilityResume` enum stays compact (other variants
        /// are zero-sized or carry only an `Option`).
        #[serde(deserialize_with = "crate::types::ability::deserialize_ability_cost_compat_boxed")]
        cost: Box<AbilityCost>,
        pending_effect: Box<ResolvedAbility>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_description: Option<String>,
        /// CR 118.12a: Carried-through "unless any player pays" poll list — see
        /// `WaitingFor::UnlessPayment.remaining`. Survives the player tapping a
        /// mana ability mid-payment.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining: Vec<PlayerId>,
    },
}

/// CR 605.3b + CR 106.1a: A pre-resolved choice that short-circuits the normal
/// `ChooseManaColor` prompt. Auto-tap sets this when the cost-payment planner
/// has already determined the exact mana to produce; manual activation leaves
/// it `None` so the player is prompted.
///
/// Typed enum (never a bool): `SingleColor` covers the one-color-repeated
/// variants (`AnyOneColor`, `ChoiceAmongExiledColors`), while `Combination`
/// carries the full pre-chosen multi-mana sequence for fixed combinations
/// (`ChoiceAmongCombinations`) and free per-slot choices (`AnyCombination`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ProductionOverride {
    /// The caller picked a single color; every unit of mana the ability
    /// produces becomes this color (mirrors the pre-widening `Option<ManaType>`
    /// semantics).
    SingleColor(ManaType),
    /// The caller picked one complete mana sequence; the ability produces
    /// exactly these mana types in order.
    Combination(Vec<ManaType>),
}

/// CR 608.2d + CR 605.3b: The shape of the prompt surfaced via
/// `WaitingFor::ChooseManaColor`.
/// Typed enum rather than a bool discriminator: the continuation logic is
/// identical (validate choice → produce mana → resume), only the option set
/// differs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ManaChoicePrompt {
    /// Legacy prompt shape: pick one color from the list (Treasure,
    /// City of Brass, Pit of Offerings, `AnyOneColor`).
    SingleColor { options: Vec<ManaType> },
    /// Filter-land prompt: pick one complete multi-mana combination.
    Combination { options: Vec<Vec<ManaType>> },
    /// Spell/effect prompt: pick one mana type for each produced mana unit.
    AnyCombination {
        count: usize,
        options: Vec<ManaType>,
    },
}

/// CR 608.2d + CR 605.3b: Player's answer to a `ManaChoicePrompt`, carried by
/// `GameAction::ChooseManaColor`. Shape mirrors the prompt variant.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ManaChoice {
    SingleColor(ManaType),
    Combination(Vec<ManaType>),
}

/// CR 106.3 + CR 608.2d + CR 605.3b: What resumes after a mana-color choice.
/// Mana abilities and resolving spell/ability effects share the same prompt and
/// response action, but resume through different rules paths.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum ManaChoiceContext {
    ManaAbility(Box<PendingManaAbility>),
    ResolvingEffect(Box<ResolvedAbility>),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingManaAbility {
    pub player: PlayerId,
    pub source_id: ObjectId,
    pub ability_index: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_override: Option<ProductionOverride>,
    pub resume: ManaAbilityResume,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_tappers: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_discards: Vec<ObjectId>,
    /// CR 107.4e + CR 605.3a: Pre-resolved hybrid-color choices for a `Mana` sub-cost
    /// inside an `AbilityCost::Composite` (e.g. filter lands' `{W/U}, {T}` payment).
    /// One entry per hybrid shard, in printed order. `None` means the payment hasn't
    /// been resolved yet; the activation flow either auto-picks (unambiguous pool) or
    /// surfaces `WaitingFor::PayManaAbilityMana` for a genuine choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_mana_payment: Option<Vec<ManaType>>,
    /// CR 107.1c + CR 605.3a: Chosen count for "remove any number of counters"
    /// in a mana-ability cost. The amount is chosen before mana production.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_counter_count: Option<u32>,
    /// CR 117.1 + CR 118.3: Pre-selected objects to exile as part of an
    /// `AbilityCost::Exile { filter: !SelfRef, .. }` mana ability cost. Used
    /// by Food Chain's battlefield exile cost and Titans' Nest's graveyard
    /// exile cost. Empty means the choice has not been made yet; the activation
    /// flow either surfaces `WaitingFor::ExileForManaAbility` or fills this for
    /// deterministic top-of-library costs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_exiled: Vec<ObjectId>,
    /// CR 117.1 + CR 118.3: Pre-selected battlefield permanents to sacrifice
    /// as part of an `AbilityCost::Sacrifice(SacrificeCost::count(!SelfRef, 1)`. Used by
    /// Phyrexian Altar and the broader sacrifice-for-mana-by-property class.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub chosen_sacrificed_battlefield: Vec<ObjectId>,
    /// CR 117.1 + CR 400.7j + CR 608.2k: Public characteristics of the
    /// cost-paid object captured before it leaves its zone. Threaded into
    /// `produce_mana_from_ability` so cost-paid-object quantity refs can
    /// resolve in inline mana ability resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_paid_object: Option<CostPaidObjectSnapshot>,
    /// CR 605.3a: Other identical, choice-free mana sources the controller
    /// could activate for the same `SingleColor` prompt (their other
    /// Treasures, etc.). Computed only when the prompt is `SingleColor` and the
    /// cost resolves with no further player choice. `GameAction::ChooseManaColor`
    /// may bulk-activate up to this many additional sources with the chosen
    /// color. The frontend reads `.len()` to cap its quantity stepper. Empty for
    /// every non-batchable activation (the default).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub batch_siblings: Vec<ObjectId>,
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

/// Lattice tracking which battlefield objects need layer (continuous-effect)
/// re-evaluation. Replaces the old `bool` flag so that a token / conjure / copy
/// entry can request an INCREMENTAL re-derive of only the entering object(s)
/// instead of a full battlefield reset+reapply.
///
/// CR 613.1: continuous effects are evaluated in layer order over the whole
/// board. A full evaluation is always correct; the incremental path is a
/// performance optimization that `flush_layers` only takes when it can prove
/// (per-entered preconditions + a board-wide escalation scan) that re-deriving
/// just the entered objects produces a board state identical to a full pass.
/// `mark_full()` is the conservative escalation any non-entry mutation uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum LayersDirty {
    /// Layers are up to date; nothing to flush.
    #[default]
    Clean,
    /// Only these objects entered the battlefield since the last flush and no
    /// other layer-affecting mutation occurred. Candidate for the incremental
    /// fast path.
    EnteredObjects(HashSet<ObjectId>),
    /// A full battlefield re-evaluation is required.
    Full,
}

impl LayersDirty {
    /// Constructor used as the `#[serde(default)]` for the field: deserialized
    /// snapshots conservatively rebuild fully on first flush.
    pub fn full() -> Self {
        Self::Full
    }

    pub fn is_dirty(&self) -> bool {
        !matches!(self, Self::Clean)
    }

    pub fn mark_full(&mut self) {
        *self = Self::Full;
    }

    pub fn mark_entered(&mut self, id: ObjectId) {
        match self {
            Self::Full => {}
            Self::Clean => *self = Self::EnteredObjects(HashSet::from([id])),
            Self::EnteredObjects(s) => {
                s.insert(id);
            }
        }
    }
}

/// Cache key for the source-level enabling-condition truth of a single
/// CONTINUOUS static ability, used by the incremental layer-flush
/// truth-delta short-circuit (`game/layers.rs`).
///
/// CR 611.3a + CR 611.3b: a static-ability continuous effect isn't "locked
/// in"; it applies at all times the source is on the battlefield, re-evaluated
/// against whatever its text indicates. When an object enters, an incremental
/// flush re-derives only the entered objects. If a pre-existing source's
/// population-sensitive, SOURCE-LEVEL (non-recipient-context) enabling
/// condition would change truth, pre-existing recipients must be re-derived —
/// so the flush must escalate to a full pass. This key indexes the recorded
/// BEFORE truth so the consult can compare against a freshly-recomputed AFTER.
///
/// `def_index` indexes the LIVE post-layer `static_definitions` vec
/// (`iter_all().enumerate()`), NOT `base_static_definitions`. The refresh and
/// the consult both observe the identical live vec for pre-existing sources, so
/// the index aligns (see invariant 5 in the plan / the consult below).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct StaticGateKey {
    pub source: ObjectId,
    pub def_index: usize,
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
    /// CR 115.1 + CR 601.2c: Object targets must be controlled by different players.
    DifferentObjectControllers,
    /// CR 202.3 + CR 601.2c: the chosen target set's combined mana value must
    /// satisfy `comparator` against `value`. `value` is a `QuantityExpr` (not
    /// `i32` like `SearchSelectionConstraint::TotalManaValue`) because the bound
    /// is the dynamic where-X die result (`EventContextAmount`). NOT unified with
    /// `SearchSelectionConstraint::TotalManaValue` — different CR section
    /// (CR 115.1 / CR 601.2c target declaration vs CR 701.23 search-set) and a
    /// different value type.
    TotalManaValue {
        comparator: Comparator,
        value: QuantityExpr,
    },
}

/// CR 508.1d + CR 509.1c: Which combat step a `WaitingFor::CombatTaxPayment` belongs to.
///
/// Drives the resume branch after the tax decision — on accept, the engine submits the
/// stored attacker / blocker declaration; on decline, the engine filters the taxed
/// creatures out of that declaration and submits the remainder.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CombatTaxContext {
    Attacking,
    Blocking,
}

/// CR 508.1d + CR 509.1c: The declaration that is paused awaiting a combat-tax
/// decision. Keyed by `CombatTaxContext` — the engine resumes the matching
/// variant on `GameAction::PayCombatTax`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CombatTaxPending {
    Attack {
        attacks: Vec<(ObjectId, crate::game::combat::AttackTarget)>,
        /// CR 702.22c: attacking-band declarations captured alongside the
        /// attacks so the resume path (after combat-tax payment) stamps
        /// `band_id` via `declare_attackers_with_bands` and groups the band for
        /// blocking (CR 702.22h).
        bands: Vec<Vec<ObjectId>>,
    },
    Block {
        assignments: Vec<(ObjectId, ObjectId)>,
    },
}

/// CR 107.4f + CR 601.2h: Which legal payments a single Phyrexian shard offers to the
/// caster. Computed from the mana pool state (Phyrexian color availability) combined with
/// the caster's life total and CantLoseLife status (CR 118.3 + CR 119.8).
///
/// The engine pauses at `WaitingFor::PhyrexianPayment` whenever any shard would deduct
/// life — both `ManaOrLife` (player explicitly picks mana vs life) and `LifeOnly` (life
/// is the only remaining payment route; player confirms or cancels via `CancelCast`).
/// Only `ManaOnly` shards auto-resolve without surfacing the prompt, since they have no
/// life consequence (issue #704: silent life deduction violated CR 601.2h's right to
/// refuse the cast).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShardOptions {
    /// Both mana and 2 life are legal payments; player must choose.
    ManaOrLife,
    /// Only mana is legal (insufficient life or CR 119.8 CantLoseLife lock).
    ManaOnly,
    /// Only 2 life is legal (no mana of the shard's color available, given restrictions).
    LifeOnly,
}

/// CR 107.4f + CR 601.2f: The caster's resolved choice for one Phyrexian shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ShardChoice {
    /// Pay one mana of the shard's color (or either component color for hybrid-Phyrexian).
    PayMana,
    /// Pay 2 life.
    PayLife,
}

/// CR 107.4f: Per-shard payment context surfaced to the UI during `WaitingFor::PhyrexianPayment`.
///
/// `shard_index` identifies the shard's position within the cost's `shards` vector so that
/// the resume handler can align `Vec<ShardChoice>` to the shards that actually need a choice.
/// `color` is the printed shard color (one color for plain Phyrexian, one representative for
/// hybrid-Phyrexian display — the full hybrid routing happens inside payment).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhyrexianShard {
    pub shard_index: usize,
    pub color: ManaColor,
    pub options: ShardOptions,
}

/// Per-player deck pool — registered (initial) and current (live) card
/// lists for main deck, sideboard, and commander zone.
///
/// All six `Vec<DeckEntry>` fields are wrapped in `Arc<Vec<_>>` so
/// `GameState::clone()` shares the underlying deck slice via refcount
/// bump instead of deep-cloning every card's `CardFace` (and its nested
/// `Vec<AbilityDefinition>`) on every AI search-node clone. Mutations
/// (shuffle, draw-from-library, tutor removal) go through `Arc::make_mut`
/// for copy-on-write semantics. Subsequent mutations on a unique-refcount
/// Arc are in-place — only the first mutation of a shared Arc allocates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct PlayerDeckPool {
    pub player: PlayerId,
    pub registered_main: std::sync::Arc<Vec<DeckEntry>>,
    pub registered_sideboard: std::sync::Arc<Vec<DeckEntry>>,
    pub current_main: std::sync::Arc<Vec<DeckEntry>>,
    pub current_sideboard: std::sync::Arc<Vec<DeckEntry>>,
    #[serde(default)]
    pub registered_commander: std::sync::Arc<Vec<DeckEntry>>,
    #[serde(default)]
    pub current_commander: std::sync::Arc<Vec<DeckEntry>>,
    /// Oathbreaker RC: registered and current signature spell entries.
    /// Empty for all non-Oathbreaker formats. Mirrors the commander Arc pair
    /// so between-games persistence works correctly.
    #[serde(default)]
    pub registered_signature_spell: std::sync::Arc<Vec<DeckEntry>>,
    #[serde(default)]
    pub current_signature_spell: std::sync::Arc<Vec<DeckEntry>>,
    /// The declared bracket tier for this player's deck. Used by the AI to
    /// determine whether cEDH-specific policies apply (Phase 5 `ComboLinePolicy`,
    /// Phase 6 `CedhKeepablesMulligan`). Defaults to `Core` for backward
    /// compatibility with saved states and test fixtures that omit the field.
    #[serde(default)]
    pub bracket_tier: CommanderBracketTier,
}

/// CR 400.11/400.11a/400.11b: Tracks sideboard cards brought into this game
/// without mutating the between-games sideboard partition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideGameCardUse {
    pub player: PlayerId,
    pub sideboard_index: usize,
    pub count: u32,
}

/// CR 400.11 + CR 406.3: A discriminated source for one outside-game selection.
/// Sideboard entries (the wishboard pool) and face-up exile cards (the Karn /
/// Coax wishboard return pool) are surfaced through one choice list so the
/// caster picks across both pools in a single decision.
///
/// The size delta between the two variants (`Sideboard` carries a full
/// `CardFace` so the UI can render the wishboard card without a sideboard
/// lookup; `FaceUpExile` holds only an `ObjectId`) is intentional —
/// `OutsideGameChoiceEntry` lists are short-lived (one entry per offered
/// candidate while a single `WaitingFor::OutsideGameChoice` is active) and
/// never collected by the million, so the asymmetry doesn't warrant boxing
/// every CardFace through a heap indirection.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum OutsideGameChoiceSource {
    /// CR 400.11a: A card in the player's sideboard.
    Sideboard {
        sideboard_index: usize,
        card: crate::types::card::CardFace,
    },
    /// CR 406.3: A face-up card the player owns in the exile zone.
    FaceUpExile { object_id: ObjectId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OutsideGameChoiceEntry {
    pub source: OutsideGameChoiceSource,
    /// Remaining copies eligible (sideboard: copies not yet brought in; exile: 1).
    #[serde(default = "default_one_u32")]
    pub count: u32,
    /// Display name for UI; mirrors the underlying card / object's printed name.
    pub name: String,
}

fn default_one_u32() -> u32 {
    1
}

/// CR 103.6: A beginning-of-game ability waiting to resolve after mulligans.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBeginGameAbility {
    pub ability: ResolvedAbility,
}

/// CR 103.5: Per-player state during the simultaneous mulligan decision phase.
/// One entry per player who has not yet declared "keep".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MulliganDecisionEntry {
    pub player: PlayerId,
    pub mulligan_count: u8,
}

/// CR 103.5: Per-player state during the simultaneous bottom-cards phase.
/// One entry per player who must put cards on the bottom of their library.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MulliganBottomEntry {
    pub player: PlayerId,
    pub count: u8,
}

/// CR 103.5 / TL:R 906.6a: Why a player is bottoming cards from an opening
/// hand before the normal mulligan-decision step.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum OpeningHandBottomReason {
    TinyLeadersMultiCommander,
}

/// CR 603.3b: Display payload for one collected-but-not-yet-stacked trigger
/// awaiting its controller's ordering choice. Engine-derived so the filtered
/// state snapshot (multiplayer) and the frontend overlay never re-derive
/// trigger source/description from `state.objects`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTriggerSummary {
    pub source_id: ObjectId,
    pub source_name: String,
    pub description: String,
}

/// CR 603.3b: One controller's group within an in-flight trigger ordering
/// pass. `ordered = true` once the controller has submitted their permutation
/// (or once the group is single-trigger and trivially in final order, or once
/// the controller has been eliminated per CR 800.4a).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriggerOrderGroup {
    pub controller: PlayerId,
    pub triggers: Vec<crate::game::triggers::PendingTriggerContext>,
    pub ordered: bool,
}

/// CR 603.3b: Engine-internal scheduling state for the per-controller ordering
/// pass. `groups` are kept in **placement order** (NAP-group first → AP-group
/// last) — the order they will be concatenated into the dispatch queue once
/// every group is `ordered`. Controllers are *prompted* in choice order
/// (AP-first per CR 101.4), but each chosen permutation is applied only within
/// that controller's fixed placement slot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingTriggerOrder {
    pub groups: Vec<TriggerOrderGroup>,
    /// CR 603.3b + CR 605.4a: Waiting state interrupted by the ordering pass.
    /// Used when triggered mana abilities pause a casting/payment chain for
    /// APNAP ordering; after all ordered triggers are dispatched, the engine
    /// resumes the suspended state instead of falling back to bare Priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_after_ordering: Option<Box<WaitingFor>>,
}

/// CR 101.4 + CR 608.2 (Battlebond friend-or-foe keyword action — no explicit
/// CR section): The "who acts" semantic for a `WaitingFor::VoteChoice` step.
///
/// * `SubjectActs` — the player named by `player` casts the vote for
///   themselves. Classic Council's-dilemma (CR 701.38) is exclusively this
///   case: each voter acts on their own behalf and APNAP iteration changes
///   both subject and actor together.
/// * `Delegated(actor)` — a fixed `actor` casts every vote on behalf of the
///   cycling subjects. The Battlebond friend-or-foe spell controller pins
///   themselves here so `player` cycles through every player in APNAP order
///   while authorization stays with the controller.
///
/// Stored on `WaitingFor::VoteChoice` instead of `Option<PlayerId>` so the
/// "is this delegated?" discriminator is a named sum type with a meaningful
/// pair of variant names, not a boolean-flavored optional. Callers route
/// through [`VoteActor::resolve`] to get the authorized submitter without
/// branching at every call site.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum VoteActor {
    SubjectActs,
    Delegated(PlayerId),
}

impl VoteActor {
    /// Resolve to the player authorized to submit the current
    /// `GameAction::ChooseOption`, given the subject being voted-for or
    /// labeled on this step.
    pub fn resolve(&self, subject: PlayerId) -> PlayerId {
        match self {
            VoteActor::SubjectActs => subject,
            VoteActor::Delegated(actor) => *actor,
        }
    }
}

/// CR 700.3: Identifies one of the two piles produced by a
/// `SeparateIntoPiles` partition. Typed rather than `bool` so the
/// `GameAction::ChoosePile` payload and the engine handler share a
/// self-documenting domain and the parser/AI cannot accidentally swap
/// pile semantics. Pile A is the partitioner's chosen subset; pile B is
/// `eligible \ pile_a`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PileSide {
    A,
    B,
}

/// CR 700.3 + CR 700.3a + CR 700.3d: One subject's completed partition.
/// Both piles are present (CR 700.3a: the partition is exhaustive and
/// disjoint), and either pile may be empty (CR 700.3d). Per CR 700.3b a
/// pile is not a `GameObject` — these are transient `im::Vector` ledgers
/// that live on the `WaitingFor` until the chooser picks a side and the
/// pile sub-effect resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PileResult {
    /// CR 700.3 + CR 101.4: The player whose objects were partitioned.
    pub subject: PlayerId,
    /// CR 700.3a: The partitioner-selected subset.
    pub pile_a: im::Vector<ObjectId>,
    /// CR 700.3a: `eligible \ pile_a`, derived by the partition handler.
    pub pile_b: im::Vector<ObjectId>,
}

/// CR 118.9: Identifies which keyword ability granted an alternative casting
/// cost so the `WaitingFor::AlternativeCastChoice` dispatcher can route to the
/// keyword-specific post-payment handler. The four keywords share a single
/// player decision shape (printed cost vs. alternative cost) but diverge in
/// post-payment semantics — this enum keeps the prompt unified while
/// preserving CR fidelity at resolution.
///
/// Adding a new alternative-cost keyword (e.g., Madness CR 702.35a, Spectacle
/// CR 702.137a) is a compile error at every dispatch site until handled.
#[derive(Debug, Copy, Clone, PartialEq, Eq, Serialize, Deserialize, Hash)]
#[serde(tag = "type")]
pub enum AlternativeCastKeyword {
    /// Custom Warp keyword — exile-at-end-step rider; no CR section.
    Warp,
    /// CR 702.74a: ETB + sacrifice trigger fires when the resolving permanent
    /// was cast for its evoke cost (CR 702.74b).
    Evoke,
    /// CR 702.119a-c: Emerge alternative cost requires sacrificing a creature
    /// while casting and reduces the emerge cost by that creature's mana value.
    Emerge,
    /// CR 702.109a: Cast for the dash cost — the resolving permanent gains haste
    /// and is returned to its owner's hand at the next end step.
    Dash,
    /// CR 702.152a: Cast for the blitz cost — the resolving permanent gains
    /// haste and a dies-draw trigger, and is sacrificed at the next end step.
    Blitz,
    /// CR 702.96a: Spell's text changes "target" to "each" (CR 702.96b-c).
    Overload,
    /// CR 702.103a: Spell becomes an Aura with enchant creature (CR 702.103b).
    Bestow,
    /// CR 702.113a: "If this spell's awaken cost was paid, put N +1/+1 counters
    /// on target land you control. That land becomes a 0/0 Elemental creature
    /// with haste. It's still a land." Paying the awaken cost adds the land
    /// target (CR 702.113b); casting normally adds no target and no rider.
    Awaken,
    /// CR 702.148a-b + CR 612: Paying the cleave cost removes every
    /// square-bracketed span from the spell's text (a text-changing effect).
    Cleave,
    /// CR 702.162a: Cast converted (back face up, CR 712.14a) for the MTMTE cost.
    MoreThanMeetsTheEye,
    /// CR 702.176a: Impending alternative cost paid from hand. On resolution the
    /// permanent enters with N time counters and isn't a creature until the last
    /// is removed. An end-step trigger removes one counter per turn.
    Impending,
    /// CR 702.160a: Prototype alternative cost paid from hand. The resulting
    /// spell/permanent uses the secondary power, toughness, and mana cost
    /// characteristics while it is a creature.
    Prototype,
    /// CR 702.140a: Mutate alternative cost paid from hand. The spell becomes a
    /// mutating creature spell targeting a non-Human creature the caster owns
    /// (CR 702.140a); on resolution it merges with that creature (CR 730) rather
    /// than entering the battlefield, unless the target is illegal (CR 702.140b).
    Mutate,
    /// CR 702.137a: Spectacle alternative cost paid from hand, available only if
    /// an opponent lost life this turn. A pure cost substitution — the spell
    /// resolves normally (no riders); spectacle changes only how the cost is paid.
    Spectacle,
}

/// CR 601.2b: Engine-authored cast-variant option for spells with more than
/// one legal casting permission from the same zone. The frontend displays this
/// data and returns an index; it never reconstructs legality or variants.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CastingVariantChoiceOption {
    pub variant: CastingVariant,
    pub mana_cost: ManaCost,
}

/// CR 118.3 + CR 601.2b + CR 605.3b: Identifies the specific action to take
/// on the objects a player selects while paying a cost.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PayCostKind {
    Discard,
    Sacrifice,
    ReturnToHand,
    /// Exile objects from the specified zone.
    ExileFromZone {
        zone: ExileCostSourceZone,
    },
    /// CR 702.167a/b: Exile craft materials chosen from the union of the
    /// battlefield (permanents you control) and your graveyard. `materials` is
    /// the dual-zone `TargetFilter` the choices were drawn from; the handler
    /// re-validates eligibility against it before exiling.
    ExileMaterials {
        materials: TargetFilter,
    },
    /// Exile objects from any zone (mana-ability exile costs).
    ExileFromManaZone {
        zone: Zone,
    },
    RemoveCounter {
        counter_type: CounterMatch,
        /// CR 118.3 + CR 122.1: number of counters to remove from the one
        /// selected permanent, or from among selected permanents when
        /// `selection` is `AmongObjects`. `WaitingFor::PayCost.count` remains
        /// the number of objects to choose.
        count: u32,
        #[serde(default)]
        selection: CounterCostSelection,
    },
    TapCreatures,
    Behold {
        action: BeholdCostAction,
    },
}

/// CR 601.2b + CR 605.3b: Resumption context after a PayCost choice completes.
/// Determines whether the engine re-enters the spell-casting pipeline or the
/// mana-ability pipeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CostResume {
    Spell {
        #[serde(rename = "Spell")]
        spell: Box<PendingCast>,
    },
    SpellCost {
        #[serde(rename = "Spell")]
        spell: Box<PendingCast>,
        cost: Box<AbilityCost>,
        source: SpellCostSource,
    },
    ManaAbility {
        #[serde(rename = "ManaAbility")]
        mana_ability: Box<PendingManaAbility>,
    },
}

/// CR 601.2h + CR 702.48c: Identifies which spell-cost component a
/// `WaitingFor::PayCost` choice is paying when the same `AbilityCost` shape can
/// come from different rules.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpellCostSource {
    #[default]
    Other,
    Offering,
    Emerge,
}

/// The specific kind of cast offer being presented to the player.
/// Parameterizes `WaitingFor::CastOffer` — all variants share `player: PlayerId`
/// at the outer level; the kind-specific payload lives here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastOfferKind {
    /// CR 715.3a: Player chooses creature face vs Adventure half.
    Adventure {
        object_id: ObjectId,
        card_id: CardId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 702.94a: Miracle triggered ability resolved — cast for miracle cost.
    Miracle {
        object_id: ObjectId,
        cost: super::mana::ManaCost,
    },
    /// CR 702.35a: Madness triggered ability resolved — cast from exile or go to graveyard.
    Madness {
        object_id: ObjectId,
        cost: super::mana::ManaCost,
    },
    /// CR 702.xxx: Paradigm (Strixhaven) — turn-based offer to cast a copy.
    Paradigm { offers: Vec<ObjectId> },
    /// CR 702.85a: Cascade — cast the hit card without paying mana cost or decline.
    Cascade {
        hit_card: ObjectId,
        exiled_misses: Vec<ObjectId>,
        source_mv: u32,
    },
    /// CR 701.57a: Discover — cast the discovered card or put it to hand.
    Discover {
        hit_card: ObjectId,
        exiled_misses: Vec<ObjectId>,
        /// CR 701.57a: "Discover N" — the resulting spell's mana value must be
        /// less than or equal to N for the cast to proceed. Carried on the
        /// offer so the cast-during-resolution path can build the `ManaValue`
        /// gate. `serde(default)` because this is live serialized pause-state.
        #[serde(default)]
        discover_value: u32,
    },
    /// CR 702.60a: Ripple — cast a revealed same-named card without paying its
    /// mana cost, or decline. `hit_card` is the matching revealed card being
    /// offered, `remaining_hits` are other same-named cards from the same reveal
    /// still eligible to cast, and `revealed_misses` are revealed cards that
    /// cannot be cast this way.
    Ripple {
        hit_card: ObjectId,
        remaining_hits: Vec<ObjectId>,
        revealed_misses: Vec<ObjectId>,
    },
    /// CR 608.2g + CR 601.2 + CR 118.9: Interactive free-cast window opened by
    /// `Effect::FreeCastFromZones` (Invoke Calamity). The controller repeatedly
    /// chooses one `candidate` to cast for free (or declines to finish), up to
    /// `remaining_casts` times, while the chosen spells' running total mana
    /// value stays within `remaining_mv_budget`. After each successful cast the
    /// window is re-offered with `remaining_casts` decremented, the budget
    /// reduced, and `candidates` re-filtered to those still affordable.
    FreeCastWindow {
        /// CR 601.2a: Instant/sorcery cards (in the controller's graveyard
        /// and/or hand) that match the effect's filter and still fit the
        /// remaining MV budget.
        candidates: Vec<ObjectId>,
        /// CR 601.2: Casts still available in this window.
        remaining_casts: u8,
        /// CR 202.3: Running-total mana-value budget remaining, or `None` for
        /// no MV cap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        remaining_mv_budget: Option<u32>,
        /// CR 601.2a: Filter the candidates must match. Carried so the handler
        /// can rebuild the post-cast re-offer's candidate set.
        filter: crate::types::ability::TargetFilter,
        /// CR 601.2a: Zones searched for candidates (controller's graveyard
        /// and/or hand).
        zones: Vec<crate::types::zones::Zone>,
        /// CR 614.1a: Whether spells cast this way are exiled instead of going
        /// to their owner's graveyard.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        exile_instead_of_graveyard: bool,
    },
}

/// CR 701.56a: Which half of a time-travel choice is currently being
/// presented. Typed instead of boolean so serialized engine state says whether
/// the player is adding or removing counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TimeTravelPhase {
    Remove,
    Add,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WaitingFor {
    Priority {
        player: PlayerId,
    },
    /// CR 103.5 + 103.5b: London mulligan — each un-kept player decides
    /// simultaneously. The `pending` list holds every player who has not yet
    /// chosen `MulliganChoice::Keep`, each with their current mulligan count.
    /// Players act in any order; `Keep` removes the actor from `pending`,
    /// `Mulligan` increments their count and redraws but keeps them in
    /// `pending`, and `UseSerumPowder { object_id }` (CR 103.5b) exiles the
    /// hand and redraws the same number without incrementing the count, also
    /// keeping the player in `pending`. When `pending` is empty the flow
    /// advances to `MulliganBottomCards` (if anyone owes bottoms) or
    /// `finish_mulligans`.
    ///
    /// CR 103.5d deferred: Two-Headed Giant team mulligans are not modeled
    /// (the format lacks team semantics).
    MulliganDecision {
        pending: Vec<MulliganDecisionEntry>,
        /// CR 103.5c + Commander RC supplement: whether this game grants a
        /// free first mulligan (multiplayer ≥3 seats, or a duel in a format
        /// where `GameFormat::grants_free_first_mulligan()` is true).
        /// Surfaced so display layers can render "Free Mulligan" labelling
        /// without re-deriving format/seat rules.
        free_first_mulligan: bool,
    },
    /// CR 103.5: After all players have kept, each player who mulliganed at
    /// least once (and is not on the free-first discount) must put N cards on
    /// the bottom of their library, where N = `count`. All such players choose
    /// simultaneously and submit `SelectCards { cards }` in any order.
    MulliganBottomCards {
        pending: Vec<MulliganBottomEntry>,
    },
    /// TL:R 906.6a/e: A player with more than one Tiny Leader performs a
    /// forced first mulligan before any player may make a normal mulligan
    /// decision or use "any time you could mulligan" actions.
    OpeningHandBottomCards {
        pending: Vec<MulliganBottomEntry>,
        reason: OpeningHandBottomReason,
    },
    ManaPayment {
        player: PlayerId,
        /// CR 702.51a / Waterbend: When present, the player can tap untapped
        /// creatures/artifacts to pay mana. Summoning sickness does not apply (CR 302.6).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    /// CR 702.132a: Assist — when casting a spell with assist whose locked total
    /// cost has a generic component, before the caster pays they MAY choose
    /// another player to help pay the generic mana. The CASTER acts on this step
    /// (`ChooseAssistPlayer`); choosing `None` declines and proceeds to normal
    /// payment, choosing a player advances to `AssistPayment`. `max_generic` is
    /// the generic component of the locked cost; `convoke_mode` threads through
    /// to the eventual `ManaPayment`.
    AssistChoosePlayer {
        player: PlayerId,
        candidates: Vec<PlayerId>,
        max_generic: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    /// CR 702.132a: Assist — the CHOSEN player decides how much of the spell's
    /// generic mana to pay (`CommitAssistPayment { generic }`, 0 = contribute
    /// nothing). `acting_player()` returns `chosen`, so authorization routes the
    /// step to that player rather than the caster. `max_generic` is the most the
    /// chosen player may contribute (capped to both the cost's generic and what
    /// they can produce); the committed mana is applied to the caster's spell and
    /// the cast resumes at normal `ManaPayment`.
    AssistPayment {
        caster: PlayerId,
        chosen: PlayerId,
        max_generic: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    /// CR 107.1b + CR 601.2f: Caster chooses the value of X for a pending cast
    /// whose cost contains `ManaCostShard::X`. Usually fires after target
    /// selection and before `ManaPayment`; fires before target selection when a
    /// selected mode's target legality depends on X. `max` is the
    /// engine-computed upper bound for UI display and AI enumeration (see
    /// `casting_costs::max_x_value`).
    /// `min` defaults to zero and is raised by parser-stamped restrictions such
    /// as "X can't be 0."
    /// `convoke_mode` passes through to the subsequent `ManaPayment` step.
    /// `pending_cast` is embedded so filtered state snapshots (multiplayer)
    /// still carry enough context for the UI to render the spell name/cost.
    ChooseXValue {
        player: PlayerId,
        #[serde(default)]
        min: u32,
        max: u32,
        pending_cast: Box<PendingCast>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    TargetSelection {
        player: PlayerId,
        pending_cast: Box<PendingCast>,
        target_slots: Vec<TargetSelectionSlot>,
        /// CR 700.2 / CR 601.2b: For a modal spell whose chosen modes each
        /// require targets, this carries a per-slot display label naming the
        /// mode each target belongs to. `mode_labels[i]` ↔ `target_slots[i]`
        /// (same length when present); `None` for slots without a mode
        /// context (non-modal spells, or modes whose description is missing).
        /// Display-only — the engine owns the slot→mode mapping; the UI just
        /// surfaces it in the targeting banner.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mode_labels: Vec<Option<String>>,
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
        /// CR 702.111b (Menace) + CR 509.1b: per-attacker minimum-blocker count
        /// for attackers requiring more than one blocker. Lets the UI surface
        /// "needs N blockers" feedback and guard confirmation; attackers with
        /// the trivial requirement of 1 are omitted. Computed by
        /// `combat::block_requirements_for_player` — the same authority that
        /// enforces the requirement in `validate_blocks`.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        block_requirements: HashMap<ObjectId, u32>,
    },
    /// CR 502.3: During the untap step, the active player may choose not to
    /// untap permanents with "You may choose not to untap..." static abilities.
    UntapChoice {
        player: PlayerId,
        candidates: Vec<ObjectId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chosen_not_to_untap: Vec<ObjectId>,
    },
    /// CR 508.1g + CR 701.43d: As attackers are declared, the active player may
    /// pay the optional "exert this creature as it attacks" cost on each
    /// attacker that has an exert-as-attack ability and hasn't been exerted this
    /// turn. `attacker` is the creature currently being decided; `remaining` is
    /// the queue of further exert candidates this declaration. Mirrors the
    /// one-at-a-time loop of `UntapChoice`.
    ExertChoice {
        player: PlayerId,
        attacker: ObjectId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining: Vec<ObjectId>,
    },
    /// CR 508.1g + CR 702.154a: As attackers are declared, the active player
    /// may tap up to one eligible creature for each Enlist instance on an
    /// attacking creature. `eligible` is the current legal tap set for this
    /// instance; `remaining` is the queue of later Enlist instances this
    /// declaration.
    EnlistChoice {
        player: PlayerId,
        attacker: ObjectId,
        eligible: Vec<ObjectId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining: Vec<ObjectId>,
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
    /// CR 603.3b: When a player controls 2+ triggered abilities placed on the
    /// stack in the same pass, that player chooses the order. The variant is
    /// emitted in **choice order** (APNAP per CR 101.4 — active player chooses
    /// first), one player at a time. Only when the prompted group has
    /// `triggers.len() >= 2`; single-trigger groups never prompt. The chosen
    /// permutation is applied within the controller's fixed placement slot;
    /// placement order across controllers stays NAP-first (CR 405.3 + 603.3b).
    OrderTriggers {
        player: PlayerId,
        triggers: Vec<PendingTriggerSummary>,
    },
    /// CR 707.9: Player chooses a permanent to copy as part of an "enter as a copy of"
    /// replacement effect. This is a choice, not targeting (hexproof/shroud don't apply).
    CopyTargetChoice {
        player: PlayerId,
        /// The permanent that just entered the battlefield (the clone).
        source_id: ObjectId,
        /// Legal permanents on the battlefield that can be copied.
        valid_targets: Vec<ObjectId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        max_mana_value: Option<u32>,
    },
    /// CR 701.44d: Player chooses which of their remaining permanents explores next.
    ExploreChoice {
        player: PlayerId,
        source_id: ObjectId,
        choosable: Vec<ObjectId>,
        remaining: Vec<ObjectId>,
        pending_effect: Box<ResolvedAbility>,
    },
    /// CR 303.4 + CR 303.4a + CR 303.4f + CR 303.4g + CR 614.12 + CR 115.1b:
    /// After a return-as-Aura sub-effect or a non-spell Aura battlefield entry
    /// finds 2+ legal objects or players matching the parsed enchant filter,
    /// the controller picks which host the Aura attaches to. This is a CHOICE
    /// (CR 303.4f / CR 303.4g), not a target (CR 115.1b applies to Aura spells
    /// being cast), so hexproof / shroud / protection do NOT filter
    /// `legal_targets`.
    ///
    /// **Forward-looking note (per add-engine-variant gate):** if a fourth
    /// resolution-time-pick `WaitingFor` variant is added (e.g., a future
    /// CR 706 emerge replacement, CR 305 land-attach pick), refactor this
    /// sibling cluster (ExploreChoice / CopyTargetChoice / EquipTarget /
    /// ReturnAsAuraTarget) into a unified
    /// `WaitingFor::ObjectPick { kind: ObjectPickKind, ... }` BEFORE adding
    /// the fourth.
    ReturnAsAuraTarget {
        player: PlayerId,
        source_id: ObjectId,
        /// The Aura object on the battlefield awaiting a controller-selected
        /// enchant host.
        returned_id: ObjectId,
        /// Battlefield objects (excluding `returned_id`) or players that
        /// satisfy the parsed `enchant_filter`. Built via
        /// `filter::matches_target_filter` / `player_matches_target_filter` —
        /// hexproof / shroud / protection are intentionally NOT applied here
        /// (CR 303.4 / CR 115.1b distinction).
        legal_targets: Vec<TargetRef>,
        /// The `ResolvedAbility` that emitted this picker; cloned so
        /// return-as-Aura can re-read `effect.enchant_filter` / `effect.grants`,
        /// and generic Aura entry can preserve source metadata for completion.
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
    /// CR 702.184a: Player must pick another untapped creature they control
    /// to tap as the station ability's cost. The chosen creature's power
    /// becomes the number of charge counters added to the Spacecraft.
    StationTarget {
        player: PlayerId,
        spacecraft_id: ObjectId,
        /// Other untapped creatures the player controls (excluding the Spacecraft itself).
        eligible_creatures: Vec<ObjectId>,
    },
    /// CR 702.171a: Player must tap creatures with total power >= saddle_power
    /// to saddle this Mount (sorcery speed).
    SaddleMount {
        player: PlayerId,
        mount_id: ObjectId,
        /// The saddle N value from the keyword.
        saddle_power: u32,
        /// Untapped creatures the player controls (excluding the Mount itself).
        eligible_creatures: Vec<ObjectId>,
    },
    ScryChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    /// CR 705.1 + CR 614.1a: Krark's Thumb — the controller flipped `results.len()`
    /// coins for one logical flip and must ignore all but `keep_count`. `results[i]`
    /// is true for heads/won (CR 705.2).
    CoinFlipKeepChoice {
        player: PlayerId,
        results: Vec<bool>,
        keep_count: usize,
    },
    /// CR 701.20e: Waiting for the player to choose which looked-at cards to keep.
    DigChoice {
        /// Player who looks at the cards and makes any selection.
        player: PlayerId,
        /// Player whose library the cards came from.
        #[serde(default)]
        library_owner: PlayerId,
        cards: Vec<ObjectId>,
        keep_count: usize,
        /// True = select 0..=keep_count ("up to N"), false = exactly keep_count.
        #[serde(default)]
        up_to: bool,
        /// Cards that pass the filter — frontend greys out others.
        #[serde(default)]
        selectable_cards: Vec<ObjectId>,
        /// Where kept cards go. None means the kept cards stay in their current
        /// zone and are only published for downstream continuations.
        #[serde(default)]
        kept_destination: Option<Zone>,
        /// Where unchosen cards go (None = Graveyard, Some(Library) = bottom).
        #[serde(default)]
        rest_destination: Option<Zone>,
        /// Source ability's object ID for filter context.
        #[serde(default)]
        source_id: Option<ObjectId>,
        /// CR 614.1 / CR 110.5b: Kept cards entering the battlefield via this
        /// dig are tapped.
        #[serde(default)]
        enter_tapped: bool,
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
        /// CR 701.20a: When true, the prompt offers a "decline" option (empty
        /// `SelectCards` payload). Used by "you may reveal" patterns (reveal-lands
        /// like Port Town and Gilt-Leaf Palace) where a player can choose to skip
        /// the reveal. The decline branch is stashed on the effect source and
        /// resolved via `pending_continuation` when the empty pick arrives.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        optional: bool,
        /// CR 701.20a: Optional reveal-from-hand effects use an empty selection
        /// to run an explicit decline branch. Optional post-reveal hand choices
        /// use an empty selection to skip their follow-up instead.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        decline_runs_continuation: bool,
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
        /// CR 107.1c + CR 701.23d: When true, the searcher may select 0..=count
        /// cards. When false, they must select exactly count cards.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        /// CR 608.2c: Selection-time constraint propagated from
        /// `Effect::SearchLibrary.selection_constraint` (e.g., "with different
        /// names"). Enforced by the Select-handler call site and used by the
        /// AI candidate enumerator to prune illegal combinations.
        #[serde(default)]
        constraint: SearchSelectionConstraint,
        /// CR 701.23a + CR 608.2c: Split-destination metadata propagated from
        /// `Effect::SearchLibrary.split` (cultivate-class "put one onto the
        /// battlefield tapped and the other into your hand"). When set, the
        /// SearchChoice-completion handler partitions the found set: it either
        /// fast-paths (found <= primary_count) or parks
        /// `SearchPartitionChoice` for the searcher to choose. Mirrors how
        /// `constraint` carries selection metadata onto the choice state.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<SearchDestinationSplit>,
    },
    /// CR 701.23a + CR 608.2c: After a split-destination search finds more cards
    /// than `primary_count`, the searcher chooses which `primary_count` cards go
    /// to `primary_destination` (Battlefield, possibly tapped); the rest go to
    /// `rest_destination` (Hand). Used by cultivate-class effects. The found set
    /// was already chosen via `SearchChoice`.
    SearchPartitionChoice {
        player: PlayerId,
        /// The found set (already chosen via SearchChoice).
        cards: Vec<ObjectId>,
        primary_destination: Zone,
        primary_count: u32,
        primary_enter_tapped: EtbTapState,
        rest_destination: Zone,
        source_id: ObjectId,
    },
    /// CR 400.11/400.11a + CR 701.23j: Player chooses card(s) they own from
    /// outside the game. The engine's bounded outside-game set is the player's
    /// current sideboard, represented by `DeckEntry`s rather than `GameObject`s.
    OutsideGameChoice {
        player: PlayerId,
        source_id: ObjectId,
        choices: Vec<OutsideGameChoiceEntry>,
        count: usize,
        #[serde(default)]
        reveal: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        destination: Zone,
    },
    /// CR 700.2: Player selects card(s) from a tracked set (e.g., exiled cards).
    /// Chosen/unchosen cards flow into sub-abilities via pending_continuation,
    /// unlike DigChoice which moves to fixed zones.
    ChooseFromZoneChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        count: usize,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<ChooseFromZoneConstraint>,
        source_id: ObjectId,
    },
    /// CR 701.55a: Player chooses one branch while facing a villainous choice,
    /// or another inline resolution-time "choose A or B" effect.
    ChooseOneOfBranch {
        player: PlayerId,
        controller: PlayerId,
        source_id: ObjectId,
        branches: Vec<AbilityDefinition>,
        /// Display labels for each branch, derived from branch ability descriptions.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        branch_descriptions: Vec<String>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        parent_targets: Vec<TargetRef>,
        #[serde(default)]
        context: super::ability::SpellContext,
        /// Players still to face the same choice in APNAP order (CR 701.55d).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining_players: Vec<PlayerId>,
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
        /// CR 107.1c: Minimum number of cards that must be selected when a
        /// choice allows a range. Defaults to 0 for ordinary "up to" choices.
        #[serde(default, skip_serializing_if = "is_zero_usize")]
        min_count: usize,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        up_to: bool,
        source_id: ObjectId,
        effect_kind: crate::types::ability::EffectKind,
        /// Source zone of eligible objects (Battlefield for sacrifice, Hand for put-onto-BF).
        zone: Zone,
        /// Destination zone for ChangeZone effects. None for Sacrifice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        destination: Option<Zone>,
        #[serde(
            default,
            with = "super::zones::etb_tap_bool_compat",
            skip_serializing_if = "EtbTapState::is_unspecified"
        )]
        enter_tapped: EtbTapState,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enter_transformed: bool,
        /// CR 110.2a: Resolved-once controller override carried through the
        /// `EffectZoneChoice` round-trip. `Some(pid)` routes the chosen
        /// object(s) to `pid` on battlefield entry; `None` leaves them
        /// under their owner's control.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enters_under_player: Option<PlayerId>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        enters_attacking: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        owner_library: bool,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        track_exiled_by_source: bool,
        /// CR 708.2a + CR 708.3: face-down entry profile carried across the
        /// `EffectZoneChoice` round-trip so a selected `ChangeZone` card that
        /// must enter face down (Yedora-style "return it face down ... It's a
        /// Forest land") still applies the profile when the choice resolves,
        /// instead of resuming face up and exposing its real characteristics.
        /// `None` = normal face-up entry. Mirrors the `enter_tapped` /
        /// `enter_transformed` / `enters_under_player` carry-through above.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        face_down_profile: Option<crate::types::ability::FaceDownProfile>,
        /// CR 701.68a: N for Blight N — number of -1/-1 counters to place.
        /// Zero for all non-blight EffectZoneChoice uses.
        #[serde(default)]
        count_param: u32,
    },
    /// Player chooses which drawn-this-turn hand cards to put on top of their
    /// library. Each unchosen required card is kept by paying life.
    DrawnThisTurnTopdeckChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        count: usize,
        min_count: usize,
        life_payment: u32,
        source_id: ObjectId,
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
        /// CR 700.2 / CR 601.2b: Per-slot mode display label, parallel to
        /// `target_slots` (`mode_labels[i]` ↔ `target_slots[i]`). Populated for
        /// modal triggered abilities (CR 700.2b) whose chosen modes target;
        /// `None` per slot otherwise. Display-only — see `TargetSelection`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        mode_labels: Vec<Option<String>>,
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
    /// Alchemy "draft a card from [card]'s spellbook": `player` chooses one card
    /// name from `options` (the source card's spellbook list); the chosen card is
    /// then conjured into `destination` (`tapped` if a "tapped" rider applied).
    SpellbookDraft {
        player: PlayerId,
        source_id: ObjectId,
        options: Vec<String>,
        destination: Zone,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        tapped: bool,
    },
    /// CR 609.7a: Player must choose a source of damage from currently
    /// represented legal source objects.
    DamageSourceChoice {
        player: PlayerId,
        source_filter: TargetFilter,
        options: Vec<ObjectId>,
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
        /// CR 702.33c/d: How many times this spell has already been kicked. Lets the
        /// frontend present a kick-count-aware modal for repeatable multikicker re-prompts.
        /// Zero for the first prompt and for non-kicker optional costs.
        #[serde(default)]
        times_kicked: u32,
        pending_cast: Box<PendingCast>,
    },
    /// CR 702.47a–e: As an Arcane (or other matching-subtype) spell is cast, its
    /// controller may reveal a "Splice onto [subtype]" card from hand to copy its
    /// text box onto the spell and pay its splice cost as an additional cost.
    /// `eligible` are the hand cards still available to splice; the prompt is
    /// re-presented after each acceptance until the caster declines (`card: None`)
    /// or `eligible` is exhausted.
    SpliceOffer {
        player: PlayerId,
        pending_cast: Box<PendingCast>,
        eligible: Vec<ObjectId>,
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
    /// CR 715.3a + CR 702.94a + CR 702.35a + CR 702.85a + CR 701.57a + CR 702.xxx:
    /// A player is offered a card to cast via a special rule.
    CastOffer {
        player: PlayerId,
        kind: CastOfferKind,
    },
    /// CR 712.12 / CR 712.11b: Player chooses which face of an MDFC to
    /// play/cast. Two cases reach this prompt: (a) both faces are lands (CR
    /// 712.12 — the player picks which to put onto the battlefield via the
    /// play-land action), and (b) both faces are spells (CR 712.11b — e.g.
    /// Esika, God of the Tree // The Prismatic Bridge and the other Kaldheim
    /// gods — where the player picks which face to cast before it goes on the
    /// stack). The `ChooseModalFace` handler routes the post-choice re-entry
    /// by the now-active face's type (land → play-land, spell → cast).
    /// `payment_mode` carries the manual/auto mana mode forward into the
    /// spell-cast re-entry (ignored for the land path, which is always Auto).
    ModalFaceChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
    },
    /// CR 118.9: Player chooses between paying the spell's printed mana cost
    /// and paying a keyword-granted alternative mana cost. Only presented when
    /// both costs are affordable (and, for Bestow, a legal Aura target exists
    /// per CR 702.103a + CR 303.4a). The `keyword` axis disambiguates the
    /// post-payment semantics; the prompt shape is uniform per CR 118.9 ("you
    /// may pay [cost] rather than this spell's mana cost").
    ///
    /// - `Warp` — custom keyword: cast for warp cost, exile at next end step,
    ///   may be recast from exile later (no CR section; rider lives on the
    ///   keyword).
    /// - `Evoke` (CR 702.74a) — creature ETBs and sacrifices itself when cast
    ///   for the evoke cost (CR 702.74b).
    /// - `Overload` (CR 702.96a) — substitutes the overload cost and rewrites
    ///   every "target" in the spell's text to "each" (CR 702.96b-c).
    /// - `Bestow` (CR 702.103a) — substitutes the bestow cost and turns the
    ///   spell into an Aura with enchant creature (CR 702.103b).
    AlternativeCastChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
        /// Which keyword granted the alternative cost — drives post-payment
        /// dispatch and the modal copy. Exhaustively matched everywhere so a
        /// future keyword addition (e.g., Madness, Spectacle) is a compile
        /// error at every site.
        keyword: AlternativeCastKeyword,
        /// The card's printed mana cost (for display in the choice modal).
        normal_cost: ManaCost,
        /// The mana portion of the keyword-granted alternative cost (for
        /// display in the choice modal). `None` for purely non-mana
        /// alternative costs (e.g., Solitude's "Evoke—Exile a white card from
        /// your hand."). Typed `Option` rather than `ManaCost::zero()`
        /// sentinel so callers must explicitly handle absence (no
        /// `feedback_no_bool_flags`-style sentinel ambiguity).
        #[serde(default)]
        alternative_cost: Option<ManaCost>,
        /// CR 702.74a + CR 118.9: Display payload for the non-mana portion of
        /// the alternative cost (e.g., `AbilityCost::Exile { count, zone,
        /// filter }` for the MH2 Evoke Incarnations). `None` when the
        /// alternative cost is pure mana (Warp, Lorwyn Evoke, Overload,
        /// Bestow, mana-only Flashback). Engine owns the derived display
        /// string; the frontend renders the engine-provided description.
        #[serde(default)]
        alternative_additional_cost: Option<AbilityCost>,
    },
    /// CR 702.140c + CR 730.2a: As a mutating creature spell resolves with a
    /// legal target, the spell's controller chooses whether the spell is put on
    /// TOP of the target creature or on the BOTTOM. `merging_id` is the resolving
    /// mutate spell object (popped from the stack into a paused state); `target_id`
    /// is the surviving battlefield creature whose `ObjectId` the merged permanent
    /// keeps (CR 730.2c). The choice only sets which component supplies copiable
    /// characteristics (CR 730.2a); the merged permanent always has the union of
    /// all components' abilities (CR 702.140e). Resolved by
    /// `merge::handle_mutate_merge_choice` via `GameAction::ChooseMutateMergeSide`.
    MutateMergeChoice {
        player: PlayerId,
        merging_id: ObjectId,
        target_id: ObjectId,
    },
    /// CR 702.99a: A resolving Cipher spell offers "you may exile this card
    /// encoded on a creature you control". `card_id` is the resolving spell
    /// (held in limbo off the stack until the choice completes, mirroring
    /// `MutateMergeChoice`); `creatures` are the legal hosts the controller may
    /// pick from, or decline (sending the card to its graveyard).
    CipherEncodeChoice {
        player: PlayerId,
        card_id: ObjectId,
        creatures: Vec<ObjectId>,
    },
    /// CR 601.2b: Player chooses which legal cast permission / variant to use
    /// when more than one applies to the same spell from the same zone.
    CastingVariantChoice {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
        options: Vec<CastingVariantChoiceOption>,
    },
    /// CR 110.4: Player chooses which permanent type slot to consume when
    /// casting/playing a multi-type card from the graveyard via a
    /// `OncePerTurnPerPermanentType` permission source (Muldrotha).
    /// Only presented when the card has more than one available slot.
    ChoosePermanentTypeSlot {
        player: PlayerId,
        object_id: ObjectId,
        card_id: CardId,
        source: ObjectId,
        #[serde(default)]
        payment_mode: CastPaymentMode,
        available_slots: Vec<super::card_type::CoreType>,
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
        #[serde(default, skip_serializing_if = "Option::is_none")]
        may_trigger_key: Option<MayTriggerAutoChoiceKey>,
    },
    /// CR 702.95a + CR 608.2d: Soulbond partner choice made while the PairWith
    /// effect resolves. The listed objects are legal choices, not targets.
    PairChoice {
        player: PlayerId,
        source_id: ObjectId,
        choices: Vec<ObjectId>,
    },
    /// CR 702.104a: The chosen opponent of a Tribute creature must decide whether
    /// to place the Tribute +1/+1 counters. `source_id` is the entering Tribute
    /// creature; `count` is the number of +1/+1 counters to place on accept. On
    /// either branch, a `ChosenAttribute::TributeOutcome` is persisted on the
    /// source so the companion "if tribute wasn't paid" trigger (CR 702.104b) can
    /// read the outcome. Reuses `GameAction::DecideOptionalEffect`.
    TributeChoice {
        player: PlayerId,
        source_id: ObjectId,
        count: u32,
    },
    /// CR 702.94a + CR 603.11: `player` may reveal `object_id` from their hand
    /// and cast it for the miracle mana cost `cost`, or decline. Flushed from
    /// the head of `pending_miracle_offers` when `run_post_action_pipeline`
    /// would otherwise return `WaitingFor::Priority` for the offer's player.
    /// `GameAction::CastSpellAsMiracle` accepts; `GameAction::DecideOptionalEffect
    /// { accept: false }` declines (reuses the generic optional-decline path).
    /// Either response consumes the offer.
    MiracleReveal {
        player: PlayerId,
        object_id: ObjectId,
        cost: super::mana::ManaCost,
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
        /// CR 118.12: The cost to pay. Stored as the unified `AbilityCost`
        /// taxonomy. Forward-compatible deserialization accepts the legacy
        /// `UnlessCost` JSON shape (see `deserialize_ability_cost_compat` in
        /// `types/ability.rs`).
        #[serde(deserialize_with = "crate::types::ability::deserialize_ability_cost_compat")]
        cost: AbilityCost,
        /// The effect to execute if the player declines to pay.
        pending_effect: Box<ResolvedAbility>,
        /// Trigger event context to restore if declining the payment resumes a
        /// triggered ability effect that still references the triggering event.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
        /// Human-readable description for the frontend (e.g., "counter target spell", "draw a card").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_description: Option<String>,
        /// CR 118.12a: Players still to poll after the current `player`, in
        /// APNAP order. Non-empty only for "unless any player pays ..." clauses
        /// (`TargetFilter::AllPlayers` payer): if `player` declines, the next
        /// player in `remaining` is prompted; the first to pay prevents the
        /// effect. Empty for ordinary single-payer unless-costs (Mana Leak).
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining: Vec<PlayerId>,
    },
    /// CR 118.12a: Player must choose **which** sub-cost to pay from a
    /// disjunctive ("unless they X or Y") unless-cost. Once a sub-cost is
    /// chosen, the resolver re-enters `handle_unless_payment` with that
    /// single cost as if the OR had never been there. Declining surfaces
    /// the cost-payment-failure path (the original effect happens).
    ///
    /// Drives Tergrid's Lantern ("unless they sacrifice a nonland permanent
    /// of their choice or discard a card") and the broader punisher-disjunction
    /// class.
    UnlessPaymentChooseCost {
        player: PlayerId,
        /// The sub-costs the paying player may choose between.
        /// Stored as the unified `AbilityCost` taxonomy; forward-compatible
        /// deserialization accepts the legacy `UnlessCost` JSON shape per-item.
        costs: Vec<AbilityCost>,
        /// The pending effect (with `unless_pay` already stripped) to apply if
        /// the player declines to pay any branch.
        pending_effect: Box<ResolvedAbility>,
        /// Trigger event context to restore.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
        /// Human-readable description for the frontend.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_description: Option<String>,
        /// CR 702.24a + CR 118.12: Remaining disjunctive choice queues, one
        /// entry per remaining `OneOf` sub-cost in a `Composite`-of-`OneOf`s
        /// expansion. Used to drive sequential per-counter choices for
        /// cumulative-upkeep-style "each choice is made separately for each
        /// age counter" prompts. Empty for single-choice unless-payments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining_choices: Vec<Vec<AbilityCost>>,
        /// CR 702.24a + CR 118.12: Picks accumulated from prior prompts in the
        /// sequence; combined into a final `Composite` cost when
        /// `remaining_choices` is exhausted. Empty for single-choice
        /// unless-payments.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chosen: Vec<AbilityCost>,
    },
    /// CR 702.21a: Player must choose a card to discard as ward cost payment.
    WardDiscardChoice {
        player: PlayerId,
        /// Eligible cards in hand.
        cards: Vec<ObjectId>,
        /// The counter effect to prevent if the discard succeeds.
        pending_effect: Box<ResolvedAbility>,
        /// CR 702.24a: cards remaining to discard (per-age-counter scaling). One card per round-trip.
        #[serde(default = "default_remaining_one")]
        remaining: u32,
        /// CR 701.9b: eligibility filter, threaded so the re-prompt branch can re-derive hand
        /// eligibility after each discard (the just-discarded card is moved to graveyard but STILL
        /// EXISTS in state.objects, so a contains_key filter would be wrong).
        #[serde(default)]
        filter: Option<TargetFilter>,
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
        /// CR 118.12: Multi-select sacrifice whose combined power must meet this
        /// threshold. `None` = pick exactly one per round-trip until `remaining`
        /// reaches zero.
        #[serde(default)]
        min_total_power: Option<i32>,
    },
    /// CR 118.12: Player must choose permanent(s) to return to hand as unless cost.
    UnlessBounceChoice {
        player: PlayerId,
        permanents: Vec<ObjectId>,
        pending_effect: Box<ResolvedAbility>,
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
    /// Digital-only Specialize: choose which color specialization to apply.
    SpecializeColor {
        player: PlayerId,
        object_id: crate::types::identifiers::ObjectId,
        options: Vec<crate::types::mana::ManaColor>,
    },
    /// CR 118.3 + CR 601.2b + CR 605.3b: Player must select `count` objects
    /// from `choices` to pay a cost, then the engine resumes via `resume`.
    /// Replaces: DiscardForCost, SacrificeForCost, ReturnToHandForCost,
    /// ExileForCost, RemoveCounterForCost, TapCreaturesForSpellCost,
    /// BeholdForCost, TapCreaturesForManaAbility, DiscardForManaAbility,
    /// ExileForManaAbility, SacrificeForManaAbility.
    PayCost {
        player: PlayerId,
        kind: PayCostKind,
        /// Pre-filtered eligible objects. The player chooses `count` of these.
        choices: Vec<ObjectId>,
        count: usize,
        /// Minimum to choose (0 for exact-count costs; > 0 for at-least-N costs
        /// like SacrificeForCost's `min_count`).
        #[serde(default)]
        min_count: usize,
        resume: CostResume,
    },
    /// CR 118.12a: Player must choose which branch of a disjunctive activation cost
    /// (`AbilityCost::OneOf`) to pay.
    ActivationCostOneOfChoice {
        player: PlayerId,
        costs: Vec<AbilityCost>,
        pending_cast: Box<PendingCast>,
    },
    /// Blight N — player must choose one creature to put N -1/-1 counters on as cost.
    BlightChoice {
        player: PlayerId,
        /// CR 701.68a: N — the number of -1/-1 counters to place on the one chosen creature.
        counters: u32,
        /// Pre-filtered eligible creatures on the battlefield.
        creatures: Vec<ObjectId>,
        /// The pending cast to resume after blight is complete.
        pending_cast: Box<PendingCast>,
    },
    /// CR 605.3a + CR 601.2h + CR 107.4e: A mana ability whose cost is
    /// `Composite { Mana(..), Tap, .. }` (filter lands, Cabal Coffers-style
    /// pay-to-produce abilities) requires the activator to debit mana from
    /// their pool. When the cost contains a hybrid shard with more than one
    /// legal color assignment given the current pool, the player must choose.
    /// `options` lists every legal per-hybrid-shard color vector; each vector
    /// aligns 1:1 with hybrid shards in the cost in printed order. The
    /// unambiguous case (zero hybrid shards or a single legal assignment) is
    /// auto-paid inline and never surfaces this variant.
    PayManaAbilityMana {
        player: PlayerId,
        options: Vec<Vec<ManaType>>,
        pending_mana_ability: Box<PendingManaAbility>,
    },
    /// CR 106.3 + CR 608.2d + CR 605.3b: Mana production with a choice dimension
    /// — player must answer before mana is added to the pool. The prompt shape
    /// depends on the `ManaProduction` variant. All shapes
    /// share this single `WaitingFor` variant so AI candidate generation,
    /// multiplayer filtering, and auto-pass all follow one code path.
    ChooseManaColor {
        player: PlayerId,
        choice: ManaChoicePrompt,
        context: ManaChoiceContext,
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
    /// CR 701.20a + CR 608.2c: "You may put that card onto the battlefield" — the
    /// controller chooses the kept card's destination after `RevealUntil` finds a
    /// hit. Accept → `accept_zone`; decline → `decline_zone`. The misses (and, on
    /// decline, the hit card when its zone is the rest pile) are moved by the
    /// choice handler so the random-order shuffle includes the declined card.
    RevealUntilKeptChoice {
        player: PlayerId,
        hit_card: ObjectId,
        /// CR 508.4: The ability source (e.g. the attacking creature whose
        /// trigger revealed this card). Supplies the defending player when the
        /// accepted card enters the battlefield attacking.
        source_id: ObjectId,
        accept_zone: Zone,
        decline_zone: Zone,
        enter_tapped: EtbTapState,
        /// CR 508.4: When the accepted card goes to the battlefield, it enters
        /// attacking ("tapped and attacking"). Carried from `Effect::RevealUntil`.
        #[serde(default)]
        enters_attacking: bool,
        revealed_misses: Vec<ObjectId>,
        rest_destination: Zone,
    },
    /// CR 107.1c + CR 608.2c: After one iteration of a "you may repeat this
    /// process any number of times" effect resolves, the controller chooses
    /// whether to run the process again. Answered by
    /// `GameAction::DecideOptionalEffect { accept }`.
    RepeatDecision {
        player: PlayerId,
        /// The ability chain to re-resolve on accept (one further iteration).
        /// `repeat_until` is retained so the next iteration re-prompts.
        ability: Box<crate::types::ability::ResolvedAbility>,
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
    /// CR 701.30b: "Clash with an opponent" lets the clashing player choose
    /// which opponent to clash with. Only entered when two or more opponents
    /// are available (with one opponent there is no decision). `candidates`
    /// is the set of legal opponents; `ability` is the resolving clash ability,
    /// carried so the clash can be performed against the chosen opponent.
    ClashChooseOpponent {
        player: PlayerId,
        candidates: Vec<PlayerId>,
        ability: Box<crate::types::ability::ResolvedAbility>,
    },
    /// CR 701.30c: After a clash, each player puts their revealed card on top or
    /// bottom of their library. Choices are made in APNAP order. `remaining` holds
    /// the next player/card pairs still awaiting a choice.
    ClashCardPlacement {
        player: PlayerId,
        card: ObjectId,
        remaining: Vec<(PlayerId, ObjectId)>,
    },
    /// CR 701.38: A player is voting on the listed choices. After this player
    /// has cast all of their votes (1 + extras from "you may vote an additional
    /// time" static abilities), the engine advances to the next player in
    /// APNAP order until every non-eliminated player has voted, then resolves
    /// the per-choice tally sub-effects. Lives in the engine — frontend just
    /// renders the modal.
    VoteChoice {
        /// The voter currently making a choice.
        player: PlayerId,
        /// CR 701.38d: Remaining votes this player must cast before passing
        /// the turn to the next voter. Always >= 1 when this state is entered.
        remaining_votes: u32,
        /// Lowercase choice identifiers as defined in `Effect::Vote.choices`.
        /// Persisted on `WaitingFor` (not just on the ability) so multiplayer
        /// state filtering and the frontend modal can render the prompt
        /// without re-walking the stack.
        options: Vec<String>,
        /// Display labels (original-case from Oracle text) — frontend renders
        /// these; the engine compares votes against `options`.
        option_labels: Vec<String>,
        /// Players still awaiting their first vote, in APNAP order from the
        /// starting voter. Each entry is `(player_id, total_votes)` where
        /// `total_votes` is computed at vote-session start (CR 701.38d: extra
        /// votes resolve at the same time the player would otherwise vote).
        remaining_voters: Vec<(PlayerId, u32)>,
        /// Vote tallies indexed parallel to `options`. `tallies[i]` is the
        /// number of votes cast for `options[i]` so far.
        tallies: Vec<u32>,
        /// CR 608.2c + CR 701.38: Per-vote ballot ledger. Each entry is
        /// `(voter, choice_index)` recorded when the voter casts that vote.
        /// Mirrors `tallies` aggregation but preserves voter identity so the
        /// per-choice sub-effect can route to `PlayerFilter::VotedFor` against
        /// `state.last_vote_ballots`. Append-only; the lifecycle matches
        /// `last_zone_changed_ids` (cleared at chain depth 0).
        #[serde(default)]
        ballots: im::Vector<(PlayerId, u8)>,
        /// CR 701.38: Per-choice sub-effects. `per_choice_effect[i]` resolves
        /// once for each vote tallied against `options[i]`. Carried on the
        /// WaitingFor so the resolver chain doesn't need to re-find the source
        /// ability — voting can outlive permanents (LKI) and the WaitingFor is
        /// always the canonical state.
        per_choice_effect: Vec<Box<super::ability::AbilityDefinition>>,
        /// Ability controller — the player who owns the Vote effect. Used by
        /// the tally resolver to scope sub-effects to the correct controller.
        controller: PlayerId,
        /// Source ability's object ID — used by logging and for state-filter
        /// echoes; mirrors the `source_id` carried on other interactive
        /// `WaitingFor` variants (e.g., NamedChoice).
        source_id: ObjectId,
        /// CR 101.4 + CR 608.2 (Battlebond keyword action, no explicit CR
        /// section): The "who acts" descriptor for the current step. See
        /// [`VoteActor`] for the two cases (`SubjectActs` for classic
        /// Council's-dilemma, `Delegated(controller)` for friend-or-foe).
        /// Use [`VoteActor::resolve`] with `player` to get the player
        /// authorized to submit the next `ChooseOption`.
        actor: VoteActor,
    },
    /// CR 700.3 + CR 700.3a + CR 101.4: A subject is partitioning their own
    /// objects into two piles for an `Effect::SeparateIntoPiles`. `pile_a`
    /// is submitted by `player` via `GameAction::SubmitPilePartition`; pile B
    /// is derived as `eligible \ pile_a` by the handler. After each
    /// submission the queue advances to the next subject in APNAP order
    /// (CR 101.4b — each subject sees prior subjects' completed piles
    /// before partitioning their own). When the subject queue empties, the
    /// engine transitions to [`Self::SeparatePilesChoice`] for `chooser`.
    SeparatePilesPartition {
        /// The subject currently partitioning their own objects.
        player: PlayerId,
        /// CR 700.3 + CR 700.3a: Eligible objects controlled by `player`
        /// that match the effect's `object_filter`. The partition must be
        /// a subset of this set.
        eligible: im::Vector<ObjectId>,
        /// CR 101.4 + CR 800.4g: Remaining subjects still to partition, in
        /// APNAP order from the active player. Each entry is paired with
        /// that subject's pre-computed eligible set so the handler does not
        /// need to re-walk the battlefield.
        remaining_subjects: im::Vector<(PlayerId, im::Vector<ObjectId>)>,
        /// CR 700.3a: Completed partitions accumulated so prior subjects'
        /// pile shapes are visible to later subjects (CR 101.4b) and the
        /// chooser can resolve each in turn.
        completed: im::Vector<PileResult>,
        /// CR 700.3: The player who will choose one pile per subject.
        chooser: PlayerId,
        /// CR 608.2c: Sub-effect applied to each chosen pile, once per
        /// object, with the subject rebound as controller.
        chosen_pile_effect: Box<super::ability::AbilityDefinition>,
        /// Source ability's object ID — for logging and state filter echoes.
        source_id: ObjectId,
    },
    /// CR 700.3 + CR 101.4c: The chooser picks one pile (A or B) per
    /// completed `PileResult`. CR 101.4c allows the chooser to make
    /// multiple simultaneous choices in any order; the engine drains the
    /// `pending` queue in completion order and the chooser submits one
    /// `GameAction::ChoosePile` per step. When the queue empties, the
    /// chosen-pile sub-effect resolves once per object in each chosen pile.
    SeparatePilesChoice {
        /// The chooser (typically the spell controller).
        player: PlayerId,
        /// Subjects whose chosen pile has not yet been picked.
        pending: im::Vector<PileResult>,
        /// The subject currently being chosen for (head of the original
        /// completed queue).
        current: PileResult,
        /// CR 608.2c: Sub-effect applied to each chosen pile, once per
        /// object, with the subject rebound as controller.
        chosen_pile_effect: Box<super::ability::AbilityDefinition>,
        /// Source ability's object ID — for logging and state filter echoes.
        source_id: ObjectId,
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
    /// CR 903.9a: A commander in a graveyard or exile (put there since the last
    /// SBA check) may be returned to the command zone by its owner. The player
    /// chooses accept (move to command zone) or decline (leave in current zone).
    /// Reuses `GameAction::DecideOptionalEffect`.
    CommanderZoneChoice {
        player: PlayerId,
        commander_id: ObjectId,
        /// The zone the commander is currently in (Graveyard, Exile, Hand, or Library).
        current_zone: Zone,
    },
    /// CR 310.10 + CR 704.5w + CR 704.5x: A battle that isn't being attacked has no
    /// protector, an illegal protector, or (for Sieges) a protector equal to its
    /// controller. The battle's controller (`player`) chooses a legal protector from
    /// `candidates`. Emitted only when `candidates.len() > 1`; the SBA auto-applies
    /// the singleton case and sends the battle to the graveyard when empty.
    BattleProtectorChoice {
        player: PlayerId,
        battle_id: ObjectId,
        candidates: Vec<PlayerId>,
    },
    /// CR 701.34a: Player chooses any number of permanents and/or players that have
    /// counters on them, then adds one counter of each kind already there.
    ProliferateChoice {
        player: PlayerId,
        /// Eligible permanents (with counters) and players (with poison/energy).
        eligible: Vec<TargetRef>,
    },
    /// CR 701.56a: Time travel — the player chooses any number of eligible
    /// objects (permanents they control with a time counter and/or suspended
    /// cards they own in exile with a time counter) and, for each, puts or
    /// removes a time counter. Modeled in two phases over
    /// `GameAction::SelectTargets`: `TimeTravelPhase::Remove` first selects
    /// objects to remove a time counter from; then `TimeTravelPhase::Add`
    /// selects (from the still-eligible remainder) objects to add a time
    /// counter to.
    TimeTravelChoice {
        player: PlayerId,
        eligible: Vec<TargetRef>,
        phase: TimeTravelPhase,
    },
    /// CR 603.7e: The affected player of a `ChooseObjectsIntoTrackedSet` effect
    /// selects any number of battlefield permanents from `eligible`. The
    /// chosen objects are written into a fresh tracked set so a downstream
    /// `PayCost { ScaledMana }` and `IfYouDo`/`Untap` reference the exact
    /// selection. An empty selection is legal — the player declines.
    ChooseObjectsSelection {
        player: PlayerId,
        /// Eligible battlefield permanents matching the effect's filter.
        eligible: Vec<TargetRef>,
        /// CR 608.2: triggering event of the ability whose `ChooseObjectsIntoTrackedSet`
        /// raised this prompt. Restored around the continuation drain so the stashed
        /// `PayCost { payer: TriggeringPlayer }` resolves to the correct player.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<crate::types::events::GameEvent>,
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
        /// CR 101.4: Whether each player chooses independently or one player decides for all.
        #[serde(default)]
        chooser_scope: CategoryChooserScope,
        /// Permanents eligible to be chosen for the category slots.
        #[serde(default = "default_target_filter_permanent")]
        choose_filter: TargetFilter,
        /// Permanents in scope for the final sacrifice sweep.
        #[serde(default = "default_target_filter_permanent")]
        sacrifice_filter: TargetFilter,
        /// Controller of the source ability. Needed after a save/reload or any
        /// paused choice because `player` is the chooser, not necessarily the
        /// source controller.
        #[serde(default)]
        source_controller: PlayerId,
        /// For each category, the eligible permanent IDs (battlefield objects matching that type).
        eligible_per_category: Vec<Vec<ObjectId>>,
        source_id: ObjectId,
        /// Players still to choose after the current one (APNAP order).
        remaining_players: Vec<PlayerId>,
        /// Permanents chosen by previous players — protected from sacrifice.
        all_kept: Vec<ObjectId>,
        /// CR 102.2 (two-player) / CR 102.3 (team multiplayer): the APNAP-ordered
        /// set of players within the effect's `player_scope`. Only permanents
        /// controlled by these players are subject to the sweep. Empty only on a
        /// mid-resolution save/reload (`#[serde(default)]`), in which case
        /// `sacrifice_unchosen` falls back to the full APNAP set.
        #[serde(default)]
        scoped_players: Vec<PlayerId>,
    },
    /// CR 707.10c: When a spell is copied, the controller may choose new targets.
    /// Each slot shows the current target and legal alternatives.
    CopyRetarget {
        player: PlayerId,
        copy_id: ObjectId,
        target_slots: Vec<CopyTargetSlot>,
        /// Effect metadata emitted when this retarget choice completes.
        #[serde(default = "default_copy_retarget_effect_kind")]
        effect_kind: EffectKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        effect_source_id: Option<ObjectId>,
        /// Index of the slot currently awaiting a ChooseTarget action.
        #[serde(default)]
        current_slot: usize,
    },
    /// CR 510.1c: Attacker with multiple blockers — controller divides damage as they choose.
    /// CR 702.19b/c: Trample requires lethal to each blocker before assigning excess.
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
    /// CR 510.1d + CR 702.22k: A blocking creature is blocking a creature with
    /// banding (or, in the deferred "bands with other" form, the relevant
    /// quality pair), so the ACTIVE player — rather than the blocker's
    /// controller — chooses how the blocker's combat damage is divided among the
    /// attackers it is blocking. Unlike `AssignCombatDamage`, a blocker's damage
    /// has no lethal, trample, or planeswalker dimension; it is divided freely
    /// among the blocked attackers (CR 510.1d).
    AssignBlockerDamage {
        player: PlayerId,
        blocker_id: ObjectId,
        total_damage: u32,
        attackers: Vec<ObjectId>,
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
    /// CR 122.5 + CR 608.2d: "Move any number of counters ... onto [set]"
    /// chooses destinations and counts as the ability resolves.
    MoveCountersDistribution {
        player: PlayerId,
        source_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<CounterType>,
        available: Vec<(CounterType, u32)>,
        destinations: Vec<ObjectId>,
        pending_effect: Box<ResolvedAbility>,
    },
    /// CR 107.1c + CR 107.14: "Pay any amount of {E}" — mid-resolution prompt.
    /// Player picks any integer between `min` and `max` inclusive; the chosen
    /// amount is deducted from the relevant resource pool and stamped into
    /// `state.last_effect_count` so subsequent chain steps referencing
    /// `QuantityRef::EventContextAmount` resolve to the paid amount.
    PayAmountChoice {
        player: PlayerId,
        resource: PayableResource,
        min: u32,
        max: u32,
        #[serde(default)]
        accumulated: u32,
        source_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        pending_mana_ability: Option<Box<PendingManaAbility>>,
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
    /// CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: A combat declaration is paused
    /// because one or more declared creatures are covered by "can't attack/block unless
    /// [player] pays [cost]" static abilities (Ghostly Prison, Propaganda, Sphere of
    /// Safety, Windborn Muse, etc.).
    ///
    /// CR 508.1h / 509.1d: `total_cost` is the "locked in" aggregate across all affected
    /// creatures. `per_creature` exposes the breakdown so the UI (and AI policy) can
    /// reason about which attackers/blockers the decline path would strip from the
    /// declaration.
    ///
    /// On `GameAction::PayCombatTax { accept: true }` the engine pays `total_cost` and
    /// resumes the declaration in `pending`. On `accept: false` the engine filters the
    /// taxed creatures out of `pending` (or, if all declared creatures are taxed and the
    /// controller declines, submits an empty declaration — CR 508.8 handles the "no
    /// attackers" path).
    CombatTaxPayment {
        player: PlayerId,
        context: CombatTaxContext,
        total_cost: crate::types::mana::ManaCost,
        per_creature: Vec<(ObjectId, crate::types::mana::ManaCost)>,
        pending: CombatTaxPending,
    },
    /// CR 107.4f + CR 601.2f + CR 601.2h: Caster must approve every Phyrexian shard
    /// that would deduct life — either by choosing between mana and 2 life
    /// (`ShardOptions::ManaOrLife`) or by confirming the life-only payment
    /// (`ShardOptions::LifeOnly`). Only `ShardOptions::ManaOnly` shards auto-resolve
    /// and skip this state, since they carry no life consequence. The player may
    /// always submit `CancelCast` here to abandon the cast rather than pay life
    /// (issue #704).
    ///
    /// The `PendingCast` still lives in `GameState::pending_cast` (same ManaPayment
    /// convention), so multiplayer visibility filtering continues to clear inner detail
    /// for opponents while they see the spell on the stack.
    PhyrexianPayment {
        player: PlayerId,
        /// The spell object being cast.
        spell_object: ObjectId,
        /// One entry per Phyrexian shard in the cost. `shards.len()` is the required
        /// length of the submitted `Vec<ShardChoice>`.
        shards: Vec<PhyrexianShard>,
    },
}

/// CR 707.10c / CR 722.3c: A target slot on a copied spell, showing the
/// current target when one exists and the legal alternatives. A normal copied
/// spell starts with copied targets; a freshly cast prepare-spell copy has no
/// chosen target until the player chooses one during casting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyTargetSlot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<TargetRef>,
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
/// `lethal_minimum` is only enforced as a hard constraint before trample excess (CR 702.19b).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DamageSlot {
    pub blocker_id: ObjectId,
    /// Lethal damage threshold. CR 702.2c: With deathtouch, lethal = 1.
    /// Informational for non-trample; enforced before trample excess (CR 702.19b).
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

/// CR 107.14 + CR 118.8: Resource that can be paid in a "pay any amount of X"
/// prompt. Typed so the same `WaitingFor::PayAmountChoice` variant generalizes
/// to future classes (energy, life, mana) without re-introducing boolean flags.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum PayableResource {
    /// CR 107.14: Pay any amount of `{E}` — removes N energy counters from the player.
    Energy,
    /// CR 107.3 + CR 118.1: Pay a chosen X as generic mana while resolving an effect.
    ManaGeneric {
        #[serde(default = "default_one")]
        per_x: u32,
    },
    /// CR 107.1c + CR 122.1: Choose how many counters to remove.
    Counters,
}

fn default_one() -> u32 {
    1
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
    /// Canonical stable variant name (engine-owned labeler).
    ///
    /// Exhaustive over every `WaitingFor` variant — no wildcard fallback, so the
    /// compiler flags any new variant that fails to register a label. Used by the
    /// stuck-decision diagnostic (`ai_support::stuck_decision_diagnostic`) to
    /// surface which decision is wedged. Distinct from the test-harness labelers
    /// in `game/scenario.rs`, which are private and non-exhaustive.
    pub fn variant_name(&self) -> &'static str {
        match self {
            WaitingFor::Priority { .. } => "Priority",
            WaitingFor::MulliganDecision { .. } => "MulliganDecision",
            WaitingFor::MulliganBottomCards { .. } => "MulliganBottomCards",
            WaitingFor::OpeningHandBottomCards { .. } => "OpeningHandBottomCards",
            WaitingFor::ManaPayment { .. } => "ManaPayment",
            WaitingFor::ChooseXValue { .. } => "ChooseXValue",
            WaitingFor::TargetSelection { .. } => "TargetSelection",
            WaitingFor::DeclareAttackers { .. } => "DeclareAttackers",
            WaitingFor::DeclareBlockers { .. } => "DeclareBlockers",
            WaitingFor::UntapChoice { .. } => "UntapChoice",
            WaitingFor::ExertChoice { .. } => "ExertChoice",
            WaitingFor::EnlistChoice { .. } => "EnlistChoice",
            WaitingFor::GameOver { .. } => "GameOver",
            WaitingFor::ReplacementChoice { .. } => "ReplacementChoice",
            WaitingFor::OrderTriggers { .. } => "OrderTriggers",
            WaitingFor::CopyTargetChoice { .. } => "CopyTargetChoice",
            WaitingFor::ExploreChoice { .. } => "ExploreChoice",
            WaitingFor::ReturnAsAuraTarget { .. } => "ReturnAsAuraTarget",
            WaitingFor::EquipTarget { .. } => "EquipTarget",
            WaitingFor::CrewVehicle { .. } => "CrewVehicle",
            WaitingFor::StationTarget { .. } => "StationTarget",
            WaitingFor::SaddleMount { .. } => "SaddleMount",
            WaitingFor::ScryChoice { .. } => "ScryChoice",
            WaitingFor::CoinFlipKeepChoice { .. } => "CoinFlipKeepChoice",
            WaitingFor::DigChoice { .. } => "DigChoice",
            WaitingFor::SurveilChoice { .. } => "SurveilChoice",
            WaitingFor::RevealChoice { .. } => "RevealChoice",
            WaitingFor::SearchChoice { .. } => "SearchChoice",
            WaitingFor::SearchPartitionChoice { .. } => "SearchPartitionChoice",
            WaitingFor::OutsideGameChoice { .. } => "OutsideGameChoice",
            WaitingFor::ChooseFromZoneChoice { .. } => "ChooseFromZoneChoice",
            WaitingFor::ChooseOneOfBranch { .. } => "ChooseOneOfBranch",
            WaitingFor::ConniveDiscard { .. } => "ConniveDiscard",
            WaitingFor::DiscardChoice { .. } => "DiscardChoice",
            WaitingFor::EffectZoneChoice { .. } => "EffectZoneChoice",
            WaitingFor::DrawnThisTurnTopdeckChoice { .. } => "DrawnThisTurnTopdeckChoice",
            WaitingFor::LearnChoice { .. } => "LearnChoice",
            WaitingFor::ManifestDreadChoice { .. } => "ManifestDreadChoice",
            WaitingFor::TriggerTargetSelection { .. } => "TriggerTargetSelection",
            WaitingFor::BetweenGamesSideboard { .. } => "BetweenGamesSideboard",
            WaitingFor::BetweenGamesChoosePlayDraw { .. } => "BetweenGamesChoosePlayDraw",
            WaitingFor::NamedChoice { .. } => "NamedChoice",
            WaitingFor::SpellbookDraft { .. } => "SpellbookDraft",
            WaitingFor::DamageSourceChoice { .. } => "DamageSourceChoice",
            WaitingFor::ModeChoice { .. } => "ModeChoice",
            WaitingFor::DiscardToHandSize { .. } => "DiscardToHandSize",
            WaitingFor::OptionalCostChoice { .. } => "OptionalCostChoice",
            WaitingFor::SpliceOffer { .. } => "SpliceOffer",
            WaitingFor::DefilerPayment { .. } => "DefilerPayment",
            WaitingFor::CastOffer { .. } => "CastOffer",
            WaitingFor::ModalFaceChoice { .. } => "ModalFaceChoice",
            WaitingFor::AlternativeCastChoice { .. } => "AlternativeCastChoice",
            WaitingFor::MutateMergeChoice { .. } => "MutateMergeChoice",
            WaitingFor::CipherEncodeChoice { .. } => "CipherEncodeChoice",
            WaitingFor::CastingVariantChoice { .. } => "CastingVariantChoice",
            WaitingFor::ChoosePermanentTypeSlot { .. } => "ChoosePermanentTypeSlot",
            WaitingFor::MultiTargetSelection { .. } => "MultiTargetSelection",
            WaitingFor::AbilityModeChoice { .. } => "AbilityModeChoice",
            WaitingFor::OptionalEffectChoice { .. } => "OptionalEffectChoice",
            WaitingFor::PairChoice { .. } => "PairChoice",
            WaitingFor::TributeChoice { .. } => "TributeChoice",
            WaitingFor::MiracleReveal { .. } => "MiracleReveal",
            WaitingFor::OpponentMayChoice { .. } => "OpponentMayChoice",
            WaitingFor::UnlessPayment { .. } => "UnlessPayment",
            WaitingFor::UnlessPaymentChooseCost { .. } => "UnlessPaymentChooseCost",
            WaitingFor::WardDiscardChoice { .. } => "WardDiscardChoice",
            WaitingFor::WardSacrificeChoice { .. } => "WardSacrificeChoice",
            WaitingFor::UnlessBounceChoice { .. } => "UnlessBounceChoice",
            WaitingFor::ChooseRingBearer { .. } => "ChooseRingBearer",
            WaitingFor::ChooseDungeon { .. } => "ChooseDungeon",
            WaitingFor::ChooseDungeonRoom { .. } => "ChooseDungeonRoom",
            WaitingFor::SpecializeColor { .. } => "SpecializeColor",
            WaitingFor::PayCost { .. } => "PayCost",
            WaitingFor::ActivationCostOneOfChoice { .. } => "ActivationCostOneOfChoice",
            WaitingFor::BlightChoice { .. } => "BlightChoice",
            WaitingFor::PayManaAbilityMana { .. } => "PayManaAbilityMana",
            WaitingFor::ChooseManaColor { .. } => "ChooseManaColor",
            WaitingFor::CollectEvidenceChoice { .. } => "CollectEvidenceChoice",
            WaitingFor::HarmonizeTapChoice { .. } => "HarmonizeTapChoice",
            WaitingFor::RevealUntilKeptChoice { .. } => "RevealUntilKeptChoice",
            WaitingFor::RepeatDecision { .. } => "RepeatDecision",
            WaitingFor::TopOrBottomChoice { .. } => "TopOrBottomChoice",
            WaitingFor::PopulateChoice { .. } => "PopulateChoice",
            WaitingFor::ClashChooseOpponent { .. } => "ClashChooseOpponent",
            WaitingFor::ClashCardPlacement { .. } => "ClashCardPlacement",
            WaitingFor::VoteChoice { .. } => "VoteChoice",
            WaitingFor::SeparatePilesPartition { .. } => "SeparatePilesPartition",
            WaitingFor::SeparatePilesChoice { .. } => "SeparatePilesChoice",
            WaitingFor::CompanionReveal { .. } => "CompanionReveal",
            WaitingFor::ChooseLegend { .. } => "ChooseLegend",
            WaitingFor::CommanderZoneChoice { .. } => "CommanderZoneChoice",
            WaitingFor::BattleProtectorChoice { .. } => "BattleProtectorChoice",
            WaitingFor::ProliferateChoice { .. } => "ProliferateChoice",
            WaitingFor::TimeTravelChoice { .. } => "TimeTravelChoice",
            WaitingFor::AssistChoosePlayer { .. } => "AssistChoosePlayer",
            WaitingFor::AssistPayment { .. } => "AssistPayment",
            WaitingFor::ChooseObjectsSelection { .. } => "ChooseObjectsSelection",
            WaitingFor::CategoryChoice { .. } => "CategoryChoice",
            WaitingFor::CopyRetarget { .. } => "CopyRetarget",
            WaitingFor::AssignCombatDamage { .. } => "AssignCombatDamage",
            WaitingFor::AssignBlockerDamage { .. } => "AssignBlockerDamage",
            WaitingFor::DistributeAmong { .. } => "DistributeAmong",
            WaitingFor::MoveCountersDistribution { .. } => "MoveCountersDistribution",
            WaitingFor::PayAmountChoice { .. } => "PayAmountChoice",
            WaitingFor::RetargetChoice { .. } => "RetargetChoice",
            WaitingFor::CombatTaxPayment { .. } => "CombatTaxPayment",
            WaitingFor::PhyrexianPayment { .. } => "PhyrexianPayment",
        }
    }

    /// Extract the player who must act, if any.
    ///
    /// CR 103.5: For simultaneous-decision states (`MulliganDecision`,
    /// `MulliganBottomCards`, `OpeningHandBottomCards`) this returns `Some(p)` only when exactly one
    /// player is pending, and `None` when multiple are pending — callers
    /// that need set semantics must use [`Self::acting_players`] instead.
    pub fn acting_player(&self) -> Option<PlayerId> {
        match self {
            WaitingFor::MulliganDecision { pending, .. } => {
                if pending.len() == 1 {
                    Some(pending[0].player)
                } else {
                    None
                }
            }
            WaitingFor::MulliganBottomCards { pending } => {
                if pending.len() == 1 {
                    Some(pending[0].player)
                } else {
                    None
                }
            }
            WaitingFor::OpeningHandBottomCards { pending, .. } => {
                if pending.len() == 1 {
                    Some(pending[0].player)
                } else {
                    None
                }
            }
            WaitingFor::Priority { player }
            | WaitingFor::ManaPayment { player, .. }
            | WaitingFor::ChooseXValue { player, .. }
            | WaitingFor::TargetSelection { player, .. }
            | WaitingFor::DeclareAttackers { player, .. }
            | WaitingFor::DeclareBlockers { player, .. }
            | WaitingFor::UntapChoice { player, .. }
            | WaitingFor::ExertChoice { player, .. }
            | WaitingFor::EnlistChoice { player, .. }
            | WaitingFor::ReplacementChoice { player, .. }
            | WaitingFor::OrderTriggers { player, .. }
            | WaitingFor::CopyTargetChoice { player, .. }
            | WaitingFor::ExploreChoice { player, .. }
            | WaitingFor::ReturnAsAuraTarget { player, .. }
            | WaitingFor::EquipTarget { player, .. }
            | WaitingFor::CrewVehicle { player, .. }
            | WaitingFor::StationTarget { player, .. }
            | WaitingFor::SaddleMount { player, .. }
            | WaitingFor::ScryChoice { player, .. }
            | WaitingFor::CoinFlipKeepChoice { player, .. }
            | WaitingFor::DigChoice { player, .. }
            | WaitingFor::SurveilChoice { player, .. }
            | WaitingFor::RevealChoice { player, .. }
            | WaitingFor::SearchChoice { player, .. }
            | WaitingFor::SearchPartitionChoice { player, .. }
            | WaitingFor::OutsideGameChoice { player, .. }
            | WaitingFor::ChooseFromZoneChoice { player, .. }
            | WaitingFor::ChooseOneOfBranch { player, .. }
            | WaitingFor::LearnChoice { player, .. }
            | WaitingFor::ManifestDreadChoice { player, .. }
            | WaitingFor::EffectZoneChoice { player, .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { player, .. }
            | WaitingFor::TriggerTargetSelection { player, .. }
            | WaitingFor::BetweenGamesSideboard { player, .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { player, .. }
            | WaitingFor::NamedChoice { player, .. }
            | WaitingFor::SpellbookDraft { player, .. }
            | WaitingFor::DamageSourceChoice { player, .. }
            | WaitingFor::ModeChoice { player, .. }
            | WaitingFor::DiscardToHandSize { player, .. }
            | WaitingFor::OptionalCostChoice { player, .. }
            | WaitingFor::SpliceOffer { player, .. }
            | WaitingFor::DefilerPayment { player, .. }
            | WaitingFor::AbilityModeChoice { player, .. }
            | WaitingFor::MultiTargetSelection { player, .. }
            | WaitingFor::CastOffer { player, .. }
            | WaitingFor::ModalFaceChoice { player, .. }
            | WaitingFor::AlternativeCastChoice { player, .. }
            | WaitingFor::MutateMergeChoice { player, .. }
            | WaitingFor::CipherEncodeChoice { player, .. }
            | WaitingFor::CastingVariantChoice { player, .. }
            | WaitingFor::ChoosePermanentTypeSlot { player, .. }
            | WaitingFor::ChooseRingBearer { player, .. }
            | WaitingFor::ChooseDungeon { player, .. }
            | WaitingFor::ChooseDungeonRoom { player, .. }
            | WaitingFor::SpecializeColor { player, .. }
            | WaitingFor::PayCost { player, .. }
            | WaitingFor::ActivationCostOneOfChoice { player, .. }
            | WaitingFor::BlightChoice { player, .. }
            | WaitingFor::PayManaAbilityMana { player, .. }
            | WaitingFor::ChooseManaColor { player, .. }
            | WaitingFor::CollectEvidenceChoice { player, .. }
            | WaitingFor::HarmonizeTapChoice { player, .. }
            | WaitingFor::OptionalEffectChoice { player, .. }
            | WaitingFor::PairChoice { player, .. }
            | WaitingFor::OpponentMayChoice { player, .. }
            | WaitingFor::TributeChoice { player, .. }
            | WaitingFor::UnlessPayment { player, .. }
            | WaitingFor::UnlessPaymentChooseCost { player, .. }
            | WaitingFor::RevealUntilKeptChoice { player, .. }
            | WaitingFor::RepeatDecision { player, .. }
            | WaitingFor::TopOrBottomChoice { player, .. }
            | WaitingFor::PopulateChoice { player, .. }
            | WaitingFor::ClashChooseOpponent { player, .. }
            | WaitingFor::ClashCardPlacement { player, .. }
            | WaitingFor::CompanionReveal { player, .. }
            | WaitingFor::ChooseLegend { player, .. }
            | WaitingFor::BattleProtectorChoice { player, .. }
            | WaitingFor::ProliferateChoice { player, .. }
            | WaitingFor::TimeTravelChoice { player, .. }
            | WaitingFor::AssistChoosePlayer { player, .. }
            | WaitingFor::ChooseObjectsSelection { player, .. }
            | WaitingFor::CategoryChoice { player, .. }
            | WaitingFor::CopyRetarget { player, .. }
            | WaitingFor::AssignCombatDamage { player, .. }
            | WaitingFor::AssignBlockerDamage { player, .. }
            | WaitingFor::DistributeAmong { player, .. }
            | WaitingFor::MoveCountersDistribution { player, .. }
            | WaitingFor::PayAmountChoice { player, .. }
            | WaitingFor::RetargetChoice { player, .. }
            | WaitingFor::WardDiscardChoice { player, .. }
            | WaitingFor::WardSacrificeChoice { player, .. }
            | WaitingFor::UnlessBounceChoice { player, .. }
            | WaitingFor::ConniveDiscard { player, .. }
            | WaitingFor::CombatTaxPayment { player, .. }
            | WaitingFor::PhyrexianPayment { player, .. }
            | WaitingFor::DiscardChoice { player, .. }
            | WaitingFor::MiracleReveal { player, .. }
            | WaitingFor::CommanderZoneChoice { player, .. }
            | WaitingFor::SeparatePilesPartition { player, .. }
            | WaitingFor::SeparatePilesChoice { player, .. } => Some(*player),
            // CR 608.2c: For `ControllerLabels` votes (Battlebond friend-or-foe
            // cards), the ACTOR is the spell controller, not `player` (the
            // subject being labeled). `VoteActor::resolve` returns the
            // authorized submitter without the call site needing to know
            // which voting shape this is.
            WaitingFor::VoteChoice { player, actor, .. } => Some(actor.resolve(*player)),
            // CR 702.132a: the assisting (chosen) player acts on the payment step,
            // not the caster — route authorization to them.
            WaitingFor::AssistPayment { chosen, .. } => Some(*chosen),
            WaitingFor::GameOver { .. } => None,
        }
    }

    /// CR 103.5: Set of players who are currently authorized to act in this
    /// `WaitingFor` state. For all single-player-pending variants this returns
    /// a single-element Vec containing [`Self::acting_player`]. For the
    /// simultaneous mulligan variants this returns every player still pending.
    ///
    /// Engine authorization checks should use this in preference to
    /// `acting_player()` so the simultaneous variants accept actions from any
    /// of the pending players in any arrival order.
    pub fn acting_players(&self) -> Vec<PlayerId> {
        match self {
            WaitingFor::MulliganDecision { pending, .. } => {
                pending.iter().map(|e| e.player).collect()
            }
            WaitingFor::MulliganBottomCards { pending } => {
                pending.iter().map(|e| e.player).collect()
            }
            WaitingFor::OpeningHandBottomCards { pending, .. } => {
                pending.iter().map(|e| e.player).collect()
            }
            _ => self.acting_player().into_iter().collect(),
        }
    }

    /// Returns a reference to the pending cast embedded in this state, if any.
    ///
    /// This is the single authority on which `WaitingFor` variants carry an
    /// inline `PendingCast`. `has_pending_cast()` delegates here.
    ///
    /// Runtime drift detector: the `debug_assert!` in `game::derived` trips
    /// in tests if a new variant populates `GameState::pending_cast` without
    /// being covered here (or by the `ManaPayment` exception in
    /// `has_pending_cast`). That is the practical safeguard — the `_ => None`
    /// wildcard below does not compile-enforce variant coverage on its own.
    ///
    /// Note: `ManaPayment` is the one casting-flow variant that does NOT embed
    /// its `PendingCast`. It reads from `GameState::pending_cast` instead so
    /// multiplayer visibility filtering (`game::visibility`) can clear
    /// mid-payment detail for opponents while preserving the public "spell on
    /// the stack" view elsewhere. `has_pending_cast()` accounts for this.
    pub fn pending_cast_ref(&self) -> Option<&PendingCast> {
        match self {
            WaitingFor::ChooseXValue { pending_cast, .. }
            | WaitingFor::TargetSelection { pending_cast, .. }
            | WaitingFor::ModeChoice { pending_cast, .. }
            | WaitingFor::OptionalCostChoice { pending_cast, .. }
            | WaitingFor::SpliceOffer { pending_cast, .. }
            | WaitingFor::DefilerPayment { pending_cast, .. }
            | WaitingFor::ActivationCostOneOfChoice { pending_cast, .. }
            | WaitingFor::BlightChoice { pending_cast, .. }
            | WaitingFor::HarmonizeTapChoice { pending_cast, .. } => Some(pending_cast),
            WaitingFor::PayCost { resume, .. } => match resume {
                CostResume::Spell {
                    spell: pending_cast,
                }
                | CostResume::SpellCost {
                    spell: pending_cast,
                    ..
                } => Some(pending_cast),
                CostResume::ManaAbility { .. } => None,
            },
            WaitingFor::CollectEvidenceChoice { resume, .. } => match resume.as_ref() {
                CollectEvidenceResume::Casting { pending_cast } => Some(pending_cast),
                CollectEvidenceResume::Effect { .. } => None,
            },
            _ => None,
        }
    }

    /// Mutable variant of `pending_cast_ref()` for call sites that need to
    /// annotate in-flight cast metadata (for example rollback markers).
    pub fn pending_cast_mut(&mut self) -> Option<&mut PendingCast> {
        match self {
            WaitingFor::ChooseXValue { pending_cast, .. }
            | WaitingFor::TargetSelection { pending_cast, .. }
            | WaitingFor::ModeChoice { pending_cast, .. }
            | WaitingFor::OptionalCostChoice { pending_cast, .. }
            | WaitingFor::SpliceOffer { pending_cast, .. }
            | WaitingFor::DefilerPayment { pending_cast, .. }
            | WaitingFor::ActivationCostOneOfChoice { pending_cast, .. }
            | WaitingFor::BlightChoice { pending_cast, .. }
            | WaitingFor::HarmonizeTapChoice { pending_cast, .. } => Some(pending_cast),
            WaitingFor::PayCost { resume, .. } => match resume {
                CostResume::Spell {
                    spell: pending_cast,
                }
                | CostResume::SpellCost {
                    spell: pending_cast,
                    ..
                } => Some(pending_cast),
                CostResume::ManaAbility { .. } => None,
            },
            WaitingFor::CollectEvidenceChoice { resume, .. } => match resume.as_mut() {
                CollectEvidenceResume::Casting { pending_cast } => Some(pending_cast),
                CollectEvidenceResume::Effect { .. } => None,
            },
            _ => None,
        }
    }

    /// Whether this state is part of the casting flow and can be backed out of
    /// with `CancelCast` (CR 601.2).
    ///
    /// Derived from `pending_cast_ref()` plus the single `ManaPayment`
    /// exception (which externalizes its `PendingCast` into
    /// `GameState::pending_cast`). Centralizing the predicate here guarantees
    /// that every variant carrying a `PendingCast` is covered — drift between
    /// data model and predicate is structurally prevented.
    ///
    /// `TapCreaturesForManaAbility` is intentionally NOT a cast state: it
    /// carries a `PendingManaAbility`, not a `PendingCast`, and the engine
    /// does not accept `CancelCast` during that step. A mana ability activated
    /// inside a spell's mana payment still routes the cast via the outer
    /// `ManaPayment` state (which is a cast state).
    pub fn has_pending_cast(&self) -> bool {
        self.pending_cast_ref().is_some()
            || matches!(
                self,
                WaitingFor::ManaPayment { .. } | WaitingFor::PhyrexianPayment { .. }
            )
    }

    /// Look-at-top-N states whose legal selections cannot be captured by the
    /// candidate enumerator (it lists only {empty, full-in-original-order,
    /// singletons}), so the multiplayer legality gate would wrongly reject a
    /// legal reordered or partial selection. For these, `apply()` is the real
    /// validation boundary and validates the submitted selection structurally
    /// (see handle_resolution_choice); the server bypasses its enumeration gate.
    ///
    /// - CR 701.22a / CR 701.25a: scry/surveil keep the chosen cards on top
    ///   "in any order" — any duplicate-free subset, in any order, is legal.
    /// - Dig (look at N, keep some): the handler enforces the keep_count /
    ///   up_to constraint, uniqueness, and the selectable-cards filter, and
    ///   preserves the chosen order for library-destined keeps.
    pub fn accepts_freeform_card_selection(&self) -> bool {
        matches!(
            self,
            WaitingFor::ScryChoice { .. }
                | WaitingFor::SurveilChoice { .. }
                | WaitingFor::DigChoice { .. }
        )
    }

    pub fn accepts_freeform_counter_move_distribution(&self) -> bool {
        matches!(self, WaitingFor::MoveCountersDistribution { .. })
    }

    /// Combat-damage assignment whose legal divisions cannot be captured by the
    /// candidate enumerator. `candidates.rs` lists exactly one
    /// `AssignCombatDamage` candidate (the greedy trample-through split), so the
    /// multiplayer legality gate would wrongly reject every other legal division
    /// — e.g. keeping excess on the blocker instead of trampling it through
    /// (CR 702.19b), or any of the freely-chosen splits across multiple blockers
    /// (CR 510.1c/d). The combinatorial space of legal divisions is too large to
    /// enumerate, so `apply()` (handle_assign_combat_damage) is the real
    /// validation boundary: it enforces total conservation, blocker membership,
    /// and the CR 702.19b lethal-before-excess precondition, and rejects illegal
    /// submissions. The server bypasses its enumeration gate for these.
    pub fn accepts_freeform_combat_damage_assignment(&self) -> bool {
        matches!(self, WaitingFor::AssignCombatDamage { .. })
    }

    /// CR 510.1d + CR 702.22k: A blocker's free division of its combat damage
    /// among the attackers it blocks cannot be captured by the candidate
    /// enumerator (the combinatorial space of legal divisions is too large to
    /// enumerate), so the server bypasses its enumeration gate for this state
    /// and `apply()` (handle_assign_blocker_damage) is the real validation
    /// boundary: it enforces total conservation and blocked-attacker membership.
    pub fn accepts_freeform_blocker_damage_assignment(&self) -> bool {
        matches!(self, WaitingFor::AssignBlockerDamage { .. })
    }
}

/// What the frontend requests for auto-pass (no internal state).
///
/// Phase stops that should interrupt `UntilEndOfTurn` are a separate per-player
/// preference on `GameState::phase_stops`, managed via `GameAction::SetPhaseStops`.
/// Keeping them out of the request preserves a single source of truth and lets
/// the preference change mid-session without requiring a new auto-pass request.
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
    /// Auto-pass through priority/combat stops until the flagged player's next
    /// turn starts. Interrupted by opponent stack activity (MTGA-style) or when
    /// the current phase matches the player's entry in `GameState::phase_stops`.
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
    /// Returns `None` for permanent spells with no spell-level effect, and for
    /// `KeywordAction` entries which carry a typed payload instead of a
    /// `ResolvedAbility`.
    pub fn ability(&self) -> Option<&ResolvedAbility> {
        match &self.kind {
            StackEntryKind::Spell { ability, .. } => ability.as_ref(),
            StackEntryKind::ActivatedAbility { ability, .. } => Some(ability),
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability),
            StackEntryKind::KeywordAction { .. } => None,
        }
    }

    /// Access the resolved ability for this stack entry (mutable).
    /// Returns `None` for permanent spells with no spell-level effect, and for
    /// `KeywordAction` entries which carry a typed payload instead of a
    /// `ResolvedAbility`.
    pub fn ability_mut(&mut self) -> Option<&mut ResolvedAbility> {
        match &mut self.kind {
            StackEntryKind::Spell { ability, .. } => ability.as_mut(),
            StackEntryKind::ActivatedAbility { ability, .. } => Some(ability),
            StackEntryKind::TriggeredAbility { ability, .. } => Some(ability),
            StackEntryKind::KeywordAction { .. } => None,
        }
    }
}

/// CR 702.94a + CR 603.11: A pending miracle reveal offer queued during the
/// resolution of an action that caused `player` to draw `object_id` as their
/// first card of the turn. `cost` is the miracle mana cost taken from the
/// card's `Keyword::Miracle(ManaCost)` payload at queue time — captured here
/// so the reveal prompt stays accurate even if the keyword is later removed
/// mid-resolution by a replacement or layer effect.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MiracleOffer {
    pub player: PlayerId,
    pub object_id: ObjectId,
    pub cost: super::mana::ManaCost,
}

/// CR 702.190b: Placement data for a Sneak-cast **permanent** spell —
/// captures the `(defender, attack_target)` pair from the returned creature's
/// `AttackerInfo` at cost-payment time, so the permanent can enter the
/// battlefield attacking the same target after resolution (by which point
/// combat no longer remembers the returned creature).
///
/// Absent for instant/sorcery Sneak casts (CR 702.190b applies only to
/// permanent spells).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SneakPlacement {
    pub defender: PlayerId,
    pub attack_target: AttackTarget,
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
    /// CR 720.3d / CR 720.4: Cast as the Omen half. On resolution, shuffled
    /// into its owner's library with normal characteristics restored.
    Omen,
    /// CR 702.185a: Cast via Warp alternative cost from hand. On resolution,
    /// creates a delayed trigger to exile at end step with WarpExile permission.
    Warp,
    /// CR 702.138: Cast from graveyard via Escape. On resolution, goes to
    /// appropriate zone normally (unlike Flashback which exiles).
    Escape,
    /// CR 702.81a: Cast from graveyard via Retrace by discarding a land card as
    /// an additional cost. Resolution uses normal spell routing.
    Retrace,
    /// CR 702.180a: Cast from graveyard for harmonize cost. On resolution, exiled
    /// instead of going anywhere else (unlike Escape which returns to graveyard).
    Harmonize,
    /// CR 702.187b: Cast from graveyard for mayhem cost (allowed only while the
    /// card was discarded this turn). Unlike Flashback/Harmonize, the spell is
    /// NOT exiled — it resolves normally (like Escape), so it can be discarded
    /// and recast again on a later turn.
    Mayhem,
    /// CR 702.34a: Cast from graveyard for flashback cost. On resolution (or
    /// whenever leaving the stack for any reason), exiled instead of going anywhere else.
    Flashback,
    /// CR 702.127a: Cast an aftermath half of a split card from a graveyard.
    /// If it was cast from a graveyard, exile it any time it leaves the stack.
    Aftermath,
    /// CR 702.146a-b + CR 712.8c: Cast transformed from graveyard for disturb
    /// cost. The stack spell uses its back-face characteristics and the
    /// permanent enters the battlefield back face up on resolution.
    Disturb,
    /// CR 601.2a: Cast from graveyard via a static permission source (e.g. Lurrus).
    /// Stores the granting permanent's ObjectId for per-turn tracking.
    /// CR 400.7: Zone change creates new ObjectId, naturally resetting permission.
    GraveyardPermission {
        source: ObjectId,
        /// CR 601.2a: When `OncePerTurn`, casting consumes this source's slot in
        /// `graveyard_cast_permissions_used`. `Unlimited` permissions (Conduit)
        /// skip tracking entirely. When `OncePerTurnPerPermanentType` (Muldrotha),
        /// casting consumes the `(source, slot_type)` entry in
        /// `graveyard_cast_permissions_used_per_type` — see `slot_type`.
        frequency: super::statics::CastFrequency,
        /// CR 110.4: Permanent type slot consumed when `frequency` is
        /// `OncePerTurnPerPermanentType`. Always one of the six CR 110.4
        /// permanent types. `None` for `Unlimited` and `OncePerTurn`
        /// frequencies (those track by source only).
        #[serde(default)]
        slot_type: Option<super::card_type::CoreType>,
        /// CR 614.1a: Some graveyard cast permissions add "If a spell cast
        /// this way would be put into your graveyard, exile it instead."
        /// This replaces only stack-to-graveyard destinations.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graveyard_destination_replacement: Option<Zone>,
    },
    /// CR 601.2b + CR 118.9a: Cast from hand via a `CastFromHandFree` static
    /// permission source (Zaffai). Stores the granting permanent's ObjectId for
    /// per-turn tracking. Omniscience's unconditional silent path does not use
    /// this variant — it short-circuits the mana cost to NoCost while leaving
    /// `casting_variant = Normal`.
    /// CR 400.7: Zone change creates new ObjectId, naturally resetting permission.
    HandPermission {
        source: ObjectId,
        /// CR 601.2b: When `OncePerTurn`, casting consumes this source's slot in
        /// `hand_cast_free_permissions_used`.
        frequency: super::statics::CastFrequency,
    },
    /// CR 601.2a + CR 113.6b + CR 118.9a: Cast from exile via a
    /// `StaticMode::ExileCastPermission` source (Maralen, Fae Ascendant).
    /// Stores the granting permanent's ObjectId for per-turn tracking; the
    /// finalize-cast step zeroes the spell's mana cost when the static carries
    /// `without_paying_mana_cost: true` (the only published shape today). The
    /// resolution-time routing matches a normal cast — no on-resolve exile
    /// behavior — so this is treated as a casting-context tag, not as an
    /// alternative cost.
    /// CR 400.7: Zone change creates a new source `ObjectId`, naturally
    /// resetting the per-turn slot when the source leaves and re-enters play.
    ExilePermission {
        source: ObjectId,
        /// CR 601.2a: When `OncePerTurn`, casting consumes this source's slot
        /// in `exile_cast_permissions_used`. `Unlimited` skips tracking.
        frequency: super::statics::CastFrequency,
    },
    /// CR 702.190a: Cast from HAND via the Sneak alternative cost. Legal only
    /// during the declare-blockers step. The returned unblocked attacker you
    /// control is part of the cost, bounced to its owner's hand at
    /// `finalize_cast_to_stack`.
    ///
    /// CR 702.190b applies only to **permanent spells**: on resolution the
    /// permanent enters tapped and attacking the same defender as the
    /// returned creature. Non-permanent spells (instants/sorceries) resolve
    /// normally with no alongside-attacker placement, so `placement` is
    /// `None` for those casts. This `Option` carries real per-card-class data
    /// (not a discriminator) — see `SneakPlacement`.
    Sneak {
        returned_creature: ObjectId,
        /// CR 702.190b data for permanent spells; `None` for instants/sorceries.
        placement: Option<SneakPlacement>,
    },
    /// CR 702.188a: Cast from hand via Web-slinging's alternative cost by
    /// returning a tapped creature you control to its owner's hand rather than
    /// paying the spell's mana cost. Unlike Sneak, Web-slinging grants no
    /// special timing permission and has no enter-attacking placement rule.
    WebSlinging { returned_creature: ObjectId },
    /// CR 702.94a: Cast from hand via Miracle's alternative cost after revealing
    /// the card as the first card drawn this turn. The granting keyword carries
    /// the miracle mana cost, which `prepare_spell_cast_with_variant_override`
    /// substitutes for the printed mana cost. The keyword's `ManaCost` payload
    /// is read at preparation time rather than stored here because
    /// `prepare_spell_cast` already reads `obj.keywords` for analogous paths.
    Miracle,
    /// CR 702.35a: Cast from exile via Madness after the discard replacement
    /// exiled the card and its madness triggered ability resolved.
    Madness,
    /// CR 702.74a: Cast from hand via Evoke's alternative cost. On resolution,
    /// the permanent enters tagged with `CastVariantPaid::Evoke`, which fires
    /// the synthesized intervening-if ETB sacrifice trigger.
    Evoke,
    /// CR 702.119a-c: Cast from hand via Emerge's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Emerge(cost)` at cast preparation;
    /// casting requires sacrificing a creature, then reduces that emerge cost by
    /// the sacrificed creature's mana value. Resolution routing matches a normal
    /// cast; Emerge has no resolution rider.
    Emerge,
    /// CR 702.109a: Cast from hand via Dash's alternative cost. On resolution,
    /// `dash::install_dash_riders` grants the permanent haste and schedules a
    /// next-end-step return to its owner's hand.
    Dash,
    /// CR 702.152a: Cast from hand via Blitz's alternative cost. On resolution,
    /// `blitz::install_blitz_riders` grants the permanent haste and a dies-draw
    /// trigger and schedules a next-end-step sacrifice.
    Blitz,
    /// CR 702.137a: Cast from hand via Spectacle's alternative cost, available
    /// only if an opponent lost life this turn. A pure cost substitution — the
    /// spell resolves normally with no resolution riders.
    Spectacle,
    /// CR 702.62a: Cast from exile via Suspend's "play it without paying its
    /// mana cost" trigger after the last time counter was removed. On resolution
    /// of the resulting permanent, the stack handler tags
    /// `CastVariantPaid::Suspend` and — for creature spells — installs a
    /// transient continuous "has haste" effect that lasts as long as the
    /// resolution-time controller still controls the permanent.
    Suspend,
    /// CR 702.170d: Cast from exile via the Plot "cast without paying its mana
    /// cost" permission during the owner's main phase on a turn after the card
    /// was plotted. Detected at cast preparation when the exile-zone source has
    /// a `CastingPermission::Plotted { turn_plotted }` and the current turn is
    /// strictly greater than `turn_plotted`. Zeroes the mana cost and routes
    /// through the normal cast pipeline; no special resolution-time behavior.
    Plot,
    /// CR 702.143a-c: Cast from exile via a foretold card's foretell cost on a
    /// turn after it was foretold. Detected at cast preparation when the
    /// exile-zone source has a `CastingPermission::Foretold { .. }`. The
    /// permission supplies the alternative mana cost; stack finalization tags
    /// the source with `CastVariantPaid::Foretell` so "if this spell was
    /// foretold" clauses can evaluate while the spell resolves.
    Foretell,
    /// CR 702.96a-c: Cast from hand via Overload's alternative cost. The
    /// printed mana cost is replaced by `Keyword::Overload(cost)` at cast
    /// preparation (mirrors `Evoke`/`Warp`). Per CR 702.96b, every "target"
    /// in the spell's text is replaced by "each" — applied as a cast-time
    /// transformation of the spell's ability tree (`Destroy`→`DestroyAll`,
    /// `Pump`→`PumpAll`, `DealDamage`→`DamageAll`, `Tap`→`TapAll`,
    /// `Bounce`→`ChangeZoneAll`). Per CR 702.96c, the resulting spell has
    /// no targets, so target selection is naturally skipped because the
    /// transformed effects carry no `TargetRef` slots.
    Overload,
    /// CR 702.103a-b: Cast from hand via Bestow's alternative cost. The
    /// printed mana cost is replaced by `Keyword::Bestow(cost)` at cast
    /// preparation; the spell becomes an Aura with `enchant creature` while
    /// on the stack and as the resulting permanent (until it becomes
    /// unattached, per CR 702.103f). The type-changing mutation is applied
    /// directly to the stack object (mirroring `swap_to_alternative_spell_face`
    /// for Adventure/Omen) — Layers cannot be used here because they only
    /// apply to battlefield/hand objects, not stack objects.
    ///
    /// Per CR 702.103e: if the target is illegal at resolution, the
    /// type-changing effect ends and the spell resolves as a creature spell.
    /// Per CR 702.103f: when a bestowed Aura becomes unattached on the
    /// battlefield, the type-changing effect ends — it remains as an
    /// enchantment creature (overrides CR 704.5m for bestow Auras).
    Bestow,
    /// CR 702.113a: Cast from hand via Awaken's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Awaken { cost }` at cast preparation
    /// (mirrors `Overload`). A resolution rider is appended to the tail of the
    /// spell's ability tree (`effects::awaken::append_awaken_rider`): the
    /// printed effect resolves first, then "put N +1/+1 counters on target land
    /// you control; that land becomes a 0/0 Elemental creature with haste; it's
    /// still a land." Per CR 702.113b, the land target only exists on the awaken
    /// variant — a normal cast appends no rider and requests no land target.
    /// CR 702.113a: the spell goes to the graveyard normally, so this variant is
    /// deliberately absent from `exiles_when_leaving_stack_for_any_reason`.
    Awaken,
    /// CR 702.148a-b + CR 612: Cast from hand via Cleave's alternative cost
    /// (CR 118.9). The printed mana cost is replaced by `Keyword::Cleave(cost)`
    /// at cast preparation (mirrors `Evoke`/`Overload`). Per CR 702.148a, paying
    /// the cleave cost is a text-changing effect (CR 612) that removes every
    /// square-bracketed span from the spell's rules text. The bracket-removed
    /// ability set is parsed at build time into `CardFace::cleave_variant` and
    /// swapped onto the stack object before preparation (mirroring the Bestow
    /// object-mutation-before-prepare seam). Resolution routing matches a normal
    /// spell — there is no on-resolve special behavior, so the spell goes to its
    /// owner's graveyard like any instant/sorcery.
    Cleave,
    /// CR 702.162a + CR 712.14a: Cast from any castable zone via the More Than
    /// Meets the Eye alternative cost. The printed mana cost is replaced by the
    /// `Keyword::MoreThanMeetsTheEye(cost)` payload at cast preparation (mirrors
    /// Overload). On resolution the spell is cast CONVERTED — the resulting
    /// permanent enters the battlefield transformed (back face up) via the
    /// existing `enter_transformed` ZoneChange seed. CR 701.28 (Convert).
    MoreThanMeetsTheEye,
    /// CR 702.176a: Cast from hand via Impending's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Impending { cost, .. }` at cast
    /// preparation (mirrors Overload/Evoke). On resolution the permanent enters
    /// with N time counters (from the keyword) and is not a creature while any
    /// remain. At the beginning of your end step one time counter is removed.
    Impending,
    /// CR 702.160a: Cast from hand prototyped. The printed mana cost is replaced
    /// by the prototype cost during cast preparation, and the object is tagged so
    /// stack display plus layer evaluation use the secondary mana cost and P/T
    /// while it is a creature.
    Prototype,
    /// CR 702.140a-c: Cast from hand via Mutate's alternative cost. The printed
    /// mana cost is replaced by `Keyword::Mutate(cost)` at cast preparation
    /// (mirrors Bestow). The spell gains a single target — a non-Human creature
    /// the caster owns (CR 702.140a) — attached Bestow-style before preparation.
    /// On resolution (`stack::resolve_top`): if the target is illegal
    /// (CR 702.140b) the spell reverts to a plain creature spell and enters the
    /// battlefield normally; if legal (CR 702.140c) it does NOT enter — instead
    /// it merges with the target creature (CR 730) and the controller chooses
    /// top/bottom. Unlike Bestow this variant neither exiles on leaving the stack
    /// nor restores a front face, so it is intentionally absent from
    /// `exiles_when_leaving_stack_for_any_reason` and
    /// `restores_front_face_after_stack_exit`.
    Mutate,
    /// CR 702.173a: Cast from hand via Freerunning's alternative cost. Legal
    /// only when a player was dealt combat damage this turn by an Assassin
    /// creature or a commander under the caster's control. The printed mana
    /// cost is replaced by the `Keyword::Freerunning(cost)` payload at cast
    /// preparation (mirrors `Overload` / `Foretell`). Resolution routing
    /// matches a normal cast — no on-resolve special behavior — so this is a
    /// casting-context tag, not a resolution-affecting variant.
    Freerunning,
    /// CR 702.76a: Cast from hand via Prowl's alternative cost. Legal only when a
    /// player was dealt combat damage this turn by a source under the caster's
    /// control that, at damage time, had any of this spell's creature types. The
    /// printed mana cost is replaced by the `Keyword::Prowl(cost)` payload at cast
    /// preparation (mirrors `Freerunning`/`Overload`). Resolution routing matches
    /// a normal cast — no on-resolve special behavior — so this is a
    /// casting-context tag, not a resolution-affecting variant.
    Prowl,
    /// CR 702.133a: Cast from a graveyard via Jump-start. The card is cast for
    /// its normal mana cost plus an additional cost of discarding a card
    /// (CR 601.2b/601.2f–h) — so, like `Retrace`/`Aftermath`, this is an
    /// additional cost, not an alternative cost, and is absent from
    /// `uses_alternative_cost`. Like `Flashback`, a spell cast this way is
    /// exiled instead of going anywhere else any time it would leave the stack
    /// (see `exiles_when_leaving_stack_for_any_reason`).
    JumpStart,
    /// CR 702.102a-d: Both halves of a split card cast from hand as a fused
    /// split spell. The mana cost is the combined cost of both halves
    /// (CR 702.102c). On resolution, the left half's instructions are followed
    /// first, then the right half's (CR 702.102d). Not an alternative cost
    /// (CR 118.9a) — the player pays the full combined printed mana cost.
    Fuse,
    /// CR 702.117a: Cast from hand for the surge alternative cost, legal only if
    /// the caster has cast another spell this turn. Resolution is normal (no
    /// exile/restore), so it appears only in `uses_alternative_cost`.
    Surge,
}

impl CastingVariant {
    pub fn is_normal(&self) -> bool {
        *self == CastingVariant::Normal
    }

    /// CR 118.9a: Only one alternative cost can be applied to a spell.
    pub fn uses_alternative_cost(self) -> bool {
        match self {
            CastingVariant::Warp
            | CastingVariant::Escape
            | CastingVariant::Harmonize
            | CastingVariant::Mayhem
            | CastingVariant::Flashback
            | CastingVariant::HandPermission { .. }
            | CastingVariant::Sneak { .. }
            | CastingVariant::WebSlinging { .. }
            | CastingVariant::Miracle
            | CastingVariant::Madness
            | CastingVariant::Evoke
            | CastingVariant::Emerge
            | CastingVariant::Dash
            | CastingVariant::Blitz
            | CastingVariant::Spectacle
            | CastingVariant::Suspend
            | CastingVariant::Plot
            | CastingVariant::Foretell
            | CastingVariant::Overload
            | CastingVariant::Bestow
            | CastingVariant::Awaken
            | CastingVariant::Cleave
            | CastingVariant::MoreThanMeetsTheEye
            | CastingVariant::Disturb
            | CastingVariant::Impending
            | CastingVariant::Prototype
            // CR 702.140a: Mutate replaces the spell's mana cost with the mutate
            // cost — an alternative cost, so only one may apply (CR 118.9a).
            | CastingVariant::Mutate
            // CR 702.76a: Prowl substitutes the prowl cost for the printed cost.
            | CastingVariant::Prowl
            // CR 702.117a: Surge substitutes the surge cost for the printed cost.
            | CastingVariant::Surge
            | CastingVariant::Freerunning => true,
            CastingVariant::Normal
            | CastingVariant::Adventure
            | CastingVariant::Omen
            | CastingVariant::Retrace
            | CastingVariant::Aftermath
            // CR 702.133a: Jump-start discards a card as an *additional* cost on
            // top of the normal mana cost — not an alternative cost (CR 118.9a).
            | CastingVariant::JumpStart
            // CR 702.102c + CR 118.9a: Fuse pays the full combined printed mana
            // cost of both halves — not an alternative cost.
            | CastingVariant::Fuse
            | CastingVariant::GraveyardPermission { .. }
            | CastingVariant::ExilePermission { .. } => false,
        }
    }

    pub fn exiles_when_leaving_stack_for_any_reason(self) -> bool {
        matches!(
            self,
            CastingVariant::Flashback
                | CastingVariant::Aftermath
                | CastingVariant::Harmonize
                // CR 702.133a: "exile this card instead of putting it anywhere
                // else any time it would leave the stack."
                | CastingVariant::JumpStart
        )
    }

    pub fn stack_to_graveyard_replacement(self) -> Option<Zone> {
        if self.exiles_when_leaving_stack_for_any_reason() {
            return Some(Zone::Exile);
        }
        if let CastingVariant::GraveyardPermission {
            graveyard_destination_replacement,
            ..
        } = self
        {
            return graveyard_destination_replacement;
        }
        None
    }

    pub fn replaces_stack_to_graveyard_with_exile(self) -> bool {
        matches!(self.stack_to_graveyard_replacement(), Some(Zone::Exile))
    }

    /// CR 400.7 + CR 712.11a: these variants put a non-front face on the
    /// stack. If the spell leaves the stack without becoming that face on the
    /// battlefield, restore the object's normal front-face characteristics.
    pub fn restores_front_face_after_stack_exit(self) -> bool {
        matches!(
            self,
            CastingVariant::Adventure
                | CastingVariant::Omen
                | CastingVariant::MoreThanMeetsTheEye
                | CastingVariant::Disturb
        )
    }
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
        #[serde(default)]
        actual_mana_spent: u32,
    },
    ActivatedAbility {
        source_id: ObjectId,
        ability: ResolvedAbility,
    },
    TriggeredAbility {
        source_id: ObjectId,
        ability: Box<ResolvedAbility>,
        #[serde(default)]
        condition: Option<TriggerCondition>,
        /// CR 603.7c: The event that caused this trigger, for event-context resolution.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
        /// Human-readable trigger description from the Oracle text.
        /// Used by the frontend to distinguish triggers from the same source.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        description: Option<String>,
        /// Display name of the source object captured when this trigger went on
        /// the stack. Pre-resolved here so the frontend can render
        /// "From <name>" without dereferencing `source_id` through the objects
        /// map (which is display-layer logic per the engine/frontend split).
        /// Empty when the source has no name (synthetic game-rule triggers
        /// like monarch draw use `ObjectId(0)`).
        #[serde(default, skip_serializing_if = "String::is_empty")]
        source_name: String,
        /// CR 603.2c: For batched triggers with a `valid_card` filter, the
        /// count of subjects in the firing event batch that satisfied the
        /// filter. Flows from `collect_matching_triggers` →
        /// `push_pending_trigger_to_stack_with_event_batch` →
        /// `state.current_trigger_match_count` at resolution start. `None` for
        /// non-batched triggers and for batched triggers without a
        /// `valid_card` filter.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject_match_count: Option<u32>,
        /// CR 706.2 + CR 706.4 + CR 603.12: die-roll result captured at trigger
        /// push so a reflexive "When you do … the result" sub-ability that
        /// resolves on its own stack entry (in a later apply(), after the
        /// original resolution scope cleared) can re-stamp
        /// `die_result_this_resolution`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        die_result: Option<i32>,
    },
    /// CR 113.3b: Activated keyword abilities (Equip / Crew / Saddle / Station)
    /// enter the stack after cost-payment + target selection and resolve with
    /// last-known information per CR 113.7a. The source permanent id lives on
    /// the enclosing `StackEntry.source_id` — each `KeywordAction` variant
    /// additionally carries its own typed object ids (equipment_id, vehicle_id,
    /// mount_id, spacecraft_id) needed at resolution.
    KeywordAction { action: KeywordAction },
}

/// CR 608.2e: A clause-local snapshot of an equalization minimum/maximum,
/// frozen when a `player_scope` link begins so every player in that clause's
/// APNAP fan-out resolves its disposal count against the same pre-clause board.
///
/// Balance's three clauses ("sacrifice lands", "discard cards", "sacrifice
/// creatures") each compute an independent extremum at a different time. The
/// `player_scope` driver re-resolves the effect's `count` expression on every
/// per-player iteration; without a snapshot, after APNAP player 0 sacrifices
/// down to the minimum, player 1 would recompute a smaller minimum. The
/// snapshot freezes only the cross-player aggregate (`ControlledByEachPlayer` /
/// `HandSize { AllPlayers }`); the per-player `left` operand still re-resolves
/// per iteration, which is correct.
///
/// Transient — never serialized. Captured before a `player_scope` link's
/// fan-out and cleared when the link completes, so the next clause re-enters
/// the driver with `None` and re-captures against the post-clause board.
///
/// # Single-cell invariant
///
/// This is stored as a single `Option<ClauseMinimumSnapshot>` on `GameState`
/// (not a `Vec` stack). That is sound today because no inline-recursion path
/// exists for the only effects Balance uses (`Effect::Sacrifice` and
/// `Effect::Discard`): a player-scope clause's per-player iteration never
/// re-enters the `player_scope` driver mid-fan-out, so an outer snapshot is
/// never overwritten by an inner one within a single clause.
///
/// If a future feature inlines a nested ability-chain resolution during a
/// Balance-style clause's fan-out — for example, a replacement effect on
/// sacrifice that spawns another player-scope effect — the outer Balance
/// snapshot would be silently corrupted by the inner capture. At that point
/// this field MUST become a `Vec<ClauseMinimumSnapshot>` stack with
/// push/pop bracketing each `player_scope` link entry/exit.
#[derive(Debug, Clone, Default)]
pub struct ClauseMinimumSnapshot {
    /// Reduced cross-player aggregates keyed by the originating quantity
    /// reference, so multiple distinct refs in one clause do not collide.
    entries: Vec<(super::ability::QuantityRef, i32)>,
}

impl ClauseMinimumSnapshot {
    /// Record a captured aggregate for a quantity reference.
    pub fn insert(&mut self, qty: super::ability::QuantityRef, value: i32) {
        self.entries.push((qty, value));
    }

    /// Look up the frozen aggregate for a quantity reference, if captured.
    pub fn get(&self, qty: &super::ability::QuantityRef) -> Option<i32> {
        self.entries.iter().find(|(k, _)| k == qty).map(|(_, v)| *v)
    }
}

/// Display-safe public payment facts captured when a spell is finalized onto
/// the stack. Some underlying cast bookkeeping is transient and intentionally
/// cleared after trigger collection, but the stack UI still needs the paid
/// facts while the spell remains pending.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StackPaidSnapshot {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub actual_mana_spent: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub x_value: Option<u32>,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub distinct_colors_spent: u32,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub kickers_paid: usize,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub additional_cost_payment_count: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_cost_payments: Vec<AdditionalCostInstancePayment>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub additional_cost_paid: bool,
    #[serde(default, skip_serializing_if = "CastingVariant::is_normal")]
    pub casting_variant: CastingVariant,
    /// CR 310.11b + CR 712.14a: Exile alt-cost casts that were explicitly cast
    /// transformed resolve onto the battlefield back face up.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub cast_transformed: bool,
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub convoked_creatures: usize,
}

/// CR 603.2: Maintained index from `TriggerEventKey` to the candidate set of
/// battlefield permanents whose triggers could match an event with that key.
/// Consulted by `collect_pending_triggers` to skip the full battlefield scan
/// that previously asked every permanent on every event whether it cared.
///
/// CR 603.2 invariant: every battlefield object whose trigger could match
/// event E must appear in the union of buckets `keys_from_event(E)` looks up
/// OR in `unclassified`. Over-approximation is correctness-preserving; under-
/// approximation is a silent trigger drop.
///
/// CR 603.6a + CR 611.2e: Granted triggers (sliver lords, Cairn Wanderer,
/// Bramble Sovereign) are materialized by `evaluate_layers` into
/// `obj.trigger_definitions`. The index's battlefield-scoped portion is
/// rebuilt at the end of `evaluate_layers` so it always reflects post-layer
/// trigger sets. That rebuild is the **authoritative correctness path**; the
/// `move_to_zone` hooks (`game::zones`) are incremental optimization only.
///
/// Backed by `im::HashMap` so `GameState::clone()` (hot path through AI
/// search, casting affordability simulation, restriction probes) stays O(1)
/// structural share rather than O(buckets × ObjectIds) deep copy.
#[derive(Debug, Clone, Default)]
pub struct TriggerIndex {
    /// Buckets keyed by event shape. `SmallVec` keeps allocation off the heap
    /// for the typical bucket size (≤ 4 candidates for most keys on most
    /// battlefields).
    pub by_key: im::HashMap<super::triggers::TriggerEventKey, smallvec::SmallVec<[ObjectId; 4]>>,
    /// Catch-all bucket: any battlefield object whose trigger definitions
    /// could not be statically classified by `keys_from_trigger_def`.
    /// Consulted on every event regardless of `keys_from_event` output.
    /// Empty for the common case where every trigger's mode is known.
    pub unclassified: smallvec::SmallVec<[ObjectId; 4]>,
}

/// CR 611.2 + CR 613.1: Candidate pre-filter for `for_each_static_effect_source`.
/// Holds the ids of objects that GENERATE ≥1 continuous effect for the TWO
/// `layers_dirty`-covered source categories: battlefield permanents with a
/// continuous `static_definitions` entry (including `GrantStaticAbility` hosts)
/// and command-zone emblems. The opt-in-zone / off-zone arm (Incarnation cycle —
/// Anger/Brawn/Filth/Wonder/Valor, `active_zones`-gated statics functioning from
/// the graveyard) is INTENTIONALLY NOT indexed: its generator-set changes (e.g.
/// self-milling an Anger into the graveyard) do not all mark `layers_dirty`
/// (`zones.rs` marks dirty only on battlefield/hand transitions; mill/effect
/// movers add no mark), so a `layers_dirty`-gated cache of off-zone generators
/// would go stale. That arm keeps its live `state.objects` scan in
/// `for_each_static_effect_source`.
///
/// Backed by `im::Vector` so `GameState::clone()` stays O(1) structural share
/// (and `GameState: Send` is preserved — no `Rc`). Rebuilt at the TOP of
/// `evaluate_layers` / `apply_layers_incremental` (after the Step-1 base reset,
/// before the first gather — unlike `TriggerIndex`, this index is consulted
/// MID-pass, so it must be fresh before the gather) and lazily on first consult
/// after deserialize via the empty-index direct-scan fallback.
#[derive(Debug, Clone, Default)]
pub struct StaticSourceIndex {
    /// Battlefield generators, in `state.battlefield` order (preserves the
    /// current gather order; phased-out objects are included here and skipped
    /// at consult via `is_phased_out()`).
    pub battlefield_sources: im::Vector<ObjectId>,
    /// Command-zone emblem generators, in `state.command_zone` order.
    pub command_sources: im::Vector<ObjectId>,
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

    // Central object store. Uses FxBuildHasher (fast, deterministic) instead of
    // the default SipHash RandomState: ObjectId is a thin integer key and this
    // map is looked up millions of times per large-board resolution — profiling
    // showed SipHash hashing + HAMT lookup was ~35% of resolution CPU.
    pub objects: im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>,
    pub next_object_id: u64,

    // Shared zones
    pub battlefield: im::Vector<ObjectId>,
    pub stack: im::Vector<StackEntry>,
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stack_paid_facts: HashMap<ObjectId, StackPaidSnapshot>,
    pub exile: im::Vector<ObjectId>,

    /// Objects in the command zone (commanders, emblems).
    #[serde(default)]
    pub command_zone: im::Vector<ObjectId>,

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
    /// CR 614.12a: set by `continue_replacement` when an optional `MayCost`
    /// accept's payment paused for an interactive sub-choice (e.g. Mox Diamond's
    /// "discard a land card" with multiple eligible lands). It re-parks the
    /// pending replacement (`may_cost_paid: true`, plus any `may_cost_remaining`)
    /// and leaves `waiting_for` on the live sub-choice prompt.
    /// `handle_replacement_choice` reads this flag to surface that prompt instead
    /// of re-applying the replacement, and the sub-choice's resolution resumes
    /// the accept once the cost is settled.
    /// Cleared the moment it is observed. Transient — never serialized.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub replacement_may_cost_paused: bool,
    /// CR 614.12a + CR 615.5: Continuation effect to resolve after a
    /// replacement's modifications complete. The two binding states (Template
    /// AST vs. Resolved with captured targets) share one slot via
    /// `PostReplacementContinuation`. Set by `continue_replacement` for
    /// Optional replacements and by `apply_single_replacement` for Mandatory
    /// post-effects; drained by `apply_pending_post_replacement_effect`.
    ///
    /// Pre-2026-05-09 audit M4 fold: legacy `post_replacement_effect` and
    /// `post_replacement_resolved_effect` fields were merged here. Old saved
    /// JSON migrates via `migrate_post_replacement_continuation`, called from
    /// `finalize_public_state`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_continuation: Option<crate::types::ability::PostReplacementContinuation>,
    /// Pre-2026-05-09 audit M4 compat: legacy template slot. Read from old
    /// JSON only; migrated into `post_replacement_continuation` by
    /// `migrate_post_replacement_continuation`. Never written to.
    #[serde(default, skip_serializing, rename = "post_replacement_effect")]
    pub(crate) legacy_post_replacement_effect:
        Option<Box<crate::types::ability::AbilityDefinition>>,
    /// Pre-2026-05-09 audit M4 compat: legacy resolved slot. Read from old
    /// JSON only; migrated into `post_replacement_continuation` by
    /// `migrate_post_replacement_continuation`. Never written to.
    #[serde(default, skip_serializing, rename = "post_replacement_resolved_effect")]
    pub(crate) legacy_post_replacement_resolved_effect:
        Option<Box<crate::types::ability::ResolvedAbility>>,

    /// CR 615.5: Source object of the replacement that stashed
    /// `post_replacement_continuation`. Used by prevention follow-ups (e.g.
    /// Phyrexian Hydra) so the post-effect's `SelfRef`-targeted PutCounter
    /// resolves against the shield's own object rather than the damaged target.
    /// Set alongside `post_replacement_continuation` and consumed at the same
    /// time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_source: Option<crate::types::identifiers::ObjectId>,

    /// CR 615.5 + CR 609.7: Source object of the *prevented event itself*
    /// (e.g. the damage dealer in a damage-prevention replacement) — distinct
    /// from `post_replacement_source` (which is the replacement's own source,
    /// e.g. Swans of Bryn Argoll). Used by `TargetFilter::PostReplacementSourceController`
    /// to resolve "the source's controller draws cards" / "deals damage to the
    /// source's controller" follow-ups. Architectural twin of `last_effect_count`
    /// (the quantity-side post-replacement fallback at `replacement.rs:317`):
    /// both stash event context that lives outside the trigger window. Set
    /// only at the prevention applier's `Prevented` arm; cleared at every
    /// other set-site of `post_replacement_source` and at every consume-site.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_event_source: Option<crate::types::identifiers::ObjectId>,

    /// CR 615.5: Target of the prevented event itself. Used by
    /// `TargetFilter::PostReplacementDamageTarget` for follow-ups like
    /// "that player exiles that many cards" after damage to a player is
    /// prevented and replaced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_event_target: Option<crate::types::ability::TargetRef>,

    /// Transient: post-resolution context for a permanent spell whose ETB replacement
    /// needs a player choice (NeedsChoice). Consumed by `handle_replacement_choice`
    /// after the zone change completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_spell_resolution: Option<PendingSpellResolution>,

    /// CR 702.140c + CR 730.2: Transient context for a mutating creature spell
    /// whose resolution is paused awaiting the controller's top/bottom merge
    /// choice. Set in `stack::resolve_top` (legal-target branch), consumed by
    /// `merge::handle_mutate_merge_choice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_mutate_merge: Option<PendingMutateMerge>,

    /// CR 614.12a + CR 707.9 + CR 603.2: `ZoneChanged`-to-battlefield events
    /// for an object whose entry is paused mid-resolution awaiting an
    /// interactive choice (e.g. `WaitingFor::CopyTargetChoice`). Per CR
    /// 614.12a, effects that modify how a permanent enters function
    /// continuously *while it is entering* — so the entry isn't finalized
    /// (and trigger scanning can't run) until the choice resolves. The
    /// post-action pipeline moves matching events here before
    /// `process_triggers`, and `handle_copy_target_choice` replays them
    /// after `BecomeCopy` resolves + layers re-evaluate so granted ETBs
    /// (Callidus Assassin's destroy-same-name) and observer ETBs
    /// (Soul Warden) match against the fully-realized copy.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_entry_events: Vec<GameEvent>,

    // Layer system
    // CONSERVATIVE: deserialized snapshots (e.g. the WASM-export repro) rebuild
    // fully on first flush. The previous `bool` field serialized as `true`
    // initially; skipping + defaulting to `Full` preserves that intent without
    // serializing the (derived) entered-object set.
    #[serde(skip, default = "LayersDirty::full")]
    pub layers_dirty: LayersDirty,
    /// CR 611.3a + CR 611.3b: truth of each CONTINUOUS static's SOURCE-LEVEL
    /// (non-recipient-context) enabling condition as of the last full
    /// `evaluate_layers`. Read by the incremental-flush truth-delta
    /// short-circuit to skip escalation when an entry perturbs the gate but
    /// does not flip it. Recipient-context conditions are NEVER stored here
    /// (their truth is per-recipient; `source_condition_gate_passes` is only an
    /// over-approximation for them) and always escalate. Refreshed wholesale
    /// every full eval (`refresh_static_gate_truth`). `#[serde(skip)]` derived
    /// state, like `layers_dirty`/`trigger_index`. `im::HashMap` keeps
    /// clone-per-candidate legality probes from deep-copying this derived cache.
    #[serde(skip)]
    pub static_gate_truth: im::HashMap<StaticGateKey, bool>,
    /// CR 603.2: Candidate pre-filter for `collect_pending_triggers`. Rebuilt
    /// lazily after deserialize via a sentinel check at the top of the consult
    /// site; rebuilt eagerly at the end of `evaluate_layers` (CR 611.2e) so the
    /// post-layer trigger set is reflected. `#[serde(skip)]` because the index
    /// is derived state — reconstructed from `state.battlefield` + per-object
    /// `trigger_definitions` whenever needed.
    #[serde(skip)]
    pub trigger_index: TriggerIndex,
    /// CR 611.2 + CR 613.1: Derived generator index for the layer gather.
    /// `#[serde(skip)]` derived state (like `trigger_index`/`layers_dirty`);
    /// reconstructed from `state.battlefield` + `state.command_zone` +
    /// per-object `static_definitions` at the top of every layer pass, and
    /// lazily on first consult after deserialize via the empty-index fallback.
    /// INTENTIONALLY omitted from `impl PartialEq for GameState` — derived state
    /// must not break AI-search dedup on semantically-identical positions.
    #[serde(skip)]
    pub static_source_index: StaticSourceIndex,
    pub next_timestamp: u64,
    #[serde(skip, default = "PublicStateDirty::all_dirty")]
    pub public_state_dirty: PublicStateDirty,
    #[serde(skip, default)]
    pub state_revision: u64,

    // Runtime continuous effects (from resolved spells/abilities, not printed card text)
    #[serde(default)]
    pub transient_continuous_effects: im::Vector<TransientContinuousEffect>,
    #[serde(default)]
    pub next_continuous_effect_id: u64,

    /// Per-object source-attribution side-table, rebuilt fresh every layers
    /// pass. Records which continuous effects contributed grants/removals to
    /// each object so the frontend can display "Flying — from Akroma's
    /// Memorial" without inferring source by name-diffing. Display metadata
    /// only — never read by game logic. Empty objects skip serialization.
    #[serde(default, skip_serializing_if = "im::HashMap::is_empty")]
    pub attribution: im::HashMap<ObjectId, ObjectAttribution>,

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

    /// (source_id, ability_index) pairs for activated abilities pushed to the
    /// stack during the current priority window. Transient AI-guard that
    /// prevents the AI's softmax policy from re-choosing the same activated
    /// ability while its prior activation is still unresolved on the stack —
    /// a pathological scoring outcome when the effect is redundant (e.g.
    /// self-exile with delayed return, or gain indestructible UEOT when the
    /// buff is already active). CR 117.1b permits unbounded activation at
    /// priority, and absent a CR 602.5b restriction there is no per-turn cap,
    /// so this is a pure AI-pathology mitigation, not a rules concern.
    /// Cleared on PassPriority (when the stack will begin resolving).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_activations: Vec<(ObjectId, usize)>,

    // Triggered ability targeting
    #[serde(default)]
    pub pending_trigger: Option<crate::game::triggers::PendingTrigger>,
    /// Sidecar for `pending_trigger`: full simultaneous event set for batched
    /// trigger context, consumed when the pending trigger is put on the stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_trigger_event_batch: Vec<GameEvent>,
    /// CR 603.3c + CR 603.3d: ObjectId of the stack entry currently being
    /// constructed (mode / target / division still being chosen by the
    /// controller). `Some` only while a pause-path `WaitingFor` is outstanding.
    ///
    /// "Push first, choose second" invariant: when this is `Some(id)`, the top
    /// of `state.stack` is the trigger entry with that id, and its
    /// `ResolvedAbility` has unfilled slots that the active `WaitingFor` is
    /// gathering. `stack::resolve_top` refuses to fire on this id.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_trigger_entry: Option<ObjectId>,
    /// CR 113.2c + CR 603.2 + CR 603.3b: Queue of triggers that fired in the
    /// same pass but were deferred because an earlier trigger needed player
    /// input (modal choice, target selection, or division). Each instance of a
    /// printed ability fires independently, so multiple copies of the same
    /// permanent (e.g., two Boggart Pranksters seeing "you attack") must each
    /// reach the stack. Drained in FIFO order by
    /// `triggers::drain_deferred_trigger_queue` after the active
    /// `pending_trigger` is pushed to the stack.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_triggers: Vec<crate::game::triggers::DeferredTrigger>,

    /// CR 603.3b: In-flight per-controller ordering pass. `Some` only while a
    /// `WaitingFor::OrderTriggers` choice (or its APNAP successor) is
    /// outstanding. Holds every group's triggers in placement order (NAP-first)
    /// plus the per-group `ordered` flag. When every group is `ordered`,
    /// `handle_order_triggers` concatenates and dispatches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_trigger_order: Option<PendingTriggerOrder>,

    // CR 607.2a + CR 406.5: Exile tracking for "until leaves" linked abilities.
    #[serde(default)]
    pub exile_links: Vec<ExileLink>,

    /// CR 702.xxx: Paradigm (Strixhaven) — first-resolution gate.
    ///
    /// Each entry records the `(player, card_name)` pair for which Paradigm
    /// has already armed. Subsequent resolutions of any spell with the same
    /// name by the same player do NOT re-arm (reminder: "After you **first**
    /// resolve a spell with this name"). Entries are never cleared — Paradigm
    /// is a once-per-name-per-player gate for the game. Assign when WotC
    /// publishes SOS CR update.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub paradigm_primed: Vec<ParadigmPrime>,

    /// CR 603.7: Delayed triggered abilities waiting to fire.
    #[serde(default)]
    pub delayed_triggers: Vec<DelayedTrigger>,

    /// CR 603.7: Object sets tracked for delayed triggers ("those cards", "that creature").
    #[serde(default)]
    pub tracked_object_sets: HashMap<TrackedSetId, Vec<ObjectId>>,

    #[serde(default)]
    pub next_tracked_set_id: u64,

    /// CR 603.7 + CR 608.2c: The tracked set published by the currently-resolving
    /// ability chain, if any. Set by the first publish inside a chain and reused
    /// (extended) by later publishes in the same chain so compound zone-changing
    /// effects (e.g., "Exile target permanent and the top card of your library
    /// ... For each of those cards") merge their results into a single set
    /// before downstream "those cards" references resolve. Cleared at the
    /// top-level chain entry (depth == 0) in `resolve_ability_chain`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chain_tracked_set_id: Option<TrackedSetId>,

    /// CR 608.2c + CR 614.6: Per-member producer-action provenance for tracked
    /// sets. When a producer publishes (or extends) a chain tracked set, each
    /// affected object is additionally stamped here with the ACTION that made it
    /// part of the set — derived from the resolving EFFECT, NOT the member's
    /// final landing zone (Sacrificed for `Effect::Sacrifice`, Destroyed for
    /// `Effect::Destroy`/`DestroyAll`, Milled for `Effect::Mill`, Discarded for
    /// `Effect::Discard`/`DiscardCard`, Exiled/Returned/Bounced for zone changes
    /// by destination). A downstream "this way" consumer that binds to a
    /// specific verb — `TargetFilter::TrackedSetFiltered`/
    /// `QuantityRef::FilteredTrackedSetSize` with `caused_by: Some(cause)` —
    /// consults this map so it counts only the members the matching action
    /// produced. Because the stamp is the action, a sacrifice that a replacement
    /// redirects to Exile (CR 614.6) is still `Sacrificed`, and same-destination
    /// actions (mill vs. sacrifice, both → graveyard) never collide. This keeps
    /// `tracked_object_sets` (the id-only membership read by every existing
    /// consumer) byte-identical while letting a single merged exile→sacrifice
    /// chain set serve both an "exiled this way" return and a sibling
    /// "sacrificed this way" reference (issue #2932). Members published without
    /// action provenance (selection sets via `publish_fresh_tracked_set`) are
    /// absent here and are read only by `caused_by: None` references.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub tracked_set_member_causes: HashMap<TrackedSetId, HashMap<ObjectId, ThisWayCause>>,

    // Commander support
    #[serde(default)]
    pub commander_cast_count: HashMap<ObjectId, u32>,

    /// Owner stamped when a commander cast from the command zone is recorded.
    /// CR 903.8: `commander_casts_from_command_zone` must count committed casts
    /// even when the recorded `ObjectId` no longer has `is_commander` set.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub commander_cast_owners: HashMap<ObjectId, PlayerId>,

    /// CR 903.9a: Commanders whose owner declined the zone-return choice this
    /// SBA cycle. Cleared when the commander changes zones again (giving the
    /// owner a fresh choice opportunity).
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub commander_declined_zone_return: HashSet<ObjectId>,

    /// CR 500.7: Extra turns granted by effects, stored as a LIFO stack.
    /// Most recently created extra turn is taken first (pop from end).
    #[serde(default)]
    pub extra_turns: Vec<PlayerId>,

    /// CR 614.10: Per-player count of turns to skip. When a player would begin their
    /// turn with a non-zero counter, the turn is skipped and the counter is decremented.
    #[serde(default)]
    pub turns_to_skip: Vec<u32>,

    /// CR 614.10a: Per-player counts of step occurrences to skip. A pending skip
    /// is consumed only when the named step would otherwise happen.
    #[serde(default)]
    pub steps_to_skip: Vec<HashMap<Phase, u32>>,
    #[serde(default)]
    pub scheduled_turn_controls: Vec<ScheduledTurnControl>,

    /// CR 500.8: Extra phases granted by effects, stored as a LIFO stack of
    /// anchored entries. Each `ExtraPhase` records the phase it occurs
    /// directly after (`anchor`) and the phase to insert (`phase`).
    /// Consumed by `advance_phase()` — only entries whose `anchor` matches
    /// `state.phase` are popped, scanned from the end so the most recently
    /// created entry occurs first.
    #[serde(default)]
    pub extra_phases: Vec<ExtraPhase>,

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

    /// Per-player phase-stop preferences. While a player's `UntilEndOfTurn`
    /// auto-pass session is active, the engine will interrupt auto-pass whenever
    /// the current phase appears in that player's list. Also consulted when
    /// deciding whether to auto-submit empty blockers during Declare Blockers,
    /// so users can pause the step to activate instants / Ninjutsu even when
    /// no legal blockers exist.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub phase_stops: HashMap<PlayerId, Vec<Phase>>,

    /// CR 605.3: Lands manually tapped for mana via TapLandForMana this priority window.
    /// Per-player map enables multiplayer correctness (e.g., UnlessPayment opponent tapping).
    /// Cleared on priority pass, cast, non-mana action, or phase transition.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lands_tapped_for_mana: HashMap<PlayerId, Vec<ObjectId>>,

    /// CR 103.5: Per-player locked-in mulligan count, populated as each player
    /// declares "keep" during the simultaneous decision phase. Read by the
    /// bottoms-phase builder to compute how many cards each player must put
    /// on the bottom of their library. Cleared when the mulligan flow finishes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub final_mulligan_counts: HashMap<PlayerId, u8>,

    /// TL:R 906.6a: Per-player bottoms already paid by a forced opening-hand
    /// mulligan before the normal mulligan-decision step. Subtracted from the
    /// later London bottom count so the forced bottom is not charged twice.
    /// Cleared with `final_mulligan_counts` when the mulligan flow finishes.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub prepaid_mulligan_bottoms: HashMap<PlayerId, u8>,

    /// When true, `GameAction::Debug(...)` actions are accepted.
    /// Set at game initialization, immutable after creation.
    /// Always false for multiplayer games.
    #[serde(default)]
    pub debug_mode: bool,

    /// Set of players who have been granted permission to submit
    /// `GameAction::Debug(_)` in a sandbox game. Initialized to the host's
    /// `PlayerId` at game creation when `format_config.allow_debug_actions`
    /// is true; empty otherwise. The host can grant/revoke entries via
    /// `GameAction::GrantDebugPermission` / `RevokeDebugPermission`.
    #[serde(default)]
    pub debug_permitted: BTreeSet<PlayerId>,

    /// Set of players for whom the "infinite mana" debug toggle is active. While
    /// a player is in this set, their mana pool is topped up after every action
    /// (`mana_payment::refill_infinite_mana`) and is NOT emptied at end of
    /// step/phase — CR 500.5 is deliberately suppressed for this player only.
    /// This is a debug-only departure from the rules, gated behind the same
    /// debug-action permission as every other `DebugAction`. Toggled via
    /// `DebugAction::SetInfiniteMana`; empty by default.
    #[serde(default)]
    pub debug_infinite_mana: BTreeSet<PlayerId>,

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
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outside_game_cards_brought_in: Vec<OutsideGameCardUse>,
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
    /// CR 603.2: Tracks per-opponent-per-turn firing for
    /// OncePerOpponentPerTurn. Keyed by (object_id, trigger_index, opponent_id).
    #[serde(default)]
    pub triggers_fired_this_turn_per_opponent: HashSet<(ObjectId, usize, PlayerId)>,
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
    /// CR 602.5b + CR 702.122: Vehicles whose crew ability has been activated this
    /// turn. Populated on a successful crew announcement; read to enforce an
    /// "Activate only once each turn" crew restriction. Crew is not an
    /// `abilities[]` entry, so it cannot use `activated_abilities_this_turn`
    /// (keyed by `(source_id, ability_index)`). Cleared at turn start.
    #[serde(default)]
    pub crew_activated_this_turn: HashSet<ObjectId>,
    /// CR 606.1 + CR 606.3 + CR 603.4: Per-player count of loyalty-ability
    /// activations this turn. Incremented in
    /// `planeswalker::finalize_loyalty_activation` whenever any loyalty ability
    /// resolves onto the stack (CR 606.1: loyalty abilities are a subset of
    /// activated abilities; the activation event happens at announcement, not
    /// resolution — which matches the CR 603.4 "this turn" history reading).
    /// Read by `QuantityRef::LoyaltyAbilitiesActivatedThisTurn` for intervening-if
    /// conditions like The Chain Veil's "if you activated a loyalty ability of
    /// a planeswalker this turn". Cleared at turn start.
    #[serde(default)]
    pub loyalty_abilities_activated_this_turn: HashMap<PlayerId, u32>,
    /// CR 606.3: Per-player extra loyalty-activation grants for this turn —
    /// each entry raises the per-permanent CR 606.3 cap for every planeswalker
    /// the player controls. Populated by the
    /// `Effect::GrantExtraLoyaltyActivations` resolver (The Chain Veil's
    /// activated ability). Consumed by
    /// `planeswalker::can_activate_loyalty_ability`. Cleared at turn start.
    #[serde(default)]
    pub extra_loyalty_activations_this_turn: HashMap<PlayerId, u32>,
    /// CR 701.43d: Permanents exerted this turn via the "you may exert it as it
    /// attacks" optional attack cost (Combat Celebrant, Glory-Bound Initiate,
    /// Exemplar of Strength, ...). Gates the linked "when you do" trigger to
    /// fire at most once per turn ("if this creature hasn't been exerted this
    /// turn") and prevents re-prompting in extra combat phases. Cleared at turn
    /// start. Distinct from the exert *cost* path (a `CantUntap` transient), this
    /// set is the authoritative "was exerted this turn" record.
    #[serde(default)]
    pub exerted_this_turn: std::collections::HashSet<ObjectId>,
    /// CR 508.1g + CR 508.2: Declaration events (e.g. `AttackersDeclared`) held
    /// while the active player resolves the optional "exert as it attacks"
    /// sub-step. Because triggers are matched against the per-action event slice
    /// (which does not persist across the interactive exert prompts), the
    /// declaration events are buffered here and processed together with the
    /// `CreatureExerted` events once the exert queue drains — so all
    /// declaration/exert triggers go on the stack simultaneously per CR 508.2.
    /// Empty except mid-declaration; drained by `finish_declare_attackers`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_attack_trigger_events: Vec<crate::types::events::GameEvent>,
    /// CR 603.4: Per-ability per-turn resolution counter.
    /// Keyed by `(source_id, ability_index)` — identifies a specific printed
    /// ability on a specific source object. Incremented at the top of
    /// `resolve_ability_chain` (depth 0) when the resolving ability has a
    /// `Some(ability_index)` stamp; read by
    /// `AbilityCondition::NthResolutionThisTurn` to gate Omnath-style
    /// "if this is the [Nth] time this ability has resolved this turn" patterns.
    /// Cleared in `start_next_turn` alongside other per-turn counters.
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "tuple_key_map"
    )]
    pub ability_resolutions_this_turn: HashMap<(ObjectId, usize), u32>,
    /// CR 601.2a: Tracks which graveyard-cast permission sources have been
    /// used this turn. Keyed by the granting permanent's ObjectId.
    /// CR 400.7: Zone change creates new ObjectId, naturally resetting.
    #[serde(default)]
    pub graveyard_cast_permissions_used: HashSet<ObjectId>,
    /// CR 110.4 + CR 601.2a: Tracks which permanent-type slots a
    /// `OncePerTurnPerPermanentType` graveyard-cast permission source has
    /// already consumed this turn. Keyed by `(source_id, slot_core_type)`
    /// where `slot_core_type` is the permanent type the cast/play was credited
    /// to (one of the six CR 110.4 permanent types). Muldrotha, the Gravetide
    /// is the canonical user: each permanent type acts as an independent
    /// per-turn slot, so a single source may credit one cast per permanent
    /// type per turn.
    /// CR 400.7: Zone change creates a new source `ObjectId`, naturally
    /// resetting all slots.
    #[serde(default)]
    pub graveyard_cast_permissions_used_per_type: HashSet<(ObjectId, super::card_type::CoreType)>,
    /// CR 110.4: Transient slot stashed by the ChoosePermanentTypeSlot dispatch
    /// for the land-play path. Consumed by `record_graveyard_play_permission` on
    /// re-entry into `handle_play_land`. `None` when no slot choice is pending.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_permanent_type_slot: Option<(ObjectId, super::card_type::CoreType)>,
    /// CR 601.2b: Tracks which `CastFromHandFree` once-per-turn permission sources
    /// have been used this turn (Zaffai and the Tempests). Keyed by the granting
    /// permanent's ObjectId. Unlimited sources (Omniscience) never populate this.
    /// CR 400.7: Zone change creates new ObjectId, naturally resetting.
    #[serde(default)]
    pub hand_cast_free_permissions_used: HashSet<ObjectId>,
    /// CR 601.2a: Tracks once-per-turn `PlayFromExile` permission sources
    /// consumed this turn. Keyed by the granting source's ObjectId.
    #[serde(default)]
    pub exile_play_permissions_used: HashSet<ObjectId>,
    /// CR 601.2a + CR 603.7 + CR 611.2a: Tracks `single_use` `PlayFromExile`
    /// grants whose one allowed cast has already been spent. Keyed by the
    /// tracked set published by the effect (Chandra, Hope's Beacon +1: "you may
    /// cast *a/an* [type] spell from among those exiled cards"). Distinct from
    /// `exile_play_permissions_used`: that set is per-turn and cleared at every
    /// turn boundary, whereas a single-use grant authorizes ONE cast across its
    /// entire (possibly multi-turn) duration window, so this set is NOT cleared
    /// per turn — it is pruned only when the grant itself expires
    /// (`layers::prune_*_casting_permissions` clears stale tracked-set entries).
    #[serde(default)]
    pub exile_play_single_use_consumed: HashSet<TrackedSetId>,
    /// CR 601.2a + CR 113.6b: Tracks `OncePerTurn` `StaticMode::ExileCastPermission`
    /// sources that have already had a spell cast through them this turn
    /// (Maralen, Fae Ascendant — "Once each turn, you may cast …"). Keyed by
    /// the granting permanent's ObjectId. `Unlimited` frequency permissions
    /// never populate this set. Cleared at the start of each turn alongside
    /// the other per-turn cast-permission slots.
    /// CR 400.7: Zone change creates a new source `ObjectId`, naturally
    /// resetting the slot when the source leaves and re-enters play.
    #[serde(default)]
    pub exile_cast_permissions_used: HashSet<ObjectId>,
    /// CR 113.6b + CR 601.2a: Per-turn rolling list of cards that have been
    /// exiled "with" each linked-exile source during the current turn. Keyed
    /// by the source's `ObjectId`; the `Vec` is the list of card `ObjectId`s
    /// exiled this turn by that source, in exile order. Populated by
    /// `exile_links::push_exiled_with_source_this_turn` whenever a tracked
    /// exile happens; cleared at the start of each turn so "cards exiled with
    /// ~ this turn" cast permissions (Maralen, Fae Ascendant) only see the
    /// current turn's pool.
    ///
    /// Distinct from `exile_links`: those persist for the lifetime of the
    /// source-link contract (CR 610.3) and back the open-ended "cards exiled
    /// with ~" filter. This map is the turn-scoped slice and is consulted
    /// only by `StaticMode::ExileCastPermission` and similar per-turn
    /// permissions.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub cards_exiled_with_source_this_turn: HashMap<ObjectId, Vec<ObjectId>>,
    /// CR 702.94a + CR 603.11: Per-player first-card-drawn-this-turn tracking for
    /// miracle's linked triggered ability. Populated by the draw pipeline on the
    /// first `CardDrawn` event each turn per player; reset at turn start. The
    /// `ObjectId` identifies the specific drawn card so the `MiracleReveal`
    /// prompt can target the right hand object and enforce the CR 702.94a
    /// "first card drawn" condition without re-counting. Absent key means the
    /// player has not drawn yet this turn.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub first_card_drawn_this_turn: HashMap<PlayerId, ObjectId>,
    /// Object IDs of cards actually drawn this turn, per player. Cards remain
    /// in this list even if they later leave hand; consumers filter by current
    /// zone when presenting choices.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub cards_drawn_this_turn: HashMap<PlayerId, Vec<ObjectId>>,
    /// CR 702.94a + CR 603.11: FIFO queue of miracle reveal offers accumulated
    /// during the current action's resolution. Populated by the draw pipeline
    /// when a card with `Keyword::Miracle(cost)` becomes the first card drawn
    /// this turn; drained one-at-a-time by `flush_pending_miracle_offer` at the
    /// tail of `run_post_action_pipeline`. Each flush replaces an outgoing
    /// `WaitingFor::Priority` with `WaitingFor::MiracleReveal` for the offer's
    /// player, consuming the offer regardless of accept/decline so a second
    /// draw in the same resolution step queues its own prompt. Reset at turn
    /// start (stale offers from prior turns are never valid per CR 702.94a's
    /// "first card drawn this turn" condition).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_miracle_offers: Vec<MiracleOffer>,
    #[serde(default)]
    pub spells_cast_this_game: HashMap<PlayerId, u32>,
    /// Per-player spell cast history this game.
    /// CR 117.1: Mirrors `spells_cast_this_turn_by_player` but is not cleared
    /// between turns, so name-filtered "this game" queries (Approach of the
    /// Second Sun's "another spell named {LITERAL} this game") can scan the
    /// full game-scope history.
    #[serde(default)]
    pub spells_cast_this_game_by_player: HashMap<PlayerId, im::Vector<SpellCastRecord>>,
    /// Per-player spell cast history this turn.
    /// Each entry records the spell's relevant characteristics at cast time,
    /// enabling data-driven filtered counting at resolution.
    #[serde(default)]
    pub spells_cast_this_turn_by_player: HashMap<PlayerId, im::Vector<SpellCastRecord>>,
    /// Per-player land play origin history this turn.
    /// Mirrors `Player::lands_played_this_turn` when origin-sensitive
    /// conditions need to distinguish hand plays from exile/graveyard plays.
    #[serde(default)]
    pub lands_played_this_turn_by_player: HashMap<PlayerId, im::Vector<LandPlayRecord>>,
    #[serde(default)]
    pub players_who_searched_library_this_turn: HashSet<PlayerId>,
    /// CR 603.4: Typed player-action events performed this turn. This is the
    /// turn-scoped counterpart to `player_actions_this_way`, preserving repeated
    /// actions for count-style conditions while reusing `PlayerActionKind`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub player_actions_this_turn: Vec<(PlayerId, PlayerActionKind)>,
    #[serde(default)]
    pub players_attacked_this_step: HashSet<PlayerId>,
    #[serde(default)]
    pub players_attacked_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub attacking_creatures_this_turn: HashMap<PlayerId, u32>,
    /// CR 508.6 + CR 508.1b: For each attacking player, the set of defending
    /// players they attacked this turn, accumulated across every combat's
    /// declare-attackers step (CR 508.5 "defending player": planeswalker/battle
    /// attacks resolve to controller/protector). Counted by
    /// `PlayerFilter::OpponentAttacked { You, ThisTurn }` for "opponents you
    /// attacked this turn" (Militant Angel).
    #[serde(default)]
    pub attacked_defenders_this_turn: HashMap<PlayerId, HashSet<PlayerId>>,
    /// CR 508.6 + CR 508.1b: For each creature declared as an attacker this
    /// turn, the defending players it attacked. This is the source-specific
    /// counterpart to `attacked_defenders_this_turn` for text like "each player
    /// this creature attacked this turn" (Angel of Destiny).
    #[serde(default)]
    pub creature_attacked_defenders_this_turn: HashMap<ObjectId, HashSet<PlayerId>>,
    /// CR 500.8 + CR 506.1: Number of combat phases that have begun this turn.
    /// Used by intervening-if triggers that only fire during the first combat phase.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub combat_phases_started_this_turn: u32,
    /// CR 508.1a: Object IDs of creatures declared as attackers this turn.
    /// Persists after combat ends for post-combat filtering.
    #[serde(default)]
    pub creatures_attacked_this_turn: HashSet<ObjectId>,
    /// CR 508.1a + CR 608.2c: Declaration-time attacker snapshots for filtered
    /// post-combat queries ("attacked with a token/commander/Dinosaur this
    /// turn"). Persists after combat ends because attackers may have left the
    /// battlefield by resolution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attacker_declarations_this_turn: Vec<AttackDeclarationRecord>,
    /// CR 509.1a: Object IDs of creatures declared as blockers this turn.
    /// Persists after combat ends for post-combat filtering.
    #[serde(default)]
    pub creatures_blocked_this_turn: HashSet<ObjectId>,
    #[serde(default)]
    pub players_who_created_token_this_turn: HashSet<PlayerId>,
    /// CR 111.2: Token creation snapshots this turn, preserving creation-time
    /// characteristics for filtered "tokens you created this turn" quantities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub created_tokens_this_turn: Vec<ZoneChangeRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counter_added_this_turn: Vec<CounterAddedRecord>,
    #[serde(default)]
    pub players_who_discarded_card_this_turn: HashSet<PlayerId>,
    #[serde(default)]
    pub cards_discarded_this_turn_by_player: HashMap<PlayerId, u32>,
    #[serde(default)]
    pub players_who_sacrificed_artifact_this_turn: HashSet<PlayerId>,
    /// CR 701.21a: Sacrificed permanent snapshots this turn, preserving
    /// event-time characteristics for filtered "you sacrificed [quality] this
    /// turn" conditions and quantities.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub sacrificed_permanents_this_turn: Vec<ZoneChangeRecord>,
    /// CR 400.7: Zone-change snapshots this turn, enabling data-driven condition queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub zone_changes_this_turn: Vec<ZoneChangeRecord>,
    /// CR 403.3: Battlefield entry snapshots this turn, enabling data-driven ETB queries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub battlefield_entries_this_turn: Vec<BattlefieldEntryRecord>,
    /// CR 120.1: Damage records this turn for "was dealt damage by" condition queries.
    /// Backed by `im::Vector` so `GameState::clone()` structurally shares the
    /// `DamageRecord` snapshots (each holds a `String` + several `Vec`s) instead
    /// of deep-copying them on the AI-search hot path.
    #[serde(default)]
    pub damage_dealt_this_turn: im::Vector<DamageRecord>,
    /// CR 702.173a + CR 608.2i: Set of players P such that, at some point this
    /// turn, a creature controlled by P that was an Assassin OR a commander
    /// (snapshot at damage-dealing time per CR 608.2i — "looks back in time")
    /// dealt combat damage to ANY player. Populated by the trigger pipeline's
    /// `DamageDealt` observer in `game::triggers` and cleared in
    /// `turns::start_next_turn` per CR 514. Read by `casting_variant_candidates`
    /// to gate the Freerunning cast permission on the spell's controller.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub assassin_or_commander_dealt_combat_damage_this_turn: HashSet<PlayerId>,
    /// CR 702.76a + CR 608.2i: Set of `(controller, creature type)` entries for
    /// sources that dealt combat damage to a player this turn (snapshot at
    /// damage-dealing time — "looks back in time", so a source that later
    /// changes types or leaves does not invalidate the entry). Flat persistent
    /// storage keeps `GameState::clone()` structurally shared on AI/search paths.
    /// Populated by the `DamageDealt` observer in `game::triggers` and cleared in
    /// `turns::start_next_turn` per CR 514. Read by `casting_variant_candidates`
    /// to gate the Prowl cast permission ("had any of this spell's creature types").
    #[serde(default, skip_serializing_if = "im::HashSet::is_empty")]
    pub creature_types_dealt_combat_damage_this_turn: im::HashSet<(PlayerId, String)>,
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
    pub pending_etb_counters: Vec<(ObjectId, CounterType, u32)>,

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
    /// Cards that have been publicly revealed at least once. Unlike
    /// `revealed_cards`, this is not cleared at the next action boundary.
    #[serde(default)]
    pub public_revealed_cards: HashSet<ObjectId>,

    // Pending ability continuation after a player choice (Scry/Dig/Surveil,
    // SearchChoice, ChooseFromZoneChoice, replacement-choice, etc.) or after
    // a replacement proposal pauses mid-chain. See `PendingContinuation` for
    // how parent-kind metadata is carried alongside the chain so the drain
    // re-emits the parent `EffectResolved` event that the non-pause path
    // fires at the tail of its resolver.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_continuation: Option<PendingContinuation>,

    /// CR 609.3 + CR 109.5: Pending `repeat_for` iteration loop paused mid-flight
    /// because the inner effect entered an interactive `WaitingFor` state.
    /// Drained by `drain_pending_continuation` AFTER `pending_continuation`,
    /// so the per-iteration chain (e.g., the SearchLibrary's
    /// "put-onto-battlefield" continuation) completes before the next
    /// iteration begins. See [`PendingRepeatIteration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_repeat_iteration: Option<PendingRepeatIteration>,

    /// CR 614.12b + CR 614.1c + CR 614.13: Pending multi-target `ChangeZone`
    /// iteration loop paused mid-flight because one of the moving objects
    /// triggered a per-permanent replacement choice. Drained by
    /// `drain_pending_continuation` BEFORE `pending_repeat_iteration` so the
    /// inner ChangeZone iteration completes (and its `EffectResolved` event
    /// fires) before the outer repeat loop advances. See
    /// [`PendingChangeZoneIteration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_change_zone_iteration: Option<PendingChangeZoneIteration>,

    /// CR 614.12a + CR 614.13a/b: Battlefield objects eligible to be chosen by an
    /// as-enters Devour sacrifice (CR 702.82a/c), captured the instant BEFORE the
    /// FIRST co-entering devourer enters and PERSISTED for the whole simultaneous
    /// entry. Because CR 614.12a makes every co-entering permanent's as-enters
    /// choice happen before ANY of them enter, the engine (which serializes entry)
    /// reuses this one pre-entry snapshot for every co-entering devourer — so a
    /// second devourer cannot devour the first (it entered "at the same time",
    /// CR 614.13a), and the eligible pool (live battlefield ∩ snapshot) also
    /// excludes anything an earlier devourer already sacrificed (it left the
    /// battlefield) and the devourers themselves (absent from the pre-entry set).
    /// `None` outside a Devour co-entry; cleared when the whole ChangeZone entry
    /// event completes (all co-entering members resolved), NOT per-sacrifice.
    ///
    /// WARNING — save/resume: the serde attr MUST stay `skip_serializing_if =
    /// "Option::is_none"` (skips only `None`; a live `Some` is serialized so a
    /// mid-prompt save keeps the constraint). Never broaden to skip `Some`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub devour_eligible_snapshot: Option<HashSet<ObjectId>>,

    /// CR 730.3e (second clause): routing override for the card components of a
    /// TOKEN merged permanent leaving the battlefield under a card-scoped
    /// (`NonToken`) `Moved` redirect. "If the merged permanent is a token but
    /// some of its components are cards, the merged permanent and its token
    /// components are put into the appropriate [default] zone, and the
    /// components that are cards are moved by the replacement effect."
    ///
    /// Set by `zone_pipeline::deliver_replaced_zone_change` from the single
    /// component-aware consult immediately before the survivor's
    /// `zones::move_to_zone`, read by `merge::split_merged_permanent_on_leave`
    /// to route CARD components to `card_dest` while token components follow the
    /// survivor's `default_dest`, then cleared. Purely synchronous (set →
    /// `move_to_zone` split consumes → cleared in the same delivery), so it
    /// never survives a pause; the serde guard is belt-and-suspenders.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_card_component_route: Option<MergedCardComponentRoute>,

    /// CR 707.2 + CR 614.1a + CR 616.1: Pending `CopyTokenOf` source loop
    /// paused by an interactive token-creation replacement. Drained by
    /// `token_copy::drain_pending_copy_token_resolution` after the current
    /// replacement choice creates the accepted copy token(s).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_copy_token_resolution: Option<PendingCopyTokenResolution>,

    /// CR 705.1 + CR 614.1a: Pending multi-flip coin resolver paused mid-loop
    /// for a Krark's Thumb keep-1 choice. Stashes the full resolution context +
    /// loop position so `resume_after_keep` can re-enter the flip loop after the
    /// player's `CoinFlipKeepChoice`. See [`PendingCoinFlip`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_coin_flip: Option<PendingCoinFlip>,

    /// CR 608.2c + CR 107.1c: Pending "repeat this process" loop paused because
    /// an iteration's process entered an interactive `WaitingFor` state.
    /// Drained by `drain_pending_continuation` after `pending_continuation`,
    /// so the iteration's player choice fully resolves before the loop decides
    /// whether to run another pass. See [`PendingRepeatUntil`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_repeat_until: Option<PendingRepeatUntil>,

    /// CR 701.55d: Pending continuation of a multi-player ChooseOneOf after a
    /// selected branch has finished resolving, including any nested choices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_choose_one_of: Option<PendingChooseOneOf>,

    /// CR 122.5: Pending atomic counter moves selected during a resolution-time
    /// distribution prompt. Drained before normal pending continuations so
    /// replacement choices inside a move resume the remaining selected moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_counter_moves: Option<PendingCounterMoveQueue>,

    /// CR 603.10a + CR 616.1: Pending simultaneous zone-move batch tail paused
    /// by a per-object replacement choice (see [`PendingBatchDeliveries`]).
    /// Drained by the replacement-choice resume path after the chosen event
    /// delivers so the remaining objects complete their moves instead of
    /// stranding. Serde alias keeps the old `pending_mill_deliveries` field name
    /// readable from existing saves.
    #[serde(
        default,
        alias = "pending_mill_deliveries",
        skip_serializing_if = "Option::is_none"
    )]
    pub pending_batch_deliveries: Option<PendingBatchDeliveries>,

    /// CR 122.1 + CR 616.1e: Pending counter-addition batch paused by a
    /// replacement choice. Drained before normal pending continuations so
    /// multi-recipient effects such as proliferate and double counters resume
    /// their remaining counter placements after the current choice resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_counter_additions: Option<PendingCounterAdditionQueue>,

    /// Pending optional effect ability chain, awaiting player accept/decline.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_optional_effect: Option<Box<crate::types::ability::ResolvedAbility>>,

    /// Transient: the triggering event of the ability stashed in
    /// `pending_optional_effect`, captured while it is still live (before
    /// `resolve_top` clears `current_trigger_event`). Restored around
    /// `resolve_optional_effect_decision` so an optional ("may") triggered
    /// ability's effect resolves `TriggeringPlayer` / event-context refs
    /// exactly as a non-optional trigger would. Mirrors
    /// `WaitingFor::UnlessPayment.trigger_event`. Set ONLY for the
    /// `OptionalEffectChoice` stash; taken by `handle_optional_effect_choice`.
    /// CR 608.2: an ability's resolution is a single process.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_optional_trigger_event: Option<crate::types::events::GameEvent>,

    /// CR 603.2c: Saves/restores the firing batched trigger's filtered subject
    /// count across an `OptionalEffectChoice` round-trip so a "you may"
    /// sub-ability (e.g. The Ur-Dragon: "you may put a permanent card from
    /// your hand onto the battlefield") resumes with the same
    /// `EventContextAmount` the pre-pause resolution observed. Mirror of
    /// `pending_optional_trigger_event`. Set ONLY when stashing into
    /// `pending_optional_effect`; taken by `handle_optional_effect_choice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_optional_trigger_match_count: Option<u32>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub may_trigger_auto_choices: Vec<MayTriggerAutoChoiceRecord>,

    /// CR 103.6: Beginning-of-game abilities queued after all players finish
    /// mulligans. Stored in reverse resolution order so `pop()` preserves APNAP
    /// collection order without shifting.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_begin_game_abilities: Vec<PendingBeginGameAbility>,

    /// True while CR 103.6 beginning-of-game abilities are draining. Used by
    /// optional-choice continuations to resume the queue instead of granting
    /// turn priority early.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub resolving_begin_game_abilities: bool,

    /// The most recently chosen named value (creature type, color, etc.).
    /// Set by the NamedChoice handler, consumed by continuation effects.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_named_choice: Option<ChoiceValue>,

    /// CR 609.7a-b: The most recently chosen damage source and its source
    /// filter. Set by `DamageSourceChoice`, consumed by prevention/replacement
    /// continuation effects, and then cleared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_chosen_damage_source: Option<ChosenDamageSource>,

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

    /// CR 701.20e: Cards the controller is privately "looking at" during the
    /// current resolution — the looker-scoped peek window of a bare
    /// "look at the top card of your library" (Dig with `keep_count == 0`,
    /// `reveal == false`). Unlike `revealed_cards` (public, all players) and
    /// `last_revealed_ids` (condition bookkeeping, not viewer-scoped), these ids
    /// are surfaced by `filter_state_for_viewer` ONLY to `private_look_player`,
    /// so the looking player can see the card while deciding a subsequent
    /// "you may reveal that card" optional, without leaking it to opponents.
    /// Cleared at depth 0 of `resolve_ability_chain` and at action boundaries
    /// once no optional-effect decision that depends on the peek is pending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub private_look_ids: Vec<ObjectId>,
    /// CR 701.20e: The player to whom `private_look_ids` is visible (the looker).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub private_look_player: Option<PlayerId>,

    /// ObjectIds of objects moved by the most recent zone-change effect.
    /// Used by AbilityCondition::ZoneChangedThisWay to gate sub_abilities on
    /// whether the parent effect moved an object matching a type filter.
    /// Cleared at depth 0 in resolve_ability_chain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub last_zone_changed_ids: Vec<ObjectId>,

    /// CR 608.2c + CR 701.38: Per-vote ballots from the most recent
    /// `Effect::Vote` resolution within the current top-level ability
    /// resolution. Each entry is `(voter, choice_index)`; populated by
    /// `vote::resolve_tally` immediately before per-choice sub-effects fan
    /// out, and read by `PlayerFilter::VotedFor` to route per-choice
    /// `player_scope` sub-effects ("for each player who chose money,
    /// you and that player each ...").
    ///
    /// Mirrors `last_zone_changed_ids` lifecycle: cleared at chain depth 0
    /// in `resolve_ability_chain` so cross-resolution leakage is impossible.
    #[serde(default)]
    pub last_vote_ballots: im::Vector<(PlayerId, u8)>,

    /// CR 608.2c + CR 109.5: Player actions performed during the current
    /// top-level ability resolution. Distinct from turn-level trackers like
    /// `players_who_searched_library_this_turn`: this set accumulates only
    /// within one resolving chain so "for each opponent who searched their
    /// library this way" counts the opponents who accepted that offer, even
    /// across player-scope iterations and interactive continuations.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub player_actions_this_way: HashSet<(PlayerId, PlayerActionKind)>,

    /// CR 609.3: Numeric result from the preceding effect in a sub_ability chain.
    /// Set after resolve_effect for effects producing a numeric result (life loss,
    /// damage, counter removal). Read by QuantityRef::PreviousEffectAmount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_amount: Option<i32>,

    /// CR 706.2 + CR 706.4: The actual scalar result available to the current
    /// ability resolution. During a results-table roll, `roll_die::resolve`
    /// stamps each individual die result before resolving that die's branch
    /// (CR 706.3a). After a no-table multi-die roll, it stamps the aggregate
    /// total so an inline "equal to the result(s)" sub_ability consumes the
    /// rolled value rather than the numeric amount of the triggering event
    /// (e.g. combat damage). Resolution-scoped: cleared at `apply()` entry and
    /// at cross-resolution stack boundaries. Follows the `last_effect_amount`
    /// PartialEq-OMISSION pattern: NOT compared in the hand-written `PartialEq`
    /// (safe — always cleared at comparison boundaries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub die_result_this_resolution: Option<i32>,

    /// Count from the most recent interactive effect resolution (e.g., number of cards
    /// actually discarded in a DiscardChoice). Used as fallback for EventContextAmount
    /// in sub_ability continuations where current_trigger_event has no amount.
    /// Cleared at the top of apply() (once per player action).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_count: Option<i32>,

    /// CR 608.2c + CR 701.9a: Per-player counts produced by the preceding
    /// effect in the current ability chain. Used by carried-subject
    /// continuations like "Each player discards ..., then draws that many ..."
    /// after all players have completed the discard pass.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub last_effect_counts_by_player: HashMap<PlayerId, i32>,

    /// CR 608.2e: Clause-local equalization snapshot. Each `player_scope` link
    /// (e.g. a Balance clause) captures its cross-player extremum here before
    /// the APNAP fan-out begins and clears it when the link completes, so every
    /// player in that clause resolves against the same pre-clause board. The
    /// per-link lifecycle is deliberately narrower than `last_vote_ballots`'
    /// per-chain reset — three Balance clauses are three links in one chain and
    /// must each snapshot independently. Transient.
    #[serde(skip)]
    pub clause_minimum_snapshot: Option<ClauseMinimumSnapshot>,

    /// CR 400.7 + CR 608.2c: Number of cards exiled from a hand by the most recent
    /// `Effect::ChangeZoneAll` resolution. Read by `QuantityRef::ExiledFromHandThisResolution`
    /// for "draws a card for each card exiled from their hand this way" patterns
    /// (Deadly Cover-Up, Lost Legacy class). Cleared at the top of apply() so each
    /// resolution starts at 0.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub exiled_from_hand_this_resolution: u32,

    /// CR 725: The current monarch, if any. At the beginning of the monarch's end step,
    /// the monarch draws a card. When a creature deals combat damage to the monarch,
    /// the creature's controller becomes the monarch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub monarch: Option<PlayerId>,

    /// CR 702.131a: Players who have the city's blessing (from Ascend).
    /// Once gained, the city's blessing is permanent for the rest of the game.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub city_blessing: HashSet<PlayerId>,

    /// CR 702.50a-b: Active Epic effects — one per resolved Epic spell. Each
    /// entry is a rest-of-game record: its controller can't cast spells
    /// (CR 702.50b, derived via `epic::is_epic_locked`) and, at the beginning of
    /// each of that player's upkeeps, the engine synthesizes an `EpicCopy`
    /// triggered ability from the stored snapshot (CR 702.50a, fired through the
    /// normal delayed-trigger path in `check_delayed_triggers`). Persistent —
    /// never cleared, never purged at cleanup — so the effect lasts the whole
    /// game. Mirrors the rest-of-game collections `city_blessing` /
    /// `paradigm_primed`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub epic_effects: Vec<EpicEffect>,

    /// Active game-level restrictions (e.g., damage prevention disabled).
    /// Checked by relevant game systems; expired entries cleaned up at phase transitions.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub restrictions: Vec<GameRestriction>,

    /// CR 614.1a + CR 615.3: Game-state-level pending damage replacements.
    /// Instant/sorcery prevention effects (e.g., Fog: "prevent all combat damage")
    /// and resolving-trigger replacements that are not tied to a permanent live here.
    /// Checked during damage application in `deal_damage.rs` and pruned by expiry.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_damage_replacements: Vec<crate::types::ability::ReplacementDefinition>,

    /// CR 703.4q + CR 616.1: Game-state-level pending step-end mana handlers,
    /// scanned at the start of `drain_pending_phase_transition_progress` for
    /// each player in APNAP order. Indexed by `ReplacementId::index` with the
    /// sentinel source `ObjectId(0)` (mirrors `pending_damage_replacements`).
    /// Populated and drained per-player; never serialized in a paused state
    /// outside the engine's own phase-transition drain.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_step_end_mana_handlers: Vec<StepEndManaScanEntry>,

    /// CR 500.1 + CR 616.1: Per-phase APNAP-queue progress for resolving
    /// step-end empty-mana events across players. Set in `enter_phase` when
    /// transitioning between phases; cleared when the queue empties and
    /// `finish_enter_phase` runs. Parallel to `pending_replacement` /
    /// `pending_continuation` as a resume primitive across pipeline pauses
    /// (CR 616.1e iteration).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_phase_transition_progress: Option<PhaseTransitionProgress>,

    /// Transient: set to the phase whose beginning-of-step triggers still need
    /// to run when `auto_advance` returns early because
    /// `pending_phase_transition_progress` is set (CR 616.1 mana-pool choice
    /// deferred `enter_phase`). Cleared when `handle_replacement_choice`
    /// resumes `auto_advance` after the drain completes so beginning-of-step
    /// triggers (CR 513.1 + CR 603.3b) still fire.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deferred_step_trigger_resume: Option<Phase>,

    /// Transient: set by stack.rs before resolving a triggered ability, cleared after.
    /// Used by event-context TargetFilter variants to resolve trigger event data.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_trigger_event: Option<GameEvent>,
    /// CR 603.2c: Count of subjects in the firing trigger's event batch that
    /// satisfied the trigger's `valid_card` filter. Set in lockstep with
    /// `current_trigger_event`/`current_trigger_events` when a batched
    /// triggered ability begins resolving. Read by
    /// `QuantityRef::EventContextAmount` so "that many" resolves to the
    /// filtered subject count (e.g. The Ur-Dragon: "Whenever one or more
    /// Dragons you control attack, draw that many cards"). `None` outside
    /// batched-trigger resolution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_trigger_match_count: Option<u32>,
    /// CR 707.10: Transient snapshot of the spell or ability stack entry
    /// currently resolving. `resolve_top` pops the entry off `state.stack`
    /// before running its effect, so a `CopySpell { target: SelfRef }` carried
    /// as the resolving spell's own effect (the Chain cycle — Chain of Acid /
    /// Plasma / Smog / Vapor — "you may copy this spell") can no longer find
    /// itself on the stack. This holds the popped entry; `copy_spell::resolve`
    /// falls back to it for `SelfRef`. Set by `resolve_top` before
    /// `execute_effect` and cleared at the START of the next `resolve_top` —
    /// it must survive a `WaitingFor::OptionalEffectChoice` round-trip (the
    /// Chain cycle defers the copy past a player decision). For that same
    /// reason it must be serialized: a server game persisted while a
    /// Chain-cycle optional-copy prompt is pending and later reloaded would
    /// otherwise lose the entry and silently drop the accepted copy. Mirrors
    /// `current_trigger_event`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolving_stack_entry: Option<StackEntry>,
    /// Transient plural form of `current_trigger_event` for batched triggers.
    /// Event-context filters that can legally compare against a group read this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_trigger_events: Vec<GameEvent>,
    /// Full event batches for triggered abilities currently on the stack,
    /// keyed by stack entry id. Single-event triggers omit an entry here.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stack_trigger_event_batches: HashMap<ObjectId, Vec<GameEvent>>,

    /// CR 400.7: Last Known Information cache.
    /// Populated before zone changes for objects leaving the battlefield.
    /// Cleared on phase/step transitions via `advance_phase()`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lki_cache: HashMap<ObjectId, LKISnapshot>,

    /// CR 607.2b + CR 603.10e: Last-known "cards exiled with [source]" linkage,
    /// captured when a source with `TrackedBySource` exile links leaves the
    /// battlefield. The live `exile_links` are pruned on battlefield exit
    /// (CR 400.7), but an ability that sacrifices its own source as a cost and
    /// then refers to "cards exiled with this permanent" (Rod of Absorption)
    /// must still see those cards at resolution. `linked_exile_cards_for_source`
    /// consults this as its final fallback, filtered to cards still in exile, so
    /// stale entries (cards that later left exile) contribute nothing.
    /// Cleared on phase/step transitions via `advance_phase()`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub linked_exile_lki: HashMap<ObjectId, Vec<LinkedExileSnapshot>>,

    /// Transient: set by PayCost resolver when payment fails.
    /// Gates IfYouDo sub-abilities. Reset in DecideOptionalEffect handler.
    #[serde(skip)]
    pub cost_payment_failed_flag: bool,

    /// CR 601.2h + CR 616.1: Resume state when `handle_discard_for_cost` pauses mid-loop
    /// for a replacement choice. The card at `paused_at_index` is completed by
    /// `handle_replacement_choice`; resume continues at `paused_at_index + 1`.
    #[serde(skip)]
    pub pending_discard_for_cost: Option<PendingDiscardForCostResume>,

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
    /// CR 510.2 + CR 615.7: Transient per-shield combat-damage prevention tally.
    /// Set to `Some(empty)` by `apply_combat_damage` for the duration of one
    /// simultaneous combat-damage batch. While `Some`, the `Prevention::All`
    /// branch of the damage-replacement applier accumulates each prevented
    /// amount into this map (keyed by the shield's `ReplacementId`) instead of
    /// stamping `last_effect_count` per source. After the batch, the combat
    /// resolver reads the aggregate to fire each shield's `runtime_execute`
    /// rider exactly once (CR 615.13). Always `None` at every `apply()`
    /// boundary, so it is excluded from serialization and structural equality.
    #[serde(skip)]
    pub combat_prevention_tally: Option<HashMap<ReplacementId, i32>>,
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
    /// Snapshot of the originating object's name, captured at construction.
    /// The originating spell/ability typically moves to a new zone (graveyard,
    /// stack→exile, etc.) with a new ObjectId per CR 400.7 after resolution,
    /// so live `state.objects[source_id]` lookup may not return the original
    /// card. Snapshot is captured here so attribution display ("+3/+3 from
    /// Giant Growth") survives the source's zone change.
    #[serde(default)]
    pub source_name: String,
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
    /// CR 701.24a: the library placement requested by the original `move_object`
    /// call whose replacement consult parked here (W3 library-placement arm only).
    /// `Some` solely for a parked Library-targeting `ZoneChange`; the resume path
    /// (`handle_replacement_choice`) threads it back into the delivery so the
    /// object lands at the requested index instead of the tail auto-shuffling it
    /// away. `None` for every other parked event (the common case).
    #[serde(default)]
    pub library_placement: Option<crate::types::ability::LibraryPosition>,
    /// CR 614.12a: set when an optional `MayCost` accept already paid its cost
    /// but the payment paused for an interactive sub-choice (e.g. Mox Diamond's
    /// "discard a land card" with more than one eligible land surfaces a
    /// `WaitingFor::DiscardChoice`). The pending record is re-parked so the
    /// post-choice resume re-enters `continue_replacement` with the accept
    /// index; this flag tells that resume to continue MayCost payment from
    /// `may_cost_remaining` instead of restarting the whole cost. `false` for
    /// every other parked event.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub may_cost_paid: bool,
    /// CR 614.12a + CR 118.12: unpaid suffix of a composite `MayCost` after an
    /// interactive sub-choice paused payment. `None` means the sub-choice was
    /// the final cost component; `Some(cost)` is paid on the post-choice resume
    /// before the replacement is applied.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub may_cost_remaining: Option<AbilityCost>,
}

/// CR 703.4q + CR 616.1 + CR 614.1a: One step-end mana handler entry pending
/// resolution for the current phase transition. Built from the printed-static
/// and transient-continuous-effect scans at the start of each per-player drain,
/// and addressed by the replacement pipeline via `ReplacementId { source:
/// ObjectId(0), index }`.
///
/// `description` is the player-facing string surfaced in `WaitingFor::
/// ReplacementChoice::candidate_descriptions` when multiple handlers apply to
/// the same emptying event and CR 616.1 requires the affected player to choose
/// ordering.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepEndManaScanEntry {
    pub source: ObjectId,
    pub controller: PlayerId,
    pub filter: Option<ManaColor>,
    pub action: StepEndManaAction,
    pub description: String,
}

/// CR 500.1 + CR 616.1: Resume primitive for the per-phase APNAP-queue of
/// step-end empty-mana events. Drained by
/// `drain_pending_phase_transition_progress` (commit 2). When all players are
/// processed (queue empties), the drain calls `finish_enter_phase` to complete
/// the phase entry (priority reset, LKI clear, `PhaseChanged` emission).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PhaseTransitionProgress {
    pub remaining_players: VecDeque<PlayerId>,
    pub next_phase: Phase,
    pub in_combat: bool,
    pub entering_cleanup: bool,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_controller: Option<PlayerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cast_timing_permission: Option<crate::types::ability::CastTimingPermission>,
    pub spell_targets: Vec<crate::types::ability::TargetRef>,
    #[serde(default)]
    pub actual_mana_spent: u32,
    /// CR 702.33d + CR 702.33f: Carry kicker payment data through the
    /// pending-spell-resolution detour (replacement-needs-choice path) so the
    /// permanent ends up with the same `kickers_paid` as the direct resolution
    /// path in `stack.rs`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub kickers_paid: Vec<crate::types::ability::KickerVariant>,
    /// CR 601.2b/f/h + CR 702.157a: Carry non-kicker additional-cost payment
    /// count through the replacement-choice detour, matching the direct
    /// stack-resolution path.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub additional_cost_payment_count: u32,
    /// CR 607.2g + CR 702.157b/702.175b: Carry per-instance non-kicker
    /// additional-cost payment data through the replacement-choice detour so
    /// ETB-linked Squad/Offspring triggers read the same facts as the direct
    /// stack-resolution path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub additional_cost_payments: Vec<crate::types::ability::AdditionalCostInstancePayment>,
    /// CR 702.51c: Carry convoked-creature data through the replacement-choice
    /// detour so ETB triggers/replacements see the same cast history as the
    /// direct resolution path.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub convoked_creatures: Vec<ObjectId>,
}

/// CR 702.140c + CR 730.2: Context stored when a mutating creature spell resolves
/// with a legal target. Resolution pauses (the stack entry is popped, mirroring
/// the Clone replacement-needs-choice detour) until the spell's controller chooses
/// top or bottom via `GameAction::ChooseMutateMergeSide`; then
/// `merge::handle_mutate_merge_choice` performs the merge.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMutateMerge {
    /// The resolving mutate spell object (the card/token being merged onto the
    /// target). Retains its original owner so CR 730.3 can route it correctly.
    pub merging_id: ObjectId,
    /// The surviving battlefield creature. The merged permanent keeps THIS
    /// object's `ObjectId` (CR 730.2c continuity).
    pub target_id: ObjectId,
    /// The mutate spell's controller — the player who chooses top/bottom
    /// (CR 702.140c).
    pub controller: PlayerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScheduledTurnControl {
    pub target_player: PlayerId,
    pub controller: PlayerId,
    #[serde(default)]
    pub grant_extra_turn_after: bool,
}

/// CR 500.8: An extra phase added to a turn by an effect, anchored to the
/// phase it occurs *directly after*. Stored on `GameState.extra_phases` and
/// consumed by `advance_phase` only when the current phase matches `anchor`.
///
/// CR 500.8 ("phases are added directly after the specified phase") requires
/// per-entry anchor typing — a flat `Vec<Phase>` consumed at every transition
/// silently misroutes Aurelia-style "after this phase" extra combats into the
/// middle of the current combat, skipping declare-blockers / combat-damage /
/// end-of-combat.
///
/// LIFO ordering ("the most recently created phase will occur first") is
/// preserved by scanning `extra_phases` from the end (`rposition`) for the
/// first matching anchor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraPhase {
    /// The phase after which this extra phase is inserted (CR 500.8).
    pub anchor: Phase,
    /// The phase to insert.
    pub phase: Phase,
}

// Pin `GameState: Send + Sync` at compile time. Blocks accidental imports of
// `im-rc` (the single-threaded variant of `im`, which is !Send/!Sync) and
// catches any future field addition that violates thread-safety.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<GameState>();
};

impl GameState {
    /// CR 702.26b: Returns battlefield object ids filtered to only phased-in
    /// permanents. Use this instead of `state.battlefield.iter()` anywhere a
    /// rule would otherwise treat a phased-out permanent as existing
    /// (state-based actions, combat scans, trigger source scans, etc.).
    pub fn battlefield_phased_in_ids(&self) -> Vec<ObjectId> {
        self.battlefield
            .iter()
            .copied()
            .filter(|id| self.objects.get(id).is_some_and(|obj| obj.is_phased_in()))
            .collect()
    }

    /// CR 730.2: True if `object_id` is an absorbed (non-surviving) component of
    /// some merged permanent. Such a component is part of one battlefield object
    /// (the merged permanent, identified by the surviving target's `ObjectId`) and
    /// is NOT independently present in `state.battlefield`, yet its `GameObject`
    /// is retained in `state.objects` so the CR 730.3 leave-split can restore it.
    ///
    /// Any code that scans `state.objects` and gates on `obj.zone == Battlefield`
    /// to enumerate independent permanents MUST skip these ids — otherwise the
    /// single merged permanent would be observed as multiple permanents (double-
    /// counted as a same-name permanent, an extra mana source, etc.).
    pub fn is_absorbed_merge_component(&self, object_id: ObjectId) -> bool {
        self.objects.get(&object_id).is_some_and(|obj| {
            obj.zone == Zone::Battlefield && !self.battlefield.contains(&object_id)
        })
    }

    /// CR 508.6: True if `attacker` declared one or more creatures attacking
    /// `defender` this turn (reads the per-turn attacked-defenders ledger).
    pub fn has_attacked(&self, attacker: PlayerId, defender: PlayerId) -> bool {
        self.attacked_defenders_this_turn
            .get(&attacker)
            .is_some_and(|defenders| defenders.contains(&defender))
    }

    /// CR 508.6: True if `attacker` was declared attacking `defender` this turn.
    pub fn creature_attacked_player_this_turn(
        &self,
        attacker: ObjectId,
        defender: PlayerId,
    ) -> bool {
        self.creature_attacked_defenders_this_turn
            .get(&attacker)
            .is_some_and(|defenders| defenders.contains(&defender))
    }

    /// CR 508.6: Did `subject` attack player `target` within `scope`? Centralizes
    /// the turn- vs combat-scoped lookup behind `PlayerFilter::OpponentAttacked`.
    pub fn opponent_attacked(
        &self,
        subject: AttackSubject,
        scope: crate::types::ability::AttackScope,
        controller: PlayerId,
        source_id: ObjectId,
        target: PlayerId,
    ) -> bool {
        use crate::types::ability::{AttackScope, AttackSubject};
        match (subject, scope) {
            (AttackSubject::You, AttackScope::ThisTurn) => self.has_attacked(controller, target),
            (AttackSubject::Source, AttackScope::ThisTurn) => {
                self.creature_attacked_player_this_turn(source_id, target)
            }
            (AttackSubject::You, AttackScope::ThisCombat) => {
                self.player_attacked_player_this_combat(controller, target)
            }
            (AttackSubject::Source, AttackScope::ThisCombat) => {
                self.creature_attacked_player_this_combat(source_id, target)
            }
        }
    }

    /// CR 508.6 + CR 506.1: Within the CURRENT combat, did `attacker_controller`
    /// declare any creature attacking `defender`? Read from the combat's
    /// declaration ledger, so it reflects only this combat while surviving
    /// attackers leaving combat before a trigger resolves. `defending_player`
    /// already resolves planeswalker/battle attacks to the defending player
    /// (CR 508.5).
    pub fn player_attacked_player_this_combat(
        &self,
        attacker_controller: PlayerId,
        defender: PlayerId,
    ) -> bool {
        self.combat.as_ref().is_some_and(|combat| {
            combat
                .attacked_defenders_this_combat
                .get(&attacker_controller)
                .is_some_and(|defenders| defenders.contains(&defender))
        })
    }

    /// CR 508.6: Within the CURRENT combat, did creature `source_id` attack
    /// `defender`? Reads declaration history, not live combat membership.
    pub fn creature_attacked_player_this_combat(
        &self,
        source_id: ObjectId,
        defender: PlayerId,
    ) -> bool {
        self.combat.as_ref().is_some_and(|combat| {
            combat
                .creature_attacked_defenders_this_combat
                .get(&source_id)
                .is_some_and(|defenders| defenders.contains(&defender))
        })
    }

    /// CR 508.6 + CR 702.121a: Defending players the subject attacked in the
    /// current combat, read from declaration history for Melee-style counts.
    pub fn attacked_defenders_this_combat_for(
        &self,
        subject: AttackSubject,
        controller: PlayerId,
        source_id: ObjectId,
    ) -> Option<&HashSet<PlayerId>> {
        let combat = self.combat.as_ref()?;
        match subject {
            AttackSubject::You => combat.attacked_defenders_this_combat.get(&controller),
            AttackSubject::Source => combat
                .creature_attacked_defenders_this_combat
                .get(&source_id),
        }
    }

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
            objects: im::HashMap::default(),
            next_object_id: 1,
            battlefield: im::Vector::new(),
            stack: im::Vector::new(),
            stack_paid_facts: HashMap::new(),
            exile: im::Vector::new(),
            command_zone: im::Vector::new(),
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
            replacement_may_cost_paused: false,
            post_replacement_continuation: None,
            legacy_post_replacement_effect: None,
            legacy_post_replacement_resolved_effect: None,
            post_replacement_source: None,
            post_replacement_event_source: None,
            post_replacement_event_target: None,
            pending_spell_resolution: None,
            pending_mutate_merge: None,
            deferred_entry_events: Vec::new(),
            layers_dirty: LayersDirty::full(),
            static_gate_truth: im::HashMap::new(),
            trigger_index: TriggerIndex::default(),
            static_source_index: StaticSourceIndex::default(),
            next_timestamp: 1,
            public_state_dirty: PublicStateDirty::all_dirty(),
            state_revision: 0,
            transient_continuous_effects: im::Vector::new(),
            next_continuous_effect_id: 1,
            attribution: im::HashMap::new(),
            day_night: None,
            spells_cast_this_turn: 0,
            spells_cast_last_turn: None,
            pending_trigger: None,
            pending_trigger_event_batch: Vec::new(),
            pending_trigger_entry: None,
            deferred_triggers: Vec::new(),
            pending_trigger_order: None,
            exile_links: Vec::new(),
            paradigm_primed: Vec::new(),
            delayed_triggers: Vec::new(),
            tracked_object_sets: HashMap::new(),
            next_tracked_set_id: 1,
            chain_tracked_set_id: None,
            tracked_set_member_causes: HashMap::new(),
            commander_cast_count: HashMap::new(),
            commander_cast_owners: HashMap::new(),
            extra_turns: Vec::new(),
            turns_to_skip: vec![0; player_count as usize],
            steps_to_skip: vec![HashMap::new(); player_count as usize],
            scheduled_turn_controls: Vec::new(),
            extra_phases: Vec::new(),
            seat_order,
            format_config: config,
            eliminated_players: Vec::new(),
            commander_damage: Vec::new(),
            priority_passes: BTreeSet::new(),
            auto_pass: HashMap::new(),
            phase_stops: HashMap::new(),
            lands_tapped_for_mana: HashMap::new(),
            final_mulligan_counts: HashMap::new(),
            prepaid_mulligan_bottoms: HashMap::new(),
            match_config: MatchConfig::default(),
            match_phase: MatchPhase::InGame,
            match_score: MatchScore::default(),
            game_number: default_game_number(),
            current_starting_player: PlayerId(0),
            next_game_chooser: None,
            deck_pools: Vec::new(),
            outside_game_cards_brought_in: Vec::new(),
            sideboard_submitted: Vec::new(),
            triggers_fired_this_turn: HashSet::new(),
            trigger_fire_counts_this_turn: HashMap::new(),
            triggers_fired_this_turn_per_opponent: HashSet::new(),
            triggers_fired_this_game: HashSet::new(),
            activated_abilities_this_turn: HashMap::new(),
            activated_abilities_this_game: HashMap::new(),
            crew_activated_this_turn: HashSet::new(),
            loyalty_abilities_activated_this_turn: HashMap::new(),
            extra_loyalty_activations_this_turn: HashMap::new(),
            exerted_this_turn: std::collections::HashSet::new(),
            pending_attack_trigger_events: Vec::new(),
            ability_resolutions_this_turn: HashMap::new(),
            graveyard_cast_permissions_used: HashSet::new(),
            graveyard_cast_permissions_used_per_type: HashSet::new(),
            pending_permanent_type_slot: None,
            hand_cast_free_permissions_used: HashSet::new(),
            exile_play_permissions_used: HashSet::new(),
            exile_play_single_use_consumed: HashSet::new(),
            exile_cast_permissions_used: HashSet::new(),
            cards_exiled_with_source_this_turn: HashMap::new(),
            first_card_drawn_this_turn: HashMap::new(),
            cards_drawn_this_turn: HashMap::new(),
            pending_miracle_offers: Vec::new(),
            spells_cast_this_game: HashMap::new(),
            spells_cast_this_game_by_player: HashMap::new(),
            spells_cast_this_turn_by_player: HashMap::new(),
            lands_played_this_turn_by_player: HashMap::new(),
            players_who_searched_library_this_turn: HashSet::new(),
            player_actions_this_turn: Vec::new(),
            players_attacked_this_step: HashSet::new(),
            players_attacked_this_turn: HashSet::new(),
            attacking_creatures_this_turn: HashMap::new(),
            attacked_defenders_this_turn: HashMap::new(),
            creature_attacked_defenders_this_turn: HashMap::new(),
            combat_phases_started_this_turn: 0,
            creatures_attacked_this_turn: HashSet::new(),
            attacker_declarations_this_turn: Vec::new(),
            creatures_blocked_this_turn: HashSet::new(),
            players_who_created_token_this_turn: HashSet::new(),
            created_tokens_this_turn: Vec::new(),
            counter_added_this_turn: Vec::new(),
            players_who_discarded_card_this_turn: HashSet::new(),
            cards_discarded_this_turn_by_player: HashMap::new(),
            players_who_sacrificed_artifact_this_turn: HashSet::new(),
            sacrificed_permanents_this_turn: Vec::new(),
            zone_changes_this_turn: Vec::new(),
            battlefield_entries_this_turn: Vec::new(),
            damage_dealt_this_turn: im::Vector::new(),
            assassin_or_commander_dealt_combat_damage_this_turn: HashSet::new(),
            creature_types_dealt_combat_damage_this_turn: im::HashSet::new(),
            mana_spent_on_spells_this_turn: HashMap::new(),
            pending_spell_cost_reductions: Vec::new(),
            pending_next_spell_modifiers: Vec::new(),
            pending_etb_counters: Vec::new(),
            modal_modes_chosen_this_turn: HashSet::new(),
            modal_modes_chosen_this_game: HashSet::new(),
            revealed_cards: HashSet::new(),
            public_revealed_cards: HashSet::new(),
            pending_continuation: None,
            pending_repeat_iteration: None,
            pending_change_zone_iteration: None,
            devour_eligible_snapshot: None,
            merged_card_component_route: None,
            pending_copy_token_resolution: None,
            pending_coin_flip: None,
            pending_repeat_until: None,
            pending_choose_one_of: None,
            pending_counter_moves: None,
            pending_batch_deliveries: None,
            pending_counter_additions: None,
            pending_optional_effect: None,
            pending_optional_trigger_event: None,
            pending_optional_trigger_match_count: None,
            may_trigger_auto_choices: Vec::new(),
            pending_begin_game_abilities: Vec::new(),
            resolving_begin_game_abilities: false,
            last_named_choice: None,
            last_chosen_damage_source: None,
            all_creature_types: Vec::new(),
            all_card_names: Arc::from([]),
            card_face_registry: Arc::new(HashMap::new()),
            log_player_names: Vec::new(),
            last_created_token_ids: Vec::new(),
            last_revealed_ids: Vec::new(),
            private_look_ids: Vec::new(),
            private_look_player: None,
            last_zone_changed_ids: Vec::new(),
            last_vote_ballots: im::Vector::new(),
            player_actions_this_way: HashSet::new(),
            last_effect_amount: None,
            die_result_this_resolution: None,
            last_effect_count: None,
            last_effect_counts_by_player: HashMap::new(),
            clause_minimum_snapshot: None,
            exiled_from_hand_this_resolution: 0,
            monarch: None,
            city_blessing: HashSet::new(),
            epic_effects: Vec::new(),
            restrictions: Vec::new(),
            pending_damage_replacements: Vec::new(),
            pending_step_end_mana_handlers: Vec::new(),
            pending_phase_transition_progress: None,
            deferred_step_trigger_resume: None,
            current_trigger_event: None,
            current_trigger_match_count: None,
            resolving_stack_entry: None,
            current_trigger_events: Vec::new(),
            stack_trigger_event_batches: HashMap::new(),
            lki_cache: HashMap::new(),
            linked_exile_lki: HashMap::new(),
            cost_payment_failed_flag: false,
            pending_discard_for_cost: None,
            pending_cast: None,
            ring_level: HashMap::new(),
            ring_bearer: HashMap::new(),
            dungeon_progress: HashMap::new(),
            initiative: None,
            combat_prevention_tally: None,
            cancelled_casts: Vec::new(),
            pending_activations: Vec::new(),
            commander_declined_zone_return: HashSet::new(),
            debug_mode: false,
            debug_permitted: BTreeSet::new(),
            debug_infinite_mana: BTreeSet::new(),
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

    pub fn may_trigger_auto_choice(&self, key: &MayTriggerAutoChoiceKey) -> Option<AutoMayChoice> {
        self.may_trigger_auto_choices
            .iter()
            .find(|record| record.key == *key)
            .map(|record| record.choice)
    }

    pub fn set_may_trigger_auto_choice(
        &mut self,
        key: MayTriggerAutoChoiceKey,
        choice: AutoMayChoice,
    ) {
        if let Some(record) = self
            .may_trigger_auto_choices
            .iter_mut()
            .find(|record| record.key == key)
        {
            record.choice = choice;
        } else {
            self.may_trigger_auto_choices
                .push(MayTriggerAutoChoiceRecord { key, choice });
        }
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
        // CR 400.7 + CR 603.10: When a triggered ability creates a transient
        // continuous effect AFTER its source has left a public zone (e.g., a
        // leaves-the-battlefield trigger), `state.objects` no longer holds the
        // pre-zone-change ObjectId — `lki_cache` is the canonical snapshot of
        // the source's characteristics at the moment it left. Falling back to
        // LKI mirrors the same name-resolution pattern used in `filter.rs`,
        // `quantity.rs`, and `log.rs`.
        let source_name = self
            .objects
            .get(&source_id)
            .map(|o| o.name.clone())
            .or_else(|| self.lki_cache.get(&source_id).map(|lki| lki.name.clone()))
            .unwrap_or_default();
        self.transient_continuous_effects
            .push_back(TransientContinuousEffect {
                id,
                source_id,
                controller,
                timestamp,
                duration,
                affected,
                modifications,
                condition,
                source_name,
            });
        self.layers_dirty.mark_full();
        id
    }

    /// CR 614.12a + CR 615.5: Migrate the pre-2026-05-09 audit M4 split-slot
    /// shape (`post_replacement_effect` + `post_replacement_resolved_effect`)
    /// into the unified `post_replacement_continuation` slot. Idempotent —
    /// no-op when both legacy slots are empty (the steady-state case once a
    /// post-load hop has run). Called from `finalize_public_state` so every
    /// deserialize boundary (engine-wasm restore, multiplayer host resume,
    /// gamePersistence rehydration) gets the migration without per-callsite
    /// plumbing. The Resolved arm wins when both legacy slots are
    /// (impossibly) populated, mirroring the pre-fold dispatcher precedence
    /// at `engine_replacement.rs::apply_pending_post_replacement_effect`.
    pub fn migrate_post_replacement_continuation(&mut self) {
        if self.post_replacement_continuation.is_some() {
            self.legacy_post_replacement_effect = None;
            self.legacy_post_replacement_resolved_effect = None;
            return;
        }
        if let Some(resolved) = self.legacy_post_replacement_resolved_effect.take() {
            self.post_replacement_continuation =
                Some(crate::types::ability::PostReplacementContinuation::Resolved(resolved));
            self.legacy_post_replacement_effect = None;
        } else if let Some(template) = self.legacy_post_replacement_effect.take() {
            self.post_replacement_continuation =
                Some(crate::types::ability::PostReplacementContinuation::Template(template));
        }
    }

    /// CR 104.4b: a cheap pre-filter fingerprint of loop-mutable state. It need
    /// NOT be complete — a confirmation pass (`loop_states_equal`) deep-compares
    /// before any draw, so a fingerprint collision can never cause a wrongful
    /// draw; the fingerprint only decides *when to bother confirming*. Includes
    /// the RNG stream position so a loop that consumes randomness (shuffle, coin
    /// flip) gets a distinct fingerprint and is never confirmed — CR 104.4b
    /// excludes loops containing a nondeterministic action.
    pub(crate) fn loop_fingerprint(&self) -> u64 {
        use std::hash::{Hash, Hasher};
        let mut h = rustc_hash::FxHasher::default();
        self.turn_number.hash(&mut h);
        self.phase.hash(&mut h);
        self.active_player.hash(&mut h);
        self.priority_player.hash(&mut h);
        self.stack.len().hash(&mut h);
        self.objects.len().hash(&mut h);
        // im::Vector<ObjectId>: Hash, ordered.
        self.battlefield.hash(&mut h);
        for player in &self.players {
            player.id.hash(&mut h);
            player.life.hash(&mut h);
            player.hand.len().hash(&mut h);
            player.library.len().hash(&mut h);
            player.graveyard.len().hash(&mut h);
        }
        // Per-object tapped/damage rollup cheaply distinguishes tap/untap and
        // damage-ping states without a full content hash. Folded together with XOR
        // so the rollup is order-independent (im::HashMap iteration order is not
        // stable across states) in O(N) with zero allocation — sorting the id set
        // on every call was the hot-path cost on large boards (~2,936 permanents).
        // Each per-object hash folds in the unique id, so equal (tapped, damage)
        // on different objects never cancels.
        let mut objects_rollup = 0u64;
        for (id, object) in &self.objects {
            let mut object_hash = rustc_hash::FxHasher::default();
            id.0.hash(&mut object_hash);
            object.tapped.hash(&mut object_hash);
            object.damage_marked.hash(&mut object_hash);
            objects_rollup ^= object_hash.finish();
        }
        objects_rollup.hash(&mut h);
        // Any randomness consumed ⇒ different stream position ⇒ no collision.
        self.rng.get_word_pos().hash(&mut h);
        h.finish()
    }

    /// Clone with the volatile, monotonically-advancing fields the `PartialEq`
    /// impl compares zeroed/canonicalized, so two states reached at different
    /// times can compare equal on everything a mandatory action could change.
    pub(crate) fn normalize_for_loop(&self) -> GameState {
        let mut clone = self.clone();
        clone.state_revision = 0;
        clone.next_timestamp = 0;
        clone.next_object_id = 0;
        clone.layers_dirty = LayersDirty::full();
        clone.public_state_dirty = PublicStateDirty::all_dirty();
        clone
    }
}

/// CR 104.4b confirmation between two states that have BOTH already been
/// `normalize_for_loop`d. Reuses `PartialEq` for the ~95 non-object fields and
/// supplements its `objects.len()`-only object check with per-object content
/// equality. Only a true match permits a draw, so the cheap `loop_fingerprint`
/// can never cause a wrongful draw.
pub(crate) fn loop_states_equal(a: &GameState, b: &GameState) -> bool {
    a == b && objects_content_eq(&a.objects, &b.objects)
}

/// CR 104.4b: per-object mutable-content equality — supplements `GameState`'s
/// `objects.len()`-only `PartialEq` object check. Card-intrinsic fields
/// (`base_*`, abilities, definitions) are immutable for a given object id within
/// a game and so cannot differ between two states; only the fields a mandatory
/// action could change are compared.
fn objects_content_eq(
    a: &im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>,
    b: &im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>,
) -> bool {
    a.len() == b.len()
        && a.iter().all(|(id, x)| {
            b.get(id).is_some_and(|y| {
                x.controller == y.controller
                    && x.zone == y.zone
                    && x.tapped == y.tapped
                    && x.face_down == y.face_down
                    && x.flipped == y.flipped
                    && x.transformed == y.transformed
                    // CR 702.26: phasing is mutable per-object status that leaves
                    // zone and objects.len() unchanged, so two states differing only
                    // in phased-in/out must not compare equal — else a loop that
                    // phases a permanent in and out is a wrongful CR 104.4b draw.
                    && x.phase_status == y.phase_status
                    && x.damage_marked == y.damage_marked
                    && x.dealt_deathtouch_damage == y.dealt_deathtouch_damage
                    && x.attached_to == y.attached_to
                    && x.attachments == y.attachments
                    && x.paired_with == y.paired_with
                    && x.counters == y.counters
                    && x.power == y.power
                    && x.toughness == y.toughness
                    && x.loyalty == y.loyalty
                    && x.defense == y.defense
                    && x.name == y.name
            })
        })
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
            && self.stack_paid_facts == other.stack_paid_facts
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
            && self.deferred_entry_events == other.deferred_entry_events
            && self.layers_dirty == other.layers_dirty
            // `static_gate_truth` is INTENTIONALLY excluded: unlike
            // `layers_dirty`/`public_state_dirty` (which encode pending work),
            // it is pure derived/self-healing state (reconstructed at the next
            // full eval; implied entirely by objects + battlefield +
            // static_definitions). Including it would break AI-search dedup on
            // semantically-identical positions whose caches differ only in
            // freshness.
            && self.next_timestamp == other.next_timestamp
            && self.public_state_dirty == other.public_state_dirty
            && self.state_revision == other.state_revision
            && self.day_night == other.day_night
            && self.spells_cast_this_turn == other.spells_cast_this_turn
            && self.spells_cast_last_turn == other.spells_cast_last_turn
            && self.pending_trigger == other.pending_trigger
            && self.pending_trigger_entry == other.pending_trigger_entry
            && self.deferred_triggers == other.deferred_triggers
            && self.pending_trigger_order == other.pending_trigger_order
            && self.exile_links == other.exile_links
            && self.paradigm_primed == other.paradigm_primed
            && self.delayed_triggers == other.delayed_triggers
            && self.epic_effects == other.epic_effects
            && self.tracked_object_sets == other.tracked_object_sets
            && self.next_tracked_set_id == other.next_tracked_set_id
            && self.chain_tracked_set_id == other.chain_tracked_set_id
            && self.tracked_set_member_causes == other.tracked_set_member_causes
            && self.commander_cast_count == other.commander_cast_count
            && self.commander_cast_owners == other.commander_cast_owners
            && self.commander_declined_zone_return == other.commander_declined_zone_return
            && self.extra_turns == other.extra_turns
            && self.turns_to_skip == other.turns_to_skip
            && self.steps_to_skip == other.steps_to_skip
            && self.scheduled_turn_controls == other.scheduled_turn_controls
            && self.extra_phases == other.extra_phases
            && self.seat_order == other.seat_order
            && self.format_config == other.format_config
            && self.eliminated_players == other.eliminated_players
            && self.commander_damage == other.commander_damage
            && self.priority_passes == other.priority_passes
            && self.auto_pass == other.auto_pass
            && self.phase_stops == other.phase_stops
            && self.lands_tapped_for_mana == other.lands_tapped_for_mana
            && self.match_config == other.match_config
            && self.match_phase == other.match_phase
            && self.match_score == other.match_score
            && self.game_number == other.game_number
            && self.current_starting_player == other.current_starting_player
            && self.next_game_chooser == other.next_game_chooser
            && self.deck_pools == other.deck_pools
            && self.outside_game_cards_brought_in == other.outside_game_cards_brought_in
            && self.sideboard_submitted == other.sideboard_submitted
            && self.triggers_fired_this_turn == other.triggers_fired_this_turn
            && self.trigger_fire_counts_this_turn == other.trigger_fire_counts_this_turn
            && self.triggers_fired_this_turn_per_opponent == other.triggers_fired_this_turn_per_opponent
            && self.triggers_fired_this_game == other.triggers_fired_this_game
            && self.activated_abilities_this_turn == other.activated_abilities_this_turn
            && self.activated_abilities_this_game == other.activated_abilities_this_game
            && self.crew_activated_this_turn == other.crew_activated_this_turn
            && self.loyalty_abilities_activated_this_turn
                == other.loyalty_abilities_activated_this_turn
            && self.extra_loyalty_activations_this_turn == other.extra_loyalty_activations_this_turn
            && self.ability_resolutions_this_turn == other.ability_resolutions_this_turn
            && self.graveyard_cast_permissions_used == other.graveyard_cast_permissions_used
            && self.graveyard_cast_permissions_used_per_type
                == other.graveyard_cast_permissions_used_per_type
            && self.pending_permanent_type_slot == other.pending_permanent_type_slot
            && self.hand_cast_free_permissions_used == other.hand_cast_free_permissions_used
            && self.exile_play_permissions_used == other.exile_play_permissions_used
            && self.exile_play_single_use_consumed == other.exile_play_single_use_consumed
            && self.exile_cast_permissions_used == other.exile_cast_permissions_used
            && self.cards_exiled_with_source_this_turn == other.cards_exiled_with_source_this_turn
            && self.first_card_drawn_this_turn == other.first_card_drawn_this_turn
            && self.cards_drawn_this_turn == other.cards_drawn_this_turn
            && self.pending_miracle_offers == other.pending_miracle_offers
            && self.spells_cast_this_game == other.spells_cast_this_game
            && self.spells_cast_this_game_by_player == other.spells_cast_this_game_by_player
            && self.spells_cast_this_turn_by_player == other.spells_cast_this_turn_by_player
            && self.lands_played_this_turn_by_player == other.lands_played_this_turn_by_player
            && self.players_who_searched_library_this_turn
                == other.players_who_searched_library_this_turn
            && self.player_actions_this_turn == other.player_actions_this_turn
            && self.players_attacked_this_step == other.players_attacked_this_step
            && self.players_attacked_this_turn == other.players_attacked_this_turn
            && self.attacking_creatures_this_turn == other.attacking_creatures_this_turn
            && self.attacked_defenders_this_turn == other.attacked_defenders_this_turn
            && self.creature_attacked_defenders_this_turn
                == other.creature_attacked_defenders_this_turn
            && self.combat_phases_started_this_turn == other.combat_phases_started_this_turn
            && self.creatures_attacked_this_turn == other.creatures_attacked_this_turn
            && self.attacker_declarations_this_turn == other.attacker_declarations_this_turn
            && self.creatures_blocked_this_turn == other.creatures_blocked_this_turn
            && self.players_who_created_token_this_turn == other.players_who_created_token_this_turn
            && self.created_tokens_this_turn == other.created_tokens_this_turn
            && self.counter_added_this_turn == other.counter_added_this_turn
            && self.players_who_discarded_card_this_turn
                == other.players_who_discarded_card_this_turn
            && self.cards_discarded_this_turn_by_player == other.cards_discarded_this_turn_by_player
            && self.players_who_sacrificed_artifact_this_turn
                == other.players_who_sacrificed_artifact_this_turn
            && self.sacrificed_permanents_this_turn == other.sacrificed_permanents_this_turn
            && self.zone_changes_this_turn == other.zone_changes_this_turn
            && self.battlefield_entries_this_turn == other.battlefield_entries_this_turn
            && self.damage_dealt_this_turn == other.damage_dealt_this_turn
            && self.assassin_or_commander_dealt_combat_damage_this_turn
                == other.assassin_or_commander_dealt_combat_damage_this_turn
            && self.creature_types_dealt_combat_damage_this_turn
                == other.creature_types_dealt_combat_damage_this_turn
            && self.pending_spell_cost_reductions == other.pending_spell_cost_reductions
            && self.pending_next_spell_modifiers == other.pending_next_spell_modifiers
            && self.pending_etb_counters == other.pending_etb_counters
            && self.modal_modes_chosen_this_turn == other.modal_modes_chosen_this_turn
            && self.modal_modes_chosen_this_game == other.modal_modes_chosen_this_game
            && self.revealed_cards == other.revealed_cards
            && self.public_revealed_cards == other.public_revealed_cards
            && self.pending_continuation == other.pending_continuation
            && self.pending_repeat_iteration == other.pending_repeat_iteration
            && self.pending_change_zone_iteration == other.pending_change_zone_iteration
            // `devour_eligible_snapshot` is INTENTIONALLY excluded from PartialEq.
            // It is a TRANSIENT mid-resolution carrier (CR 614.12a/13a): `Some`
            // only while a Devour co-entry is in flight, `None` everywhere else.
            // It is NOT necessarily recoverable from the other compared fields
            // during its Some-window — at the as-enters sacrifice prompt the
            // Devour PutCounter sub-ability has not run, so for a vanilla devourer
            // `pending_etb_counters` does not contain the entering ObjectId; the
            // snapshot can be live across this boundary. Exclusion is safe anyway:
            // PartialEq is used for AI-search position dedup, and the only effect
            // of ignoring this field is that two otherwise-identical transient
            // mid-resolution states may dedup together — an AI-search collapse,
            // never a game-rule error (the rule-bearing constraint is the live
            // snapshot itself, which IS preserved on serde round-trip: the field
            // is serialized whenever `Some` — see `skip_serializing_if` above —
            // so a mid-prompt save/resume keeps the constraint intact).
            && self.pending_copy_token_resolution == other.pending_copy_token_resolution
            && self.pending_coin_flip == other.pending_coin_flip
            && self.pending_repeat_until == other.pending_repeat_until
            && self.pending_choose_one_of == other.pending_choose_one_of
            && self.pending_counter_moves == other.pending_counter_moves
            && self.pending_batch_deliveries == other.pending_batch_deliveries
            && self.pending_counter_additions == other.pending_counter_additions
            && self.may_trigger_auto_choices == other.may_trigger_auto_choices
            && self.pending_begin_game_abilities == other.pending_begin_game_abilities
            && self.resolving_begin_game_abilities == other.resolving_begin_game_abilities
            && self.pending_cast == other.pending_cast
            && self.last_named_choice == other.last_named_choice
            && self.last_revealed_ids == other.last_revealed_ids
            && self.private_look_ids == other.private_look_ids
            && self.private_look_player == other.private_look_player
            && self.last_zone_changed_ids == other.last_zone_changed_ids
            && self.last_vote_ballots == other.last_vote_ballots
            && self.player_actions_this_way == other.player_actions_this_way
            && self.last_effect_count == other.last_effect_count
            && self.last_effect_counts_by_player == other.last_effect_counts_by_player
            && self.current_trigger_match_count == other.current_trigger_match_count
            && self.pending_optional_trigger_match_count
                == other.pending_optional_trigger_match_count
            && self.exiled_from_hand_this_resolution == other.exiled_from_hand_this_resolution
            && self.lki_cache == other.lki_cache
            && self.city_blessing == other.city_blessing
    }
}

impl Eq for GameState {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PostReplacementContinuation, QuantityExpr,
        ResolvedAbility, TargetFilter,
    };

    /// CR 104.4b: the loop fingerprint must distinguish object tap state — else a
    /// tap/untap loop's two phases would be indistinguishable. (A false negative
    /// is safe; this guards detection quality, not correctness.)
    #[test]
    fn loop_fingerprint_reflects_object_tap_state() {
        let mut state = GameState::new_two_player(7);
        let object = GameObject::new(
            ObjectId(500),
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        state.objects.insert(ObjectId(500), object);
        state.battlefield.push_back(ObjectId(500));

        let untapped = state.loop_fingerprint();
        if let Some(object) = state.objects.get_mut(&ObjectId(500)) {
            object.tapped = true;
        }
        assert_ne!(
            untapped,
            state.loop_fingerprint(),
            "tapping an object must change the loop fingerprint"
        );
    }

    /// CR 104.4b: any randomness consumed advances the RNG stream position, which
    /// the fingerprint includes — so a loop containing a shuffle/coin flip never
    /// collides and is correctly NOT drawn.
    #[test]
    fn loop_fingerprint_reflects_rng_consumption() {
        let mut state = GameState::new_two_player(7);
        let before = state.loop_fingerprint();
        state.rng.set_word_pos(4096);
        assert_ne!(
            before,
            state.loop_fingerprint(),
            "advancing the RNG stream must change the loop fingerprint"
        );
    }

    /// CR 104.4b confirmation: two states reached at different times (advancing
    /// the volatile counters PartialEq compares) but otherwise identical must
    /// confirm as equal — else a real loop could never be confirmed and drawn.
    #[test]
    fn loop_states_equal_ignores_volatile_counters() {
        let base = GameState::new_two_player(7);
        let mut later = base.clone();
        later.state_revision = 99;
        later.next_timestamp = 42;
        later.next_object_id = base.next_object_id + 5;

        assert!(
            loop_states_equal(&base.normalize_for_loop(), &later.normalize_for_loop()),
            "states differing only in volatile counters must confirm as a repeat"
        );
    }

    /// CR 104.4b confirmation must NOT treat two states as equal when an object's
    /// mutable content differs — guards the `objects.len()`-only `PartialEq` gap
    /// that would otherwise permit a wrongful draw.
    #[test]
    fn loop_states_equal_detects_object_content_difference() {
        let mut a = GameState::new_two_player(7);
        let object = GameObject::new(
            ObjectId(500),
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        a.objects.insert(ObjectId(500), object);
        a.battlefield.push_back(ObjectId(500));
        let mut b = a.clone();
        if let Some(object) = b.objects.get_mut(&ObjectId(500)) {
            object.tapped = true;
        }

        assert!(
            loop_states_equal(&a.normalize_for_loop(), &a.normalize_for_loop()),
            "identical states must confirm as a repeat"
        );
        assert!(
            !loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "a tapped-vs-untapped object difference must NOT confirm (no wrongful draw)"
        );
    }

    #[test]
    fn default_creates_two_player_game() {
        let state = GameState::default();
        assert_eq!(state.players.len(), 2);
    }

    #[test]
    fn accepts_freeform_card_selection_for_scry_surveil_and_dig() {
        // CR 701.22a / CR 701.25a: scry and surveil keep-on-top are freeform.
        assert!(WaitingFor::ScryChoice {
            player: PlayerId(0),
            cards: vec![],
        }
        .accepts_freeform_card_selection());
        assert!(WaitingFor::SurveilChoice {
            player: PlayerId(0),
            cards: vec![],
        }
        .accepts_freeform_card_selection());
        // Dig: legal selections (count-constrained / reordered) also can't be
        // enumerated; apply() validates them structurally.
        assert!(WaitingFor::DigChoice {
            player: PlayerId(0),
            library_owner: PlayerId(0),
            cards: vec![],
            keep_count: 1,
            up_to: false,
            selectable_cards: vec![],
            kept_destination: None,
            rest_destination: None,
            source_id: None,
            enter_tapped: false,
        }
        .accepts_freeform_card_selection());

        // A sampling of other selection/decision states must NOT be freeform —
        // they remain validated by candidate enumeration.
        assert!(!WaitingFor::Priority {
            player: PlayerId(0),
        }
        .accepts_freeform_card_selection());
        assert!(!WaitingFor::RevealChoice {
            player: PlayerId(0),
            cards: vec![],
            filter: TargetFilter::Any,
            optional: false,
            decline_runs_continuation: false,
        }
        .accepts_freeform_card_selection());
        assert!(!WaitingFor::ManifestDreadChoice {
            player: PlayerId(0),
            cards: vec![],
        }
        .accepts_freeform_card_selection());
    }

    #[test]
    fn accepts_freeform_combat_damage_assignment_for_assign_combat_damage() {
        // CR 510.1c/d + CR 702.19b: legal damage divisions (e.g. keeping excess
        // on the blocker rather than trampling through) cannot be enumerated as
        // candidate actions, so the multiplayer gate must bypass exact-match and
        // let apply() validate the submitted division.
        assert!(WaitingFor::AssignCombatDamage {
            player: PlayerId(0),
            attacker_id: ObjectId(1),
            total_damage: 3,
            blockers: vec![],
            assignment_modes: vec![],
            trample: None,
            defending_player: PlayerId(1),
            attack_target: crate::game::combat::AttackTarget::Player(PlayerId(1)),
            pw_loyalty: None,
            pw_controller: None,
        }
        .accepts_freeform_combat_damage_assignment());

        // Other states must NOT be freeform for combat damage — they remain
        // validated by candidate enumeration.
        assert!(!WaitingFor::Priority {
            player: PlayerId(0),
        }
        .accepts_freeform_combat_damage_assignment());
        assert!(!WaitingFor::ScryChoice {
            player: PlayerId(0),
            cards: vec![],
        }
        .accepts_freeform_combat_damage_assignment());
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
                base_cost: None,
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: vec![],
                casting_variant: CastingVariant::Normal,
                cast_timing_permission: None,
                distribute: None,
                origin_zone: Zone::Hand,
                additional_cost_flow: None,
                deferred_required_additional_cost: None,
                additional_cost_queue: Vec::new(),
                additional_cost_source: SpellCostSource::Other,
                deferred_modal_choice: None,
                deferred_target_selection: false,
                chosen_modes: Vec::new(),
                additional_cost_decided: false,
                declared_kickers_to_pay: Vec::new(),
                declined_kickers: Vec::new(),
                convoked_creatures: Vec::new(),
                cancel_restore_prepared_source: None,
                payment_mode: CastPaymentMode::Auto,
                assist_state: AssistState::NotOffered,
            })
        }

        // Use push to avoid large stack frame from vec! macro expansion.
        let mut variants: Vec<Box<WaitingFor>> = Vec::new();
        variants.push(Box::new(WaitingFor::Priority {
            player: PlayerId(0),
        }));
        variants.push(Box::new(WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 1,
            }],
            free_first_mulligan: false,
        }));
        variants.push(Box::new(WaitingFor::MulliganBottomCards {
            pending: vec![MulliganBottomEntry {
                player: PlayerId(0),
                count: 2,
            }],
        }));
        variants.push(Box::new(WaitingFor::OpeningHandBottomCards {
            pending: vec![MulliganBottomEntry {
                player: PlayerId(0),
                count: 1,
            }],
            reason: OpeningHandBottomReason::TinyLeadersMultiCommander,
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
            block_requirements: HashMap::new(),
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
            library_owner: PlayerId(0),
            cards: vec![ObjectId(1)],
            keep_count: 1,
            up_to: false,
            selectable_cards: vec![ObjectId(1)],
            kept_destination: None,
            rest_destination: None,
            source_id: None,
            enter_tapped: false,
        }));
        variants.push(Box::new(WaitingFor::SurveilChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
        }));
        variants.push(Box::new(WaitingFor::ChooseFromZoneChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1)],
            count: 1,
            up_to: false,
            constraint: None,
            source_id: ObjectId(100),
        }));
        variants.push(Box::new(WaitingFor::ChooseOneOfBranch {
            player: PlayerId(0),
            controller: PlayerId(0),
            source_id: ObjectId(100),
            branches: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
            branch_descriptions: vec!["Draw a card.".to_string()],
            parent_targets: vec![],
            context: crate::types::ability::SpellContext::default(),
            remaining_players: vec![],
        }));
        variants.push(Box::new(WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(ObjectId(1))],
                optional: false,
            }],
            mode_labels: Vec::new(),
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
            cost: AdditionalCost::Optional {
                cost: crate::types::ability::AbilityCost::Blight { count: 1 },
                repeatability: crate::types::ability::AdditionalCostRepeatability::Once,
            },
            times_kicked: 0,
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
        variants.push(Box::new(WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::Discard,
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell {
                spell: dummy_pending(),
            },
        }));
        variants.push(Box::new(WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Hand,
            },
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell {
                spell: dummy_pending(),
            },
        }));
        variants.push(Box::new(WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::ExileFromZone {
                zone: ExileCostSourceZone::Graveyard,
            },
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell {
                spell: dummy_pending(),
            },
        }));
        variants.push(Box::new(WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::Sacrifice,
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 1,
            resume: CostResume::Spell {
                spell: dummy_pending(),
            },
        }));
        variants.push(Box::new(WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::ReturnToHand,
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell {
                spell: dummy_pending(),
            },
        }));
        variants.push(Box::new(WaitingFor::BlightChoice {
            player: PlayerId(0),
            counters: 1,
            creatures: vec![ObjectId(1)],
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::HarmonizeTapChoice {
            player: PlayerId(0),
            eligible_creatures: vec![ObjectId(1)],
            pending_cast: dummy_pending(),
        }));
        variants.push(Box::new(WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::Behold {
                action: BeholdCostAction::ChooseOrReveal,
            },
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::Spell {
                spell: dummy_pending(),
            },
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
            min_count: 0,
            up_to: false,
            source_id: ObjectId(100),
            effect_kind: crate::types::ability::EffectKind::Sacrifice,
            zone: Zone::Battlefield,
            destination: None,
            enter_tapped: EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: None,
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            count_param: 0,
        }));
        variants.push(Box::new(WaitingFor::DefilerPayment {
            player: PlayerId(0),
            life_cost: 2,
            mana_reduction: ManaCost::zero(),
            pending_cast: dummy_pending(),
        }));
        assert_eq!(variants.len(), 33);
    }

    #[test]
    fn pending_cast_ref_is_single_source_of_truth_for_inline_variants() {
        // CR 601.2f: Every WaitingFor variant that carries `pending_cast: Box<PendingCast>`
        // inline must expose it via `pending_cast_ref`, which in turn drives
        // `has_pending_cast`. This test guards the mapping for ChooseXValue (the
        // variant whose earlier omission caused the Unsummon cast/cancel loop
        // regression and produced the ChooseXValue-fallback latent bug). Remaining
        // inline variants share the same match arm; the destructuring pattern
        // makes coverage compiler-visible.
        let pending = Box::new(PendingCast {
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
            base_cost: None,
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: vec![],
            casting_variant: CastingVariant::Normal,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
        });
        let choose_x = WaitingFor::ChooseXValue {
            player: PlayerId(0),
            min: 0,
            max: 5,
            pending_cast: pending,
            convoke_mode: None,
        };
        assert!(choose_x.pending_cast_ref().is_some());
        assert!(choose_x.has_pending_cast());
    }

    #[test]
    fn has_pending_cast_covers_mana_payment_exception() {
        // ManaPayment externalizes its PendingCast into GameState::pending_cast
        // for multiplayer visibility filtering. has_pending_cast must account
        // for this variant even though pending_cast_ref returns None.
        let mana_payment = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };
        assert!(mana_payment.pending_cast_ref().is_none());
        assert!(mana_payment.has_pending_cast());
    }

    #[test]
    fn has_pending_cast_excludes_non_cast_states() {
        // Priority is never a cast state.
        let priority = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert!(!priority.has_pending_cast());
        assert!(priority.pending_cast_ref().is_none());

        // A PayCost with a ManaAbility resume carries PendingManaAbility, not
        // PendingCast. A mana ability activated inside a spell cast still routes
        // the cast through the outer ManaPayment state, so excluding this
        // variant here does not lose mid-cast tracking.
        let tap_mana = WaitingFor::PayCost {
            player: PlayerId(0),
            kind: PayCostKind::TapCreatures,
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::ManaAbility {
                mana_ability: Box::new(PendingManaAbility {
                    player: PlayerId(0),
                    source_id: ObjectId(1),
                    ability_index: 0,
                    color_override: None,
                    resume: ManaAbilityResume::Priority,
                    chosen_tappers: Vec::new(),
                    chosen_discards: Vec::new(),
                    chosen_mana_payment: None,
                    chosen_counter_count: None,
                    chosen_exiled: Vec::new(),
                    chosen_sacrificed_battlefield: Vec::new(),
                    cost_paid_object: None,
                    batch_siblings: Vec::new(),
                }),
            },
        };
        assert!(!tap_mana.has_pending_cast());
        assert!(tap_mana.pending_cast_ref().is_none());
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
                actual_mana_spent: 0,
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
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
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
            mode_labels: Vec::new(),
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
            min_count: 0,
            up_to: true,
            source_id: ObjectId(10),
            effect_kind: crate::types::ability::EffectKind::ChangeZone,
            zone: Zone::Hand,
            destination: Some(Zone::Battlefield),
            enter_tapped: EtbTapState::Tapped,
            enter_transformed: false,
            enters_under_player: Some(PlayerId(0)),
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            face_down_profile: None,
            count_param: 0,
        };
        let json = serde_json::to_string(&wf).unwrap();
        let deserialized: WaitingFor = serde_json::from_str(&json).unwrap();
        assert_eq!(wf, deserialized);
        assert!(json.contains("\"EffectZoneChoice\""));
    }

    // ---------------------------------------------------------------------
    // CR 110.2a: serde coverage for the resolved-once runtime carriers
    // (`PendingChangeZoneIteration` and `WaitingFor::EffectZoneChoice`).
    // ---------------------------------------------------------------------

    #[test]
    fn pending_change_zone_iteration_modern_shape_roundtrips() {
        let original = PendingChangeZoneIteration {
            remaining: vec![],
            source_id: ObjectId(7),
            controller: PlayerId(0),
            origin: None,
            destination: Zone::Battlefield,
            enter_transformed: false,
            enter_tapped: EtbTapState::Unspecified,
            enters_under_player: Some(PlayerId(1)),
            enters_attacking: false,
            enter_with_counters: vec![],
            duration: None,
            track_exiled_by_source: false,
            moved_count: None,
            // CR 708.2a + CR 708.3: the face-down profile must survive the
            // pause/resume serde round-trip so a paused face-down return
            // (Yedora) resumes face down with the same characteristics.
            face_down_profile: Some(crate::types::ability::FaceDownProfile {
                power: None,
                toughness: None,
                body: crate::types::ability::FaceDownBody::Noncreature,
                extra_core_types: vec![crate::types::card_type::CoreType::Land],
                subtypes: vec!["Forest".to_string()],
                ward: None,
            }),
            effect_kind: crate::types::ability::EffectKind::ChangeZone,
        };
        let json = serde_json::to_string(&original).expect("serialize");
        // Modern shape must be emitted, NOT the legacy bool field.
        assert!(
            json.contains("\"enters_under_player\""),
            "expected modern field name in: {json}"
        );
        let parsed: PendingChangeZoneIteration = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(parsed.enters_under_player, Some(PlayerId(1)));
        assert_eq!(
            parsed.face_down_profile, original.face_down_profile,
            "face_down_profile must survive the pause/resume round-trip"
        );
        assert_eq!(parsed, original);
    }

    #[test]
    fn effect_zone_choice_modern_shape_roundtrips_with_player_id() {
        let wf = WaitingFor::EffectZoneChoice {
            player: PlayerId(0),
            cards: vec![],
            count: 1,
            min_count: 0,
            up_to: false,
            source_id: ObjectId(10),
            effect_kind: crate::types::ability::EffectKind::ChangeZone,
            zone: Zone::Hand,
            destination: Some(Zone::Battlefield),
            enter_tapped: EtbTapState::Unspecified,
            enter_transformed: false,
            enters_under_player: Some(PlayerId(1)),
            enters_attacking: false,
            owner_library: false,
            track_exiled_by_source: false,
            // CR 708.2a + CR 708.3: a face-down `ChangeZone` selection must keep
            // its profile across the `EffectZoneChoice` round-trip.
            face_down_profile: Some(crate::types::ability::FaceDownProfile {
                power: None,
                toughness: None,
                body: crate::types::ability::FaceDownBody::Noncreature,
                extra_core_types: vec![crate::types::card_type::CoreType::Land],
                subtypes: vec!["Forest".to_string()],
                ward: None,
            }),
            count_param: 0,
        };
        let json = serde_json::to_string(&wf).expect("serialize");
        // Modern shape must be emitted, NOT the legacy bool field.
        assert!(
            json.contains("\"enters_under_player\""),
            "expected modern field name in: {json}"
        );
        let parsed: WaitingFor = serde_json::from_str(&json).expect("roundtrip");
        assert_eq!(parsed, wf);
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
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 42,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
        };
        let json = serde_json::to_string(&trigger).unwrap();
        let deserialized: PendingTrigger = serde_json::from_str(&json).unwrap();
        assert_eq!(trigger, deserialized);
    }

    #[test]
    fn may_trigger_auto_choices_roundtrip_and_default_empty() {
        let empty = GameState::new_two_player(42);
        assert!(empty.may_trigger_auto_choices.is_empty());

        let mut state = GameState::new_two_player(42);
        let key = MayTriggerAutoChoiceKey {
            player: PlayerId(0),
            source_id: ObjectId(5),
            origin: MayTriggerOrigin::Printed { trigger_index: 1 },
        };
        state.set_may_trigger_auto_choice(key, AutoMayChoice::Accept);

        let serialized = serde_json::to_string(&state).unwrap();
        let mut deserialized: GameState = serde_json::from_str(&serialized).unwrap();
        deserialized.rng = ChaCha20Rng::seed_from_u64(deserialized.rng_seed);

        assert_eq!(
            deserialized.may_trigger_auto_choice(&key),
            Some(AutoMayChoice::Accept)
        );
        assert_eq!(state, deserialized);
    }

    #[test]
    fn game_state_with_pending_trigger_and_exile_links() {
        use crate::game::triggers::PendingTrigger;
        use crate::types::ability::{Effect, QuantityExpr, ResolvedAbility};

        let mut state = GameState::new_two_player(42);
        state.exile_links.push(ExileLink {
            exiled_id: ObjectId(10),
            source_id: ObjectId(5),
            kind: ExileLinkKind::UntilSourceLeaves {
                return_zone: Zone::Battlefield,
            },
        });
        state.pending_trigger = Some(PendingTrigger {
            source_id: ObjectId(5),
            controller: PlayerId(0),
            condition: None,
            ability: ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            ),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: None,
            trigger_event: None,
            modal: None,
            mode_abilities: vec![],
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
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

    /// 2026-05-09 audit M4 backward-compat: a JSON snapshot saved before the
    /// post-replacement-continuation slot fold (with the legacy
    /// `post_replacement_effect` field) deserializes cleanly and the legacy
    /// content lifts into the new unified slot once
    /// `migrate_post_replacement_continuation` runs (called from
    /// `finalize_public_state` at every deserialize boundary).
    #[test]
    fn legacy_post_replacement_effect_field_lifts_into_unified_slot() {
        // Build a baseline state, serialize it, then splice in the legacy
        // field name so the snapshot mirrors a pre-fold producer.
        let baseline = GameState::new_two_player(42);
        let mut snapshot: serde_json::Value = serde_json::to_value(&baseline).unwrap();
        let template = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: None,
            },
        );
        let template_json = serde_json::to_value(&template).unwrap();
        snapshot
            .as_object_mut()
            .unwrap()
            .insert("post_replacement_effect".to_string(), template_json);

        let serialized = serde_json::to_string(&snapshot).unwrap();
        let mut state: GameState = serde_json::from_str(&serialized).unwrap();
        // Pre-migration: legacy slot populated, unified slot empty.
        assert!(state.post_replacement_continuation.is_none());
        assert!(state.legacy_post_replacement_effect.is_some());

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation {
            Some(PostReplacementContinuation::Template(ref def)) => {
                assert_eq!(**def, template);
            }
            other => panic!("expected Template after migration, got {other:?}"),
        }
        assert!(state.legacy_post_replacement_effect.is_none());
    }

    /// 2026-05-09 audit M4 backward-compat (Resolved variant): a pre-fold
    /// snapshot with `post_replacement_resolved_effect` lifts to
    /// `PostReplacementContinuation::Resolved` after migration.
    #[test]
    fn legacy_post_replacement_resolved_effect_field_lifts_into_unified_slot() {
        let baseline = GameState::new_two_player(42);
        let mut snapshot: serde_json::Value = serde_json::to_value(&baseline).unwrap();
        let resolved = ResolvedAbility::new(
            Effect::LoseLife {
                amount: QuantityExpr::Fixed { value: 1 },
                target: Some(TargetFilter::Controller),
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let resolved_json = serde_json::to_value(&resolved).unwrap();
        snapshot.as_object_mut().unwrap().insert(
            "post_replacement_resolved_effect".to_string(),
            resolved_json,
        );

        let serialized = serde_json::to_string(&snapshot).unwrap();
        let mut state: GameState = serde_json::from_str(&serialized).unwrap();
        assert!(state.post_replacement_continuation.is_none());
        assert!(state.legacy_post_replacement_resolved_effect.is_some());

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation {
            Some(PostReplacementContinuation::Resolved(ref boxed)) => {
                assert_eq!(**boxed, resolved);
            }
            other => panic!("expected Resolved after migration, got {other:?}"),
        }
        assert!(state.legacy_post_replacement_resolved_effect.is_none());
    }

    /// CR 601.2a: A `SpellCastRecord` snapshot from an older serialized state
    /// (when `from_zone` was `Option<Zone>` and the default was `null`) must
    /// deserialize into a record whose `from_zone` is `Zone::Hand` — the
    /// dominant cast-from origin per CR 601.2a.
    #[test]
    fn spell_cast_record_legacy_null_from_zone_deserializes_to_hand() {
        let legacy_json = r#"{
            "core_types": ["Creature"],
            "supertypes": [],
            "subtypes": ["Bird"],
            "keywords": ["Flying"],
            "colors": ["Blue"],
            "mana_value": 3,
            "from_zone": null
        }"#;
        let record: SpellCastRecord = serde_json::from_str(legacy_json).unwrap();
        assert_eq!(record.from_zone, Zone::Hand);
    }

    /// CR 601.2a: A `SpellCastRecord` snapshot that omits `from_zone` entirely
    /// (e.g., a pre-migration snapshot serialized while the field still had
    /// `skip_serializing_if = "Option::is_none"`) must deserialize into
    /// `Zone::Hand` via the `serde(default = …)` hook.
    #[test]
    fn spell_cast_record_missing_from_zone_deserializes_to_hand() {
        let no_field_json = r#"{
            "core_types": ["Instant"],
            "supertypes": [],
            "subtypes": [],
            "keywords": [],
            "colors": [],
            "mana_value": 1
        }"#;
        let record: SpellCastRecord = serde_json::from_str(no_field_json).unwrap();
        assert_eq!(record.from_zone, Zone::Hand);
    }

    /// CR 601.2a: A snapshot with a real `from_zone` value (the modern non-Option
    /// encoding) must deserialize unchanged — the legacy adapter must not
    /// rewrite valid origin zones.
    #[test]
    fn spell_cast_record_explicit_from_zone_round_trips() {
        let original = SpellCastRecord {
            name: String::new(),
            core_types: vec![CoreType::Sorcery],
            supertypes: vec![],
            subtypes: vec![],
            keywords: vec![],
            colors: vec![],
            mana_value: 4,
            has_x_in_cost: false,
            from_zone: Zone::Graveyard,
            cast_variant: CastingVariant::Normal,
        };
        let json = serde_json::to_string(&original).unwrap();
        let round_tripped: SpellCastRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, original);
        assert_eq!(round_tripped.from_zone, Zone::Graveyard);
    }
}

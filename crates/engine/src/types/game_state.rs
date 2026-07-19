use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Arc;

use rand::SeedableRng;
use rand_chacha::ChaCha20Rng;
use serde::{Deserialize, Serialize};

use super::ability::{
    default_target_filter_permanent, AbilityCost, AbilityDefinition, AdditionalCost,
    AdditionalCostInstance, AdditionalCostInstancePayment, AttackSubject, BeholdCostAction,
    CastVariantPaid, CategoryChooserScope, ChoiceType, ChoiceValue, ChooseFromZoneConstraint,
    ChosenAttribute, CoinFlipResult, Comparator, ContinuousModification, ControlWindow,
    CopiableValues, CopyChooseScope, CopyScale, CostPaidObjectSnapshot, CounterCostSelection,
    DelayedTriggerCondition, Duration, EffectKind, FaceDownProfile, GameRestriction, KeywordAction,
    KickerVariant, LibraryPosition, ModalChoice, PermanentEntryMode, PileSource, QuantityExpr,
    ResolvedAbility, SearchDestinationSplit, SearchSelectionConstraint, StaticCondition,
    TapCreaturesAggregate, TargetFilter, TargetRef, ThisWayCause, TriggerCondition,
    TriggerDefinitionRef, TriggerEntry,
};
use super::attribution::ObjectAttribution;
use super::card::{CardFace, TokenImageRef};
use super::card_type::{CoreType, Supertype};
use super::counter::{counter_map_serde, CounterMatch, CounterType};
use super::events::{GameEvent, PlayerActionKind};
use super::format::FormatConfig;
use super::identifiers::{CardId, ObjectId, ObjectIncarnationRef, TrackedSetId};
use super::keywords::{Keyword, KeywordKind};
use super::mana::{ManaColor, ManaCost, ManaPipId, ManaType, ManaUnit, StepEndManaAction};
use super::match_config::{MatchConfig, MatchPhase, MatchScore};
use super::phase::{Phase, PhaseStop, TurnDirection};
use super::player::{Player, PlayerCounterKind, PlayerId};
use super::proposed_event::{
    AppliedReplacementKey, CopyTokenSpec, ProposedEvent, ReplacementId, TokenSpec,
};
use super::replacements::ReplacementEvent;
use super::zones::EtbTapState;
use super::zones::{ExileCostSourceZone, Zone};

use crate::analysis::resource::ResourceAxis;
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

/// Serde adapter for trigger occurrence ledgers. JSON object keys must be
/// strings, while a `TriggerDefinitionRef` is structured identity; encode the
/// map as an explicit entry list rather than flattening or guessing a key.
mod trigger_definition_ref_map {
    use super::*;

    pub fn serialize<S>(
        map: &HashMap<TriggerDefinitionRef, u32>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        map.iter().collect::<Vec<_>>().serialize(serializer)
    }

    pub fn deserialize<'de, D>(
        deserializer: D,
    ) -> Result<HashMap<TriggerDefinitionRef, u32>, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Vec::<(TriggerDefinitionRef, u32)>::deserialize(deserializer)
            .map(|entries| entries.into_iter().collect())
    }
}

/// Deserializes the object store and validates the one legacy trigger shape
/// that can be materialized without guessing: a complete ordered payload list
/// proven by the persisted printed base slots. Runtime copied/granted payloads
/// have no such proof and are rejected at the state restore boundary.
fn deserialize_objects_with_trigger_provenance<'de, D>(
    deserializer: D,
) -> Result<im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    let mut objects =
        im::HashMap::<ObjectId, GameObject, rustc_hash::FxBuildHasher>::deserialize(deserializer)?;
    for (_, object) in objects.iter_mut() {
        object
            .migrate_legacy_trigger_definitions()
            .map_err(serde::de::Error::custom)?;
    }
    Ok(objects)
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

/// CR 702.132a + CR 601.2h: Tracks the once-per-cast Assist offer and payment
/// lifecycle on a `PendingCast`. A typed enum (not a bool) keeps the selected
/// contribution distinct from the irreversible helper-payment boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum AssistState {
    /// The Assist offer has not been made for this cast.
    #[default]
    NotOffered,
    /// The offer was made and the caster declined (or contributed nothing).
    Offered,
    /// The caster chose `helper`, who will pay `generic` of the spell's generic
    /// mana. The caster's owed cost is reduced by `generic` now, but the helper
    /// has not yet begun paying; the cast remains cancellable.
    Committed { helper: PlayerId, generic: u32 },
    /// The helper has started paying its committed contribution. This survives
    /// a paused source-cost replacement so cancellation cannot strand a paid
    /// prefix or allow the helper contribution to be applied twice on resume.
    PaymentStarted { helper: PlayerId, generic: u32 },
    /// The committed helper contribution has been paid. This checkpoint is
    /// retained if the caster's later mana payment pauses on a replacement
    /// choice, so the helper is never charged again on resume.
    Paid { helper: PlayerId, generic: u32 },
}

/// CR 614.10 + CR 614.10a: Turn-scoped combat-phase skip lifecycle for a single
/// player. Backs `GameState::combat_phase_skip_next_turn` (False Peace / Empty
/// City Ruse: "skips all combat phases of their next turn").
///
/// CR 614.10a: stacked skips are independent — two resolved effects make the
/// player skip combat on their next *two* non-skipped turns ("one effect will be
/// satisfied in skipping the first occurrence, while the other will remain"). So
/// the model is a count, not a tri-state: `pending` is the number of armed skips
/// not yet bound to a turn, and `active` marks the player's current turn as a
/// bound skip turn. The axes are orthogonal — while a turn is `active`, further
/// `pending` skips wait for subsequent turns.
///
/// Lifecycle, advanced in `start_next_turn` for the player whose turn begins:
/// - a skip effect resolves -> `pending += 1`
/// - that player's previous bound turn ended -> `active = false` (skip satisfied)
/// - this turn isn't itself skipped and `pending > 0` -> `pending -= 1`, `active = true`
///
/// While `active`, a virtual replacement effect prevents every combat phase of
/// the bound turn (CR 614.10: "skip" == "instead of doing this, do nothing").
/// `Default` (`pending: 0`, `active: false`) is "no skip armed".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CombatPhaseSkipState {
    /// CR 614.10a: armed turn-scoped combat skips that have not yet bound to a
    /// turn; each waits past skipped turns for its own non-skipped turn.
    #[serde(default)]
    pub pending: u32,
    /// True while the player's current turn is a bound combat-skip turn; the
    /// replacement layer prevents every combat phase of that turn.
    #[serde(default)]
    pub active: bool,
}

/// CR 400.7: Snapshot of an object's characteristics at the time it left a public zone.
/// Used for event-context resolution when the object is no longer in its original zone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LKISnapshot {
    pub name: String,
    /// Display-only token catalog ref as it last existed in the public zone.
    /// Preserved so stack entries from dead token sources can render the exact
    /// token image without falling back to name-based lookup.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_image_ref: Option<TokenImageRef>,
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
    /// CR 110.5 + CR 110.5d: Tap status as it last existed on the battlefield.
    /// A permanent's tapped/untapped status is battlefield-only — once the object
    /// leaves a public zone it is neither tapped nor untapped, so a look-back rider
    /// ("Return target creature to its owner's hand. If it was tapped, ..." —
    /// Brackish Blunder) must read this captured value via `FilterProp::Tapped`
    /// (use_lki). `#[serde(default)]` ⇒ pre-existing saved states deserialize to
    /// `tapped = false`.
    #[serde(default)]
    pub tapped: bool,
    /// CR 701.60b + CR 608.2c: Suspected status as it last existed in the public
    /// zone. Suspected is a battlefield-only status reset on any zone change
    /// (`GameObject::is_suspected` clears when the object moves), so a cost-paid
    /// look-back ("the sacrificed creature was suspected" — Agency Coroner) must
    /// read this captured value via `FilterProp::Suspected` (LKI). The snapshot
    /// is taken at cost payment, before the sacrifice zone-change resets the flag.
    /// `#[serde(default)]` ⇒ pre-existing saved states deserialize to `false`.
    #[serde(default)]
    pub is_suspected: bool,
    /// CR 608.2h + CR 400.7: Attachments (Auras/Equipment) as they last existed on
    /// the battlefield. Attachment is a battlefield-only relationship — SBA unattaches
    /// everything the instant the host leaves (CR 704.5m/n) — so a source-referential
    /// intervening-if ("if this creature is enchanted" — Dreampod Druid; "if he's
    /// equipped" — Whiplash) re-checked at resolution (CR 603.4) has nothing live to
    /// read once its source is gone. CR 608.2h routes that question to LAST KNOWN
    /// INFORMATION, so the attachment set must be captured on battlefield exit like
    /// every other look-back characteristic here.
    ///
    /// Captured via [`capture_attachment_snapshot`](crate::game::zones::capture_attachment_snapshot),
    /// the same authority that fills `ZoneChangeRecord::attachments` — one snapshot
    /// shape, one capture site.
    ///
    /// `#[serde(default)]` ⇒ pre-existing saved states deserialize to an empty set,
    /// which is exactly the pre-change fail-closed behavior.
    #[serde(default)]
    pub attachments: Vec<AttachmentSnapshot>,
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
    /// CR 702.33d: Whether kicker was paid when this spell was cast. Captured at
    /// cast-time for per-turn spell-history filters ("first kicked spell each turn").
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub was_kicked: bool,
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
            was_kicked: false,
        }
    }
}

/// CR 601.2a + CR 702.27a: the cast-time snapshot the PR-7 Phase 4d-ii object-growth
/// detection hook replays. Captured at cast finalization (the single first-class point,
/// `finalize_cast_with_phyrexian_choices`), carried on the loop-detection clone, replayed
/// by the recast injector. NOT reconstructed at the hook seam — `SpellCastRecord` lacks
/// both the buyback-paid flag and the convoke shape. Every field is loop-INVARIANT across
/// a homogeneous recast (unit-variant `ConvokeMode` carries zero per-iteration data;
/// `CardId` is cross-incarnation-stable per CR 400.7), so the whole struct is COMPARED
/// (never excluded) in the object-growth cover gates — a heterogeneous recast (one whose
/// iterations alternate `uses_buyback` or `from_zone`) is caught and rejected (fail-closed).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecastContext {
    /// CR 400.7 card identity — re-found live in the castable zone each iteration (a
    /// fresh incarnation on every hand-return), never an `ObjectId` that churns.
    pub card_id: CardId,
    pub controller: PlayerId,
    /// CR 601.2a: the zone the recast is cast from (Hand — buyback returns the spell here).
    pub from_zone: Zone,
    /// CR 702.27a: the recast must re-pay buyback each iteration to sustain the loop.
    pub uses_buyback: BuybackUsage,
    /// CR 702.51a: the convoke mode the injector's pin re-binds live each iteration
    /// (`None` when the recast pays no convoke cost).
    pub convoke: Option<ConvokeMode>,
}

/// CR 702.27a: whether a homogeneous recast re-pays the buyback additional cost each iteration.
/// Typed (not `bool`) so the recast frame's cost shape is self-documenting where it is compared
/// (the object-growth cover gates) and consumed (the replay's `DecideOptionalCost` beat).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum BuybackUsage {
    Used,
    NotUsed,
}

impl BuybackUsage {
    /// CR 601.2f/702.27a: true when the recast re-pays buyback (drives the `DecideOptionalCost`
    /// beat during object-growth replay).
    pub const fn pays(self) -> bool {
        matches!(self, BuybackUsage::Used)
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
    /// Permanent that granted this modifier. Required for source-dependent spell
    /// filters such as `IsChosenCreatureType` (CR 607.2d).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<ObjectId>,
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
    pub trigger_definitions: Vec<TriggerEntry>,
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
    /// CR 601.2a: Cast origin as of the zone change — distinct from `from_zone`
    /// for objects put onto the battlefield without being cast (reanimate, etc.).
    #[serde(default)]
    pub cast_from_zone: Option<Zone>,
    /// CR 305.1: Land-play provenance as of the zone change.
    #[serde(default)]
    pub played_from_zone: Option<Zone>,
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
    /// CR 400.7: the entrant's incarnation captured AFTER its battlefield-entry
    /// bump, so a later leave + re-entry (same ObjectId, higher incarnation) is
    /// distinguishable from the original entrant at intervening-if recheck.
    /// `None` for non-battlefield destinations and for records built before the
    /// post-entry bump (filled in by `move_to_zone`).
    #[serde(default)]
    pub entered_incarnation: Option<u64>,
    /// CR 303.4b + CR 603.10a: The attachment target (player or object) as it
    /// existed immediately before the zone change. For Aura Curses attached to a
    /// player, this preserves the enchanted-player identity after
    /// `sever_battlefield_attachment_graph_on_exit` clears the live field. Used by
    /// the co-departed observer path so `ControllerRef::EnchantedPlayer` can
    /// resolve via LKI when the Curse leaves in the same simultaneous event as
    /// the watched creature.
    #[serde(default)]
    pub attached_to: Option<AttachTarget>,
    /// Per-turn monotonic index assigned when the zone change is recorded (CR
    /// 400.7). Distinguishes repeated identical `(object, from, to)` transitions
    /// within the same turn for batched trigger replay guards (issue #3866).
    #[serde(default)]
    pub turn_zone_change_index: usize,
    /// CR 701.60b + CR 608.2c: Suspected status as of the zone change. Suspected
    /// is a battlefield-only status reset on any zone change, so a cost-paid
    /// look-back ("the sacrificed creature was suspected" — Agency Coroner)
    /// evaluated via the LKI snapshot synthesized in
    /// `matches_target_filter_on_lki_snapshot` must read this captured value.
    /// `#[serde(default)]` ⇒ pre-existing saved states deserialize to `false`.
    #[serde(default)]
    pub is_suspected: bool,
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
    /// CR 506.5 + CR 603.10a: Whether the object was the sole attacker when it
    /// left combat. Captured at zone-exit because combat membership is cleared
    /// by CR 506.4 before look-back ("leaves the battlefield") trigger
    /// conditions are evaluated, so live combat can no longer answer it.
    #[serde(default)]
    pub attacking_alone: bool,
    /// CR 506.5 + CR 603.10a: Whether the object was the sole blocker when it
    /// left combat. Captured at zone-exit for the same look-back reason as
    /// `attacking_alone`.
    #[serde(default)]
    pub blocking_alone: bool,
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
            cast_from_zone: None,
            played_from_zone: None,
            to_zone: to,
            attachments: Vec::new(),
            linked_exile_snapshot: Vec::new(),
            is_token: false,
            combat_status: ZoneChangeCombatStatus::default(),
            co_departed: Vec::new(),
            attached_to: None,
            entered_incarnation: None,
            turn_zone_change_index: 0,
            is_suspected: false,
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
    /// CR 403.3 + CR 603.10: keyword abilities the object had at the moment it
    /// entered (entry snapshot per CR 403.3), so look-back conditions ("a creature
    /// with flying entered this turn") evaluate via the CR 603.10 last-known-state
    /// against entry-time characteristics (like the existing core_types/colors
    /// snapshots). KNOWN LIMITATION: this captures the object's keywords at record
    /// time, which is BEFORE the layer system re-evaluates (layers are only marked
    /// dirty, not recomputed, at zone-change). Printed flyers and keyword-counter /
    /// intrinsic flyers are counted; a creature granted flying ONLY by a Layer-6
    /// continuous effect (e.g. an anthem) at the moment it enters is NOT counted.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub keywords: Vec<Keyword>,
    pub controller: PlayerId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoMayChoice {
    Accept,
    Decline,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum MayTriggerOrigin {
    Definition {
        definition_ref: TriggerDefinitionRef,
    },
    /// Compatibility-only wire shape for saves created before trigger
    /// occurrences were serialized. Live collection always emits
    /// [`Self::Definition`]; consumers must not derive new authority from this
    /// display index.
    Printed {
        trigger_index: usize,
    },
    Keyword {
        keyword: KeywordKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MayTriggerAutoChoiceKey {
    pub player: PlayerId,
    pub source_id: ObjectId,
    pub origin: MayTriggerOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MayTriggerAutoChoiceRecord {
    pub key: MayTriggerAutoChoiceKey,
    pub choice: AutoMayChoice,
}

/// CR 117.3d: The scope of a player's pre-committed decision to pass priority
/// while a class of triggered ability is on the stack ("yield").
///
/// `ThisObject` yields only for triggers from one specific object incarnation
/// (CR 400.7 — a re-entered permanent is a new object and no longer matches).
/// `AllCopies` yields for every trigger from any object sharing the source's
/// card identity, so it keeps matching after a token source ceases to exist
/// (CR 704.5d) and matches newly created copies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum YieldScope {
    ThisObject,
    AllCopies,
}

/// CR 117.3d + CR 400.7: The resolved identity a `PriorityYield` matches against,
/// latched at the moment the yield is registered.
///
/// `ThisObject` binds a concrete object incarnation: a matching stack entry must
/// carry the same `source_id` and `source_incarnation`. Here `incarnation` is an
/// `Option<u64>`, so an `incarnation` of `None` matches a trigger whose
/// `source_incarnation` is *also* `None` — synthetic/delayed game-rule triggers
/// that never latched an incarnation can now be yielded (Option == Option
/// compare). `AllCopies` binds a `CardId`: any trigger whose `source_card_id`
/// equals it matches, regardless of which object (or whether the object still
/// exists, CR 704.5d).
///
/// Both variants carry an optional `trigger_description`, the per-trigger
/// discriminator the stack entry already exposes
/// (`StackEntryKind::TriggeredAbility.description`). A `trigger_description` of
/// `None` is a **wildcard** that matches ANY entry description (the coarse,
/// source-level yield used by legacy persisted yields and pre-upgrade saves),
/// while `Some(desc)` gives per-trigger precision so one source's distinct
/// triggers can be yielded independently.
///
// serde: legacy bare-u64 incarnation loads as Some (serde maps only null→None),
// so old persisted `{"incarnation":26}` still deserializes and matches; an
// absent `trigger_description` defaults to None (the wildcard).
// `Ord` (all fields already `Ord`) gives `DecisionGroupKey`'s canonical
// sorted `sources` multiset a total order (PR-7 B1/B2).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum YieldTarget {
    ThisObject {
        source_id: ObjectId,
        incarnation: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_description: Option<String>,
    },
    AllCopies {
        card_id: CardId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_description: Option<String>,
    },
}

/// CR 117.3d: A player's standing decision to pass priority automatically
/// whenever a triggered ability matching `target` is the top of the stack.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriorityYield {
    pub player: PlayerId,
    pub target: YieldTarget,
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
    /// CR 400.7: Incarnation of an object target when it was dealt damage.
    /// `None` preserves compatibility with legacy records and player targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub target_incarnation: Option<u64>,
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
    /// CR 120.10: Excess damage beyond lethal for creatures/planeswalkers/battles.
    /// Zero for players and for damage that does not overkill. Used by the
    /// "was dealt excess damage this turn" intervening-if condition class.
    #[serde(default)]
    pub excess: u32,
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
            target_incarnation: None,
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
            excess: 0,
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
    /// CR 303.4f: Attach host captured before SearchChoice overwrites parent targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_attach_host: Option<AttachTarget>,
    /// CR 608.2: The resolving ability's trigger-event context, snapshotted at
    /// stash time so TargetFilter::TriggeringPlayer and its siblings resolve
    /// correctly when drain_pending_continuation resumes this chain —
    /// stack::resolve_top unconditionally clears the live context once the
    /// stack entry that started this resolution has left the stack (CR 603.7c
    /// cleanup), regardless of whether the ability's OWN resolution actually
    /// finished or merely paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_context: Option<ResolvingTriggerContext>,
}

impl PendingContinuation {
    /// Construct a continuation with no parent-kind emission. Used for chains
    /// whose per-node `EffectResolved` events are the full observable story
    /// (targeted damage continuations, Learn rummage, Bolster, Clash, etc.).
    pub fn new(chain: Box<ResolvedAbility>, state: &GameState) -> Self {
        Self {
            chain,
            parent_kind: None,
            search_attach_host: None,
            trigger_context: ResolvingTriggerContext::capture(state),
        }
    }

    /// Construct a continuation whose drain must re-emit the outer effect's
    /// `EffectResolved { kind, source_id }` once the chain completes. The
    /// `source_id` used for emission is read from `chain.source_id` at drain
    /// time, matching the non-pause path.
    pub fn with_parent_kind(
        chain: Box<ResolvedAbility>,
        parent_kind: EffectKind,
        state: &GameState,
    ) -> Self {
        Self {
            chain,
            parent_kind: Some(parent_kind),
            search_attach_host: None,
            trigger_context: ResolvingTriggerContext::capture(state),
        }
    }
}

/// CR 608.2c + CR 109.5: Resume state for a `repeat_for` iteration loop paused
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

/// CR 603.12a + CR 608.2c: A "you may pay {cost} up to N times. When you do,
/// [reflexive]" process paused for one of its per-iteration optional-payment
/// decisions (Hawkeye, Master Marksman — "Trick Arrows"). Unlike a generic
/// `repeat_for` loop, each iteration's "you may" is offered SEPARATELY, the
/// number of successful payments (K, accumulated in
/// `GameState::optional_cost_payments_this_resolution`) sizes the reflexive
/// modal (CR 700.2d), and the reflexive triggers EXACTLY ONCE for K >= 1
/// (CR 603.12a) — never per payment. The resolution-time mana payment is
/// synchronous (auto-tap, never pauses), so the only async boundaries are the
/// per-iteration `OptionalEffectChoice` and the final `AbilityModeChoice`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingRepeatedOptionalPayment {
    /// The PayCost-only unit prompted (and, on accept, paid) this iteration —
    /// `repeat_for`/`sub_ability` cleared so resolving it neither re-enters the
    /// driver nor re-resolves the reflexive.
    pub payment_unit: Box<crate::types::ability::ResolvedAbility>,
    /// The reflexive sub-ability (the modal) resolved exactly once after the
    /// loop, iff at least one payment succeeded (CR 603.12a).
    pub reflexive: Box<crate::types::ability::ResolvedAbility>,
    /// Number of further per-iteration payment prompts after the one currently
    /// outstanding (the "up to N" budget minus the iterations already offered).
    pub remaining: u32,
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
    /// CR 705.2: The player who flips (and therefore wins/loses) the coin — the
    /// already-resolved `Effect::FlipCoin::flipper`. The kept Krark's-Thumb flip's
    /// `CoinFlipped` is recorded for this player, not `controller`. Defaults to
    /// the controller for in-flight states serialized before this field existed.
    #[serde(default)]
    pub flipper: PlayerId,
    pub targets: Vec<TargetRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub win_effect: Option<Box<AbilityDefinition>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lose_effect: Option<Box<AbilityDefinition>>,
    pub kind: PendingCoinFlipKind,
}

/// CR 705.2: The controller-relevant result of the most recent coin flip
/// performed during the current resolution. Written by the flip authority
/// (`flip_through_replacement` / `resume_after_keep`), read by
/// `AbilityCondition::CoinFlipOutcome` when a `RepeatContinuation::WhileCondition`
/// loop re-evaluates its predicate ("if you lose the flip, repeat this process").
/// Carries the `flipper` (CR 705.2: only the player who flips wins/loses) so the
/// gate stays controller-relative even in a hypothetical multi-flipper process.
/// The stored `result` reuses the same `CoinFlipResult` vocabulary that
/// `AbilityCondition::CoinFlipOutcome` matches against, so the written value and
/// the read predicate can never drift into a `bool`-vs-enum mismatch.
/// Resolution-scoped like `last_revealed_ids`: overwrite-on-produce, cleared at
/// the authoritative resolution-lifetime boundary (top-level `resolve_ability_chain`
/// entry) and again at each `WhileCondition` iteration start so the gate reads
/// only the current iteration's flip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionCoinFlip {
    pub flipper: PlayerId,
    pub result: CoinFlipResult,
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
    /// Conditional entry-counter specs carried across a pause so each remaining
    /// object can be re-evaluated per-object on resume (Winter Soldier Hero
    /// rider through `EffectZoneChoice`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conditional_enter_with_counters: Vec<(
        crate::types::ability::TargetFilter,
        crate::types::counter::CounterType,
        crate::types::ability::QuantityExpr,
    )>,
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
    /// CR 401.4 + CR 701.24a: Library placement override carried across a
    /// replacement-ordering pause so Endurance-style "on the bottom of their
    /// library" still suppresses auto-shuffle on resume.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_placement: Option<crate::types::ability::LibraryPosition>,
    /// CR 614.12: gates the `enter_tapped`/`enters_attacking` riders on the
    /// moved object's type, carried across a replacement-ordering / as-enters
    /// pause so a paused-then-resumed gated move (Summoner's Grimoire) still
    /// applies the riders only to a matching object on resume. `None` = apply
    /// unconditionally. Mirrors the `enter_tapped`/`face_down_profile`
    /// carry-through pattern on this same struct.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enters_modified_if: Option<crate::types::ability::TargetFilter>,
    /// CR 303.4f: Pre-resolved Aura host carried across a paused ChangeZone loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enter_attached_to: Option<AttachTarget>,
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

/// CR 616.1: Which pausing primitive of an `EachPlayerCopyChosen` per-player
/// step is currently mid-flight, so the drain resumes at the right point
/// (neither re-reading a stale token nor double-placing counters).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum CopyChosenStage {
    /// The inner `CopyTokenOf` paused on a replacement choice; on resume, the
    /// created token(s) are now recorded in `last_created_token_ids` and the
    /// counter step must be driven.
    AwaitingCopy,
    /// The copy completed and the +1/+1 counter placement was initiated and
    /// paused (2+ counter-modifying replacements needed ordering); on resume,
    /// the counters have finished — advance to the next player.
    AwaitingCounters,
}

/// CR 101.4: One player's completed ordered choice for
/// [`Effect::EachPlayerCopyChosen`]. `chosen[0]` is copied and `chosen[1]`, when
/// present, scales the copy.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyChosenSelection {
    pub player: PlayerId,
    pub chosen: Vec<ObjectId>,
}

/// CR 101.4 + CR 616.1: Resume state for a single player's copy+counter step of
/// `EachPlayerCopyChosen` that paused on a CR 616.1 replacement choice. `stage`
/// disambiguates which primitive paused; `chosen[0]` was copied and `chosen[1]`
/// (if present) scales the copy; `remaining_choices` + the effect params
/// continue the already-collected action walk after this player's step completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingEachPlayerCopyChosen {
    pub stage: CopyChosenStage,
    pub player: PlayerId,
    /// `[0]` copied, `[1]` (optional) scales the copy.
    pub chosen: Vec<ObjectId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub remaining_choices: Vec<CopyChosenSelection>,
    // Effect params for the remainder of the walk.
    pub choose_filter: TargetFilter,
    pub min: u32,
    pub max: u32,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub copy_modifications: Vec<ContinuousModification>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scale: Option<CopyScale>,
    /// CR 102.1 + CR 103.1: whose battlefield the paused chooser drew from, so a
    /// mid-flight save/reload reconstructs the correct eligibility controller.
    #[serde(default)]
    pub choose_scope: CopyChooseScope,
    pub source_id: ObjectId,
    pub source_controller: PlayerId,
    /// APNAP-ordered scoped player set (for a mid-choice save/reload).
    #[serde(default)]
    pub scoped_players: Vec<PlayerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trigger_event: Option<crate::types::events::GameEvent>,
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
    /// CR 614.5 + CR 616.1f: replacement effects already applied to the event
    /// that produced this queued branch choice.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub replacement_applied: HashSet<AppliedReplacementKey>,
    pub remaining_players: Vec<PlayerId>,
}

/// CR 101.4 + CR 608.2c: Per-player `ChooseFromZone { zone_owner: EachPlayer }`
/// iteration state. A single chooser (the spell's controller) picks one card
/// from EACH player's zone in APNAP order; this stashes the players not yet
/// prompted while the current player's `WaitingFor::ChooseFromZoneChoice` is
/// outstanding. Created when the first player's choice is parked, drained after
/// each pick accumulates into the resolution chain's tracked set, and disposed
/// once every player has been prompted — at which point the parked
/// `pending_continuation` (e.g. "put those cards onto the battlefield") runs.
/// Building block for Breach the Multiverse.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingPerPlayerZoneChoice {
    /// The `Effect::ChooseFromZone` ability whose per-player body repeats. Its
    /// `zone`/`filter`/`count`/`chooser` describe each player's prompt.
    pub ability: Box<ResolvedAbility>,
    /// Players not yet prompted, in APNAP order.
    pub remaining_players: Vec<PlayerId>,
    /// CR 603.7 + CR 608.2c: Whether a pick from THIS per-player iteration has
    /// already started its fresh chosen-card tracked set. The first non-empty
    /// pick must START a fresh set (so the chosen cards do NOT merge with an
    /// earlier producer's set — e.g. Breach the Multiverse's preceding mill,
    /// whose milled cards would otherwise reanimate alongside the chosen ones);
    /// every later pick EXTENDS that fresh set. `false` until the first pick is
    /// published, then `true` for the remainder of the iteration.
    #[serde(default)]
    pub accumulated: bool,
}

/// CR 101.4: If players make choices for one instruction, they choose in
/// APNAP order before the simultaneous action happens.
/// CR 701.21a: To sacrifice a permanent, its controller moves it from the
/// battlefield to its owner's graveyard.
/// Per-player sacrifice choices for one simultaneous instruction such as
/// "each player sacrifices a creature."
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingPlayerScopeSacrificeChoice {
    /// The scoped sacrifice ability template. The current chooser is rebound
    /// onto a clone before each prompt is built.
    pub ability: Box<ResolvedAbility>,
    /// Players not yet prompted, in APNAP order.
    pub remaining_players: Vec<PlayerId>,
    /// Already collected choices, paired with the player who will sacrifice
    /// those permanents once all choices are known.
    pub selections: Vec<(PlayerId, Vec<ObjectId>)>,
    /// CR 101.4 + CR 701.21a + CR 616.1: Terminal bookkeeping for the one
    /// simultaneous sacrifice action. It survives every replacement-choice
    /// pause, including a pause on the final announced permanent when
    /// `selections` is empty, so the batch is finalized before any later
    /// continuation can run.
    #[serde(default)]
    pub completion: PendingPlayerScopeSacrificeCompletion,
}

/// CR 101.4 + CR 701.21a: Accumulated terminal state for a simultaneous
/// player-scope sacrifice action that may span one or more CR 616.1 choices.
/// The individual choice queue can become empty while the final proposed
/// sacrifice is still awaiting replacement resolution, so this state must be
/// carried independently of the remaining selections.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingPlayerScopeSacrificeCompletion {
    /// Every permanent announced for this sacrifice action. The ids scope
    /// event-derived bookkeeping to this batch when a replacement resumes.
    #[serde(default)]
    pub announced: Vec<ObjectId>,
    /// Announced permanents whose sacrifice event has actually completed.
    #[serde(default)]
    pub sacrificed: Vec<ObjectId>,
    /// Announced permanents that changed zones while being sacrificed.
    #[serde(default)]
    pub zone_changed: Vec<ObjectId>,
    /// Per-turn `ZoneChangeRecord` identities for the announced permanents that
    /// actually departed. These records are retained across a replacement
    /// pause so terminal co-departure stamping updates both the resumed event
    /// stream and the authoritative per-turn LKI ledger.
    #[serde(default)]
    pub departed_zone_change_indices: Vec<usize>,
    /// CR 603.10a + CR 616.1: Zone-change and sacrifice events emitted before
    /// an inner replacement choice are held until the entire simultaneous
    /// sacrifice instruction has completed. The resumed delivery appends its
    /// events to this span, then terminal completion stamps one co-departure
    /// group and exposes the complete event batch to trigger collection.
    #[serde(default)]
    pub deferred_events: Vec<GameEvent>,
    /// True once this sacrifice action has crossed a replacement-choice
    /// boundary. The resumed action's event buffer already contains the event
    /// just delivered by `engine_replacement`, even when no earlier event was
    /// available to defer, so terminal stamping must cover the full resumed
    /// buffer rather than only events produced by the tail drain.
    #[serde(default)]
    pub spans_replacement_pause: bool,
    /// The resolution event emitted once the complete sacrifice batch settles.
    /// Most callers inherit `EffectKind::Sacrifice` from their template; callers
    /// that reuse this queue for a broader resolution instruction override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effect_kind: Option<EffectKind>,
    /// A per-permanent event that must follow every completed sacrifice, even
    /// when delivery crossed a replacement-choice action boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub follow_up: Option<PendingPlayerScopeSacrificeFollowUp>,
    /// Completed follow-up events, retained so a resumed event buffer can never
    /// emit the same per-sacrifice event twice.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub followed_up_sacrifices: Vec<ObjectId>,
    /// `EffectZoneChoice` starts a fresh selected-object set. Its replacement
    /// pause must defer that publication until every selected sacrifice has
    /// settled, so continuation consumers see the full actual set.
    #[serde(default)]
    pub publish_fresh_tracked_set: bool,
    /// The interactive selection path normally snapshots the completed
    /// sacrifice for a parent-referential continuation. Retain that suffix work
    /// on the same typed batch when the selected sacrifice pauses.
    #[serde(default)]
    pub propagate_parent_context: bool,
}

/// CR 702.110b + CR 701.21a + CR 616.1: A per-sacrifice consequence carried
/// by the replacement-safe simultaneous-sacrifice queue.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PendingPlayerScopeSacrificeFollowUp {
    /// Emit the exploit event only after the chosen creature's sacrifice has
    /// actually completed, including after a graveyard-move replacement choice.
    Exploit { exploiter: ObjectId },
}

/// CR 101.4 + CR 701.23i: APNAP state for a self-library search instruction
/// whose selected cards are delivered only after every searching player has
/// made their private choice. The original spell's controller remains on
/// `ability`; a per-player clone is rebound only while calculating that
/// player's local candidates and local-X quantity.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct PendingScopedLibrarySearch {
    /// Search + parent-target delivery template, with the outer player scope
    /// removed by the resolution driver.
    pub ability: Box<ResolvedAbility>,
    pub phase: ScopedLibrarySearchPhase,
    /// The once-after-all-searches tail (Natural Balance's searched-this-way
    /// shuffle). It is carried through a replacement-paused batch delivery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_scope: Option<Box<ResolvedAbility>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ScopedLibrarySearchPhase {
    CollectAcceptance {
        remaining_players: Vec<PlayerId>,
        accepted_players: Vec<PlayerId>,
        acceptance_authorities: Vec<(PlayerId, ActiveSearchDecisionAuthority)>,
        current_player: Option<PlayerId>,
    },
    CollectSelections {
        prepared_choices: Vec<PreparedScopedLibrarySearchChoice>,
        next_selection_index: usize,
        current_player: Option<PlayerId>,
        selections: Vec<(PlayerId, Vec<ObjectIncarnationRef>)>,
        frozen_dispositions: Vec<FrozenScopedSearchFoundDisposition>,
        #[serde(default)]
        pending_reveals: Vec<(PlayerId, Vec<ObjectIncarnationRef>)>,
    },
    Delivering {
        search_keys: Vec<PlayerId>,
    },
}

#[derive(Deserialize)]
struct PendingScopedLibrarySearchWire {
    ability: Box<ResolvedAbility>,
    phase: ScopedLibrarySearchPhase,
    #[serde(default)]
    after_scope: Option<Box<ResolvedAbility>>,
}

impl<'de> Deserialize<'de> for PendingScopedLibrarySearch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = PendingScopedLibrarySearchWire::deserialize(deserializer)?;
        let valid = match &wire.phase {
            ScopedLibrarySearchPhase::CollectAcceptance {
                remaining_players,
                accepted_players,
                acceptance_authorities,
                current_player,
            } => {
                let mut participants = remaining_players.clone();
                participants.extend(accepted_players.iter().copied());
                participants.extend(current_player.iter().copied());
                let unique: BTreeSet<_> = participants.iter().copied().collect();
                let authority_players: BTreeSet<_> = acceptance_authorities
                    .iter()
                    .map(|(player, _)| *player)
                    .collect();
                unique.len() == participants.len()
                    && authority_players.len() == acceptance_authorities.len()
                    && unique == authority_players
            }
            ScopedLibrarySearchPhase::CollectSelections {
                prepared_choices,
                next_selection_index,
                current_player,
                selections,
                frozen_dispositions,
                pending_reveals,
            } => {
                let prepared: BTreeSet<_> = prepared_choices
                    .iter()
                    .map(|choice| choice.player)
                    .collect();
                let selected: BTreeSet<_> = selections.iter().map(|(player, _)| *player).collect();
                let frozen: HashSet<_> = frozen_dispositions
                    .iter()
                    .map(|entry| (entry.searcher, entry.identity))
                    .collect();
                let reveal_players: BTreeSet<_> =
                    pending_reveals.iter().map(|(player, _)| *player).collect();
                let candidates_by_player: HashMap<_, HashSet<_>> = prepared_choices
                    .iter()
                    .map(|choice| {
                        (
                            choice.player,
                            choice.candidates.iter().copied().collect::<HashSet<_>>(),
                        )
                    })
                    .collect();
                let candidate_count: usize = prepared_choices
                    .iter()
                    .map(|choice| choice.candidates.len())
                    .sum();
                let selections_by_player: HashMap<_, HashSet<_>> = selections
                    .iter()
                    .map(|(player, identities)| {
                        (*player, identities.iter().copied().collect::<HashSet<_>>())
                    })
                    .collect();
                let announced_by_player: HashMap<_, HashSet<_>> = prepared_choices
                    .iter()
                    .filter_map(|choice| {
                        choice.announced_selection.as_ref().map(|announced| {
                            (
                                choice.player,
                                announced.iter().copied().collect::<HashSet<_>>(),
                            )
                        })
                    })
                    .collect();
                let mut original_dispositions_by_player =
                    HashMap::<PlayerId, HashSet<ObjectIncarnationRef>>::new();
                for frozen in frozen_dispositions.iter().filter(|frozen| {
                    matches!(
                        &frozen.disposition,
                        crate::types::proposed_event::SearchFoundDisposition::Original
                    )
                }) {
                    original_dispositions_by_player
                        .entry(frozen.searcher)
                        .or_default()
                        .insert(frozen.identity);
                }
                let selections_valid = selections.iter().all(|(player, identities)| {
                    let survivors = identities.iter().copied().collect::<HashSet<_>>();
                    survivors.len() == identities.len()
                        && announced_by_player.get(player).is_some_and(|announced| {
                            survivors
                                .iter()
                                .all(|identity| announced.contains(identity))
                        })
                        && original_dispositions_by_player
                            .get(player)
                            .map_or(survivors.is_empty(), |originals| originals == &survivors)
                });
                let frozen_valid = frozen_dispositions.iter().all(|entry| {
                    let was_announced_by_searcher = announced_by_player
                        .get(&entry.searcher)
                        .is_some_and(|announced| announced.contains(&entry.identity));
                    let attribution_is_active_or_complete = selected.contains(&entry.searcher)
                        || *current_player == Some(entry.searcher);
                    was_announced_by_searcher && attribution_is_active_or_complete
                });
                let reveals_valid = pending_reveals.iter().all(|(player, identities)| {
                    let reveal_enabled = prepared_choices
                        .iter()
                        .find(|choice| choice.player == *player)
                        .is_some_and(|choice| choice.reveal);
                    reveal_enabled
                        && identities.iter().copied().collect::<HashSet<_>>().len()
                            == identities.len()
                        && selections_by_player.get(player).is_some_and(|selected| {
                            identities
                                .iter()
                                .all(|identity| selected.contains(identity))
                        })
                });
                let announced_sets_valid = prepared_choices.iter().all(|choice| {
                    let completed = selected.contains(&choice.player);
                    let current = *current_player == Some(choice.player);
                    let permits_partial = choice.up_to
                        || choice.allows_partial_find
                        || choice.constraint.permits_partial_find();
                    match (choice.offered_count, choice.announced_selection.as_ref()) {
                        (None, None) => !completed && !current,
                        (Some(offered_count), None) => {
                            current && !completed && offered_count <= choice.candidates.len()
                        }
                        (Some(offered_count), Some(announced)) => {
                            let announced_set = announced
                                .iter()
                                .copied()
                                .collect::<HashSet<ObjectIncarnationRef>>();
                            let prompt_count = choice.count.min(offered_count);
                            (completed || current)
                                && offered_count <= choice.candidates.len()
                                && announced_set.len() == announced.len()
                                && candidates_by_player.get(&choice.player).is_some_and(
                                    |candidates| {
                                        announced_set
                                            .iter()
                                            .all(|identity| candidates.contains(identity))
                                    },
                                )
                                && if permits_partial {
                                    announced.len() <= prompt_count
                                } else {
                                    announced.len() == prompt_count
                                }
                        }
                        (None, Some(_)) => false,
                    }
                });
                prepared.len() == prepared_choices.len()
                    && candidates_by_player
                        .values()
                        .map(HashSet::len)
                        .sum::<usize>()
                        == candidate_count
                    && selected.len() == selections.len()
                    && frozen.len() == frozen_dispositions.len()
                    && reveal_players.len() == pending_reveals.len()
                    && selections_valid
                    && frozen_valid
                    && reveals_valid
                    && announced_sets_valid
                    && *next_selection_index <= prepared_choices.len()
                    && current_player.is_none_or(|player| {
                        *next_selection_index > 0
                            && prepared_choices[*next_selection_index - 1].player == player
                            && !selected.contains(&player)
                    })
            }
            ScopedLibrarySearchPhase::Delivering { search_keys } => {
                search_keys.iter().copied().collect::<BTreeSet<_>>().len() == search_keys.len()
            }
        };
        if !valid {
            return Err(serde::de::Error::custom(
                "invalid scoped library search phase invariants",
            ));
        }
        Ok(Self {
            ability: wire.ability,
            phase: wire.phase,
            after_scope: wire.after_scope,
        })
    }
}

impl PendingScopedLibrarySearch {
    pub fn current_player(&self) -> Option<PlayerId> {
        match &self.phase {
            ScopedLibrarySearchPhase::CollectAcceptance { current_player, .. }
            | ScopedLibrarySearchPhase::CollectSelections { current_player, .. } => *current_player,
            ScopedLibrarySearchPhase::Delivering { .. } => None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreparedScopedLibrarySearchChoice {
    pub player: PlayerId,
    pub library_owner: Option<PlayerId>,
    pub candidates: Vec<ObjectIncarnationRef>,
    /// Number of still-live exact candidates shown when this player's prompt
    /// was created. This may be smaller than `candidates.len()` after an
    /// earlier simultaneous participant changes a candidate's incarnation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub offered_count: Option<usize>,
    /// The exact cards this player announced before SearchFound replacement
    /// processing began. It remains present across a parked replacement prefix
    /// so serde validation can attribute both Original and Modified outcomes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub announced_selection: Option<Vec<ObjectIncarnationRef>>,
    #[serde(default = "default_prepared_search_filter")]
    pub filter: TargetFilter,
    pub count: usize,
    pub reveal: bool,
    pub up_to: bool,
    pub allows_partial_find: bool,
    pub constraint: SearchSelectionConstraint,
}

fn default_prepared_search_filter() -> TargetFilter {
    TargetFilter::Any
}

/// One exact selected object and its terminal SearchFound replacement outcome.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenScopedSearchFoundDisposition {
    pub searcher: PlayerId,
    pub identity: ObjectIncarnationRef,
    pub disposition: crate::types::proposed_event::SearchFoundDisposition,
}

/// The owner/zone/exact-incarnation facts learned during one effective hidden-zone
/// search. Construction is validated so serialized provenance cannot claim that a
/// card from one player's hidden zone was learned while searching another player.
/// CR 400.7: Exact incarnations prevent search knowledge from carrying across a
/// zone change to the new object that card becomes.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ActiveLibrarySearch {
    searcher: PlayerId,
    searched_zone_owner: PlayerId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    effective_library_owner: Option<PlayerId>,
    learned_audience: Vec<PlayerId>,
    looked_at: Vec<(PlayerId, Zone, ObjectIncarnationRef)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ActiveLibrarySearchInvariantError {
    TupleOwnerMismatch,
    EffectiveLibraryOwnerMismatch,
    LibraryTupleWithoutEffectiveLibrary,
}

impl std::fmt::Display for ActiveLibrarySearchInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::TupleOwnerMismatch => "looked-at tuple owner differs from searched-zone owner",
            Self::EffectiveLibraryOwnerMismatch => {
                "effective library owner differs from searched-zone owner"
            }
            Self::LibraryTupleWithoutEffectiveLibrary => {
                "library tuple exists without an effective library owner"
            }
        })
    }
}

impl std::error::Error for ActiveLibrarySearchInvariantError {}

impl ActiveLibrarySearch {
    pub fn try_new(
        searcher: PlayerId,
        searched_zone_owner: PlayerId,
        effective_library_owner: Option<PlayerId>,
        learned_audience: Vec<PlayerId>,
        looked_at: Vec<(PlayerId, Zone, ObjectIncarnationRef)>,
    ) -> Result<Self, ActiveLibrarySearchInvariantError> {
        if looked_at
            .iter()
            .any(|(owner, _, _)| *owner != searched_zone_owner)
        {
            return Err(ActiveLibrarySearchInvariantError::TupleOwnerMismatch);
        }
        if effective_library_owner.is_some_and(|owner| owner != searched_zone_owner) {
            return Err(ActiveLibrarySearchInvariantError::EffectiveLibraryOwnerMismatch);
        }
        if effective_library_owner.is_none()
            && looked_at.iter().any(|(_, zone, _)| *zone == Zone::Library)
        {
            return Err(ActiveLibrarySearchInvariantError::LibraryTupleWithoutEffectiveLibrary);
        }
        Ok(Self {
            searcher,
            searched_zone_owner,
            effective_library_owner,
            learned_audience,
            looked_at,
        })
    }

    pub fn searcher(&self) -> PlayerId {
        self.searcher
    }
    pub fn searched_zone_owner(&self) -> PlayerId {
        self.searched_zone_owner
    }
    pub fn effective_library_owner(&self) -> Option<PlayerId> {
        self.effective_library_owner
    }
    pub fn learned_audience(&self) -> &[PlayerId] {
        &self.learned_audience
    }
    pub fn looked_at(&self) -> &[(PlayerId, Zone, ObjectIncarnationRef)] {
        &self.looked_at
    }
    pub(crate) fn remove_from_audience(&mut self, player: PlayerId) {
        self.learned_audience.retain(|viewer| *viewer != player);
    }
}

#[derive(Deserialize)]
struct ActiveLibrarySearchWire {
    searcher: PlayerId,
    searched_zone_owner: PlayerId,
    #[serde(default)]
    effective_library_owner: Option<PlayerId>,
    learned_audience: Vec<PlayerId>,
    looked_at: Vec<(PlayerId, Zone, ObjectIncarnationRef)>,
}

impl<'de> Deserialize<'de> for ActiveLibrarySearch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = ActiveLibrarySearchWire::deserialize(deserializer)?;
        Self::try_new(
            wire.searcher,
            wire.searched_zone_owner,
            wire.effective_library_owner,
            wire.learned_audience,
            wire.looked_at,
        )
        .map_err(serde::de::Error::custom)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ActiveSearchDecisionAuthority {
    LatchedController { controller: PlayerId },
    SearcherFallback,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
/// CR 723.5: While one player controls another, the controller makes the
/// controlled player's choices and decisions.
pub struct ActiveSearchDecisionControl {
    pub searcher: PlayerId,
    /// Owner of the zones this search instruction examines. This protocol
    /// provenance exists even when no hidden-zone tuples are exposed.
    pub searched_zone_owner: PlayerId,
    pub authority: ActiveSearchDecisionAuthority,
}

macro_rules! validating_search_map {
    ($name:ident, $value:ty) => {
        #[derive(Clone, Debug, Default, PartialEq, Eq, Serialize)]
        #[serde(transparent)]
        pub struct $name(BTreeMap<PlayerId, $value>);

        impl $name {
            pub fn is_empty(&self) -> bool {
                self.0.is_empty()
            }
            pub fn get(&self, player: &PlayerId) -> Option<&$value> {
                self.0.get(player)
            }
            pub fn get_mut(&mut self, player: &PlayerId) -> Option<&mut $value> {
                self.0.get_mut(player)
            }
            pub fn remove(&mut self, player: &PlayerId) -> Option<$value> {
                self.0.remove(player)
            }
            pub fn iter(&self) -> impl Iterator<Item = (&PlayerId, &$value)> {
                self.0.iter()
            }
            pub fn retain(&mut self, mut f: impl FnMut(&PlayerId, &mut $value) -> bool) {
                self.0.retain(|k, v| f(k, v));
            }
        }
    };
}

validating_search_map!(ActiveLibrarySearches, ActiveLibrarySearch);
validating_search_map!(ActiveSearchDecisionControls, ActiveSearchDecisionControl);

impl ActiveLibrarySearches {
    pub fn insert(&mut self, record: ActiveLibrarySearch) -> Option<ActiveLibrarySearch> {
        self.0.insert(record.searcher(), record)
    }
}

impl ActiveSearchDecisionControls {
    pub fn insert(
        &mut self,
        record: ActiveSearchDecisionControl,
    ) -> Option<ActiveSearchDecisionControl> {
        self.0.insert(record.searcher, record)
    }
}

macro_rules! deserialize_validating_search_map {
    ($name:ident, $value:ty, $searcher:expr) => {
        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
            where
                D: serde::Deserializer<'de>,
            {
                struct Visitor;
                impl<'de> serde::de::Visitor<'de> for Visitor {
                    type Value = $name;
                    fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                        f.write_str("a searcher-keyed map")
                    }
                    fn visit_map<M>(self, mut access: M) -> Result<Self::Value, M::Error>
                    where
                        M: serde::de::MapAccess<'de>,
                    {
                        let mut values = BTreeMap::new();
                        while let Some((key, value)) = access.next_entry::<PlayerId, $value>()? {
                            let searcher: PlayerId = $searcher(&value);
                            if key != searcher {
                                return Err(serde::de::Error::custom(
                                    "search map key differs from embedded searcher",
                                ));
                            }
                            if values.insert(key, value).is_some() {
                                return Err(serde::de::Error::custom("duplicate searcher key"));
                            }
                        }
                        Ok($name(values))
                    }
                }
                deserializer.deserialize_map(Visitor)
            }
        }
    };
}

deserialize_validating_search_map!(
    ActiveLibrarySearches,
    ActiveLibrarySearch,
    |value: &ActiveLibrarySearch| value.searcher()
);
deserialize_validating_search_map!(
    ActiveSearchDecisionControls,
    ActiveSearchDecisionControl,
    |value: &ActiveSearchDecisionControl| value.searcher
);

/// CR 701.23e: Whether cards surviving a SearchFound replacement batch are
/// publicly revealed by the containing search instruction. The enum remains
/// boolean on the wire so persisted in-flight searches from before the typed
/// migration continue to round-trip unchanged.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(from = "bool", into = "bool")]
pub enum SearchFoundVisibility {
    #[default]
    Private,
    Public,
}

impl SearchFoundVisibility {
    pub fn is_public(self) -> bool {
        matches!(self, Self::Public)
    }
}

impl From<bool> for SearchFoundVisibility {
    fn from(reveal: bool) -> Self {
        if reveal {
            Self::Public
        } else {
            Self::Private
        }
    }
}

impl From<SearchFoundVisibility> for bool {
    fn from(visibility: SearchFoundVisibility) -> Self {
        visibility.is_public()
    }
}

/// CR 616.1 + CR 701.23a: A per-card found-event batch parked while the
/// affected card's owner orders multiple applicable replacement effects. The
/// current event itself lives in `pending_replacement`; this record preserves
/// the already-processed survivors and the exact unprocessed suffix so resume
/// never rescans earlier cards.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingSearchFoundBatch {
    pub searcher: PlayerId,
    /// CR 701.23a: Owner of the library component actually searched. Bound
    /// after search prohibitions remove impossible zones; never reconstructed
    /// from an individual selected card's current zone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_owner: Option<PlayerId>,
    pub remaining: Vec<ObjectIncarnationRef>,
    pub survivors: Vec<ObjectIncarnationRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current: Option<ObjectIncarnationRef>,
    pub continuation: PendingSearchFoundContinuation,
    #[serde(default, rename = "reveal")]
    pub visibility: SearchFoundVisibility,
}

/// The mutually exclusive continuation protocols available after every
/// SearchFound event in a batch reaches its terminal disposition. Encoding the
/// protocol as an enum prevents a malformed `scoped = true, split = Some(_)`
/// state from being serialized or resumed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum PendingSearchFoundContinuation {
    Standard {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        split: Option<crate::types::ability::SearchDestinationSplit>,
    },
    Scoped,
}

/// CR 608.2c + CR 105.1 / CR 205.2a: Per-category-member
/// `Effect::ForEachCategoryExile` iteration paused by the current member's
/// interactive choice. Mirrors [`PendingPerPlayerZoneChoice`], but the
/// iteration unit is a fixed-category member (a color or card type) rather than
/// a player: each `remaining_member_filters` entry is the `TargetFilter`
/// restricting the shared pool to cards of that member ("a card of that color/
/// type"). Each pick accumulates into the resolution chain's tracked object set
/// so a downstream "from among them" / "put the rest …" reads exactly the cards
/// exiled across all members.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingPerCategoryZoneChoice {
    /// The `Effect::ForEachCategoryExile` ability whose per-member body repeats.
    pub ability: Box<ResolvedAbility>,
    /// CR 608.2c: The full revealed/exiled pool snapshot, captured once at the
    /// start of the iteration. Each member filters THIS pool (minus cards
    /// already exiled by an earlier member) — it must not read the mutating
    /// chain tracked set, which the drain rebinds to the exiled cards.
    pub pool: Vec<ObjectId>,
    /// Per-member candidate filters not yet prompted, in category member order
    /// (WUBRG for colors, CR 205.2a order for card types).
    pub remaining_member_filters: Vec<crate::types::ability::TargetFilter>,
}

/// CR 701.38d + CR 608.2c: Stores the remaining voters whose per-ballot
/// interactive body has not yet been resolved. Created when the first
/// ballot's ChooseFromZone parks WaitingFor::ChooseFromZoneChoice; drained
/// after each choice resolves until all voters are processed.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PendingVoteBallotIteration {
    /// The ability template to instantiate for each remaining voter.
    pub ability_template: Box<AbilityDefinition>,
    /// Voters whose ballots have not yet been processed (in APNAP order).
    pub remaining_voters: Vec<PlayerId>,
    /// The source object that initiated the vote.
    pub source_id: ObjectId,
    /// The controller of the vote spell/ability.
    pub controller: PlayerId,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CounterMoveChoice {
    pub destination_id: ObjectId,
    pub counter_type: CounterType,
    pub count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CounterCostChoice {
    pub object_id: ObjectId,
    pub counter_type: CounterType,
    pub count: u32,
}

/// CR 107.1c + CR 608.2d: One per-type entry of a resolution-time
/// "remove any number of counters" selection (Rhys, the Evermore / Tetravus).
/// Unlike [`CounterCostChoice`], there is no `object_id`: the removal source is
/// the single object fixed by the effect (the ability's target or `SelfRef`),
/// so the client only chooses which counter types and how many of each to shed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub struct CounterRemoveChoice {
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

/// CR 107.1c + CR 608.2d + CR 608.2h: The not-yet-applied tail of a resolved
/// "remove any number of counters" selection. Mirrors [`PendingCounterMoveQueue`]:
/// drained one `(counter_type, count)` at a time by
/// `effects::counters::drain_pending_counter_removals`, which re-parks the queue
/// when a per-removal replacement surfaces a `ReplacementChoice` mid-batch. When
/// the queue empties, `total` is stamped into `last_effect_count` so a downstream
/// "create that many" / "add that much" rider (Tetravus, storage lands) reading
/// `QuantityRef::EventContextAmount` picks up the count removed.
///
/// Serialized (like `pending_counter_moves`) so a mid-batch re-park survives the
/// server→client→server state round-trip a `ReplacementChoice` requires; the
/// `skip_serializing_if` on the field keeps it off the wire when `None`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingCounterRemovalQueue {
    /// Remaining per-type removals to apply to `source_id`.
    pub remaining: Vec<(CounterType, u32)>,
    /// The object counters are removed from (the effect's single source).
    pub source_id: ObjectId,
    /// Effect kind for the terminating `EffectResolved` event.
    pub effect_kind: EffectKind,
    /// Ability source object for the terminating `EffectResolved` event.
    pub source_ability_id: ObjectId,
    /// Total counters requested across all entries; stamped into
    /// `last_effect_count` when the queue empties.
    pub total: u32,
}

/// CR 603.10a + CR 616.1: The not-yet-delivered tail of a simultaneous
/// zone-move batch, parked when a per-object `Moved` replacement surfaces a
/// replacement choice mid-batch (e.g. two simultaneously-applicable
/// graveyard→exile redirects — Rest in Peace + Leyline of the Void — racing on
/// the same object). Drained by `zone_pipeline::drain_pending_batch_deliveries`
/// from the replacement-choice resume path after the chosen event delivers; the
/// drain re-parks when the next object surfaces its own choice.
///
/// Shared by every batch flow that delivers one simultaneous set through the
/// pipeline, including heterogeneous destinations and causes. Serializes
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
    /// across the pause boundary. This and the other uniform fields are a
    /// backward-compatible projection for old saves; `requests` is authoritative
    /// for newly parked actions.
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
    /// CR 614.5: replacement definitions already applied to the event whose
    /// physical-card delivery is being resumed. Meld result redirects reuse
    /// this set for each component move so the redirect cannot apply again to
    /// either modified event.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub replacement_applied: HashSet<AppliedReplacementKey>,
    /// Exact heterogeneous undelivered suffix. New saves use this authority;
    /// the uniform fields above remain as a compatibility projection for older
    /// serialized states.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub requests: Vec<PendingBatchZoneMoveRequest>,
    /// Every object announced in the original simultaneous action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attempted: Vec<ObjectId>,
    /// Index where this action's authoritative per-turn zone-change records
    /// begin. Terminal completion stamps the entire range once.
    #[serde(default)]
    pub zone_change_record_start: usize,
    /// Events produced by already-delivered members. They remain hidden until
    /// the full action settles so one co-departure/LKI batch is observable.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_events: Vec<GameEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PendingBatchZoneChangeCause {
    Effect {
        source: ObjectId,
    },
    Cost {
        source: ObjectId,
    },
    SpellResolutionDefault,
    StateBasedAction,
    CommanderRuleReturn,
    Draw {
        #[serde(default, skip_serializing_if = "HashSet::is_empty")]
        seed_applied: HashSet<AppliedReplacementKey>,
    },
    CastingToStack {
        source: ObjectId,
    },
    PregameProcedure,
    PlayerLeftGame,
    MergedComponentRouting,
    DebugCommand,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingBatchZoneMoveRequest {
    pub object_id: ObjectId,
    pub destination: Zone,
    pub cause: PendingBatchZoneChangeCause,
    #[serde(default, skip_serializing_if = "EtbTapState::is_unspecified")]
    pub enter_tapped: EtbTapState,
    #[serde(default)]
    pub enter_transformed: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub controller_override: Option<PlayerId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub enter_with_counters: Vec<(CounterType, u32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub face_down_profile: Option<FaceDownProfile>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attach_to: Option<AttachTarget>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub library_placement: Option<LibraryPosition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exile_duration: Option<Duration>,
    #[serde(default)]
    pub exile_tracking: ZoneDeliveryExileTracking,
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub replacement_applied: HashSet<AppliedReplacementKey>,
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
pub struct CloakExileMember {
    /// The tracked pile member that was proposed to leave the battlefield.
    pub object_id: ObjectId,
    /// CR 603.10a: the host's pre-departure attachments. Delivery clears the
    /// live host list before LTB collection, so the cloak tail uses this list to
    /// sever the former attachments' back-edges after the batch settles.
    pub attachments: Vec<ObjectId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum BatchCompletion {
    /// CR 303.4g + CR 614.1 + CR 616.1: A return-as-Aura host had no legal
    /// object to enchant, and its proposed Battlefield→Graveyard move settled.
    /// The completion event waits for any replacement choice without carrying
    /// redundant move data.
    ReturnAsAuraNoTargetComplete { source_id: ObjectId },
    /// CR 701.44a + CR 701.44b + CR 614.1 + CR 616.1: A revealed explore land's
    /// proposed Library→Hand delivery settled. The explore completion event is
    /// deferred so its trigger boundary cannot precede a replacement choice.
    ExploreLandDeliveryComplete { explorer_id: ObjectId },
    /// CR 701.58a + CR 603.10a + CR 614.1 + CR 616.1: A tracked cloak pile's
    /// Battlefield→Exile batch settled. Keep its pre-departure attachment
    /// snapshots so the tail can detach every actual departure and manifest
    /// only cards that actually reached Exile.
    CloakExileDeliveryComplete {
        player: PlayerId,
        source_id: ObjectId,
        members: Vec<CloakExileMember>,
    },
    /// CR 614.1 + CR 616.1 + CR 611.2a: A `CastFromZone` current-zone-to-Exile
    /// batch settled. Keep the resolved ability and its two target partitions
    /// so the permission is recorded only after the exile delivery, while
    /// established in-place Hand/Graveyard/Exile grants remain unchanged.
    CastFromZoneExileDeliveryComplete {
        ability: Box<ResolvedAbility>,
        in_place_ids: Vec<ObjectId>,
        exile_delivery_ids: Vec<ObjectId>,
    },
    /// CR 702.85a + CR 614.1 + CR 616.1: One proposed cascade exile settled.
    /// Keep the current card, source threshold, controller, and already-exiled
    /// misses with the batch so a replacement choice resumes the loop correctly.
    CascadeExileLoopComplete {
        controller: PlayerId,
        source_id: ObjectId,
        source_mv: u32,
        exiled_misses: Vec<ObjectId>,
        current_card: ObjectId,
    },
    /// CR 702.85a + CR 616.1: A no-hit Cascade's randomized bottom batch has
    /// settled (including any redirected cards), so its completion events may
    /// fire exactly once.
    CascadeBottomComplete {
        controller: PlayerId,
        source_id: ObjectId,
        exiled_count: u32,
    },
    /// CR 701.57a + CR 616.1: A no-hit Discover's randomized bottom batch has
    /// settled, so the discover resolution completes exactly once.
    DiscoverBottomComplete { source_id: ObjectId },
    /// CR 701.57a + CR 616.1: A Discover that found a card has finished placing
    /// every miss after its cast/hand decision. Its resolution event waits for
    /// that batch.
    DiscoverPlacementComplete { source_id: ObjectId },
    /// CR 701.57a + CR 616.1: A declined Discover's miss batch settled; carry
    /// the printed hit-to-hand instruction through its own replacement-aware
    /// delivery before the Discover completion tail.
    DiscoverDeclined {
        player: PlayerId,
        hit_card: ObjectId,
        source_id: ObjectId,
    },
    /// CR 608.2g + CR 701.57a + CR 616.1: A Discover cast rejected at
    /// finalization waits for the miss batch before its replacement-aware
    /// hit-to-hand delivery and priority tail.
    ResolutionCastRejectedToHand {
        player: PlayerId,
        hit_card: ObjectId,
        source_id: ObjectId,
    },
    /// CR 701.57a + CR 616.1: The declined Discover's replacement-aware
    /// hit-to-hand delivery settled, so its resolution event and continuation
    /// may run exactly once.
    DiscoverDeclinedComplete {
        player: PlayerId,
        source_id: ObjectId,
    },
    /// CR 608.2g + CR 701.57a + CR 616.1: The rejected Discover hit's
    /// replacement-aware hand delivery settled, so its resolution event and
    /// priority restoration may run exactly once.
    ResolutionCastRejectionComplete {
        player: PlayerId,
        source_id: ObjectId,
    },
    /// CR 401.4 + CR 616.1: All requested library placements for one
    /// PutAtLibraryPosition instruction have settled. Source-linked exile
    /// bookkeeping and the resolution event occur only after the full batch.
    PutOnTopComplete {
        source_id: ObjectId,
        removed_exile_links: Vec<ObjectId>,
    },
    /// CR 614.1 + CR 616.1: A PutOnTopOrBottom object's Library delivery has settled, so
    /// its chained resolution tail may run exactly once.
    TopOrBottomComplete { player: PlayerId },
    /// CR 401.4 + CR 614.1 + CR 616.1 + CR 608.2c: The selected non-Library
    /// cards have settled through their replaceable Library deliveries. Reorder
    /// the cards that began in their libraries and then run the one resolution
    /// tail, preserving the original mixed-source placement order.
    EffectZonePutAtLibraryPositionComplete {
        /// Player whose pending continuation drains after the complete choice.
        player: PlayerId,
        /// Resolving effect source for the terminal `EffectResolved` event.
        source_id: ObjectId,
        /// Full selected order. It determines the printed placement order and
        /// remains the selected-object tracked set / effect count.
        chosen: Vec<ObjectId>,
        /// Selection members that were already in a library at delivery time.
        /// They are repositions, not replaceable zone-change requests.
        library_origin: Vec<ObjectId>,
        /// Normalized placement requested by the original instruction.
        library_position: LibraryPosition,
    },
    /// CR 614.1 + CR 616.1 + CR 608.2c: A Dig kept-card batch settled outside
    /// the battlefield. Its rest routing, tracked-set publication, and
    /// continuation drain must follow the delivery, while the published set and
    /// parent-target continuation set remain independently typed.
    DigKeptDeliveryComplete {
        player: PlayerId,
        source_id: Option<ObjectId>,
        rest_cards: Vec<ObjectId>,
        rest_destination: Zone,
        publish_tracked_set: Vec<ObjectId>,
        continuation_targets: Vec<ObjectId>,
    },
    /// CR 701.13a + CR 614.1 + CR 616.1: A per-category exile member has
    /// settled, so its tracked-set extension and next-member prompt can run.
    ForEachCategoryExileComplete {
        ability: Box<ResolvedAbility>,
        pool: Vec<ObjectId>,
        remaining_member_filters: Vec<crate::types::ability::TargetFilter>,
        chosen: Vec<ObjectId>,
    },
    /// CR 401.4 + CR 614.1 + CR 616.1 + CR 608.2c: A drawn-this-turn
    /// topdeck batch has settled, so its life-payment and resolution tail can
    /// run exactly once after the selected cards' ordered Library delivery.
    DrawnThisTurnTopdeckComplete {
        player: PlayerId,
        life_payment: u32,
        payment_count: usize,
        topdecked_count: usize,
        source_id: ObjectId,
    },
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
    /// CR 614.1 + CR 616.1 + CR 701.20b: A reveal-until / dig kept-card
    /// delivery paused before its unkept "rest pile" was moved. Defer the
    /// rest-pile move + reveal-marker cleanup onto the parked batch tail so it
    /// runs exactly once after the kept delivery settles — otherwise the rest
    /// cards strand in the library (the early-`return` bug).
    RevealRestPile {
        /// The player whose continuation drains after the pile lands.
        player: PlayerId,
        /// CR 400.7: The resolving effect's source, preserved so any rest-pile
        /// requests rebuilt after an earlier kept-card pause retain their
        /// original attribution. `None` retains the legacy self-anchor only for
        /// synthesized test/compatibility states that have no ability source.
        source_id: Option<ObjectId>,
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
    /// CR 608.2c + CR 616.1: The rest half of a deterministic mass Dig settled
    /// after a replacement choice. Resume its selected-card delivery only now,
    /// preserving the printed rest-before-kept sequence across the pause.
    DigMassPutAllRestComplete {
        player: PlayerId,
        source_id: ObjectId,
        selected: Vec<ObjectId>,
        destination: Zone,
        enter_tapped: EtbTapState,
    },
    /// CR 608.2c + CR 616.1: Every selected card of a deterministic mass Dig
    /// has settled. Publish only cards that actually reached `destination`, then
    /// emit the parent Dig result; the normal resolver/resume path owns the
    /// continuation drain.
    DigMassPutAllComplete {
        player: PlayerId,
        source_id: ObjectId,
        selected: Vec<ObjectId>,
        destination: Zone,
    },
    /// CR 608.2c + CR 616.1: A prior-look Dig's automatic rest move settled.
    /// The private-look window and parent resolution event remain live until the
    /// full replacement-aware batch has finished.
    DigPriorLookRestComplete {
        player: PlayerId,
        source_id: ObjectId,
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
    /// Unstable Contraptions: a Contraption being assembled paused on a
    /// battlefield-entry replacement-ordering choice. Defer the assembled
    /// object's bookkeeping and any remaining assembles of the same effect
    /// until the paused entry resolves.
    ContraptionAssembleRemainder {
        player: PlayerId,
        source_id: ObjectId,
        object_id: ObjectId,
        sprocket: u8,
        remaining_after: u32,
    },
    /// CR 101.4 + CR 701.23i + CR 616.1: A simultaneous scoped-library-search
    /// delivery paused on an individual zone-change replacement. Once every
    /// selected card has entered, continue with the once-after-all-searches
    /// tail; the tail retains its `PlayerFilter::PerformedActionThisWay` ledger
    /// and therefore shuffles only players who actually searched.
    LibrarySearchDeliverySettled { resume: LibrarySearchDeliveryResume },
    /// CR 701.23a + CR 616.1: The primary pile of a split search has settled.
    /// Route the second, destination-uniform pile through its own replacement-
    /// aware batch, then settle the shared search visibility exactly once.
    SearchPartitionPrimaryDelivered {
        rest_ids: Vec<ObjectId>,
        rest_destination: Zone,
        source_id: ObjectId,
        resume: LibrarySearchDeliveryResume,
    },
    /// CR 701.23a + CR 616.1: A found-card replacement sent the card through a
    /// zone move that itself paused for replacement ordering. Resume the saved
    /// found-card batch only after that move finishes.
    SearchFoundZoneDelivery {
        object_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grant: Option<crate::types::proposed_event::BoundSearchFoundGrant>,
    },
    /// CR 701.42 + CR 616.1: both selected meld referents have completed their
    /// simultaneous exile attempts. The typed context survives any replacement
    /// ordering pauses so physical-pair validation runs exactly once afterward.
    MeldExile { context: MeldSelection },
    /// CR 701.42 + CR 508.4: the meld result's battlefield entry paused on an
    /// as-enters replacement. Finish result layers/combat placement and emit the
    /// resolution marker exactly once after delivery.
    MeldEntry {
        context: MeldSelection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attack_target: Option<AttackTarget>,
    },
    /// CR 400.6 + CR 614.5: finish a redirected meld instruction after the
    /// second physical card has completed its independently replaceable move,
    /// carrying the originating event's applied-set through every pause.
    MeldRedirect { source_id: ObjectId },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LibrarySearchDeliveryResume {
    Standard {
        searcher: PlayerId,
    },
    Scoped {
        player: PlayerId,
        source_id: ObjectId,
        search_keys: Vec<PlayerId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        grants: Vec<(
            ObjectIncarnationRef,
            crate::types::proposed_event::BoundSearchFoundGrant,
        )>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        after_scope: Option<Box<ResolvedAbility>>,
    },
}

/// Resolution-stable identity for one selected meld pair. Live filters choose
/// these object IDs before exile; the canonical names validate their physical
/// card identities only after the simultaneous exile instruction completes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeldSelection {
    pub source_id: ObjectId,
    pub partner_id: ObjectId,
    pub controller: PlayerId,
    pub expected_source: String,
    pub expected_partner: String,
    pub result: String,
    #[serde(default)]
    pub entry: PermanentEntryMode,
}

/// Canonical meld relation derived from the loaded [`CardDatabase`]'s meld
/// layouts and parsed instigator instruction. Runtime resolution uses this
/// registry after exile instead of trusting live names or arbitrary effect data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeldPairRecord {
    pub source: String,
    pub partner: String,
    pub result: String,
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
pub enum LiminalTokenAbilityInjection {
    PredefinedToken,
    ResolvedToken,
}

/// CR 603.6a + CR 111.1: Copy-token batch members suppress per-entry emission until batch finalization emits the token entry once.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum TokenEntryEventEmission {
    Emit,
    Suppress,
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
    ContinueAmassAfterTokenCreation {
        controller: PlayerId,
        subtype: String,
        count: u32,
        ability: Box<ResolvedAbility>,
    },
    FinalizeAmass {
        object_id: ObjectId,
        subtype: String,
        ability: Box<ResolvedAbility>,
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
    FinalizeCommittedLiminalTokenEntry {
        object_id: ObjectId,
        name: String,
        source_id: ObjectId,
        controller: PlayerId,
        enters_attacking: bool,
        attach_to: Option<AttachTarget>,
        sacrifice_at: Option<Duration>,
        created_ids: Vec<ObjectId>,
        ability_injection: LiminalTokenAbilityInjection,
        entry_events: TokenEntryEventEmission,
    },
    ContinueLiminalCopyTokenBatch {
        owner: PlayerId,
        copy: Box<CopyTokenSpec>,
        enter_tapped: EtbTapState,
        enter_with_counters: Vec<(CounterType, u32)>,
        remaining_count: u32,
    },
    EmitCommittedCopyTokenEntry {
        object_id: ObjectId,
        name: String,
        source_id: ObjectId,
    },
    /// CR 701.42 + CR 707.9: finish a meld instruction after a copy-as-enters
    /// choice whose entry counters paused on their own replacement choice.
    FinishMeldEntry {
        context: MeldSelection,
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

/// CR 701.12c + CR 119.7 + CR 119.8 + CR 616.1: Remaining deltas from a
/// simultaneous life-total assignment that paused on a replacement choice. The
/// current gain/loss event is owned by `pending_replacement`; this record holds
/// only the tail plus the completion work that runs after the full assignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingLifeTotalAssignment {
    pub completion_player: PlayerId,
    pub remaining: Vec<(PlayerId, i32)>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completion: Option<PendingEffectResolved>,
}

/// CR 701.34a + CR 614.1a: Remaining proliferate actions after a replacement
/// effect (Tekuthal class) doubles the count. Each completed `ProliferateChoice`
/// drains one action; when `remaining` reaches zero the originating effect
/// resolves.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingProliferateActions {
    pub actor: PlayerId,
    pub source_id: ObjectId,
    pub remaining: u32,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum CastPaymentMode {
    #[default]
    Auto,
    Manual,
}

/// CR 601.2g + CR 601.2h + CR 602.2b: POSITIVE signal of which "mana first,
/// non-mana cost last" detour a pending activation took, so
/// `push_activated_ability_to_stack` knows whether `activation_cost` is the
/// still-unpaid residual non-mana tail and which interactive sub-cost to
/// re-surface. Replaces the former `x_residual_activation: bool`.
///
/// - `None`: no detour ran (direct path; `activation_cost`, if any, is the full
///   cost handled by the standard payment fall-through).
/// - `XMana`: the `{X}`-mana detour (`extract_x_mana_cost`) ran first; the
///   residual is the non-self DISCARD tail still outstanding after mana payment.
/// - `ManaLeg`: the non-X mana-leg detour ran first (CR 601.2g window opened on
///   the intact board); the residual is a non-self battlefield-removal tail
///   (Sacrifice / battlefield Exile / ReturnToHand) still outstanding after
///   mana payment.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ActivationResidual {
    #[default]
    None,
    XMana,
    ManaLeg,
}

/// CR 601.2c + CR 602.2b: Tracks whether an activation's target-declaration
/// step has completed before a later payment continuation reaches the stack.
///
/// This is intentionally distinct from `ActivationResidual`: the latter owns
/// unpaid cost legs, while this lifecycle records the completed target step.
/// A typed state prevents an explicitly declined optional target from being
/// presented again after a cost-move replacement pause.
#[derive(Default, Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum ActivationTargetSelection {
    #[default]
    Pending,
    Settled,
}

impl ActivationTargetSelection {
    /// `true` iff target declaration has not completed; used to omit the
    /// default from serialized pending roots for legacy save compatibility.
    pub fn is_pending(&self) -> bool {
        matches!(self, Self::Pending)
    }
}

impl ActivationResidual {
    /// `true` iff no residual detour was taken. Used as the serde
    /// `skip_serializing_if` predicate so the default does not hit the wire.
    pub fn is_none(&self) -> bool {
        matches!(self, ActivationResidual::None)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeferredSacrificeSelection {
    pub object_id: ObjectId,
    pub filter: TargetFilter,
}

/// Stable identity of a permission in a spell object's `casting_permissions`
/// vector while that spell is being cast.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CastingPermissionIndex(pub usize);

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
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
    /// CR 601.2b + CR 601.2f: Mana components of additional costs the caster
    /// has declared for this spell (Buyback, Splice, Spree mode costs, etc.).
    /// Recomputed totals start from `base_cost`, add these declarations, then
    /// apply cost modifiers and floors in total-cost order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub declared_mana_additions: Vec<ManaCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_cost: Option<AbilityCost>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation_ability_index: Option<usize>,
    /// CR 606.3: Loyalty activation history is recorded only after the loyalty
    /// cost is successfully paid. Positive loyalty costs can pause for a CR 616.1
    /// replacement-choice ordering prompt, so this marker carries the activator
    /// across the pause/resume path until the ability reaches the stack.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_loyalty_activation_player: Option<PlayerId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub target_constraints: Vec<TargetSelectionConstraint>,
    /// How this spell was cast — threads through the casting pipeline to finalize_cast.
    #[serde(default)]
    pub casting_variant: CastingVariant,
    /// CR 601.2a: Object-attached permission elected for this cast. Keeping the
    /// exact index prevents payment concessions from leaking between competing
    /// `PlayFromExile` / `ExileWithAltCost` grants on the same object.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub casting_permission_index: Option<CastingPermissionIndex>,
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
    /// CR 601.2f/h: Tap-payment mode contributed by an additional-cost mana
    /// component (currently Waterbend). Stored on the pending cast so composite
    /// costs can pay residual non-mana pieces before entering mana payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_cost_payment_mode: Option<ConvokeMode>,
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
    /// CR 601.2g + CR 601.2h: Non-mana spell additional-cost permanents selected
    /// for sacrifice, but whose actual zone move is deferred until the final
    /// payment commit so mana abilities can be activated first.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_sacrificed_permanents: Vec<DeferredSacrificeSelection>,
    /// CR 118.3a: Player-directed pin hints recorded during
    /// `WaitingFor::ManaPayment`. Each id names a pool `ManaUnit` the caster
    /// prefers to spend first; pins are priority hints, not removals — the unit
    /// stays in the pool and is consumed by the normal finalize spend.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pinned_pool_units: Vec<ManaPipId>,
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
    /// CR 601.2g + CR 601.2h + CR 602.2b: POSITIVE signal of which "mana first,
    /// non-mana cost last" detour this pending activation took, so its
    /// `activation_cost` residual tail is paid correctly after mana payment. See
    /// [`ActivationResidual`]. Set ONLY by the X-residual and mana-leg detours in
    /// `handle_activate_ability`; the discard/sacrifice-FIRST detours leave it
    /// `None` because they already paid the non-mana cost before resuming.
    /// Skipped on the wire when `None` (legacy saves deserialize to `None`);
    /// serialized otherwise so an activation paused mid-payment keeps the signal
    /// across a multiplayer save/restore.
    #[serde(default, skip_serializing_if = "ActivationResidual::is_none")]
    pub activation_residual: ActivationResidual,
    /// CR 601.2c + CR 602.2b: Preserves a completed activation target step
    /// through a cost-payment continuation without conflating it with cost legs.
    #[serde(default, skip_serializing_if = "ActivationTargetSelection::is_pending")]
    pub activation_target_selection: ActivationTargetSelection,
    /// CR 118.9 + CR 601.2b: When this cast is offered a once-per-turn
    /// `CastWithAlternativeCost` grant (As Foretold), the granting permanent's id.
    /// Carried across the `OptionalCostChoice` round-trip so the accept handler can
    /// stamp `ability.context.alt_cost_grant_source`; `None` for self-options and
    /// `Unlimited` grants.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub alt_cost_grant_source: Option<ObjectId>,
}

fn default_origin_zone() -> Zone {
    Zone::Hand
}

/// CR 601.2h + CR 616.1: Tail behavior for a sequential cost move that paused
/// on a replacement choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum PendingCostMoveCompletion {
    FinishPending,
    /// The automatic prefix of an activation cost paused before its already
    /// selected return-to-hand leg could move. Finish that selected leg before
    /// the activation resumes and surfaces any later return costs.
    CompleteSelectedReturnToHand {
        selected: Vec<ObjectId>,
        /// The automatic suffix left unpaid when its preceding self-move paused
        /// for a replacement choice. It must finish before the selected return
        /// move and any later return-to-hand chooser resume.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        automatic_remaining: Option<AbilityCost>,
    },
    PublishExileTrackedSet,
    FinalizeCast {
        phyrexian_choices: Option<Vec<ShardChoice>>,
        cascade_cast_transformed: bool,
        resolution_success_waiting_for: Option<Box<WaitingFor>>,
        pool_before: usize,
        prepaid_actual_mana_spent: Option<u32>,
    },
}

/// CR 605.3b: Selects whether completing a mana-ability cost payment may ask
/// the activator to choose the produced mana. An auto-tap plan already selected
/// production; a direct activation has not.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManaAbilityCostResolutionMode {
    #[default]
    Interactive,
    AutoResolved,
}

/// CR 605.3b + CR 616.1: Re-entry ownership for a costed parent mana ability.
/// This state exists only with a parent frame, so a parentless cursor cannot be
/// marked suspended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ManaAbilityCostParentLifecycle {
    #[default]
    Synchronous,
    Suspended,
}

/// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: A parent mana ability while
/// one of its mana sub-cost sources is resolving. The parent cursor still
/// contains its current Mana component, because that component is unpaid until
/// the child has produced mana and the parent can spend it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaAbilityCostParent {
    pub pending: Box<PendingManaAbility>,
    pub cursor: Box<ManaAbilityCostCursor>,
    #[serde(default)]
    pub lifecycle: ManaAbilityCostParentLifecycle,
}

/// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: The unpaid suffix of an
/// activated mana ability's cost. The selected-list cursors prevent a
/// replacement-choice resume from re-paying components or selections that
/// already completed before the interrupted move.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ManaAbilityCostCursor {
    pub remaining: Vec<AbilityCost>,
    /// Direct auto-tap resolution has already chosen its production path and
    /// therefore must not surface an output-color prompt after a paused cost
    /// move resumes. Interactive activation retains the prompt.
    #[serde(default)]
    pub resolution_mode: ManaAbilityCostResolutionMode,
    /// CR 605.3c: Ancestor mana sources excluded while a nested mana sub-cost
    /// is being auto-paid. Serialized with the root so a replacement choice
    /// cannot re-enable a suspended source after the move resumes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub excluded_sources: Vec<ObjectId>,
    /// CR 107.4b + CR 118.10: Colored demand from an outer mana payment.
    /// Preserving it prevents a resumed nested activation from consuming mana
    /// reserved for the outer cost.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sub_cost_demand: Option<[u32; 5]>,
    #[serde(default)]
    pub next_tapper: usize,
    #[serde(default)]
    pub next_discard: usize,
    #[serde(default)]
    pub next_exiled: usize,
    #[serde(default)]
    pub next_sacrificed: usize,
    /// The current selected-exile component, after the move that paused has
    /// been consumed. Its remaining objects must move before the cost cursor
    /// advances to the next component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_exile_remaining: Option<Vec<ObjectId>>,
    /// The current selected-sacrifice component, after the sacrifice that
    /// paused has been consumed. Its remaining objects must be sacrificed
    /// before the cost cursor advances to the next component.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub selected_sacrifice_remaining: Option<Vec<ObjectId>>,
    /// CR 603.2 + CR 603.3b: Cost events produced before a replacement-choice
    /// pause cannot reach the ordinary post-action pipeline. Keep them with
    /// their typed payment root so observers are collected exactly once when
    /// that root completes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deferred_cost_events: Vec<GameEvent>,
    /// Ephemeral split point for the active reducer action. It lets a newly
    /// nested typed root prepend its parent's unscanned batch without copying
    /// the local events this root already captured on pause. The split is
    /// consumed before state is returned to a player, so it is not serialized.
    #[serde(skip)]
    pub current_action_deferred_start: usize,
    /// A nested costed mana source owns this parent until it completes. This
    /// is a typed activation stack, never an effect continuation: on child
    /// completion the parent resumes its current unpaid Mana component without
    /// replaying any paid prefix.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<Box<ManaAbilityCostParent>>,
}

/// CR 601.2h + CR 602.2b + CR 616.1: The typed terminal path for a
/// replacement-paused sacrifice cost. Selected sacrifices remove their
/// activation-cost component; a SelfRef sacrifice resumes the automatic
/// additional-cost path without replaying the sacrifice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PendingSacrificeCostCompletion {
    SelectedNonSelf,
    SelfRef,
}

/// CR 702.21a + CR 701.21 + CR 616.1: The unpaid work after one ward sacrifice
/// pauses on a replacement choice. Aggregate ward costs retain their already
/// selected ordered suffix; sequential ward costs retain the live eligibility
/// basis needed to reconstruct the next one-per-round-trip choice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum WardSacrificePaymentResume {
    /// The selected aggregate sacrifice suffix, in the order the player chose.
    MultiSacrifice { remaining: Vec<ObjectId> },
    /// The selected sequential sacrifice settled through the replacement
    /// action. `remaining` includes that sacrifice, matching
    /// `WaitingFor::WardSacrificeChoice` before its ordinary re-prompt tail.
    Sequential {
        permanents: Vec<ObjectId>,
        sacrificed: ObjectId,
        remaining: u32,
    },
}

/// CR 601.2h + CR 614.1 + CR 616.1: A cost move paused for a replacement
/// choice. `Cast` resumes a cast or activation after its next object is
/// delivered. `ReplacementMayCost` keeps the outer optional replacement parked
/// while an inner MayCost move finishes through the replacement pipeline.
/// `Foretell` records the special action until its replacement-aware exile move
/// has been delivered or prevented. `ManaAbilityPayment` owns the exact
/// activation and unpaid payment cursor until the move has settled.
/// `DelveManaPayment` owns the single Delve fuel's post-move payment state;
/// the zone pipeline's delivery tail owns its delivered-only exile link.
/// `SacrificeForCost` owns a full selected sacrifice component across one or
/// more replacement-choice action boundaries, including its event span and
/// LKI record identities. `CollectEvidencePayment` and `UnlessBouncePayment`
/// retain their selected-object program counters and exact completion tails.
/// `WardSacrificePayment` retains either the ordered aggregate suffix or the
/// sequential re-prompt basis until the current sacrifice's replacement choice
/// has settled.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum PendingCostMoveResume {
    Cast {
        player: PlayerId,
        pending: Option<Box<PendingCast>>,
        chosen: Vec<ObjectId>,
        /// Index into `chosen` whose move completes during
        /// `handle_replacement_choice`; resumption starts with the next object.
        paused_at_index: usize,
        destination: Zone,
        completion: PendingCostMoveCompletion,
    },
    SacrificeForCost {
        player: PlayerId,
        pending: Box<PendingCast>,
        chosen: Vec<ObjectId>,
        /// Index into `chosen` whose sacrifice completes or is prevented by
        /// the replacement action. Resumption continues at the next index.
        paused_at_index: usize,
        completion: PendingSacrificeCostCompletion,
        /// CR 603.2 + CR 603.10a: Cost events emitted in earlier actions of
        /// this one logical sacrifice payment. They are stamped and settled
        /// only when every selected sacrifice has completed.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        deferred_cost_events: Vec<GameEvent>,
        /// CR 603.10a: Per-turn LKI record identities emitted by completed
        /// sacrifices before a replacement-choice boundary.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        departure_record_indices: Vec<usize>,
    },
    /// CR 702.21a + CR 701.21 + CR 614.1 + CR 616.1: A ward sacrifice payment
    /// that must wait for a competing replacement choice before its unpaid
    /// suffix or terminal effect-resolution tail can proceed.
    WardSacrificePayment {
        player: PlayerId,
        pending_effect: Box<ResolvedAbility>,
        resume: WardSacrificePaymentResume,
    },
    ReplacementMayCost {
        source_id: ObjectId,
        /// The object whose delivery is currently waiting on a replacement
        /// choice. It is indexed only if it actually arrives in exile.
        current: ObjectId,
        remaining: Vec<ObjectId>,
        paid_count: i32,
        /// The outer optional replacement is restored only after every inner
        /// cost move is delivered or prevented.
        outer_replacement: Option<Box<PendingReplacement>>,
    },
    Foretell {
        player: PlayerId,
        object_id: ObjectId,
        cost: ManaCost,
        turn_foretold: u32,
    },
    DelveManaPayment {
        player: PlayerId,
        fuel_id: ObjectId,
    },
    /// CR 701.59a + CR 614.1 + CR 616.1: The selected evidence cards are
    /// exiled one at a time as a cost. A replacement choice settles the card
    /// at `paused_at_index`; resumption continues with the unpaid suffix and
    /// performs the linked completion exactly once.
    CollectEvidencePayment {
        player: PlayerId,
        chosen: Vec<ObjectId>,
        paused_at_index: usize,
        resume: Box<CollectEvidenceResume>,
    },
    /// CR 118.12 + CR 614.1 + CR 616.1: A selected return-to-hand unless cost
    /// awaits its replacement outcome. Its typed tail either surfaces the next
    /// return choice or records that the unless payment avoided the effect.
    UnlessBouncePayment {
        player: PlayerId,
        moved: ObjectId,
        permanents: Vec<ObjectId>,
        pending_effect: Box<ResolvedAbility>,
        remaining: u32,
    },
    ManaAbilityPayment {
        pending: Box<PendingManaAbility>,
        cursor: ManaAbilityCostCursor,
    },
}

/// CR 601.2h + CR 616.1: Resume paying a sequential cost after a replacement
/// choice. The object at `paused_at_index` completes during
/// `handle_replacement_choice`; resumption starts with the following object.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDiscardForCostResume {
    pub player: PlayerId,
    pub pending: PendingCast,
    pub chosen: Vec<ObjectId>,
    /// Index into `chosen` whose move was paused; that move completes during
    /// `handle_replacement_choice` before this resume runs.
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
            declared_mana_additions: Vec::new(),
            activation_cost: None,
            activation_ability_index: None,
            pending_loyalty_activation_player: None,
            target_constraints: Vec::new(),
            casting_variant: CastingVariant::Normal,
            casting_permission_index: None,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            additional_cost_payment_mode: None,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            deferred_sacrificed_permanents: Vec::new(),
            pinned_pool_units: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            activation_residual: ActivationResidual::None,
            activation_target_selection: ActivationTargetSelection::Pending,
            alt_cost_grant_source: None,
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
        #[serde(default)]
        source: SpellCostSource,
    },
    Effect {
        pending_ability: Box<ResolvedAbility>,
    },
    /// CR 605.2 + CR 701.59: Collect evidence paid as a mana ability's
    /// activation cost (Cryptex's `{T}, Collect evidence 3: Add one mana...`).
    /// Resumes the parked mana-ability activation with the chosen cards stamped
    /// into `PendingManaAbility::collected_evidence`, rather than a `PendingCast`.
    ManaAbility {
        pending_mana_ability: Box<PendingManaAbility>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaAbilityResume {
    Priority,
    /// CR 116.2g + CR 702.139a + CR 605.3b + CR 616.1: A companion special
    /// action whose auto-tapped mana source paused on a replacement-aware cost
    /// move. `cost` is the final cost locked at action initiation, after
    /// special-action reductions; resumption must not recompute it against a
    /// changed board.
    CompanionToHand {
        player: PlayerId,
        cost: ManaCost,
    },
    ManaPayment {
        /// The payer of the outer spell/ability cost. This is intentionally
        /// independent from `PendingManaAbility::player`: Assist can activate
        /// a helper-controlled mana source while returning to the caster's
        /// `WaitingFor::ManaPayment` root.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outer_player: Option<PlayerId>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
    },
    UnlessPayment {
        /// The player who owns the outer unless-payment poll. It can differ
        /// from the controller of a helper mana source activated while paying.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        outer_player: Option<PlayerId>,
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
    /// CR 118.12 + CR 605.3b + CR 616.1: A resolving `Effect::PayCost` whose
    /// auto-tapped mana source paused on a replaceable cost move. The exact
    /// payer-adjusted ability and concrete outer cost are retried by the cost
    /// authority after that source finishes; this is not an effect-chain
    /// continuation.
    EffectPayCost {
        payer: PlayerId,
        return_to: PlayerId,
        ability: Box<ResolvedAbility>,
        cost: Box<AbilityCost>,
    },
    /// CR 107.4f + CR 601.2f-h + CR 605.3b + CR 616.1: Submitted
    /// Phyrexian shard choices remain authoritative while a helper's or
    /// caster's auto-tapped mana source pauses on a replaceable cost move.
    /// The pending cast is restored by the finalizer; this root retries that
    /// exact finalization rather than falling back to either player's priority.
    PhyrexianCastPayment {
        caster: PlayerId,
        choices: Vec<ShardChoice>,
    },
    /// CR 601.2h + CR 602.2b + CR 605.3b + CR 616.1: An automatic cast or
    /// mana-leg activation paused while an auto-tapped source paid a cost.
    /// The live `PendingCast` is the authoritative root; retry its shared
    /// finalizer rather than returning to priority or opening a manual window.
    FinalizePendingManaPayment {
        player: PlayerId,
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
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// CR 605.3b + CR 400.7: Mana ability choices can be answered after the
    /// source paid a cost that moved it out of existence (Treasure tokens, etc.).
    /// Preserve the activated ability definition from activation time so the
    /// chosen-color resume can resolve from LKI even when `source_id` is gone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ability_snapshot: Option<AbilityDefinition>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub color_override: Option<ProductionOverride>,
    pub resume: ManaAbilityResume,
    /// CR 605.3b + CR 616.1: An auto-tapped mana source normally returns to
    /// its immediate caller. If one of its own cost moves pauses, this is the
    /// serialized outer payment root that must be promoted before resumption.
    /// Keeping it separate prevents synchronous auto-taps from replaying an
    /// already-live outer payment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_move_resume: Option<ManaAbilityResume>,
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
    /// CR 107.3a + CR 601.2b + CR 702.179e/f: Announced value of X for a
    /// `Pay X speed` mana-ability cost (Chicago Loop's `Pay X speed: Add X mana
    /// in any combination of colors`). Chosen before cost payment and mana
    /// production; bound to BOTH the speed cost and the produced-mana count via
    /// `set_chosen_x_recursive`. `None` until the player announces X.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chosen_x: Option<u32>,
    /// CR 605.2 + CR 701.59: Cards exiled to pay a `Collect evidence N`
    /// mana-ability cost (Cryptex). Filled by the `CollectEvidenceChoice` resume
    /// before mana production; empty until the player selects cards.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collected_evidence: Vec<ObjectId>,
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
    /// CR 601.2c + CR 115.1: The player who *announces* (chooses the target for)
    /// this slot. `None` (the default) means the spell/ability's controller — the
    /// CR-601.2c default announcer. `Some(player)` is set only when the slot's
    /// Oracle text routes the announcement to another player ("of an opponent's
    /// choice", e.g. Volcanic Offering). The spell is
    /// still controlled, paid for, and put on the stack by its controller
    /// (CR 115.1) regardless of who announced a slot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chooser: Option<PlayerId>,
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
    EnteredObjects(BTreeSet<ObjectId>),
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
            Self::Clean => *self = Self::EnteredObjects(BTreeSet::from([id])),
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
    /// CR 115.1 + CR 601.2c + CR 400.1: Object targets must come from the same
    /// player-owned zone of the given kind, e.g. "from a single graveyard".
    SameZoneOwner {
        zone: Zone,
    },
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
    /// CR 508.1k + CR 400.7: the paused attack declaration snapshots each proposed
    /// attacker and band member as an `ObjectIncarnationRef`, not a bare `ObjectId`.
    /// On accept, only refs whose incarnation still matches the live object (and
    /// which remain controlled by the attacking team) become attackers — a creature
    /// that left and re-entered the battlefield during the tax pause is a new object
    /// (CR 400.7) and is dropped. Legacy saves that stored bare `ObjectId` numbers
    /// deserialize through `ObjectIncarnationRefCompat::Legacy` (→ `LEGACY_INCARNATION`),
    /// which never matches a live incarnation, so those stale attackers are also dropped.
    Attack {
        attacks: Vec<(
            crate::types::identifiers::ObjectIncarnationRef,
            crate::game::combat::AttackTarget,
        )>,
        /// CR 702.22c: attacking-band declarations captured alongside the
        /// attacks so the resume path (after combat-tax payment) stamps
        /// `band_id` via `commit_attack_declaration_from_snapshot` and groups the
        /// band for blocking (CR 702.22h).
        bands: Vec<Vec<crate::types::identifiers::ObjectIncarnationRef>>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
/// lists for main deck, sideboard, command-zone deck components, and
/// supplementary payloads carried across match games.
///
/// All `Vec<DeckEntry>` fields are wrapped in `Arc<Vec<_>>` so
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
    /// Commander-family companion registered outside the 100-card deck. This
    /// is intentionally distinct from `current_sideboard`: Commander games do
    /// not use sideboards (CR 903.5e), while a companion remains outside the
    /// game until it is revealed (CR 702.139a).
    #[serde(default)]
    pub registered_companion: std::sync::Arc<Vec<DeckEntry>>,
    #[serde(default)]
    pub current_companion: std::sync::Arc<Vec<DeckEntry>>,
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
    /// CR 901.15a: Registered shared Planechase planar deck payload. Only the
    /// PlayerId(0) pool carries the communal deck, matching `DeckPayload`.
    #[serde(default)]
    pub registered_planar_deck: std::sync::Arc<Vec<DeckEntry>>,
    /// CR 904.3: Registered Archenemy scheme deck payload. The configured
    /// archenemy's pool carries the shared scheme deck.
    #[serde(default)]
    pub registered_scheme_deck: std::sync::Arc<Vec<DeckEntry>>,
    #[serde(default)]
    pub current_scheme_deck: std::sync::Arc<Vec<DeckEntry>>,
    /// The declared bracket tier for this player's deck. Used by the AI to
    /// determine whether cEDH-specific policies apply (Phase 5 `ComboLinePolicy`,
    /// Phase 6 `CedhKeepablesMulligan`). Defaults to `Core` for backward
    /// compatibility with saved states and test fixtures that omit the field.
    #[serde(default)]
    pub bracket_tier: CommanderBracketTier,
}

/// The authoritative source of a companion offered during pre-game setup.
///
/// Keeping the provenance in the offered value (rather than exposing an
/// untyped index) makes a response self-validating across independent deck
/// pools and prevents a stale client from selecting a different card after a
/// pool changes.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CompanionChoiceSource {
    Sideboard { index: usize },
    Dedicated,
}

/// One companion offer published by `WaitingFor::CompanionReveal`.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub struct CompanionRevealChoice {
    pub name: String,
    pub source: CompanionChoiceSource,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum CompanionRevealChoiceWire {
    Current {
        name: String,
        source: CompanionChoiceSource,
    },
    /// Legacy saves encoded each normal-format offer as `(name, sideboard_index)`.
    /// The old engine never offered a Commander companion, so that representation
    /// unambiguously maps to the typed sideboard source.
    LegacySideboard((String, usize)),
}

impl<'de> Deserialize<'de> for CompanionRevealChoice {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        match CompanionRevealChoiceWire::deserialize(deserializer)? {
            CompanionRevealChoiceWire::Current { name, source } => Ok(Self { name, source }),
            CompanionRevealChoiceWire::LegacySideboard((name, index)) => Ok(Self {
                name,
                source: CompanionChoiceSource::Sideboard { index },
            }),
        }
    }
}

/// A player's complete response to a pre-game companion offer.
///
/// This is deliberately an enum rather than `Option<CompanionRevealChoice>`:
/// serde treats a missing `Option` field as `None`, which would turn an
/// incompatible legacy `card_index` payload into a silent decline. The wire
/// response must make both revealing and declining explicit.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum CompanionDeclaration {
    Reveal(CompanionRevealChoice),
    Decline,
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

/// CR 103.5b: Which declare-point action a pending `BottomCards` obligation
/// will complete once resolved. `Keep` locks in the hand (CR 103.5); the
/// player exits `pending`. `UseSerumPowder` runs the exile+redraw effect on
/// the now-reduced hand and returns the entry to `Declare` (Serum Powder
/// itself is not a mulligan — CR 103.5b + Serum Powder Oracle text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum PendingMulliganAction {
    Keep,
    UseSerumPowder { object_id: ObjectId },
}

/// CR 103.5 + 103.5b: Per-entry sub-state for the declare-point mulligan
/// flow. `Declare` is the default. `BottomCards` is entered the instant
/// `Keep` or `UseSerumPowder` is declared with `count > 0` still owed
/// against the per-player `prepaid_mulligan_bottoms` ledger; it must be
/// resolved via `SelectCards` before the entry can advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(tag = "type")]
pub enum MulliganDecisionPhase {
    #[default]
    Declare,
    BottomCards {
        count: u8,
        then: PendingMulliganAction,
    },
}

/// CR 103.5: Per-player state during the simultaneous mulligan decision phase.
/// One entry per player who has not yet declared "keep".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MulliganDecisionEntry {
    pub player: PlayerId,
    pub mulligan_count: u8,
    #[serde(default)]
    pub phase: MulliganDecisionPhase,
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

/// CR 616.1 / CR 614: Display payload for one replacement-effect option — either
/// one candidate in a CR 616.1 ordering prompt, or one branch (accept/decline)
/// of an optional "you may" replacement. Engine-derived so the filtered state
/// snapshot (multiplayer) and the frontend `ReplacementModal` never re-derive
/// the source object/description from `state.objects`, exactly as
/// [`PendingTriggerSummary`] does for CR 603.3b trigger ordering. For the
/// optional case both branches carry the same `source_id` (one object, two
/// outcomes); rule-based virtual replacements (shield counter, Umbra armor,
/// Compleated, combat skip) still point at the object they act on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReplacementCandidateSummary {
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
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
    /// CR 702.76a: Prowl alternative cost paid from hand, available only if a
    /// creature the caster controlled dealt combat damage to a player this turn
    /// while sharing one of the spell's creature types. A pure cost substitution
    /// — the spell resolves normally; the prowl provenance is recorded so "if
    /// its prowl cost was paid" intervening-ifs (Latchkey Faerie) can read it.
    Prowl,
    /// CR 702.37c (Morph) / CR 702.168b (Disguise): Cast the card face down as a
    /// 2/2 face-down creature spell for a fixed {3} (CR 601.2b alternative cost)
    /// rather than its mana cost. Offered from hand for any card with Morph,
    /// Megamorph, or Disguise; the resulting spell is blanked before it is put on
    /// the stack (CR 708.4) and resolves to a face-down permanent. Maps to
    /// `CastingVariant::FaceDown`.
    FaceDown,
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
    /// CR 701.20a + CR 601.2b: Reveal a card from hand matching the cost's
    /// filter as an additional/alternative cast cost. The chosen card stays
    /// in hand (revealing doesn't move it) and becomes the resolving
    /// ability's cost-paid-object referent (CR 608.2k).
    Reveal,
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
    /// CR 601.2h + CR 701.13: Exile a permanent the player controls from the
    /// battlefield as an additional/alternative cost (Food Chain class; Lunar
    /// Hatchling's "Exile a land you control"). Single-zone (battlefield only),
    /// distinct from `ExileFromZone` (Hand|Graveyard only) and `ExileMaterials`
    /// (dual-zone craft union). `filter` is the permanent-implying `TargetFilter`
    /// the choices were drawn from; the handler re-validates eligibility against
    /// the live battlefield before exiling. This is EXILE (CR 701.13), not
    /// sacrifice (CR 701.21) — no sacrifice/death triggers fire.
    ExilePermanent {
        filter: Option<TargetFilter>,
    },
    /// Exile objects from any zone (mana-ability exile costs).
    ExileFromManaZone {
        zone: Zone,
    },
    /// CR 701.3d + CR 601.2h: Unattach a matching attachment from the source
    /// host as an activation cost (Captain America's Throw). `filter` is the
    /// attachment-implying `TargetFilter` the choices were drawn from; the
    /// handler re-validates that each chosen object is still on the battlefield,
    /// controlled by the player, and attached to the source before detaching it.
    /// The Equipment stays on the battlefield (CR 701.3d) and its snapshot
    /// becomes the resolving ability's cost-referent (CR 608.2k).
    UnattachFrom {
        filter: TargetFilter,
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
    /// CR 601.2b: Tap creatures as a cost. `aggregate` distinguishes the two
    /// `TapCreaturesRequirement` shapes at the interactive payment layer: `None`
    /// is the fixed-count form (player taps exactly `WaitingFor::PayCost` `count`
    /// creatures; Conspire/Convoke), while `Some(aggregate)` is the aggregate
    /// "tap any number satisfying the constraint" form (Crew CR 702.122a / Saddle
    /// CR 702.171a / Teamwork) — the chosen set may be any size whose total
    /// positive power (CR 208.1) satisfies `aggregate`'s comparator vs its value.
    /// Carrying the full `TapCreaturesAggregate` (not just a threshold int) keeps
    /// the payment validator honoring the advertised comparator instead of
    /// hard-coding `>=`.
    TapCreatures {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        aggregate: Option<TapCreaturesAggregate>,
    },
    Behold {
        action: BeholdCostAction,
    },
    /// CR 117.1 + CR 601.2b + CR 602.2b: Interactive payment of an
    /// `AbilityCost::ExileWithAggregate` — the player exiles *any number* of the
    /// pre-filtered `WaitingFor::PayCost.choices` from `zone` such that the
    /// aggregate `function` of `property` over the chosen set satisfies
    /// `comparator` against `value`. Modeled on `PayCostKind::TapCreatures`
    /// (aggregate-threshold payment, validated by the handler rather than a fixed
    /// cardinality) combined with `ExileFromZone` (graveyard exile). The handler
    /// (`handle_exile_aggregate_for_cost`) re-validates uniqueness, still-in-zone
    /// membership, and the threshold, then publishes the exiled cards as a fresh
    /// tracked set and binds the resolving ability's tracked-set sentinel to it
    /// before the ability is pushed to the stack (CR 608.2c, Baron Helmut Zemo).
    ExileAggregate {
        zone: Zone,
        function: crate::types::ability::AggregateFunction,
        property: crate::types::ability::ObjectProperty,
        comparator: crate::types::ability::Comparator,
        value: i32,
        filter: crate::types::ability::TargetFilter,
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
    /// CR 118.12: Resume a resolution-time `Effect::PayCost` after the payer
    /// selects objects for an interactive cost such as tapping creatures.
    Resolution,
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
        /// CR 702.85a: Preserve the cascading ability's source across the
        /// cast-offer boundary for a declined or rejected cleanup batch.
        source_id: ObjectId,
    },
    /// CR 701.57a: Discover — cast the discovered card or put it to hand.
    Discover {
        hit_card: ObjectId,
        exiled_misses: Vec<ObjectId>,
        /// CR 701.57a: Preserve the resolving discover ability's source across
        /// the cast-offer boundary for a declined or rejected cleanup batch.
        source_id: ObjectId,
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
        /// CR 702.60a: Preserve the resolving Ripple source for its shared
        /// bottom-placement cleanup.
        source_id: ObjectId,
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
    /// CR 608.2g + CR 609.4b: A during-resolution PAID cast of a single card
    /// from a graveyard (Quistis Trepe, Tinybones the Pickpocket: "you may cast
    /// target X card from a graveyard, and mana of any type can be spent to cast
    /// that spell"). Unlike Cascade/Discover/Ripple this is not a free cast — on
    /// accept the caster pays the card's real printed cost with the any-type
    /// concession applied; on decline the card stays in the graveyard.
    GraveyardPaidCast {
        /// CR 601.2a: The graveyard card being offered for casting.
        hit_card: ObjectId,
        /// CR 609.4b: "mana of any type can be spent to cast that spell" —
        /// forwarded onto the granted permission so off-color mana pays the cost.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mana_spend_permission: Option<crate::types::ability::ManaSpendPermission>,
        /// CR 614.1a + CR 608.2n: Optional graveyard-redirect rider on the cast
        /// (e.g. "if that spell would be put into a graveyard, exile it instead").
        #[serde(default, skip_serializing_if = "Option::is_none")]
        graveyard_replacement: Option<crate::types::ability::SpellStackToGraveyardReplacement>,
        /// CR 712.14a: Whether the spell is cast transformed. Rare for this
        /// class; carried for parity with the free during-resolution casts.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        cast_transformed: bool,
        /// CR 601.2b: Optional cast-time predicate gating the cast.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        constraint: Option<crate::types::ability::CastPermissionConstraint>,
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

/// CR 119.7 + CR 119.8: One legal outcome of a "redistribute any number of players' life
/// totals" instruction — a complete assignment of a resulting life total to each
/// participating player. Enumerated by the engine resolver (which filters
/// CR 119.7 can't-gain / CR 119.8 can't-lose per receiver and dedupes
/// behaviorally-identical outcomes); the frontend renders it and returns an
/// index. `assignment[i] = (receiver, resulting_life)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifeRedistributionOption {
    pub assignment: Vec<(PlayerId, i32)>,
}

/// Private, live-only state for the finite pre-cast Chain-copy shortcut.
///
/// The public `WaitingFor` variants deliberately carry only actor-facing
/// opaque capabilities and display counts. The verified transcript, selected
/// route, suppression latch, and replay controls stay engine-private so a
/// viewer cannot fabricate or recover a route from serialized game state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(crate) struct PrecastShortcutRuntime {
    pub(crate) next_epoch: u64,
    pub(crate) offer: Option<PrecastShortcutOfferRuntime>,
    pub(crate) suppressed_cast: Option<ObjectId>,
    pub(crate) must_diverge: Option<PlayerId>,
    pub(crate) materializing: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PrecastShortcutOfferRuntime {
    pub(crate) caster: PlayerId,
    pub(crate) spell_id: ObjectId,
    pub(crate) epoch: u64,
    pub(crate) route_id: u64,
    pub(crate) responders: Vec<PlayerId>,
    pub(crate) transcript: Vec<PrecastShortcutReplayStep>,
    pub(crate) breakpoints: Vec<PrecastShortcutBreakpoint>,
    pub(crate) shortened: Option<PrecastShortcutBreakpoint>,
}

/// One exact normal-reducer action in the private route prefix. A shortener
/// never supplies these actions; they are authored and replayed only by the
/// engine after every responder has answered.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PrecastShortcutReplayStep {
    pub(crate) actor: PlayerId,
    pub(crate) action: crate::types::actions::GameAction,
}

/// A reducer-valid, actor-owned pass boundary. `prefix_length` indexes the
/// private replay transcript; the public protocol exposes only `id`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct PrecastShortcutBreakpoint {
    pub(crate) id: u64,
    pub(crate) owner: PlayerId,
    pub(crate) prefix_length: usize,
    pub(crate) expected_priority_holder: PlayerId,
    pub(crate) expected_active_player: PlayerId,
    pub(crate) expected_priority_passes: BTreeSet<PlayerId>,
    pub(crate) fingerprint: u64,
}

/// Trusted-persistence-only envelope for runtime data that must never cross a
/// raw or public `GameState` serialization boundary.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrustedGameStateEnvelope {
    state: GameState,
    #[serde(default)]
    precast_shortcut_runtime: PrecastShortcutRuntime,
}

impl TrustedGameStateEnvelope {
    /// Captures a trusted persistence snapshot without changing `GameState`'s
    /// raw serialization contract.
    pub fn capture(state: GameState) -> Self {
        Self {
            precast_shortcut_runtime: state.precast_shortcut_runtime.clone(),
            state,
        }
    }

    /// Restores private runtime data and rotates opaque capabilities before the
    /// restored state is exposed to clients.
    pub fn into_game_state(self) -> GameState {
        let mut state = self.state;
        state.precast_shortcut_runtime = self.precast_shortcut_runtime;
        crate::game::precast_copy_shortcut::rekey_after_trusted_restore(&mut state);
        state
    }
}

/// Decodes both current trusted snapshots and historical raw `GameState`
/// snapshots. The raw form has no pre-cast route authority, so restoring it
/// always drops any protocol wait before it reaches a live game session.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
pub enum PersistedGameState {
    Raw(Box<GameState>),
    Trusted(Box<TrustedGameStateEnvelope>),
}

impl<'de> Deserialize<'de> for PersistedGameState {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = serde_json::Value::deserialize(deserializer)?;
        if value.get("state").is_some() {
            serde_json::from_value(value)
                .map(|envelope| Self::Trusted(Box::new(envelope)))
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(|state| Self::Raw(Box::new(state)))
                .map_err(serde::de::Error::custom)
        }
    }
}

impl PersistedGameState {
    /// Captures a current trusted snapshot for a persistence boundary.
    pub fn capture(state: GameState) -> Self {
        Self::Trusted(Box::new(TrustedGameStateEnvelope::capture(state)))
    }

    /// Restores the persisted form through the appropriate trust boundary.
    pub fn into_game_state(self) -> GameState {
        match self {
            Self::Raw(state) => {
                let mut state = *state;
                crate::game::precast_copy_shortcut::normalize_untrusted_restore(&mut state);
                state
            }
            Self::Trusted(envelope) => (*envelope).into_game_state(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum WaitingFor {
    Priority {
        player: PlayerId,
    },
    /// CR 608.2d + CR 701.42: choose the exact pair of current battlefield
    /// referents the meld instruction will exile. Candidate identity is frozen
    /// in the tuples; the physical meld-card check intentionally happens later.
    MeldPairChoice {
        player: PlayerId,
        choices: Vec<MeldSelection>,
    },
    /// CR 508.4a: choose what the meld result enters attacking. The engine
    /// supplies the complete legal topology; clients only return one member.
    MeldAttackTargetChoice {
        player: PlayerId,
        context: MeldSelection,
        valid_targets: Vec<AttackTarget>,
    },
    /// CR 103.5 + 103.5b: London mulligan — each un-kept player decides
    /// simultaneously. The `pending` list holds every player who has not yet
    /// finished the flow, each with their current mulligan count and a
    /// per-entry `phase` (`MulliganDecisionPhase`). Players act in any order.
    /// In `Declare`, `Keep`/`Mulligan`/`UseSerumPowder` apply as usual;
    /// `Mulligan` increments the count, redraws, and resets that player's
    /// bottoms ledger. Bottoming is folded into this same variant: at a
    /// declare point (`Keep` or `UseSerumPowder`), if any bottoms are still
    /// owed against `prepaid_mulligan_bottoms`, the entry transitions to
    /// `BottomCards { count, then }` and the player resolves it with
    /// `SelectCards { cards }` — this happens at that player's own declare
    /// point, independent of every other player (CR 103.5b). When `pending`
    /// empties, the flow advances directly to `finish_mulligans`; there is no
    /// separate batch bottoms phase.
    ///
    /// CR 103.5d + CR 805.3a + CR 810.2: shared-team-turn mulligans are
    /// represented in the same simultaneous-decision model; every player
    /// remains independently pending until their own keep/mulligan decision.
    MulliganDecision {
        pending: Vec<MulliganDecisionEntry>,
        /// CR 103.5c + Commander RC supplement: whether this game grants a
        /// free first mulligan (multiplayer ≥3 seats, or a duel in a format
        /// where `GameFormat::grants_free_first_mulligan()` is true).
        /// Surfaced so display layers can render "Free Mulligan" labelling
        /// without re-deriving format/seat rules.
        free_first_mulligan: bool,
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
    /// `x_cost_previews` maps each legal X in `[min, max]` to the engine-
    /// authoritative total mana cost after concretizing X and applying cost
    /// modifiers (Affinity, reductions, floors). Display-only for the Choose-X
    /// UI — omitted when the range is empty or unreasonably large.
    ChooseXValue {
        player: PlayerId,
        #[serde(default)]
        min: u32,
        max: u32,
        pending_cast: Box<PendingCast>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        convoke_mode: Option<ConvokeMode>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        x_cost_previews: Vec<(u32, ManaCost)>,
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
        /// CR 508.1a–d: engine-authoritative per-attacker legal `AttackTarget`s.
        /// `None` = legacy serialized state (consumers fall back to the aggregate
        /// `valid_attack_targets`). `Some(map)` = authoritative; a missing attacker
        /// key means "no legal targets" (no fallback). New prompts always emit
        /// `Some` — computed by the same engine constraints model that enforces
        /// legality in `validate_attack_declaration`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        valid_attack_targets_by_attacker:
            Option<HashMap<ObjectId, Vec<crate::game::combat::AttackTarget>>>,
        /// CR 508.1c / CR 508.1d: per-creature combat requirement/restriction
        /// (must-attack / can't-attack) for display badges and Confirm gating.
        /// Display-only — computed by `combat::attacker_constraints_for_active_player`,
        /// the same predicates that enforce legality in `validate_attackers`.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        attacker_constraints: HashMap<ObjectId, crate::game::combat::CombatRequirement>,
    },
    DeclareBlockers {
        player: PlayerId,
        valid_blocker_ids: Vec<ObjectId>,
        #[serde(default)]
        valid_block_targets: HashMap<ObjectId, Vec<ObjectId>>,
        /// CR 702.111b (Menace) + CR 509.1b: per-attacker minimum-blocker count
        /// (`count`) plus the `sources` carriers imposing it, for attackers
        /// requiring more than one blocker. Lets the UI surface "needs N blockers"
        /// feedback (and which permanents demand it) and guard confirmation;
        /// attackers with the trivial requirement of 1 are omitted. Computed by
        /// `combat::block_requirements_for_player` — the same authority that
        /// enforces the requirement in `validate_blocks`.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        block_requirements: HashMap<ObjectId, crate::game::combat::BlockRequirement>,
        /// CR 509.1b / CR 509.1c: per-creature combat requirement/restriction
        /// (must-block / can't-block) for display badges and Confirm gating.
        /// Display-only — computed by `combat::blocker_constraints_for_player`,
        /// the same predicate that enforces legality in `validate_blockers_for_player`.
        #[serde(default, skip_serializing_if = "HashMap::is_empty")]
        blocker_constraints: HashMap<ObjectId, crate::game::combat::CombatRequirement>,
    },
    /// CR 502.3: During the untap step, the active player may choose not to
    /// untap permanents with "You may choose not to untap..." static abilities.
    UntapChoice {
        player: PlayerId,
        candidates: Vec<ObjectId>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        chosen_not_to_untap: Vec<ObjectId>,
    },
    /// CR 502.3: "the active player determines which permanents they control
    /// will untap." When a `StaticMode::MaxUntapPerType` cap (Smoke /
    /// Stoic Angel / Damping Field / Winter Orb class) leaves more than `max`
    /// matching tapped permanents eligible to untap, the active player must
    /// directly choose the bounded subset that untaps — this is a REQUIRED
    /// bounded selection, NOT the per-permanent optional decline modeled by
    /// `UntapChoice` (which is the "you may choose not to untap" Vedalken
    /// Shackles class). `group` is the over-cap set of eligible permanents
    /// (after declines / CantUntap have been removed); the player answers with
    /// `GameAction::SelectCards { cards }` naming up to `max` members of `group`
    /// to untap. The complement of the chosen set stays tapped. Enforcement in
    /// `turns::execute_untap_with_choices` keeps a deterministic clamp purely as
    /// a safety net for malformed selections.
    ChooseUntapSubset {
        player: PlayerId,
        /// The over-cap eligible permanents the player chooses among. All are
        /// tapped, controlled by `player`, match the cap's filter, and can
        /// legally untap (no CantUntap). `group.len() > max`.
        group: Vec<ObjectId>,
        /// CR 502.3 cap: the player may untap at most this many of `group`.
        max: usize,
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
        candidates: Vec<ReplacementCandidateSummary>,
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
        /// CR 702.122a: each eligible creature's crew-power contribution
        /// (`object_crew_power_contribution`), aligned index-for-index with
        /// `eligible_creatures`. The engine owns this computation — "as though its
        /// power were N greater" (Pilot tokens) and "using its toughness" (Giant
        /// Ox) mean the contribution differs from the creature's printed power, so
        /// the UI MUST sum these values, not raw power, when gating the selection.
        #[serde(default)]
        contributions: Vec<i32>,
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
        /// CR 702.171a: each eligible creature's saddle-power contribution,
        /// aligned index-for-index with `eligible_creatures`.
        #[serde(default)]
        contributions: Vec<i32>,
    },
    ScryChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
    },
    /// CR 901.15 + CR 701.22a analogue: Arrange the top N cards of the planar
    /// deck — put exactly `keep_on_top` on top in the submitted order and the
    /// rest on the bottom in any order (Susan Foreman).
    ArrangePlanarDeckTopChoice {
        player: PlayerId,
        cards: Vec<ObjectId>,
        keep_on_top: usize,
    },
    /// CR 119.7 + CR 119.8: The controlling player redistributes participating players'
    /// life totals (Reverse the Sands, The Doctor's Tomb). `options` is the
    /// engine-enumerated set of legal assignments (identity always present); the
    /// player submits `GameAction::SubmitLifeRedistribution { option_index }`.
    RedistributeLifeTotals {
        player: PlayerId,
        options: Vec<LifeRedistributionOption>,
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
        /// CR 701.23a: Owner of the library component actually included in
        /// this search after prohibitions are applied.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_owner: Option<PlayerId>,
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
        /// CR 701.23b: Hidden-zone stated-quality searches may select fewer
        /// than `count` cards even when the printed text is not an "up to" search.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        allows_partial_find: bool,
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
    /// CR 608.2d: Player selects card(s) from a tracked set (e.g., exiled cards).
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
    /// CR 701.4a: Behold a [quality] — the resolving player chooses which
    /// beholdable object to reveal-or-choose from a MIXED-ZONE candidate set
    /// (permanents they control on the battlefield ∪ matching cards in their
    /// hand). Only raised when two or more candidates exist (one candidate
    /// auto-collapses; none whiffs). `choices` are the controller's own private
    /// objects, so `visibility.rs::filter_state_for_viewer` redacts them to
    /// `ObjectId(0)` for other viewers — the pre-choice hand-Dragon list must not
    /// leak. On submit, a chosen HAND card emits `CardsRevealed` (CR 701.4a, card
    /// stays in hand); a chosen battlefield permanent reveals nothing.
    BeholdChoice {
        player: PlayerId,
        choices: Vec<ObjectId>,
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
        /// CR 614.5 + CR 616.1f: replacement effects already applied to the
        /// event that produced this choice.
        #[serde(default, skip_serializing_if = "HashSet::is_empty")]
        replacement_applied: HashSet<AppliedReplacementKey>,
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
        /// CR 122.1 + CR 614.1c: Unconditional entry-time counters carried across
        /// the `EffectZoneChoice` round-trip (e.g. "enters with two +1/+1
        /// counters").
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        enter_with_counters: Vec<(CounterType, u32)>,
        /// CR 122.1 + CR 614.1c: Conditional entry-time counter specs carried
        /// across the `EffectZoneChoice` round-trip (e.g. "If a Hero enters
        /// this way, it enters with an additional +1/+1 counter on it").
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        conditional_enter_with_counters: Vec<(TargetFilter, CounterType, QuantityExpr)>,
        /// CR 701.68a: N for Blight N — number of -1/-1 counters to place.
        /// Zero for all non-blight EffectZoneChoice uses.
        #[serde(default)]
        count_param: u32,
        /// CR 401.4: Explicit library placement for resolution-time
        /// `PutAtLibraryPosition` choices. `None` = top (Brainstorm); `Some`
        /// preserves bottom/nth placement across the choice round-trip.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        library_position: Option<LibraryPosition>,
        /// CR 118.3: When true, this choice is for a cost payment (e.g., exile cost)
        /// rather than effect resolution. Cost-payment choices require special
        /// handling for exile-link tracking (push_exiled_with_source_this_turn).
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        is_cost_payment: bool,
        /// CR 614.12: gates the `enter_tapped`/`enters_attacking` riders on the
        /// chosen object's type, carried across the `EffectZoneChoice` round-trip
        /// so the gate is evaluated per chosen object at resume (Summoner's
        /// Grimoire). `None` = apply the riders unconditionally (every non-Grimoire
        /// use). Mirrors the `enter_tapped` / `enters_attacking` carry-through above.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        enters_modified_if: Option<crate::types::ability::TargetFilter>,
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
        /// CR 701.62a: resolving ability source for zone-pipeline attribution on
        /// the chosen manifest entry (Abhorrent Oculus class).
        source_id: ObjectId,
    },
    TriggerTargetSelection {
        player: PlayerId,
        /// Controller of the triggered ability whose targets are being chosen.
        /// This can differ from `player` for "of their choice" prompts.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_controller: Option<PlayerId>,
        /// Event that caused this triggered ability, if the trigger needs it for
        /// display or event-context quantities while targets are being chosen.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<GameEvent>,
        /// Full simultaneous-event batch for this trigger instance.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        trigger_events: Vec<GameEvent>,
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
        /// The object that originated this choice. Persistable choice types store
        /// their value there; transient prompts use this as source context.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
        /// CR 607.2d / CR 607.2m (by analogy): when set, this choice's answer is a
        /// PER-PLAYER persistent anchor label — the answer binds
        /// `ChosenAttribute::Label` onto `state.players[persist_player]`
        /// (`chosen_attributes`) instead of onto `source_id`'s object. Set during
        /// a `player_scope: All` fan-out of a persisting `Effect::Choose` to the
        /// fanned per-player value (`ability.scoped_player`). `None` preserves the
        /// object-scoped binding used by Khans Sieges and every other named choice.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        persist_player: Option<PlayerId>,
    },
    /// CR 608.2d + CR 608.2e: a player other than the controller (an opponent /
    /// the defending player) guesses a committed value or proposition during
    /// resolution of an `Effect::OpponentGuess`. `player` is the guesser;
    /// `source_id` lets the answer handler derive the controller and read the
    /// committed `ChosenAttribute::Number`. This wait is a member of
    /// `waits_for_resolution_choice` — the branch chain is auto-stashed onto
    /// `pending_continuation` and re-evaluated on drain once the outcome is known
    /// (the deferred "If you do" / `NamedChoice` resolution pattern).
    OpponentGuess {
        player: PlayerId,
        options: Vec<String>,
        choice_type: ChoiceType,
        source_id: ObjectId,
        /// CR 608.2d: For a `GuessSubject::Proposition`, the proposition's truth
        /// resolved at the moment the guess was raised (when the resolving
        /// ability's targets are still in scope). The answer handler compares the
        /// guesser's chosen label against this to decide correctness. `None` for
        /// `GuessSubject::CommittedChoice`, whose correctness is read from the
        /// source's last committed `ChosenAttribute::Number` at answer time.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        proposition_truth: Option<bool>,
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
        /// Mode indices unavailable due to NoRepeat constraints or unsatisfied
        /// targeting requirements (CR 700.2a-b). Mirrors `AbilityModeChoice`.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        unavailable_modes: Vec<usize>,
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
    /// CR 732.2a: the interactive loop-shortcut OFFER. Raised (only under
    /// `LoopDetectionMode::Interactive`) when the reconcile bridge confirms an OPTIONAL
    /// loop. The player with priority (`proposer`) may declare the shortcut; the measured
    /// `predicted_winner`, when present, remains distinct because the priority holder need
    /// not be the player expected to win. `acting_player()` routes to `proposer`. The
    /// `certificate` is the confirmed loop's public summary.
    LoopShortcut {
        proposer: PlayerId,
        /// The winner measured by the offer-time loop detector. Object-growth offers that
        /// establish unbounded advantage without a determinate winner carry `None`.
        predicted_winner: Option<PlayerId>,
        certificate: crate::analysis::loop_check::LoopCertificate,
        /// CR 732.2a: the READ-side decision schema the frontend renders to declare the
        /// shortcut (open per-iteration choices + their legal option sets). Built against the
        /// proposer's full view at offer construction; hidden-info legal targets are redacted
        /// per-viewer in `game::visibility::filter_state_for_viewer`. `#[serde(default)]` for
        /// forward-compatible deserialization of pre-schema snapshots.
        #[serde(default)]
        schema: crate::analysis::decision_template::ShortcutDecisionSchema,
    },
    /// CR 732.2b/c: the APNAP accept-or-shorten window. After the proposer declares the
    /// shortcut, each other living player is prompted in turn order (drain-one-advance
    /// via `remaining_players`, mirroring `OpponentMayChoice.remaining`). `player` is the
    /// current responder; `proposal` is the public offer summary.
    RespondToShortcut {
        player: PlayerId,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining_players: Vec<PlayerId>,
        proposal: crate::analysis::loop_check::ShortcutProposal,
    },
    /// CR 732.2a: an engine-proved finite, pre-cast shortcut proposal. This
    /// family is intentionally separate from `LoopShortcut`; it is not a
    /// generic loop declaration and exposes no replay transcript.
    PrecastCopyShortcutOffer {
        proposer: PlayerId,
        epoch: u64,
        route_count: u8,
    },
    /// CR 732.2b/c: a responder's accept-or-shorten turn for the finite
    /// pre-cast route. `breakpoint_ids` are opaque, engine-issued pass
    /// boundaries owned by this responder.
    RespondToPrecastCopyShortcut {
        player: PlayerId,
        epoch: u64,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        breakpoint_ids: Vec<u64>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        remaining_players: Vec<PlayerId>,
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
    /// CR 709.5f-g: A resolving lock/unlock-door effect needs the player to
    /// choose which door (half) of the targeted Room to act on. `options`
    /// enumerates each legal (operation, door) pair from `room::eligible_doors`
    /// — locked halves to unlock (CR 709.5f), unlocked halves to lock (CR
    /// 709.5g). For a "lock or unlock" effect both operations can appear, so the
    /// player chooses operation and door together. Answered with
    /// `GameAction::ChooseRoomDoor { object_id, op, door }`.
    ChooseRoomDoor {
        player: PlayerId,
        object_id: ObjectId,
        options: Vec<(
            crate::types::ability::DoorLockOp,
            crate::game::game_object::RoomDoor,
        )>,
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
    /// CR 601.2b + CR 701.4a: The player must choose a value (creature type for
    /// Celestial Reunion) as part of paying a `Behold { type_choice: Some(_) }`
    /// additional cost, before the behold selection. The chosen value is written
    /// as a `ChosenAttribute` on the spell object; cost payment then resumes the
    /// behold step. `options` is the feasible set (types with >= count beholdable
    /// creatures), so an unpayable type is never offered.
    CostTypeChoice {
        player: PlayerId,
        choice_type: crate::types::ability::ChoiceType,
        options: Vec<String>,
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
    /// CR 601.2c + CR 115.1: A spell with an "of an opponent's choice" target slot
    /// is being cast in a multiplayer game; the controller (`player`) chooses
    /// which opponent will announce that slot's target. Only entered with two or
    /// more opponents (one opponent has no decision). `candidates` is the legal
    /// opponent set; `choice_index` and `choice_count` identify the printed
    /// opponent-choice target group currently being assigned, so a display
    /// client can distinguish consecutive prompts without reinterpreting the
    /// pending spell. `pending_cast` carries the in-flight cast so target
    /// declaration resumes (deferred) once the announcer is chosen.
    ChooseAnnouncingOpponent {
        player: PlayerId,
        candidates: Vec<PlayerId>,
        choice_index: usize,
        choice_count: usize,
        /// The primary type constraint of the target group, when there is one.
        /// This is a display fact only: target legality remains engine-owned.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_type: Option<crate::types::card_type::CoreType>,
        pending_cast: Box<PendingCast>,
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
        ballots: im::Vector<(PlayerId, u32)>,
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
        /// CR 701.38a: How the completed tally maps to effects (the
        /// top-tally/tie outcome of `TopVotes` is card-defined, not a
        /// CR subrule). Carried on the WaitingFor (not re-derived from the
        /// source ability) so the final `resolve_tally` can branch between
        /// per-vote fan-out (`VoteTally::PerVote`) and Will-of-the-council
        /// winner resolution (`VoteTally::TopVotes`) once the voter queue
        /// empties. `TopVotes` itself resolves either a single winner
        /// (`TieResolution::Breaker`) or every tied winner
        /// (`TieResolution::AllTied`, multi-outcome).
        /// Defaults to `PerVote` so pre-existing serialized vote states
        /// deserialize unchanged.
        #[serde(default)]
        tally_mode: super::ability::VoteTally,
        /// CR 701.38b: For object-pool votes (`VoteSubject::Objects` —
        /// Council's Judgment, Prime Minister's Cabinet Room), the candidate
        /// objects enumerated at resolution. Parallel to `options` /
        /// `option_labels`: a ballot `(PlayerId, u8)` indexes into this vector.
        /// Empty for named votes; ballots there carry the `options` index.
        #[serde(default)]
        candidate_objects: im::Vector<ObjectId>,
        /// CR 701.38b + CR 608.2c: For object-pool votes, the per-winner
        /// outcome template (e.g. single-target Exile). `resolve_top_votes_tally`
        /// resolves it once per winning object with that object injected as the
        /// single target. `None` for named votes (which use `per_choice_effect`).
        #[serde(default)]
        outcome_template: Option<Box<super::ability::AbilityDefinition>>,
        /// Card-defined: whether this vote is public (`Open`) or secret
        /// (`Secret` — Truth or Consequences). Under `Secret`, per-ballot
        /// `VoteCast` events are suppressed and `filter_state_for_viewer`
        /// scrubs running tallies/ballots until the simultaneous reveal.
        /// Defaults to `Open` so pre-existing serialized states deserialize
        /// unchanged.
        #[serde(default)]
        visibility: super::ability::VoteVisibility,
    },
    /// CR 608.2d + CR 700.3: "An opponent separates" — in multiplayer the
    /// controller chooses which opponent will perform the partition. With a
    /// single opponent this state is skipped (no decision). The chosen
    /// opponent feeds into [`Self::SeparatePilesPartition`].
    SeparatePilesChooseOpponent {
        /// The controller making the choice.
        player: PlayerId,
        /// Non-eliminated opponents eligible to be chosen.
        candidates: Vec<PlayerId>,
        /// The revealed card pool to be partitioned.
        eligible: im::Vector<ObjectId>,
        /// Who will choose a pile after partitioning.
        chooser: PlayerId,
        /// Sub-effect for the chosen pile.
        chosen_pile_effect: Box<super::ability::AbilityDefinition>,
        /// Optional sub-effect for the unchosen pile.
        unchosen_pile_effect: Option<Box<super::ability::AbilityDefinition>>,
        /// Source ability's object ID.
        source_id: ObjectId,
        /// CR 700.3: Where the objects originate (battlefield, library top, exile).
        #[serde(default = "default_pile_source_battlefield")]
        pile_source: PileSource,
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
        /// CR 608.2c: Optional sub-effect applied to each unchosen pile object.
        unchosen_pile_effect: Option<Box<super::ability::AbilityDefinition>>,
        /// Source ability's object ID — for logging and state filter echoes.
        source_id: ObjectId,
        /// CR 700.3: Where the objects originate (battlefield, library top, exile).
        #[serde(default = "default_pile_source_battlefield")]
        pile_source: PileSource,
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
        /// CR 608.2c: Optional sub-effect applied to each unchosen pile object.
        unchosen_pile_effect: Option<Box<super::ability::AbilityDefinition>>,
        /// Source ability's object ID — for logging and state filter echoes.
        source_id: ObjectId,
        /// CR 700.3: Where the objects originate (battlefield, library top, exile).
        #[serde(default = "default_pile_source_battlefield")]
        pile_source: PileSource,
    },
    /// CR 702.139a: Before the game begins, reveal companion from outside the game.
    CompanionReveal {
        player: PlayerId,
        /// The exact companions the player may reveal, including their source.
        /// Responses must exactly equal one of these offered values.
        eligible_companions: Vec<CompanionRevealChoice>,
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
    /// CR 101.4 + CR 707.2: One player selects an ordered `min..=max` objects for
    /// [`Effect::EachPlayerCopyChosen`]. `order` is load-bearing: index 0 is
    /// copied, index 1 (if present) scales the copy. The effect params + walk
    /// state are threaded so the continuation can recompute eligibility and drive
    /// each subsequent player in APNAP order.
    EachPlayerCopyChosenSelection {
        player: PlayerId,
        /// Eligible objects for this chooser — either their own or their
        /// seat-neighbor's, per `choose_scope` (all public battlefield info).
        eligible: Vec<TargetRef>,
        min: u32,
        max: u32,
        choose_filter: TargetFilter,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        copy_modifications: Vec<ContinuousModification>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        scale: Option<CopyScale>,
        /// CR 102.1 + CR 103.1: whose battlefield this chooser's pool was drawn
        /// from, so live re-validation (CR 608.2c) uses the right controller.
        #[serde(default)]
        choose_scope: CopyChooseScope,
        source_id: ObjectId,
        source_controller: PlayerId,
        /// Players still to choose after the current one (APNAP order).
        remaining_players: Vec<PlayerId>,
        /// CR 101.4: choices already made by earlier players. Actions are not
        /// performed until this contains the complete APNAP choice set.
        #[serde(default)]
        all_choices: Vec<CopyChosenSelection>,
        /// CR 101.4: the APNAP-ordered scoped player set. Empty only on a
        /// mid-resolution save/reload (`#[serde(default)]`).
        #[serde(default)]
        scoped_players: Vec<PlayerId>,
        /// CR 608.2: triggering event of the phenomenon trigger, restored around
        /// the continuation so resolution-scoped reads resolve correctly.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger_event: Option<crate::types::events::GameEvent>,
    },
    /// CR 107.1c + CR 701.21a (Slaughter the Strong): `player` keeps any number of
    /// `target_player`'s `eligible` creatures whose combined power is at most
    /// `cap`, then the rest are sacrificed. Entered only when keeping all eligible
    /// creatures would exceed the cap (otherwise the keep-all case auto-resolves).
    KeepWithinTotalPowerChoice {
        player: PlayerId,
        target_player: PlayerId,
        /// The chooser's creatures eligible to be kept (battlefield objects).
        eligible: Vec<ObjectId>,
        /// CR 208.3: maximum combined power of the kept subset.
        cap: i32,
        #[serde(default = "default_target_filter_permanent")]
        choose_filter: TargetFilter,
        #[serde(default = "default_target_filter_permanent")]
        sacrifice_filter: TargetFilter,
        #[serde(default)]
        chooser_scope: CategoryChooserScope,
        source_id: ObjectId,
        #[serde(default)]
        source_controller: PlayerId,
        /// Players still to choose after the current one (APNAP order).
        remaining_players: Vec<PlayerId>,
        /// Creatures kept by previous players — protected from the sacrifice sweep.
        all_kept: Vec<ObjectId>,
        /// APNAP-ordered set of players within the effect's `player_scope`.
        #[serde(default)]
        scoped_players: Vec<PlayerId>,
    },
    /// CR 101.4 + CR 701.21a: The player protects exactly `count` of the
    /// eligible permanents; every other in-scope permanent is sacrificed after
    /// all scoped players have chosen. Used by the generic exact keeper
    /// constraint, not a card-specific choice shape.
    KeepExactPermanentsChoice {
        player: PlayerId,
        target_player: PlayerId,
        eligible: Vec<ObjectId>,
        /// Exact number of permanents the engine requires the player to keep,
        /// already capped to this prompt's eligible pool under CR 609.3's
        /// "do as much as possible" rule. Display clients render this value
        /// directly; they must not derive a second legality rule from
        /// `eligible.len()`.
        #[serde(alias = "count")]
        required_count: usize,
        #[serde(default = "default_target_filter_permanent")]
        choose_filter: TargetFilter,
        #[serde(default = "default_target_filter_permanent")]
        sacrifice_filter: TargetFilter,
        #[serde(default)]
        chooser_scope: CategoryChooserScope,
        source_id: ObjectId,
        #[serde(default)]
        source_controller: PlayerId,
        #[serde(default)]
        remaining_players: Vec<PlayerId>,
        #[serde(default)]
        all_kept: Vec<ObjectId>,
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
        /// Remaining paradigm sources to re-offer after this copy's targets are
        /// chosen (issue #3660).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        paradigm_remaining_offers: Option<Vec<ObjectId>>,
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
    /// CR 107.1c + CR 608.2d: "Remove any number of counters from [source]"
    /// chooses which counter types and how many of each to remove as the ability
    /// resolves (Rhys, the Evermore; Tetravus). CR 107.1c: the empty selection
    /// (remove zero) is always legal. `available` exposes only the public per-type
    /// counter counts on `source_id` (CR 122.1 — counters are public markers), so
    /// no multiplayer redaction is needed. Sibling of `MoveCountersDistribution`
    /// with no destination axis (counters are shed, not relocated).
    RemoveCountersChoice {
        player: PlayerId,
        source_id: ObjectId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        counter_type: Option<CounterType>,
        available: Vec<(CounterType, u32)>,
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
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
    /// No player *choice* in HOW it's split, but whether the flow pauses at
    /// `WaitingFor::DistributeAmong` depends on the casting route:
    /// - Non-deferred-target-selection flow (inside `finalize_mana_payment`):
    ///   the split is applied inline and `WaitingFor::DistributeAmong` is bypassed.
    /// - Deferred-target-selection flow (e.g. Fireball, gated by
    ///   `ability_utils::ability_distribution_pool_needs_chosen_x`): the cast still
    ///   pauses at `WaitingFor::DistributeAmong` via
    ///   `casting_targets::maybe_pause_for_cast_distribution` — the split itself is
    ///   automatic, but the pause is needed so cost (CR 601.2f) and target legality
    ///   can re-resolve once targets are known.
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
    /// CR 119.4: Pay any amount of life — N is deducted as life loss via
    /// life_costs::pay_life_as_cost (life-loss replacement pipeline + CantLoseLife).
    Life,
    /// CR 702.179e/f: Announce X for a `Pay X speed` mana-ability cost. The chosen
    /// amount is the announced X, bounded above by the player's current speed
    /// (CR 702.179f: no speed counts as 0). Mana-ability only — paid via the
    /// `PendingManaAbility::chosen_x` path, never the standalone resource branch.
    Speed,
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
            WaitingFor::MeldPairChoice { .. } => "MeldPairChoice",
            WaitingFor::MeldAttackTargetChoice { .. } => "MeldAttackTargetChoice",
            WaitingFor::MulliganDecision { .. } => "MulliganDecision",
            WaitingFor::OpeningHandBottomCards { .. } => "OpeningHandBottomCards",
            WaitingFor::ManaPayment { .. } => "ManaPayment",
            WaitingFor::ChooseXValue { .. } => "ChooseXValue",
            WaitingFor::TargetSelection { .. } => "TargetSelection",
            WaitingFor::DeclareAttackers { .. } => "DeclareAttackers",
            WaitingFor::DeclareBlockers { .. } => "DeclareBlockers",
            WaitingFor::UntapChoice { .. } => "UntapChoice",
            WaitingFor::ChooseUntapSubset { .. } => "ChooseUntapSubset",
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
            WaitingFor::ArrangePlanarDeckTopChoice { .. } => "ArrangePlanarDeckTopChoice",
            WaitingFor::RedistributeLifeTotals { .. } => "RedistributeLifeTotals",
            WaitingFor::CoinFlipKeepChoice { .. } => "CoinFlipKeepChoice",
            WaitingFor::DigChoice { .. } => "DigChoice",
            WaitingFor::SurveilChoice { .. } => "SurveilChoice",
            WaitingFor::RevealChoice { .. } => "RevealChoice",
            WaitingFor::SearchChoice { .. } => "SearchChoice",
            WaitingFor::SearchPartitionChoice { .. } => "SearchPartitionChoice",
            WaitingFor::OutsideGameChoice { .. } => "OutsideGameChoice",
            WaitingFor::ChooseFromZoneChoice { .. } => "ChooseFromZoneChoice",
            WaitingFor::BeholdChoice { .. } => "BeholdChoice",
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
            WaitingFor::OpponentGuess { .. } => "OpponentGuess",
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
            WaitingFor::LoopShortcut { .. } => "LoopShortcut",
            WaitingFor::RespondToShortcut { .. } => "RespondToShortcut",
            WaitingFor::PrecastCopyShortcutOffer { .. } => "PrecastCopyShortcutOffer",
            WaitingFor::RespondToPrecastCopyShortcut { .. } => "RespondToPrecastCopyShortcut",
            WaitingFor::UnlessPayment { .. } => "UnlessPayment",
            WaitingFor::UnlessPaymentChooseCost { .. } => "UnlessPaymentChooseCost",
            WaitingFor::WardDiscardChoice { .. } => "WardDiscardChoice",
            WaitingFor::WardSacrificeChoice { .. } => "WardSacrificeChoice",
            WaitingFor::UnlessBounceChoice { .. } => "UnlessBounceChoice",
            WaitingFor::ChooseRingBearer { .. } => "ChooseRingBearer",
            WaitingFor::ChooseRoomDoor { .. } => "ChooseRoomDoor",
            WaitingFor::ChooseDungeon { .. } => "ChooseDungeon",
            WaitingFor::ChooseDungeonRoom { .. } => "ChooseDungeonRoom",
            WaitingFor::SpecializeColor { .. } => "SpecializeColor",
            WaitingFor::PayCost { .. } => "PayCost",
            WaitingFor::ActivationCostOneOfChoice { .. } => "ActivationCostOneOfChoice",
            WaitingFor::CostTypeChoice { .. } => "CostTypeChoice",
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
            WaitingFor::ChooseAnnouncingOpponent { .. } => "ChooseAnnouncingOpponent",
            WaitingFor::ClashCardPlacement { .. } => "ClashCardPlacement",
            WaitingFor::VoteChoice { .. } => "VoteChoice",
            WaitingFor::SeparatePilesChooseOpponent { .. } => "SeparatePilesChooseOpponent",
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
            WaitingFor::EachPlayerCopyChosenSelection { .. } => "EachPlayerCopyChosenSelection",
            WaitingFor::KeepWithinTotalPowerChoice { .. } => "KeepWithinTotalPowerChoice",
            WaitingFor::KeepExactPermanentsChoice { .. } => "KeepExactPermanentsChoice",
            WaitingFor::CopyRetarget { .. } => "CopyRetarget",
            WaitingFor::AssignCombatDamage { .. } => "AssignCombatDamage",
            WaitingFor::AssignBlockerDamage { .. } => "AssignBlockerDamage",
            WaitingFor::DistributeAmong { .. } => "DistributeAmong",
            WaitingFor::MoveCountersDistribution { .. } => "MoveCountersDistribution",
            WaitingFor::RemoveCountersChoice { .. } => "RemoveCountersChoice",
            WaitingFor::PayAmountChoice { .. } => "PayAmountChoice",
            WaitingFor::RetargetChoice { .. } => "RetargetChoice",
            WaitingFor::CombatTaxPayment { .. } => "CombatTaxPayment",
            WaitingFor::PhyrexianPayment { .. } => "PhyrexianPayment",
        }
    }

    /// Extract the player who must act, if any.
    ///
    /// CR 103.5: For simultaneous-decision states (`MulliganDecision`,
    /// `OpeningHandBottomCards`) this returns `Some(p)` only when exactly one
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
            WaitingFor::OpeningHandBottomCards { pending, .. } => {
                if pending.len() == 1 {
                    Some(pending[0].player)
                } else {
                    None
                }
            }
            WaitingFor::Priority { player }
            | WaitingFor::MeldPairChoice { player, .. }
            | WaitingFor::MeldAttackTargetChoice { player, .. }
            | WaitingFor::ManaPayment { player, .. }
            | WaitingFor::ChooseXValue { player, .. }
            | WaitingFor::TargetSelection { player, .. }
            | WaitingFor::DeclareAttackers { player, .. }
            | WaitingFor::DeclareBlockers { player, .. }
            | WaitingFor::UntapChoice { player, .. }
            | WaitingFor::ChooseUntapSubset { player, .. }
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
            | WaitingFor::ArrangePlanarDeckTopChoice { player, .. }
            | WaitingFor::RedistributeLifeTotals { player, .. }
            | WaitingFor::CoinFlipKeepChoice { player, .. }
            | WaitingFor::DigChoice { player, .. }
            | WaitingFor::SurveilChoice { player, .. }
            | WaitingFor::RevealChoice { player, .. }
            | WaitingFor::SearchChoice { player, .. }
            | WaitingFor::SearchPartitionChoice { player, .. }
            | WaitingFor::OutsideGameChoice { player, .. }
            | WaitingFor::ChooseFromZoneChoice { player, .. }
            | WaitingFor::BeholdChoice { player, .. }
            | WaitingFor::ChooseOneOfBranch { player, .. }
            | WaitingFor::LearnChoice { player, .. }
            | WaitingFor::ManifestDreadChoice { player, .. }
            | WaitingFor::EffectZoneChoice { player, .. }
            | WaitingFor::DrawnThisTurnTopdeckChoice { player, .. }
            | WaitingFor::TriggerTargetSelection { player, .. }
            | WaitingFor::BetweenGamesSideboard { player, .. }
            | WaitingFor::BetweenGamesChoosePlayDraw { player, .. }
            | WaitingFor::NamedChoice { player, .. }
            | WaitingFor::OpponentGuess { player, .. }
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
            | WaitingFor::ChooseRoomDoor { player, .. }
            | WaitingFor::ChooseDungeon { player, .. }
            | WaitingFor::ChooseDungeonRoom { player, .. }
            | WaitingFor::SpecializeColor { player, .. }
            | WaitingFor::PayCost { player, .. }
            | WaitingFor::ActivationCostOneOfChoice { player, .. }
            | WaitingFor::CostTypeChoice { player, .. }
            | WaitingFor::BlightChoice { player, .. }
            | WaitingFor::PayManaAbilityMana { player, .. }
            | WaitingFor::ChooseManaColor { player, .. }
            | WaitingFor::CollectEvidenceChoice { player, .. }
            | WaitingFor::HarmonizeTapChoice { player, .. }
            | WaitingFor::OptionalEffectChoice { player, .. }
            | WaitingFor::PairChoice { player, .. }
            | WaitingFor::OpponentMayChoice { player, .. }
            | WaitingFor::RespondToShortcut { player, .. }
            | WaitingFor::RespondToPrecastCopyShortcut { player, .. }
            | WaitingFor::TributeChoice { player, .. }
            | WaitingFor::UnlessPayment { player, .. }
            | WaitingFor::UnlessPaymentChooseCost { player, .. }
            | WaitingFor::RevealUntilKeptChoice { player, .. }
            | WaitingFor::RepeatDecision { player, .. }
            | WaitingFor::TopOrBottomChoice { player, .. }
            | WaitingFor::PopulateChoice { player, .. }
            | WaitingFor::ClashChooseOpponent { player, .. }
            | WaitingFor::ChooseAnnouncingOpponent { player, .. }
            | WaitingFor::ClashCardPlacement { player, .. }
            | WaitingFor::CompanionReveal { player, .. }
            | WaitingFor::ChooseLegend { player, .. }
            | WaitingFor::BattleProtectorChoice { player, .. }
            | WaitingFor::ProliferateChoice { player, .. }
            | WaitingFor::TimeTravelChoice { player, .. }
            | WaitingFor::AssistChoosePlayer { player, .. }
            | WaitingFor::ChooseObjectsSelection { player, .. }
            | WaitingFor::CategoryChoice { player, .. }
            | WaitingFor::EachPlayerCopyChosenSelection { player, .. }
            | WaitingFor::KeepWithinTotalPowerChoice { player, .. }
            | WaitingFor::KeepExactPermanentsChoice { player, .. }
            | WaitingFor::CopyRetarget { player, .. }
            | WaitingFor::AssignCombatDamage { player, .. }
            | WaitingFor::AssignBlockerDamage { player, .. }
            | WaitingFor::DistributeAmong { player, .. }
            | WaitingFor::MoveCountersDistribution { player, .. }
            | WaitingFor::RemoveCountersChoice { player, .. }
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
            | WaitingFor::SeparatePilesChooseOpponent { player, .. }
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
            // CR 732.2a: the loop-shortcut proposer is the player with priority, carried
            // in `proposer` (not a `player` field) — dedicated arm like `AssistPayment`.
            WaitingFor::LoopShortcut { proposer, .. } => Some(*proposer),
            WaitingFor::PrecastCopyShortcutOffer { proposer, .. } => Some(*proposer),
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
            | WaitingFor::CostTypeChoice { pending_cast, .. }
            | WaitingFor::BlightChoice { pending_cast, .. }
            | WaitingFor::HarmonizeTapChoice { pending_cast, .. }
            | WaitingFor::ChooseAnnouncingOpponent { pending_cast, .. } => Some(pending_cast),
            WaitingFor::PayCost { resume, .. } => match resume {
                CostResume::Spell {
                    spell: pending_cast,
                }
                | CostResume::SpellCost {
                    spell: pending_cast,
                    ..
                } => Some(pending_cast),
                CostResume::ManaAbility { .. } | CostResume::Resolution => None,
            },
            WaitingFor::CollectEvidenceChoice { resume, .. } => match resume.as_ref() {
                CollectEvidenceResume::Casting { pending_cast, .. } => Some(pending_cast),
                CollectEvidenceResume::Effect { .. }
                | CollectEvidenceResume::ManaAbility { .. } => None,
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
            | WaitingFor::CostTypeChoice { pending_cast, .. }
            | WaitingFor::BlightChoice { pending_cast, .. }
            | WaitingFor::HarmonizeTapChoice { pending_cast, .. }
            | WaitingFor::ChooseAnnouncingOpponent { pending_cast, .. } => Some(pending_cast),
            WaitingFor::PayCost { resume, .. } => match resume {
                CostResume::Spell {
                    spell: pending_cast,
                }
                | CostResume::SpellCost {
                    spell: pending_cast,
                    ..
                } => Some(pending_cast),
                CostResume::ManaAbility { .. } | CostResume::Resolution => None,
            },
            WaitingFor::CollectEvidenceChoice { resume, .. } => match resume.as_mut() {
                CollectEvidenceResume::Casting { pending_cast, .. } => Some(pending_cast),
                CollectEvidenceResume::Effect { .. }
                | CollectEvidenceResume::ManaAbility { .. } => None,
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
                | WaitingFor::ArrangePlanarDeckTopChoice { .. }
                | WaitingFor::SurveilChoice { .. }
                | WaitingFor::DigChoice { .. }
        )
    }

    pub fn accepts_freeform_counter_move_distribution(&self) -> bool {
        matches!(self, WaitingFor::MoveCountersDistribution { .. })
    }

    /// CR 107.1c: "Remove any number of counters" has a combinatorial legal
    /// space (any per-type subset 0..=available, including the empty set) that
    /// the coarse AI candidate enumerator (`counter_removal_candidates`, which
    /// offers only "remove all" and "remove none") cannot fully cover. The
    /// server bypasses its enumeration gate for this state so a human's
    /// intermediate submission (e.g. "remove 2 of 3") is not wrongly rejected;
    /// `apply()` (the `RemoveCountersChoice` handler) is the real validation
    /// boundary via `validate_counter_selection`.
    pub fn accepts_freeform_counter_removal(&self) -> bool {
        matches!(self, WaitingFor::RemoveCountersChoice { .. })
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

/// CR 102.1 + CR 500.1: which turn boundary ends an auto-pass session.
///
/// The session owner is compared against the active player (CR 102.1) at each
/// turn start (CR 500.1) to decide whether the session survives the boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
pub enum TurnBoundary {
    /// Clears at the next turn start (i.e. the end of the session owner's
    /// current turn), regardless of whose turn begins. Legacy behavior; the
    /// serde-migration default.
    #[default]
    EndOfCurrentTurn,
    /// Persists through intervening opponent turns; clears only when the
    /// session owner's own next turn begins (CR 102.1 active player == owner).
    MyNextTurnStart,
}

/// What the frontend requests for auto-pass (no internal state).
///
/// Phase stops that should interrupt a turn-boundary session are a separate
/// per-player preference on `GameState::phase_stops`, managed via
/// `GameAction::SetPhaseStops`. Keeping them out of the request preserves a
/// single source of truth and lets the preference change mid-session without
/// requiring a new auto-pass request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoPassRequest {
    UntilStackEmpty,
    /// Requests deserialize the same shape as the stored mode. Requests are
    /// transient (never persisted); the `alias`/`default` here is deploy-window
    /// insurance for FE/engine version skew, not a persistence requirement.
    #[serde(alias = "UntilEndOfTurn")]
    UntilTurnBoundary {
        #[serde(default)]
        until: TurnBoundary,
    },
}

/// What the engine stores for auto-pass (includes captured state).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AutoPassMode {
    /// Auto-pass while stack is non-empty. Clears when stack empties or grows
    /// beyond `initial_stack_len` (the stack size when the flag was set).
    UntilStackEmpty { initial_stack_len: usize },
    /// Auto-pass through priority/combat stops until the turn boundary in
    /// `until` is reached — `EndOfCurrentTurn` (the flag dies at the next turn
    /// start, regardless of whose turn it is) or `MyNextTurnStart` (persists
    /// through opponents' turns, dies only when the owner's next turn begins).
    /// Interrupted per-window by opponent stack activity (unless priority-
    /// yielded, CR 117.3d) or a scope-applicable `phase_stops` entry (see
    /// `GameState::phase_stop_hit`).
    ///
    /// `#[serde(alias = "UntilEndOfTurn")]` + `#[serde(default)]` on `until`
    /// migrate legacy persisted `{"type":"UntilEndOfTurn"}` payloads (IndexedDB/
    /// SQLite) forward to `UntilTurnBoundary { until: EndOfCurrentTurn }`.
    #[serde(alias = "UntilEndOfTurn")]
    UntilTurnBoundary {
        #[serde(default)]
        until: TurnBoundary,
    },
}

/// CR 732.2a: user-controllable gate for the live combo (infinite-loop) detector.
///
/// `Off` (the default) restores EXACT pre-detector behavior: the engine records
/// no loop-detection samples (no per-resolution `normalize_for_loop` clone), never
/// fires the mandatory-loop game-ending shortcut (CR 732.2a / CR 732.5 / CR 704.5a),
/// and never marks `∞` unbounded resources from a detected loop. `On` enables the
/// detector. New game-changing functionality is opt-in so it can be developed
/// safely (issue #4603).
///
/// This is a game-wide setting (the gated shortcut ends the whole game, so a
/// per-player flag would be meaningless). It is INTENTIONALLY a typed mode enum
/// rather than a `bool`, matching the engine's `*Mode` idiom (`AutoPassMode`,
/// `ConvokeMode`, `CastPaymentMode`) and leaving room for a future detect-only
/// mode (display `∞` without the game-ending shortcut). The debug
/// `DebugAction::SetInfiniteMana` toggle is a SEPARATE producer of
/// `unbounded_resources` and is NOT gated by this flag.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum LoopDetectionMode {
    /// Pre-feature behavior: no loop sampling, no shortcut, no detector `∞`.
    #[default]
    Off,
    /// Live combo-detector active: samples loops, fires the CR 732.2a mandatory-loop
    /// shortcut, and marks `unbounded_resources` for a confirmed loop.
    On,
    /// CR 732.2a/b/c: samples loops like `On`, but instead of only auto-winning a
    /// mandatory lethal drain it OFFERS the interactive loop-shortcut + runs the APNAP
    /// accept-or-shorten window for an OPTIONAL winning drain, and adds the CR 732.4
    /// all-mandatory net-progress no-loss DRAW. A mandatory winning drain still
    /// auto-wins exactly as `On` does. Opt-in / default stays `Off`. (Phase 4 reuses
    /// this same mode for B5's non-winning hold — one serialized-enum add, not two.)
    Interactive,
}

impl LoopDetectionMode {
    /// True when the live combo-detector is enabled (auto-lethal-win only).
    pub fn is_on(self) -> bool {
        matches!(self, LoopDetectionMode::On)
    }

    /// True when the detector is off (pre-feature behavior). Takes `&self` so it can
    /// serve as a serde `skip_serializing_if` predicate on `MatchConfig.loop_detection`.
    pub fn is_off(&self) -> bool {
        matches!(self, LoopDetectionMode::Off)
    }

    /// CR 732.2a: whether this mode populates the loop-detect ring and enters the
    /// reconcile shortcut block. Both `On` and `Interactive` sample; `Off` samples
    /// neither. Crucially `samples() == is_on()` for `Off` (false) and `On` (true), so
    /// swapping the two live gates from `is_on()` to `samples()` leaves the `Off` and
    /// `On` code paths byte-identical — only `Interactive` newly samples/enters.
    pub fn samples(self) -> bool {
        matches!(self, LoopDetectionMode::On | LoopDetectionMode::Interactive)
    }
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

/// CR 400.7j: from→to record of a source object moved by its own resolving
/// ability, so `source_is_current` can re-find it after the all-zone incarnation
/// bump. `original_stamp` is the incarnation the resolving ability captured (fixed
/// across chained self-moves); `current_incarnation` tracks the latest post-move
/// value. Bound to `original_stamp` so only the ability that captured the pre-move
/// identity relatches — a stale-stamped delayed trigger for the same `object_id`
/// cannot ride the record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolutionSourceRelatch {
    pub object_id: ObjectId,
    pub original_stamp: u64,
    pub current_incarnation: u64,
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

/// CR 702.xxx: Remaining Paradigm sources paused while copy-announcement
/// observer triggers drain (issue #3660). Resumed when priority next settles.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingParadigmRemainingOffers {
    pub player: PlayerId,
    pub offers: Vec<ObjectId>,
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
    /// CR 702.94a + CR 608.2g: Cast from hand via Miracle's alternative cost after
    /// revealing the card as the first card drawn this turn. This is a UNIT variant;
    /// the concrete miracle cost is NOT re-read from live keywords at cast time.
    /// Instead it is latched at offer-enqueue on `CastOfferKind::Miracle.cost` (a
    /// concrete `ManaCost::Cost` resolved by `draw.rs`) and threaded through
    /// `handle_cast_spell_as_miracle_with_payment_mode` →
    /// `prepare_spell_cast_with_variant_override_inner(latched_alt_cost)`. This
    /// preserves the offered cost even if the granting source (e.g. Aminatou) has
    /// left the battlefield between reveal-accept and trigger resolution
    /// (CR 608.2b last-known-information).
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
    /// CR 708.4 + CR 702.37c (Morph) / CR 702.168b (Disguise): Cast a card face
    /// down as a 2/2 face-down creature spell by paying a fixed {3} (an
    /// alternative cost, CR 601.2b) rather than its mana cost. Morph, Megamorph,
    /// and Disguise all cast identically this way — the only differences (ward {2}
    /// for Disguise per CR 702.168a; the turn-face-up cost) are read downstream
    /// from the hidden real card's keyword, so this variant is parameterless.
    ///
    /// Runtime: `casting::continue_cast_face_down` blanks the object to its
    /// face-down 2/2 (stashing the real card in `back_face`) BEFORE it is put on
    /// the stack (CR 708.4), so the whole downstream face-down machinery is
    /// inherited: `visibility` redacts the stack spell to opponents, the object
    /// resolves onto the battlefield still face down (CR 702.37c), and
    /// `GameAction::TurnFaceUp` (CR 702.37e) flips it. The face-down spell is an
    /// alternative cast (appears in `uses_alternative_cost`); it neither exiles
    /// on resolution nor restores a front face (the object simply stays face down),
    /// but CR 708.9 reveal-on-leave-stack is handled by the shared
    /// `apply_zone_exit_cleanup`, so it needs no `restores_front_face_after_stack_exit`.
    FaceDown,
}

impl CastingVariant {
    pub fn is_normal(&self) -> bool {
        *self == CastingVariant::Normal
    }

    /// CR 601.2a: The `ObjectId` of the `StaticMode::ExileCastPermission` source
    /// elected for this cast, when the variant is `ExilePermission`. The cast
    /// pipeline carries the elected source so per-source cost treatment
    /// (extra-cost riders) binds to the permission the player actually cast
    /// through — not whichever functioning source a battlefield scan reaches
    /// first when several permissions offer the same exiled spell.
    pub fn exile_permission_source(self) -> Option<ObjectId> {
        match self {
            CastingVariant::ExilePermission { source, .. } => Some(source),
            _ => None,
        }
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
            // CR 601.2b + CR 702.37c / CR 702.168b: casting face down pays a fixed
            // {3} rather than the printed mana cost — an alternative cost, so only
            // one alternative casting method may apply (CR 118.9a).
            | CastingVariant::FaceDown
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReplacementIndexEntry {
    pub id: ReplacementId,
    pub ordinal: usize,
}

/// CR 614.1: Derived candidate pre-filter for replacement effects. The index is
/// an optional acceleration over the legacy active-replacement scan: it stores
/// only `ReplacementId`s plus their legacy scan ordinal, never replacement
/// definitions, so consults re-read live object state and preserve CR 616 order.
#[derive(Debug, PartialEq, Eq)]
pub struct ReplacementIndex {
    pub initialized: bool,
    pub dirty: bool,
    pub pipeline_active: bool,
    pub by_event: im::HashMap<ReplacementEvent, im::Vector<ReplacementIndexEntry>>,
}

impl Default for ReplacementIndex {
    fn default() -> Self {
        Self {
            initialized: false,
            dirty: true,
            pipeline_active: false,
            by_event: im::HashMap::new(),
        }
    }
}

impl Clone for ReplacementIndex {
    fn clone(&self) -> Self {
        Self::default()
    }
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

/// CR 608.2: The resolution-scoped triggering-event context of an ability that
/// paused for an interactive `ChooseFromZoneChoice`. An ability's resolution is a
/// single, ongoing process (CR 608.2); when it parks on a player choice,
/// `stack::resolve_top` runs to completion and unconditionally clears the live
/// trigger context. These three values are exactly the inputs the
/// `EventContextAmount` ("that many") cascade in `game::quantity` consults, so
/// they are captured while still live and restored around the continuation drain
/// when the player answers — letting an `EventContextAmount` sub_ability
/// (Amy Pond: "choose a suspended card you own and remove that many time counters
/// from it") read the triggering event's amount after the pause. Building-block
/// generalization of the `pending_optional_trigger_event` /
/// `pending_optional_trigger_match_count` pair (The Ur-Dragon) and the
/// `WaitingFor::ChooseObjectsSelection` save/restore.
///
/// Mechanism map (as of the `PendingContinuation.trigger_context` fix):
/// - **Primary / generic** — `PendingContinuation.trigger_context` (this type)
///   preserves the trigger context across ANY continuation-based pause, and is
///   the mechanism to reach for going forward.
/// - **Narrower pre-existing #1** — `GameState.pending_choose_zone_trigger_context`
///   (this type), used only by `ChooseFromZoneChoice`.
/// - **Narrower pre-existing #2** — `WaitingFor::ChooseObjectsSelection.trigger_event`,
///   used only by that specific choice type.
///
/// The two narrower mechanisms solve the same conceptual problem for their
/// specific pause types and were deliberately NOT deleted or consolidated as
/// part of the `PendingContinuation` fix. Both remain independently functional,
/// giving redundant (not conflicting) protection to the pauses they cover —
/// those pauses ALSO route through `PendingContinuation`, so they are now
/// additionally covered by the primary mechanism. Consolidating all three onto
/// the single `PendingContinuation.trigger_context` path is a real, identified
/// follow-up opportunity, deliberately deferred here to keep that fix's blast
/// radius on the reported bug rather than bundling a deletion of currently
/// working code into a user-facing bugfix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvingTriggerContext {
    /// CR 608.2: The triggering event — source of `extract_amount_from_event`.
    pub event: Option<GameEvent>,
    /// CR 603.2c + CR 603.7c: the plural batched-trigger event list mirroring
    /// GameState::current_trigger_events — added so a batched trigger's
    /// plural-event-context reads during a drained continuation don't fall back
    /// to just the singular event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub events: Vec<GameEvent>,
    /// CR 603.2c: The firing trigger's filtered subject/occurrence count (the
    /// batched "that many"); outranks the event amount in the cascade.
    pub match_count: Option<u32>,
    /// CR 706.4: A die result recorded earlier in this resolution ("roll a die …
    /// remove that many counters"); outranks the event amount in the cascade.
    pub die_result: Option<i32>,
}

impl ResolvingTriggerContext {
    /// CR 608.2: Snapshot the live, resolution-scoped trigger context for
    /// later replay across an interactive pause. `None` when nothing is
    /// live (an activated ability or untriggered resolution has no context
    /// to preserve).
    pub(crate) fn capture(state: &GameState) -> Option<Self> {
        (state.current_trigger_event.is_some()
            || !state.current_trigger_events.is_empty()
            || state.current_trigger_match_count.is_some()
            || state.die_result_this_resolution.is_some())
        .then(|| Self {
            event: state.current_trigger_event.clone(),
            events: state.current_trigger_events.clone(),
            match_count: state.current_trigger_match_count,
            die_result: state.die_result_this_resolution,
        })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LiminalEntry {
    pub object: GameObject,
    pub name: String,
    pub source_id: ObjectId,
    pub controller: PlayerId,
    pub enters_attacking: bool,
    pub attach_to: Option<AttachTarget>,
    pub sacrifice_at: Option<Duration>,
    pub remaining_count: u32,
    pub created_ids: Vec<ObjectId>,
    pub copy_resume: Option<Box<CopyTokenSpec>>,
    pub spec_resume: Option<Box<TokenSpec>>,
    pub enter_tapped: EtbTapState,
    pub enter_with_counters: Vec<(CounterType, u32)>,
    #[serde(default)]
    pub kind: LiminalEntryKind,
    /// CR 614.5: applied replacement identities from the projected entry. Meld
    /// redirects seed both physical component moves with this shared set.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub replacement_applied: HashSet<AppliedReplacementKey>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub enum LiminalEntryKind {
    #[default]
    Token,
    Meld {
        context: MeldSelection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attack_target: Option<AttackTarget>,
    },
}

#[derive(Debug, Clone, Serialize)]
pub enum PendingLiminalEntryResume {
    Token {
        source_id: ObjectId,
        player: PlayerId,
        event: ProposedEvent,
    },
    Meld {
        source_id: ObjectId,
        player: PlayerId,
        context: MeldSelection,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        attack_target: Option<AttackTarget>,
    },
}

#[derive(Deserialize)]
enum TaggedPendingLiminalEntryResume {
    Token {
        source_id: ObjectId,
        player: PlayerId,
        event: ProposedEvent,
    },
    Meld {
        source_id: ObjectId,
        player: PlayerId,
        context: MeldSelection,
        #[serde(default)]
        attack_target: Option<AttackTarget>,
    },
}

#[derive(Deserialize)]
#[serde(untagged)]
enum PendingLiminalEntryResumeCompat {
    Tagged(TaggedPendingLiminalEntryResume),
    LegacyToken {
        source_id: ObjectId,
        player: PlayerId,
        event: ProposedEvent,
    },
}

impl<'de> Deserialize<'de> for PendingLiminalEntryResume {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Ok(
            match PendingLiminalEntryResumeCompat::deserialize(deserializer)? {
                PendingLiminalEntryResumeCompat::Tagged(
                    TaggedPendingLiminalEntryResume::Token {
                        source_id,
                        player,
                        event,
                    },
                )
                | PendingLiminalEntryResumeCompat::LegacyToken {
                    source_id,
                    player,
                    event,
                } => Self::Token {
                    source_id,
                    player,
                    event,
                },
                PendingLiminalEntryResumeCompat::Tagged(
                    TaggedPendingLiminalEntryResume::Meld {
                        source_id,
                        player,
                        context,
                        attack_target,
                    },
                ) => Self::Meld {
                    source_id,
                    player,
                    context,
                    attack_target,
                },
            },
        )
    }
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
    /// CR 723.1a: Creation timestamp of the player-control effect currently
    /// latched in `turn_decision_controller`. This remains independent from
    /// future scheduled effects that may replace the consumed schedule entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_decision_control_timestamp: Option<u64>,
    /// CR 723.1a: Identity of the full-turn player-control effect that is
    /// currently applicable. Kept separately from the winning decision latch
    /// so a newer phase-scoped effect can temporarily override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_full_turn_control: Option<ActivePlayerControl>,
    /// CR 723.1a + CR 723.2: Identity of the combat-phase player-control effect
    /// that is currently applicable. The newest applicable identity wins, and
    /// the full-turn identity resumes after this window ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_combat_phase_control: Option<ActivePlayerControl>,
    #[serde(default, skip_serializing_if = "ActiveLibrarySearches::is_empty")]
    pub active_library_searches: ActiveLibrarySearches,
    #[serde(
        default,
        skip_serializing_if = "ActiveSearchDecisionControls::is_empty"
    )]
    pub active_search_decision_controls: ActiveSearchDecisionControls,
    /// Viewer-filtering sidecar: the authoritative prompt submitter projected
    /// before private search-control records are stripped. Double Option keeps
    /// "not filtered" distinct from an authoritative multi/no-actor `None`.
    // Central object store. Uses FxBuildHasher (fast, deterministic) instead of
    // the default SipHash RandomState: ObjectId is a thin integer key and this
    // map is looked up millions of times per large-board resolution — profiling
    // showed SipHash hashing + HAMT lookup was ~35% of resolution CPU.
    #[serde(deserialize_with = "deserialize_objects_with_trigger_provenance")]
    pub objects: im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>,
    pub next_object_id: u64,
    /// CR 118.3a: monotonic counter minting `ManaPipId`s for pool units so they
    /// can be pinned. Serialized plainly (mirrors `next_object_id`) so reloaded
    /// games don't re-mint colliding ids.
    #[serde(default)]
    pub next_pip_id: u64,
    /// CR 118.3a: transient carrier for the caster's pin hints during a single
    /// finalize spend. `finalize_mana_payment` takes `pending_cast` (removing the
    /// pins) BEFORE the spend runs, so the pins are moved here for the duration of
    /// the spend and cleared immediately after. Never serialized and never part of
    /// state equality — it is empty outside the synchronous finalize window.
    #[serde(skip)]
    pub active_payment_pins: Vec<ManaPipId>,
    /// CR 601.2a: transient copy of the object-attached casting permission
    /// identity while finalization owns the `PendingCast` by value. Payment
    /// consults it only inside that synchronous window; it is never serialized.
    #[serde(skip)]
    pub active_casting_permission_index: Option<CastingPermissionIndex>,

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
    /// ChaCha20 stream position (word offset) captured at serialize time.
    /// `rng` is `#[serde(skip)]`, so without persisting the position a restored
    /// snapshot reseeds to offset 0 and replays the random sequence the game
    /// already consumed (issue #5466). Synced from `rng.get_word_pos()` before
    /// export and re-applied via `set_word_pos` on restore. Like `rng` itself it
    /// is excluded from `PartialEq` (mutable stream position, not identity);
    /// `#[serde(default)]` = 0 keeps pre-#5466 saves on today's from-origin
    /// (rewind) behavior.
    #[serde(default)]
    pub rng_word_pos: u128,
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
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub liminal_entries: HashMap<ObjectId, LiminalEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_liminal_entry_resume: Option<PendingLiminalEntryResume>,
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
    /// CR 614.6 + CR 615.5: Continuation effect to resolve after a
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
    #[serde(default, skip_serializing_if = "PostReplacementDrainStack::is_empty")]
    pub post_replacement_drains: PostReplacementDrainStack,

    /// Pre-2026-05-09 audit M4 compat: legacy template slot. Read from old
    /// JSON only; migrated into `post_replacement_drains` by
    /// `migrate_post_replacement_continuation`. Never written to.
    #[serde(default, skip_serializing, rename = "post_replacement_effect")]
    pub(crate) legacy_post_replacement_effect:
        Option<Box<crate::types::ability::AbilityDefinition>>,
    /// Pre-2026-05-09 audit M4 compat: legacy resolved slot. Read from old
    /// JSON only; migrated into `post_replacement_drains` by
    /// `migrate_post_replacement_continuation`. Never written to.
    #[serde(default, skip_serializing, rename = "post_replacement_resolved_effect")]
    pub(crate) legacy_post_replacement_resolved_effect:
        Option<Box<crate::types::ability::ResolvedAbility>>,

    /// Legacy flat save shape for the drain's companion values, superseded by the
    /// fields inside [`PostReplacementDrain`]. Read from old JSON only; folded
    /// into the resident drain by `migrate_post_replacement_continuation`.
    #[serde(default, skip_serializing, rename = "post_replacement_continuation")]
    pub(crate) legacy_post_replacement_continuation:
        Option<crate::types::ability::PostReplacementContinuation>,
    #[serde(default, skip_serializing, rename = "post_replacement_source")]
    pub(crate) legacy_post_replacement_source: Option<crate::types::identifiers::ObjectId>,
    #[serde(default, skip_serializing, rename = "post_replacement_applied")]
    pub(crate) legacy_post_replacement_applied: HashSet<AppliedReplacementKey>,
    #[serde(default, skip_serializing, rename = "post_replacement_event_source")]
    pub(crate) legacy_post_replacement_event_source: Option<crate::types::identifiers::ObjectId>,
    #[serde(default, skip_serializing, rename = "post_replacement_event_target")]
    pub(crate) legacy_post_replacement_event_target: Option<crate::types::ability::TargetRef>,

    /// CR 614.6 + CR 616.1: When an optional CreateToken replacement defers a
    /// `ChooseOneOf` post-effect (Jinnie Fay class), the chosen branch's token
    /// event must inherit the originating event's applied replacement ids so
    /// the same replacement cannot re-prompt on its own substitute tokens.
    ///
    /// Ownership: this seed is OWNED by the originating token-choice
    /// continuation and outlives every nested choice, every stashed sub-ability
    /// continuation, AND every repeat/repeat-until drain. It is seeded exactly
    /// once (`replacement.rs`, only when a `CreateToken` event is replaced by a
    /// token-choice continuation — Jinnie Fay-class), read by every token
    /// proposal (`effects/token.rs`), and cleared ONLY at true full-drain
    /// (`effects/mod.rs::drain_pending_continuation`: back at priority with no
    /// `pending_continuation`, no `pending_repeat_iteration`, AND no
    /// `pending_repeat_until`). The replacement pipeline and ChooseOneOf
    /// completion NEVER clear it — a branch may stash a token-bearing
    /// sub-ability or pause inside a repeat-until loop that drains only later
    /// via `resolve_ability_chain`, so clearing earlier wipes the seed before
    /// those later token proposals and re-prompts the originating token-choice
    /// replacement (issue #4886).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_token_choice_applied:
        Option<std::collections::HashSet<crate::types::proposed_event::AppliedReplacementKey>>,

    /// CR 614.1a: "that many" copy count for a `CopyTokenOf` substitution
    /// replacement (Moonlit Meditation). Seeded from the replaced
    /// `CreateToken` event's `count` when the substitution is accepted, read by
    /// `QuantityRef::EventContextAmount` (highest priority) while the
    /// substitution continuation resolves, and cleared at true full-drain — same
    /// transient, mid-resolution lifetime as `post_replacement_token_choice_applied`
    /// above (and, like it, excluded from `PartialEq`). `None` outside a
    /// copy-token substitution.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub post_replacement_token_substitution_count: Option<i32>,

    /// CR 701.50a + CR 614.5 + CR 616.1f: deferred connive link of a connive
    /// replacement whose leading draw parked a replacement-ordering choice. See
    /// `PendingConniveReentry`. Drained only by
    /// `engine_replacement::handle_replacement_choice` (accept and decline) —
    /// never by the shared zone-delivery tail. Transient; serde-skipped when None;
    /// `.take()`-cleared at drain.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_connive_reentry: Option<PendingConniveReentry>,

    /// CR 121.2 + CR 121.6b + CR 616.1g: draw instructions in flight, innermost
    /// last. See [`DrawSequenceStack`]. Every pause and resume of a multi-card
    /// draw addresses a frame here by [`DrawSequenceFrameId`]; the single resume
    /// authority is `effects::draw::resume_draw_sequence`.
    ///
    /// Replaced the single `pending_multi_draw` slot, which could not represent a
    /// nested instruction (CR 616.1g) — a substituted inner draw overwrote the
    /// outer frame and its remaining units were silently lost.
    #[serde(default, skip_serializing_if = "DrawSequenceStack::is_empty")]
    pub draw_sequences: DrawSequenceStack,

    /// Legacy save shape for the single in-flight multi-card draw, superseded by
    /// [`Self::draw_sequences`]. JSON only; migrated into the stack by
    /// [`Self::migrate_pending_multi_draw`]. Never written.
    #[serde(
        default,
        rename = "pending_multi_draw",
        skip_serializing_if = "Option::is_none"
    )]
    pub legacy_pending_multi_draw: Option<PendingMultiDraw>,

    /// CR 701.12c + CR 616.1: Tail of a life-total assignment that paused on a
    /// gain/loss replacement choice. Drained by `handle_replacement_choice` after
    /// the chosen replacement finishes, preserving the simultaneous snapshot's
    /// remaining deltas across the prompt boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_life_total_assignment: Option<PendingLifeTotalAssignment>,

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
    /// CR 614.1: Derived replacement-effect candidate index. Rebuilt from the
    /// legacy `active_replacements(state)` order before replacement pipeline
    /// entry, invalidated after every applied replacement, and ignored whenever
    /// dirty/uninitialized. `#[serde(skip, default)]` because it is pure derived
    /// acceleration and must not affect equality or serialized state.
    #[serde(skip, default)]
    pub replacement_index: ReplacementIndex,
    /// CR 611.2 + CR 613.1: Derived generator index for the layer gather.
    /// `#[serde(skip)]` derived state (like `trigger_index`/`layers_dirty`);
    /// reconstructed from `state.battlefield` + `state.command_zone` +
    /// per-object `static_definitions` at the top of every layer pass, and
    /// lazily on first consult after deserialize via the empty-index fallback.
    /// INTENTIONALLY omitted from `impl PartialEq for GameState` — derived state
    /// must not break AI-search dedup on semantically-identical positions.
    #[serde(skip)]
    pub static_source_index: StaticSourceIndex,
    /// O(1) presence index over `StaticModeKind` discriminants — "does any functioning
    /// static of kind K exist on the board?" Rebuilt wholesale from `game_functioning_statics`
    /// as a byproduct of the layers pipeline (`layers::refresh_static_mode_presence`), so it is
    /// exactly `.any(kind)` for every kind. Lets discriminant-only scan gates (e.g. the
    /// hexproof scans in `static_abilities`) skip an O(battlefield) `.any()` when zero statics
    /// of that kind exist.
    ///
    /// DERIVED CACHE — `#[serde(skip)]` with an `all_present` default: before the first flush
    /// makes the index precise, every consumer must fall through to its exact per-object check,
    /// so the conservative all-true default can only cost a redundant scan, never miss a grant.
    /// INTENTIONALLY omitted from `impl PartialEq for GameState` — CR 104.4b loop detection
    /// compares semantic board state; derived caches must not perturb loop-detection equality
    /// (like `static_source_index`/`static_gate_truth`).
    #[serde(
        skip,
        default = "crate::types::statics::StaticModePresence::all_present"
    )]
    pub static_mode_presence: crate::types::statics::StaticModePresence,
    /// CR 732.2a loop-shortcut detection ring (PR-3). A bounded FIFO of recent
    /// post-resolution NORMALIZED board snapshots, captured at the post-pipeline frame
    /// of `game::engine::pass_priority_once_with_pipeline` (after
    /// `run_post_action_pipeline` places refilling triggers, CR 603.3) and scanned at
    /// the SBA-reconciliation seam (`game::engine::reconcile_terminal_result`). A
    /// self-refilling MANDATORY cascade drives the engine one resolution per `apply()`
    /// with no call-local window (the per-beat single-apply drive), so the window that
    /// detects the loop MUST persist across `apply()` calls — hence on `GameState`.
    ///
    /// TRANSIENT DERIVED STATE — `#[serde(skip, default)]`. It is never serialized: it
    /// is rebuilt deterministically from play and is a pure optimization over the
    /// existing CR 704.5a SBA (which already ends every realistic-life drain), so
    /// losing it across a save/load/MP-snapshot boundary only defers the shortcut by a
    /// few resolutions — never changes a winner. Snapshots are `Arc`-shared so the
    /// frequent `GameState::clone` (AI search, §9 probes) pays O(ring.len()) refcount
    /// bumps, not deep copies. INTENTIONALLY omitted from `impl PartialEq for GameState`
    /// (derived state, like `static_source_index`/`static_gate_truth` — both
    /// `#[serde(skip)]` AND eq-excluded; NOT `public_state_dirty`/`state_revision`/
    /// `layers_dirty`, which are `serde(skip)` but ARE compared in `eq`) so AI-search
    /// dedup on semantically-identical positions is unaffected.
    #[serde(skip, default)]
    pub loop_detect_ring: std::collections::VecDeque<std::sync::Arc<GameState>>,
    /// Live-only authority for the finite pre-cast shortcut. It is absent from
    /// raw/public serialization; trusted persistence uses the explicit codec
    /// envelope in `game::precast_copy_shortcut`.
    #[serde(skip, default)]
    pub(crate) precast_shortcut_runtime: PrecastShortcutRuntime,
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

    /// CR 613.1d: Remote recipients whose live card types were derived by a
    /// Layer-4 continuous effect during the preceding evaluation. The next
    /// full pass restores only these objects' type baselines before applying
    /// the new Layer-4 effect set, leaving independent spell/card state (such
    /// as cast-time ability grants) untouched.
    ///
    /// This is an engine-only derived cache. It is reconstructed from the
    /// serialized attribution side-table on the first post-deserialization
    /// layer pass, then rebuilt directly by the layer application pipeline.
    /// It is intentionally excluded from equality like the other layer caches.
    #[serde(skip, default)]
    pub(crate) remote_type_layer_recipients: im::HashSet<ObjectId>,

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

    /// CR 603.3b: PhaseChanged occurrences whose delayed triggers were merged
    /// into a simultaneous normal-trigger ordering batch before priority. The
    /// generic delayed-trigger pass filters these exact occurrences so the same
    /// delayed ability is not dispatched again. Transient engine coordination,
    /// cleared at action/pipeline boundaries.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub consumed_before_priority_trigger_events:
        Vec<crate::game::triggers::ConsumedTriggerEventOccurrence>,

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

    /// CR 120.3 + CR 120.6 + CR 702.11b: Battlefield objects that have actually
    /// dealt damage (combat or noncombat) since entering the battlefield. Sticky
    /// per-object flag backing the `StaticCondition::SourceHasDealtDamage`
    /// predicate (e.g. "has hexproof if it hasn't dealt damage yet"). Set only
    /// when a nonzero amount of damage is actually dealt (CR 120.3/120.6, not the
    /// would-deal amount of CR 120.1a); cleared when the object leaves the
    /// battlefield so a flickered object starts with a clean slate.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub objects_that_dealt_damage: HashSet<ObjectId>,

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

    /// CR 614.10 + CR 614.10a: Per-player turn-scoped combat-phase skip state.
    /// Drives "skips all combat phases of their next turn" (False Peace / Empty
    /// City Ruse). Unlike `steps_to_skip` (a finite per-step counter), this is a
    /// turn-bound marker resolved as a virtual replacement effect on every combat
    /// phase of the bound turn.
    ///
    /// State machine (per active player, advanced in `start_next_turn`):
    /// - `Pending`: set when the skip effect resolves; the skip is *armed* but
    ///   has not yet attached to a turn. Per CR 614.10a it waits past any
    ///   skipped turns and binds to the player's first non-skipped turn.
    /// - `Active`: promoted from `Pending` once the player's bound (non-skipped)
    ///   turn actually begins. While `Active`, the replacement layer prevents
    ///   every combat phase that turn (including extra combat phases).
    /// - `None`: cleared at the start of the player's *following* turn, after the
    ///   bound turn has ended. A turn that was `Active` becomes `None`.
    #[serde(default)]
    pub combat_phase_skip_next_turn: Vec<CombatPhaseSkipState>,
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

    /// CR 500.8 + CR 501.1: LIFO stack of anchor phases for inserted beginning
    /// phases (Temple of Atropos, Sphinx/Shadow of the Second Sun, Cyclonus)
    /// currently in progress. When such a phase's draw step ends, the turn
    /// resumes at the anchor's natural successor (or runs the next queued
    /// beginning phase for the same anchor) rather than at the draw step's
    /// default successor. Empty outside inserted beginning phases.
    /// `#[serde(default)]` so saved games load unchanged.
    #[serde(default)]
    pub extra_phase_resume: Vec<Phase>,

    /// CR 103.1: The current turn-order direction. Durable — persists across
    /// turns until an effect reverses it again. Default `Normal` is the game's
    /// clockwise turn order (CR 103.1). `#[serde(default)]` for save compat.
    #[serde(default)]
    pub turn_direction: TurnDirection,

    /// CR 508.1c + CR 506.1: When the current combat phase was scheduled with an
    /// attacker restriction (Last Night Together / Bumi), only creatures matching
    /// this filter may be declared as attackers. Set on entering that
    /// BeginCombat, cleared at end of combat (CR 511.3). `None` during ordinary
    /// (unrestricted) combats.
    #[serde(default)]
    pub current_combat_attacker_restriction: Option<TargetFilter>,
    /// CR 611.2c: The source `ObjectId` of the effect that imposed
    /// `current_combat_attacker_restriction`. Propagated from `ExtraPhase` so
    /// `passes_combat_attacker_restriction` can build a correct `FilterContext`
    /// for source-relative restriction predicates. `None` when there is no
    /// active restriction.
    #[serde(default)]
    pub current_combat_attacker_restriction_source: Option<ObjectId>,

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

    /// Per-player phase-stop preferences. While a player's `UntilTurnBoundary`
    /// auto-pass session is active, the engine will interrupt auto-pass whenever
    /// the current phase appears in that player's list and its scope applies on
    /// the current turn. Also consulted when deciding whether to auto-submit
    /// empty blockers during Declare Blockers, so users can pause the step to
    /// activate instants / Ninjutsu even when no legal blockers exist. Each stop
    /// carries a `PhaseStopScope` (all turns / own turn / opponents' turns),
    /// resolved against `active_player` (CR 102.1) by `phase_stop_hit`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub phase_stops: HashMap<PlayerId, Vec<PhaseStop>>,

    /// CR 605.3: Lands manually tapped for mana via TapLandForMana this priority window.
    /// Per-player map enables multiplayer correctness (e.g., UnlessPayment opponent tapping).
    /// Cleared on priority pass, cast, non-mana action, or phase transition.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lands_tapped_for_mana: HashMap<PlayerId, Vec<ObjectId>>,

    /// CR 103.5 + 103.5b: Per-player ledger of bottom cards already put on the
    /// bottom of the library toward the current mulligan obligation. This is
    /// the single source of truth for "how many bottoms has this player
    /// already paid at their current mulligan count". Discipline:
    /// - Reset to 0 on every `MulliganChoice::Mulligan` — a fresh redraw
    ///   invalidates any prior credit (the obligation for the new count starts
    ///   from scratch, CR 103.5).
    /// - Accumulated (never reset) across repeated `UseSerumPowder` uses at the
    ///   same mulligan count, so cycling Serum Powder never double-charges an
    ///   already-paid obligation (CR 103.5b + Serum Powder Oracle text).
    /// - Also credited by the Tiny Leaders forced opening-hand bottom
    ///   (TL:R 906.6a) so that first bottom is not charged twice.
    ///
    /// Cleared when the mulligan flow finishes (all players out of `pending`).
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

    /// Per-controller set of resource axes a detected/forced unbounded loop pumps,
    /// the engine-authoritative source for the `∞` HUD projection (`derive_views`)
    /// and the byte-preserved infinite-mana refill/keep gates. The infinite-mana
    /// debug toggle (`DebugAction::SetInfiniteMana`) is one producer: it records
    /// the six `ResourceAxis::Mana(_)` axes (`INFINITE_MANA_AXES`) for the player,
    /// which the `mana_payment::refill_infinite_mana` top-up and the
    /// `turns` end-of-step keep gate read (CR 500.5 suppressed for that player
    /// only — a debug-only departure from the rules). Written ONLY through
    /// `mark_unbounded_loop` / `clear_unbounded_loop`.
    ///
    /// INTENTIONALLY EXCLUDED from `PartialEq`, `normalize_for_loop`, and
    /// `loop_fingerprint` (same family as `static_gate_truth` /
    /// `devour_eligible_snapshot`): this is display/annotation state, not rules
    /// state for equality. CR 104.4b/CR 732.2a loop detection (`loop_states_equal`)
    /// and AI-search position dedup compare two states reached at different times;
    /// a populated live state must still compare equal to the empty-`unbounded_resources`
    /// ring snapshots, or loop detection yields false negatives. (`debug_infinite_mana`
    /// relied on this same exclusion implicitly; it is now explicit.)
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unbounded_resources: BTreeMap<PlayerId, BTreeSet<ResourceAxis>>,

    /// CR 104.4b / CR 732.2a: for each controller with a marked revocable-∞ capability
    /// (`unbounded_resources`), the set of battlefield permanents whose presence enables the
    /// loop (the stable recurring board: `battlefield_ids(prior) ∩ battlefield_ids(state)`).
    /// Populated ONLY by the Interactive B5 bridge arm (`interactive_loop_bridge` Path C);
    /// the `apply_zone_exit_cleanup` defuse hook clears the whole capability when ANY member
    /// leaves the battlefield (CR 110.1 / CR 700.4).
    ///
    /// INTENTIONALLY EXCLUDED from `PartialEq`, `normalize_for_loop`, and `loop_fingerprint`
    /// (same family as `unbounded_resources`): revocation annotation, not rules state for
    /// equality — a populated live state must still compare equal to the empty-enabler ring
    /// snapshots, or loop detection yields false negatives.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub unbounded_loop_enablers: BTreeMap<PlayerId, BTreeSet<ObjectId>>,

    /// Oracle ids (fallback: object names) of cards whose abilities hit
    /// `Effect::Unimplemented` at resolution this game. Diagnostics only —
    /// records *runtime resolution hits*, is game-scoped, and survives zone
    /// changes. This is distinct from the per-object `unimplemented_mechanics`
    /// (`game/coverage.rs`), which is a static parse-coverage projection; this
    /// accumulator is the telemetry `game_summary` surface. Not a duplication.
    ///
    /// INTENTIONALLY EXCLUDED from `PartialEq`, `normalize_for_loop`, and
    /// `loop_fingerprint` (same family as `unbounded_resources`): this is
    /// diagnostics/annotation state, not rules state for equality. CR 104.4b /
    /// CR 732.2a loop detection compares two states reached at different times;
    /// a populated live state must still compare equal to snapshots taken
    /// before the unimplemented effect resolved, or loop detection yields false
    /// negatives.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub unimplemented_oracle_ids: BTreeSet<String>,

    /// Descriptors (source name + dead stack-entry id) of push-first triggered
    /// abilities whose in-construction stack entry vanished before mode/target/
    /// division selection completed, forcing the engine to abandon construction
    /// (`triggers::abandon_ceased_pending_trigger`). This records recovery from
    /// an UNIDENTIFIED state-coherence defect: the push-first construction cursor
    /// (`pending_trigger_entry`) was left dangling by some upstream path, so the
    /// completion action could not find its entry. Diagnostics only — game-scoped,
    /// this is the telemetry `game_summary` surface for the (previously
    /// engine-panicking) recovery. A `Vec`, not a set: the raw occurrence COUNT
    /// matters — ~6 hits/night in production before the panic was made
    /// recoverable — so repeated abandons must not be deduplicated away.
    ///
    /// INTENTIONALLY EXCLUDED from `PartialEq`, `normalize_for_loop`, and
    /// `loop_fingerprint` (same family as `unimplemented_oracle_ids` /
    /// `unbounded_resources`): this is diagnostics/annotation state, not rules
    /// state for equality. CR 104.4b / CR 732.2a loop detection compares two
    /// states reached at different times; a populated live state must still
    /// compare equal to snapshots taken before the abandon, or loop detection
    /// yields false negatives.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_trigger_abandons: Vec<String>,

    /// CR 732.2a: per-game runtime gate for the live combo (infinite-loop) detector.
    /// Default `Off` = exact pre-combo-detector behavior. This is the hot-path flag the
    /// detector gates read; it is PROJECTED from the immutable [`MatchConfig::loop_detection`]
    /// by [`GameState::set_match_config`] at game creation and is NOT mutated mid-game —
    /// there is no `GameAction` that flips it, so no seat can opt the match in or out
    /// during play. Game-wide because the gated shortcut ends the whole game; chosen at
    /// match creation, whole-table by construction. See [`LoopDetectionMode`].
    ///
    /// INTENTIONALLY EXCLUDED from `impl PartialEq for GameState` and
    /// `loop_fingerprint` (same family as `unbounded_resources`): this is
    /// control/display state, not rules state for equality. It is invariant across
    /// the snapshots CR 732.2a loop detection compares (a player cannot toggle it
    /// mid-loop), and AI-search dedup ignores it (the AI never reads the detector),
    /// so excluding it cannot cause a false loop match or a missed position dedup.
    #[serde(default)]
    pub loop_detection: LoopDetectionMode,

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

    // Trigger constraint tracking keyed by exact source incarnation + occurrence.
    #[serde(default)]
    pub triggers_fired_this_turn: HashSet<TriggerDefinitionRef>,
    /// CR 603.4: Per-trigger fire counts for MaxTimesPerTurn constraint.
    /// Tracks how many times each exact occurrence has fired this turn.
    #[serde(
        default,
        skip_serializing_if = "HashMap::is_empty",
        with = "trigger_definition_ref_map"
    )]
    pub trigger_fire_counts_this_turn: HashMap<TriggerDefinitionRef, u32>,
    /// CR 603.2: Tracks per-opponent-per-turn firing for
    /// OncePerOpponentPerTurn. Keyed by exact occurrence and opponent.
    #[serde(default)]
    pub triggers_fired_this_turn_per_opponent: HashSet<(TriggerDefinitionRef, PlayerId)>,
    #[serde(default)]
    pub triggers_fired_this_game: HashSet<TriggerDefinitionRef>,
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
    pub crew_activated_this_turn: HashSet<ObjectIncarnationRef>,
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
    /// CR 701.26 + CR 603.4: Count of times each object became tapped this turn,
    /// keyed by object id. Populated at the central `GameEvent::PermanentTapped`
    /// observer (the same sink that records damage), so combat, effect, and crew
    /// taps all count. Cleared at turn start. A value of 1 means "first time this
    /// turn" — the count model (not a HashSet) keeps the CR 603.4 resolution-time
    /// re-check of `FirstTimeObjectTappedThisTurn` correct.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub object_tap_count_this_turn: std::collections::HashMap<ObjectId, u32>,
    /// CR 122.1 + CR 603.4: Count of distinct counter-placement *occurrences*
    /// (put-events, not records) on each object this turn, keyed by object id.
    /// Bumped once per object per batch in `observe_object_counter_placements`
    /// (the same `collect_pending_triggers` chokepoint as the tap sibling), so a
    /// multi-KIND placement — which pushes multiple `CounterAdded` records/events
    /// for one object — counts as ONE occurrence. Cleared at turn start. A value
    /// of 1 means "first time this turn"; the count model (vs. a set) keeps the
    /// CR 603.4 resolution-time re-check of
    /// `FirstTimeObjectCountersAddedThisTurn` correct. Deliberately EXCLUDED from
    /// `impl PartialEq` (mirrors `object_tap_count_this_turn`): a per-turn
    /// observational counter must not perturb board-equality.
    #[serde(default, skip_serializing_if = "std::collections::HashMap::is_empty")]
    pub object_counter_placement_count_this_turn: std::collections::HashMap<ObjectId, u32>,
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
    /// CR 118.9 + CR 601.2b: Tracks which once-per-turn `CastWithAlternativeCost`
    /// grant sources (As Foretold) have already had their alternative cost applied
    /// to a spell this turn. Keyed by the granting permanent's ObjectId. Unlimited
    /// grants (Fist of Suns, Rooftop Storm, Jodah) never populate this.
    /// CR 400.7: Zone change creates a new ObjectId, so the permission naturally
    /// resets when the source leaves and returns.
    #[serde(default)]
    pub alt_cost_grant_permissions_used: HashSet<ObjectId>,
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
    /// CR 601.2a + CR 401.5: Tracks `OncePerTurn`
    /// `StaticMode::TopOfLibraryCastPermission` sources that have already had a
    /// spell cast through them this turn (Assemble the Players, Johann,
    /// Apprentice Sorcerer — "Once each turn, you may cast … from the top of
    /// your library"). Keyed by the granting permanent's ObjectId. `Unlimited`
    /// frequency permissions (Realmwalker, Future Sight, Bolas's Citadel) never
    /// populate this set. Cleared at the start of each turn alongside the other
    /// per-turn cast-permission slots.
    /// CR 400.7: Zone change creates a new source `ObjectId`, naturally
    /// resetting the slot when the source leaves and re-enters play.
    #[serde(default)]
    pub top_of_library_cast_permissions_used: HashSet<ObjectId>,
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
    /// CR 702.xxx: Paradigm sources still owed after a targeted copy's
    /// `CopyRetarget` finalization paused on deferred copy observers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_paradigm_remaining_offers: Option<PendingParadigmRemainingOffers>,
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
    /// CR 500.8 + CR 513.1: Number of end steps that have begun this turn.
    /// Mirrors `combat_phases_started_this_turn` for the end-step axis; used by
    /// conditions that gate a follow-up only during the first end step
    /// (Y'shtola Rhul's "if it's the first end step of the turn" loop guard).
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub end_steps_started_this_turn: u32,
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
    /// CR 603.2c: Batched zone-change triggers already collected for
    /// `(definition_ref, turn_zone_change_index)`. Prevents a second
    /// `process_triggers` pass over the same `ZoneChanged` events from
    /// stacking duplicate batched triggers (issue #3866) without suppressing a
    /// later distinct leave by the same object in the same turn.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub batched_zone_change_trigger_fired: HashSet<(TriggerDefinitionRef, usize)>,
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

    /// CR 303.4f: Attach host captured before SearchChoice replaces parent targets.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search_continuation_attach_host: Option<AttachTarget>,

    /// CR 608.2c + CR 109.5: Pending `repeat_for` iteration loop paused mid-flight
    /// because the inner effect entered an interactive `WaitingFor` state.
    /// Drained by `drain_pending_continuation` AFTER `pending_continuation`,
    /// so the per-iteration chain (e.g., the SearchLibrary's
    /// "put-onto-battlefield" continuation) completes before the next
    /// iteration begins. See [`PendingRepeatIteration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_repeat_iteration: Option<PendingRepeatIteration>,

    /// CR 603.12a + CR 608.2c: A repeated-optional-payment process (Hawkeye,
    /// Master Marksman — "you may pay {1} up to three times. When you do, choose
    /// up to that many.") paused for one of its per-iteration payment decisions.
    /// Carries the PayCost-only unit, the reflexive modal to resolve once after
    /// the loop, and the remaining payment budget. Driven by
    /// `resolve_repeated_optional_payment_choice` on each `DecideOptionalEffect`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_repeated_optional_payment: Option<Box<PendingRepeatedOptionalPayment>>,

    /// CR 614.12b + CR 614.1c + CR 614.13: Pending multi-target `ChangeZone`
    /// iteration loop paused mid-flight because one of the moving objects
    /// triggered a per-permanent replacement choice. Drained by
    /// `drain_pending_continuation` BEFORE `pending_repeat_iteration` so the
    /// inner ChangeZone iteration completes (and its `EffectResolved` event
    /// fires) before the outer repeat loop advances. See
    /// [`PendingChangeZoneIteration`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_change_zone_iteration: Option<PendingChangeZoneIteration>,

    /// CR 608.2c: The single object whose move paused the active
    /// `pending_change_zone_iteration` on a per-permanent replacement CHOICE
    /// (`ZoneMoveResult::NeedsChoice`), paired with its pre-move zone. Unlike the
    /// `remaining` members, this object is delivered out-of-band by the
    /// replacement resume (not by the iteration drain), so the drain would
    /// otherwise never count it toward `moved_count`. The drain consumes this at
    /// its top and increments the carried count iff the object actually reached
    /// the iteration's destination — so a downstream "that many" includes the
    /// object that prompted the replacement. Pause/resume is strictly sequential,
    /// so at most one object is ever in flight (set on the pause, taken on the
    /// next drain pass).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_change_zone_in_flight: Option<(ObjectId, crate::types::zones::Zone)>,

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

    /// CR 101.4 + CR 616.1: Deferred resume state for `EachPlayerCopyChosen` when
    /// the current player's inner token copy OR its +1/+1 counter placement
    /// paused on a replacement choice. Drained by
    /// `each_player_copy_chosen::drain_pending` after the copy/counter drains in
    /// `engine_replacement.rs`, once state is back at Priority.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_each_player_copy_chosen: Option<PendingEachPlayerCopyChosen>,

    /// CR 705.1 + CR 614.1a: Pending multi-flip coin resolver paused mid-loop
    /// for a Krark's Thumb keep-1 choice. Stashes the full resolution context +
    /// loop position so `resume_after_keep` can re-enter the flip loop after the
    /// player's `CoinFlipKeepChoice`. See [`PendingCoinFlip`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_coin_flip: Option<PendingCoinFlip>,

    /// CR 705.2: Result of the most recent coin flip in the current resolution,
    /// carrying the flipper so `AbilityCondition::CoinFlipOutcome` is
    /// controller-relative. Written by the flip authority and read when a
    /// `RepeatContinuation::WhileCondition` loop re-evaluates ("if you lose the
    /// flip, repeat this process"). Resolution-scoped like `last_revealed_ids`:
    /// cleared at top-level `resolve_ability_chain` entry (CR 608.2c — the
    /// authoritative resolution-lifetime boundary) so a stale flip from a prior
    /// resolution can never satisfy a later gate.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_coin_flip: Option<ResolutionCoinFlip>,

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
    /// CR 701.38d + CR 608.2c: Per-ballot vote iteration paused by an
    /// interactive choice. Drained after `pending_change_zone_iteration` and
    /// before `pending_repeat_iteration`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_vote_ballot_iteration: Option<PendingVoteBallotIteration>,
    /// CR 101.4 + CR 608.2c: Per-player `ChooseFromZone { EachPlayer }`
    /// iteration paused by the current player's interactive choice. Drained
    /// alongside `pending_vote_ballot_iteration`, BEFORE `pending_continuation`
    /// runs, so every player's graveyard pick accumulates into the chain's
    /// tracked set before "put those cards onto the battlefield" resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_per_player_zone_choice: Option<PendingPerPlayerZoneChoice>,
    /// CR 101.4: If players make choices for one instruction, they choose in
    /// APNAP order before the simultaneous action happens.
    /// CR 701.21a: To sacrifice a permanent, its controller moves it from the
    /// battlefield to its owner's graveyard.
    /// Per-player sacrifice choices paused by the current player's
    /// `EffectZoneChoice`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_player_scope_sacrifice_choice: Option<PendingPlayerScopeSacrificeChoice>,
    /// CR 101.4 + CR 701.23i: Pending private selections for a simultaneous
    /// scoped self-library search. Kept separate from the generic continuation
    /// so the action phase cannot begin before every player has chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_scoped_library_search: Option<PendingScopedLibrarySearch>,
    /// Typed cleanup waiting for an ordinary search's leading ChangeZone to
    /// settle. The chain walker consumes it synchronously; the paused
    /// ChangeZone drain consumes the same value after its final member lands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_library_search_delivery: Option<LibrarySearchDeliveryResume>,
    /// CR 616.1: search-found replacement batch parked across a choice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_search_found_batch: Option<PendingSearchFoundBatch>,
    /// CR 608.2c + CR 105.1 / CR 205.2a: Per-category-member
    /// `Effect::ForEachCategoryExile` iteration paused by the current member's
    /// interactive choice ("for each color/card type, you may exile a card of
    /// that color/type"). Drained alongside `pending_per_player_zone_choice`,
    /// BEFORE `pending_continuation` runs, so every member's pool pick
    /// accumulates into the chain's tracked set before a downstream
    /// "from among them" clause resolves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_per_category_zone_choice: Option<PendingPerCategoryZoneChoice>,

    /// CR 122.5: Pending atomic counter moves selected during a resolution-time
    /// distribution prompt. Drained before normal pending continuations so
    /// replacement choices inside a move resume the remaining selected moves.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_counter_moves: Option<PendingCounterMoveQueue>,

    /// CR 107.1c + CR 608.2h: Pending per-type counter removals selected during a
    /// "remove any number of counters" resolution-time prompt. Drained before
    /// normal pending continuations (so a "create that many" rider sees the
    /// stamped `last_effect_count`), and re-parked when a per-removal replacement
    /// surfaces a `ReplacementChoice`. See [`PendingCounterRemovalQueue`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_counter_removals: Option<PendingCounterRemovalQueue>,

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

    /// CR 701.34a + CR 614.1a: Remaining proliferate actions after a count-
    /// modifying replacement (Tekuthal class). Resumed after each
    /// `ProliferateChoice` completes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_proliferate_actions: Option<PendingProliferateActions>,

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

    /// CR 608.2 + CR 603.2c + CR 706.4: The resolution-scoped trigger context of
    /// an ability that paused for an interactive `ChooseFromZoneChoice`, captured
    /// while still live (before `stack::resolve_top` clears it) and restored
    /// around the continuation drain in the `ChooseFromZoneChoice` handler so an
    /// `EventContextAmount` ("that many") sub_ability resolves the triggering
    /// event's amount after the pause (Amy Pond). Set on every single-pool
    /// `ChooseFromZone` raise (`None` for non-trigger ChooseFromZone) and consumed
    /// by `.take()` in the handler, so it never persists beyond one round-trip.
    /// It must survive the pause→answer action boundary, so it is intentionally
    /// NOT in the `apply()`-top transient clear. Building-block generalization of
    /// `pending_optional_trigger_event` / `pending_optional_trigger_match_count`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_choose_zone_trigger_context: Option<ResolvingTriggerContext>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub may_trigger_auto_choices: Vec<MayTriggerAutoChoiceRecord>,

    /// CR 603.3b (TriggerOrdering) / CR 732.2a (LoopChoice): captured recurring
    /// decisions (PR-7). Two lifetimes share this Vec, distinguished by their
    /// `key.sources` variant (invariant): an **ephemeral** template is keyed with
    /// all-`ThisObject` sources (the per-batch CR 603.3b coverage marker,
    /// registered mid-batch and cleared before the next Priority frame), a
    /// **persistent** template is keyed with `AllCopies` sources (a saved
    /// player-ordering preference that survives across batches / loop iterations,
    /// CR 704.5d). Excluded from `loop_fingerprint` (mid-batch ephemerals never
    /// reach a Priority sample; persistent templates are identical across
    /// iterations) but kept IN `PartialEq` — the safe direction.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub decision_templates: Vec<crate::analysis::decision_template::DecisionTemplate>,

    /// CR 117.3d: Standing per-player decisions to auto-pass priority while a
    /// matching triggered ability is on the stack (a "yield"). Preference state,
    /// so it persists across turns and is exempt from the auto-pass session
    /// clearing; cleared only by explicit revoke/clear-all.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub priority_yields: Vec<PriorityYield>,

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

    /// CR 701.42b: canonical physical meld pairs derived from the loaded card
    /// database. Key is `lowercase(source) + NUL + lowercase(partner)`.
    #[serde(skip)]
    pub meld_pair_registry: Arc<HashMap<String, MeldPairRecord>>,

    /// Momir Basic selection index: mana value -> sorted creature face names.
    /// CR 707.2 + CR 202.3: the random-token pool, keyed by mana value so the
    /// emblem's `{X}` ability can pick a creature with mana value X. Built only
    /// when `format == Momir` (see `rehydrate_card_db_metadata`); empty
    /// otherwise. Skipped in serialization and rebuilt deterministically per peer
    /// from the loaded card DB.
    #[serde(skip)]
    pub momir_pool: BTreeMap<i32, Vec<String>>,

    /// Momir Basic hydration map: lowercase creature name -> `CardFace`. The
    /// resolver reads this (NEVER `card_face_registry`, which is conjure-scoped
    /// and misses most creatures) to build the copy token. Skipped in
    /// serialization; rebuilt with `momir_pool`.
    #[serde(skip)]
    pub momir_pool_faces: Arc<HashMap<String, CardFace>>,

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

    /// CR 401.5 + CR 608.2c + CR 609.3 + issue #4950: Set when the most
    /// recently resolved `Dig`/`ChooseFromZone`/`RevealHand` reveal-choice
    /// came up with nothing (empty library, no eligible card, or an empty
    /// reveal-choice set respectively) — distinct from "none of those has run
    /// in this chain link," which is `None`. This is a brief, transient
    /// relay: `effects::apply_parent_chain_context` reads and immediately
    /// clears it at the very next parent->child hand-off (whatever that
    /// child turns out to be), copying it onto that ONE child's typed
    /// `ResolvedAbility::parent_target_missing_reason` field. Nothing else
    /// reads this flag directly — in particular, the shared
    /// `resolved_targets` chokepoint does not, so an empty Dig/ChooseFromZone/
    /// RevealHand can never affect any `ParentTarget` consumer beyond its own
    /// immediate sub_ability (e.g. Avenging Angel's unrelated LTB self-return
    /// stays unaffected). See [`crate::types::ability::ParentTargetMissingReason`]
    /// for what each reason gates and who consults it. Transient resolution
    /// bookkeeping — not serialized. (Consolidated from three parallel
    /// booleans — `last_dig_found_nothing`, `last_choose_from_zone_found_nothing`,
    /// `last_reveal_choice_found_nothing` — per the PR #5834/#5836 review.)
    #[serde(skip)]
    pub last_parent_target_missing_reason: Option<crate::types::ability::ParentTargetMissingReason>,

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
    pub last_vote_ballots: im::Vector<(PlayerId, u32)>,

    /// CR 608.2c + CR 109.5: Player actions performed during the current
    /// top-level ability resolution. Distinct from turn-level trackers like
    /// `players_who_searched_library_this_turn`: this set accumulates only
    /// within one resolving chain so "for each opponent who searched their
    /// library this way" counts the opponents who accepted that offer, even
    /// across player-scope iterations and interactive continuations.
    #[serde(default, skip_serializing_if = "HashSet::is_empty")]
    pub player_actions_this_way: HashSet<(PlayerId, PlayerActionKind)>,

    /// CR 608.2c: Numeric result from the preceding effect in a sub_ability chain.
    /// Set after resolve_effect for effects producing a numeric result (life loss,
    /// damage, counter removal). Read by QuantityRef::PreviousEffectAmount.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_amount: Option<i32>,

    /// CR 120.10: Resolution-local twin of `last_effect_amount` carrying the
    /// *excess* damage dealt by the preceding effect (damage beyond lethal), as
    /// distinct from the total (CR 120.6). Stamped alongside `last_effect_amount`
    /// after a damage-dealing effect resolves and read by
    /// `AbilityCondition::PreviousEffectAmount { channel: DamageChannel::Excess }`
    /// for the "if excess damage was dealt … this way" class. Resolution-scoped:
    /// reset to `None` at depth-0. Follows the `last_effect_amount`
    /// PartialEq-OMISSION pattern: NOT compared in the hand-written `PartialEq`
    /// (safe — always cleared at comparison boundaries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_effect_excess_amount: Option<i32>,

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

    /// CR 603.12a: Number of times the controller has paid a repeated optional
    /// cost ("you may pay {C} up to N times") during the CURRENT ability
    /// resolution. Read by `QuantityRef::TimesCostPaidThisResolution` to size a
    /// reflexive "choose up to that many" modal (Hawkeye, Master Marksman /
    /// Tranquil Frillback class). Incremented once per successful payment by the
    /// repeated-optional-payment driver in `resolve_chain_body`, and cleared at
    /// the `depth == 0` prelude of `resolve_ability_chain` so each top-level
    /// resolution starts at 0.
    ///
    /// This counter is nonzero AT the per-iteration `WaitingFor::OptionalEffectChoice`
    /// pause: `resolve_repeated_optional_payment_choice` increments K and THEN
    /// sets `waiting_for` and returns, so each `DecideOptionalEffect` is a
    /// SEPARATE `apply()` call with K already nonzero. That pause is a serde
    /// boundary — the persistence layer (`to_persisted`/`from_persisted`,
    /// single-player save/load, multiplayer host-resume) can serialize the state
    /// between two payment prompts. Because the paired continuation
    /// `pending_repeated_optional_payment` is serialized-when-`Some` and
    /// eq-included precisely to survive that pause, K must survive it too — a
    /// roundtrip restoring K=0 would collapse the reflexive modal cap (CR 700.2d)
    /// below the payments actually made, denying the player modes they paid for.
    ///
    /// Therefore K mirrors `exiled_from_hand_this_resolution` EXACTLY (the
    /// resolution-local u32 counter observable at a pause), NOT `static_gate_truth`
    /// (which is genuinely derived and recomputable via `refresh_static_gate_truth`):
    /// `#[serde(default, skip_serializing_if = "is_zero_u32")]` (serialized
    /// only when nonzero, so a roundtrip is faithful) and INCLUDED in `PartialEq`.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub optional_cost_payments_this_resolution: u32,

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

    /// CR 805.4b: queue of players who still owe their turn-based draw-step
    /// draw THIS step. Seeded by `turns::enter_phase`'s `Phase::Draw` arm on
    /// first entry (`[active_player]` normally, or `[active_player,
    /// teammate]` under the shared team turns option) and drained front-to-
    /// back by `turns::drain_pending_team_draw_step`. A draw that pauses on
    /// a CR 616.1 competing-replacement choice leaves its player at the
    /// front of the queue (not popped) so resumption — via
    /// `handle_replacement_choice`'s epilogue, which also calls the same
    /// drain function — retries exactly that player's draw and then
    /// continues to any still-queued teammate, instead of either redrawing
    /// a completed player or silently dropping a queued one.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_team_draw_step: Vec<PlayerId>,

    /// CR 502.3: Transient untap-step carry. When a `MaxUntapPerType` cap
    /// (Smoke / Stoic Angel / Damping Field) raises `WaitingFor::ChooseUntapSubset`,
    /// the permanents the active player already chose not to untap (from the
    /// preceding `UntapChoice` optional-decline prompt) are stashed here so the
    /// subset resolution can fold the unchosen complement in alongside them when
    /// it executes the untap. Cleared as soon as the subset prompt resolves.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_untap_declines: Vec<ObjectId>,

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
    /// CR 107.3i: the X announced for an in-flight COST, keyed by the object whose cost
    /// it is. CR 107.3i: "Normally, all instances of X on an object have the same value
    /// at any given time" — so a triggered ability of that SAME object which fires
    /// because of that announcement reads the same X.
    /// `triggers::build_triggered_ability` consumes this and stamps it onto the
    /// triggered ability's `chosen_x`.
    ///
    /// There are exactly TWO announce surfaces, and this one field serves both — they
    /// differ only in *which* rule fixes the value, never in what is carried:
    ///
    /// * **CR 107.3a — an activated ability.** "While an activated ability is on the
    ///   stack, any X in its activation cost equals the announced value." Covers Hydra
    ///   Broodmaster ("when this becomes monstrous, create X X/X tokens") and Shark
    ///   Typhoon ("when you cycle this card, create an X/X Shark").
    /// * **CR 107.3d — a SPECIAL ACTION.** "If a cost associated with a special action,
    ///   such as a suspend cost or a morph cost, has an {X} … in it, the value of X is
    ///   chosen by the player taking the special action immediately before they pay that
    ///   cost." A turn-face-up (CR 116.2b) is such a special action: it uses no stack and
    ///   never passes through `push_ability_entry`, so it needs its own publication.
    ///   **CR 702.37f** (morph) / **CR 702.168e** (disguise) then bind it: "If a
    ///   permanent's morph cost includes X, other abilities of that permanent may also
    ///   refer to X. The value of X in those abilities is equal to the value of X chosen
    ///   as the morph special action was taken." Covers Warbreak Trumpeter, Bane of the
    ///   Living, and Aurelia's Vindicator.
    ///
    /// This MUST be a channel of its own and must NOT reuse `GameObject::cost_x_paid`:
    /// that field is the CR 107.3m *cast*-X channel (`QuantityRef::CostXPaid`), and CR
    /// 107.3k makes an activated ability's X "independent of any other values of X
    /// chosen for that object". Writing an announced X there would read the wrong X
    /// by rule, not merely a missing one.
    ///
    /// Published at exactly the three moments an announced X can precede a trigger event
    /// — an activated ability's announcement (`casting_costs::push_ability_entry`, which
    /// emits `Cycled` / `KeywordAbilityActivated`), its own resolution
    /// (`stack::resolve_top`, covering `EffectResolved` emitters such as Monstrosity),
    /// and the turn-face-up special action (`engine`'s `GameAction::TurnFaceUp` handler,
    /// which emits `TurnedFaceUp`). Set to `None` for every other stack-entry kind and
    /// cleared at the start of each `resolve_top` alongside `resolving_stack_entry`, so a
    /// resolving SPELL never publishes: that is what keeps a permanent put onto the
    /// battlefield by an unrelated X-spell at X=0 (CR 107.3m: "the value of X for that
    /// permanent is 0") instead of inheriting the spell's X.
    /// Transient decision context, serialized so a mid-activation pause round-trips.
    ///
    /// `serde(alias)`: this field was named `activated_ability_x` before the special-action
    /// surface was added. `GameState` sets no `deny_unknown_fields`, so without the alias an
    /// older save written mid-activation would have its live X **silently dropped** rather
    /// than rejected.
    #[serde(
        default,
        alias = "activated_ability_x",
        skip_serializing_if = "Option::is_none"
    )]
    pub announced_source_x: Option<(ObjectId, u32)>,
    /// CR 400.7j (+ CR 400.7g/h cast hop): a resolution-scoped record of a source
    /// object that the currently-resolving ability moved as part of its own
    /// resolution (Siege "exile it, then you may cast it"). It lets
    /// `source_is_current` re-find the moved object even though the all-zone
    /// incarnation bump advanced its epoch. Chains across multiple self-moves in
    /// one resolution (BF→Exile→Stack). Like `resolving_stack_entry` this is a
    /// serialized, resolution-scoped field that survives an optional-choice pause;
    /// it is cleared at the same sites. It carries incarnation values, so it is
    /// deliberately EXCLUDED from `GameState::PartialEq` (loop equality) — including
    /// it would recreate the identity-field loop leak Condition 2 fixes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resolution_source_relatch: Option<ResolutionSourceRelatch>,
    /// CR 732.2a (PR-7 Phase 4d-ii): cast-time snapshot of the most recent buyback-paid,
    /// permanent-creating spell — the object-growth recast the loop-shortcut hook replays.
    /// Set at cast finalization, read at the post-resolution empty-stack `Priority` window.
    /// Transient: deliberately EXCLUDED from `impl PartialEq for GameState` (a decision
    /// context, not durable board state) and COMPARED explicitly only in the object-growth
    /// cover gates (`analysis::resource::eq_except_growable` /
    /// `loop_states_equal_modulo_resources`, fail-closed). `None` in filtered/serialized
    /// snapshots (byte-preserving).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_recast_context: Option<RecastContext>,
    /// Transient plural form of `current_trigger_event` for batched triggers.
    /// Event-context filters that can legally compare against a group read this.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub current_trigger_events: Vec<GameEvent>,
    /// CR 701.57a: The mana-value limit `N` of the most recently resolved
    /// discover. Set when a `discover N` resolves so that a "whenever you
    /// discover" trigger's effect can reference "the same value" (Curator of
    /// Sun's Creation: "discover again for the same value" →
    /// `QuantityRef::TriggeringDiscoverValue`). Transient — not part of durable
    /// game state, but serialized so a mid-resolution pause round-trips.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_discover_value: Option<i32>,
    /// Full event batches for triggered abilities currently on the stack,
    /// keyed by stack entry id. Single-event triggers omit an entry here.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub stack_trigger_event_batches: HashMap<ObjectId, Vec<GameEvent>>,

    /// CR 400.7: Last Known Information cache.
    /// Populated before zone changes for objects leaving the battlefield.
    /// Cleared on phase/step transitions via `advance_phase()`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lki_cache: HashMap<ObjectId, LKISnapshot>,
    /// CR 608.2h + CR 707.2: Full copiable-values LKI for objects that leave a
    /// public zone. Ordinary `LKISnapshot` is intentionally filter-shaped and
    /// does not carry ability definitions; copy effects need the complete
    /// copiable surface when the copy source later ceases to exist.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lki_copiable_values: HashMap<ObjectId, CopiableValues>,

    /// CR 400.7 + CR 608.2h: LKI keyed by exact object incarnation. The legacy
    /// `lki_cache` remains the broad ObjectId-only authority for existing callers;
    /// resolution paths that carry an incarnation use this history so a later
    /// departure of a re-entered object cannot overwrite the earlier object's LKI.
    /// Cleared with `lki_cache` on phase/step transitions.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub lki_by_incarnation: HashMap<ObjectId, HashMap<u64, LKISnapshot>>,

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

    /// Transient auto-tap aura overrides. Before `resolve_tap_mana_triggers_inline`,
    /// `auto_tap_mana_sources_inner` inserts one entry per aura whose `TapsForMana`
    /// trigger is about to fire. Keyed by aura `ObjectId`; value is the color that
    /// the auto-tap planner chose for this aura. Consumed by
    /// `resolve_triggered_mana_ability_inline`; cleared immediately after inline
    /// trigger resolution. Never serialized — it is only valid within the synchronous
    /// auto-tap call and must always be empty in any persisted snapshot.
    #[serde(skip)]
    pub pending_taps_for_mana_overrides: std::collections::HashMap<ObjectId, ProductionOverride>,

    /// Transient color override forwarded to the currently resolving triggered mana
    /// ability (via `resolve_triggered_mana_ability_inline`). Set from
    /// `pending_taps_for_mana_overrides`; read by `effects::mana::resolve` when
    /// `is_triggered_mana_inline` is true; cleared immediately after the ability chain
    /// returns. Never serialized.
    #[serde(skip)]
    pub current_triggered_mana_override: Option<ProductionOverride>,

    /// CR 601.2h + CR 614.12a + CR 616.1: Typed continuation for a sequential
    /// cost move paused by a replacement choice. This remains serialized with
    /// the matching `WaitingFor::ReplacementChoice`, so a host checkpoint can
    /// resume the same cost-payment action after the player answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pending_cost_move_resume: Option<PendingCostMoveResume>,

    /// CR 601.2h + CR 616.1: Resume a sequential discard cost after a
    /// replacement choice. Cost moves use `pending_cost_move_resume` above.
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
    /// CR 901.15: The planar deck (single-deck Planechase option). Front = top;
    /// the active face-up plane/phenomenon lives in the command zone, NOT here.
    #[serde(default, skip_serializing_if = "im::Vector::is_empty")]
    pub planar_deck: im::Vector<ObjectId>,
    /// CR 311.5: The planar controller — the player designated to roll the
    /// planar die and resolve the active plane's abilities. `None` outside a
    /// Planechase game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub planar_controller: Option<PlayerId>,
    /// CR 901.9 / CR 116.2i: Number of planar die special actions each player
    /// has taken this turn. Effect-caused planar die rolls do not increment
    /// this counter.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub planar_die_actions_this_turn: HashMap<PlayerId, u32>,
    /// CR 904.3 / CR 904.4: The archenemy's scheme deck (single-deck Archenemy
    /// option). Front = top; face-down in the command zone (CR 314.2). Schemes
    /// that are set in motion turn face up and stay in the command zone, NOT here.
    #[serde(default, skip_serializing_if = "im::Vector::is_empty")]
    pub scheme_deck: im::Vector<ObjectId>,
    /// CR 904.2a: The archenemy — owner/controller of all scheme cards
    /// (CR 314.5 / CR 904.7). `None` outside an Archenemy game.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archenemy: Option<PlayerId>,
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
    pub combat_prevention_tally: Option<HashMap<AppliedReplacementKey, i32>>,
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
    /// CR 611.2b + CR 110.5d: the concrete object a target-relative
    /// `ForAsLongAs` duration tracks, captured at resolution time. Distinct
    /// from `affected` when the modification's recipient and the duration's
    /// subject diverge — Zygon Infiltrator: the copy modification applies to
    /// the source, but the duration tracks the copy *target*'s tap state.
    /// `None` for the common case where the duration tracks `affected` or the
    /// source. Set only via [`GameState::set_transient_duration_subject`] on the
    /// TCE that `add_transient_continuous_effect` just created, so all TCE
    /// construction stays in one authority. Backward-compatible across the
    /// WASM/multiplayer serialization boundary.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_subject: Option<ObjectId>,
    /// Snapshot of the originating object's name, captured at construction.
    /// The originating spell/ability typically moves to a new zone (graveyard,
    /// stack→exile, etc.) with a new ObjectId per CR 400.7 after resolution,
    /// so live `state.objects[source_id]` lookup may not return the original
    /// card. Snapshot is captured here so attribution display ("+3/+3 from
    /// Giant Growth") survives the source's zone change.
    #[serde(default)]
    pub source_name: String,
}

/// CR 701.50a + CR 614.5 + CR 616.1f: deferred "then that creature connives"
/// link of a connive replacement whose LEADING `Draw` link parked an interactive
/// `ReplacementChoice` (the controller's own draw is itself replaced). CR 701.50a's
/// replacement reads "instead you draw a card, THEN that creature connives" — the
/// "then" fixes the printed order, so the connive must run only AFTER the parked
/// draw choice resolves. Held in a DEDICATED slot (NOT
/// `post_replacement_continuation`) so the shared zone-delivery tail
/// (`apply_zone_delivery_tail`, `DeliveryTail` owner) cannot drain it mid-draw —
/// it is drained ONLY by the post-replacement-choice epilogue
/// (`engine_replacement::handle_replacement_choice`), after the leading draw fully
/// delivers, on both the accept and decline resume paths. On drain it re-enters
/// the pipeline via `propose_connive` with the already-applied rids excluded
/// (CR 614.5) so the CR 616.1f repeat covers the remaining connive replacements
/// without self-invoking. (CR 614.11a — completing a replacement's actions before
/// resuming a draw — is the analogous supporting principle.)
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingConniveReentry {
    pub conniver: ObjectId,
    pub count: u32,
    pub applied: HashSet<AppliedReplacementKey>,
}

/// CR 614.6 + CR 615.5: a post-replacement continuation and every value that is
/// consumed with it — one record, installed and drained as a unit.
///
/// These five values used to be five parallel `GameState` fields. They were
/// written by three different install paths, none of which set all five, and the
/// teardown had to remember to null each one by hand. `elimination.rs` still
/// carries the scar of that design: *"this field was added after the teardown
/// block below was written and was missed until this regression."* Bundling them
/// makes "a continuation is pending" one fact instead of an invariant maintained
/// by hand across ~40 sites.
/// CR 615.5: where a drain is in its lifecycle.
///
/// The continuation and the drain do not die together, and that is load-bearing.
/// A drain's *event context* (CR 615.5 — the prevented event's source and target)
/// must stay readable while its continuation runs: that is how
/// `TargetFilter::PostReplacementSourceController` resolves "the source's
/// controller draws cards" (Swans of Bryn Argoll). But the continuation itself
/// must already be gone, so a nested "is a continuation pending?" check taken
/// during the dispatch sees none and does not re-drain it.
///
/// The old single slot expressed this by taking the continuation early and
/// clearing the event fields late — an interleaving that no type enforced and
/// every caller had to respect. Here it is a state transition:
/// `Ready(work)` → `Dispatching` → popped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DrainStatus {
    /// Not yet run. `Template` is an AST resolved against `source`; `Resolved`
    /// carries targets captured at shield-install time.
    Ready(crate::types::ability::PostReplacementContinuation),
    /// Taken and running. The drain stays resident so the running effect can still
    /// read its event context (CR 615.5).
    Dispatching,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PostReplacementDrain {
    /// The work to run, and whether it has been taken yet.
    pub status: DrainStatus,

    /// CR 615.5: the replacement's own source (Swans of Bryn Argoll), so a
    /// `SelfRef`-targeted post-effect resolves against the shield rather than the
    /// damaged object.
    ///
    /// `Option` *inside* the drain, not a parallel field: several paths
    /// deliberately clear the source while the continuation stays resident
    /// (a zone change's caller epilogue drains with the
    /// spell-resolution ctx and no source).
    pub source: Option<crate::types::identifiers::ObjectId>,

    /// CR 614.5 + CR 616.1f: replacement identities already applied to the event
    /// that produced this continuation, so a def cannot fire twice on it.
    ///
    /// This lives INSIDE the drain rather than beside it, and that is a deliberate
    /// call backed by a census of the field's every use before the bundling
    /// (`git grep post_replacement_applied d1f7d05ea8`, 7 sites):
    ///
    ///   * exactly ONE read — `apply_pending_post_replacement_effect`'s
    ///     `std::mem::take`, which IS the drain;
    ///   * both writes sit with a continuation install (`stash_*`, and the optional
    ///     accept/decline branch);
    ///   * both clears pair with one too — `combat_damage.rs`'s clear is the line
    ///     immediately before the continuation it is zeroing the set *for*, and the
    ///     optional branch's clear pairs with that branch installing no continuation.
    ///
    /// So the set never lives without a drain and is never read except at drain
    /// time. It is co-owned, not merely co-located. (The reading that it has an
    /// independent lifecycle comes from looking at the *instant* of the
    /// `combat_damage` clear rather than its *purpose*; that reading is wrong.)
    pub applied: HashSet<AppliedReplacementKey>,

    /// CR 615.5 + CR 609.7: source of the *prevented event itself* (the damage
    /// dealer), distinct from `source` (the shield). Resolves
    /// `TargetFilter::PostReplacementSourceController` — "the source's controller
    /// draws cards".
    pub event_source: Option<crate::types::identifiers::ObjectId>,

    /// CR 615.5: target of the prevented event itself, for
    /// `TargetFilter::PostReplacementDamageTarget`.
    pub event_target: Option<crate::types::ability::TargetRef>,
}

/// CR 616.1g: what an install does when a continuation is already resident.
///
/// This is an explicit parameter because the three production install paths
/// genuinely disagree today, and a refactor that silently picked one would change
/// behaviour:
///
///   * [`Self::KeepResident`] — `apply_single_replacement`'s stash: the *incoming*
///     continuation is discarded.
///   * [`Self::Replace`] — the optional accept/decline path and the combat
///     prevention riders: the *resident* continuation is overwritten.
///
/// Naming them is the point: today the policy is an accident of where the
/// assignment happens to sit.
///
/// # What the `KeepResident` drop actually is — measured, not inferred
///
/// An earlier reading of this code held that the drop was an *accidental CR 614.5
/// dedup*: the same replacement's continuation stashed twice, the second discarded,
/// so the effect runs once — and that letting the stack nest would therefore make
/// Wolverine, Fierce Fighter heal twice and Krark's Thumb double twice. **That is
/// false in every part, and this comment is the correction.** It was inferred from
/// the observation that the dropped payload is byte-identical to the resident one;
/// payload equality was then read as evidence of a dedup. It is not.
///
/// Instrumenting the engine integration suite (2732 tests) records **96 installs, 2
/// collision-drops**, owned by
/// `wolverine_noncombat_separate_instance_heals_prior_damage` and
/// `flip_coins_three_with_krark_prompts_three_times`. Three facts about them:
///
///   * **They are SIBLING events, not one event twice.** Wolverine's two stashes come
///     from two distinct `Damage` events (two blockers, CR 510.2 simultaneity);
///     Krark's from two distinct `CoinFlip` events. Each event applies the definition
///     exactly once, which is what CR 614.5 licenses — it grants one opportunity *per
///     event*, and there are two events. The applied-set dedup is fully wired
///     (`already_applied` gates candidate selection) and correctly declines to
///     suppress here. Nothing is missing from this path.
///   * **The dropped continuation never runs — in either regime.** Dispatch counts are
///     identical with the drop and with the stack forced to nest: Wolverine dispatches
///     its continuation **zero** times ever (its heal is delivered by
///     `dealt_damage_applier`, never by this stack); Krark dispatches **once**, drop or
///     push. So the drop de-duplicates nothing. Forcing a naive nest leaves the whole
///     engine suite green (16220 passing) — the only casualty is the unit test that
///     tautologically asserts the drop.
///   * **Neither cited rule governs them.** CR 614.5 is per-event; CR 616.1g is about an
///     event *contained within* another. Sibling events fall through both.
///
/// So the drop is not a dedup and not a rules gate. It is a **leak-guard**: it keeps
/// an un-dispatchable sibling-event stash from accumulating on the stack, where a
/// resident `Ready` drain would make `has_ready()` true forever and permanently gate
/// `draw_through_replacement`.
///
/// # CR 616.1g nesting already works — it does not need this policy relaxed
///
/// A genuinely *different* definition on a *contained* event nests today, because the
/// outer drain is `Dispatching` (not `Ready`) while its continuation runs, and
/// `KeepResident` collides on [`PostReplacementDrainStack::has_ready`] rather than on
/// residency. The suite records **3 live dispatches at depth 2**, pinned by
/// `nested_mandatory_post_effect_runs_when_a_dispatching_continuation_draws`.
///
/// # The real open defect
///
/// A sibling-event mandatory post-effect is *stashed and never dispatched* — dead
/// work. It is invisible today only because both witnesses deliver their effect by
/// another path. A future card whose sibling continuation is the **only** delivery
/// path would lose it silently. That, not an identity gate, is what GitHub issue
/// #5676 tracks.
///
/// An identity gate keyed on "is the incoming `ReplacementId` already in the event's
/// `applied` set" was designed and **withdrawn**: `mark_applied(rid)` runs *before*
/// the stash, so that predicate is true at 100% of stashes. It would suppress every
/// mandatory post-effect, Swans of Bryn Argoll included — an off-switch, not a gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResidentDrainPolicy {
    KeepResident,
    Replace,
}

/// CR 616.1g: the post-replacement continuations awaiting a drain.
///
/// Depth is currently capped at one by [`ResidentDrainPolicy`] — this type
/// reproduces the old single-slot behaviour exactly. It is a stack so that
/// nesting can be turned on as an isolated, reviewable change rather than as a
/// side effect of the bundling.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct PostReplacementDrainStack {
    drains: Vec<PostReplacementDrain>,
}

impl PostReplacementDrain {
    /// A ready drain carrying only a continuation — no source, no inherited applied
    /// set, no prevented-event context. The shape the combat prevention riders and
    /// most test setups want.
    pub fn ready(continuation: crate::types::ability::PostReplacementContinuation) -> Self {
        Self {
            status: DrainStatus::Ready(continuation),
            source: None,
            applied: HashSet::new(),
            event_source: None,
            event_target: None,
        }
    }

    /// The continuation, if it has not been taken for dispatch yet.
    pub fn ready_continuation(
        &self,
    ) -> Option<&crate::types::ability::PostReplacementContinuation> {
        match &self.status {
            DrainStatus::Ready(continuation) => Some(continuation),
            DrainStatus::Dispatching => None,
        }
    }
}

impl PostReplacementDrainStack {
    pub fn is_empty(&self) -> bool {
        self.drains.is_empty()
    }

    /// Is there a continuation that has not run yet?
    ///
    /// A `Dispatching` drain does NOT count: its continuation is already running,
    /// and a nested check taken during that dispatch must not try to re-drain it.
    /// This is exactly what `post_replacement_continuation.is_some()` meant, since
    /// the old code took the continuation out of the slot before dispatching.
    pub fn has_ready(&self) -> bool {
        self.drains
            .iter()
            .any(|drain| matches!(drain.status, DrainStatus::Ready(_)))
    }

    /// The innermost drain, whatever its status. Event-context reads (CR 615.5)
    /// go through here, because they must still resolve while it is `Dispatching`.
    pub fn resident(&self) -> Option<&PostReplacementDrain> {
        self.drains.last()
    }

    pub fn resident_mut(&mut self) -> Option<&mut PostReplacementDrain> {
        self.drains.last_mut()
    }

    /// Install `drain`, resolving a collision with any resident one per `policy`.
    ///
    /// Returns `false` when the incoming drain was discarded (`KeepResident` while
    /// another continuation is still *pending*), so a caller can distinguish
    /// "installed" from "silently dropped" — something the old
    /// `stash_post_replacement_continuation` could not.
    ///
    /// `KeepResident` collides on [`Self::has_ready`], **not** on
    /// `!drains.is_empty()`, and the difference is a real bug rather than a
    /// nicety. A drain stays resident while it *dispatches* (its event context must
    /// remain readable — CR 615.5), but its continuation has already been taken, so
    /// it is no longer pending work. The predecessor slot expressed this by moving
    /// the continuation out before dispatching: the slot then read empty, and a
    /// re-entrant stash landed.
    ///
    /// CR 616.1g: that re-entrant stash is real. A running continuation draws, the
    /// draw is replaced, and the replacement carries a mandatory post-effect (Jace,
    /// Wielder of Mysteries' win; Abundance's reveal-until). Colliding on mere
    /// residency drops it, and `draw_through_replacement` — which gates its drain on
    /// `has_post_replacement_drain()`, i.e. on `has_ready()` — then never runs it.
    pub fn install(&mut self, drain: PostReplacementDrain, policy: ResidentDrainPolicy) -> bool {
        match policy {
            ResidentDrainPolicy::KeepResident if self.has_ready() => false,
            ResidentDrainPolicy::KeepResident => {
                self.drains.push(drain);
                true
            }
            ResidentDrainPolicy::Replace => {
                // CR 615.5: evict a READY resident, never a DISPATCHING one. A
                // `Dispatching` drain is not stale state to overwrite — it is the
                // event context of a continuation running right now, and it is what
                // `TargetFilter::PostReplacementSourceController` reads to answer
                // "the source's controller draws cards" (Swans of Bryn Argoll).
                // Popping it mid-dispatch destroys that answer under the running
                // effect. The incoming continuation nests above it instead
                // (CR 616.1g) — the same READY-not-RESIDENT predicate `KeepResident`
                // uses.
                if self
                    .drains
                    .last()
                    .is_some_and(|resident| matches!(resident.status, DrainStatus::Ready(_)))
                {
                    self.drains.pop();
                }
                self.drains.push(drain);
                true
            }
        }
    }

    /// CR 615.5: take the resident continuation and mark the drain `Dispatching`,
    /// leaving it resident so the running effect can still read its event context.
    ///
    /// Returns `None` if there is no resident drain, or its continuation was
    /// already taken.
    pub fn begin_dispatch(&mut self) -> Option<crate::types::ability::PostReplacementContinuation> {
        let drain = self.drains.last_mut()?;
        match std::mem::replace(&mut drain.status, DrainStatus::Dispatching) {
            DrainStatus::Ready(continuation) => Some(continuation),
            // Already dispatching: report no work. The `mem::replace` above has
            // already written `Dispatching` back, so there is nothing to restore.
            DrainStatus::Dispatching => None,
        }
    }

    /// Pop the drain whose continuation has finished dispatching.
    pub fn finish_dispatch(&mut self) -> Option<PostReplacementDrain> {
        if matches!(self.drains.last()?.status, DrainStatus::Dispatching) {
            return self.drains.pop();
        }
        None
    }

    /// CR 800.4a: abandon every pending continuation (player departure).
    pub fn abandon_all(&mut self) {
        self.drains.clear();
    }
}

/// Legacy pre-`DrawSequenceStack` save shape: the single in-flight multi-card
/// draw. Deserialize-only — [`GameState::migrate_pending_multi_draw`] converts it
/// into a one-frame [`DrawSequenceStack`]. No production writer remains.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingMultiDraw {
    pub player: PlayerId,
    pub remaining: u32,
    pub accumulated: u32,
}

/// Identifies one draw-instruction frame within [`DrawSequenceStack`].
///
/// Frames are addressed by ID, never by position: a resume that arrives after a
/// pause must prove it is resuming the frame it parked, not merely "whatever is
/// on top now". Between the park and the resume a nested instruction (CR 616.1g)
/// may have been pushed and popped above it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct DrawSequenceFrameId(pub u64);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DrawSequenceOrigin {
    #[default]
    Plain,
    /// CR 701.50a/701.50d: after the connive draws settle, discard `count` cards and
    /// put +1/+1 counters equal to nonland cards discarded on `conniver`.
    ConniveTail { conniver: ObjectId, count: u32 },
    /// CR 701.22d-adjacent bookkeeping: a scry replaced into a draw completes by
    /// emitting EffectResolved{Scry} for `source_id` once the draws settle.
    ScryCompletion { source_id: ObjectId },
}

/// CR 121.2: "Cards may only be drawn one at a time. If a player is instructed to
/// draw multiple cards, that player performs that many individual card draws."
///
/// One frame is one *draw instruction* in flight — the unit of a `Draw N`, not of
/// a single card. It survives a pause (a per-unit replacement choice: Dredge,
/// Notion Thief, Hullbreacher, a Miracle reveal) so the remaining individual
/// draws resume against exactly this instruction.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawSequenceFrame {
    pub frame_id: DrawSequenceFrameId,
    pub player: PlayerId,
    /// CR 614.5: replacement identities already applied to the draw instruction
    /// that produced this frame. Every individual draw, including one resumed
    /// after a replacement choice, starts with this set so an originating
    /// replacement cannot apply to its own substitute draw.
    #[serde(default)]
    pub applied: HashSet<AppliedReplacementKey>,
    /// The instruction's completion behavior. Old saves default to [`DrawSequenceOrigin::Plain`].
    #[serde(default)]
    pub origin: DrawSequenceOrigin,
    /// CR 121.6b: individual draws of this instruction not yet attempted. "If an
    /// effect replaces a draw within a sequence of card draws, the replacement
    /// effect is completed before resuming the sequence."
    pub remaining: u32,
    /// CR 608.2c: running total of cards ACTUALLY delivered across every
    /// completed unit of this instruction. This is the value a later "that many"
    /// clause on the same card reads ("Draw two cards, then discard that many") —
    /// "later text on the card may modify the meaning of earlier text". Committed
    /// to `state.last_effect_count` exactly once, when the instruction completes,
    /// so the chained clause sees the true total across the WHOLE instruction and
    /// not just the last unit. A unit whose draw was replaced by something else
    /// (Dredge) contributes 0; a unit doubled by a count modifier contributes its
    /// post-replacement count.
    pub accumulated: u32,
}

/// CR 121.2 + CR 616.1g: the stack of draw instructions in flight.
///
/// A stack rather than a single slot because a replacement applied to one
/// instruction's individual draw may itself perform a draw (CR 616.1g: "one
/// replacement or prevention effect may apply to an event, and another may apply
/// to an event contained within the first event"). The inner instruction must run
/// to completion and then resume the outer one.
///
/// The predecessor of this type was a single `Option<PendingMultiDraw>` slot,
/// which could not represent that nesting: a substituted inner draw overwrote the
/// outer instruction's frame and its remaining units were silently lost.
///
/// Invariants, enforced by [`DrawSequenceStack::validate`]:
///   * every `frame_id` is distinct and `< next_frame_id`;
///   * `next_frame_id` never rewinds, so a stale [`DrawSequenceFrameId`] from an
///     abandoned frame can never alias a live one.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DrawSequenceStack {
    frames: Vec<DrawSequenceFrame>,
    next_frame_id: u64,
}

impl DrawSequenceStack {
    pub fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    /// The instruction currently being executed — the innermost one.
    pub fn active(&self) -> Option<&DrawSequenceFrame> {
        self.frames.last()
    }

    pub fn active_mut(&mut self) -> Option<&mut DrawSequenceFrame> {
        self.frames.last_mut()
    }

    /// The active frame, but only if it is the one the caller parked.
    ///
    /// CR 616.1g: a resume must address its own instruction. If a nested
    /// instruction is still above this one, the outer frame is not resumable yet
    /// and this returns `None` rather than letting the outer resume run against
    /// the inner frame's cursor.
    pub fn active_if(&mut self, frame_id: DrawSequenceFrameId) -> Option<&mut DrawSequenceFrame> {
        self.frames
            .last_mut()
            .filter(|frame| frame.frame_id == frame_id)
    }

    /// Push a new instruction and return its ID. Monotonic: the allocator never
    /// rewinds, so an ID from a popped frame is never reissued.
    pub fn push(&mut self, player: PlayerId, count: u32) -> DrawSequenceFrameId {
        self.push_with_replacement_applied(player, count, HashSet::new())
    }

    /// Push a draw instruction carrying replacement identity from its origin.
    pub fn push_with_replacement_applied(
        &mut self,
        player: PlayerId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
    ) -> DrawSequenceFrameId {
        self.push_with_replacement_applied_and_origin(
            player,
            count,
            applied,
            DrawSequenceOrigin::Plain,
        )
    }

    /// Push a draw instruction carrying its originating replacement identities and
    /// completion behavior.
    pub fn push_with_replacement_applied_and_origin(
        &mut self,
        player: PlayerId,
        count: u32,
        applied: HashSet<AppliedReplacementKey>,
        origin: DrawSequenceOrigin,
    ) -> DrawSequenceFrameId {
        let frame_id = DrawSequenceFrameId(self.next_frame_id);
        self.next_frame_id += 1;
        self.frames.push(DrawSequenceFrame {
            frame_id,
            player,
            applied,
            origin,
            remaining: count,
            accumulated: 0,
        });
        debug_assert!(
            self.validate().is_ok(),
            "draw-sequence stack invariant broken after push: {:?}",
            self.validate()
        );
        frame_id
    }

    /// Pop the active instruction, which must be `frame_id`.
    pub fn pop(&mut self, frame_id: DrawSequenceFrameId) -> Option<DrawSequenceFrame> {
        if self.frames.last()?.frame_id != frame_id {
            return None;
        }
        self.frames.pop()
    }

    /// CR 800.4a: abandon every in-flight instruction (player departure).
    ///
    /// The allocator deliberately does NOT rewind: a [`DrawSequenceFrameId`]
    /// captured before the abandonment must never alias a frame allocated after
    /// it, or a stale resume would drive the wrong instruction.
    pub fn abandon_all(&mut self) {
        self.frames.clear();
    }

    /// CR 104.4b: loop-equality projection — compares game *position*, not history.
    ///
    /// Two states that differ only in how many draw frames the game has allocated
    /// over its lifetime are the same position, so the monotonic `next_frame_id`
    /// allocator and the per-frame `frame_id` (both pure identity) are excluded.
    /// What remains is exactly what the predecessor `Option<PendingMultiDraw>`
    /// compared: who is drawing, how many units are owed, how many have landed,
    /// and the completion origin. Origin is included because different origins
    /// produce different eventual game states when their frames complete.
    ///
    /// This must NOT be the derived `PartialEq`. Comparing the allocator would
    /// mean two identical positions never compare equal, and CR 104.4b loop
    /// detection would silently stop firing — a failure invisible to every draw
    /// test, surfacing only as "the engine no longer draws a mandatory loop".
    /// `GameState`'s hand-curated `PartialEq` already excludes the other
    /// identity-bearing state (`transient_continuous_effects`,
    /// `resolution_source_relatch`) for the same reason.
    pub(crate) fn loop_equal(&self, other: &Self) -> bool {
        self.frames.len() == other.frames.len()
            && self.frames.iter().zip(&other.frames).all(|(a, b)| {
                a.player == b.player
                    && a.remaining == b.remaining
                    && a.accumulated == b.accumulated
                    && a.origin == b.origin
            })
    }

    /// Returns `Err` describing the first broken invariant, if any.
    pub fn validate(&self) -> Result<(), String> {
        let mut seen = std::collections::HashSet::new();
        for frame in &self.frames {
            if frame.frame_id.0 >= self.next_frame_id {
                return Err(format!(
                    "draw frame {:?} is at or above the allocator {} — a stale ID can alias a live frame",
                    frame.frame_id, self.next_frame_id
                ));
            }
            if !seen.insert(frame.frame_id) {
                return Err(format!("duplicate draw frame id {:?}", frame.frame_id));
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingReplacement {
    pub proposed: ProposedEvent,
    /// CR 701.21a + CR 614.1: An inner Battlefield→graveyard `ZoneChange`
    /// can pause for a `Moved` replacement after its enclosing sacrifice was
    /// already accepted. Preserve that action's subject and controller until
    /// the resumed zone change actually delivers, then emit the one
    /// `PermanentSacrificed` event that sacrifice triggers observe (CR 603.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sacrifice_provenance: Option<PendingSacrificeProvenance>,
    pub candidates: Vec<ReplacementId>,
    /// CR 616.1: SearchFound choices snapshot the selected source
    /// incarnation, controller/grantee, modifier, and display data at offer
    /// time. Empty for every other replacement event.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_found_candidates: Vec<crate::types::proposed_event::BoundSearchFoundCandidate>,
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
    /// CR 120.4a: carries the excess-redirect rider ("Excess damage is dealt to
    /// that creature's controller instead") across a damage replacement *choice*
    /// pause. The resume in `handle_replacement_choice` rebuilds the
    /// `DamageContext` from the source (which cannot re-derive an effect rider),
    /// so it restores this onto the ctx to keep redirecting the excess. `None`
    /// for every parked event that is not an excess-redirect damage hit.
    #[serde(default)]
    pub excess_recipient: Option<crate::types::ability::ExcessRecipient>,
    /// CR 702.15b: the deferred lifelink bonus carried by a redirect leg (the
    /// earlier creature leg's lethal). Preserved across the redirect leg's own
    /// damage-replacement choice pause so the combined lifelink total is still
    /// gained on resume. `0` for every parked event that is not such a leg.
    #[serde(default)]
    pub lifelink_bonus: u32,
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

/// CR 701.21a: The subject and controller of a sacrifice whose inner zone
/// change is paused in the replacement pipeline.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingSacrificeProvenance {
    pub object_id: ObjectId,
    pub player_id: PlayerId,
}

/// CR 703.4q + CR 616.1 + CR 614.1a: One step-end mana handler entry pending
/// resolution for the current phase transition. Built from the printed-static
/// and transient-continuous-effect scans at the start of each per-player drain,
/// and addressed by the replacement pipeline via `ReplacementId { source:
/// ObjectId(0), index }`.
///
/// `description` (paired with `source`) is surfaced as a
/// `ReplacementCandidateSummary` in `WaitingFor::ReplacementChoice::candidates`
/// when multiple handlers apply to the same emptying event and CR 616.1
/// requires the affected player to choose ordering.
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
    /// CR 723.1a: Creation timestamp used only to compare this player-control
    /// effect with other currently applicable player-control effects. Legacy
    /// saves deserialize to zero, making them deterministically oldest.
    #[serde(default)]
    pub timestamp: u64,
    #[serde(default)]
    pub grant_extra_turn_after: bool,
    /// CR 723.1 / CR 723.2: which window this control binds to. `NextTurn` is
    /// activated/released at turn boundaries; `NextCombatPhase` at the target's
    /// next combat-phase boundaries. `#[serde(default)]` = `NextTurn` so saved
    /// games predating this field load unchanged.
    #[serde(default)]
    pub window: ControlWindow,
}

/// CR 723.1a: Stable identity of one currently applicable player-control
/// effect. Controller alone is insufficient when the same player creates
/// multiple effects; creation timestamp distinguishes them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActivePlayerControl {
    pub controller: PlayerId,
    pub timestamp: u64,
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraPhase {
    /// The phase after which this extra phase is inserted (CR 500.8).
    pub anchor: Phase,
    /// The phase to insert.
    pub phase: Phase,
    /// CR 508.1c: Attacker restriction active while this scheduled combat phase
    /// is current (concretized at resolution to `TrackedSet` / `Typed` /
    /// `SpecificObject`). `None` for ordinary extra phases. Carried here so the
    /// restriction activates exactly when (and only when) this phase begins.
    #[serde(default)]
    pub attacker_restriction: Option<TargetFilter>,
    /// CR 611.2c: The source `ObjectId` of the effect that imposed this
    /// attacker restriction. Used to build a correct `FilterContext` at
    /// evaluation time so source-relative restriction predicates (e.g.,
    /// "creatures that share a color with this card") resolve against the actual
    /// scheduling spell rather than a dummy sentinel. `None` for unrestricted
    /// extra phases.
    #[serde(default)]
    pub attacker_restriction_source: Option<ObjectId>,
}

// Pin `GameState: Send + Sync` at compile time. Blocks accidental imports of
// `im-rc` (the single-threaded variant of `im`, which is !Send/!Sync) and
// catches any future field addition that violates thread-safety.
const _: fn() = || {
    fn assert_send_sync<T: Send + Sync>() {}
    assert_send_sync::<GameState>();
};

impl GameState {
    /// Capture the live ChaCha20 stream offset into `rng_word_pos` so it
    /// survives serialization — `rng` is `#[serde(skip)]`, so this field is the
    /// only carrier of the position across a snapshot (issue #5466). Callers
    /// serializing a faithfully-restorable snapshot invoke this first; the
    /// randomness logic lives here in the engine, not in transport layers.
    pub fn capture_rng_word_pos(&mut self) {
        self.rng_word_pos = self.rng.get_word_pos();
    }

    /// Reconstruct `rng` from the serialized `rng_seed` and fast-forward it to
    /// the saved `rng_word_pos`, so a restored snapshot resumes the random
    /// stream where it left off instead of rewinding to origin and replaying
    /// already-consumed values (issue #5466). Pre-#5466 snapshots carry
    /// `rng_word_pos == 0`, which reproduces the previous from-origin behavior.
    pub fn rehydrate_rng(&mut self) {
        self.rng = ChaCha20Rng::seed_from_u64(self.rng_seed);
        self.rng.set_word_pos(self.rng_word_pos);
    }

    /// CR 118.3a: Mint the next stable `ManaPipId` for a pool unit. Monotonic,
    /// never returns the `ManaPipId(0)` unstamped sentinel (counter starts at 1).
    fn next_pip_id(&mut self) -> ManaPipId {
        let id = self.next_pip_id;
        self.next_pip_id += 1;
        ManaPipId(id)
    }

    /// CR 118.3a: Stamp a stable pip id on `unit` and add it to `player`'s mana
    /// pool. This is the single authority for mana entering a *real* pool: every
    /// production/refill/convoke/delve injection routes here so that each pooled
    /// unit has a unique id the player can pin to direct payment. Detached
    /// preview pools (with no `GameState`) keep calling `ManaPool::add` directly.
    pub fn add_mana_to_pool(&mut self, player: PlayerId, mut unit: ManaUnit) {
        unit.pip_id = self.next_pip_id();
        if let Some(p) = self.players.iter_mut().find(|p| p.id == player) {
            p.mana_pool.add(unit);
        }
    }

    /// CR 118.3a: defensively guarantee every unit in `player`'s mana pool carries
    /// a unique, nonzero `pip_id`, re-stamping the `ManaPipId(0)` sentinel and any
    /// duplicate. Production mana is stamped on entry via [`Self::add_mana_to_pool`],
    /// but mana from debug tooling, restored pre-stamping saves, or any path that
    /// reached `ManaPool::add` directly can carry the sentinel — which would make
    /// every such unit pin/unpin together in manual payment. Run at payment entry
    /// so each unit is individually pinnable regardless of how it was produced.
    /// Safe for loop detection: `pip_id` is excluded from `ManaUnit` equality and
    /// `next_pip_id` is zeroed by `normalize_for_loop`.
    pub(crate) fn restamp_pool_pip_ids(&mut self, player: PlayerId) {
        let Some(idx) = self.players.iter().position(|p| p.id == player) else {
            return;
        };
        // First pass (immutable): count units needing a fresh id — the sentinel 0
        // or a duplicate of an earlier unit. `pid == 0` short-circuits so the
        // sentinel is never inserted into `seen`; only real ids populate it.
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        let needed = self.players[idx]
            .mana_pool
            .mana
            .iter()
            .filter(|u| u.pip_id.0 == 0 || !seen.insert(u.pip_id.0))
            .count();
        if needed == 0 {
            return;
        }
        // Mint the fresh ids before borrowing the pool mutably (`next_pip_id` needs
        // `&mut self`), so the assignment pass can use `iter_mut` — idiomatic and
        // compatible with both `Vec` and `im::Vector` without relying on `IndexMut`.
        let mut fresh = Vec::with_capacity(needed);
        for _ in 0..needed {
            fresh.push(self.next_pip_id());
        }
        let mut fresh = fresh.into_iter();
        let mut seen: std::collections::HashSet<u64> = std::collections::HashSet::new();
        for unit in self.players[idx].mana_pool.mana.iter_mut() {
            if unit.pip_id.0 != 0 && seen.insert(unit.pip_id.0) {
                continue; // already unique and stamped — leave it
            }
            let id = fresh
                .next()
                .expect("minted exactly one fresh id per unit needing one");
            seen.insert(id.0);
            unit.pip_id = id;
        }
    }

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

    /// True when the current phase is a configured stop for `player` that applies
    /// on the current turn. Consulted at every engine-driven auto-pass site so the
    /// user's preference is respected whether or not an auto-pass session is active.
    ///
    /// CR 102.1: scope (`AllTurns` / `OwnTurn` / `OpponentsTurns`) is resolved
    /// against `active_player` (the player whose turn it is).
    pub fn phase_stop_hit(&self, player: PlayerId) -> bool {
        self.phase_stops.get(&player).is_some_and(|stops| {
            stops
                .iter()
                .any(|stop| stop.phase == self.phase && stop.applies(player, self.active_player))
        })
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
                life: config.starting_life_for_player(PlayerId(i)),
                ..Player::default()
            })
            .collect();
        let seat_order: Vec<PlayerId> = (0..player_count).map(PlayerId).collect();
        let starting_player = config.starting_player();
        let archenemy = config.archenemy_player();

        GameState {
            turn_number: 0,
            active_player: starting_player,
            phase: Phase::Untap,
            players,
            priority_player: starting_player,
            turn_decision_controller: None,
            turn_decision_control_timestamp: None,
            active_full_turn_control: None,
            active_combat_phase_control: None,
            active_library_searches: ActiveLibrarySearches::default(),
            active_search_decision_controls: ActiveSearchDecisionControls::default(),
            objects: im::HashMap::default(),
            next_object_id: 1,
            // CR 118.3a: start at 1 so minted pip ids never collide with the
            // `ManaPipId(0)` unstamped sentinel.
            next_pip_id: 1,
            active_payment_pins: Vec::new(),
            active_casting_permission_index: None,
            battlefield: im::Vector::new(),
            stack: im::Vector::new(),
            stack_paid_facts: HashMap::new(),
            exile: im::Vector::new(),
            command_zone: im::Vector::new(),
            rng_seed: seed,
            rng_word_pos: 0,
            rng: ChaCha20Rng::seed_from_u64(seed),
            combat: None,
            waiting_for: WaitingFor::Priority {
                player: starting_player,
            },
            has_pending_cast: false,
            lands_played_this_turn: 0,
            max_lands_per_turn: 1,
            priority_pass_count: 0,
            pending_replacement: None,
            liminal_entries: HashMap::new(),
            pending_liminal_entry_resume: None,
            replacement_may_cost_paused: false,
            post_replacement_drains: PostReplacementDrainStack::default(),
            legacy_post_replacement_effect: None,
            legacy_post_replacement_resolved_effect: None,
            legacy_post_replacement_continuation: None,
            legacy_post_replacement_source: None,
            legacy_post_replacement_applied: HashSet::new(),
            legacy_post_replacement_event_source: None,
            legacy_post_replacement_event_target: None,
            post_replacement_token_choice_applied: None,
            post_replacement_token_substitution_count: None,
            pending_connive_reentry: None,
            draw_sequences: DrawSequenceStack::default(),
            legacy_pending_multi_draw: None,
            pending_life_total_assignment: None,
            pending_spell_resolution: None,
            pending_mutate_merge: None,
            deferred_entry_events: Vec::new(),
            layers_dirty: LayersDirty::full(),
            static_gate_truth: im::HashMap::new(),
            trigger_index: TriggerIndex::default(),
            replacement_index: ReplacementIndex::default(),
            static_source_index: StaticSourceIndex::default(),
            static_mode_presence: crate::types::statics::StaticModePresence::all_present(),
            loop_detect_ring: std::collections::VecDeque::new(),
            precast_shortcut_runtime: PrecastShortcutRuntime::default(),
            next_timestamp: 1,
            public_state_dirty: PublicStateDirty::all_dirty(),
            state_revision: 0,
            transient_continuous_effects: im::Vector::new(),
            next_continuous_effect_id: 1,
            attribution: im::HashMap::new(),
            remote_type_layer_recipients: im::HashSet::new(),
            day_night: None,
            spells_cast_this_turn: 0,
            spells_cast_last_turn: None,
            pending_trigger: None,
            pending_trigger_event_batch: Vec::new(),
            pending_trigger_entry: None,
            deferred_triggers: Vec::new(),
            pending_trigger_order: None,
            consumed_before_priority_trigger_events: Vec::new(),
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
            combat_phase_skip_next_turn: vec![
                CombatPhaseSkipState::default();
                player_count as usize
            ],
            scheduled_turn_controls: Vec::new(),
            extra_phases: Vec::new(),
            extra_phase_resume: Vec::new(),
            turn_direction: TurnDirection::Normal,
            current_combat_attacker_restriction: None,
            current_combat_attacker_restriction_source: None,
            seat_order,
            format_config: config,
            eliminated_players: Vec::new(),
            commander_damage: Vec::new(),
            priority_passes: BTreeSet::new(),
            auto_pass: HashMap::new(),
            phase_stops: HashMap::new(),
            lands_tapped_for_mana: HashMap::new(),
            prepaid_mulligan_bottoms: HashMap::new(),
            match_config: MatchConfig::default(),
            match_phase: MatchPhase::InGame,
            match_score: MatchScore::default(),
            game_number: default_game_number(),
            current_starting_player: starting_player,
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
            object_tap_count_this_turn: std::collections::HashMap::new(),
            object_counter_placement_count_this_turn: std::collections::HashMap::new(),
            pending_attack_trigger_events: Vec::new(),
            ability_resolutions_this_turn: HashMap::new(),
            graveyard_cast_permissions_used: HashSet::new(),
            graveyard_cast_permissions_used_per_type: HashSet::new(),
            pending_permanent_type_slot: None,
            hand_cast_free_permissions_used: HashSet::new(),
            alt_cost_grant_permissions_used: HashSet::new(),
            exile_play_permissions_used: HashSet::new(),
            exile_play_single_use_consumed: HashSet::new(),
            exile_cast_permissions_used: HashSet::new(),
            top_of_library_cast_permissions_used: HashSet::new(),
            cards_exiled_with_source_this_turn: HashMap::new(),
            first_card_drawn_this_turn: HashMap::new(),
            cards_drawn_this_turn: HashMap::new(),
            pending_miracle_offers: Vec::new(),
            pending_paradigm_remaining_offers: None,
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
            end_steps_started_this_turn: 0,
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
            batched_zone_change_trigger_fired: HashSet::new(),
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
            search_continuation_attach_host: None,
            pending_repeat_iteration: None,
            pending_repeated_optional_payment: None,
            pending_change_zone_iteration: None,
            pending_change_zone_in_flight: None,
            devour_eligible_snapshot: None,
            merged_card_component_route: None,
            pending_copy_token_resolution: None,
            pending_each_player_copy_chosen: None,
            pending_coin_flip: None,
            resolution_coin_flip: None,
            pending_repeat_until: None,
            pending_choose_one_of: None,
            pending_vote_ballot_iteration: None,
            pending_per_player_zone_choice: None,
            pending_player_scope_sacrifice_choice: None,
            pending_scoped_library_search: None,
            pending_library_search_delivery: None,
            pending_search_found_batch: None,
            pending_per_category_zone_choice: None,
            pending_counter_moves: None,
            pending_counter_removals: None,
            pending_batch_deliveries: None,
            pending_counter_additions: None,
            pending_proliferate_actions: None,
            pending_optional_effect: None,
            pending_optional_trigger_event: None,
            pending_optional_trigger_match_count: None,
            pending_choose_zone_trigger_context: None,
            may_trigger_auto_choices: Vec::new(),
            decision_templates: Vec::new(),
            priority_yields: Vec::new(),
            pending_begin_game_abilities: Vec::new(),
            resolving_begin_game_abilities: false,
            last_named_choice: None,
            last_chosen_damage_source: None,
            all_creature_types: Vec::new(),
            all_card_names: Arc::from([]),
            card_face_registry: Arc::new(HashMap::new()),
            meld_pair_registry: Arc::new(HashMap::new()),
            momir_pool: BTreeMap::new(),
            momir_pool_faces: Arc::new(HashMap::new()),
            log_player_names: Vec::new(),
            last_created_token_ids: Vec::new(),
            last_revealed_ids: Vec::new(),
            last_parent_target_missing_reason: None,
            private_look_ids: Vec::new(),
            private_look_player: None,
            last_zone_changed_ids: Vec::new(),
            last_vote_ballots: im::Vector::new(),
            player_actions_this_way: HashSet::new(),
            last_effect_amount: None,
            last_effect_excess_amount: None,
            die_result_this_resolution: None,
            last_effect_count: None,
            last_effect_counts_by_player: HashMap::new(),
            clause_minimum_snapshot: None,
            exiled_from_hand_this_resolution: 0,
            optional_cost_payments_this_resolution: 0,
            monarch: None,
            city_blessing: HashSet::new(),
            epic_effects: Vec::new(),
            restrictions: Vec::new(),
            pending_damage_replacements: Vec::new(),
            pending_step_end_mana_handlers: Vec::new(),
            pending_phase_transition_progress: None,
            deferred_step_trigger_resume: None,
            pending_team_draw_step: Vec::new(),
            pending_untap_declines: Vec::new(),
            current_trigger_event: None,
            announced_source_x: None,
            current_trigger_match_count: None,
            resolving_stack_entry: None,
            resolution_source_relatch: None,
            last_recast_context: None,
            current_trigger_events: Vec::new(),
            last_discover_value: None,
            stack_trigger_event_batches: HashMap::new(),
            lki_cache: HashMap::new(),
            lki_copiable_values: HashMap::new(),
            lki_by_incarnation: HashMap::new(),
            linked_exile_lki: HashMap::new(),
            cost_payment_failed_flag: false,
            pending_taps_for_mana_overrides: std::collections::HashMap::new(),
            current_triggered_mana_override: None,
            pending_cost_move_resume: None,
            pending_discard_for_cost: None,
            pending_cast: None,
            ring_level: HashMap::new(),
            ring_bearer: HashMap::new(),
            dungeon_progress: HashMap::new(),
            planar_deck: im::Vector::new(),
            planar_controller: None,
            planar_die_actions_this_turn: HashMap::new(),
            scheme_deck: im::Vector::new(),
            archenemy,
            initiative: None,
            combat_prevention_tally: None,
            cancelled_casts: Vec::new(),
            pending_activations: Vec::new(),
            commander_declined_zone_return: HashSet::new(),
            objects_that_dealt_damage: HashSet::new(),
            debug_mode: false,
            debug_permitted: BTreeSet::new(),
            unbounded_resources: BTreeMap::new(),
            unbounded_loop_enablers: BTreeMap::new(),
            unimplemented_oracle_ids: BTreeSet::new(),
            pending_trigger_abandons: Vec::new(),
            loop_detection: LoopDetectionMode::Off,
        }
    }

    /// Create a standard 2-player game (backward-compatible).
    pub fn new_two_player(seed: u64) -> Self {
        Self::new(FormatConfig::standard(), 2, seed)
    }

    /// CR 732.2a: adopt a match's immutable configuration, projecting the per-game
    /// runtime gate(s) it controls. The combo-detector opt-in lives on [`MatchConfig`]
    /// (chosen at match creation, whole-table, immutable during play); this is the
    /// single authority that projects it onto [`GameState::loop_detection`] — the flag
    /// the detector gates read. Called once per game at creation and at each
    /// between-games rebuild so a multi-game match keeps a consistent detector setting.
    pub fn set_match_config(&mut self, config: MatchConfig) {
        self.match_config = config;
        self.loop_detection = config.loop_detection;
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

    /// CR 603.5: Revoke a single stored "don't ask again" auto-choice for an
    /// optional ("may") trigger. The key already scopes to one player, source,
    /// and origin.
    pub fn remove_may_trigger_auto_choice(&mut self, key: &MayTriggerAutoChoiceKey) {
        self.may_trigger_auto_choices
            .retain(|record| record.key != *key);
    }

    /// CR 603.5: Revoke all stored "don't ask again" auto-choices belonging to
    /// `player` for optional ("may") triggers.
    pub fn clear_may_trigger_auto_choices(&mut self, player: PlayerId) {
        self.may_trigger_auto_choices
            .retain(|record| record.key.player != player);
    }

    /// CR 603.3b: upsert a trigger-ordering [`DecisionTemplate`], replacing any existing
    /// template with the same `(owner, key)`. Used by both tiers: the prompt path and
    /// the persistent-permute path register ephemeral markers; the live
    /// `OrderTriggers` submission records persistent ones.
    pub fn set_trigger_order_template(
        &mut self,
        tmpl: crate::analysis::decision_template::DecisionTemplate,
    ) {
        if let Some(existing) = self
            .decision_templates
            .iter_mut()
            .find(|t| t.owner == tmpl.owner && t.key == tmpl.key)
        {
            *existing = tmpl;
        } else {
            self.decision_templates.push(tmpl);
        }
    }

    /// CR 603.3b: first `owner`/`kind` template whose `key.sources` multiset **covers**
    /// `group_sources` (a shrinking deferred suffix stays covered). The caller supplies
    /// the group's source multiset in the tier-appropriate variant (`ThisObject` for the
    /// ephemeral consult, `AllCopies` for the persistent consult) — the `covers` match
    /// never crosses variants, so tier selection falls out of the source representation.
    pub fn find_trigger_order_template_for(
        &self,
        controller: PlayerId,
        kind: crate::analysis::decision_template::DecisionKind,
        group_sources: &[YieldTarget],
    ) -> Option<&crate::analysis::decision_template::DecisionTemplate> {
        self.decision_templates
            .iter()
            .find(|t| t.owner == controller && t.key.kind == kind && t.key.covers(group_sources))
    }

    /// CR 603.3b: revoke all of `actor`'s PERSISTENT (`AllCopies`-keyed) ordering
    /// preferences. Ephemeral markers are left to the boundary clear.
    pub fn clear_trigger_order_templates(&mut self, actor: PlayerId) {
        self.decision_templates
            .retain(|t| !(t.owner == actor && t.key.is_persistent()));
    }

    /// CR 603.3b resolution boundary: drop every EPHEMERAL (`ThisObject`-keyed)
    /// trigger-ordering marker. Called at each batch-completion point so no per-batch
    /// coverage marker survives into the next Priority frame. Idempotent (clearing an
    /// empty set is a no-op) — the callers guard on `deferred_triggers.is_empty()` so a
    /// mid-batch pause never triggers it. Persistent (`AllCopies`) templates survive.
    pub fn clear_ephemeral_trigger_order_templates(&mut self) {
        self.decision_templates.retain(|t| {
            !(t.key.kind == crate::analysis::decision_template::DecisionKind::TriggerOrdering
                && t.key.is_ephemeral())
        });
    }

    /// CR 117.3d: True when `player` has a standing yield matching the top stack
    /// entry, meaning they have pre-committed to pass priority while it resolves.
    /// Only triggered abilities can be yielded — spells, activated abilities, and
    /// keyword actions never match (a player never pre-declines those). A
    /// `ThisObject` yield matches only while the source keeps the same
    /// incarnation (CR 400.7); an `AllCopies` yield matches any trigger sharing
    /// the latched card identity, even after the original source ceases to exist.
    pub fn is_priority_yielded(&self, player: PlayerId, entry: &StackEntry) -> bool {
        match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id,
                ability,
                description,
                ..
            } => self.priority_yields.iter().any(|y| {
                y.player == player
                    && match &y.target {
                        YieldTarget::ThisObject {
                            source_id: yielded_id,
                            incarnation,
                            trigger_description,
                        } => {
                            // CR 400.7: incarnation identity — a None-yield
                            // matches a trigger that latched no incarnation
                            // (synthetic/delayed), Some matches the same epoch.
                            *source_id == *yielded_id
                                && ability.source_incarnation == *incarnation
                                && (trigger_description.is_none()
                                    || trigger_description.as_deref() == description.as_deref())
                        }
                        YieldTarget::AllCopies {
                            card_id,
                            trigger_description,
                        } => {
                            ability.source_card_id == Some(*card_id)
                                && (trigger_description.is_none()
                                    || trigger_description.as_deref() == description.as_deref())
                        }
                    }
            }),
            StackEntryKind::Spell { .. }
            | StackEntryKind::ActivatedAbility { .. }
            | StackEntryKind::KeywordAction { .. } => false,
        }
    }

    /// CR 400.7 identity latch: resolve a `YieldScope` for `source_id` into a
    /// concrete `YieldTarget` by scanning the stack (top-down) for that source's
    /// triggered ability and reading the identity it captured at push. Returns
    /// `None` — caller no-ops — when no matching triggered entry is on the stack,
    /// or when the requested `AllCopies` scope needs a `source_card_id` the
    /// trigger never latched. A `ThisObject` yield always resolves: a trigger
    /// with no `source_incarnation` latches `incarnation: None`, which matches
    /// only entries that likewise latched no incarnation (CR 400.7).
    pub fn resolve_yield_target_from_stack(
        &self,
        source_id: ObjectId,
        scope: YieldScope,
    ) -> Option<YieldTarget> {
        self.stack.iter().rev().find_map(|entry| match &entry.kind {
            StackEntryKind::TriggeredAbility {
                source_id: sid,
                ability,
                description,
                ..
            } if *sid == source_id => match scope {
                // CR 400.7: latch the incarnation identity (now Option — a
                // synthetic/delayed trigger with no `source_incarnation` still
                // yields, storing `None`) and the per-trigger description.
                YieldScope::ThisObject => Some(YieldTarget::ThisObject {
                    source_id,
                    incarnation: ability.source_incarnation,
                    trigger_description: description.clone(),
                }),
                YieldScope::AllCopies => {
                    ability
                        .source_card_id
                        .map(|card_id| YieldTarget::AllCopies {
                            card_id,
                            trigger_description: description.clone(),
                        })
                }
            },
            _ => None,
        })
    }

    /// CR 117.3d: Register a standing priority yield for `player`. No-op when an
    /// equal `(player, target)` yield is already stored (idempotent toggle).
    pub fn add_priority_yield(&mut self, player: PlayerId, target: YieldTarget) {
        if self
            .priority_yields
            .iter()
            .any(|y| y.player == player && y.target == target)
        {
            return;
        }
        self.priority_yields.push(PriorityYield { player, target });
    }

    /// CR 117.3d: Revoke a single standing yield for `player`.
    pub fn remove_priority_yield(&mut self, player: PlayerId, target: &YieldTarget) {
        self.priority_yields
            .retain(|y| !(y.player == player && y.target == *target));
    }

    /// CR 117.3d: Revoke all standing yields for `player`.
    pub fn clear_priority_yields(&mut self, player: PlayerId) {
        self.priority_yields.retain(|y| y.player != player);
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
                duration_subject: None,
                source_name,
            });
        self.layers_dirty.mark_full();
        id
    }

    /// CR 611.2b + CR 110.5d: bind a target-relative `ForAsLongAs` duration to a
    /// concrete object resolved at effect-resolution time, on the TCE that
    /// [`Self::add_transient_continuous_effect`] just created (addressed by its
    /// returned `id`). Used when the duration's tracked subject diverges from the
    /// effect's `affected` recipient — Zygon Infiltrator: the copy modification
    /// applies to the source, but the duration tracks the copy *target*'s tap
    /// state. Keeps construction in one authority (no second constructor); the
    /// only divergent caller is `effects/become_copy.rs`. Marks layers dirty so
    /// the duration re-evaluation picks up the binding.
    pub fn set_transient_duration_subject(&mut self, id: u64, subject: ObjectId) {
        if let Some(tce) = self
            .transient_continuous_effects
            .iter_mut()
            .find(|tce| tce.id == id)
        {
            tce.duration_subject = Some(subject);
            self.layers_dirty.mark_full();
        }
    }

    /// Migrate the pre-2026-05-09 audit M4 split-slot
    /// shape (`post_replacement_effect` + `post_replacement_resolved_effect`)
    /// into the unified `post_replacement_continuation` slot. Idempotent —
    /// no-op when both legacy slots are empty (the steady-state case once a
    /// post-load hop has run). Called from `finalize_public_state` so every
    /// deserialize boundary (engine-wasm restore, multiplayer host resume,
    /// gamePersistence rehydration) gets the migration without per-callsite
    /// plumbing. The Resolved arm wins when both legacy slots are
    /// (impossibly) populated, mirroring the pre-fold dispatcher precedence
    /// at `engine_replacement.rs::apply_pending_post_replacement_effect`.
    /// Is a post-replacement continuation waiting to drain?
    ///
    /// The scattered `post_replacement_continuation.is_some()` checks this replaces
    /// were asking exactly this: is there work that has NOT been taken yet. A drain
    /// that is mid-dispatch does not count — the old slot was already empty at that
    /// point, because the continuation had been moved out of it before dispatching.
    pub fn has_post_replacement_drain(&self) -> bool {
        self.post_replacement_drains.has_ready()
    }

    /// Install a ready continuation carrying no source, no inherited
    /// applied set and no prevented-event context.
    ///
    /// Policy is `Replace` — the shape the combat prevention riders use, which have
    /// always overwritten a resident continuation rather than deferring to it.
    pub fn install_ready_continuation(
        &mut self,
        continuation: crate::types::ability::PostReplacementContinuation,
    ) {
        self.post_replacement_drains.install(
            PostReplacementDrain::ready(continuation),
            ResidentDrainPolicy::Replace,
        );
    }

    /// The resident drain's continuation, if it has not been taken for
    /// dispatch yet.
    pub fn post_replacement_continuation(
        &self,
    ) -> Option<&crate::types::ability::PostReplacementContinuation> {
        self.post_replacement_drains
            .resident()
            .and_then(|drain| drain.ready_continuation())
    }

    /// CR 615.5: the resident drain's replacement source (the shield's own object).
    pub fn post_replacement_source(&self) -> Option<crate::types::identifiers::ObjectId> {
        self.post_replacement_drains
            .resident()
            .and_then(|drain| drain.source)
    }

    /// CR 615.5 + CR 609.7: the resident drain's *prevented-event* source — the
    /// damage dealer, not the shield.
    pub fn post_replacement_event_source(&self) -> Option<crate::types::identifiers::ObjectId> {
        self.post_replacement_drains
            .resident()
            .and_then(|drain| drain.event_source)
    }

    /// CR 615.5: the resident drain's prevented-event target.
    pub fn post_replacement_event_target(&self) -> Option<&crate::types::ability::TargetRef> {
        self.post_replacement_drains
            .resident()
            .and_then(|drain| drain.event_target.as_ref())
    }

    /// Clear the resident drain's replacement source while leaving the
    /// continuation itself resident.
    ///
    /// A real thing several callers need, not a convenience: a zone change's
    /// caller epilogue drains with the spell-resolution ctx and must not resolve
    /// `SelfRef` against the replacement's source.
    pub fn clear_post_replacement_source(&mut self) {
        if let Some(drain) = self.post_replacement_drains.resident_mut() {
            drain.source = None;
        }
    }

    pub fn migrate_post_replacement_continuation(&mut self) {
        // The canonical stack wins outright: every legacy slot is stale.
        if !self.post_replacement_drains.is_empty() {
            self.legacy_post_replacement_effect = None;
            self.legacy_post_replacement_resolved_effect = None;
            self.legacy_post_replacement_continuation = None;
            self.legacy_post_replacement_source = None;
            self.legacy_post_replacement_applied.clear();
            self.legacy_post_replacement_event_source = None;
            self.legacy_post_replacement_event_target = None;
            return;
        }

        // The continuation itself comes from whichever generation of the save
        // recorded it. The Resolved arm wins when both pre-fold slots are
        // (impossibly) populated, mirroring the pre-fold dispatcher precedence.
        let continuation = self
            .legacy_post_replacement_continuation
            .take()
            .or_else(|| {
                self.legacy_post_replacement_resolved_effect
                    .take()
                    .map(crate::types::ability::PostReplacementContinuation::Resolved)
            })
            .or_else(|| {
                self.legacy_post_replacement_effect
                    .take()
                    .map(crate::types::ability::PostReplacementContinuation::Template)
            });
        self.legacy_post_replacement_effect = None;
        self.legacy_post_replacement_resolved_effect = None;

        let Some(continuation) = continuation else {
            // No continuation means the companion values are orphans; drop them
            // rather than leaving them to bleed into an unrelated later drain.
            self.legacy_post_replacement_source = None;
            self.legacy_post_replacement_applied.clear();
            self.legacy_post_replacement_event_source = None;
            self.legacy_post_replacement_event_target = None;
            return;
        };

        self.post_replacement_drains.install(
            PostReplacementDrain {
                // A legacy save recorded a continuation that had not run, so it
                // deserializes as `Ready`. A save can never have captured one
                // mid-dispatch: the old slot was emptied before dispatching.
                status: DrainStatus::Ready(continuation),
                source: self.legacy_post_replacement_source.take(),
                applied: std::mem::take(&mut self.legacy_post_replacement_applied),
                event_source: self.legacy_post_replacement_event_source.take(),
                event_target: self.legacy_post_replacement_event_target.take(),
            },
            ResidentDrainPolicy::Replace,
        );
    }

    /// CR 121.2: Migrate the legacy single-slot `pending_multi_draw` save shape
    /// into [`Self::draw_sequences`] as a one-frame stack. Idempotent — a no-op
    /// once the legacy slot is empty (the steady state after one post-load hop).
    /// Called from `finalize_public_state` alongside
    /// [`Self::migrate_post_replacement_continuation`], so every deserialize
    /// boundary (engine-wasm restore, multiplayer host resume, gamePersistence
    /// rehydration) migrates without per-callsite plumbing.
    ///
    /// A legacy save can only ever have recorded ONE in-flight instruction, so it
    /// converts to exactly one frame. Nesting (CR 616.1g) that the old shape could
    /// not record is not invented here.
    pub fn migrate_pending_multi_draw(&mut self) {
        let Some(legacy) = self.legacy_pending_multi_draw.take() else {
            return;
        };
        // A canonical stack already present wins: the legacy slot is stale.
        if !self.draw_sequences.is_empty() {
            return;
        }
        let frame_id = self.draw_sequences.push(legacy.player, legacy.remaining);
        if let Some(frame) = self.draw_sequences.active_if(frame_id) {
            frame.accumulated = legacy.accumulated;
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
        // CR 104.4b: pip-id counter is a volatile monotonic field; zero it (like
        // next_object_id) so two otherwise-identical loop states compare equal.
        clone.next_pip_id = 0;
        clone.layers_dirty = LayersDirty::full();
        clone.public_state_dirty = PublicStateDirty::all_dirty();
        // PR-3 (Option C): snapshots stored in `loop_detect_ring` are produced BY this
        // method, so without clearing here each stored snapshot would carry a clone of
        // the live ring → recursive/quadratic growth. Cleared ⇒ every stored snapshot
        // has clone depth 1. Does not affect any comparison (the ring is eq-excluded).
        clone.loop_detect_ring.clear();
        // Private shortcut capabilities are live interaction state, never part
        // of a CR 104.4b position sample.
        clone.precast_shortcut_runtime = PrecastShortcutRuntime::default();
        // CR 104.4b + CR 400.7: the all-zone incarnation bump advances a source's
        // epoch on every zone change, so a mandatory loop that cycles its source's
        // zones would otherwise carry a growing `ResolvedAbility::source_incarnation`
        // into loop equality and never confirm a draw. Canonicalize it to `None`
        // across EVERY eq-compared carrier that transitively holds a
        // `ResolvedAbility`. (`pending_trigger_entry` is an `Option<ObjectId>`, not
        // an ability carrier, so it needs no normalization; `waiting_for` is
        // `Priority` at the post-pipeline loop-sample point and `priority_yields`
        // latches its `YieldTarget::ThisObject { incarnation }` once at
        // registration — both are loop-stable and carry no growing epoch.)
        for entry in clone.stack.iter_mut() {
            if let Some(ability) = entry.ability_mut() {
                ability.set_source_incarnation_recursive(None);
            }
        }
        if let Some(pt) = clone.pending_trigger.as_mut() {
            pt.ability.set_source_incarnation_recursive(None);
        }
        for ctx in clone.deferred_triggers.iter_mut() {
            ctx.pending.ability.set_source_incarnation_recursive(None);
        }
        if let Some(order) = clone.pending_trigger_order.as_mut() {
            for group in order.groups.iter_mut() {
                for ctx in group.triggers.iter_mut() {
                    ctx.pending.ability.set_source_incarnation_recursive(None);
                }
            }
        }
        for dt in clone.delayed_triggers.iter_mut() {
            dt.ability.set_source_incarnation_recursive(None);
        }
        for epic in clone.epic_effects.iter_mut() {
            epic.spell.set_source_incarnation_recursive(None);
        }

        // CR 104.4b + CR 732.2a: incarnation-versioned LKI is historical support
        // state, not independently loop-material state. Retain only snapshots
        // reachable from trigger-event carriers that can still resume or resolve:
        // stack/resolving entries, pending/deferred/ordering triggers, current and
        // batched trigger contexts, and continuation/optional-choice sidecars.
        // WaitingFor copies are intentionally not a separate authority: every
        // trigger-event-bearing prompt has one of those pending/continuation
        // carriers, and loop samples are taken at the post-pipeline Priority frame.
        let mut referenced_lki = HashSet::new();
        let mut record_event = |event: &GameEvent| {
            if let GameEvent::ZoneChanged {
                object_id, record, ..
            } = event
            {
                if let Some(incarnation) = record.entered_incarnation {
                    referenced_lki.insert(ObjectIncarnationRef::of(*object_id, incarnation));
                }
            }
        };

        for entry in &clone.stack {
            if let StackEntryKind::TriggeredAbility {
                trigger_event: Some(event),
                ..
            } = &entry.kind
            {
                record_event(event);
            }
        }
        if let Some(entry) = clone.resolving_stack_entry.as_ref() {
            if let StackEntryKind::TriggeredAbility {
                trigger_event: Some(event),
                ..
            } = &entry.kind
            {
                record_event(event);
            }
        }
        if let Some(pending) = clone.pending_trigger.as_ref() {
            if let Some(event) = pending.trigger_event.as_ref() {
                record_event(event);
            }
        }
        for event in &clone.pending_trigger_event_batch {
            record_event(event);
        }
        for context in &clone.deferred_triggers {
            if let Some(event) = context.pending.trigger_event.as_ref() {
                record_event(event);
            }
            for event in &context.trigger_events {
                record_event(event);
            }
        }
        if let Some(order) = clone.pending_trigger_order.as_ref() {
            for context in order.groups.iter().flat_map(|group| group.triggers.iter()) {
                if let Some(event) = context.pending.trigger_event.as_ref() {
                    record_event(event);
                }
                for event in &context.trigger_events {
                    record_event(event);
                }
            }
        }
        if let Some(event) = clone.current_trigger_event.as_ref() {
            record_event(event);
        }
        for event in &clone.current_trigger_events {
            record_event(event);
        }
        for events in clone.stack_trigger_event_batches.values() {
            for event in events {
                record_event(event);
            }
        }
        if let Some(event) = clone.pending_optional_trigger_event.as_ref() {
            record_event(event);
        }
        if let Some(context) = clone.pending_choose_zone_trigger_context.as_ref() {
            if let Some(event) = context.event.as_ref() {
                record_event(event);
            }
            for event in &context.events {
                record_event(event);
            }
        }
        if let Some(context) = clone
            .pending_continuation
            .as_ref()
            .and_then(|continuation| continuation.trigger_context.as_ref())
        {
            if let Some(event) = context.event.as_ref() {
                record_event(event);
            }
            for event in &context.events {
                record_event(event);
            }
        }

        clone.lki_by_incarnation.retain(|object_id, history| {
            history.retain(|incarnation, _| {
                referenced_lki.contains(&ObjectIncarnationRef::of(*object_id, *incarnation))
            });
            !history.is_empty()
        });
        clone
    }

    /// PR-3 (Option C): push one NORMALIZED post-resolution snapshot onto the
    /// CR 732.2a loop-detection ring, evicting the oldest at `LOOP_DETECT_RING_CAP`.
    /// The snapshot is `normalize_for_loop`d (its own ring cleared, see above) and
    /// `Arc`-shared so storage is O(1) per element. Called only from the post-pipeline
    /// frame behind the refill gate (`game::engine::pass_priority_once_with_pipeline`,
    /// after `run_post_action_pipeline` places refilling triggers).
    pub(crate) fn record_loop_detect_sample(&mut self) {
        if self.loop_detect_ring.len() == LOOP_DETECT_RING_CAP {
            self.loop_detect_ring.pop_front();
        }
        let snapshot = std::sync::Arc::new(self.normalize_for_loop());
        self.loop_detect_ring.push_back(snapshot);
    }

    /// CR 732.2a: record that an unbounded (net-progress) loop under `controller`
    /// pumps `axes`. The single write authority for `unbounded_resources` —
    /// every producer routes through here, never mutating the map inline. Two
    /// producers exist: the infinite-mana debug toggle
    /// (`DebugAction::SetInfiniteMana`, ungated) and the live combo-detector at the
    /// reconcile seam (`game::engine::reconcile_terminal_result`), which persists a
    /// confirmed loop's `delta.unbounded_axes_for(winner)` — the same axes
    /// `detect_loop` names in `LoopCertificate.unbounded` — but ONLY when the
    /// user-controllable `GameState::loop_detection` gate is `On`. Idempotent
    /// set-union: storing exactly the axes it is given
    /// (so a mana toggle stores its six `Mana(_)` axes, a drain certificate stores
    /// its `Life`/`DamageDealt` axes) without clobbering axes a prior producer
    /// already recorded for the same controller.
    pub fn mark_unbounded_loop(&mut self, controller: PlayerId, axes: &[ResourceAxis]) {
        let entry = self.unbounded_resources.entry(controller).or_default();
        entry.extend(axes.iter().copied());
    }

    /// CR 104.4b / CR 110.1: single write authority for `unbounded_loop_enablers` —
    /// only the Interactive B5 bridge arm (`interactive_loop_bridge` Path C) calls
    /// this. Overwrites (idempotent re-registration each re-detection beat with the
    /// same stable board). A no-op for an empty set (nothing to defuse on later).
    pub fn register_unbounded_loop_enablers(
        &mut self,
        controller: PlayerId,
        enablers: BTreeSet<ObjectId>,
    ) {
        if enablers.is_empty() {
            return;
        }
        self.unbounded_loop_enablers.insert(controller, enablers);
    }

    /// CR 732.2a: clear every unbounded-resource axis recorded for `controller`.
    /// Whole-player clear: with the infinite-mana toggle as the only PR-6 producer
    /// this matches today's all-or-nothing disable; an axis-scoped clear can be
    /// added when multiple producers coexist on one controller.
    pub fn clear_unbounded_loop(&mut self, controller: PlayerId) {
        self.unbounded_resources.remove(&controller);
        self.unbounded_loop_enablers.remove(&controller); // keep the two maps in lockstep
    }
}

/// PR-3 (Option C): max retained CR 732.2a loop-detection snapshots. A determinate
/// drain has period ≤ 2; 16 covers ≥ 8 cycles and any loop whose repeating phase
/// begins within a 16-resolution preamble. A longer-period/longer-preamble loop
/// simply falls back to the natural CR 704.5a SBA death (fail-safe — never a wrong
/// win). Kept small because the live `GameState` carries it through every clone.
const LOOP_DETECT_RING_CAP: usize = 16;

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
pub(crate) fn objects_content_eq(
    a: &im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>,
    b: &im::HashMap<ObjectId, GameObject, rustc_hash::FxBuildHasher>,
) -> bool {
    a.len() == b.len()
        && a.iter()
            .all(|(id, x)| b.get(id).is_some_and(|y| object_content_eq(x, y)))
}

/// CR 104.4b: per-object mutable-content equality — the single-authority row
/// comparator for [`objects_content_eq`] and the PR-7 Phase 4a object-growth
/// cover gate (`analysis::resource::board_covers`, the non-grown complement).
///
/// The compared set is the bucket-(i) partition of §5.2c (see
/// `_gameobject_partition_is_total`): every per-object field a MANDATORY action can
/// change on a stable (same-zone) object between two loop frames. Fields omitted
/// here are justified by write site, not doc-string — volatile layer identity
/// (`timestamp`/`incarnation`/`transformation_count`), projected P/T, cast-fact
/// latches co-variate of a
/// compared field, monotone-saturating latches (`foretold`/`monstrous`/…), and
/// layer-derived characteristics (firewall-scanned statics) — see §5.2c.
///
/// Strictness here is FAIL-SAFE for the shared 2p CR 104.4b path: a stricter
/// equality can only SUPPRESS a wrongful draw, and every compared field represents
/// REAL accumulated progress, so two states differing in it are correctly NOT a
/// repeat.
pub(crate) fn object_content_eq(x: &GameObject, y: &GameObject) -> bool {
    x.controller == y.controller
        && x.zone == y.zone
        && x.tapped == y.tapped
        && x.face_down == y.face_down
        && x.flipped == y.flipped
        && x.transformed == y.transformed
        // CR 712.8a: MDFC back-face toggle — oscillates without changing zone or
        // objects.len().
        && x.modal_back_face == y.modal_back_face
        // CR 702.26: phasing is mutable per-object status that leaves zone and
        // objects.len() unchanged, so two states differing only in phased-in/out
        // must not compare equal — else a loop that phases a permanent in and out
        // is a wrongful CR 104.4b draw.
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
        // §5.2c ADD set (v4): firewall-blind numeric/growable accumulators and
        // oscillating designations that a loop body can drift on a stable object.
        && x.intensity == y.intensity // Alchemy Intensify accumulator
        && x.perpetual_mods == y.perpetual_mods // perpetual-edit accumulator
        && x.stickers == y.stickers // CR 123.1 sticker accumulator
        && x.class_level == y.class_level // CR 716.3 level-up accumulator
        && x.contraption_sprocket == y.contraption_sprocket
        && x.is_suspected == y.is_suspected // CR 701.60a designation
        && x.prepared == y.prepared // SOS prepare/unprepare toggle
        && x.room_unlocks == y.room_unlocks // CR 709.5c door lock/unlock
        // §5.2c ADD set (v5, S6): firewall-blind per-iteration accumulators on
        // live battlefield/exile objects.
        && x.chosen_attributes == y.chosen_attributes // CR 205.2 remember/choose accumulator
        && x.goaded_by == y.goaded_by // CR 701.15c goad set
        && x.detained_by == y.detained_by // CR 701.35a detain set
        && x.casting_permissions == y.casting_permissions // CR 715.3d exile-grant Vec
        && x.saddled_by == y.saddled_by // CR 702.171c saddle set
}

/// CR 104.4b compile-time totality guard for the object-growth cover gate's
/// GameState axis (`analysis::resource::eq_except_growable`, which reuses
/// `impl PartialEq for GameState` wholesale after stripping grown objects). This
/// no-`..` destructure breaks the build the instant a GameState field is added,
/// forcing a reviewer to decide whether `PartialEq` compares it — so no future
/// field can become a hidden per-cycle accumulator that rides a covering pair to a
/// false CR 732.2a win. Mirror of `_gameobject_partition_is_total` (§5.2b).
#[cfg(test)]
fn _gamestate_partition_is_total(s: &GameState) {
    let GameState {
        turn_number: _,
        active_player: _,
        phase: _,
        players: _,
        priority_player: _,
        turn_decision_controller: _,
        turn_decision_control_timestamp: _,
        active_full_turn_control: _,
        active_combat_phase_control: _,
        active_library_searches: _,
        active_search_decision_controls: _,
        objects: _,
        next_object_id: _,
        next_pip_id: _,
        active_payment_pins: _,
        active_casting_permission_index: _,
        battlefield: _,
        stack: _,
        stack_paid_facts: _,
        exile: _,
        command_zone: _,
        rng_seed: _,
        rng_word_pos: _,
        rng: _,
        combat: _,
        waiting_for: _,
        has_pending_cast: _,
        lands_played_this_turn: _,
        max_lands_per_turn: _,
        priority_pass_count: _,
        pending_replacement: _,
        replacement_may_cost_paused: _,
        post_replacement_drains: _,
        legacy_post_replacement_continuation: _,
        legacy_post_replacement_effect: _,
        legacy_post_replacement_resolved_effect: _,
        legacy_post_replacement_source: _,
        legacy_post_replacement_applied: _,
        legacy_post_replacement_event_source: _,
        legacy_post_replacement_event_target: _,
        post_replacement_token_choice_applied: _,
        pending_connive_reentry: _,
        legacy_pending_multi_draw: _,
        draw_sequences: _,
        pending_life_total_assignment: _,
        pending_spell_resolution: _,
        pending_mutate_merge: _,
        deferred_entry_events: _,
        layers_dirty: _,
        static_gate_truth: _,
        trigger_index: _,
        replacement_index: _,
        static_source_index: _,
        static_mode_presence: _,
        loop_detect_ring: _,
        precast_shortcut_runtime: _,
        next_timestamp: _,
        public_state_dirty: _,
        state_revision: _,
        transient_continuous_effects: _,
        next_continuous_effect_id: _,
        attribution: _,
        remote_type_layer_recipients: _,
        day_night: _,
        spells_cast_this_turn: _,
        spells_cast_last_turn: _,
        cancelled_casts: _,
        pending_activations: _,
        pending_trigger: _,
        pending_trigger_event_batch: _,
        pending_trigger_entry: _,
        deferred_triggers: _,
        pending_trigger_order: _,
        consumed_before_priority_trigger_events: _,
        exile_links: _,
        paradigm_primed: _,
        delayed_triggers: _,
        tracked_object_sets: _,
        next_tracked_set_id: _,
        chain_tracked_set_id: _,
        tracked_set_member_causes: _,
        commander_cast_count: _,
        commander_cast_owners: _,
        commander_declined_zone_return: _,
        objects_that_dealt_damage: _,
        extra_turns: _,
        turns_to_skip: _,
        steps_to_skip: _,
        combat_phase_skip_next_turn: _,
        scheduled_turn_controls: _,
        extra_phases: _,
        extra_phase_resume: _,
        turn_direction: _,
        current_combat_attacker_restriction: _,
        current_combat_attacker_restriction_source: _,
        seat_order: _,
        format_config: _,
        eliminated_players: _,
        commander_damage: _,
        priority_passes: _,
        auto_pass: _,
        phase_stops: _,
        lands_tapped_for_mana: _,
        prepaid_mulligan_bottoms: _,
        debug_mode: _,
        debug_permitted: _,
        unbounded_resources: _,
        unbounded_loop_enablers: _,
        unimplemented_oracle_ids: _,
        pending_trigger_abandons: _,
        loop_detection: _,
        match_config: _,
        match_phase: _,
        match_score: _,
        game_number: _,
        current_starting_player: _,
        next_game_chooser: _,
        deck_pools: _,
        outside_game_cards_brought_in: _,
        sideboard_submitted: _,
        triggers_fired_this_turn: _,
        trigger_fire_counts_this_turn: _,
        triggers_fired_this_turn_per_opponent: _,
        triggers_fired_this_game: _,
        activated_abilities_this_turn: _,
        activated_abilities_this_game: _,
        crew_activated_this_turn: _,
        loyalty_abilities_activated_this_turn: _,
        extra_loyalty_activations_this_turn: _,
        exerted_this_turn: _,
        object_tap_count_this_turn: _,
        object_counter_placement_count_this_turn: _,
        pending_attack_trigger_events: _,
        ability_resolutions_this_turn: _,
        graveyard_cast_permissions_used: _,
        graveyard_cast_permissions_used_per_type: _,
        pending_permanent_type_slot: _,
        hand_cast_free_permissions_used: _,
        alt_cost_grant_permissions_used: _,
        exile_play_permissions_used: _,
        exile_play_single_use_consumed: _,
        exile_cast_permissions_used: _,
        top_of_library_cast_permissions_used: _,
        cards_exiled_with_source_this_turn: _,
        first_card_drawn_this_turn: _,
        cards_drawn_this_turn: _,
        pending_miracle_offers: _,
        pending_paradigm_remaining_offers: _,
        spells_cast_this_game: _,
        spells_cast_this_game_by_player: _,
        spells_cast_this_turn_by_player: _,
        lands_played_this_turn_by_player: _,
        players_who_searched_library_this_turn: _,
        player_actions_this_turn: _,
        players_attacked_this_step: _,
        players_attacked_this_turn: _,
        attacking_creatures_this_turn: _,
        attacked_defenders_this_turn: _,
        creature_attacked_defenders_this_turn: _,
        combat_phases_started_this_turn: _,
        end_steps_started_this_turn: _,
        creatures_attacked_this_turn: _,
        attacker_declarations_this_turn: _,
        creatures_blocked_this_turn: _,
        players_who_created_token_this_turn: _,
        created_tokens_this_turn: _,
        counter_added_this_turn: _,
        players_who_discarded_card_this_turn: _,
        cards_discarded_this_turn_by_player: _,
        players_who_sacrificed_artifact_this_turn: _,
        sacrificed_permanents_this_turn: _,
        zone_changes_this_turn: _,
        batched_zone_change_trigger_fired: _,
        battlefield_entries_this_turn: _,
        damage_dealt_this_turn: _,
        assassin_or_commander_dealt_combat_damage_this_turn: _,
        creature_types_dealt_combat_damage_this_turn: _,
        mana_spent_on_spells_this_turn: _,
        pending_spell_cost_reductions: _,
        pending_next_spell_modifiers: _,
        pending_etb_counters: _,
        modal_modes_chosen_this_turn: _,
        modal_modes_chosen_this_game: _,
        revealed_cards: _,
        public_revealed_cards: _,
        pending_continuation: _,
        search_continuation_attach_host: _,
        pending_repeat_iteration: _,
        pending_repeated_optional_payment: _,
        pending_change_zone_iteration: _,
        pending_change_zone_in_flight: _,
        devour_eligible_snapshot: _,
        merged_card_component_route: _,
        pending_copy_token_resolution: _,
        pending_each_player_copy_chosen: _,
        pending_coin_flip: _,
        resolution_coin_flip: _,
        pending_repeat_until: _,
        pending_choose_one_of: _,
        pending_vote_ballot_iteration: _,
        pending_per_player_zone_choice: _,
        pending_per_category_zone_choice: _,
        pending_counter_moves: _,
        pending_counter_removals: _,
        pending_batch_deliveries: _,
        pending_counter_additions: _,
        pending_proliferate_actions: _,
        pending_optional_effect: _,
        pending_optional_trigger_event: _,
        pending_optional_trigger_match_count: _,
        pending_choose_zone_trigger_context: _,
        may_trigger_auto_choices: _,
        decision_templates: _,
        priority_yields: _,
        pending_begin_game_abilities: _,
        resolving_begin_game_abilities: _,
        last_named_choice: _,
        last_chosen_damage_source: _,
        all_creature_types: _,
        all_card_names: _,
        card_face_registry: _,
        meld_pair_registry: _,
        momir_pool: _,
        momir_pool_faces: _,
        log_player_names: _,
        last_created_token_ids: _,
        last_revealed_ids: _,
        last_parent_target_missing_reason: _,
        private_look_ids: _,
        private_look_player: _,
        last_zone_changed_ids: _,
        last_vote_ballots: _,
        player_actions_this_way: _,
        last_effect_amount: _,
        last_effect_excess_amount: _,
        die_result_this_resolution: _,
        last_effect_count: _,
        last_effect_counts_by_player: _,
        clause_minimum_snapshot: _,
        exiled_from_hand_this_resolution: _,
        optional_cost_payments_this_resolution: _,
        monarch: _,
        city_blessing: _,
        epic_effects: _,
        restrictions: _,
        pending_damage_replacements: _,
        pending_step_end_mana_handlers: _,
        pending_phase_transition_progress: _,
        deferred_step_trigger_resume: _,
        pending_team_draw_step: _,
        pending_untap_declines: _,
        current_trigger_event: _,
        current_trigger_match_count: _,
        // CR 107.3a announce-scoped carrier, cleared at each `resolve_top`; a decision
        // context, not durable board state — like `current_trigger_event`, it is not a
        // per-cycle accumulator and PartialEq does not compare it.
        announced_source_x: _,
        resolving_stack_entry: _,
        current_trigger_events: _,
        stack_trigger_event_batches: _,
        lki_cache: _,
        lki_copiable_values: _,
        lki_by_incarnation: _,
        linked_exile_lki: _,
        cost_payment_failed_flag: _,
        pending_taps_for_mana_overrides: _,
        current_triggered_mana_override: _,
        pending_cost_move_resume: _,
        pending_discard_for_cost: _,
        pending_cast: _,
        ring_level: _,
        ring_bearer: _,
        dungeon_progress: _,
        planar_deck: _,
        planar_controller: _,
        planar_die_actions_this_turn: _,
        scheme_deck: _,
        archenemy: _,
        initiative: _,
        combat_prevention_tally: _,
        // Post-rebase upstream additions (v0.21.x: #5515 discover + liminal mechanic).
        // Strict-compared by eq_except_growable's GameState PartialEq reuse (fail-safe:
        // a differing value is correctly not a fixed-point repeat); object-growth loops
        // never involve these, so no certification-death.
        liminal_entries: _,
        pending_liminal_entry_resume: _,
        last_discover_value: _,
        // Post-rebase upstream additions (rebased onto d1a1e995e), classified by ONE-SIDED-SAFETY
        // (COMPARED is fail-safe; EXCLUSION is the fail-DANGEROUS direction — a field is excluded
        // ONLY when COMPARING it would break legitimate loop detection):
        //   - `pending_player_scope_sacrifice_choice`: COMPARED (upstream's `impl PartialEq`) — a
        //     paused sacrifice-choice interaction state; a differing value is correctly not a
        //     fixed-point repeat.
        //   - `pending_scoped_library_search`: COMPARED (upstream's `impl PartialEq`) — a
        //     paused multi-player search-selection state; a differing selection or player is
        //     correctly not a fixed-point repeat.
        //   - `post_replacement_token_substitution_count` (CR 614.1a copy-token "that many" count):
        //     COMPARED — upstream's PartialEq excludes it, but excluding a COUNT from the cover gate
        //     is the fail-DANGEROUS direction, so `eq_except_growable` (resource.rs) compares it
        //     explicitly. It is `None` at every sample beat (cleared whenever `waiting_for ==
        //     Priority`, effects/mod.rs:759) or a constant direct-assigned count across a real
        //     copy-token loop, so COMPARING never suppresses a legitimate loop's detection.
        pending_player_scope_sacrifice_choice: _,
        pending_scoped_library_search: _,
        pending_library_search_delivery: _,
        pending_search_found_batch: _,
        post_replacement_token_substitution_count: _,
        //   - `last_recast_context` (PR-7 Phase 4d-ii object-growth recast snapshot):
        //     EXCLUDED from `impl PartialEq for GameState` (a transient decision context, not
        //     durable board state), but COMPARED explicitly in `eq_except_growable` /
        //     `loop_states_equal_modulo_resources` (fail-closed one-sided-safety — its fields
        //     are loop-INVARIANT across a homogeneous recast, so COMPARING never suppresses a
        //     legitimate loop; a heterogeneous recast is correctly caught and rejected).
        last_recast_context: _,
        //   - `resolution_source_relatch` (CR 400.7j self-move re-latch): EXCLUDED-REQUIRED (measured
        //     by ordering trace, not doc-trust). The clear at stack.rs:194 fires at the START of the
        //     NEXT resolution, while `record_loop_detect_sample` fires at the Priority window AFTER
        //     this resolution's self-move SET it (zones.rs:610) — so at the sample beat it HOLDS this
        //     iteration's `current_incarnation`, which bumps every iteration. COMPARING it would make
        //     every self-moving loop compare UNEQUAL (a false-negative — it would make the 4d
        //     Sprout-Swarm buyback loop undetectable). It is an incarnation/timestamp identity, and
        //     object-growth lives in `objects` (stripped+compared by `eq_except_growable`), so
        //     excluding this single-object identity field cannot hide growth.
        resolution_source_relatch: _,
    } = s;
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
            && self.turn_decision_control_timestamp == other.turn_decision_control_timestamp
            && self.active_full_turn_control == other.active_full_turn_control
            && self.active_combat_phase_control == other.active_combat_phase_control
            && self.active_library_searches == other.active_library_searches
            && self.active_search_decision_controls == other.active_search_decision_controls
            && self.objects.len() == other.objects.len()
            && self.next_object_id == other.next_object_id
            && self.next_pip_id == other.next_pip_id
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
            && self.pending_connive_reentry == other.pending_connive_reentry
            // CR 104.4b: position, not history — see `DrawSequenceStack::loop_equal`.
            // Comparing the stack structurally would fold the monotonic frame-ID
            // allocator into loop equality and silently disable loop detection.
            && self.draw_sequences.loop_equal(&other.draw_sequences)
            && self.pending_life_total_assignment == other.pending_life_total_assignment
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
            // `static_mode_presence` is INTENTIONALLY excluded for the same reason
            // (CR 104.4b) — a derived O(1) presence cache rebuilt from
            // `game_functioning_statics`; it must not perturb loop-detection equality.
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
            && self.objects_that_dealt_damage == other.objects_that_dealt_damage
            && self.extra_turns == other.extra_turns
            && self.turns_to_skip == other.turns_to_skip
            && self.steps_to_skip == other.steps_to_skip
            && self.combat_phase_skip_next_turn == other.combat_phase_skip_next_turn
            && self.scheduled_turn_controls == other.scheduled_turn_controls
            && self.extra_phases == other.extra_phases
            && self.extra_phase_resume == other.extra_phase_resume
            && self.turn_direction == other.turn_direction
            && self.current_combat_attacker_restriction
                == other.current_combat_attacker_restriction
            && self.current_combat_attacker_restriction_source
                == other.current_combat_attacker_restriction_source
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
            && self.alt_cost_grant_permissions_used == other.alt_cost_grant_permissions_used
            && self.exile_play_permissions_used == other.exile_play_permissions_used
            && self.exile_play_single_use_consumed == other.exile_play_single_use_consumed
            && self.exile_cast_permissions_used == other.exile_cast_permissions_used
            && self.cards_exiled_with_source_this_turn == other.cards_exiled_with_source_this_turn
            && self.first_card_drawn_this_turn == other.first_card_drawn_this_turn
            && self.cards_drawn_this_turn == other.cards_drawn_this_turn
            && self.pending_miracle_offers == other.pending_miracle_offers
            && self.pending_paradigm_remaining_offers == other.pending_paradigm_remaining_offers
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
            && self.end_steps_started_this_turn == other.end_steps_started_this_turn
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
            && self.batched_zone_change_trigger_fired == other.batched_zone_change_trigger_fired
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
            && self.pending_repeated_optional_payment == other.pending_repeated_optional_payment
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
            && self.pending_each_player_copy_chosen == other.pending_each_player_copy_chosen
            && self.pending_coin_flip == other.pending_coin_flip
            // CR 104.4b: volatile resolution-scoped flip result. A flip already
            // advances `state.rng`, so iterations differ regardless; comparing
            // this field never masks a real repeat (safe to include).
            && self.resolution_coin_flip == other.resolution_coin_flip
            && self.pending_repeat_until == other.pending_repeat_until
            && self.pending_choose_one_of == other.pending_choose_one_of
            && self.pending_vote_ballot_iteration == other.pending_vote_ballot_iteration
            && self.pending_per_player_zone_choice == other.pending_per_player_zone_choice
            && self.pending_player_scope_sacrifice_choice
                == other.pending_player_scope_sacrifice_choice
            && self.pending_scoped_library_search == other.pending_scoped_library_search
            && self.pending_library_search_delivery == other.pending_library_search_delivery
            && self.pending_search_found_batch == other.pending_search_found_batch
            && self.pending_counter_moves == other.pending_counter_moves
            && self.pending_counter_removals == other.pending_counter_removals
            && self.pending_batch_deliveries == other.pending_batch_deliveries
            && self.pending_counter_additions == other.pending_counter_additions
            && self.pending_proliferate_actions == other.pending_proliferate_actions
            && self.pending_cost_move_resume == other.pending_cost_move_resume
            && self.may_trigger_auto_choices == other.may_trigger_auto_choices
            && self.decision_templates == other.decision_templates
            && self.priority_yields == other.priority_yields
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
            && self.pending_choose_zone_trigger_context
                == other.pending_choose_zone_trigger_context
            && self.exiled_from_hand_this_resolution == other.exiled_from_hand_this_resolution
            // CR 603.12a: K is nonzero AT the per-iteration `OptionalEffectChoice`
            // pause (a serde boundary across separate `apply()` calls). It is
            // serialized-when-nonzero and eq-included — mirroring
            // `exiled_from_hand_this_resolution` — so a save/restore mid-payment-loop
            // preserves the reflexive modal cap (CR 700.2d).
            && self.optional_cost_payments_this_resolution
                == other.optional_cost_payments_this_resolution
            && self.lki_cache == other.lki_cache
            && self.lki_copiable_values == other.lki_copiable_values
            && self.lki_by_incarnation == other.lki_by_incarnation
            && self.city_blessing == other.city_blessing
            && self.planar_deck == other.planar_deck
            && self.planar_controller == other.planar_controller
            && self.planar_die_actions_this_turn == other.planar_die_actions_this_turn
            && self.scheme_deck == other.scheme_deck
            && self.archenemy == other.archenemy
    }
}

impl Eq for GameState {}

#[cfg(test)]
mod active_search_provenance_tests {
    use super::*;

    fn record() -> ActiveLibrarySearch {
        ActiveLibrarySearch::try_new(
            PlayerId(1),
            PlayerId(2),
            Some(PlayerId(2)),
            vec![PlayerId(1), PlayerId(3)],
            vec![(
                PlayerId(2),
                Zone::Library,
                ObjectIncarnationRef::of(ObjectId(9), 4),
            )],
        )
        .unwrap()
    }

    #[test]
    fn active_search_rejects_each_contradictory_provenance_shape() {
        assert_eq!(
            ActiveLibrarySearch::try_new(
                PlayerId(1),
                PlayerId(2),
                Some(PlayerId(2)),
                vec![PlayerId(1)],
                vec![(
                    PlayerId(3),
                    Zone::Hand,
                    ObjectIncarnationRef::of(ObjectId(9), 4),
                )],
            ),
            Err(ActiveLibrarySearchInvariantError::TupleOwnerMismatch)
        );
        assert_eq!(
            ActiveLibrarySearch::try_new(
                PlayerId(1),
                PlayerId(2),
                Some(PlayerId(3)),
                vec![PlayerId(1)],
                Vec::new(),
            ),
            Err(ActiveLibrarySearchInvariantError::EffectiveLibraryOwnerMismatch)
        );
        assert_eq!(
            ActiveLibrarySearch::try_new(
                PlayerId(1),
                PlayerId(2),
                None,
                vec![PlayerId(1)],
                vec![(
                    PlayerId(2),
                    Zone::Library,
                    ObjectIncarnationRef::of(ObjectId(9), 4),
                )],
            ),
            Err(ActiveLibrarySearchInvariantError::LibraryTupleWithoutEffectiveLibrary)
        );
        assert!(ActiveLibrarySearch::try_new(
            PlayerId(1),
            PlayerId(2),
            Some(PlayerId(2)),
            vec![PlayerId(1)],
            Vec::new(),
        )
        .is_ok());
    }

    #[test]
    fn validating_maps_reject_key_mismatch_and_duplicate_keys() {
        let mut searches = ActiveLibrarySearches::default();
        searches.insert(record());
        let json = serde_json::to_string(&searches).unwrap();
        assert_eq!(
            serde_json::from_str::<ActiveLibrarySearches>(&json).unwrap(),
            searches
        );

        let value = serde_json::to_value(record()).unwrap();
        let mismatch = serde_json::json!({ "0": value });
        assert!(serde_json::from_value::<ActiveLibrarySearches>(mismatch).is_err());

        let value = serde_json::to_string(&record()).unwrap();
        let duplicate = format!(r#"{{"1":{value},"1":{value}}}"#);
        assert!(serde_json::from_str::<ActiveLibrarySearches>(&duplicate).is_err());

        let decision = serde_json::json!({
            "0": {
                "searcher": 1,
                "authority": { "type": "searcher_fallback" }
            }
        });
        assert!(serde_json::from_value::<ActiveSearchDecisionControls>(decision).is_err());
    }
}

/// Default pile source is Battlefield (backward-compatible with pre-existing
/// serialized `WaitingFor::SeparatePiles*` states).
fn default_pile_source_battlefield() -> PileSource {
    PileSource::Battlefield
}

#[cfg(test)]
mod drain_stack_reentrancy_tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PostReplacementContinuation,
    };

    fn ready_drain(name: &str) -> PostReplacementDrain {
        PostReplacementDrain::ready(PostReplacementContinuation::Template(Box::new(
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::unimplemented(name, "drain-reentrancy fixture"),
            ),
        )))
    }

    /// CR 616.1g: a continuation that is RUNNING must not block a new one from
    /// being installed.
    ///
    /// A drain stays resident while it dispatches (its event context must remain
    /// readable — CR 615.5). But its continuation has already been taken, so it is
    /// no longer *pending work*. If the running continuation causes a fresh
    /// replacement to stash a post-effect — a continuation draws, that draw is
    /// replaced, and the replacement carries a mandatory post-effect (Jace Wielder
    /// of Mysteries' win, Abundance's reveal-until) — that post-effect MUST install.
    ///
    /// The predecessor slot got this right for the wrong reason: it moved the
    /// continuation out of the slot before dispatching, so the slot read empty and
    /// the re-entrant stash landed. Guarding the stack on "is a drain resident?"
    /// instead of "is a drain READY?" silently drops it, and the nested post-effect
    /// never runs — `draw_through_replacement` gates its drain on
    /// `has_post_replacement_drain()`, which reports only Ready drains.
    ///
    /// The correct predicate is the one `has_ready` already documents.
    #[test]
    fn keep_resident_does_not_drop_a_stash_arriving_while_the_outer_drain_dispatches() {
        let mut stack = PostReplacementDrainStack::default();

        // Outer replacement stashes its continuation, then begins running it.
        assert!(stack.install(ready_drain("outer"), ResidentDrainPolicy::KeepResident));
        let taken = stack.begin_dispatch();
        assert!(
            taken.is_some(),
            "the outer continuation is handed out to run"
        );

        // The outer drain is still resident (its CR 615.5 event context must stay
        // readable) but it is no longer pending work.
        assert!(!stack.is_empty(), "the dispatching drain stays resident");
        assert!(
            !stack.has_ready(),
            "a Dispatching drain is not pending work — this is what the old slot's \
             emptiness stood for"
        );

        // The running continuation now causes a fresh replacement to stash a
        // mandatory post-effect. It MUST install; dropping it strands the post-effect.
        let installed = stack.install(ready_drain("nested"), ResidentDrainPolicy::KeepResident);
        assert!(
            installed,
            "a stash arriving while the outer continuation is DISPATCHING must install, \
             not be dropped: the old code took the continuation out of the slot before \
             dispatching, so the slot read empty and this landed. Guarding on \
             `!drains.is_empty()` instead of `has_ready()` silently strands every nested \
             mandatory post-effect (Jace win, Abundance reveal-until)."
        );
        assert!(
            stack.has_ready(),
            "the nested post-effect must be visible as pending work — \
             draw_through_replacement gates its drain on exactly this predicate"
        );
    }

    /// `KeepResident` drops a stash that arrives while a READY drain is pending.
    ///
    /// This pins the drop as a **leak-guard**, which is what it actually is — not,
    /// as an earlier reading held, an accidental CR 614.5 dedup that Wolverine and
    /// Krark's Thumb depend on. Both of those witnesses are *sibling-event* stashes
    /// whose continuations are never dispatched at all (Wolverine: zero dispatches,
    /// ever; Krark: one, drop or push), so the drop de-duplicates nothing. See the
    /// `ResidentDrainPolicy` docs for the measured census.
    ///
    /// What the drop does buy: a `Ready` drain that can never be dispatched would
    /// make `has_ready()` true forever and permanently gate
    /// `draw_through_replacement`. Dropping it keeps the stack honest. Removing this
    /// guard is only safe once the sibling-event stash is fixed at its source
    /// (issue #5676) — an un-dispatchable drain must not be *installed*, rather than
    /// installed and then leaked.
    #[test]
    fn keep_resident_drops_a_stash_arriving_while_a_ready_drain_is_pending() {
        let mut stack = PostReplacementDrainStack::default();
        assert!(stack.install(ready_drain("first"), ResidentDrainPolicy::KeepResident));
        let dropped = !stack.install(ready_drain("second"), ResidentDrainPolicy::KeepResident);
        assert!(
            dropped,
            "a stash arriving while a READY continuation is still pending is \
             discarded — the leak-guard against an un-dispatchable sibling-event \
             stash pinning `has_ready()` true forever. Contrast \
             `keep_resident_does_not_drop_a_stash_arriving_while_the_outer_drain_dispatches`: \
             a DISPATCHING resident is not pending work, and a stash arriving then \
             must install (CR 616.1g)."
        );
    }

    fn dispatching_drain_event_source(stack: &PostReplacementDrainStack) -> Option<ObjectId> {
        stack
            .drains
            .iter()
            .find(|drain| matches!(drain.status, DrainStatus::Dispatching))
            .and_then(|drain| drain.event_source)
    }

    /// CR 615.5: `Replace` must never evict a **Dispatching** drain.
    ///
    /// A `Dispatching` drain is not idle state to be overwritten — it is the
    /// event context of a continuation that is running *right now*. That context
    /// is how `TargetFilter::PostReplacementSourceController` resolves "the
    /// source's controller draws cards" (Swans of Bryn Argoll): the answer is read
    /// out of the drain *while* the continuation resolves. `install(Replace)`
    /// popped unconditionally, so a `Replace`-policy install arriving mid-dispatch
    /// destroyed the running continuation's event context and left it resolving
    /// against whatever landed on top.
    ///
    /// Reachable via the optional accept/decline path (`replacement.rs`) during a
    /// dispatch. No live victim in today's suite — the census over the engine
    /// integration suite (2732 tests) records 14 `Replace` installs, all at depth 0
    /// — so this pins the seam before a card walks into it.
    ///
    /// The predicate is the same one `KeepResident` already uses: act on READY,
    /// never on DISPATCHING.
    #[test]
    fn replace_evicts_a_ready_resident_but_never_a_dispatching_one() {
        // (1) Against a READY resident, `Replace` still replaces — unchanged.
        let mut stack = PostReplacementDrainStack::default();
        assert!(stack.install(ready_drain("stale"), ResidentDrainPolicy::KeepResident));
        assert!(stack.install(ready_drain("winner"), ResidentDrainPolicy::Replace));
        assert_eq!(
            stack.drains.len(),
            1,
            "Replace evicts a READY resident: that is the policy's whole purpose"
        );

        // (2) Against a DISPATCHING resident, it must NOT.
        let mut stack = PostReplacementDrainStack::default();
        let mut outer = ready_drain("outer");
        outer.event_source = Some(ObjectId(7));
        assert!(stack.install(outer, ResidentDrainPolicy::KeepResident));
        assert!(
            stack.begin_dispatch().is_some(),
            "the outer continuation is handed out to run"
        );

        // A Replace-policy install arrives while that continuation is still running.
        assert!(stack.install(ready_drain("incoming"), ResidentDrainPolicy::Replace));

        assert_eq!(
            dispatching_drain_event_source(&stack),
            Some(ObjectId(7)),
            "CR 615.5: the RUNNING continuation's event context must survive a \
             Replace-policy install. Popping the Dispatching drain destroys the \
             answer to `PostReplacementSourceController` mid-flight — Swans of Bryn \
             Argoll resolves 'the source's controller draws cards' out of exactly \
             this field, while the continuation is resolving."
        );
        assert!(
            stack.has_ready(),
            "the incoming continuation still installs — it is nested above the \
             dispatching drain (CR 616.1g), not dropped"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PostReplacementContinuation, QuantityExpr,
        ResolvedAbility, TargetFilter,
    };

    fn scoped_selection_wire_fixture() -> PendingScopedLibrarySearch {
        let first = ObjectIncarnationRef::of(ObjectId(10), 1);
        let second = ObjectIncarnationRef::of(ObjectId(20), 2);
        PendingScopedLibrarySearch {
            ability: Box::new(ResolvedAbility::new(
                Effect::Shuffle {
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(99),
                PlayerId(0),
            )),
            phase: ScopedLibrarySearchPhase::CollectSelections {
                prepared_choices: vec![
                    PreparedScopedLibrarySearchChoice {
                        player: PlayerId(0),
                        library_owner: Some(PlayerId(0)),
                        candidates: vec![first],
                        offered_count: Some(1),
                        announced_selection: Some(vec![first]),
                        filter: TargetFilter::Any,
                        count: 1,
                        reveal: true,
                        up_to: false,
                        allows_partial_find: false,
                        constraint: SearchSelectionConstraint::None,
                    },
                    PreparedScopedLibrarySearchChoice {
                        player: PlayerId(1),
                        library_owner: Some(PlayerId(1)),
                        candidates: vec![second],
                        offered_count: Some(1),
                        announced_selection: Some(vec![second]),
                        filter: TargetFilter::Any,
                        count: 1,
                        reveal: true,
                        up_to: false,
                        allows_partial_find: false,
                        constraint: SearchSelectionConstraint::None,
                    },
                ],
                next_selection_index: 2,
                current_player: None,
                selections: vec![(PlayerId(0), vec![first]), (PlayerId(1), vec![second])],
                frozen_dispositions: vec![
                    FrozenScopedSearchFoundDisposition {
                        searcher: PlayerId(0),
                        identity: first,
                        disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
                    },
                    FrozenScopedSearchFoundDisposition {
                        searcher: PlayerId(1),
                        identity: second,
                        disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
                    },
                ],
                pending_reveals: vec![(PlayerId(0), vec![first])],
            },
            after_scope: None,
        }
    }

    #[test]
    fn scoped_selection_wire_rejects_foreign_and_cross_player_exact_refs() {
        let valid = serde_json::to_value(scoped_selection_wire_fixture()).expect("serialize");
        serde_json::from_value::<PendingScopedLibrarySearch>(valid.clone())
            .expect("fixture satisfies scoped invariants");

        let mut foreign_selection = valid.clone();
        foreign_selection["phase"]["selections"][0][1][0]["object_id"] = 999.into();
        assert!(serde_json::from_value::<PendingScopedLibrarySearch>(foreign_selection).is_err());

        let mut cross_player_frozen = valid.clone();
        cross_player_frozen["phase"]["frozen_dispositions"][0]["searcher"] = 1.into();
        assert!(serde_json::from_value::<PendingScopedLibrarySearch>(cross_player_frozen).is_err());

        let mut cross_player_reveal = valid;
        cross_player_reveal["phase"]["pending_reveals"][0][0] = 1.into();
        assert!(serde_json::from_value::<PendingScopedLibrarySearch>(cross_player_reveal).is_err());
    }

    #[test]
    fn scoped_selection_wire_round_trips_modified_completed_player_while_next_chooses() {
        let mut pending = scoped_selection_wire_fixture();
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            next_selection_index,
            current_player,
            selections,
            frozen_dispositions,
            pending_reveals,
        } = &mut pending.phase
        else {
            unreachable!("fixture is in selection phase")
        };
        let second = prepared_choices[1].candidates[0];
        prepared_choices.push(PreparedScopedLibrarySearchChoice {
            player: PlayerId(2),
            library_owner: Some(PlayerId(2)),
            candidates: vec![ObjectIncarnationRef::of(ObjectId(30), 3)],
            offered_count: Some(1),
            announced_selection: None,
            filter: TargetFilter::Any,
            count: 1,
            reveal: false,
            up_to: false,
            allows_partial_find: false,
            constraint: SearchSelectionConstraint::None,
        });
        *next_selection_index = 3;
        *current_player = Some(PlayerId(2));
        *selections = vec![
            (PlayerId(0), vec![prepared_choices[0].candidates[0]]),
            (PlayerId(1), Vec::new()),
        ];
        *frozen_dispositions = vec![
            FrozenScopedSearchFoundDisposition {
                searcher: PlayerId(0),
                identity: prepared_choices[0].candidates[0],
                disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
            },
            FrozenScopedSearchFoundDisposition {
                searcher: PlayerId(1),
                identity: second,
                disposition: crate::types::proposed_event::SearchFoundDisposition::Modified(
                    crate::types::proposed_event::BoundSearchFoundDisposition {
                        destination: Zone::Exile,
                        source: ObjectIncarnationRef::of(ObjectId(99), 0),
                        grant: None,
                    },
                ),
            },
        ];
        pending_reveals.clear();

        let json = serde_json::to_value(&pending).expect("serialize production phase");
        let restored = serde_json::from_value::<PendingScopedLibrarySearch>(json)
            .expect("modified completed player remains valid while the next player chooses");
        assert_eq!(restored, pending);
    }

    #[test]
    fn scoped_selection_wire_round_trips_parked_modified_prefix() {
        let mut pending = scoped_selection_wire_fixture();
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            next_selection_index,
            current_player,
            selections,
            frozen_dispositions,
            pending_reveals,
        } = &mut pending.phase
        else {
            unreachable!("fixture is in selection phase")
        };
        let identity = prepared_choices[0].candidates[0];
        *next_selection_index = 1;
        *current_player = Some(PlayerId(0));
        selections.clear();
        pending_reveals.clear();
        prepared_choices[1].offered_count = None;
        prepared_choices[1].announced_selection = None;
        *frozen_dispositions = vec![FrozenScopedSearchFoundDisposition {
            searcher: PlayerId(0),
            identity,
            disposition: crate::types::proposed_event::SearchFoundDisposition::Modified(
                crate::types::proposed_event::BoundSearchFoundDisposition {
                    destination: Zone::Exile,
                    source: ObjectIncarnationRef::of(ObjectId(99), 0),
                    grant: None,
                },
            ),
        }];

        let json = serde_json::to_value(&pending).unwrap();
        assert_eq!(
            serde_json::from_value::<PendingScopedLibrarySearch>(json).unwrap(),
            pending
        );
    }

    #[test]
    fn scoped_selection_wire_round_trips_current_original_and_modified_prefix() {
        let mut pending = scoped_selection_wire_fixture();
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            next_selection_index,
            current_player,
            selections,
            frozen_dispositions,
            pending_reveals,
        } = &mut pending.phase
        else {
            unreachable!("fixture is in selection phase")
        };
        let original = prepared_choices[0].candidates[0];
        let modified = ObjectIncarnationRef::of(ObjectId(11), 4);
        prepared_choices[0].candidates.push(modified);
        prepared_choices[0].count = 2;
        prepared_choices[0].offered_count = Some(2);
        prepared_choices[0].announced_selection = Some(vec![original, modified]);
        prepared_choices[1].offered_count = None;
        prepared_choices[1].announced_selection = None;
        *next_selection_index = 1;
        *current_player = Some(PlayerId(0));
        selections.clear();
        pending_reveals.clear();
        *frozen_dispositions = vec![
            FrozenScopedSearchFoundDisposition {
                searcher: PlayerId(0),
                identity: original,
                disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
            },
            FrozenScopedSearchFoundDisposition {
                searcher: PlayerId(0),
                identity: modified,
                disposition: crate::types::proposed_event::SearchFoundDisposition::Modified(
                    crate::types::proposed_event::BoundSearchFoundDisposition {
                        destination: Zone::Exile,
                        source: ObjectIncarnationRef::of(ObjectId(99), 0),
                        grant: None,
                    },
                ),
            },
        ];

        let json = serde_json::to_value(&pending).unwrap();
        assert_eq!(
            serde_json::from_value::<PendingScopedLibrarySearch>(json).unwrap(),
            pending
        );
    }

    #[test]
    fn scoped_selection_wire_round_trips_exact_search_capped_by_live_candidates() {
        let mut pending = scoped_selection_wire_fixture();
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            next_selection_index,
            current_player,
            selections,
            frozen_dispositions,
            pending_reveals,
        } = &mut pending.phase
        else {
            unreachable!("fixture is in selection phase")
        };
        let only_live_candidate = prepared_choices[0].candidates[0];
        prepared_choices.truncate(1);
        prepared_choices[0].count = 2;
        prepared_choices[0].offered_count = Some(1);
        prepared_choices[0].announced_selection = Some(vec![only_live_candidate]);
        *next_selection_index = 1;
        *current_player = None;
        *selections = vec![(PlayerId(0), vec![only_live_candidate])];
        *frozen_dispositions = vec![FrozenScopedSearchFoundDisposition {
            searcher: PlayerId(0),
            identity: only_live_candidate,
            disposition: crate::types::proposed_event::SearchFoundDisposition::Original,
        }];
        *pending_reveals = vec![(PlayerId(0), vec![only_live_candidate])];

        let json = serde_json::to_value(&pending).unwrap();
        assert_eq!(
            serde_json::from_value::<PendingScopedLibrarySearch>(json).unwrap(),
            pending
        );
    }

    #[test]
    fn scoped_selection_wire_round_trips_zero_candidate_then_active_searcher() {
        let mut pending = scoped_selection_wire_fixture();
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            next_selection_index,
            current_player,
            selections,
            frozen_dispositions,
            pending_reveals,
        } = &mut pending.phase
        else {
            unreachable!("fixture is in selection phase")
        };
        prepared_choices[0].candidates.clear();
        prepared_choices[0].offered_count = Some(0);
        prepared_choices[0].announced_selection = Some(Vec::new());
        prepared_choices[1].offered_count = Some(1);
        prepared_choices[1].announced_selection = None;
        *next_selection_index = 2;
        *current_player = Some(PlayerId(1));
        *selections = vec![(PlayerId(0), Vec::new())];
        frozen_dispositions.clear();
        pending_reveals.clear();

        let json = serde_json::to_value(&pending).unwrap();
        assert_eq!(
            serde_json::from_value::<PendingScopedLibrarySearch>(json).unwrap(),
            pending
        );
    }

    #[test]
    fn scoped_selection_wire_allows_shared_exact_candidate_across_searchers() {
        let mut pending = scoped_selection_wire_fixture();
        let ScopedLibrarySearchPhase::CollectSelections {
            prepared_choices,
            selections,
            frozen_dispositions,
            ..
        } = &mut pending.phase
        else {
            unreachable!("fixture is in selection phase")
        };
        let shared = prepared_choices[0].candidates[0];
        prepared_choices[1].candidates = vec![shared];
        prepared_choices[1].announced_selection = Some(vec![shared]);
        selections[1].1 = vec![shared];
        frozen_dispositions
            .iter_mut()
            .find(|frozen| frozen.searcher == PlayerId(1))
            .unwrap()
            .identity = shared;

        let json = serde_json::to_value(&pending).unwrap();
        assert_eq!(
            serde_json::from_value::<PendingScopedLibrarySearch>(json).unwrap(),
            pending
        );
    }

    #[test]
    fn search_found_visibility_preserves_legacy_boolean_wire_shape() {
        let batch = PendingSearchFoundBatch {
            searcher: PlayerId(1),
            library_owner: Some(PlayerId(1)),
            remaining: vec![ObjectIncarnationRef::of(ObjectId(7), 1)],
            survivors: vec![ObjectIncarnationRef::of(ObjectId(8), 2)],
            current: None,
            continuation: PendingSearchFoundContinuation::Standard { split: None },
            visibility: SearchFoundVisibility::Public,
        };

        let mut json = serde_json::to_value(&batch).expect("serialize SearchFound batch");
        assert_eq!(json["reveal"], serde_json::Value::Bool(true));
        assert_eq!(
            serde_json::from_value::<PendingSearchFoundBatch>(json.clone())
                .expect("deserialize current SearchFound batch")
                .visibility,
            SearchFoundVisibility::Public
        );

        json["reveal"] = serde_json::Value::Bool(false);
        assert_eq!(
            serde_json::from_value::<PendingSearchFoundBatch>(json.clone())
                .expect("deserialize legacy private SearchFound batch")
                .visibility,
            SearchFoundVisibility::Private
        );

        json.as_object_mut()
            .expect("batch serializes as an object")
            .remove("reveal");
        assert_eq!(
            serde_json::from_value::<PendingSearchFoundBatch>(json)
                .expect("deserialize pre-field SearchFound batch")
                .visibility,
            SearchFoundVisibility::Private
        );
    }

    #[test]
    fn search_found_delivery_grant_round_trips_and_legacy_defaults() {
        let completion = BatchCompletion::SearchFoundZoneDelivery {
            object_id: ObjectId(7),
            grant: Some(crate::types::proposed_event::BoundSearchFoundGrant {
                source: crate::types::identifiers::ObjectIncarnationRef::of(ObjectId(9), 3),
                controller: PlayerId(0),
                grantee: PlayerId(1),
                mana_spend_permission: Some(crate::types::ability::ManaSpendPermission::AnyColor),
            }),
        };
        let json = serde_json::to_value(&completion).expect("serialize bound delivery grant");
        assert_eq!(
            serde_json::from_value::<BatchCompletion>(json)
                .expect("deserialize bound delivery grant"),
            completion
        );

        let legacy = serde_json::json!({
            "SearchFoundZoneDelivery": { "object_id": 7 }
        });
        assert_eq!(
            serde_json::from_value::<BatchCompletion>(legacy)
                .expect("deserialize pre-grant delivery completion"),
            BatchCompletion::SearchFoundZoneDelivery {
                object_id: ObjectId(7),
                grant: None,
            }
        );
    }

    #[test]
    fn resolving_trigger_context_captures_plural_events_without_singular_event() {
        let mut state = GameState::new_two_player(42);
        let event = GameEvent::LifeChanged {
            player_id: PlayerId(1),
            amount: -2,
        };
        state.current_trigger_events = vec![event.clone()];

        let context = ResolvingTriggerContext::capture(&state)
            .expect("a plural trigger event list is live resolution context");
        assert_eq!(context.event, None);
        assert_eq!(context.events, vec![event]);
    }

    #[test]
    fn pending_liminal_entry_resume_accepts_legacy_token_struct_shape() {
        let event = ProposedEvent::zone_change(
            ObjectId(7),
            Zone::Exile,
            Zone::Battlefield,
            Some(ObjectId(7)),
        );
        let legacy = serde_json::json!({
            "source_id": ObjectId(7),
            "player": PlayerId(1),
            "event": event,
        });
        assert!(matches!(
            serde_json::from_value::<PendingLiminalEntryResume>(legacy).unwrap(),
            PendingLiminalEntryResume::Token {
                source_id: ObjectId(7),
                player: PlayerId(1),
                ..
            }
        ));
    }

    /// V1: legacy persisted `{"type":"UntilEndOfTurn"}` (pre-parameterization
    /// wire form) must deserialize to `UntilTurnBoundary { EndOfCurrentTurn }`
    /// via `#[serde(alias)]` + `#[serde(default)]`. The `UntilStackEmpty` arm is
    /// asserted unchanged as a positive reach-guard proving the alias captured
    /// the right tag and did not disturb the sibling variant.
    #[test]
    fn auto_pass_mode_legacy_eot_deserializes() {
        assert_eq!(
            serde_json::from_str::<AutoPassMode>(r#"{"type":"UntilEndOfTurn"}"#).unwrap(),
            AutoPassMode::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn
            }
        );
        assert_eq!(
            serde_json::from_str::<AutoPassMode>(
                r#"{"type":"UntilStackEmpty","initial_stack_len":3}"#
            )
            .unwrap(),
            AutoPassMode::UntilStackEmpty {
                initial_stack_len: 3
            }
        );
    }

    /// V2: the canonical new wire form round-trips for both boundaries, and
    /// `EndOfCurrentTurn` always serializes with an explicit `until` (never the
    /// bare legacy tag) — `alias` affects deserialization only.
    #[test]
    fn auto_pass_mode_roundtrips_both_boundaries() {
        let my_next = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::MyNextTurnStart,
        };
        let json = serde_json::to_string(&my_next).unwrap();
        assert_eq!(
            json,
            r#"{"type":"UntilTurnBoundary","until":"MyNextTurnStart"}"#
        );
        assert_eq!(
            serde_json::from_str::<AutoPassMode>(&json).unwrap(),
            my_next
        );

        let eot = AutoPassMode::UntilTurnBoundary {
            until: TurnBoundary::EndOfCurrentTurn,
        };
        assert_eq!(
            serde_json::to_string(&eot).unwrap(),
            r#"{"type":"UntilTurnBoundary","until":"EndOfCurrentTurn"}"#
        );
    }

    /// V3: the transient `AutoPassRequest` also accepts the legacy tag, covering
    /// FE/engine version skew during a deploy window.
    #[test]
    fn auto_pass_request_legacy_eot_deserializes() {
        assert_eq!(
            serde_json::from_str::<AutoPassRequest>(r#"{"type":"UntilEndOfTurn"}"#).unwrap(),
            AutoPassRequest::UntilTurnBoundary {
                until: TurnBoundary::EndOfCurrentTurn
            }
        );
    }

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

    /// T-loop (§4 Condition 2): the all-zone incarnation bump advances a source's
    /// epoch every time it changes zones, so a mandatory loop that cycles its
    /// source's zones would carry a growing `ResolvedAbility::source_incarnation`
    /// into loop equality and never confirm a CR 104.4b draw. `normalize_for_loop`
    /// canonicalizes `source_incarnation` to `None` across every eq-compared carrier
    /// (here: `delayed_triggers` — the Warp "return at next end step" loop class —
    /// and `stack`).
    ///
    /// REVERT-PROBE: drop the carrier normalization in `normalize_for_loop` → the
    /// two normalized states differ in `source_incarnation` → `loop_states_equal`
    /// returns false → the draw is missed.
    #[test]
    fn normalize_for_loop_zeroes_source_incarnation_across_carriers() {
        use crate::types::ability::{DelayedTriggerCondition, Effect};
        use crate::types::phase::Phase;

        fn draw_ability(inc: u64) -> ResolvedAbility {
            let mut a = ResolvedAbility::new(
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                vec![],
                ObjectId(5),
                PlayerId(0),
            );
            a.set_source_incarnation_recursive(Some(inc));
            a
        }

        let mut a = GameState::new_two_player(7);
        a.delayed_triggers.push(DelayedTrigger {
            condition: DelayedTriggerCondition::AtNextPhase { phase: Phase::End },
            ability: draw_ability(1),
            controller: PlayerId(0),
            source_id: ObjectId(5),
            one_shot: true,
        });
        a.stack.push_back(StackEntry {
            id: ObjectId(20),
            source_id: ObjectId(5),
            controller: PlayerId(0),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(5),
                ability: draw_ability(1),
            },
        });

        // `b` differs ONLY in the sources' incarnation (a loop iteration cycled the
        // source's zones and re-created its delayed trigger at a higher epoch).
        let mut b = a.clone();
        b.delayed_triggers[0]
            .ability
            .set_source_incarnation_recursive(Some(2));
        b.stack
            .back_mut()
            .unwrap()
            .ability_mut()
            .unwrap()
            .set_source_incarnation_recursive(Some(2));

        assert_ne!(
            a, b,
            "fixture must actually differ in source_incarnation before normalization"
        );
        assert!(
            loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "normalize_for_loop must zero source_incarnation across delayed_triggers + stack"
        );
    }

    /// CR 104.4b + CR 732.2a: repeated same-step zone cycles accumulate
    /// incarnation-versioned LKI, but snapshots no pending trigger can consume
    /// are history rather than a change to the recurring game state. A snapshot
    /// still named by a trigger event remains rules-material and must distinguish
    /// normalized states.
    #[test]
    fn normalize_for_loop_prunes_unreferenced_lki_incarnations_but_keeps_referenced_snapshot() {
        use crate::types::ability::Effect;

        fn snapshot(power: i32) -> LKISnapshot {
            LKISnapshot {
                name: "Loop Entrant".to_string(),
                token_image_ref: None,
                power: Some(power),
                toughness: Some(2),
                base_power: Some(2),
                base_toughness: Some(2),
                mana_value: 2,
                controller: PlayerId(0),
                owner: PlayerId(0),
                card_types: vec![CoreType::Creature],
                subtypes: Vec::new(),
                supertypes: Vec::new(),
                keywords: Vec::new(),
                colors: Vec::new(),
                chosen_attributes: Vec::new(),
                counters: HashMap::new(),
                tapped: false,
                is_suspected: false,
                attachments: Vec::new(),
            }
        }

        let entrant = ObjectId(50);
        let referenced_incarnation = 7;
        let mut record =
            ZoneChangeRecord::test_minimal(entrant, Some(Zone::Exile), Zone::Battlefield);
        record.entered_incarnation = Some(referenced_incarnation);
        let trigger_event = GameEvent::ZoneChanged {
            object_id: entrant,
            from: Some(Zone::Exile),
            to: Zone::Battlefield,
            record: Box::new(record),
        };
        let ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(5),
            PlayerId(0),
        );

        let mut a = GameState::new_two_player(7);
        a.stack.push_back(StackEntry {
            id: ObjectId(20),
            source_id: ObjectId(5),
            controller: PlayerId(0),
            kind: StackEntryKind::TriggeredAbility {
                source_id: ObjectId(5),
                ability: Box::new(ability),
                condition: None,
                trigger_event: Some(trigger_event),
                description: None,
                source_name: String::new(),
                subject_match_count: None,
                die_result: None,
            },
        });
        a.lki_by_incarnation
            .entry(entrant)
            .or_default()
            .insert(referenced_incarnation, snapshot(5));
        a.lki_by_incarnation
            .entry(entrant)
            .or_default()
            .insert(8, snapshot(2));

        let mut b = a.clone();
        b.lki_by_incarnation
            .entry(entrant)
            .or_default()
            .insert(9, snapshot(2));
        assert_ne!(a, b, "fixture differs by irrelevant accumulated LKI");

        let normalized_a = a.normalize_for_loop();
        let normalized_b = b.normalize_for_loop();
        assert!(
            loop_states_equal(&normalized_a, &normalized_b),
            "irrelevant incarnation history must not block loop recurrence"
        );
        assert_eq!(
            normalized_a.lki_by_incarnation[&entrant]
                .keys()
                .copied()
                .collect::<Vec<_>>(),
            vec![referenced_incarnation],
            "the trigger-referenced incarnation remains available"
        );

        let mut changed_reference = a;
        changed_reference
            .lki_by_incarnation
            .get_mut(&entrant)
            .unwrap()
            .insert(referenced_incarnation, snapshot(4));
        assert!(
            !loop_states_equal(&normalized_a, &changed_reference.normalize_for_loop()),
            "different LKI for a still-referenced incarnation remains meaningful"
        );
    }

    /// PR-6 test 8 (B2 loop-equality guard): `unbounded_resources` is
    /// display/annotation state, NOT rules state for equality. Two states
    /// identical except one has a populated `unbounded_resources` (the
    /// infinite-mana toggle's six `Mana(_)` axes via `mark_unbounded_loop`) MUST
    /// compare EQUAL through every loop-detection comparator. Otherwise a populated
    /// live state would stop matching the empty-`unbounded_resources` ring
    /// snapshots and CR 104.4b / CR 732.2a loop detection would yield false
    /// negatives (and AI-search position dedup would break).
    ///
    /// REVERT-PROBE: add `&& self.unbounded_resources == other.unbounded_resources`
    /// to the manual `impl PartialEq for GameState` → `a == b`, `loop_states_equal`,
    /// and `loop_states_equal_modulo_resources` all flip to false → every assertion
    /// below fails.
    #[test]
    fn unbounded_resources_excluded_from_loop_equality() {
        use crate::analysis::resource::loop_states_equal_modulo_resources;
        use crate::game::mana_payment::INFINITE_MANA_AXES;

        let a = GameState::new_two_player(7);
        let mut b = a.clone();
        b.mark_unbounded_loop(PlayerId(0), &INFINITE_MANA_AXES);
        // Sanity: the populated field really does differ between the two states.
        assert_ne!(
            a.unbounded_resources, b.unbounded_resources,
            "fixture must actually differ in unbounded_resources"
        );

        assert!(
            a == b,
            "manual PartialEq must exclude unbounded_resources (display state)"
        );
        assert!(
            loop_states_equal(&a, &b),
            "loop_states_equal (CR 104.4b/732.2a) must exclude unbounded_resources"
        );
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "the PR-0/PR-2 modulo path must exclude unbounded_resources"
        );
    }

    /// PR-7 Phase 4c (B5, R3): sibling of `unbounded_resources_excluded_from_loop_equality`
    /// — the new `unbounded_loop_enablers` field follows the identical exclusion-by-omission
    /// discipline (never appears in the `impl PartialEq` `&&` chain).
    ///
    /// REVERT-PROBE: add `&& self.unbounded_loop_enablers == other.unbounded_loop_enablers`
    /// to the manual `impl PartialEq for GameState` → all three assertions below fail.
    #[test]
    fn unbounded_loop_enablers_excluded_from_loop_equality() {
        use crate::analysis::resource::loop_states_equal_modulo_resources;

        let a = GameState::new_two_player(7);
        let mut b = a.clone();
        b.register_unbounded_loop_enablers(PlayerId(0), BTreeSet::from([ObjectId(1)]));
        // Sanity: the populated field really does differ between the two states.
        assert_ne!(
            a.unbounded_loop_enablers, b.unbounded_loop_enablers,
            "fixture must actually differ in unbounded_loop_enablers"
        );

        assert!(
            a == b,
            "manual PartialEq must exclude unbounded_loop_enablers (revocation annotation)"
        );
        assert!(
            loop_states_equal(&a, &b),
            "loop_states_equal (CR 104.4b/732.2a) must exclude unbounded_loop_enablers"
        );
        assert!(
            loop_states_equal_modulo_resources(&a, &b),
            "the PR-0/PR-2 modulo path must exclude unbounded_loop_enablers"
        );
    }

    /// PR-7 Phase 4c (B5 defuse): `clear_unbounded_loop` must remove BOTH
    /// `unbounded_resources` and `unbounded_loop_enablers` for the controller in
    /// lockstep — the `zones.rs` defuse hook relies on a single call revoking the
    /// whole capability.
    #[test]
    fn clear_unbounded_loop_removes_both_maps_in_lockstep() {
        let mut state = GameState::new_two_player(7);
        state.mark_unbounded_loop(
            PlayerId(0),
            &[crate::analysis::resource::ResourceAxis::Life(PlayerId(0))],
        );
        state.register_unbounded_loop_enablers(PlayerId(0), BTreeSet::from([ObjectId(1)]));
        assert!(state.unbounded_resources.contains_key(&PlayerId(0)));
        assert!(state.unbounded_loop_enablers.contains_key(&PlayerId(0)));

        state.clear_unbounded_loop(PlayerId(0));

        assert!(
            !state.unbounded_resources.contains_key(&PlayerId(0)),
            "clear_unbounded_loop must remove the unbounded_resources entry"
        );
        assert!(
            !state.unbounded_loop_enablers.contains_key(&PlayerId(0)),
            "clear_unbounded_loop must remove the unbounded_loop_enablers entry"
        );
    }

    /// `register_unbounded_loop_enablers` is a no-op for an empty set — no entry to
    /// defuse on later (mirrors `mark_unbounded_loop`'s idempotent set-union contract).
    #[test]
    fn register_unbounded_loop_enablers_empty_set_is_noop() {
        let mut state = GameState::new_two_player(7);
        state.register_unbounded_loop_enablers(PlayerId(0), BTreeSet::new());
        assert!(
            !state.unbounded_loop_enablers.contains_key(&PlayerId(0)),
            "an empty enabler set must not create an entry"
        );
    }

    /// Loop-equality guard for the telemetry accumulator: `unimplemented_oracle_ids`
    /// is diagnostics/annotation state, NOT rules state for equality. Two states
    /// identical except one has recorded an unimplemented-effect hit MUST compare
    /// EQUAL through the loop comparators. Otherwise a populated live state would
    /// stop matching the pre-hit ring snapshots and CR 104.4b / CR 732.2a loop
    /// detection would yield false negatives.
    ///
    /// REVERT-PROBE: add `&& self.unimplemented_oracle_ids == other.unimplemented_oracle_ids`
    /// to the manual `impl PartialEq for GameState` → both assertions below fail.
    #[test]
    fn unimplemented_oracle_ids_excluded_from_loop_equality() {
        let a = GameState::new_two_player(7);
        let mut b = a.clone();
        b.unimplemented_oracle_ids
            .insert("oracle-abc-123".to_string());
        // Sanity: the populated field really does differ between the two states.
        assert_ne!(
            a.unimplemented_oracle_ids, b.unimplemented_oracle_ids,
            "fixture must actually differ in unimplemented_oracle_ids"
        );

        assert!(
            a == b,
            "manual PartialEq must exclude unimplemented_oracle_ids (diagnostics state)"
        );
        assert!(
            loop_states_equal(&a, &b),
            "loop_states_equal (CR 104.4b/732.2a) must exclude unimplemented_oracle_ids"
        );
    }

    /// Loop-equality guard for the telemetry accumulator: `pending_trigger_abandons`
    /// is diagnostics/annotation state (same family as `unimplemented_oracle_ids`),
    /// NOT rules state for equality. Two states identical except one has recorded a
    /// push-first construction abandon MUST compare EQUAL through the loop
    /// comparators, or a populated live state would stop matching the pre-abandon
    /// ring snapshots and CR 104.4b / CR 732.2a loop detection would yield false
    /// negatives.
    ///
    /// REVERT-PROBE: add `&& self.pending_trigger_abandons == other.pending_trigger_abandons`
    /// to the manual `impl PartialEq for GameState` → both assertions below fail.
    #[test]
    fn pending_trigger_abandons_excluded_from_loop_equality() {
        let a = GameState::new_two_player(7);
        let mut b = a.clone();
        b.pending_trigger_abandons
            .push("Test Source (stack entry 42)".to_string());
        // Sanity: the populated field really does differ between the two states.
        assert_ne!(
            a.pending_trigger_abandons, b.pending_trigger_abandons,
            "fixture must actually differ in pending_trigger_abandons"
        );

        assert!(
            a == b,
            "manual PartialEq must exclude pending_trigger_abandons (diagnostics state)"
        );
        assert!(
            loop_states_equal(&a, &b),
            "loop_states_equal (CR 104.4b/732.2a) must exclude pending_trigger_abandons"
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

    /// L1 / CR 104.4b: an object's `timestamp` is layer-ordering metadata
    /// (CR 613.7) that `objects_content_eq` deliberately omits, like
    /// `incarnation`. Two states differing ONLY in a per-object timestamp must
    /// confirm as a repeat — otherwise a mandatory loop that re-stamps a
    /// permanent every iteration (a repeated transform or re-attach) would never
    /// draw. Revert-failing: adding `timestamp` to `objects_content_eq`'s
    /// allow-list makes the two states differ and this assertion fail.
    #[test]
    fn loop_states_equal_ignores_object_timestamp() {
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
        let ts_a = b.objects[&ObjectId(500)].timestamp;
        b.objects.get_mut(&ObjectId(500)).unwrap().timestamp = ts_a + 7;

        assert!(
            loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "states differing only in an object's CR 613.7 timestamp must confirm as a repeat"
        );
    }

    /// CR 104.4b: pool-unit `pip_id` is a runtime/UI identity tag stamped with a
    /// unique monotonic value each time mana enters a pool. It must NOT affect
    /// game-state equality, otherwise a mandatory loop that floats mana every
    /// iteration would compare its iterations unequal (each pooled unit has a
    /// fresh pip_id) and the CR 104.4b draw would never fire — burning the
    /// auto-pass iteration cap before downgrading to a wrong CR 732.2 halt.
    /// Revert-failing: if `pip_id` is restored to `ManaUnit`'s derived
    /// `PartialEq`, the two pools differ and this assertion fails.
    #[test]
    fn loop_states_equal_ignores_pool_pip_ids() {
        let mut a = GameState::new_two_player(7);
        let player = a.players[0].id;
        // One floated unit, stamped with a distinct pip_id on pool entry.
        a.add_mana_to_pool(
            player,
            ManaUnit::new(ManaType::Red, ObjectId(900), false, vec![]),
        );
        // `b` is identical EXCEPT its pool unit carries a different pip_id — the
        // shape a mandatory mana-floating loop produces (each iteration mints a
        // fresh monotonic id on the same logical unit). Pool length and every
        // other field are identical.
        let mut b = a.clone();
        let pip_a = b.players[0].mana_pool.mana[0].pip_id;
        b.players[0].mana_pool.mana[0].pip_id = ManaPipId(pip_a.0 + 1);
        let pip_b = b.players[0].mana_pool.mana[0].pip_id;
        assert_ne!(
            pip_a, pip_b,
            "the two pool units must differ only in pip_id"
        );

        assert!(
            loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "states differing only in pool-unit pip_ids must confirm as a repeat"
        );
    }

    /// CR 104.4b + CR 611.2c + CR 400.7: a materialized `MustAttackPlayer`
    /// static's `source_object` provenance is a layer-DERIVED characteristic that
    /// `object_content_eq` deliberately omits (like the whole `static_definitions`
    /// vec). Two states differing ONLY in a grafted requirement's directing-source
    /// id — the shape a mandatory loop produces when it re-creates the forcing
    /// object with a fresh CR 400.7 incarnation id each iteration — must confirm
    /// as a repeat, else the CR 104.4b draw could never fire. Revert-failing:
    /// adding `static_definitions` to `object_content_eq`'s allow-list makes the
    /// two states differ and this assertion fails, reintroducing the hazard.
    #[test]
    fn loop_states_equal_ignores_static_source_object() {
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::StaticMode;

        let mut a = GameState::new_two_player(7);
        let mut object = GameObject::new(
            ObjectId(500),
            CardId(1),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        object.static_definitions.push(
            StaticDefinition::new(StaticMode::MustAttackPlayer {
                player: PlayerId(1),
            })
            .affected(TargetFilter::SelfRef)
            .source_object(ObjectId(800)),
        );
        a.objects.insert(ObjectId(500), object);
        a.battlefield.push_back(ObjectId(500));

        // `b` is identical EXCEPT the grafted requirement's directing-source id (a
        // fresh CR 400.7 incarnation of the forcing object).
        let mut b = a.clone();
        b.objects
            .get_mut(&ObjectId(500))
            .unwrap()
            .static_definitions = vec![StaticDefinition::new(StaticMode::MustAttackPlayer {
            player: PlayerId(1),
        })
        .affected(TargetFilter::SelfRef)
        .source_object(ObjectId(801))]
        .into();

        assert!(
            loop_states_equal(&a.normalize_for_loop(), &b.normalize_for_loop()),
            "states differing only in a grafted static's source_object must confirm as a repeat"
        );

        // Paired non-vacuity: a genuinely-compared field (goaded_by, CR 701.15c)
        // DOES break equality — proving the comparator is live, not a constant true.
        let mut c = a.clone();
        c.objects
            .get_mut(&ObjectId(500))
            .unwrap()
            .goaded_by
            .insert(PlayerId(1));
        assert!(
            !loop_states_equal(&a.normalize_for_loop(), &c.normalize_for_loop()),
            "a goaded_by difference must NOT confirm (the comparator is live)"
        );
    }

    /// CR 118.3a: the self-heal that fixes the reported "tap one mana → all
    /// select" bug. Mana that bypassed `add_mana_to_pool` (debug tooling, restored
    /// pre-stamping saves) carries the sentinel `pip_id 0`; `restamp_pool_pip_ids`
    /// must give every unit a unique, nonzero id so each is individually pinnable.
    #[test]
    fn restamp_pool_pip_ids_heals_sentinel_and_duplicate_ids() {
        let mut state = GameState::new_two_player(7);
        let player = state.players[0].id;
        // Bypass the stamping authority: three units all at the unstamped sentinel.
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::Green,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        assert!(
            state.players[0]
                .mana_pool
                .mana
                .iter()
                .all(|u| u.pip_id.0 == 0),
            "precondition: all three units are unstamped (pip_id 0)"
        );

        state.restamp_pool_pip_ids(player);

        let ids: Vec<u64> = state.players[0]
            .mana_pool
            .mana
            .iter()
            .map(|u| u.pip_id.0)
            .collect();
        assert!(
            ids.iter().all(|&id| id != 0),
            "every unit must be stamped (no sentinel remains), got {ids:?}"
        );
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            ids.len(),
            "all pip ids must be unique after restamp, got {ids:?}"
        );
    }

    /// Covers the duplicate-NONZERO arm of `restamp_pool_pip_ids` (the all-zero
    /// sentinel arm is covered above). Two units share a nonzero id; only the
    /// duplicate is re-stamped — the first occurrence and the unique unit survive.
    #[test]
    fn restamp_pool_pip_ids_heals_duplicate_nonzero_ids() {
        let mut state = GameState::new_two_player(7);
        let player = state.players[0].id;
        for _ in 0..3 {
            state.players[0].mana_pool.add(ManaUnit::new(
                ManaType::Blue,
                ObjectId(0),
                false,
                vec![],
            ));
        }
        // Inject a duplicate nonzero id: [100, 100, 200]. Ids are chosen well above
        // a fresh game's `next_pip_id` so the minted replacement cannot collide.
        let mana = &mut state.players[0].mana_pool.mana;
        mana[0].pip_id = ManaPipId(100);
        mana[1].pip_id = ManaPipId(100);
        mana[2].pip_id = ManaPipId(200);

        state.restamp_pool_pip_ids(player);

        let ids: Vec<u64> = state.players[0]
            .mana_pool
            .mana
            .iter()
            .map(|u| u.pip_id.0)
            .collect();
        assert!(
            ids.iter().all(|&id| id != 0),
            "no sentinel introduced, got {ids:?}"
        );
        assert_eq!(
            ids.iter()
                .copied()
                .collect::<std::collections::HashSet<_>>()
                .len(),
            3,
            "the duplicate nonzero id must be made unique, got {ids:?}"
        );
        assert_eq!(
            ids.iter().filter(|&&id| id == 100).count(),
            1,
            "exactly one of the shared id is kept; the duplicate is re-stamped, got {ids:?}"
        );
        assert!(
            ids.contains(&200),
            "the already-unique id is preserved, got {ids:?}"
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
            source_id: ObjectId(1),
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

    /// Phase-0 migration oracle for Amendment B's shipped, split authority.
    /// This is intentionally test-only: Phase 2/3 will replace these labels
    /// with resolution frames, but must preserve the current nesting order for
    /// each valid mixed shape. `PostReplacementDrainStack` is positional today
    /// (there are no drain ids), while draw work is addressed by frame id and
    /// the connive tail has its dedicated slot.
    #[test]
    fn phase0_migration_oracle_shipped_mixed_slots_preserve_drain_order() {
        #[derive(Debug, PartialEq, Eq)]
        enum ShippedLegacySlot {
            GeneralPostReplacement,
            DrawSequence,
            ConniveTail,
        }

        fn outer_to_inner(state: &GameState) -> Vec<ShippedLegacySlot> {
            let mut order = Vec::new();
            if state.pending_connive_reentry.is_some() {
                order.push(ShippedLegacySlot::ConniveTail);
            }
            if state.post_replacement_drains.has_ready() {
                order.push(ShippedLegacySlot::GeneralPostReplacement);
            }
            if state.draw_sequences.active().is_some() {
                order.push(ShippedLegacySlot::DrawSequence);
            }
            order
        }

        let draw = |state: &mut GameState| {
            state.draw_sequences.push(PlayerId(0), 1);
        };
        let general = |state: &mut GameState| {
            state.install_ready_continuation(
                crate::types::ability::PostReplacementContinuation::Template(Box::new(
                    crate::types::ability::AbilityDefinition::new(
                        crate::types::ability::AbilityKind::Spell,
                        crate::types::ability::Effect::Draw {
                            count: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                            target: crate::types::ability::TargetFilter::Controller,
                        },
                    ),
                )),
            );
        };

        // A general drain that has entered a true draw keeps the general work
        // outside the draw sequence; the child draw is the active (inner) work.
        let mut general_then_draw = GameState::new_two_player(42);
        general(&mut general_then_draw);
        draw(&mut general_then_draw);
        assert!(general_then_draw.post_replacement_drains.has_ready());
        assert!(general_then_draw.draw_sequences.active().is_some());
        assert_eq!(
            outer_to_inner(&general_then_draw),
            vec![
                ShippedLegacySlot::GeneralPostReplacement,
                ShippedLegacySlot::DrawSequence,
            ],
            "outer-to-inner migration order for a general drain that started a draw"
        );

        // Leader-style "draw, then connive" carries its tail separately. The
        // connive tail is outer work and the draw remains the active inner work.
        let mut draw_then_connive = GameState::new_two_player(42);
        draw(&mut draw_then_connive);
        draw_then_connive.pending_connive_reentry = Some(PendingConniveReentry {
            conniver: ObjectId(99),
            count: 1,
            applied: HashSet::new(),
        });
        assert!(draw_then_connive.draw_sequences.active().is_some());
        assert!(draw_then_connive.pending_connive_reentry.is_some());
        assert_eq!(
            outer_to_inner(&draw_then_connive),
            vec![
                ShippedLegacySlot::ConniveTail,
                ShippedLegacySlot::DrawSequence,
            ],
            "outer-to-inner migration order for the dedicated connive tail and its draw"
        );
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
    fn damage_record_target_incarnation_roundtrips_and_defaults_for_legacy_records() {
        let record = DamageRecord {
            target: TargetRef::Object(ObjectId(7)),
            target_incarnation: Some(3),
            ..Default::default()
        };
        let mut value = serde_json::to_value(&record).unwrap();
        assert_eq!(value["target_incarnation"], 3);
        assert_eq!(
            serde_json::from_value::<DamageRecord>(value.clone())
                .unwrap()
                .target_incarnation,
            Some(3)
        );

        value.as_object_mut().unwrap().remove("target_incarnation");
        assert_eq!(
            serde_json::from_value::<DamageRecord>(value)
                .unwrap()
                .target_incarnation,
            None
        );
    }

    #[test]
    fn rng_word_pos_survives_serde_and_rehydrate_resumes_stream() {
        // Issue #5466: a restored snapshot must resume the ChaCha20 stream at the
        // offset it held when exported — not rewind to origin and replay the
        // values the game already consumed. Exercises the ENGINE seam that the
        // WASM bridge delegates to (`capture_rng_word_pos` on export /
        // `rehydrate_rng` on restore); reverting either method breaks this test.
        use rand::RngCore;
        let mut state = GameState::new_two_player(0xABCD_1234);
        for _ in 0..7 {
            state.rng.next_u32(); // consume randomness as gameplay would
        }
        state.capture_rng_word_pos(); // production export-time capture
        let mut expected = state.rng.clone(); // the values that come next
        let json = serde_json::to_string(&state).unwrap();

        let mut restored: GameState = serde_json::from_str(&json).unwrap();
        restored.rehydrate_rng(); // production restore-time reseed + fast-forward

        assert_ne!(
            restored.rng_word_pos, 0,
            "stream position must survive serde"
        );
        for i in 0..5 {
            assert_eq!(
                restored.rng.next_u32(),
                expected.next_u32(),
                "restored stream diverged at draw {i}",
            );
        }
    }

    #[test]
    fn rng_word_pos_defaults_to_zero_when_absent() {
        // Backward compat (#5466): a snapshot serialized before the field
        // existed omits `rng_word_pos` and must deserialize to 0 — today's
        // rewind-to-origin behavior — via `#[serde(default)]`.
        let mut value = serde_json::to_value(GameState::default()).unwrap();
        value
            .as_object_mut()
            .expect("GameState serializes as a JSON object")
            .remove("rng_word_pos");
        let restored: GameState = serde_json::from_value(value).unwrap();
        assert_eq!(restored.rng_word_pos, 0);
    }

    /// Test E — deserialize-before-flush. `static_mode_presence` is `#[serde(skip)]` with an
    /// `all_present` default, so a freshly-deserialized state (before any layers flush) has a
    /// conservative all-present index. Gated consumers must stay CORRECT under that default by
    /// falling through to their exact per-object scan; a full flush then makes the index
    /// precise (a kind absent from the board reports false).
    #[test]
    fn static_mode_presence_defaults_all_present_after_deserialize() {
        use crate::game::zones::create_object;
        use crate::types::ability::StaticDefinition;
        use crate::types::statics::{StaticMode, StaticModeKind};

        let mut state = GameState::new_two_player(42);
        let src = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Detection Tower".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&src).unwrap().static_definitions =
            vec![StaticDefinition::new(StaticMode::IgnoreHexproof)].into();
        crate::game::layers::evaluate_layers(&mut state);

        let json = serde_json::to_string(&state).unwrap();
        let restored: GameState = serde_json::from_str(&json).unwrap();

        // Before any flush: the conservative all-present default (a board-absent kind still
        // reports present).
        assert!(restored
            .static_mode_presence
            .contains(StaticModeKind::IgnoreHexproof));
        assert!(restored
            .static_mode_presence
            .contains(StaticModeKind::Shroud));
        // Gated-consumer correctness under the all-present default: the Detection Tower grant
        // is found because the consumer falls through to its exact scan, not the index.
        assert!(crate::game::static_abilities::player_ignores_hexproof(
            &restored,
            PlayerId(0)
        ));

        // After a full flush the index is precise: a board-absent kind reports false.
        let mut flushed = restored;
        flushed.layers_dirty = LayersDirty::full();
        crate::game::layers::flush_layers(&mut flushed);
        assert!(!crate::game::functioning_abilities::static_kind_present(
            &flushed,
            StaticModeKind::Shroud
        ));
        assert!(crate::game::functioning_abilities::static_kind_present(
            &flushed,
            StaticModeKind::IgnoreHexproof
        ));
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
                declared_mana_additions: Vec::new(),
                activation_cost: None,
                activation_ability_index: None,
                pending_loyalty_activation_player: None,
                target_constraints: vec![],
                casting_variant: CastingVariant::Normal,
                casting_permission_index: None,
                cast_timing_permission: None,
                distribute: None,
                origin_zone: Zone::Hand,
                additional_cost_flow: None,
                deferred_required_additional_cost: None,
                additional_cost_queue: Vec::new(),
                additional_cost_source: SpellCostSource::Other,
                additional_cost_payment_mode: None,
                deferred_modal_choice: None,
                deferred_target_selection: false,
                chosen_modes: Vec::new(),
                additional_cost_decided: false,
                declared_kickers_to_pay: Vec::new(),
                declined_kickers: Vec::new(),
                convoked_creatures: Vec::new(),
                deferred_sacrificed_permanents: Vec::new(),
                pinned_pool_units: Vec::new(),
                cancel_restore_prepared_source: None,
                payment_mode: CastPaymentMode::Auto,
                assist_state: AssistState::NotOffered,
                activation_residual: ActivationResidual::None,
                activation_target_selection: ActivationTargetSelection::Pending,
                alt_cost_grant_source: None,
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
                phase: MulliganDecisionPhase::Declare,
            }],
            free_first_mulligan: false,
        }));
        variants.push(Box::new(WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 1,
                phase: MulliganDecisionPhase::BottomCards {
                    count: 1,
                    then: PendingMulliganAction::Keep,
                },
            }],
            free_first_mulligan: false,
        }));
        variants.push(Box::new(WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 2,
                phase: MulliganDecisionPhase::BottomCards {
                    count: 2,
                    then: PendingMulliganAction::UseSerumPowder {
                        object_id: ObjectId(7),
                    },
                },
            }],
            free_first_mulligan: false,
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
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        }));
        variants.push(Box::new(WaitingFor::DeclareBlockers {
            player: PlayerId(0),
            valid_blocker_ids: vec![],
            valid_block_targets: HashMap::new(),
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
        }));
        variants.push(Box::new(WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        }));
        variants.push(Box::new(WaitingFor::ReplacementChoice {
            player: PlayerId(0),
            candidate_count: 2,
            candidates: vec![],
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
        variants.push(Box::new(WaitingFor::ArrangePlanarDeckTopChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1), ObjectId(2)],
            keep_on_top: 1,
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
            replacement_applied: Default::default(),
            remaining_players: vec![],
        }));
        variants.push(Box::new(WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            trigger_controller: None,
            trigger_event: None,
            trigger_events: Vec::new(),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Object(ObjectId(1))],
                optional: false,
                chooser: None,
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
            unavailable_modes: vec![],
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
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: None,
            is_cost_payment: false,
            enters_modified_if: None,
        }));
        variants.push(Box::new(WaitingFor::DefilerPayment {
            player: PlayerId(0),
            life_cost: 2,
            mana_reduction: ManaCost::zero(),
            pending_cast: dummy_pending(),
        }));
        assert_eq!(variants.len(), 35);
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
            declared_mana_additions: Vec::new(),
            activation_cost: None,
            activation_ability_index: None,
            pending_loyalty_activation_player: None,
            target_constraints: vec![],
            casting_variant: CastingVariant::Normal,
            casting_permission_index: None,
            cast_timing_permission: None,
            distribute: None,
            origin_zone: Zone::Hand,
            additional_cost_flow: None,
            deferred_required_additional_cost: None,
            additional_cost_queue: Vec::new(),
            additional_cost_source: SpellCostSource::Other,
            additional_cost_payment_mode: None,
            deferred_modal_choice: None,
            deferred_target_selection: false,
            chosen_modes: Vec::new(),
            additional_cost_decided: false,
            declared_kickers_to_pay: Vec::new(),
            declined_kickers: Vec::new(),
            convoked_creatures: Vec::new(),
            deferred_sacrificed_permanents: Vec::new(),
            pinned_pool_units: Vec::new(),
            cancel_restore_prepared_source: None,
            payment_mode: CastPaymentMode::Auto,
            assist_state: AssistState::NotOffered,
            activation_residual: ActivationResidual::None,
            activation_target_selection: ActivationTargetSelection::Pending,
            alt_cost_grant_source: None,
        });
        let choose_x = WaitingFor::ChooseXValue {
            player: PlayerId(0),
            min: 0,
            max: 5,
            pending_cast: pending.clone(),
            convoke_mode: None,
            x_cost_previews: vec![],
        };
        assert!(choose_x.pending_cast_ref().is_some());
        assert!(choose_x.has_pending_cast());

        let announcing_opponent = WaitingFor::ChooseAnnouncingOpponent {
            player: PlayerId(0),
            candidates: vec![PlayerId(1), PlayerId(2)],
            choice_index: 1,
            choice_count: 2,
            target_type: Some(crate::types::card_type::CoreType::Land),
            pending_cast: pending.clone(),
        };
        assert!(announcing_opponent.pending_cast_ref().is_some());
        assert!(announcing_opponent.has_pending_cast());
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
            kind: PayCostKind::TapCreatures { aggregate: None },
            choices: vec![ObjectId(1)],
            count: 1,
            min_count: 0,
            resume: CostResume::ManaAbility {
                mana_ability: Box::new(PendingManaAbility {
                    player: PlayerId(0),
                    source_id: ObjectId(1),
                    ability_index: 0,
                    ability_snapshot: None,
                    color_override: None,
                    resume: ManaAbilityResume::Priority,
                    cost_move_resume: None,
                    chosen_tappers: Vec::new(),
                    chosen_discards: Vec::new(),
                    chosen_mana_payment: None,
                    chosen_counter_count: None,
                    chosen_x: None,
                    collected_evidence: Vec::new(),
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
            trigger_controller: None,
            trigger_event: None,
            trigger_events: Vec::new(),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![
                    TargetRef::Object(ObjectId(1)),
                    TargetRef::Object(ObjectId(2)),
                ],
                optional: false,
                chooser: None,
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
    fn crew_vehicle_legacy_missing_contributions_deserializes() {
        let json = r#"{
            "type":"CrewVehicle",
            "data":{
                "player":0,
                "vehicle_id":30,
                "crew_power":3,
                "eligible_creatures":[10]
            }
        }"#;
        let wf: WaitingFor = serde_json::from_str(json).unwrap();
        assert_eq!(
            wf,
            WaitingFor::CrewVehicle {
                player: PlayerId(0),
                vehicle_id: ObjectId(30),
                crew_power: 3,
                eligible_creatures: vec![ObjectId(10)],
                contributions: Vec::new(),
            }
        );
    }

    #[test]
    fn companion_reveal_legacy_sideboard_offer_deserializes() {
        let json = r#"{
            "type":"CompanionReveal",
            "data":{
                "player":0,
                "eligible_companions":[["Lurrus of the Dream-Den",2]]
            }
        }"#;
        let waiting_for: WaitingFor = serde_json::from_str(json).unwrap();
        assert_eq!(
            waiting_for,
            WaitingFor::CompanionReveal {
                player: PlayerId(0),
                eligible_companions: vec![CompanionRevealChoice {
                    name: "Lurrus of the Dream-Den".to_string(),
                    source: CompanionChoiceSource::Sideboard { index: 2 },
                }],
            }
        );
    }

    #[test]
    fn deck_pool_without_dedicated_companion_defaults_to_empty() {
        let mut json = serde_json::to_value(PlayerDeckPool::default()).unwrap();
        let fields = json.as_object_mut().unwrap();
        fields.remove("registered_companion");
        fields.remove("current_companion");

        let pool: PlayerDeckPool = serde_json::from_value(json).unwrap();
        assert!(pool.registered_companion.is_empty());
        assert!(pool.current_companion.is_empty());
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
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: None,
            is_cost_payment: false,
            enters_modified_if: None,
        };
        let json = serde_json::to_string(&wf).unwrap();
        let deserialized: WaitingFor = serde_json::from_str(&json).unwrap();
        assert_eq!(wf, deserialized);
        assert!(json.contains("\"EffectZoneChoice\""));
    }

    /// CR 502.3: the bounded untap-subset prompt must survive serde round-trip
    /// so the human-play transport can present it (and reload it) verbatim.
    #[test]
    fn choose_untap_subset_roundtrips() {
        let wf = WaitingFor::ChooseUntapSubset {
            player: PlayerId(0),
            group: vec![ObjectId(2), ObjectId(3), ObjectId(4)],
            max: 1,
        };
        let json = serde_json::to_string(&wf).unwrap();
        let deserialized: WaitingFor = serde_json::from_str(&json).unwrap();
        assert_eq!(wf, deserialized);
        assert!(json.contains("\"ChooseUntapSubset\""));
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
            conditional_enter_with_counters: vec![],
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
            library_placement: None,
            effect_kind: crate::types::ability::EffectKind::ChangeZone,
            enters_modified_if: None,
            enter_attached_to: None,
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
            enter_with_counters: vec![],
            conditional_enter_with_counters: vec![],
            count_param: 0,
            library_position: None,
            is_cost_payment: false,
            enters_modified_if: None,
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
        state.set_may_trigger_auto_choice(key.clone(), AutoMayChoice::Accept);

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

    #[test]
    fn game_state_deserialize_materializes_proven_legacy_printed_trigger_payload() {
        let object_id = ObjectId(991);
        let trigger = crate::types::ability::TriggerDefinition::new(
            crate::types::triggers::TriggerMode::Phase,
        );
        let mut state = GameState::new_two_player(42);
        let mut object = GameObject::new(
            object_id,
            CardId(991),
            PlayerId(0),
            "Legacy printed trigger".to_string(),
            Zone::Battlefield,
        );
        object.base_trigger_definitions = Arc::new(vec![trigger.clone()]);
        state.objects.insert(object_id, object);

        let mut snapshot = serde_json::to_value(state).expect("serialize fixture state");
        snapshot["objects"][object_id.0.to_string()]["trigger_definitions"] =
            serde_json::to_value(vec![trigger]).expect("serialize legacy trigger payload");

        let restored: GameState = serde_json::from_value(snapshot)
            .expect("persisted base slots prove this legacy printed payload");
        assert!(matches!(
            restored.objects[&object_id].trigger_definitions[0].occurrence,
            crate::types::ability::TriggerDefinitionOccurrenceRef::Printed {
                printed_index: 0,
                ..
            }
        ));
    }

    #[test]
    fn game_state_deserialize_rejects_unproven_legacy_trigger_payload() {
        let object_id = ObjectId(992);
        let mut state = GameState::new_two_player(42);
        state.objects.insert(
            object_id,
            GameObject::new(
                object_id,
                CardId(992),
                PlayerId(0),
                "Hostile payload".to_string(),
                Zone::Battlefield,
            ),
        );

        let mut snapshot = serde_json::to_value(state).expect("serialize fixture state");
        snapshot["objects"][object_id.0.to_string()]["trigger_definitions"] =
            serde_json::to_value(vec![crate::types::ability::TriggerDefinition::new(
                crate::types::triggers::TriggerMode::Attacks,
            )])
            .expect("serialize hostile legacy payload");

        let error = serde_json::from_value::<GameState>(snapshot)
            .expect_err("payload-only runtime triggers must not be guessed as printed");
        assert!(
            error
                .to_string()
                .contains("legacy runtime trigger payload has no provable producer or base slot"),
            "restore must reject unproven trigger provenance, got {error}"
        );
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
        assert!(!state.has_post_replacement_drain());
        assert!(state.legacy_post_replacement_effect.is_some());

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation() {
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
        assert!(!state.has_post_replacement_drain());
        assert!(state.legacy_post_replacement_resolved_effect.is_some());

        state.migrate_post_replacement_continuation();

        match state.post_replacement_continuation() {
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
            was_kicked: false,
        };
        let json = serde_json::to_string(&original).unwrap();
        let round_tripped: SpellCastRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(round_tripped, original);
        assert_eq!(round_tripped.from_zone, Zone::Graveyard);
    }

    // ---- CR 117.3d priority-yield accessors ----

    /// Build a `TriggeredAbility` stack entry from `source_id` whose ability
    /// latched `incarnation` (CR 400.7) and `card_id` (CR 704.5d) at push.
    fn triggered_entry(
        entry_id: ObjectId,
        source_id: ObjectId,
        controller: PlayerId,
        incarnation: Option<u64>,
        card_id: Option<CardId>,
    ) -> StackEntry {
        let mut ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            source_id,
            controller,
        );
        ability.source_incarnation = incarnation;
        ability.source_card_id = card_id;
        StackEntry {
            id: entry_id,
            source_id,
            controller,
            kind: StackEntryKind::TriggeredAbility {
                source_id,
                ability: Box::new(ability),
                condition: None,
                trigger_event: None,
                description: None,
                source_name: "Token".to_string(),
                subject_match_count: None,
                die_result: None,
            },
        }
    }

    #[test]
    fn this_object_yield_matches_same_object_and_incarnation() {
        let mut state = GameState::new_two_player(1);
        let entry = triggered_entry(
            ObjectId(10),
            ObjectId(5),
            PlayerId(1),
            Some(3),
            Some(CardId(9)),
        );
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(3),
                trigger_description: None,
            },
        );
        assert!(state.is_priority_yielded(PlayerId(0), &entry));
        // Wrong player never matches.
        assert!(!state.is_priority_yielded(PlayerId(1), &entry));
    }

    /// CR 400.7: a re-entered permanent is a new object with a higher
    /// incarnation, so a `ThisObject` yield stops matching after a blink.
    #[test]
    fn this_object_yield_invalidated_by_incarnation_advance() {
        let mut state = GameState::new_two_player(1);
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(3),
                trigger_description: None,
            },
        );
        let same = triggered_entry(
            ObjectId(10),
            ObjectId(5),
            PlayerId(1),
            Some(3),
            Some(CardId(9)),
        );
        let blinked = triggered_entry(
            ObjectId(11),
            ObjectId(5),
            PlayerId(1),
            Some(4),
            Some(CardId(9)),
        );
        assert!(state.is_priority_yielded(PlayerId(0), &same));
        assert!(!state.is_priority_yielded(PlayerId(0), &blinked));
    }

    /// CR 704.5d: an `AllCopies` yield matches every trigger sharing the latched
    /// card identity — both same-CardId tokens — regardless of object id.
    #[test]
    fn all_copies_yield_matches_every_same_card_id_trigger() {
        let mut state = GameState::new_two_player(1);
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None,
            },
        );
        let token_a = triggered_entry(
            ObjectId(10),
            ObjectId(5),
            PlayerId(1),
            Some(1),
            Some(CardId(9)),
        );
        let token_b = triggered_entry(
            ObjectId(11),
            ObjectId(6),
            PlayerId(1),
            Some(2),
            Some(CardId(9)),
        );
        assert!(state.is_priority_yielded(PlayerId(0), &token_a));
        assert!(state.is_priority_yielded(PlayerId(0), &token_b));
    }

    /// Hostile: a same-NAME but different-CardId real card must NOT be swept up
    /// by an `AllCopies` yield keyed to the token's card identity.
    #[test]
    fn all_copies_yield_does_not_match_different_card_id() {
        let mut state = GameState::new_two_player(1);
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None,
            },
        );
        let real_card = triggered_entry(
            ObjectId(12),
            ObjectId(7),
            PlayerId(1),
            Some(1),
            Some(CardId(42)),
        );
        assert!(!state.is_priority_yielded(PlayerId(0), &real_card));
    }

    /// Only triggered abilities are yieldable; spells and activated abilities on
    /// top of the stack never match a yield.
    #[test]
    fn spell_and_activated_tops_never_yielded() {
        let mut state = GameState::new_two_player(1);
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None,
            },
        );
        let spell = StackEntry {
            id: ObjectId(20),
            source_id: ObjectId(5),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(9),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        };
        let mut act_ability = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            vec![],
            ObjectId(5),
            PlayerId(1),
        );
        act_ability.source_card_id = Some(CardId(9));
        let activated = StackEntry {
            id: ObjectId(21),
            source_id: ObjectId(5),
            controller: PlayerId(1),
            kind: StackEntryKind::ActivatedAbility {
                source_id: ObjectId(5),
                ability: act_ability,
            },
        };
        assert!(!state.is_priority_yielded(PlayerId(0), &spell));
        assert!(!state.is_priority_yielded(PlayerId(0), &activated));
    }

    /// CR 400.7 incarnation identity: a `Some`-incarnation `ThisObject` yield
    /// never matches a trigger that latched no incarnation (synthetic game-rule
    /// triggers, `source_incarnation: None`), but an `AllCopies` yield still
    /// matches when the card identity is present. (The matching `None`-yield /
    /// `None`-trigger case is covered by the G6 synthetic-latch test.)
    #[test]
    fn this_object_some_incarnation_never_matches_none_trigger_but_all_copies_can() {
        let mut state = GameState::new_two_player(1);
        let entry = triggered_entry(
            ObjectId(10),
            ObjectId(0),
            PlayerId(1),
            None,
            Some(CardId(9)),
        );
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::ThisObject {
                source_id: ObjectId(0),
                incarnation: Some(0),
                trigger_description: None,
            },
        );
        assert!(!state.is_priority_yielded(PlayerId(0), &entry));
        state.clear_priority_yields(PlayerId(0));
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None,
            },
        );
        assert!(state.is_priority_yielded(PlayerId(0), &entry));
    }

    #[test]
    fn add_priority_yield_is_idempotent_and_remove_and_clear_work() {
        let mut state = GameState::new_two_player(1);
        let t = YieldTarget::AllCopies {
            card_id: CardId(9),
            trigger_description: None,
        };
        state.add_priority_yield(PlayerId(0), t.clone());
        state.add_priority_yield(PlayerId(0), t.clone());
        assert_eq!(state.priority_yields.len(), 1, "dedup: no duplicate yield");
        state.add_priority_yield(PlayerId(1), t.clone());
        state.remove_priority_yield(PlayerId(0), &t);
        assert_eq!(state.priority_yields.len(), 1);
        assert_eq!(state.priority_yields[0].player, PlayerId(1));
        state.add_priority_yield(
            PlayerId(1),
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(1),
                trigger_description: None,
            },
        );
        state.clear_priority_yields(PlayerId(1));
        assert!(state.priority_yields.is_empty());
    }

    /// CR 400.7 identity latch: `resolve_yield_target_from_stack` reads the
    /// identity off the on-stack trigger, so a ceased token (absent from
    /// `objects`) still yields a bindable target.
    #[test]
    fn resolve_yield_target_reads_from_stack_topmost() {
        let mut state = GameState::new_two_player(1);
        state.stack.push_back(triggered_entry(
            ObjectId(10),
            ObjectId(5),
            PlayerId(0),
            Some(7),
            Some(CardId(9)),
        ));
        assert_eq!(
            state.resolve_yield_target_from_stack(ObjectId(5), YieldScope::ThisObject),
            Some(YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(7),
                trigger_description: None,
            })
        );
        assert_eq!(
            state.resolve_yield_target_from_stack(ObjectId(5), YieldScope::AllCopies),
            Some(YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None
            })
        );
        // No matching source on the stack → None (caller no-ops).
        assert_eq!(
            state.resolve_yield_target_from_stack(ObjectId(999), YieldScope::ThisObject),
            None
        );
    }

    /// G6 (CR 400.7): a `ThisObject` scope on a trigger that never latched an
    /// incarnation now resolves to `Some` with `incarnation: None` — the
    /// synthetic/delayed latch that was previously a silent no-op.
    #[test]
    fn resolve_yield_target_none_incarnation_latches_none() {
        let mut state = GameState::new_two_player(1);
        state.stack.push_back(triggered_entry(
            ObjectId(10),
            ObjectId(0),
            PlayerId(0),
            None,
            Some(CardId(9)),
        ));
        assert_eq!(
            state.resolve_yield_target_from_stack(ObjectId(0), YieldScope::ThisObject),
            Some(YieldTarget::ThisObject {
                source_id: ObjectId(0),
                incarnation: None,
                trigger_description: None,
            })
        );
        assert_eq!(
            state.resolve_yield_target_from_stack(ObjectId(0), YieldScope::AllCopies),
            Some(YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None
            })
        );
    }

    #[test]
    fn priority_yields_serde_round_trip_and_partial_eq() {
        let mut state = GameState::new_two_player(1);
        // Empty default omitted from serialization.
        let empty_json = serde_json::to_string(&state).unwrap();
        assert!(!empty_json.contains("priority_yields"));

        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(3),
                trigger_description: None,
            },
        );
        state.add_priority_yield(
            PlayerId(1),
            YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: None,
            },
        );
        let json = serde_json::to_string(&state).unwrap();
        let restored: GameState = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.priority_yields, state.priority_yields);
        assert_eq!(restored, state, "PartialEq must include priority_yields");

        // PartialEq is sensitive to a yield difference.
        let mut other = state.clone();
        other.priority_yields.clear();
        assert_ne!(other, state);
    }

    /// Same as `triggered_entry` but latches a per-trigger `description`
    /// (the G5 discriminator the stack entry already carries).
    fn triggered_entry_desc(
        entry_id: ObjectId,
        source_id: ObjectId,
        controller: PlayerId,
        incarnation: Option<u64>,
        card_id: Option<CardId>,
        description: Option<String>,
    ) -> StackEntry {
        let mut entry = triggered_entry(entry_id, source_id, controller, incarnation, card_id);
        if let StackEntryKind::TriggeredAbility { description: d, .. } = &mut entry.kind {
            *d = description;
        }
        entry
    }

    /// G5 (CR 117.3d per-ability key): one source with two distinctly-described
    /// triggers. Yielding the resolved target of trigger A must pass A but leave
    /// trigger B (same source, same incarnation, different description) held.
    #[test]
    fn this_object_yield_is_per_trigger_description_scoped() {
        let mut state = GameState::new_two_player(1);
        let entry_a = triggered_entry_desc(
            ObjectId(10),
            ObjectId(5),
            PlayerId(1),
            Some(3),
            Some(CardId(9)),
            Some("Whenever this enters, draw a card.".to_string()),
        );
        // Resolve the concrete yield target off trigger A on the stack.
        state.stack.push_back(entry_a.clone());
        let target = state
            .resolve_yield_target_from_stack(ObjectId(5), YieldScope::ThisObject)
            .expect("trigger A is on the stack");
        assert_eq!(
            target,
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(3),
                trigger_description: Some("Whenever this enters, draw a card.".to_string()),
            },
            "resolve must latch trigger A's description"
        );
        state.add_priority_yield(PlayerId(0), target);

        // Trigger B: same source + incarnation, DIFFERENT description.
        let entry_b = triggered_entry_desc(
            ObjectId(11),
            ObjectId(5),
            PlayerId(1),
            Some(3),
            Some(CardId(9)),
            Some("Whenever this enters, gain 2 life.".to_string()),
        );
        // Reach-guard: the yield really matches trigger A (input reached compare).
        assert!(
            state.is_priority_yielded(PlayerId(0), &entry_a),
            "trigger A must stay yielded"
        );
        // Per-trigger precision: trigger B is NOT swept up by A's yield.
        assert!(
            !state.is_priority_yielded(PlayerId(0), &entry_b),
            "a distinct trigger on the same source must remain held"
        );
    }

    /// G6 (CR 400.7 synthetic latch): a trigger that latched no incarnation
    /// (`source_incarnation: None`, e.g. a synthetic/delayed game-rule trigger)
    /// now resolves to a `ThisObject` yield storing `incarnation: None`, and that
    /// yield matches the same None-incarnation entry — previously a silent no-op.
    #[test]
    fn this_object_none_incarnation_yield_matches_synthetic_trigger() {
        let mut state = GameState::new_two_player(1);
        let synthetic = triggered_entry_desc(
            ObjectId(10),
            ObjectId(5),
            PlayerId(0),
            None,
            None,
            Some("At the beginning of your upkeep, draw a card.".to_string()),
        );
        state.stack.push_back(synthetic.clone());
        let target = state
            .resolve_yield_target_from_stack(ObjectId(5), YieldScope::ThisObject)
            .expect("G6: a None-incarnation trigger must still resolve a target");
        assert_eq!(
            target,
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: None,
                trigger_description: Some(
                    "At the beginning of your upkeep, draw a card.".to_string()
                ),
            }
        );
        state.add_priority_yield(PlayerId(0), target);
        assert!(
            state.is_priority_yielded(PlayerId(0), &synthetic),
            "G6: the None-incarnation latch must match its own trigger"
        );
        // Discrimination: a Some-incarnation entry (a real, latched object) must
        // NOT match the None-incarnation synthetic yield.
        let latched = triggered_entry_desc(
            ObjectId(11),
            ObjectId(5),
            PlayerId(0),
            Some(1),
            None,
            Some("At the beginning of your upkeep, draw a card.".to_string()),
        );
        assert!(
            !state.is_priority_yielded(PlayerId(0), &latched),
            "None-incarnation yield must not match a Some-incarnation entry"
        );
    }

    /// B3 (legacy-compat wildcard): a yield deserialized from a pre-upgrade save
    /// carries `trigger_description: None` (serde default) with a real
    /// incarnation. `None` is a wildcard, so it still matches its source's
    /// trigger regardless of that entry's description — old saves keep working.
    #[test]
    fn legacy_none_description_yield_is_source_level_wildcard() {
        let mut state = GameState::new_two_player(1);
        // Simulates a deserialized legacy yield (bare u64 incarnation → Some).
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::ThisObject {
                source_id: ObjectId(5),
                incarnation: Some(7),
                trigger_description: None,
            },
        );
        let described = triggered_entry_desc(
            ObjectId(10),
            ObjectId(5),
            PlayerId(0),
            Some(7),
            Some(CardId(9)),
            Some("Whenever this attacks, draw a card.".to_string()),
        );
        assert!(
            state.is_priority_yielded(PlayerId(0), &described),
            "None-description wildcard must match any described entry"
        );
        // Reach-guard / non-vacuous: incarnation is still enforced, so a blinked
        // (Some(8)) entry does NOT match — the wildcard is on description only.
        let blinked = triggered_entry_desc(
            ObjectId(11),
            ObjectId(5),
            PlayerId(0),
            Some(8),
            Some(CardId(9)),
            Some("Whenever this attacks, draw a card.".to_string()),
        );
        assert!(
            !state.is_priority_yielded(PlayerId(0), &blinked),
            "wildcard applies to description only, not incarnation"
        );
    }

    /// G5 for `AllCopies`: a card-scoped yield can also be description-scoped.
    /// A matching description passes; a different trigger on the same card holds.
    #[test]
    fn all_copies_yield_respects_trigger_description() {
        let mut state = GameState::new_two_player(1);
        state.add_priority_yield(
            PlayerId(0),
            YieldTarget::AllCopies {
                card_id: CardId(9),
                trigger_description: Some("Whenever this enters, draw a card.".to_string()),
            },
        );
        let matching = triggered_entry_desc(
            ObjectId(10),
            ObjectId(5),
            PlayerId(1),
            Some(1),
            Some(CardId(9)),
            Some("Whenever this enters, draw a card.".to_string()),
        );
        let other_trigger = triggered_entry_desc(
            ObjectId(11),
            ObjectId(6),
            PlayerId(1),
            Some(2),
            Some(CardId(9)),
            Some("Whenever this dies, gain 2 life.".to_string()),
        );
        // Reach-guard positive: the card id matches, so the description compare
        // is actually consulted (not a vacuous card-id miss).
        assert!(
            state.is_priority_yielded(PlayerId(0), &matching),
            "same card + same description must match"
        );
        assert!(
            !state.is_priority_yielded(PlayerId(0), &other_trigger),
            "same card but different trigger description must remain held"
        );
    }
}

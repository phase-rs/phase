use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use super::counter::CounterType;

use super::ability::{
    AbilityTag, AttachmentKind, CostPaidObjectSnapshot, EffectKind, FilterProp, TargetFilter,
    TargetRef, ThisWayCause, TypeFilter, TypedFilter,
};
use super::card_type::{CoreType, Supertype};
use super::game_state::ZoneChangeRecord;
use super::identifiers::{CardId, ObjectId, ObjectIncarnationRef, TrackedSetId};
use super::keywords::Keyword;
use super::mana::{ManaColor, ManaType};
use super::phase::Phase;
use super::player::{PlayerCounterKind, PlayerId};
use super::stickers::StickerKind;
use super::zones::Zone;

/// CR 121.1: Default `nth_in_step` for `CardDrawn` events deserialized from
/// older serialized state that predates the field. `1` means "first draw" —
/// the most permissive default for `ExceptFirstDrawInDrawStep` evaluators
/// (mirrors the natural draw-step behavior).
fn default_nth_in_step() -> u32 {
    1
}

fn default_nth_in_turn() -> u32 {
    1
}

/// CR 605.1a + CR 605.1b + CR 605.4a: Records whether a `ManaAdded` event was
/// produced by tapping a mana source, and whether the coupled `TapsForMana`
/// triggered mana abilities have already been resolved.
///
/// A triggered mana ability (CR 605.1b) resolves immediately after the mana
/// ability that triggered it, without using the stack (CR 605.4a). During an
/// auto-tapped cost payment the engine resolves those triggers inline so the
/// bonus mana is available for the affordability check; `FromTapTriggersResolved`
/// marks such events so the deferred post-action trigger scan does not resolve
/// them a second time.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaTapState {
    /// Mana not produced by a tap — effects, triggers, convoke, doublers.
    #[default]
    NotFromTap,
    /// Produced by tapping a source (CR 605.1a tap cost); coupled `TapsForMana`
    /// triggered mana abilities have not yet been resolved.
    FromTap,
    /// Produced by tapping a source; coupled triggered mana abilities were
    /// already resolved inline during cost payment (CR 605.4a).
    FromTapTriggersResolved,
}

/// CR 602.2 + CR 606.2: Discriminates how an activated ability was activated so
/// that "Whenever you activate a loyalty ability" triggers (CR 606.2) can be told
/// apart from ordinary activated abilities (CR 602.2) while both share the single
/// `GameEvent::AbilityActivated` event family. A loyalty ability is an activated
/// ability of a planeswalker paid for by adding or removing loyalty counters.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub enum ActivatedAbilityKind {
    /// CR 602.2: An ordinary activated ability.
    #[default]
    Normal,
    /// CR 606.1 + CR 606.2: A loyalty ability of a planeswalker.
    Loyalty,
}

impl ManaTapState {
    /// True when the mana was produced by tapping a source, regardless of
    /// whether the coupled triggered mana abilities have been resolved yet.
    pub fn tapped_for_mana(self) -> bool {
        !matches!(self, ManaTapState::NotFromTap)
    }

    /// Pre-resolution tap state for a freshly produced mana event: `FromTap`
    /// when the source was tapped, `NotFromTap` otherwise.
    pub fn from_tap(tapped: bool) -> Self {
        if tapped {
            ManaTapState::FromTap
        } else {
            ManaTapState::NotFromTap
        }
    }

    /// Serde `skip_serializing_if` predicate — omit the default from the wire.
    fn is_not_from_tap(&self) -> bool {
        matches!(self, ManaTapState::NotFromTap)
    }
}

/// Avatar crossover: The four elemental bending types, tracked per-turn on each player.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum BendingType {
    Fire,
    Air,
    Earth,
    Water,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum PlayerActionKind {
    /// A player accepted a resolution-time optional effect.
    AcceptedOptionalEffect,
    SearchedLibrary,
    Scry,
    Surveil,
    CollectEvidence,
    /// CR 701.24a: A player shuffled their library.
    ShuffledLibrary,
    /// CR 701.34a: A player proliferated.
    Proliferate,
    /// CR 701.16a: A player investigated (created a Clue token).
    Investigate,
}

/// CR 701.30d: Result of a clash — whether the controller won, lost, or tied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClashResult {
    Won,
    Lost,
    Tied,
}

impl ClashResult {
    /// CR 701.30d: A clash's `result` is stated from the clash controller's
    /// perspective (the player who initiated the clash). Re-express it from
    /// `player`'s perspective, returning `None` if `player` did not participate.
    /// The controller sees `self`; the opponent sees the mirror (Won ⇄ Lost, Tied
    /// unchanged).
    ///
    /// Single source of truth shared by resolution-time "if you won" gating
    /// (`event_outcome_was_won_by_controller`) and trigger MATCHING
    /// (`match_clash`'s `clash_result` gate) so both agree on who won.
    pub fn for_player(
        self,
        clash_controller: PlayerId,
        opponent: PlayerId,
        player: PlayerId,
    ) -> Option<ClashResult> {
        if player == clash_controller {
            Some(self)
        } else if player == opponent {
            Some(match self {
                ClashResult::Won => ClashResult::Lost,
                ClashResult::Lost => ClashResult::Won,
                ClashResult::Tied => ClashResult::Tied,
            })
        } else {
            None
        }
    }
}

/// CR 103.1 / CR 706: one round of the starting-player d20 roll-off.
/// `rolls` are in seat order; round 1 contains every seat, and each later
/// round contains exactly the previous round's tied-max group (CR 103.1
/// reroll). The high roller of the final round becomes the starting player.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContestRound {
    pub rolls: Vec<(PlayerId, u8)>,
}

/// CR 400.7 + CR 608.2b: One attachment on an [`EventObjectSnapshot`], addressed by
/// exact incarnation rather than by raw `ObjectId`.
///
/// The live sibling is [`crate::types::game_state::AttachmentSnapshot`], which stores a
/// bare `ObjectId`. That is a look-back convenience: by the time an event subject is
/// evaluated, the storage id may have been reused by a *new* object (CR 400.7), so a raw
/// id is not proof of identity. This record therefore carries the full
/// [`ObjectIncarnationRef`] and is never rebound to whatever currently sits at that id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventAttachmentSnapshot {
    pub identity: ObjectIncarnationRef,
    pub controller: PlayerId,
    pub kind: AttachmentKind,
}

/// CR 506.1 + CR 509.1: The subject's combat role as it stood at capture time.
///
/// `related_objects` is the exact-incarnation set the subject blocks or is blocked by.
/// Combat maps are keyed by raw `ObjectId` and are mutated (and cleared at end of
/// combat) after the event is captured, so a `CombatRelation` predicate that re-read them
/// at trigger time could match a different object — hence the frozen identity set.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCombatSnapshot {
    pub attacking: bool,
    pub blocking: bool,
    pub blocked: bool,
    pub attacking_alone: bool,
    pub blocking_alone: bool,
    /// CR 506.2: The player or planeswalker being attacked, when the subject is attacking.
    pub defending_player: Option<PlayerId>,
    pub related_objects: Vec<ObjectIncarnationRef>,
}

/// CR 603.10a: Per-turn history facts copied off the authoritative ledgers *while the
/// subject's identity is still known*, rather than re-read later by `ObjectId`.
///
/// Re-reading later is the bug this closes: the turn ledgers are keyed by raw
/// `ObjectId`, so after the subject leaves and a new object takes its id (CR 400.7), a
/// history lookup would answer for the wrong object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventObjectHistorySnapshot {
    pub was_dealt_damage_this_turn: bool,
    pub entered_this_turn: bool,
    /// CR 506.2: Defenders this subject attacked this turn.
    pub attacked_defenders_this_turn: Vec<PlayerId>,
    pub blocked_this_turn: bool,
    /// CR 603.6a: `(from, to)` zone transitions recorded for this exact incarnation.
    pub zone_changes_this_turn: Vec<(Option<Zone>, Zone)>,
    /// CR 122.1: Counters placed on this subject this turn, retaining actor and type.
    pub counters_put_on_this_turn: Vec<EventCounterHistoryEntry>,
}

/// CR 122.1: One "counters were put on this object this turn" record, retaining the actor
/// and counter kind so `CountersPutOnThisTurn { by, counter }` can be answered from the
/// snapshot alone.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventCounterHistoryEntry {
    pub actor: PlayerId,
    pub counter: CounterType,
    pub count: u32,
}

/// Object-to-object relations the subject participates in, each stored by exact
/// incarnation. A raw `ObjectId` here would let a *returned* object (a new incarnation at
/// the same id, CR 400.7) satisfy a `SaddledSource`/`ConvokedSource` predicate it never
/// earned.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventObjectRelationSnapshot {
    /// CR 702.164a: Sources that saddled this subject.
    pub saddled_sources: Vec<ObjectIncarnationRef>,
    /// CR 702.51a: Creatures that convoked this subject.
    pub convoked_sources: Vec<ObjectIncarnationRef>,
    /// Tracked-set memberships published for this exact incarnation, with the cause that
    /// put it there.
    pub tracked_sets: Vec<EventTrackedSetMembership>,
}

/// One concrete tracked-set membership for the subject, retaining the optional cause so
/// `TrackedSetFiltered { caused_by, .. }` is answerable without a live set lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventTrackedSetMembership {
    pub set_id: TrackedSetId,
    pub caused_by: Option<ThisWayCause>,
}

/// CR 400.7 + CR 608.2b + CR 701.50a: An immutable, identity-exact projection of the
/// object an event happened *to*, captured at the instruction that caused the event.
///
/// This exists because the engine's event payloads carry a raw `ObjectId`, and by the time
/// an event's observers run (replacement `valid_card` filtering, trigger `valid_card`
/// matching) the subject may have left the battlefield, or — worse — a *different* object
/// may now occupy that storage id. Under CR 400.7 that is a new object, so answering a
/// predicate about "the creature that connived" from live state keyed by `ObjectId` can
/// silently answer about someone else. Every candidate fact an observer can ask about the
/// subject is therefore frozen here at capture time.
///
/// Deliberately **not** a second `GameObject`: it stores only facts the reachable subject
/// grammar can interrogate. No ability/replacement/static definitions, no display
/// metadata, and no card-database payload enter the event.
///
/// Deliberately **not** `Default`: an absent field must never be readable as evidence that
/// the event-time fact was false. Backward compatibility lives at the enclosing
/// `Option<Box<EventObjectSnapshot>>`, not inside the record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EventObjectSnapshot {
    /// CR 400.7: the exact `(ObjectId, incarnation)` this snapshot speaks for.
    pub identity: ObjectIncarnationRef,
    pub controller: PlayerId,
    pub owner: PlayerId,
    pub zone: Zone,

    pub name: String,
    pub core_types: Vec<CoreType>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<Supertype>,
    pub colors: Vec<ManaColor>,
    pub keywords: Vec<Keyword>,
    /// CR 208.1: Power/toughness as of capture.
    pub power: Option<i32>,
    pub toughness: Option<i32>,
    /// CR 208.4b + CR 613.4b: layer-7b base P/T, so `PtComparison { scope: Base }` and
    /// `PowerExceedsBase` are answerable from the snapshot.
    pub base_power: Option<i32>,
    pub base_toughness: Option<i32>,
    /// CR 202.3: effective mana value as of capture.
    pub mana_value: u32,
    /// CR 122.1: counters on the subject as of capture.
    pub counters: HashMap<CounterType, u32>,

    pub is_token: bool,
    pub is_commander: bool,
    pub tapped: bool,
    pub face_down: bool,
    pub transformed: bool,
    pub is_suspected: bool,
    pub is_renowned: bool,
    pub is_saddled: bool,
    /// Capture-time derived fact (the existing `object_has_no_abilities` authority).
    /// Derived at capture because the ability list itself is deliberately not carried.
    pub has_no_abilities: bool,

    pub attachments: Vec<EventAttachmentSnapshot>,
    /// CR 702.16e: the subject's protector, when it has one.
    pub protector: Option<PlayerId>,
    pub combat: EventCombatSnapshot,
    pub history: EventObjectHistorySnapshot,
    pub relations: EventObjectRelationSnapshot,
}

/// How an [`EventObjectSnapshot`] can answer a given [`TargetFilter`] shape.
///
/// This is a *structural* classification, computed without a game state, so a parser gate
/// can assert at card-data generation time that every reachable Connives subject filter is
/// answerable from an event snapshot — before any card reaches the runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EventObjectFilterSupport {
    /// Fully answerable from the embedded snapshot (plus external, non-candidate context).
    Supported,
    /// Structurally reachable but necessarily `false` for an event subject, because the
    /// selector addresses a domain a permanent subject cannot be in — e.g. `ExiledBySource`
    /// (the subject is on the battlefield, not exiled with the source) or the stack-only
    /// target predicates (CR 701.50 connives a permanent, not a stack entry).
    ///
    /// Distinct from `Unsupported`: this is a decided `false`, not a coverage gap.
    PermanentDomainFalse,
    /// Not answerable from an event snapshot. Not reachable from the current subject
    /// grammar; if a parser change ever makes it reachable, the gate fails and this
    /// classification (and the evaluator) must be extended.
    Unsupported,
}

impl EventObjectFilterSupport {
    /// Fold a **conjunction** (`And`, and a `Typed` filter's types + properties, which the
    /// live evaluator combines with `.all()`).
    ///
    /// `Unsupported` dominates: an unanswerable conjunct makes the whole shape
    /// unanswerable, and the gate must fail. Otherwise a *single* `PermanentDomainFalse`
    /// conjunct decides the whole conjunction false — `false && x == false` — so
    /// `Typed { Creature, props: [HasSingleTarget] }` is domain-false, not "supported".
    fn combine_conjunction(children: impl IntoIterator<Item = Self>) -> Self {
        let mut domain_false = false;
        for c in children {
            match c {
                Self::Unsupported => return Self::Unsupported,
                Self::PermanentDomainFalse => domain_false = true,
                Self::Supported => {}
            }
        }
        if domain_false {
            Self::PermanentDomainFalse
        } else {
            // An empty conjunction is vacuously true, hence answerable.
            Self::Supported
        }
    }

    /// Fold a **disjunction** (`Or`, `FilterProp::AnyOf`, `TypeFilter::AnyOf`).
    ///
    /// `Unsupported` still dominates. But a disjunction is domain-false only when *every*
    /// branch is — `false || x == x` — so one answerable branch keeps the whole expression
    /// answerable, with the domain-false branch simply contributing `false`.
    fn combine_disjunction(children: impl IntoIterator<Item = Self>) -> Self {
        let mut saw_supported = false;
        for c in children {
            match c {
                Self::Unsupported => return Self::Unsupported,
                Self::Supported => saw_supported = true,
                Self::PermanentDomainFalse => {}
            }
        }
        if saw_supported {
            Self::Supported
        } else {
            // Empty or all-domain-false: decided false, not a coverage gap.
            Self::PermanentDomainFalse
        }
    }

    /// Negation of a decided-false is a decided-*true* — still answerable, hence
    /// `Supported`. Only an unanswerable inner shape stays unanswerable.
    fn negate(self) -> Self {
        match self {
            Self::Unsupported => Self::Unsupported,
            Self::Supported | Self::PermanentDomainFalse => Self::Supported,
        }
    }
}

impl EventObjectSnapshot {
    /// Structurally classify whether a [`TargetFilter`] can be evaluated against an event
    /// subject snapshot, without a game state.
    ///
    /// This is the gate that keeps the snapshot honest as the parser grows: card-data
    /// generation asserts that every parsed Connives `valid_card` classifies as *not*
    /// `Unsupported`. If a future parser change makes a new filter shape reachable from
    /// `parse_trigger_subject`, the gate fails and this classification — and the evaluator
    /// beside it — must be extended in the same change.
    ///
    /// Exhaustive by construction: no wildcard arm, so adding an engine `TargetFilter`
    /// variant is a compile error here rather than a silent `Unsupported`.
    pub fn classify_filter_shape(filter: &TargetFilter) -> EventObjectFilterSupport {
        use EventObjectFilterSupport::{PermanentDomainFalse, Supported, Unsupported};
        match filter {
            // ---- constants + identity: answered from the embedded snapshot ----
            // `None` is classified for exhaustiveness even though the subject grammar
            // does not currently emit it.
            TargetFilter::None | TargetFilter::Any => Supported,
            // CR 400.7: compared as exact (id, incarnation), never as a raw id.
            TargetFilter::SelfRef => Supported,
            // No parent-ability context exists for an ordinary Connives matcher, so this
            // fails closed rather than resolving the candidate from live state.
            TargetFilter::ParentTarget => Supported,
            // Compared against the exact attachment target derived from the trigger source.
            TargetFilter::AttachedTo => Supported,
            TargetFilter::HasChosenName | TargetFilter::Named { .. } => Supported,

            // ---- composites: recurse against the same snapshot ----
            TargetFilter::Typed(typed) => Self::classify_typed_filter(typed),
            TargetFilter::Not { filter } => Self::classify_filter_shape(filter).negate(),
            TargetFilter::Or { filters } => EventObjectFilterSupport::combine_disjunction(
                filters.iter().map(Self::classify_filter_shape),
            ),
            TargetFilter::And { filters } => EventObjectFilterSupport::combine_conjunction(
                filters.iter().map(Self::classify_filter_shape),
            ),
            // Concrete embedded membership + cause, then recurse the inner filter.
            TargetFilter::TrackedSetFiltered { filter, .. } => Self::classify_filter_shape(filter),

            // ---- permanent-domain false: reachable, but necessarily false ----
            // A Connive subject is a permanent on the battlefield; it is not currently
            // exiled with the source, so no exile-link payload is carried for it.
            TargetFilter::ExiledBySource => PermanentDomainFalse,
            // CR 701.50a connives a *permanent*, never a stack entry.
            TargetFilter::StackAbility { .. } | TargetFilter::StackSpell => PermanentDomainFalse,
            // Player-axis selectors: false on the object axis. This preserves current
            // semantics for a nonsensical player-Connives subject rather than inventing one.
            TargetFilter::Player
            | TargetFilter::Controller
            | TargetFilter::Owner
            | TargetFilter::AllPlayers
            | TargetFilter::ScopedPlayer
            | TargetFilter::SpecificPlayer { .. }
            | TargetFilter::PlayerWhoChoseLabel { .. }
            | TargetFilter::Neighbor { .. }
            | TargetFilter::DefendingPlayer
            | TargetFilter::SourceChosenPlayer
            | TargetFilter::OriginalController
            | TargetFilter::TriggeringPlayer
            | TargetFilter::TriggeringSpellController
            | TargetFilter::TriggeringSpellOwner
            | TargetFilter::TriggeringSourceController
            | TargetFilter::ParentTargetController
            | TargetFilter::ParentTargetOwner
            | TargetFilter::PostReplacementSourceController
            | TargetFilter::PostReplacementDamageTargetOwner => PermanentDomainFalse,

            // ---- unsupported: not reachable from the subject grammar today ----
            // Answering any of these would require resolving the candidate (or an engine
            // referent) out of live state, which is exactly what this snapshot forbids.
            // If the parser ever reaches one, the gate fails and it must be handled here.
            TargetFilter::GrantingObject
            | TargetFilter::SourceOrPaired
            | TargetFilter::SpecificObject { .. }
            | TargetFilter::LastCreated
            | TargetFilter::LastRevealed
            | TargetFilter::CostPaidObject
            | TargetFilter::ChosenCard
            | TargetFilter::TrackedSet { .. }
            | TargetFilter::ExiledCardByIndex { .. }
            | TargetFilter::TriggeringSource
            | TargetFilter::EventTarget
            | TargetFilter::ParentTargetSlot { .. }
            | TargetFilter::OriginalSource
            | TargetFilter::PostReplacementDamageTarget
            | TargetFilter::ChosenDamageSource { .. } => Unsupported,
        }
    }

    /// Classify a `Typed` filter. Its type axis and its controller axis are always
    /// answerable — types from the embedded core/sub/supertypes (with the Changeling rule
    /// read off embedded keywords), the controller by comparing the embedded controller to
    /// an externally resolved `ControllerRef` — so support is decided by its property list.
    fn classify_typed_filter(typed: &TypedFilter) -> EventObjectFilterSupport {
        EventObjectFilterSupport::combine_conjunction(
            typed
                .type_filters
                .iter()
                .map(Self::classify_type_filter)
                .chain(typed.properties.iter().map(Self::classify_prop)),
        )
    }

    /// Every type axis is answerable from the embedded types + keywords. Matched
    /// exhaustively anyway, so that a new engine `TypeFilter` variant is a compile error
    /// here rather than a silently-assumed `Supported`.
    fn classify_type_filter(ty: &TypeFilter) -> EventObjectFilterSupport {
        use EventObjectFilterSupport::Supported;
        match ty {
            TypeFilter::Creature
            | TypeFilter::Land
            | TypeFilter::Artifact
            | TypeFilter::Enchantment
            | TypeFilter::Instant
            | TypeFilter::Sorcery
            | TypeFilter::Planeswalker
            | TypeFilter::Battle
            | TypeFilter::Kindred
            | TypeFilter::Permanent
            | TypeFilter::Card
            | TypeFilter::Any
            | TypeFilter::Subtype(_) => Supported,
            TypeFilter::Non(inner) => Self::classify_type_filter(inner).negate(),
            TypeFilter::AnyOf(types) => EventObjectFilterSupport::combine_disjunction(
                types.iter().map(Self::classify_type_filter),
            ),
        }
    }

    /// Structurally classify one [`FilterProp`] against the event-subject snapshot.
    ///
    /// Exhaustive by construction — no wildcard arm.
    fn classify_prop(prop: &FilterProp) -> EventObjectFilterSupport {
        use EventObjectFilterSupport::{PermanentDomainFalse, Supported, Unsupported};
        match prop {
            // ---- embedded status bits / designations ----
            FilterProp::Token
            | FilterProp::NonToken
            | FilterProp::IsCommander
            | FilterProp::Tapped
            | FilterProp::Untapped
            | FilterProp::FaceDown
            | FilterProp::Transformed
            | FilterProp::Suspected
            | FilterProp::Renowned
            | FilterProp::IsSaddled => Supported,

            // ---- embedded characteristics ----
            FilterProp::WithKeyword { .. }
            | FilterProp::HasKeywordKind { .. }
            | FilterProp::WithoutKeyword { .. }
            | FilterProp::WithoutKeywordKind { .. }
            | FilterProp::HasColor { .. }
            | FilterProp::NotColor { .. }
            | FilterProp::ColorCount { .. }
            | FilterProp::HasSupertype { .. }
            | FilterProp::NotSupertype { .. }
            | FilterProp::Historic
            | FilterProp::NotHistoric
            | FilterProp::Cmc { .. }
            | FilterProp::ManaValueParity { .. }
            | FilterProp::HasNoAbilities => Supported,

            // ---- embedded P/T (current + layer-7b base) ----
            FilterProp::PtComparison { .. }
            | FilterProp::PowerGTSource
            | FilterProp::ToughnessGTPower
            | FilterProp::PowerExceedsBase => Supported,

            // ---- embedded counters; threshold may be resolved externally ----
            FilterProp::Counters { .. } | FilterProp::Modified => Supported,

            // ---- embedded zone ----
            FilterProp::InZone { .. } | FilterProp::InAnyZone { .. } => Supported,

            // ---- embedded controller/owner vs externally resolved ControllerRef ----
            FilterProp::Owned { .. } | FilterProp::ProtectorMatches { .. } => Supported,

            // ---- exact-identity comparisons against the trigger source ----
            // CR 400.7: a returned object at the same storage id is *another* object.
            FilterProp::Another => Supported,

            // ---- embedded attachments ----
            FilterProp::EnchantedBy
            | FilterProp::EquippedBy
            | FilterProp::HasAttachment { .. }
            | FilterProp::HasAnyAttachmentOf { .. } => Supported,

            // ---- embedded combat role; candidate membership never re-read ----
            FilterProp::Attacking { .. }
            | FilterProp::Blocking
            | FilterProp::Unblocked
            | FilterProp::AttackingAlone
            | FilterProp::BlockingAlone
            | FilterProp::CombatRelation { .. } => Supported,

            // ---- embedded exact source relations ----
            FilterProp::SaddledSource | FilterProp::ConvokedSource => Supported,

            // ---- embedded per-turn history ----
            FilterProp::WasDealtDamageThisTurn
            | FilterProp::DealtDamageThisTurn
            | FilterProp::EnteredThisTurn
            | FilterProp::AttackedThisTurn { .. }
            | FilterProp::BlockedThisTurn
            | FilterProp::AttackedOrBlockedThisTurn
            | FilterProp::ZoneChangedThisTurn { .. }
            | FilterProp::CountersPutOnThisTurn { .. } => Supported,

            // ---- embedded tracked-set membership ----
            FilterProp::InTrackedSet { .. } => Supported,

            // ---- candidate name embedded; reference half is external context ----
            FilterProp::Named { .. }
            | FilterProp::SameName
            | FilterProp::SameNameAsParentTarget
            | FilterProp::DifferentNameFrom { .. }
            | FilterProp::NameMatchesAnyPermanent { .. }
            | FilterProp::SharesQuality { .. }
            | FilterProp::IsChosenCreatureType
            | FilterProp::IsChosenCardType => Supported,

            // ---- composites ----
            FilterProp::AnyOf { props } => {
                EventObjectFilterSupport::combine_disjunction(props.iter().map(Self::classify_prop))
            }
            FilterProp::Not { prop } => Self::classify_prop(prop).negate(),

            // ---- permanent-domain false: stack-only predicates ----
            // CR 701.50a: the subject is a permanent, not a spell/ability on the stack, so
            // these are decided `false` rather than fabricated from a fake target list.
            FilterProp::HasSingleTarget
            | FilterProp::Targets { .. }
            | FilterProp::TargetsOnly { .. }
            | FilterProp::Modal => PermanentDomainFalse,

            // ---- unsupported: needs a live candidate lookup or an unmodeled field ----
            // Not reachable from the subject grammar today. Reaching one fails the gate,
            // which is the designed signal to extend the snapshot + evaluator together.
            FilterProp::WasPlayed
            // CR 108.2 + CR 108.2b: event snapshots retain token status but not whether
            // a nontoken object is a copy, so card representation cannot be reconstructed.
            | FilterProp::RepresentedByCard
            | FilterProp::ControllerChoseLabel { .. }
            | FilterProp::ControllerMatches { .. }
            | FilterProp::BlockingSource
            | FilterProp::HasHasteOrControlledSinceTurnBegan
            | FilterProp::ControlledContinuouslySinceTurnBegan
            | FilterProp::CanEnchant { .. }
            | FilterProp::ManaCostIn { .. }
            | FilterProp::ManaSymbolCount { .. }
            | FilterProp::Foretold
            | FilterProp::AttachedToSource
            | FilterProp::AttachedToRecipient
            | FilterProp::Unpaired
            | FilterProp::OtherThanTriggerObject
            | FilterProp::MostPrevalentCreatureTypeIn { .. }
            | FilterProp::IsChosenColor
            | FilterProp::MatchesLastChosenCardPredicate
            | FilterProp::DistinctFrom { .. }
            | FilterProp::CouldBeTargetedByTriggeringSpell
            | FilterProp::HasXInManaCost
            | FilterProp::HasXInActivationCost
            | FilterProp::WasKicked
            | FilterProp::HasManaAbility
            // CR 903.3: needs the live deck-pool commander registry (owner-scoped),
            // not the subject snapshot. Parsed only inside mana-spend SPELL filters
            // (CR 106.6 / CR 603.7a), which are evaluated live at the casting site —
            // never from the event-subject grammar.
            | FilterProp::SharesCreatureTypeWithCommander
            | FilterProp::Other { .. } => Unsupported,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data")]
pub enum GameEvent {
    GameStarted,
    TurnStarted {
        player_id: PlayerId,
        turn_number: u32,
    },
    PhaseChanged {
        phase: Phase,
    },
    PriorityPassed {
        player_id: PlayerId,
    },
    SpellCast {
        card_id: CardId,
        controller: PlayerId,
        object_id: ObjectId, // CR 601.2a: The spell object on the stack
    },
    /// CR 702.140c + CR 730.2: A mutating creature spell merged with a target
    /// creature, forming a mutated permanent. Emitted by
    /// `merge::merge_object_onto`. `merged_id` is the surviving permanent's
    /// `ObjectId` (the target creature's, kept per CR 730.2c); `merging_id` is the
    /// component card/token that merged onto it; `controller` is the merging
    /// spell's controller. "Whenever this creature mutates" triggers (CR 702.140d)
    /// listen here — downstream condition handling is deferred (no Phase-1 card
    /// needs it), but the event is observable now.
    Mutated {
        merged_id: ObjectId,
        merging_id: ObjectId,
        controller: PlayerId,
    },
    /// Unstable Host/Augment: a card with augment combined with a Host
    /// creature, forming a merged permanent. Emitted by `augment.rs`.
    /// `merged_id` is the surviving permanent's `ObjectId` (the Host
    /// creature's continuity id); `augmenting_id` is the augment component that
    /// merged onto it; `controller` is the player who performed the combine.
    ///
    /// Distinct from `Mutated`: Augment reuses merge-like bookkeeping but is a
    /// separate mechanic and must not satisfy `TriggerMode::Mutates`.
    Augmented {
        merged_id: ObjectId,
        augmenting_id: ObjectId,
        controller: PlayerId,
    },
    /// CR 707.10: A spell was copied onto the stack. A copy of a spell isn't
    /// cast, so this is a distinct event from `SpellCast` — copy-sensitive
    /// triggers (Magecraft, "whenever you copy a spell") fire on this, while
    /// cast-only triggers (Prowess, storm, cascade) do not.
    SpellCopied {
        card_id: CardId,
        controller: PlayerId,
        object_id: ObjectId,   // the copy's stack object id
        original_id: ObjectId, // CR 707.10: the spell that was copied
    },
    /// CR 107.1b + CR 601.2f: The caster has chosen the value of X for a
    /// pending cast whose cost contained `ManaCostShard::X`.
    XValueChosen {
        player: PlayerId,
        object_id: ObjectId,
        value: u32,
    },
    /// CR 602.1 + CR 605.3b: An activated ability has been activated and put on
    /// the stack. **Not emitted for mana abilities** (CR 605.3b: mana abilities
    /// resolve immediately without using the stack and follow a separate code
    /// path that never reaches this event). This invariant — `AbilityActivated`
    /// fires only for non-mana activations — is what makes
    /// `TriggerCondition::ActivatedAbilityIsNonMana` trivially satisfied when
    /// matched against this event, and is what lets the generic
    /// "Whenever a player activates an ability that isn't a mana ability"
    /// trigger class (Burning-Tree Shaman, Flamescroll Celebrant) listen here.
    AbilityActivated {
        /// CR 602.2a: "Its controller is the player who activated the ability."
        /// Required so `extract_player_from_event` can resolve "that player" /
        /// `TargetFilter::TriggeringPlayer` references in the resolving
        /// ability's effect (Burning-Tree Shaman, Flamescroll Celebrant).
        player_id: PlayerId,
        source_id: ObjectId,
        /// CR 606.2: Distinguishes loyalty-ability activations (planeswalker
        /// abilities paid with loyalty counters) from ordinary activated
        /// abilities so the "Whenever you activate a loyalty ability" trigger
        /// class can match without a separate event. `#[serde(default)]` keeps
        /// older serialized `AbilityActivated` events (which predate this field)
        /// deserializing as `Normal`.
        #[serde(default)]
        kind: ActivatedAbilityKind,
    },
    /// CR 603.6a: Enters-the-battlefield and zone-change triggers fire on this
    /// event. `from` is `None` when an object is created directly in a zone
    /// without a prior zone — e.g., a token is created on the battlefield
    /// (CR 111.1 + CR 603.6a: "an object that enters the battlefield as a
    /// token is created in the battlefield zone"). Treating token creation
    /// as a `ZoneChanged` event means every ETB trigger matcher (Elvish
    /// Vanguard, Soul Warden, Panharmonicon) automatically fires for tokens
    /// without bespoke per-matcher code paths.
    ZoneChanged {
        object_id: ObjectId,
        from: Option<Zone>,
        to: Zone,
        /// CR 603.10: Boxed to keep `GameEvent` variant size small. The record
        /// can be ~200 bytes and is only populated for this one variant; every
        /// other consumer (and every other event) would pay that cost inline.
        record: Box<ZoneChangeRecord>,
    },
    LifeChanged {
        player_id: PlayerId,
        amount: i32,
    },
    ManaAdded {
        player_id: PlayerId,
        mana_type: ManaType,
        source_id: ObjectId,
        /// Whether this mana came from tapping a source, and whether the
        /// coupled `TapsForMana` triggered mana abilities (CR 605.1a + CR 605.1b)
        /// have already been resolved. Consumed by the `TapsForMana` trigger
        /// matcher and by the post-action trigger scan's double-resolution guard.
        #[serde(default, skip_serializing_if = "ManaTapState::is_not_from_tap")]
        tap_state: ManaTapState,
    },
    /// CR 106.12a: A mana ability whose activation cost includes the `{T}`
    /// symbol (CR 106.12) resolved and produced mana. Emitted **exactly once
    /// per resolution** — unlike `ManaAdded`, which is per mana unit (CR 106.4)
    /// pool accounting. The `TapsForMana` trigger matcher keys off this event
    /// so triggers like Vorinclex fire once per tap, not once per mana point.
    TappedForMana {
        player_id: PlayerId,
        source_id: ObjectId,
        /// The full set of mana units produced by this resolution. Consumed by
        /// `TriggerEventManaType` (one trigger-event mana per distinct color).
        produced: Vec<ManaType>,
        /// CR 605.4a: Tracks whether the coupled `TapsForMana` triggered mana
        /// abilities have already resolved — the post-action double-resolution
        /// guard and the inline resolver's Pass-2 flip key off this.
        #[serde(default, skip_serializing_if = "ManaTapState::is_not_from_tap")]
        tap_state: ManaTapState,
    },
    /// CR 500.5 + CR 703.4q: A single mana unit was emptied from a player's
    /// pool during the step-end empty event after the CR 616.1 replacement
    /// pipeline resolved. `source_id` is the unit's original producer
    /// (mirrors `ManaAdded::source_id`).
    ManaPoolEmptied {
        player_id: PlayerId,
        source_id: ObjectId,
        color: ManaType,
    },
    /// CR 614.1a + CR 703.4q: A `Transform(_)` step-end mana handler (Horizon
    /// Stone, Kruphix, Omnath, Ozai) recolored a unit in place during the
    /// step-end empty event. The unit stays in the pool with its new color.
    ManaRecolored {
        player_id: PlayerId,
        from: ManaType,
        to: ManaType,
    },
    PermanentTapped {
        object_id: ObjectId,
        /// The source that caused the tap, if tapped by an external effect.
        /// `None` for self-initiated taps (mana abilities, attacking, crew, costs).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        caused_by: Option<ObjectId>,
    },
    /// CR 701.43a + CR 701.43d: A creature was exerted as it attacked. Fires the
    /// linked `TriggerMode::Exerted` "when you do" trigger (Combat Celebrant,
    /// Glory-Bound Initiate, ...).
    CreatureExerted {
        object_id: ObjectId,
    },
    /// CR 702.154c: A creature enlisted another creature as it attacked. Fires
    /// the linked `TriggerMode::Enlisted` "when you do" trigger and carries the
    /// tapped creature's LKI snapshot for CR 608.2h resolution.
    CreatureEnlisted {
        attacker: ObjectId,
        tapped: ObjectId,
        tapped_snapshot: Box<CostPaidObjectSnapshot>,
    },
    /// CR 701.47c: An amass instruction chose an Army creature. This event is
    /// observational; the resolving ability carries the authoritative
    /// `amassed_army_object` snapshot for later CR 701.47c references.
    ArmyAmassed {
        object_id: ObjectId,
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// CR 702.143a: A player foretold a card from their hand.
    Foretold {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    /// CR 702.143d: a card in exile became foretold via an effect (e.g. The
    /// Foretold Soldier "exile it face down. It becomes foretold."). Distinct
    /// from the CR 702.143a foretell special action — it does NOT fire
    /// "whenever you foretell" triggers (CR 702.143c reserves "foretell" for
    /// the special action).
    BecameForetold {
        object_id: ObjectId,
    },
    PlayerLost {
        player_id: PlayerId,
    },
    MulliganStarted,
    CardsDrawn {
        player_id: PlayerId,
        count: u32,
    },
    CardDrawn {
        player_id: PlayerId,
        object_id: ObjectId,
        /// Ordinal of this draw within the current turn (1-indexed). Set by
        /// the emitter after incrementing `player.cards_drawn_this_turn`, so
        /// Nth-card draw triggers evaluate against the individual draw event
        /// rather than the final post-batch turn total.
        #[serde(default = "default_nth_in_turn")]
        nth_in_turn: u32,
        /// CR 121.1 + CR 504.1: Ordinal of this draw within the current step
        /// (1-indexed). Set by the emitter to `player.cards_drawn_this_step`
        /// AFTER incrementing for this draw, so the first card drawn in a step
        /// has `nth_in_step == 1`. Used by `TriggerCondition::ExceptFirstDrawInDrawStep`
        /// to suppress the trigger on the draw step's mandatory first draw.
        #[serde(default = "default_nth_in_step")]
        nth_in_step: u32,
    },
    PermanentUntapped {
        object_id: ObjectId,
    },
    /// CR 702.26b: A permanent phased out (status changed to phased out).
    /// `indirect` is true iff this permanent was phased out because a host
    /// it was attached to phased out (CR 702.26g).
    PermanentPhasedOut {
        object_id: ObjectId,
        #[serde(default)]
        indirect: bool,
    },
    /// CR 702.26c: A permanent phased in (status changed to phased in).
    PermanentPhasedIn {
        object_id: ObjectId,
    },
    /// A player phased out. Player phasing is not formally governed by CR 702.26
    /// (which is permanent-only); semantics mirror the permanent rule and are
    /// driven by the small set of card Oracle text that says "you phase out".
    /// While phased out, the player is excluded from targeting, attacking,
    /// damage, and the 0-or-less life SBA.
    PlayerPhasedOut {
        player_id: PlayerId,
    },
    /// A player phased back in (typically at the start of their next turn or
    /// when an `UntilYourNextTurn` duration ends).
    PlayerPhasedIn {
        player_id: PlayerId,
    },
    LandPlayed {
        object_id: ObjectId,
        player_id: PlayerId,
        from_zone: Zone,
    },
    StackPushed {
        object_id: ObjectId,
    },
    StackResolved {
        object_id: ObjectId,
    },
    Discarded {
        player_id: PlayerId,
        object_id: ObjectId,
        /// CR 603.2 + CR 109.5: The spell/ability that caused this discard, if any
        /// (effect- or cost-driven discards). `None` for a player's own
        /// turn-based / hand-size discards. Carried from `ProposedEvent::Discard`
        /// so triggers like "when a spell or ability an opponent controls causes
        /// you to discard this card" can resolve the cause's controller.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        source_id: Option<ObjectId>,
    },
    DamageCleared {
        object_id: ObjectId,
    },
    GameOver {
        winner: Option<PlayerId>,
    },
    /// CR 732.2: A mandatory auto-resolution sequence hit the engine's resource
    /// ceiling without settling — a net-progress loop the engine cannot
    /// shortcut (CR 732.2 resolves these by a player-declared iteration count
    /// the engine can't infer). Resolution is paused and priority returned to
    /// the active player. NOT a draw: distinct from CR 104.4b, which requires a
    /// *repeating* state (a true loop is detected separately and ends the game).
    /// `involved` carries the in-flight cascade's distinct stack-source ids for
    /// diagnostics only — never read by game logic.
    ResolutionHalted {
        involved: Vec<ObjectId>,
    },
    DamageDealt {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
        is_combat: bool,
        /// CR 120.10: Excess damage beyond lethal for creatures/planeswalkers/battles.
        #[serde(default)]
        excess: u32,
    },
    /// CR 615: Damage was prevented (by a prevention shield or protection).
    /// Enables "when damage is prevented" triggers.
    DamagePrevented {
        source_id: ObjectId,
        target: TargetRef,
        amount: u32,
    },
    SpellCountered {
        object_id: ObjectId,
        countered_by: ObjectId,
        /// CR 109.5: "you control" on counter triggers refers to the countering
        /// spell or ability's controller, not necessarily the source object's
        /// current controller.
        countered_by_controller: PlayerId,
    },
    CounterAdded {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
    },
    /// Digital-only Alchemy (no CR entry): a card's intensity increased by
    /// `amount`. Emitted per affected card so consumers (triggers that watch for
    /// intensifying, frontend animation) can see exactly which cards changed.
    ObjectIntensified {
        object_id: ObjectId,
        amount: u32,
    },
    /// CR 702.100b: A creature evolved because one or more +1/+1 counters were
    /// put on it as a result of its evolve ability resolving.
    Evolved {
        object_id: ObjectId,
    },
    CounterRemoved {
        object_id: ObjectId,
        counter_type: CounterType,
        count: u32,
    },
    TokenCreated {
        object_id: ObjectId,
        name: String,
        /// CR 111.1: the object id of the ability/spell that created this token
        /// (the creating effect's `source_id`). Lets consumers attribute a token
        /// to its creator — e.g. "destroy all OTHER creatures" sparing only the
        /// tokens the resolving spell itself made, distinct from tokens a
        /// replacement effect produced during the same resolution.
        source_id: ObjectId,
    },
    /// Digital-only: A card was conjured from outside the game into a zone.
    ObjectConjured {
        object_id: ObjectId,
        name: String,
    },
    CreatureDestroyed {
        object_id: ObjectId,
    },
    PermanentSacrificed {
        object_id: ObjectId,
        player_id: PlayerId,
    },
    /// CR 613.1b: A continuous effect changed an object's controller in layer 2.
    ControllerChanged {
        object_id: ObjectId,
        old_controller: PlayerId,
        new_controller: PlayerId,
    },
    EffectResolved {
        kind: EffectKind,
        /// The raw storage id of the effect's source. Retained as the compatibility and
        /// trigger-index key, but it is NOT the candidate authority for an event whose
        /// subject can leave or be replaced — see `subject`.
        source_id: ObjectId,
        /// CR 400.7 + CR 608.2b: the identity-exact projection of the object this effect
        /// happened *to*, frozen at the instruction that caused it.
        ///
        /// `None` for ordinary completions, whose observers do not interrogate a subject.
        /// `Some` for Connive (CR 701.50a), where the conniver may leave the battlefield —
        /// or return as a new incarnation at the same `source_id` — before the completion
        /// event's triggers are matched. Boxed to keep `GameEvent`'s size down.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        subject: Option<Box<EventObjectSnapshot>>,
    },
    /// CR 701.3d: An Aura, Equipment, or Fortification became unattached from
    /// the object or player it was attached to.
    Unattached {
        attachment_id: ObjectId,
        old_target: TargetRef,
    },
    AttackersDeclared {
        attacker_ids: Vec<ObjectId>,
        defending_player: PlayerId,
        /// Per-attacker targets — parallel to attacker_ids, same length and order.
        #[serde(default)]
        attacks: Vec<(ObjectId, crate::game::combat::AttackTarget)>,
    },
    BlockersDeclared {
        assignments: Vec<(ObjectId, ObjectId)>,
    },
    /// CR 509.3c: An effect made an attacking creature become blocked, and it was
    /// an unblocked creature at that time — the precondition for "becomes blocked"
    /// triggers to fire from an effect-block.
    /// CR 509.3d: A "becomes blocked BY A CREATURE" trigger, and any blocker-side
    /// "whenever ~ blocks" trigger, must NOT fire from an effect-block — this event
    /// is distinct from BlockersDeclared precisely so those matchers ignore it.
    AttackerBecameBlockedByEffect {
        attacker: ObjectId,
    },
    /// CR 509.3d: A per-blocker `Blocks`/`BecomesBlocked`/`BlocksOrBecomesBlocked`
    /// firing with an explicit blocker/attacker qualifier — carries both ids so
    /// "that creature"/"the other creature" resolution never has to infer
    /// orientation from event shape.
    AttackerBecameBlockedByFilteredBlocker {
        attacker: ObjectId,
        blocker: ObjectId,
    },
    /// CR 508.1h + CR 509.1d: The aggregate combat tax was paid; the declaration
    /// proceeds with every declared creature intact.
    CombatTaxPaid {
        player: PlayerId,
        total_mana_value: u32,
    },
    /// CR 508.1d + CR 509.1c: The combat tax was declined; the listed taxed
    /// creatures have been dropped from the declaration before it completes.
    CombatTaxDeclined {
        player: PlayerId,
        dropped: Vec<ObjectId>,
    },
    BecomesTarget {
        target: TargetRef,
        source_id: ObjectId,
    },
    /// CR 702.122e: A Vehicle's crew ability resolved.
    /// Carries creature list for trigger conditions that reference "creatures that crewed it".
    VehicleCrewed {
        vehicle_id: ObjectId,
        creatures: Vec<ObjectId>,
    },
    /// CR 702.184a: A Spacecraft's station ability resolved.
    /// Fires the `TriggerMode::Stationed` event for triggers on the Spacecraft
    /// that care about the act of being stationed. Carries the tapped creature
    /// and the number of counters added so downstream consumers (logs, future
    /// "whenever ~ is stationed by a creature with X" triggers) can see the
    /// inputs without re-deriving them.
    Stationed {
        spacecraft_id: ObjectId,
        creature_id: ObjectId,
        counters_added: u32,
    },
    /// CR 702.171a: A Mount's saddle ability resolved.
    /// Fires the `TriggerMode::Saddled` / `TriggerMode::BecomesSaddled` events
    /// for triggers that care about the act of being saddled. Carries the
    /// tapped creatures so trigger conditions referencing "creatures that
    /// saddled it" can resolve against last-known information.
    Saddled {
        mount_id: ObjectId,
        creatures: Vec<ObjectId>,
    },
    ReplacementApplied {
        source_id: ObjectId,
        event_type: String,
    },
    Transformed {
        object_id: ObjectId,
    },
    /// Digital-only Specialize: a permanent became a color-specific specialized face.
    Specialized {
        object_id: ObjectId,
        color: crate::types::mana::ManaColor,
    },
    DayNightChanged {
        new_state: String,
    },
    TurnedFaceUp {
        object_id: ObjectId,
    },
    /// CR 701.27b: A face-up permanent was turned face down by a resolving effect
    /// (Cyber Conversion). Distinct from `Transformed` — turning face down and
    /// transforming are different game actions, so a "whenever a permanent is
    /// turned face down" trigger must observe THIS event, not `Transformed`.
    /// Drives the game log and the public-state/frontend re-render.
    TurnedFaceDown {
        object_id: ObjectId,
    },
    CardsRevealed {
        player: PlayerId,
        #[serde(default)]
        card_ids: Vec<ObjectId>,
        card_names: Vec<String>,
    },
    CombatDamageDealtToPlayer {
        player_id: PlayerId,
        /// CR 120.1 + CR 510.2: Per-source combat damage amounts for this
        /// specific combat damage step. Using step-local amounts instead of a
        /// bare `Vec<ObjectId>` prevents double-strike / extra-combat inflation
        /// in `matching_damage_done_once_by_controller_event`: each
        /// `apply_combat_damage` call produces exactly one event per player with
        /// the amounts from that step only.
        ///
        /// Migration note: this field replaces the former `source_ids:
        /// Vec<ObjectId>`. `#[serde(default)]` keeps deserialization of older
        /// persisted state infallible, but an old-format event (a game persisted
        /// mid-combat-damage-trigger by a pre-rename binary and restored after an
        /// upgrade) decodes to an empty set — the legacy `source_ids` array is
        /// dropped. This is acceptable: the event is transient (produced and
        /// consumed within one combat-damage step), the window is the rare
        /// mid-trigger save across a server upgrade, and it degrades to "no
        /// matching sources" rather than crashing. The old format carried no
        /// amounts, so no migration shim could recover `total_damage` regardless.
        #[serde(default)]
        source_amounts: Vec<(ObjectId, u32)>,
        /// CR 120.1: Total actual damage dealt to this player in this combat
        /// damage step — the sum of all `source_amounts` entries.
        #[serde(default)]
        total_damage: u32,
    },
    PlayerEliminated {
        player_id: PlayerId,
    },
    CrimeCommitted {
        player_id: PlayerId,
    },
    Cycled {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    PlayerPerformedAction {
        player_id: PlayerId,
        action: PlayerActionKind,
    },
    /// Engine-authored diagnostic for top-card predicate
    /// guesses. This is intentionally a log/debug event rather than rules input:
    /// `ChooseOption` remains the authoritative action, while this records
    /// which predicate AI or a human guessed.
    CardPredicateGuessMade {
        player_id: PlayerId,
        source_id: Option<ObjectId>,
        choice: String,
    },
    /// CR 701.19a: Regeneration shield — consumed on use, expires at cleanup.
    Regenerated {
        object_id: ObjectId,
    },
    /// CR 701.60a: A creature was suspected.
    CreatureSuspected {
        object_id: ObjectId,
    },
    /// CR 701.60a: A creature is no longer suspected — the un-designation
    /// transition. Emitted only when the toggle actually flips (idempotent
    /// resolver). Mirrors `BecameUnprepared`.
    CreatureNoLongerSuspected {
        object_id: ObjectId,
    },
    /// CR 701.35a: A permanent was detained — until the detaining player's next
    /// turn it can't attack or block and its activated abilities can't be
    /// activated. Display-relevant for mana sources: detaining a mana dork
    /// makes its mana ability un-activatable.
    Detained {
        object_id: ObjectId,
    },
    /// CR 702.xxx: Prepare (Strixhaven) — a creature became prepared.
    /// Emitted only when the toggle actually flips (idempotent resolvers).
    /// Assign when WotC publishes SOS CR update.
    BecamePrepared {
        object_id: ObjectId,
    },
    /// CR 702.xxx: Prepare (Strixhaven) — a creature became unprepared.
    /// Emitted only when the toggle actually flips (idempotent resolvers).
    /// Assign when WotC publishes SOS CR update.
    BecameUnprepared {
        object_id: ObjectId,
    },
    /// CR 719.3b: A Case enchantment became solved.
    CaseSolved {
        object_id: ObjectId,
    },
    /// CR 716.2a: A Class enchantment gained a new level.
    ClassLevelGained {
        object_id: ObjectId,
        level: u8,
    },
    /// CR 725: A player became the monarch.
    MonarchChanged {
        player_id: PlayerId,
    },
    /// CR 702.131b: A player gained the city's blessing (Ascend).
    CityBlessingGained {
        player_id: PlayerId,
    },
    /// CR 706: A die was rolled. `result` is `None` when the roll has no numeric
    /// face value — the symbolic planar die (CR 901.9d / CR 706.7): the
    /// `RolledDie` trigger still fires, but numeric-result consumers ignore it.
    DieRolled {
        player_id: PlayerId,
        sides: u8,
        result: Option<u8>,
    },
    /// CR 103.1 / CR 706: The game-1 starting-player roll-off, emitted as one
    /// authoritative structured event so the contest can be rendered round by
    /// round (including tie rerolls) with no downstream re-derivation. `rounds`
    /// preserves the round boundaries the engine computes; `winner` is the
    /// engine's authoritative starting player (unique max of the final round, or
    /// the lowest-seat fallback when tied at the reroll cap). Replaces the prior
    /// flat per-roll `DieRolled` batch on the starting-player contest path; in-game
    /// die rolls still emit `DieRolled`.
    StartingPlayerContest {
        rounds: Vec<ContestRound>,
        winner: PlayerId,
    },
    /// CR 705: A coin was flipped.
    CoinFlipped {
        player_id: PlayerId,
        won: bool,
    },
    /// CR 701.54: The Ring tempted a player.
    RingTemptsYou {
        player_id: PlayerId,
    },
    /// CR 309.4c: A player moved their venture marker into a dungeon room.
    RoomEntered {
        player_id: PlayerId,
        dungeon: crate::game::dungeon::DungeonId,
        room_index: u8,
        room_name: String,
    },
    /// CR 709.5h-i: A Room permanent was given an unlocked designation.
    RoomDoorUnlocked {
        player_id: PlayerId,
        object_id: ObjectId,
        door: crate::game::game_object::RoomDoor,
        fully_unlocked: bool,
    },
    /// CR 702.170c-d: A card in exile became plotted for the specified player.
    BecomesPlotted {
        object_id: ObjectId,
        player_id: PlayerId,
    },
    /// CR 309.7: A player completed a dungeon (removed from game).
    DungeonCompleted {
        player_id: PlayerId,
        dungeon: crate::game::dungeon::DungeonId,
    },
    /// CR 701.31 / CR 901.11: The planar controller planeswalked — the active
    /// plane/phenomenon (`from`) is put on the bottom of the planar deck face
    /// down and the new top card (`to`) is turned face up.
    Planeswalked {
        player_id: PlayerId,
        from: Option<ObjectId>,
        to: Option<ObjectId>,
    },
    /// CR 311.7 / CR 901.9b: Chaos ensued — the active plane's chaos-triggered
    /// ability triggers.
    ChaosEnsued {
        plane_id: ObjectId,
    },
    /// CR 901.9: The planar die was rolled, landing on the given face.
    PlanarDieRolled {
        player_id: PlayerId,
        face: crate::game::planechase::PlanarDieFace,
    },
    /// CR 904.9 / CR 701.32b: A scheme was set in motion (turned face up in the
    /// command zone). Fires "When you set this scheme in motion" (SetInMotion).
    SchemeSetInMotion {
        player_id: PlayerId,
        scheme_id: ObjectId,
    },
    /// CR 701.33b / CR 904.10: A scheme was abandoned (turned face down and put
    /// on the bottom of its owner's scheme deck).
    SchemeAbandoned {
        player_id: PlayerId,
        scheme_id: ObjectId,
    },
    /// CR 726.2: A player took the initiative.
    InitiativeTaken {
        player_id: PlayerId,
    },
    /// CR 701.51c: An Attraction was opened onto the battlefield.
    AttractionOpened {
        player_id: PlayerId,
        object_id: ObjectId,
    },
    /// Unstable Contraptions: a Contraption was assembled from a player's
    /// supplementary Contraption deck onto a sprocket.
    ContraptionAssembled {
        player_id: PlayerId,
        object_id: ObjectId,
        sprocket: u8,
    },
    StickerPlaced {
        player_id: PlayerId,
        object_id: ObjectId,
        kind: StickerKind,
    },
    /// CR 701.52: The active player rolled to visit their Attractions.
    AttractionsRolledToVisit {
        player_id: PlayerId,
        roll: u8,
    },
    /// CR 701.52a + CR 702.159a: A specific Attraction was visited this roll.
    AttractionVisited {
        player_id: PlayerId,
        roll: u8,
        attraction_id: ObjectId,
    },
    /// Unstable Contraptions: a specific Contraption on a sprocket was
    /// cranked. `TriggerMode::CrankContraption` listens to this event.
    ContraptionCranked {
        player_id: PlayerId,
        sprocket: u8,
        contraption_id: ObjectId,
    },
    /// Avatar crossover: A firebending ability resolved and produced mana.
    Firebend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// Avatar crossover: A permanent or spell was airbent (exiled with alt-cast permission).
    Airbend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// Avatar crossover: A land was earthbent (animated with counters + return trigger).
    Earthbend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// Avatar crossover: A waterbend cost was paid (tap-to-pay for generic mana).
    Waterbend {
        source_id: ObjectId,
        controller: PlayerId,
    },
    /// CR 702.139a: Companion revealed at game start.
    CompanionRevealed {
        player: PlayerId,
        card_name: String,
    },
    /// CR 702.139a: Companion moved to hand via {3} special action.
    CompanionMovedToHand {
        player: PlayerId,
        card_name: String,
    },
    /// CR 702.49a: A ninjutsu-family ability was activated (ninjutsu, commander ninjutsu, sneak).
    /// This is a special action, not an activated ability on the stack, so it does not fire
    /// AbilityActivated. Enables "whenever you activate a ninjutsu ability" triggers.
    NinjutsuActivated {
        player_id: PlayerId,
        source_id: ObjectId,
    },

    /// CR 702.107a + CR 702.142b + CR 702.177a: A keyword ability was activated.
    /// Emitted alongside `AbilityActivated` when the activated ability has a recognized
    /// `ability_tag`. `is_mana_ability` is `true` only for exhaust mana abilities; it is
    /// always `false` for boast and outlast activations. Parameterized to avoid per-keyword
    /// variant proliferation (boast, exhaust, outlast share identical event structure).
    KeywordAbilityActivated {
        ability_tag: AbilityTag,
        player_id: PlayerId,
        source_id: ObjectId,
        is_mana_ability: bool,
    },

    /// CR 702.110: A creature exploited another creature (sacrificed via exploit ETB).
    CreatureExploited {
        exploiter: ObjectId,
        sacrificed: ObjectId,
    },
    /// CR 122.1: A player's energy counter total changed.
    EnergyChanged {
        player: PlayerId,
        delta: i32,
    },
    /// CR 702.179: A player's speed changed.
    SpeedChanged {
        player: PlayerId,
        old_speed: Option<u8>,
        new_speed: Option<u8>,
    },
    /// CR 122.1: A player counter (poison, experience, rad, ticket, etc.) changed.
    PlayerCounterChanged {
        player: PlayerId,
        counter_kind: PlayerCounterKind,
        delta: i32,
    },
    /// CR 700.14: Mana was spent on a spell cast, updating the cumulative total this turn.
    ManaExpended {
        player_id: PlayerId,
        amount_spent: u32,
        new_cumulative: u32,
    },
    /// CR 701.30: A clash occurred between two players.
    Clash {
        controller: PlayerId,
        opponent: PlayerId,
        controller_mana_value: Option<u32>,
        opponent_mana_value: Option<u32>,
        result: ClashResult,
    },
    /// CR 701.38a: A player cast a single vote in a Council's-dilemma
    /// resolution. One event per vote (so a player with multiple votes
    /// produces multiple events). `choice` is the lowercase canonical
    /// option name from `Effect::Vote.choices`.
    VoteCast {
        voter: PlayerId,
        choice: String,
        source_id: ObjectId,
    },
    /// CR 701.38: All voters have voted. Emitted before the per-choice tally
    /// sub-effects fire. `tallies` is `(choice, count)` pairs in `options`
    /// declaration order.
    VoteResolved {
        source_id: ObjectId,
        tallies: Vec<(String, u32)>,
    },
    /// Emitted when layer re-evaluation changes a creature's effective power/toughness.
    /// Generic event — not tied to any specific card or effect.
    PowerToughnessChanged {
        object_id: ObjectId,
        power: i32,
        toughness: i32,
        power_delta: i32,
        toughness_delta: i32,
    },
    /// CR 702.85a: Cascade exiled the entire library (or whatever remained
    /// after replacement effects) without finding a nonland card with
    /// `mana_value < source_mv`. Emitted before the bottom-shuffle so the
    /// log/UI can announce the miss without inferring it from absence.
    CascadeMissed {
        controller: PlayerId,
        source_id: ObjectId,
        exiled_count: u32,
    },
    /// Sandbox audit log: a player with debug permission submitted a
    /// `GameAction::Debug(_)`. `description` is the engine-authored summary
    /// from `DebugAction::describe`; the FE renders it verbatim.
    DebugActionUsed {
        player_id: PlayerId,
        description: String,
    },
    /// Sandbox audit log: the host granted a player permission to submit
    /// `GameAction::Debug(_)`.
    DebugPermissionGranted {
        host: PlayerId,
        player_id: PlayerId,
    },
    /// Sandbox audit log: the host revoked a player's debug permission.
    DebugPermissionRevoked {
        host: PlayerId,
        player_id: PlayerId,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn game_started_serializes_as_tagged_union() {
        let event = GameEvent::GameStarted;
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "GameStarted");
    }

    #[test]
    fn turn_started_serializes_with_data() {
        let event = GameEvent::TurnStarted {
            player_id: PlayerId(0),
            turn_number: 1,
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "TurnStarted");
        assert_eq!(json["data"]["turn_number"], 1);
    }

    #[test]
    fn ability_activated_kind_defaults_to_normal_for_legacy_state() {
        // CR 606.2: an older serialized `AbilityActivated` event predates the
        // `kind` field. `#[serde(default)]` must deserialize it as `Normal`,
        // never failing or silently treating it as `Loyalty`.
        let legacy = serde_json::json!({
            "type": "AbilityActivated",
            "data": { "player_id": 0, "source_id": 7 }
        });
        let event: GameEvent = serde_json::from_value(legacy).unwrap();
        match event {
            GameEvent::AbilityActivated { kind, .. } => {
                assert_eq!(kind, ActivatedAbilityKind::Normal);
            }
            other => panic!("expected AbilityActivated, got {other:?}"),
        }
    }

    #[test]
    fn ability_activated_kind_round_trips() {
        // CR 606.2: the discriminator survives serialization.
        for kind in [ActivatedAbilityKind::Normal, ActivatedAbilityKind::Loyalty] {
            let event = GameEvent::AbilityActivated {
                player_id: PlayerId(1),
                source_id: ObjectId(9),
                kind,
            };
            let json = serde_json::to_value(&event).unwrap();
            let back: GameEvent = serde_json::from_value(json).unwrap();
            match back {
                GameEvent::AbilityActivated { kind: k, .. } => assert_eq!(k, kind),
                other => panic!("expected AbilityActivated, got {other:?}"),
            }
        }
    }

    #[test]
    fn zone_changed_serializes_all_fields() {
        let event = GameEvent::ZoneChanged {
            object_id: ObjectId(5),
            from: Some(Zone::Hand),
            to: Zone::Battlefield,
            record: Box::new(ZoneChangeRecord {
                name: "Test".to_string(),
                ..ZoneChangeRecord::test_minimal(ObjectId(5), Some(Zone::Hand), Zone::Battlefield)
            }),
        };
        let json = serde_json::to_value(&event).unwrap();
        assert_eq!(json["type"], "ZoneChanged");
        assert_eq!(json["data"]["from"], "Hand");
        assert_eq!(json["data"]["to"], "Battlefield");
        assert_eq!(json["data"]["record"]["name"], "Test");
    }

    #[test]
    fn game_over_with_winner_roundtrips() {
        let event = GameEvent::GameOver {
            winner: Some(PlayerId(1)),
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn game_over_without_winner_roundtrips() {
        let event = GameEvent::GameOver { winner: None };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn damage_dealt_event_roundtrips() {
        use crate::types::ability::TargetRef;
        let event = GameEvent::DamageDealt {
            source_id: ObjectId(1),
            target: TargetRef::Player(PlayerId(0)),
            amount: 3,
            is_combat: false,
            excess: 0,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn effect_resolved_event_roundtrips() {
        let event = GameEvent::EffectResolved {
            kind: EffectKind::DealDamage,
            source_id: ObjectId(5),
            subject: None,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn combat_damage_dealt_to_player_roundtrips() {
        let event = GameEvent::CombatDamageDealtToPlayer {
            player_id: PlayerId(1),
            source_amounts: vec![(ObjectId(10), 3), (ObjectId(11), 4)],
            total_damage: 7,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    #[test]
    fn power_toughness_changed_roundtrips() {
        let event = GameEvent::PowerToughnessChanged {
            object_id: ObjectId(7),
            power: 5,
            toughness: 6,
            power_delta: 2,
            toughness_delta: 2,
        };
        let serialized = serde_json::to_string(&event).unwrap();
        let deserialized: GameEvent = serde_json::from_str(&serialized).unwrap();
        assert_eq!(event, deserialized);
    }

    // ---------------------------------------------------------------------
    // EventObjectSnapshot::classify_filter_shape — the reach gate.
    //
    // The classifier is exhaustive by construction (no wildcard arm), so a new
    // engine TargetFilter/FilterProp/TypeFilter variant is a compile error rather
    // than a silent `Unsupported`. These tests pin the *semantics* the compiler
    // cannot check: which shapes are answerable, which are decided-false, and how
    // composites fold.
    // ---------------------------------------------------------------------
    use super::super::ability::{ControllerRef, TypeFilter, TypedFilter};
    use EventObjectFilterSupport::{PermanentDomainFalse, Supported, Unsupported};

    fn classify(f: &TargetFilter) -> EventObjectFilterSupport {
        EventObjectSnapshot::classify_filter_shape(f)
    }

    /// Positive reach guard. This is the ONLY `valid_card` shape any Connives trigger
    /// in the card pool actually emits today (glorious purpose / iron monger, sadistic
    /// tycoon / ultron, unlimited — 3 cards out of a 35,396-card corpus, verified
    /// 2026-07-12). If this ever classifies as anything but `Supported`, every live
    /// connive card has stopped being matchable.
    ///
    /// A negative test without this guard would be vacuous: it would pass just as
    /// happily if the classifier rejected everything.
    #[test]
    fn live_connives_subject_shape_is_supported() {
        let live = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: Some(ControllerRef::You),
            properties: vec![],
        });
        assert_eq!(classify(&live), Supported);
    }

    /// The subject grammar is shared with ~15 other trigger modes (Mutates, Exploits,
    /// Explores, Evolves, ...), so a property suffix is one Oracle word away from being
    /// reachable on a Connives subject. Properties answerable from the embedded snapshot
    /// must classify `Supported` *before* such a card prints.
    #[test]
    fn snapshot_answerable_properties_are_supported() {
        for prop in [
            FilterProp::Tapped,
            FilterProp::Token,
            FilterProp::Another,
            FilterProp::EnteredThisTurn,
            FilterProp::Attacking { defender: None },
            FilterProp::SaddledSource,
            FilterProp::HasNoAbilities,
        ] {
            let f = TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                properties: vec![prop.clone()],
            });
            assert_eq!(
                classify(&f),
                Supported,
                "{prop:?} must be snapshot-answerable"
            );
        }
    }

    /// `PermanentDomainFalse` is a *decided false*, not a coverage gap: CR 701.50a
    /// connives a permanent, so stack-only predicates and the exile-link selector can
    /// never hold — and must not be confused with "we cannot answer this".
    #[test]
    fn permanent_domain_shapes_are_decided_false_not_gaps() {
        assert_eq!(
            classify(&TargetFilter::ExiledBySource),
            PermanentDomainFalse
        );
        assert_eq!(classify(&TargetFilter::StackSpell), PermanentDomainFalse);
        // Player-axis selectors are false on the object axis.
        assert_eq!(classify(&TargetFilter::Player), PermanentDomainFalse);

        let stack_prop = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::HasSingleTarget],
        });
        assert_eq!(classify(&stack_prop), PermanentDomainFalse);
    }

    /// A shape that needs a live candidate lookup is `Unsupported` — the signal that the
    /// snapshot and its evaluator must be extended in the same change.
    #[test]
    fn live_lookup_shapes_are_unsupported() {
        assert_eq!(classify(&TargetFilter::LastCreated), Unsupported);

        let needs_live = TargetFilter::Typed(TypedFilter {
            type_filters: vec![TypeFilter::Creature],
            controller: None,
            properties: vec![FilterProp::WasKicked],
        });
        assert_eq!(classify(&needs_live), Unsupported);
    }

    /// `Unsupported` dominates a composite: if one branch cannot be answered, the whole
    /// filter cannot be. Getting this backwards would let an unanswerable filter through
    /// the gate and be silently evaluated as `false` at runtime.
    #[test]
    fn unsupported_dominates_composites() {
        let mixed = TargetFilter::Or {
            filters: vec![TargetFilter::Any, TargetFilter::LastCreated],
        };
        assert_eq!(classify(&mixed), Unsupported);

        let all_answerable = TargetFilter::Or {
            filters: vec![TargetFilter::Any, TargetFilter::SelfRef],
        };
        assert_eq!(classify(&all_answerable), Supported);
    }

    /// Conjunction and disjunction fold `PermanentDomainFalse` differently, and conflating
    /// them is a live bug: a `Typed` filter's types and properties are combined with
    /// `.all()` by the runtime evaluator, so ONE domain-false conjunct decides the whole
    /// filter false (`false && x == false`). A disjunction needs *every* branch to be
    /// domain-false before it is (`false || x == x`).
    ///
    /// Caught in review: folding `And`/`Typed` with the disjunction rule reported
    /// `Typed { Creature, props: [HasSingleTarget] }` as `Supported` — claiming the engine
    /// could meaningfully evaluate "a creature that has a single target" against a
    /// permanent, when CR 701.50a guarantees it is simply false.
    #[test]
    fn conjunction_and_disjunction_fold_domain_false_differently() {
        let one_false_conjunct = TargetFilter::And {
            filters: vec![TargetFilter::Any, TargetFilter::ExiledBySource],
        };
        assert_eq!(classify(&one_false_conjunct), PermanentDomainFalse);

        let one_false_disjunct = TargetFilter::Or {
            filters: vec![TargetFilter::Any, TargetFilter::ExiledBySource],
        };
        assert_eq!(classify(&one_false_disjunct), Supported);
    }

    /// A composite of only domain-false children is itself domain-false; mixing in one
    /// answerable child makes a *disjunction* answerable (the false child just contributes
    /// `false`).
    #[test]
    fn domain_false_folds_but_does_not_poison() {
        let all_false = TargetFilter::Or {
            filters: vec![TargetFilter::Player, TargetFilter::ExiledBySource],
        };
        assert_eq!(classify(&all_false), PermanentDomainFalse);

        let mixed = TargetFilter::Or {
            filters: vec![TargetFilter::Player, TargetFilter::SelfRef],
        };
        assert_eq!(classify(&mixed), Supported);
    }

    /// Negating a decided-false yields a decided-*true* — still answerable. Only an
    /// unanswerable inner shape stays unanswerable through a `Not`.
    #[test]
    fn negation_of_domain_false_is_answerable() {
        let not_exiled = TargetFilter::Not {
            filter: Box::new(TargetFilter::ExiledBySource),
        };
        assert_eq!(classify(&not_exiled), Supported);

        let not_unsupported = TargetFilter::Not {
            filter: Box::new(TargetFilter::LastCreated),
        };
        assert_eq!(classify(&not_unsupported), Unsupported);
    }

    /// Composites recurse rather than bottoming out at the top level.
    #[test]
    fn classification_recurses_into_nested_composites() {
        let nested = TargetFilter::And {
            filters: vec![
                TargetFilter::Any,
                TargetFilter::Or {
                    filters: vec![
                        TargetFilter::SelfRef,
                        TargetFilter::Not {
                            filter: Box::new(TargetFilter::LastCreated),
                        },
                    ],
                },
            ],
        };
        assert_eq!(classify(&nested), Unsupported);
    }
}

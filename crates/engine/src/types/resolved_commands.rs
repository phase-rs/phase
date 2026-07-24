//! Append-only, identity-bearing records for resolved rules work.
//!
//! P1 established provenance and ordering. P2 makes mana insert and exact
//! spend commands executable through their owning authority appliers.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::game::triggers::{ConsumedTriggerEventOccurrence, PendingTriggerContext};

use super::ability::TriggerDefinitionRef;
use super::card_type::CoreType;
use super::counter::CounterType;
use super::game_state::{SpellCastRecord, ZoneChangeRecord};
use super::identifiers::{ObjectId, ObjectIncarnationRef, LEGACY_INCARNATION};
use super::mana::{ManaPipId, ManaUnit};
use super::player::{PlayerCounterKind, PlayerId};
use super::resolution::{FrameKind, ResolutionFrame, ResolutionStackError};
use super::zones::Zone;

/// Globally ordered identity of a resolved command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ResolvedCommandOrdinal(pub u64);

/// Globally ordered identity of a rules-execution settlement node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SettlementNodeOrdinal(pub u64);

/// Typed identity of one resolved rules-execution node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub enum RulesExecutionNodeRef {
    Proposal(ResolvedCommandOrdinal),
    ActivatedMana(SettlementNodeOrdinal),
    TriggeredMana(SettlementNodeOrdinal),
    Payment(SettlementNodeOrdinal),
    PlayerLeave(ResolvedCommandOrdinal),
}

/// Exact recipient of one mana payment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ManaPaymentRecipient {
    Object(ObjectIncarnationRef),
    Player(PlayerId),
}

/// One exact mana-pool insertion after mana production has been resolved.
///
/// CR 106.4: resolved mana enters this player’s pool with this already-stamped
/// pip identity; replay must neither choose a new recipient nor mint a new pip.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedManaInsertCommand {
    pub player: PlayerId,
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
}

/// One exact mana unit selected by the payment solver, with its producer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedManaSpentUnit {
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
}

/// One exact mana-pool removal after the payment solver has selected its units.
///
/// CR 118.3a: this command removes precisely these units, in their recorded
/// consumption order. It never asks a solver to choose replacement mana.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedManaSpendCommand {
    pub payer: PlayerId,
    pub recipient: ManaPaymentRecipient,
    pub payment: RulesExecutionNodeRef,
    pub units: Vec<ResolvedManaSpentUnit>,
}

/// One resolved scalar change to a player's rules-visible resources.
///
/// Each variant is a semantic edit rather than a whole-player replacement, so
/// independently retained resource commands compose against the retained
/// prefix. Life changes record their final post-replacement delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedPlayerEdit {
    /// CR 119.2 + CR 119.3 + CR 119.4 + CR 119.5: A final gain/loss delta
    /// applied after any replacement.
    Life { delta: i32 },
    /// CR 122.1 + CR 107.14: A final energy-counter delta.
    Energy { delta: i32 },
    /// CR 122.1: A final delta for one exact player counter kind.
    Counter { kind: PlayerCounterKind, delta: i32 },
    /// CR 702.179b: An exact speed transition, including no-speed.
    Speed { old: Option<u8>, new: Option<u8> },
}

/// One exact player-resource mutation after replacement and quantity resolution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPlayerEditCommand {
    pub player: PlayerId,
    pub edit: ResolvedPlayerEdit,
    pub cause: RulesExecutionNodeRef,
}

/// The object-state status axis owned by a resolved command.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedObjectStatus {
    /// CR 701.26: The permanent's tapped state.
    Tapped,
    /// CR 701.43d: The exact object was exerted during this turn.
    Exerted,
}

/// One exact object-status transition with an optimistic old-status precondition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedObjectStatusCommand {
    pub object: ObjectIncarnationRef,
    pub status: ResolvedObjectStatus,
    pub expected_old: bool,
    pub new: bool,
    pub cause: RulesExecutionNodeRef,
}

/// The final mutation to one exact object's counter map.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedObjectCounterEdit {
    /// CR 122.1 + CR 122.6: Put this final post-replacement count of counters
    /// on the exact object. The actor is retained for counter-history facts.
    Add { actor: PlayerId, count: u32 },
    /// CR 122.1: Remove this final already-clamped count from the exact object.
    Remove { count: u32 },
}

/// One exact object-counter delivery after all replacement effects have settled.
///
/// `expected_old` makes this semantic delta non-idempotent: retained-prefix
/// replay applies it exactly once instead of adding/removing another count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedObjectCounterCommand {
    pub object: ObjectIncarnationRef,
    pub counter_type: CounterType,
    pub expected_old: u32,
    pub edit: ResolvedObjectCounterEdit,
    pub cause: RulesExecutionNodeRef,
}

/// The audience that received one exact revealed-card fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedInformationAudience {
    /// CR 701.20a: The controller's active reveal lease, retained only while
    /// the resolving instruction still needs the revealed card.
    Controller(PlayerId),
    /// CR 701.20a: A fact that has been published to every player.
    Public,
}

/// The precise lifetime of one revealed-card fact.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedInformationLifetime {
    /// CR 701.20a: The reveal remains available through the current effect or
    /// prompt and is cleared at the next applicable action boundary.
    UntilActionBoundary,
    /// CR 400.7: The published fact belongs to this object incarnation and
    /// expires when that object changes zones.
    UntilZoneChange,
}

/// The final information-boundary transition for exact object occurrences.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedInformationEdit {
    Reveal,
    Hide,
}

/// One resolved reveal or hide transition after all card selection is settled.
///
/// `occurrences` deliberately stores exact object incarnations rather than raw
/// `ObjectId`s: CR 400.7 makes a zone-changed object a new object even when the
/// engine reuses its storage id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedInformationCommand {
    pub occurrences: Vec<ObjectIncarnationRef>,
    pub audience: ResolvedInformationAudience,
    pub lifetime: ResolvedInformationLifetime,
    pub edit: ResolvedInformationEdit,
    pub cause: RulesExecutionNodeRef,
}

/// One exact constrained-trigger ledger fact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedTriggerLedgerEdit {
    /// CR 603.2c: This trigger occurrence has used its one-per-turn fact.
    OncePerTurn,
    /// CR 603.2c: This trigger occurrence has used its one-per-game fact.
    OncePerGame,
    /// CR 603.2c: This trigger occurrence has used this opponent's per-turn fact.
    OncePerOpponentPerTurn { opponent: PlayerId },
    /// Increment from the captured prior count for MaxTimesPerTurn.
    MaxTimesPerTurn { expected_old: u32 },
}

/// A named once-per-turn permission slot consumed by a completed play or cast.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResolvedOncePerTurnPermission {
    GraveyardCast,
    GraveyardCastPermanentType { permanent_type: CoreType },
    HandCastFree,
    AlternativeCostGrant,
    ExilePlay,
    ExileCast,
    TopOfLibraryCast,
}

/// A composable per-event ledger mutation.
///
/// Each payload changes only one exact key or append position. Turn-boundary
/// bulk clears intentionally belong to the future turn-transition family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedLedgerEdit {
    /// CR 601.2i: Append one finalized spell-cast fact to this player's history.
    SpellCast {
        player: PlayerId,
        record: SpellCastRecord,
        expected_turn_count: u8,
        expected_game_count: u32,
        expected_turn_history_len: u32,
        expected_game_history_len: u32,
    },
    /// CR 602.5b: Increment exactly one activated-ability occurrence's facts.
    AbilityActivated {
        source: super::identifiers::ObjectId,
        ability_index: usize,
        expected_turn_count: u32,
        expected_game_count: u32,
    },
    /// CR 603.2c: Record one constrained trigger occurrence.
    TriggerFired {
        trigger: TriggerDefinitionRef,
        edit: ResolvedTriggerLedgerEdit,
    },
    /// CR 601.2i: Consume one already-selected bounded permission slot.
    OncePerTurnPermission {
        source: super::identifiers::ObjectId,
        permission: ResolvedOncePerTurnPermission,
    },
    /// CR 121.1 + CR 121.2 + CR 121.4: Install one settled draw's bookkeeping
    /// after its zone transition has already been resolved. `drawn_object` is
    /// `None` only for an attempted draw from an empty library.
    CardsDrawn {
        player: PlayerId,
        drawn_object: Option<ObjectIncarnationRef>,
        attempted_empty_library: bool,
        expected_has_drawn_this_turn: bool,
        resulting_has_drawn_this_turn: bool,
        expected_cards_drawn_this_turn: u32,
        resulting_cards_drawn_this_turn: u32,
        expected_cards_drawn_this_step: u32,
        resulting_cards_drawn_this_step: u32,
        expected_drew_from_empty_library: bool,
        resulting_drew_from_empty_library: bool,
        expected_drawn_cards_len: u32,
        resulting_drawn_cards_len: u32,
        expected_first_card_drawn_this_turn: Option<ObjectId>,
        resulting_first_card_drawn_this_turn: Option<ObjectId>,
    },
}

/// One exact per-event ledger mutation with its causal node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLedgerEditCommand {
    pub edit: ResolvedLedgerEdit,
    pub cause: RulesExecutionNodeRef,
}

/// One exact library shuffle with its consumed ChaCha20 stream span.
///
/// CR 701.24a: the ordinary path randomizes the captured predecessor order
/// once. Replay installs `resulting_order` and advances only through the
/// recorded entropy span; it never samples the RNG again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLibraryShuffleCommand {
    pub player: PlayerId,
    pub precondition_order: Vec<ObjectId>,
    pub resulting_order: Vec<ObjectId>,
    pub pre_word_pos: u128,
    pub post_word_pos: u128,
    pub cause: RulesExecutionNodeRef,
}

/// One exact transition of an object occurrence between zone containers.
///
/// CR 400.7: the command binds the source occurrence and its resulting
/// incarnation, so replay neither selects a new object nor creates a new
/// incarnation. CR 613.7d: battlefield-entry timestamps are captured by the
/// ordinary path and installed exactly on replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedZoneChangeCommand {
    pub object: ObjectIncarnationRef,
    pub resulting_incarnation: u64,
    pub from: Zone,
    pub to: Zone,
    /// Zero-based position after the source occurrence has been removed.
    pub destination_position: usize,
    pub owner: PlayerId,
    pub entry_timestamp: Option<u64>,
    pub turn_zone_change_index: usize,
    pub zone_change_record: ZoneChangeRecord,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved zone transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedZoneChangeReplayInvariantError {
    #[error("zone-change command references an unknown object {0:?}")]
    UnknownObject(ObjectId),
    #[error("zone-change occurrence mismatch: expected {expected:?}, found {found:?}")]
    OccurrenceMismatch {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("zone-change owner mismatch: expected {expected:?}, found {found:?}")]
    OwnerMismatch { expected: PlayerId, found: PlayerId },
    #[error("zone-change source-zone mismatch: expected {expected:?}, found {found:?}")]
    SourceZoneMismatch { expected: Zone, found: Zone },
    #[error("zone-change destination position mismatch: expected {expected}, found {found}")]
    DestinationPositionMismatch { expected: usize, found: usize },
    #[error("zone-change turn-record index mismatch: expected {expected}, found {found}")]
    TurnRecordIndexMismatch { expected: usize, found: usize },
    #[error("zone-change battlefield entry is missing its timestamp")]
    MissingBattlefieldEntryTimestamp,
    #[error("zone-change nonbattlefield entry unexpectedly has a timestamp")]
    UnexpectedNonbattlefieldTimestamp,
    #[error("zone-change installed incarnation mismatch: expected {expected}, found {found}")]
    ResultingIncarnationMismatch { expected: u64, found: u64 },
}

/// One bounded structural transition of the resolution-frame stack.
///
/// This carries only the primitive operation and its native operand. It never
/// records stack positions, frame identities, or displaced frame payloads.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResolvedFrameTransition {
    Push { frame: ResolutionFrame },
    InsertParentOfActive { frame: ResolutionFrame },
    PopExpected { kind: FrameKind },
    ReplaceActive { frame: ResolutionFrame },
}

/// One exact resolution-frame transition under its causal rules-execution node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedFrameTransitionCommand {
    pub transition: ResolvedFrameTransition,
    pub cause: RulesExecutionNodeRef,
}

/// Exact trigger occurrences collected at one logical trigger/LKI boundary.
///
/// CR 603.2 + CR 603.3b: collected trigger contexts retain their already
/// determined firing and placement order. CR 603.10 + CR 603.10a: final
/// logical zone-change settlement uses the recorded pre-event authority.
/// CR 603.2c: consumed event occurrences prevent the generic priority scan
/// from collecting the same occurrence a second time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedTriggerCollection {
    DeferPending {
        contexts: Vec<PendingTriggerContext>,
    },
    ConsumeBeforePriority {
        occurrences: Vec<ConsumedTriggerEventOccurrence>,
    },
}

/// One exact trigger/LKI collection append under its causal rules-execution node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTriggerCollectionCommand {
    pub collection: ResolvedTriggerCollection,
    pub cause: RulesExecutionNodeRef,
}

/// Semantic command payload currently carried by a resolved-rules journal entry.
///
/// Additional command families are intentionally added by their owning P2
/// authority rather than by a central replay dispatcher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum ResolvedRulesCommand {
    ManaInsert(ResolvedManaInsertCommand),
    ManaSpend(ResolvedManaSpendCommand),
    PlayerEdit(ResolvedPlayerEditCommand),
    ObjectStatus(ResolvedObjectStatusCommand),
    ObjectCounter(ResolvedObjectCounterCommand),
    Information(ResolvedInformationCommand),
    LedgerEdit(ResolvedLedgerEditCommand),
    LibraryShuffle(ResolvedLibraryShuffleCommand),
    ZoneChange(Box<ResolvedZoneChangeCommand>),
    FrameTransition(Box<ResolvedFrameTransitionCommand>),
    TriggerCollection(ResolvedTriggerCollectionCommand),
}

/// An append-only trigger collection command has no replay-time precondition.
///
/// The uninhabited type keeps the uniform resolved-command applier signature
/// without inventing a failure mode for a pure `Vec::extend` operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedTriggerCollectionReplayInvariantError {}

/// Typed failure while applying an already-resolved frame transition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedFrameTransitionReplayInvariantError {
    #[error(transparent)]
    Stack(#[from] ResolutionStackError),
}

/// Typed failure while advancing the canonical ChaCha20 entropy high-water mark.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRngReplayInvariantError {
    HighWaterRegression { current: u128, requested: u128 },
    StreamPositionRegression { current: u128, requested: u128 },
}

impl std::fmt::Display for ResolvedRngReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::HighWaterRegression { current, requested } => write!(
                f,
                "resolved entropy command would regress high-water from {current} to {requested}"
            ),
            Self::StreamPositionRegression { current, requested } => write!(
                f,
                "resolved entropy command would rewind the ChaCha20 stream from {current} to {requested}"
            ),
        }
    }
}

impl std::error::Error for ResolvedRngReplayInvariantError {}

/// Typed failure while applying an already-resolved library shuffle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLibraryShuffleReplayInvariantError {
    UnknownPlayer(PlayerId),
    LibraryOrderPreconditionMismatch,
    RngWordPositionPreconditionMismatch {
        expected: u128,
        found: u128,
    },
    RngCursorPositionPreconditionMismatch {
        expected: u128,
        found: u128,
    },
    InvalidLibraryOrderReceipt,
    EntropyReceiptRegression {
        pre: u128,
        post: u128,
    },
    /// CR 701.24a: A Fisher-Yates shuffle of two or more cards always consumes
    /// at least one random draw, so a multi-card receipt whose entropy span is
    /// empty could not have come from a real shuffle. Accepting it would install
    /// a permutation while leaving the RNG cursor unadvanced, desynchronizing
    /// every later entropy-backed replay.
    MultiCardReceiptWithoutEntropy {
        cards: usize,
        position: u128,
    },
    RngHighWater(ResolvedRngReplayInvariantError),
}

impl std::fmt::Display for ResolvedLibraryShuffleReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPlayer(player) => {
                write!(
                    f,
                    "resolved library shuffle cannot find player {}",
                    player.0
                )
            }
            Self::LibraryOrderPreconditionMismatch => {
                write!(
                    f,
                    "resolved library shuffle does not match its recorded predecessor order"
                )
            }
            Self::RngWordPositionPreconditionMismatch { expected, found } => write!(
                f,
                "resolved library shuffle expected RNG high-water {expected}, found {found}"
            ),
            Self::RngCursorPositionPreconditionMismatch { expected, found } => write!(
                f,
                "resolved library shuffle expected ChaCha20 position {expected}, found {found}"
            ),
            Self::InvalidLibraryOrderReceipt => {
                write!(
                    f,
                    "resolved library shuffle has an invalid ordered-card receipt"
                )
            }
            Self::EntropyReceiptRegression { pre, post } => write!(
                f,
                "resolved library shuffle regresses entropy from {pre} to {post}"
            ),
            Self::MultiCardReceiptWithoutEntropy { cards, position } => write!(
                f,
                "resolved library shuffle permutes {cards} cards without advancing entropy past {position}"
            ),
            Self::RngHighWater(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for ResolvedLibraryShuffleReplayInvariantError {}

impl From<ResolvedRngReplayInvariantError> for ResolvedLibraryShuffleReplayInvariantError {
    fn from(error: ResolvedRngReplayInvariantError) -> Self {
        Self::RngHighWater(error)
    }
}

/// Typed failure while applying an already-resolved mana command to a replay state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedManaReplayInvariantError {
    UnknownPlayer(PlayerId),
    UnstampedManaPip,
    DuplicateManaPip(ManaPipId),
    ManaPipIdOverflow(ManaPipId),
    DuplicateSpentManaPip(ManaPipId),
    MissingExactManaUnit(ManaPipId),
    MismatchedExactManaUnit(ManaPipId),
}

impl std::fmt::Display for ResolvedManaReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPlayer(player) => write!(f, "unknown mana-command player {}", player.0),
            Self::UnstampedManaPip => write!(f, "resolved mana command has an unstamped pip"),
            Self::DuplicateManaPip(pip) => {
                write!(f, "resolved mana command would duplicate pip {}", pip.0)
            }
            Self::ManaPipIdOverflow(pip) => {
                write!(f, "resolved mana command cannot advance past pip {}", pip.0)
            }
            Self::DuplicateSpentManaPip(pip) => {
                write!(f, "resolved mana spend repeats pip {}", pip.0)
            }
            Self::MissingExactManaUnit(pip) => {
                write!(f, "resolved mana spend cannot find pip {}", pip.0)
            }
            Self::MismatchedExactManaUnit(pip) => {
                write!(f, "resolved mana spend found mismatched pip {}", pip.0)
            }
        }
    }
}

impl std::error::Error for ResolvedManaReplayInvariantError {}

/// Typed failure while applying an already-resolved player-resource command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedPlayerEditReplayInvariantError {
    UnknownPlayer(PlayerId),
    ZeroDelta,
    ResourceUnderflow,
    ResourceOverflow,
    SpeedPreconditionMismatch {
        expected: Option<u8>,
        found: Option<u8>,
    },
}

impl std::fmt::Display for ResolvedPlayerEditReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPlayer(player) => write!(f, "unknown player-command player {}", player.0),
            Self::ZeroDelta => write!(f, "resolved player command has a zero delta"),
            Self::ResourceUnderflow => {
                write!(f, "resolved player command would underflow a resource")
            }
            Self::ResourceOverflow => {
                write!(f, "resolved player command would overflow a resource")
            }
            Self::SpeedPreconditionMismatch { expected, found } => write!(
                f,
                "resolved speed command expected {expected:?}, found {found:?}"
            ),
        }
    }
}

impl std::error::Error for ResolvedPlayerEditReplayInvariantError {}

/// Typed failure while applying an already-resolved object-status command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedObjectStatusReplayInvariantError {
    UnknownObject(super::identifiers::ObjectId),
    MissingObject(ObjectIncarnationRef),
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    StatusPreconditionMismatch {
        status: ResolvedObjectStatus,
        expected: bool,
        found: bool,
    },
}

impl std::fmt::Display for ResolvedObjectStatusReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownObject(object) => {
                write!(
                    f,
                    "resolved object-status command cannot find object {}",
                    object.0
                )
            }
            Self::MissingObject(object) => {
                write!(f, "resolved object-status command cannot find {object:?}")
            }
            Self::StaleObject { expected, found } => write!(
                f,
                "resolved object-status command expected {expected:?}, found {found:?}"
            ),
            Self::StatusPreconditionMismatch {
                status,
                expected,
                found,
            } => write!(
                f,
                "resolved {status:?} command expected status {expected}, found {found}"
            ),
        }
    }
}

impl std::error::Error for ResolvedObjectStatusReplayInvariantError {}

/// Typed failure while applying an already-resolved object-counter command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedObjectCounterReplayInvariantError {
    MissingObject(ObjectIncarnationRef),
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    ZeroCount,
    CounterPreconditionMismatch {
        counter_type: CounterType,
        expected: u32,
        found: u32,
    },
    CounterOverflow {
        counter_type: CounterType,
        previous: u32,
        added: u32,
    },
}

impl std::fmt::Display for ResolvedObjectCounterReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::MissingObject(object) => {
                write!(f, "resolved counter command cannot find {object:?}")
            }
            Self::StaleObject { expected, found } => write!(
                f,
                "resolved counter command expected {expected:?}, found {found:?}"
            ),
            Self::ZeroCount => write!(f, "resolved counter command has a zero count"),
            Self::CounterPreconditionMismatch {
                counter_type,
                expected,
                found,
            } => write!(
                f,
                "resolved {counter_type:?} counter command expected {expected}, found {found}"
            ),
            Self::CounterOverflow {
                counter_type,
                previous,
                added,
            } => write!(
                f,
                "resolved {counter_type:?} counter command overflows {previous} + {added}"
            ),
        }
    }
}

impl std::error::Error for ResolvedObjectCounterReplayInvariantError {}

/// Typed failure while applying an exact revealed-information command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedInformationReplayInvariantError {
    EmptyOccurrences,
    DuplicateOccurrence(ObjectIncarnationRef),
    MissingObject(ObjectIncarnationRef),
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    RevealAlreadyActive(ObjectIncarnationRef),
    HideWithoutActiveReveal(ObjectIncarnationRef),
    InvalidAudienceLifetime {
        audience: ResolvedInformationAudience,
        lifetime: ResolvedInformationLifetime,
    },
}

impl std::fmt::Display for ResolvedInformationReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::EmptyOccurrences => write!(f, "resolved information command has no occurrences"),
            Self::DuplicateOccurrence(occurrence) => {
                write!(f, "resolved information command repeats {occurrence:?}")
            }
            Self::MissingObject(occurrence) => {
                write!(f, "resolved information command cannot find {occurrence:?}")
            }
            Self::StaleObject { expected, found } => write!(
                f,
                "resolved information command expected {expected:?}, found {found:?}"
            ),
            Self::RevealAlreadyActive(occurrence) => {
                write!(
                    f,
                    "resolved information command reveals active {occurrence:?}"
                )
            }
            Self::HideWithoutActiveReveal(occurrence) => {
                write!(
                    f,
                    "resolved information command hides inactive {occurrence:?}"
                )
            }
            Self::InvalidAudienceLifetime { audience, lifetime } => write!(
                f,
                "resolved information command has incompatible {audience:?} and {lifetime:?}"
            ),
        }
    }
}

impl std::error::Error for ResolvedInformationReplayInvariantError {}

/// Typed failure while applying an already-resolved per-event ledger command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedLedgerEditReplayInvariantError {
    UnknownPlayer(PlayerId),
    SpellCastPreconditionMismatch,
    AbilityActivationPreconditionMismatch,
    CardsDrawnPreconditionMismatch,
    DrawnObjectMismatch {
        expected: ObjectIncarnationRef,
        found: Option<ObjectIncarnationRef>,
    },
    DrawnObjectStillInLibrary(ObjectIncarnationRef),
    TriggerAlreadyRecorded,
    TriggerCountPreconditionMismatch {
        expected: u32,
        found: u32,
    },
    PermissionAlreadyConsumed(ResolvedOncePerTurnPermission),
    CounterOverflow,
}

impl std::fmt::Display for ResolvedLedgerEditReplayInvariantError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnknownPlayer(player) => write!(f, "unknown ledger-command player {}", player.0),
            Self::SpellCastPreconditionMismatch => {
                write!(
                    f,
                    "resolved spell-cast command does not match its ledger prefix"
                )
            }
            Self::AbilityActivationPreconditionMismatch => write!(
                f,
                "resolved activated-ability command does not match its ledger prefix"
            ),
            Self::CardsDrawnPreconditionMismatch => write!(
                f,
                "resolved draw-bookkeeping command does not match its ledger prefix"
            ),
            Self::DrawnObjectMismatch { expected, found } => write!(
                f,
                "resolved drawn-object occurrence mismatch: expected {expected:?}, found {found:?}"
            ),
            Self::DrawnObjectStillInLibrary(object) => write!(
                f,
                "resolved drawn-object occurrence remained in its library: {object:?}"
            ),
            Self::TriggerAlreadyRecorded => {
                write!(
                    f,
                    "resolved trigger command repeats an existing once-only fact"
                )
            }
            Self::TriggerCountPreconditionMismatch { expected, found } => write!(
                f,
                "resolved trigger command expected count {expected}, found {found}"
            ),
            Self::PermissionAlreadyConsumed(permission) => {
                write!(f, "resolved {permission:?} permission was already consumed")
            }
            Self::CounterOverflow => write!(f, "resolved ledger command overflows a counter"),
        }
    }
}

impl std::error::Error for ResolvedLedgerEditReplayInvariantError {}

/// Semantic category of a resolved rules-execution node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RulesExecutionNodeKind {
    Proposal,
    ActivatedMana {
        source: ObjectIncarnationRef,
    },
    TriggeredMana {
        source: ObjectIncarnationRef,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        trigger: Option<TriggerDefinitionRef>,
    },
    Payment {
        payer: PlayerId,
        recipient: ManaPaymentRecipient,
    },
    PlayerLeave,
}

/// Metadata shared by every resolved rules-execution node.
///
/// bundle_parent lets a triggered mana ability remain selectable with its
/// causing activation while retaining its own distinct causal node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SettlementNode {
    pub ordinal: SettlementNodeOrdinal,
    pub identity: RulesExecutionNodeRef,
    pub kind: RulesExecutionNodeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub caused_by: Option<RulesExecutionNodeRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<RulesExecutionNodeRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bundle_parent: Option<RulesExecutionNodeRef>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub produced_pips: Vec<ManaPipId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub spent_pips: Vec<ManaPipId>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub journal_ordinals: Vec<ResolvedCommandOrdinal>,
}

/// One command slot assigned to a journal node.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedCommandJournalEntry {
    pub ordinal: ResolvedCommandOrdinal,
    pub node: RulesExecutionNodeRef,
    /// P1 node slots intentionally have no semantic payload. P2 commands append
    /// their own globally ordered entry while preserving those original slots.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<ResolvedRulesCommand>,
}

/// Exact stamped mana created by one node.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProducedManaUnit {
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
}

impl PartialEq for ProducedManaUnit {
    fn eq(&self, other: &Self) -> bool {
        self.unit.pip_id == other.unit.pip_id
            && self.unit == other.unit
            && self.producer == other.producer
    }
}

impl Eq for ProducedManaUnit {}

/// Exact mana unit consumed for one cost component, in consumption order.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpentManaUnit {
    pub unit: ManaUnit,
    pub producer: RulesExecutionNodeRef,
    pub payment: RulesExecutionNodeRef,
    pub recipient: ManaPaymentRecipient,
}

impl PartialEq for SpentManaUnit {
    fn eq(&self, other: &Self) -> bool {
        self.unit.pip_id == other.unit.pip_id
            && self.unit == other.unit
            && self.producer == other.producer
            && self.payment == other.payment
            && self.recipient == other.recipient
    }
}

impl Eq for SpentManaUnit {}

/// Checked allocation and authority-validation failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedRulesJournalError {
    CommandOrdinalOverflow,
    SettlementNodeOrdinalOverflow,
    UnstampedManaPip,
    DuplicateProducedPip(ManaPipId),
    UnknownProducedPip(ManaPipId),
    DuplicateSpentPip(ManaPipId),
    UnknownNode(RulesExecutionNodeRef),
    InvalidSerializedAuthority(String),
}

impl std::fmt::Display for ResolvedRulesJournalError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::CommandOrdinalOverflow => write!(f, "resolved-command ordinal overflow"),
            Self::SettlementNodeOrdinalOverflow => write!(f, "settlement-node ordinal overflow"),
            Self::UnstampedManaPip => write!(f, "mana provenance requires a stamped pip id"),
            Self::DuplicateProducedPip(pip) => write!(f, "duplicate produced pip {}", pip.0),
            Self::UnknownProducedPip(pip) => write!(f, "spent pip {} has no producer", pip.0),
            Self::DuplicateSpentPip(pip) => write!(f, "pip {} was spent more than once", pip.0),
            Self::UnknownNode(node) => write!(f, "journal references unknown node {node:?}"),
            Self::InvalidSerializedAuthority(reason) => {
                write!(f, "invalid resolved-rules journal: {reason}")
            }
        }
    }
}

impl std::error::Error for ResolvedRulesJournalError {}

/// Persistent resolved rules journal.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ResolvedRulesJournal {
    next_command_ordinal: u64,
    next_settlement_node_ordinal: u64,
    entries: Vec<ResolvedCommandJournalEntry>,
    nodes: Vec<SettlementNode>,
    produced_mana: Vec<ProducedManaUnit>,
    spent_mana: Vec<SpentManaUnit>,
}

#[derive(Serialize, Deserialize)]
struct ResolvedRulesJournalWire {
    next_command_ordinal: u64,
    next_settlement_node_ordinal: u64,
    #[serde(default)]
    entries: Vec<ResolvedCommandJournalEntry>,
    #[serde(default)]
    nodes: Vec<SettlementNode>,
    #[serde(default)]
    produced_mana: Vec<ProducedManaUnit>,
    #[serde(default)]
    spent_mana: Vec<SpentManaUnit>,
}

impl Serialize for ResolvedRulesJournal {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        ResolvedRulesJournalWire {
            next_command_ordinal: self.next_command_ordinal,
            next_settlement_node_ordinal: self.next_settlement_node_ordinal,
            entries: self.entries.clone(),
            nodes: self.nodes.clone(),
            produced_mana: self.produced_mana.clone(),
            spent_mana: self.spent_mana.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ResolvedRulesJournal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = ResolvedRulesJournalWire::deserialize(deserializer)?;
        let journal = Self {
            next_command_ordinal: wire.next_command_ordinal,
            next_settlement_node_ordinal: wire.next_settlement_node_ordinal,
            entries: wire.entries,
            nodes: wire.nodes,
            produced_mana: wire.produced_mana,
            spent_mana: wire.spent_mana,
        };
        journal
            .validate_serialized_authority()
            .map_err(serde::de::Error::custom)?;
        Ok(journal)
    }
}

impl ResolvedRulesJournal {
    pub fn entries(&self) -> &[ResolvedCommandJournalEntry] {
        &self.entries
    }

    pub fn nodes(&self) -> &[SettlementNode] {
        &self.nodes
    }

    pub fn produced_mana(&self) -> &[ProducedManaUnit] {
        &self.produced_mana
    }

    pub fn spent_mana(&self) -> &[SpentManaUnit] {
        &self.spent_mana
    }

    pub fn has_produced_pip(&self, pip: ManaPipId) -> bool {
        self.produced_mana
            .iter()
            .any(|record| record.unit.pip_id == pip)
    }

    pub fn latest_mana_producer_for_source(
        &self,
        source_id: super::identifiers::ObjectId,
    ) -> Option<RulesExecutionNodeRef> {
        self.produced_mana
            .iter()
            .rev()
            .find(|record| record.unit.source_id == source_id)
            .map(|record| record.producer)
    }

    pub fn next_command_ordinal(&self) -> ResolvedCommandOrdinal {
        ResolvedCommandOrdinal(self.next_command_ordinal)
    }

    pub fn next_settlement_node_ordinal(&self) -> SettlementNodeOrdinal {
        SettlementNodeOrdinal(self.next_settlement_node_ordinal)
    }

    /// Opens a proposal node for legacy production outside a specific scope.
    pub fn begin_proposal(&mut self) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.ensure_node_capacity()?;
        let command = self.allocate_command();
        let ordinal = self.allocate_node();
        let identity = RulesExecutionNodeRef::Proposal(command);
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal: command,
            node: identity,
            command: None,
        });
        self.nodes.push(SettlementNode {
            ordinal,
            identity,
            kind: RulesExecutionNodeKind::Proposal,
            caused_by: None,
            depends_on: Vec::new(),
            bundle_parent: None,
            produced_pips: Vec::new(),
            spent_pips: Vec::new(),
            journal_ordinals: vec![command],
        });
        Ok(identity)
    }

    pub fn begin_activated_mana(
        &mut self,
        source: ObjectIncarnationRef,
        caused_by: Option<RulesExecutionNodeRef>,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.begin_settlement(
            RulesExecutionNodeRef::ActivatedMana,
            RulesExecutionNodeKind::ActivatedMana { source },
            caused_by,
            None,
        )
    }

    pub fn begin_triggered_mana(
        &mut self,
        source: ObjectIncarnationRef,
        trigger: Option<TriggerDefinitionRef>,
        caused_by: Option<RulesExecutionNodeRef>,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        let bundle_parent = caused_by
            .map(|cause| self.bundle_owner(cause))
            .transpose()?
            .flatten();
        self.begin_settlement(
            RulesExecutionNodeRef::TriggeredMana,
            RulesExecutionNodeKind::TriggeredMana { source, trigger },
            caused_by,
            bundle_parent,
        )
    }

    pub fn record_produced_mana(
        &mut self,
        producer: RulesExecutionNodeRef,
        unit: ManaUnit,
    ) -> Result<(), ResolvedRulesJournalError> {
        Self::require_stamped(unit.pip_id)?;
        let node_index = self.node_index(producer)?;
        if self
            .produced_mana
            .iter()
            .any(|record| record.unit.pip_id == unit.pip_id)
        {
            return Err(ResolvedRulesJournalError::DuplicateProducedPip(unit.pip_id));
        }
        self.nodes[node_index].produced_pips.push(unit.pip_id);
        self.produced_mana.push(ProducedManaUnit { unit, producer });
        Ok(())
    }

    /// Records and owns the exact command that inserted one mana unit.
    pub fn record_mana_insert(
        &mut self,
        command: ResolvedManaInsertCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.record_produced_mana(command.producer, command.unit.clone())?;
        self.append_command(command.producer, ResolvedRulesCommand::ManaInsert(command))
    }

    /// Records all exact units consumed by one cost component in solver order.
    pub fn record_spent_mana(
        &mut self,
        payer: PlayerId,
        recipient: ManaPaymentRecipient,
        spent: &[ManaUnit],
    ) -> Result<Option<RulesExecutionNodeRef>, ResolvedRulesJournalError> {
        if spent.is_empty() {
            return Ok(None);
        }
        let mut seen = HashSet::new();
        let mut dependencies = Vec::new();
        let mut producers = Vec::with_capacity(spent.len());
        for unit in spent {
            Self::require_stamped(unit.pip_id)?;
            if !seen.insert(unit.pip_id) || self.spent_pip_exists(unit.pip_id) {
                return Err(ResolvedRulesJournalError::DuplicateSpentPip(unit.pip_id));
            }
            let Some(produced) = self
                .produced_mana
                .iter()
                .find(|record| record.unit.pip_id == unit.pip_id)
            else {
                return Err(ResolvedRulesJournalError::UnknownProducedPip(unit.pip_id));
            };
            if !dependencies.contains(&produced.producer) {
                dependencies.push(produced.producer);
            }
            producers.push(produced.producer);
        }
        let payment = self.begin_settlement(
            RulesExecutionNodeRef::Payment,
            RulesExecutionNodeKind::Payment {
                payer,
                recipient: recipient.clone(),
            },
            None,
            None,
        )?;
        let payment_index = self.node_index(payment)?;
        self.nodes[payment_index].depends_on = dependencies;
        self.nodes[payment_index].spent_pips = spent.iter().map(|unit| unit.pip_id).collect();
        self.spent_mana.extend(
            spent
                .iter()
                .cloned()
                .zip(producers)
                .map(|(unit, producer)| SpentManaUnit {
                    unit,
                    producer,
                    payment,
                    recipient: recipient.clone(),
                }),
        );
        Ok(Some(payment))
    }

    /// Records and owns one exact solver-selected mana payment command.
    pub fn record_mana_spend(
        &mut self,
        payer: PlayerId,
        recipient: ManaPaymentRecipient,
        spent: &[ManaUnit],
    ) -> Result<Option<ResolvedManaSpendCommand>, ResolvedRulesJournalError> {
        if spent.is_empty() {
            return Ok(None);
        }
        self.ensure_command_capacity_for(2)?;
        let Some(payment) = self.record_spent_mana(payer, recipient.clone(), spent)? else {
            return Ok(None);
        };
        self.ensure_command_capacity()?;
        let units = spent
            .iter()
            .map(|unit| {
                let producer = self
                    .spent_mana
                    .iter()
                    .find(|record| record.payment == payment && record.unit.pip_id == unit.pip_id)
                    .expect("recorded spent mana must retain its producer")
                    .producer;
                ResolvedManaSpentUnit {
                    unit: unit.clone(),
                    producer,
                }
            })
            .collect();
        let command = ResolvedManaSpendCommand {
            payer,
            recipient,
            payment,
            units,
        };
        self.append_command(payment, ResolvedRulesCommand::ManaSpend(command.clone()))?;
        Ok(Some(command))
    }

    /// Records one final scalar player-resource mutation under its causal node.
    pub fn record_player_edit(
        &mut self,
        command: ResolvedPlayerEditCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::PlayerEdit(command))
    }

    /// Records one exact object-status transition under its causal node.
    pub fn record_object_status(
        &mut self,
        command: ResolvedObjectStatusCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::ObjectStatus(command))
    }

    /// Records one final object-counter delivery under its causal node.
    pub fn record_object_counter(
        &mut self,
        command: ResolvedObjectCounterCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::ObjectCounter(command))
    }

    /// Records one exact information-boundary transition under its causal node.
    pub fn record_information(
        &mut self,
        command: ResolvedInformationCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::Information(command))
    }

    /// Records one exact semantic ledger mutation under its causal node.
    pub fn record_ledger_edit(
        &mut self,
        command: ResolvedLedgerEditCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::LedgerEdit(command))
    }

    /// Records one exact library order plus its already-consumed entropy span.
    pub fn record_library_shuffle(
        &mut self,
        command: ResolvedLibraryShuffleCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::LibraryShuffle(command))
    }

    /// Records one exact zone-container transition under its causal node.
    pub fn record_zone_change(
        &mut self,
        command: ResolvedZoneChangeCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::ZoneChange(Box::new(command)),
        )
    }

    /// Records one exact bounded resolution-frame transition under its causal node.
    pub fn record_frame_transition(
        &mut self,
        command: ResolvedFrameTransitionCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::FrameTransition(Box::new(command)),
        )
    }

    /// Records one exact trigger/LKI collection append under its causal node.
    pub fn record_trigger_collection(
        &mut self,
        command: ResolvedTriggerCollectionCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::TriggerCollection(command),
        )
    }

    fn begin_settlement(
        &mut self,
        identity_for: impl FnOnce(SettlementNodeOrdinal) -> RulesExecutionNodeRef,
        kind: RulesExecutionNodeKind,
        caused_by: Option<RulesExecutionNodeRef>,
        bundle_parent: Option<RulesExecutionNodeRef>,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.ensure_node_capacity()?;
        for dependency in caused_by.iter().chain(bundle_parent.iter()) {
            self.node_index(*dependency)?;
        }
        let command = self.allocate_command();
        let ordinal = self.allocate_node();
        let identity = identity_for(ordinal);
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal: command,
            node: identity,
            command: None,
        });
        self.nodes.push(SettlementNode {
            ordinal,
            identity,
            kind,
            caused_by,
            depends_on: caused_by.into_iter().collect(),
            bundle_parent,
            produced_pips: Vec::new(),
            spent_pips: Vec::new(),
            journal_ordinals: vec![command],
        });
        Ok(identity)
    }

    fn append_command(
        &mut self,
        node: RulesExecutionNodeRef,
        command: ResolvedRulesCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        let node_index = self.node_index(node)?;
        let ordinal = self.allocate_command();
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal,
            node,
            command: Some(command),
        });
        self.nodes[node_index].journal_ordinals.push(ordinal);
        Ok(ordinal)
    }

    fn ensure_command_capacity(&self) -> Result<(), ResolvedRulesJournalError> {
        self.ensure_command_capacity_for(1)
    }

    fn ensure_command_capacity_for(&self, count: u64) -> Result<(), ResolvedRulesJournalError> {
        (self.next_command_ordinal <= u64::MAX.saturating_sub(count))
            .then_some(())
            .ok_or(ResolvedRulesJournalError::CommandOrdinalOverflow)
    }

    fn ensure_node_capacity(&self) -> Result<(), ResolvedRulesJournalError> {
        (self.next_settlement_node_ordinal != u64::MAX)
            .then_some(())
            .ok_or(ResolvedRulesJournalError::SettlementNodeOrdinalOverflow)
    }

    fn allocate_command(&mut self) -> ResolvedCommandOrdinal {
        let ordinal = ResolvedCommandOrdinal(self.next_command_ordinal);
        self.next_command_ordinal += 1;
        ordinal
    }

    fn allocate_node(&mut self) -> SettlementNodeOrdinal {
        let ordinal = SettlementNodeOrdinal(self.next_settlement_node_ordinal);
        self.next_settlement_node_ordinal += 1;
        ordinal
    }

    fn node_index(
        &self,
        identity: RulesExecutionNodeRef,
    ) -> Result<usize, ResolvedRulesJournalError> {
        self.nodes
            .iter()
            .position(|node| node.identity == identity)
            .ok_or(ResolvedRulesJournalError::UnknownNode(identity))
    }

    fn bundle_owner(
        &self,
        identity: RulesExecutionNodeRef,
    ) -> Result<Option<RulesExecutionNodeRef>, ResolvedRulesJournalError> {
        let node = &self.nodes[self.node_index(identity)?];
        Ok(node.bundle_parent.or(Some(identity)))
    }

    fn spent_pip_exists(&self, pip: ManaPipId) -> bool {
        self.spent_mana
            .iter()
            .any(|record| record.unit.pip_id == pip)
    }

    fn require_stamped(pip: ManaPipId) -> Result<(), ResolvedRulesJournalError> {
        (pip.0 != 0)
            .then_some(())
            .ok_or(ResolvedRulesJournalError::UnstampedManaPip)
    }

    fn validate_serialized_authority(&self) -> Result<(), ResolvedRulesJournalError> {
        if self.next_command_ordinal != self.entries.len() as u64
            || self.next_settlement_node_ordinal != self.nodes.len() as u64
        {
            return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                "allocator is not contiguous with its records".to_string(),
            ));
        }
        for (expected, entry) in self.entries.iter().enumerate() {
            if entry.ordinal != ResolvedCommandOrdinal(expected as u64) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "command entries are duplicate or nonmonotonic".to_string(),
                ));
            }
        }
        for (expected, node) in self.nodes.iter().enumerate() {
            if node.ordinal != SettlementNodeOrdinal(expected as u64)
                || !identity_matches_kind(node.identity, &node.kind)
            {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "settlement node identity is duplicate, nonmonotonic, or mismatched"
                        .to_string(),
                ));
            }
            if has_duplicate_values(&node.journal_ordinals)
                || has_duplicate_values(&node.produced_pips)
                || has_duplicate_values(&node.spent_pips)
            {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "node metadata contains duplicate identities".to_string(),
                ));
            }
            for dependency in node
                .caused_by
                .iter()
                .chain(node.depends_on.iter())
                .chain(node.bundle_parent.iter())
            {
                let dependency_index = self.node_index(*dependency).map_err(|_| {
                    ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "node references an unknown dependency".to_string(),
                    )
                })?;
                if dependency_index >= expected {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "node depends on a non-prior node".to_string(),
                    ));
                }
            }
        }
        for entry in &self.entries {
            let node = self.node_index(entry.node).map_err(|_| {
                ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "command entry references an unknown node".to_string(),
                )
            })?;
            if !self.nodes[node].journal_ordinals.contains(&entry.ordinal) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "command entry is absent from node metadata".to_string(),
                ));
            }
        }
        let mut inserted_command_pips = HashSet::new();
        let mut spent_command_pips = HashSet::new();
        for entry in &self.entries {
            let Some(command) = &entry.command else {
                continue;
            };
            self.validate_resolved_command(entry, command)?;
            match command {
                ResolvedRulesCommand::ManaInsert(command) => {
                    if !inserted_command_pips.insert(command.unit.pip_id) {
                        return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                            "duplicate mana-insert command pip".to_string(),
                        ));
                    }
                }
                ResolvedRulesCommand::ManaSpend(command) => {
                    for spent in &command.units {
                        if !spent_command_pips.insert(spent.unit.pip_id) {
                            return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                                "duplicate mana-spend command pip".to_string(),
                            ));
                        }
                    }
                }
                ResolvedRulesCommand::PlayerEdit(_)
                | ResolvedRulesCommand::ObjectStatus(_)
                | ResolvedRulesCommand::ObjectCounter(_)
                | ResolvedRulesCommand::Information(_)
                | ResolvedRulesCommand::LedgerEdit(_)
                | ResolvedRulesCommand::LibraryShuffle(_)
                | ResolvedRulesCommand::ZoneChange(_)
                | ResolvedRulesCommand::FrameTransition(_)
                | ResolvedRulesCommand::TriggerCollection(_) => {}
            }
        }
        for node in &self.nodes {
            for ordinal in &node.journal_ordinals {
                if !self
                    .entries
                    .iter()
                    .any(|entry| entry.ordinal == *ordinal && entry.node == node.identity)
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "node metadata references an unrelated journal entry".to_string(),
                    ));
                }
            }
        }

        let mut produced_pips = HashSet::new();
        for record in &self.produced_mana {
            Self::require_stamped(record.unit.pip_id)?;
            if !produced_pips.insert(record.unit.pip_id) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "duplicate produced mana pip".to_string(),
                ));
            }
            let node = self.node_index(record.producer).map_err(|_| {
                ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "produced mana references unknown node".to_string(),
                )
            })?;
            if !self.nodes[node].produced_pips.contains(&record.unit.pip_id) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "produced mana is absent from node metadata".to_string(),
                ));
            }
        }
        for node in &self.nodes {
            if node.produced_pips.iter().any(|pip| {
                self.produced_mana
                    .iter()
                    .filter(|record| record.producer == node.identity)
                    .all(|record| record.unit.pip_id != *pip)
            }) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "node metadata references unrecorded produced mana".to_string(),
                ));
            }
        }
        let mut spent_pips = HashSet::new();
        for record in &self.spent_mana {
            Self::require_stamped(record.unit.pip_id)?;
            if !spent_pips.insert(record.unit.pip_id) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "duplicate spent mana pip".to_string(),
                ));
            }
            let Some(produced) = self
                .produced_mana
                .iter()
                .find(|item| item.unit.pip_id == record.unit.pip_id)
            else {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana has no producer".to_string(),
                ));
            };
            let payment = self.node_index(record.payment).map_err(|_| {
                ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana references unknown payment".to_string(),
                )
            })?;
            let RulesExecutionNodeKind::Payment { recipient, .. } = &self.nodes[payment].kind
            else {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana references a non-payment node".to_string(),
                ));
            };
            if produced.producer != record.producer
                || produced.unit != record.unit
                || *recipient != record.recipient
                || !self.nodes[payment].spent_pips.contains(&record.unit.pip_id)
            {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "spent mana disagrees with recorded provenance".to_string(),
                ));
            }
        }
        for node in &self.nodes {
            if node.spent_pips.iter().any(|pip| {
                self.spent_mana
                    .iter()
                    .filter(|record| record.payment == node.identity)
                    .all(|record| record.unit.pip_id != *pip)
            }) {
                return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                    "node metadata references unrecorded spent mana".to_string(),
                ));
            }
        }
        Ok(())
    }

    fn validate_resolved_command(
        &self,
        entry: &ResolvedCommandJournalEntry,
        command: &ResolvedRulesCommand,
    ) -> Result<(), ResolvedRulesJournalError> {
        match command {
            ResolvedRulesCommand::ManaInsert(command) => {
                Self::require_stamped(command.unit.pip_id)?;
                if entry.node != command.producer
                    || !self.produced_mana.iter().any(|record| {
                        record.producer == command.producer
                            && exact_mana_unit_eq(&record.unit, &command.unit)
                    })
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-insert command disagrees with produced mana".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ManaSpend(command) => {
                if command.units.is_empty() || entry.node != command.payment {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command has an empty or unrelated payment".to_string(),
                    ));
                }
                let payment = self.node_index(command.payment).map_err(|_| {
                    ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command references an unknown payment".to_string(),
                    )
                })?;
                let RulesExecutionNodeKind::Payment { payer, recipient } =
                    &self.nodes[payment].kind
                else {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command references a non-payment node".to_string(),
                    ));
                };
                if *payer != command.payer || *recipient != command.recipient {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command disagrees with payment metadata".to_string(),
                    ));
                }
                let records: Vec<&SpentManaUnit> = self
                    .spent_mana
                    .iter()
                    .filter(|record| record.payment == command.payment)
                    .collect();
                if records.len() != command.units.len()
                    || records.iter().zip(&command.units).any(|(record, spent)| {
                        record.producer != spent.producer
                            || !exact_mana_unit_eq(&record.unit, &spent.unit)
                            || record.recipient != command.recipient
                    })
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command disagrees with spent mana".to_string(),
                    ));
                }
                let mut pips = HashSet::new();
                if command
                    .units
                    .iter()
                    .any(|spent| spent.unit.pip_id.0 == 0 || !pips.insert(spent.unit.pip_id))
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "mana-spend command has duplicate or unstamped pips".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::PlayerEdit(command) => {
                if entry.node != command.cause || player_edit_is_empty(&command.edit) {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "player command has an empty edit or unrelated cause".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ObjectStatus(command) => {
                if entry.node != command.cause || command.expected_old == command.new {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "object-status command has a no-op transition or unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ObjectCounter(command) => {
                if entry.node != command.cause || object_counter_edit_is_empty(&command.edit) {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "object-counter command has an empty edit or unrelated cause".to_string(),
                    ));
                }
                if command.object.incarnation == LEGACY_INCARNATION {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "object-counter command cannot use a legacy object identity".to_string(),
                    ));
                }
                if let ResolvedObjectCounterEdit::Remove { count } = &command.edit {
                    if *count > command.expected_old {
                        return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                            "object-counter removal has an impossible predecessor".to_string(),
                        ));
                    }
                }
            }
            ResolvedRulesCommand::Information(command) => {
                if entry.node != command.cause || information_command_is_invalid(command) {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "information command has an invalid occurrence, audience, lifetime, or cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::LedgerEdit(command) => {
                if entry.node != command.cause
                    || ledger_edit_is_invalid(&command.edit)
                    || ledger_edit_has_legacy_object_identity(&command.edit)
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "ledger command has an impossible edit, legacy identity, or unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::LibraryShuffle(command) => {
                if entry.node != command.cause || validate_library_shuffle_receipt(command).is_err()
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "library shuffle command has an invalid receipt or unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ZoneChange(command) => {
                if entry.node != command.cause || zone_change_command_is_invalid(command) {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "zone-change command has an invalid occurrence, receipt, or unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::FrameTransition(command) => {
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "frame-transition command has an unrelated cause".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::TriggerCollection(command) => {
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "trigger-collection command has an unrelated cause".to_string(),
                    ));
                }
            }
        }
        Ok(())
    }
}

fn identity_matches_kind(identity: RulesExecutionNodeRef, kind: &RulesExecutionNodeKind) -> bool {
    matches!(
        (identity, kind),
        (
            RulesExecutionNodeRef::Proposal(_),
            RulesExecutionNodeKind::Proposal
        ) | (
            RulesExecutionNodeRef::ActivatedMana(_),
            RulesExecutionNodeKind::ActivatedMana { .. }
        ) | (
            RulesExecutionNodeRef::TriggeredMana(_),
            RulesExecutionNodeKind::TriggeredMana { .. }
        ) | (
            RulesExecutionNodeRef::Payment(_),
            RulesExecutionNodeKind::Payment { .. }
        ) | (
            RulesExecutionNodeRef::PlayerLeave(_),
            RulesExecutionNodeKind::PlayerLeave
        )
    )
}

fn has_duplicate_values<T: Eq + std::hash::Hash>(values: &[T]) -> bool {
    let mut seen = HashSet::new();
    values.iter().any(|value| !seen.insert(value))
}

fn exact_mana_unit_eq(left: &ManaUnit, right: &ManaUnit) -> bool {
    left.pip_id == right.pip_id && left == right
}

fn player_edit_is_empty(edit: &ResolvedPlayerEdit) -> bool {
    match edit {
        ResolvedPlayerEdit::Life { delta }
        | ResolvedPlayerEdit::Energy { delta }
        | ResolvedPlayerEdit::Counter { delta, .. } => *delta == 0,
        ResolvedPlayerEdit::Speed { old, new } => old == new,
    }
}

fn object_counter_edit_is_empty(edit: &ResolvedObjectCounterEdit) -> bool {
    match edit {
        ResolvedObjectCounterEdit::Add { count, .. }
        | ResolvedObjectCounterEdit::Remove { count } => *count == 0,
    }
}

fn information_command_is_invalid(command: &ResolvedInformationCommand) -> bool {
    let valid_lifetime = matches!(
        (command.audience, command.lifetime),
        (
            ResolvedInformationAudience::Controller(_),
            ResolvedInformationLifetime::UntilActionBoundary
        ) | (
            ResolvedInformationAudience::Public,
            ResolvedInformationLifetime::UntilZoneChange
        )
    );
    let mut object_ids = HashSet::new();
    command.occurrences.is_empty()
        || !valid_lifetime
        || command.occurrences.iter().any(|occurrence| {
            occurrence.incarnation == LEGACY_INCARNATION || !object_ids.insert(occurrence.object_id)
        })
}

fn zone_change_command_is_invalid(command: &ResolvedZoneChangeCommand) -> bool {
    let record = &command.zone_change_record;
    let changes_incarnation = command.from != command.to;
    command.object.incarnation == LEGACY_INCARNATION
        || command.owner != record.owner
        || record.object_id != command.object.object_id
        || record.from_zone != Some(command.from)
        || record.to_zone != command.to
        || record.turn_zone_change_index != command.turn_zone_change_index
        || (changes_incarnation && command.resulting_incarnation <= command.object.incarnation)
        || (!changes_incarnation && command.resulting_incarnation != command.object.incarnation)
        || (command.to == Zone::Battlefield) != command.entry_timestamp.is_some()
        || (command.to == Zone::Battlefield
            && record.entered_incarnation != Some(command.resulting_incarnation))
        || (command.to != Zone::Battlefield && record.entered_incarnation.is_some())
}

pub(crate) fn ledger_edit_is_invalid(edit: &ResolvedLedgerEdit) -> bool {
    match edit {
        ResolvedLedgerEdit::SpellCast {
            expected_game_count,
            expected_turn_history_len,
            expected_game_history_len,
            ..
        } => {
            // `expected_turn_count` is a u8 advanced via saturating_add in the
            // applier, so 255 is a legitimate saturated value, not a reserved
            // sentinel — only the u32 count fields carry the u32::MAX
            // "never recorded" marker this pre-screen fails closed on.
            *expected_game_count == u32::MAX
                || *expected_turn_history_len == u32::MAX
                || *expected_game_history_len == u32::MAX
        }
        ResolvedLedgerEdit::AbilityActivated {
            expected_turn_count,
            expected_game_count,
            ..
        } => *expected_turn_count == u32::MAX || *expected_game_count == u32::MAX,
        ResolvedLedgerEdit::CardsDrawn {
            drawn_object,
            attempted_empty_library,
            expected_has_drawn_this_turn,
            resulting_has_drawn_this_turn,
            expected_cards_drawn_this_turn,
            resulting_cards_drawn_this_turn,
            expected_cards_drawn_this_step,
            resulting_cards_drawn_this_step,
            expected_drew_from_empty_library,
            resulting_drew_from_empty_library,
            expected_drawn_cards_len,
            resulting_drawn_cards_len,
            expected_first_card_drawn_this_turn,
            resulting_first_card_drawn_this_turn,
            ..
        } => {
            let settled_card = drawn_object.is_some();
            let expected_first = if let Some(object) = drawn_object {
                expected_first_card_drawn_this_turn.or(Some(object.object_id))
            } else {
                *expected_first_card_drawn_this_turn
            };
            (!settled_card && !attempted_empty_library)
                || *resulting_has_drawn_this_turn
                    != if settled_card {
                        true
                    } else {
                        *expected_has_drawn_this_turn
                    }
                || *resulting_cards_drawn_this_turn
                    != if settled_card {
                        expected_cards_drawn_this_turn.saturating_add(1)
                    } else {
                        *expected_cards_drawn_this_turn
                    }
                || *resulting_cards_drawn_this_step
                    != if settled_card {
                        expected_cards_drawn_this_step.saturating_add(1)
                    } else {
                        *expected_cards_drawn_this_step
                    }
                || *resulting_drew_from_empty_library
                    != (*expected_drew_from_empty_library || *attempted_empty_library)
                || *expected_drawn_cards_len == u32::MAX
                || *resulting_drawn_cards_len
                    != if settled_card {
                        expected_drawn_cards_len + 1
                    } else {
                        *expected_drawn_cards_len
                    }
                || *resulting_first_card_drawn_this_turn != expected_first
        }
        ResolvedLedgerEdit::TriggerFired {
            edit: ResolvedTriggerLedgerEdit::MaxTimesPerTurn { expected_old },
            ..
        } => *expected_old == u32::MAX,
        ResolvedLedgerEdit::TriggerFired { .. }
        | ResolvedLedgerEdit::OncePerTurnPermission { .. } => false,
    }
}

fn ledger_edit_has_legacy_object_identity(edit: &ResolvedLedgerEdit) -> bool {
    matches!(
        edit,
        ResolvedLedgerEdit::TriggerFired { trigger, .. }
            if trigger.source.incarnation == LEGACY_INCARNATION
    ) || matches!(
        edit,
        ResolvedLedgerEdit::CardsDrawn {
            drawn_object: Some(object),
            ..
        } if object.incarnation == LEGACY_INCARNATION
    )
}

/// Validates the closed operands of a library-order entropy receipt.
///
/// CR 701.24a: shuffling can only permute the same exact cards. The recorded
/// stream span can advance or remain unchanged, but never rewind.
pub(crate) fn validate_library_shuffle_receipt(
    command: &ResolvedLibraryShuffleCommand,
) -> Result<(), ResolvedLibraryShuffleReplayInvariantError> {
    if command.post_word_pos < command.pre_word_pos {
        return Err(
            ResolvedLibraryShuffleReplayInvariantError::EntropyReceiptRegression {
                pre: command.pre_word_pos,
                post: command.post_word_pos,
            },
        );
    }
    if command.precondition_order.len() != command.resulting_order.len()
        || has_duplicate_values(&command.precondition_order)
        || has_duplicate_values(&command.resulting_order)
    {
        return Err(ResolvedLibraryShuffleReplayInvariantError::InvalidLibraryOrderReceipt);
    }

    // CR 701.24a: A shuffle of two or more cards draws at least once, so its
    // entropy span must be non-empty. A zero-span multi-card receipt is a
    // corrupt permutation that would leave the RNG cursor unadvanced.
    if command.resulting_order.len() >= 2 && command.post_word_pos == command.pre_word_pos {
        return Err(
            ResolvedLibraryShuffleReplayInvariantError::MultiCardReceiptWithoutEntropy {
                cards: command.resulting_order.len(),
                position: command.pre_word_pos,
            },
        );
    }

    let expected: HashSet<_> = command.precondition_order.iter().copied().collect();
    let resulting: HashSet<_> = command.resulting_order.iter().copied().collect();
    (expected == resulting)
        .then_some(())
        .ok_or(ResolvedLibraryShuffleReplayInvariantError::InvalidLibraryOrderReceipt)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef};
    use crate::types::identifiers::ObjectId;
    use crate::types::mana::{ManaRestriction, ManaType};

    fn unit(pip: u64) -> ManaUnit {
        ManaUnit {
            color: ManaType::Green,
            source_id: ObjectId(9),
            pip_id: ManaPipId(pip),
            supertype: None,
            source_could_produce_two_or_more_colors: false,
            restrictions: vec![ManaRestriction::OnlyForSpell],
            grants: Vec::new(),
            expiry: None,
        }
    }

    #[test]
    fn ordinals_are_monotonic_unique_and_checked() {
        let mut journal = ResolvedRulesJournal::default();
        assert_eq!(
            journal.begin_proposal().unwrap(),
            RulesExecutionNodeRef::Proposal(ResolvedCommandOrdinal(0))
        );
        assert_eq!(
            journal.begin_proposal().unwrap(),
            RulesExecutionNodeRef::Proposal(ResolvedCommandOrdinal(1))
        );
        assert_eq!(journal.next_command_ordinal(), ResolvedCommandOrdinal(2));
        assert_eq!(
            journal.next_settlement_node_ordinal(),
            SettlementNodeOrdinal(2)
        );
        journal.next_command_ordinal = u64::MAX;
        assert_eq!(
            journal.begin_proposal(),
            Err(ResolvedRulesJournalError::CommandOrdinalOverflow)
        );
        let mut nodes = ResolvedRulesJournal {
            next_settlement_node_ordinal: u64::MAX,
            ..ResolvedRulesJournal::default()
        };
        assert_eq!(
            nodes.begin_activated_mana(ObjectIncarnationRef::of(ObjectId(1), 1), None),
            Err(ResolvedRulesJournalError::SettlementNodeOrdinalOverflow)
        );
    }

    #[test]
    fn records_exact_producer_spender_and_trigger_bundle() {
        let mut journal = ResolvedRulesJournal::default();
        let activation = journal
            .begin_activated_mana(ObjectIncarnationRef::of(ObjectId(1), 2), None)
            .unwrap();
        let trigger = journal
            .begin_triggered_mana(
                ObjectIncarnationRef::of(ObjectId(2), 3),
                None,
                Some(activation),
            )
            .unwrap();
        let produced = unit(1);
        journal
            .record_produced_mana(trigger, produced.clone())
            .unwrap();
        let payment = journal
            .record_spent_mana(
                PlayerId(0),
                ManaPaymentRecipient::Object(ObjectIncarnationRef::of(ObjectId(4), 5)),
                std::slice::from_ref(&produced),
            )
            .unwrap()
            .unwrap();
        assert_eq!(journal.spent_mana()[0].unit, produced);
        assert_eq!(journal.spent_mana()[0].producer, trigger);
        assert_eq!(
            journal.spent_mana()[0].unit.restrictions,
            vec![ManaRestriction::OnlyForSpell],
            "spent provenance preserves the produced unit's restrictions"
        );
        let node = journal
            .nodes()
            .iter()
            .find(|node| node.identity == trigger)
            .unwrap();
        assert_eq!(node.caused_by, Some(activation));
        assert_eq!(node.bundle_parent, Some(activation));
        assert_eq!(
            journal
                .nodes()
                .iter()
                .find(|node| node.identity == payment)
                .unwrap()
                .depends_on,
            vec![trigger]
        );
        assert_eq!(
            journal
                .nodes()
                .iter()
                .map(|node| node.journal_ordinals.clone())
                .collect::<Vec<_>>(),
            vec![
                vec![ResolvedCommandOrdinal(0)],
                vec![ResolvedCommandOrdinal(1)],
                vec![ResolvedCommandOrdinal(2)],
            ],
            "each distinct execution node receives a globally ordered journal slot"
        );
        let roundtrip =
            serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(&journal).unwrap())
                .unwrap();
        assert_eq!(roundtrip, journal);
    }

    #[test]
    fn serde_roundtrip_rejects_duplicate_and_nonmonotonic_ordinals() {
        let mut journal = ResolvedRulesJournal::default();
        journal.begin_proposal().unwrap();
        journal.begin_proposal().unwrap();
        let serialized = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serialized.clone()).unwrap(),
            journal
        );
        let mut duplicate = serialized.clone();
        duplicate["entries"][1]["ordinal"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(duplicate).is_err());
        let mut nonmonotonic = serialized;
        nonmonotonic["nodes"][1]["ordinal"] = serde_json::json!(0);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(nonmonotonic).is_err());
    }

    #[test]
    fn semantic_commands_roundtrip_and_reject_malformed_payloads() {
        let mut journal = ResolvedRulesJournal::default();
        let producer = journal.begin_proposal().unwrap();
        let produced = unit(1);
        journal
            .record_mana_insert(ResolvedManaInsertCommand {
                player: PlayerId(0),
                unit: produced.clone(),
                producer,
            })
            .unwrap();
        let spend = journal
            .record_mana_spend(
                PlayerId(0),
                ManaPaymentRecipient::Player(PlayerId(0)),
                std::slice::from_ref(&produced),
            )
            .unwrap()
            .unwrap();
        assert_eq!(spend.units[0].unit, produced);
        assert!(matches!(
            journal.entries()[1].command.as_ref(),
            Some(ResolvedRulesCommand::ManaInsert(_))
        ));
        assert!(matches!(
            journal.entries()[3].command.as_ref(),
            Some(ResolvedRulesCommand::ManaSpend(_))
        ));
        let serialized = serde_json::to_value(&journal).unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serialized).unwrap(),
            journal
        );

        let mut mismatched_insert = journal.clone();
        let Some(ResolvedRulesCommand::ManaInsert(command)) =
            mismatched_insert.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the insert command");
        };
        command.unit.pip_id = ManaPipId(99);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(mismatched_insert).unwrap()
        )
        .is_err());

        let mut duplicate_spend = journal.clone();
        let mut duplicate_entry = duplicate_spend.entries[3].clone();
        duplicate_entry.ordinal = ResolvedCommandOrdinal(4);
        duplicate_spend.entries.push(duplicate_entry);
        duplicate_spend.nodes[1]
            .journal_ordinals
            .push(ResolvedCommandOrdinal(4));
        duplicate_spend.next_command_ordinal = 5;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(duplicate_spend).unwrap()
        )
        .is_err());
    }

    #[test]
    fn scalar_and_object_status_commands_roundtrip_and_reject_malformed_payloads() {
        let mut journal = ResolvedRulesJournal::default();
        let cause = journal.begin_proposal().unwrap();
        journal
            .record_player_edit(ResolvedPlayerEditCommand {
                player: PlayerId(0),
                edit: ResolvedPlayerEdit::Life { delta: -3 },
                cause,
            })
            .unwrap();
        journal
            .record_object_status(ResolvedObjectStatusCommand {
                object: ObjectIncarnationRef::of(ObjectId(9), 0),
                status: ResolvedObjectStatus::Tapped,
                expected_old: false,
                new: true,
                cause,
            })
            .unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(&journal).unwrap())
                .unwrap(),
            journal
        );

        let mut empty_player_edit = journal.clone();
        let Some(ResolvedRulesCommand::PlayerEdit(command)) =
            empty_player_edit.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the player edit");
        };
        command.edit = ResolvedPlayerEdit::Energy { delta: 0 };
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(empty_player_edit).unwrap()
        )
        .is_err());

        let mut no_op_status = journal.clone();
        let Some(ResolvedRulesCommand::ObjectStatus(command)) =
            no_op_status.entries[2].command.as_mut()
        else {
            panic!("entry 2 must be the object-status edit");
        };
        command.new = false;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(no_op_status).unwrap()
        )
        .is_err());

        let mut unrelated_cause = journal.clone();
        let Some(ResolvedRulesCommand::PlayerEdit(command)) =
            unrelated_cause.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the player edit");
        };
        command.cause = RulesExecutionNodeRef::Payment(SettlementNodeOrdinal(99));
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(unrelated_cause).unwrap()
        )
        .is_err());
    }

    #[test]
    fn library_shuffle_command_roundtrips_and_rejects_invalid_receipts() {
        let mut journal = ResolvedRulesJournal::default();
        let cause = journal.begin_proposal().unwrap();
        journal
            .record_library_shuffle(ResolvedLibraryShuffleCommand {
                player: PlayerId(0),
                precondition_order: vec![ObjectId(1), ObjectId(2), ObjectId(3)],
                resulting_order: vec![ObjectId(3), ObjectId(1), ObjectId(2)],
                pre_word_pos: 7,
                post_word_pos: 11,
                cause,
            })
            .unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(&journal).unwrap())
                .unwrap(),
            journal
        );

        let mut missing_entropy = serde_json::to_value(&journal).unwrap();
        missing_entropy["entries"][1]["command"]["LibraryShuffle"]
            .as_object_mut()
            .unwrap()
            .remove("post_word_pos");
        assert!(serde_json::from_value::<ResolvedRulesJournal>(missing_entropy).is_err());

        let mut duplicate_card = journal.clone();
        let Some(ResolvedRulesCommand::LibraryShuffle(command)) =
            duplicate_card.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the library shuffle");
        };
        command.resulting_order = vec![ObjectId(3), ObjectId(3), ObjectId(2)];
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(duplicate_card).unwrap()
        )
        .is_err());

        let mut backwards_entropy = journal.clone();
        let Some(ResolvedRulesCommand::LibraryShuffle(command)) =
            backwards_entropy.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the library shuffle");
        };
        command.post_word_pos = 6;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(backwards_entropy).unwrap()
        )
        .is_err());

        // CR 701.24a: a three-card permutation with an empty entropy span could
        // not have come from a real shuffle and must be rejected.
        let mut zero_span_multi_card = journal.clone();
        let Some(ResolvedRulesCommand::LibraryShuffle(command)) =
            zero_span_multi_card.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the library shuffle");
        };
        command.post_word_pos = command.pre_word_pos;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(zero_span_multi_card).unwrap()
        )
        .is_err());
    }

    #[test]
    fn information_commands_roundtrip_and_reject_malformed_payloads() {
        let mut journal = ResolvedRulesJournal::default();
        let cause = journal.begin_proposal().unwrap();
        let occurrence = ObjectIncarnationRef::of(ObjectId(9), 2);
        journal
            .record_information(ResolvedInformationCommand {
                occurrences: vec![occurrence],
                audience: ResolvedInformationAudience::Controller(PlayerId(0)),
                lifetime: ResolvedInformationLifetime::UntilActionBoundary,
                edit: ResolvedInformationEdit::Reveal,
                cause,
            })
            .unwrap();
        journal
            .record_information(ResolvedInformationCommand {
                occurrences: vec![occurrence],
                audience: ResolvedInformationAudience::Public,
                lifetime: ResolvedInformationLifetime::UntilZoneChange,
                edit: ResolvedInformationEdit::Reveal,
                cause,
            })
            .unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(&journal).unwrap())
                .unwrap(),
            journal
        );

        let mut empty = journal.clone();
        let Some(ResolvedRulesCommand::Information(command)) = empty.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the controller information command");
        };
        command.occurrences.clear();
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(empty).unwrap()
        )
        .is_err());

        let mut invalid_lifetime = journal.clone();
        let Some(ResolvedRulesCommand::Information(command)) =
            invalid_lifetime.entries[2].command.as_mut()
        else {
            panic!("entry 2 must be the public information command");
        };
        command.lifetime = ResolvedInformationLifetime::UntilActionBoundary;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(invalid_lifetime).unwrap()
        )
        .is_err());

        let mut legacy_occurrence = journal.clone();
        let Some(ResolvedRulesCommand::Information(command)) =
            legacy_occurrence.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the controller information command");
        };
        command.occurrences[0].incarnation = LEGACY_INCARNATION;
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(legacy_occurrence).unwrap()
        )
        .is_err());

        let mut missing_lifetime = serde_json::to_value(&journal).unwrap();
        missing_lifetime["entries"][1]["command"]["Information"]
            .as_object_mut()
            .unwrap()
            .remove("lifetime");
        assert!(serde_json::from_value::<ResolvedRulesJournal>(missing_lifetime).is_err());
    }

    #[test]
    fn counter_and_ledger_commands_roundtrip_and_reject_malformed_payloads() {
        let mut journal = ResolvedRulesJournal::default();
        let cause = journal.begin_proposal().unwrap();
        journal
            .record_object_counter(ResolvedObjectCounterCommand {
                object: ObjectIncarnationRef::of(ObjectId(9), 0),
                counter_type: CounterType::Plus1Plus1,
                expected_old: 2,
                edit: ResolvedObjectCounterEdit::Add {
                    actor: PlayerId(0),
                    count: 1,
                },
                cause,
            })
            .unwrap();
        journal
            .record_ledger_edit(ResolvedLedgerEditCommand {
                edit: ResolvedLedgerEdit::AbilityActivated {
                    source: ObjectId(9),
                    ability_index: 0,
                    expected_turn_count: 0,
                    expected_game_count: 0,
                },
                cause,
            })
            .unwrap();
        journal
            .record_ledger_edit(ResolvedLedgerEditCommand {
                edit: ResolvedLedgerEdit::TriggerFired {
                    trigger: TriggerDefinitionRef {
                        source: ObjectIncarnationRef::of(ObjectId(10), 0),
                        occurrence: TriggerDefinitionOccurrenceRef::Printed {
                            base_set: TriggerBaseSetInstanceRef::INITIAL,
                            printed_index: 0,
                        },
                    },
                    edit: ResolvedTriggerLedgerEdit::OncePerTurn,
                },
                cause,
            })
            .unwrap();
        assert_eq!(
            serde_json::from_value::<ResolvedRulesJournal>(serde_json::to_value(&journal).unwrap())
                .unwrap(),
            journal
        );

        let mut empty_counter = journal.clone();
        let Some(ResolvedRulesCommand::ObjectCounter(command)) =
            empty_counter.entries[1].command.as_mut()
        else {
            panic!("entry 1 must be the counter command");
        };
        command.edit = ResolvedObjectCounterEdit::Add {
            actor: PlayerId(0),
            count: 0,
        };
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(empty_counter).unwrap()
        )
        .is_err());

        // A pre-incarnation bare object id deserializes to LEGACY_INCARNATION.
        // It is valid only for its original compatibility readers, never for a
        // new executable command whose applier requires an exact occurrence.
        let mut legacy_counter = serde_json::to_value(&journal).unwrap();
        legacy_counter["entries"][1]["command"]["ObjectCounter"]["object"] = serde_json::json!(9);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(legacy_counter).is_err());

        let mut legacy_trigger = serde_json::to_value(&journal).unwrap();
        legacy_trigger["entries"][3]["command"]["LedgerEdit"]["edit"]["TriggerFired"]["trigger"]
            ["source"] = serde_json::json!(10);
        assert!(serde_json::from_value::<ResolvedRulesJournal>(legacy_trigger).is_err());

        let mut impossible_ledger = journal.clone();
        let Some(ResolvedRulesCommand::LedgerEdit(command)) =
            impossible_ledger.entries[2].command.as_mut()
        else {
            panic!("entry 2 must be the ledger command");
        };
        command.edit = ResolvedLedgerEdit::AbilityActivated {
            source: ObjectId(9),
            ability_index: 0,
            expected_turn_count: u32::MAX,
            expected_game_count: 0,
        };
        assert!(serde_json::from_value::<ResolvedRulesJournal>(
            serde_json::to_value(impossible_ledger).unwrap()
        )
        .is_err());
    }
}

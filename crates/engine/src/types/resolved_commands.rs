//! Append-only, identity-bearing records for resolved rules work.
//!
//! P1 established provenance and ordering. P2 makes mana insert and exact
//! spend commands executable through their owning authority appliers.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::game::combat::{AttackTarget, CombatParticipation};
use crate::game::game_object::AttachTarget;
use crate::game::triggers::{ConsumedTriggerEventOccurrence, PendingTriggerContext};

use super::ability::{ContinuousModification, TriggerDefinitionRef};
use super::card::TokenImageRef;
use super::card_type::CoreType;
use super::counter::CounterType;
use super::game_state::{
    DelayedTrigger, SpellCastRecord, StackEntry, StackEntryKind, StackPaidSnapshot,
    TransientContinuousEffect, ZoneChangeRecord,
};
use super::identifiers::{
    DelayedTriggerInstanceId, DelayedTriggerToken, ObjectId, ObjectIncarnationRef, TriggerFiring,
    LEGACY_INCARNATION,
};
use super::mana::{ManaPipId, ManaUnit};
use super::player::{PlayerCounterKind, PlayerId};
use super::proposed_event::{CopyTokenSpec, TokenSpec};
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

/// One exact CR 701.27a transform of a double-faced permanent.
///
/// CR 613.7g: a permanent that transforms receives a NEW timestamp, which
/// orders it against continuous effects in the layer system. Replay installs
/// the exact recorded timestamp instead of re-drawing one from
/// `GameState::next_timestamp` — mirroring the zone-change family's
/// `entry_timestamp` — so a retained-prefix replay cannot silently reorder
/// layer application. The face payloads are not recorded: swapping the stashed
/// `back_face` with the displayed face is a structural operation over data the
/// object already carries, not a re-selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedObjectTransformCommand {
    pub object: ObjectIncarnationRef,
    pub expected_old_transformed: bool,
    pub resulting_transformed: bool,
    pub resulting_timestamp: u64,
    /// CR 701.27f: the post-transform count used to ignore stale self-transform
    /// instructions from abilities already on the stack.
    pub resulting_transformation_count: u32,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved transform.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedObjectTransformReplayInvariantError {
    #[error("transform command references an unknown object {0:?}")]
    UnknownObject(ObjectId),
    #[error("transform occurrence mismatch: expected {expected:?}, found {found:?}")]
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("transform precondition mismatch: expected transformed {expected}, found {found}")]
    TransformedPreconditionMismatch { expected: bool, found: bool },
    #[error("transform command object {0:?} has no back face to swap")]
    MissingBackFace(ObjectId),
}

/// One exact CR 701.3 attachment-graph edit.
///
/// The three production authorities — `attach_to`, `attach_to_player`, and
/// `unattach` — perform the same graph mutation parameterized by the resulting
/// host, so they share one command instead of three sibling variants:
/// `Some(Object)` (CR 301.5 / CR 303.4f), `Some(Player)` (CR 303.4), and `None`
/// (CR 701.3d unattach) are leaf values of the `Option<AttachTarget>` the object
/// already stores.
///
/// CR 613.7e + CR 701.3c: attaching to a DIFFERENT host draws a new timestamp,
/// which orders the attachment against continuous effects in the layer system; a
/// same-host re-attach (CR 701.3b) and an unattach draw none. `resulting_timestamp`
/// is therefore `Some` exactly when the authority drew one, and replay installs
/// that value — mirroring the transform and zone-change families — instead of
/// re-drawing from `GameState::next_timestamp` and silently reordering layers.
///
/// The host-side `attachments` list is not recorded: removing the attachment from
/// its old host and pushing it onto the new one is a structural consequence of the
/// recorded host transition, not a re-selection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedAttachmentCommand {
    pub attachment: ObjectIncarnationRef,
    pub expected_old_host: Option<AttachTarget>,
    pub resulting_host: Option<AttachTarget>,
    pub resulting_timestamp: Option<u64>,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved attachment edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedAttachmentReplayInvariantError {
    #[error("attachment command references an unknown object {0:?}")]
    UnknownAttachment(ObjectId),
    #[error("attachment occurrence mismatch: expected {expected:?}, found {found:?}")]
    StaleAttachment {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("attachment host precondition mismatch: expected {expected:?}, found {found:?}")]
    HostPreconditionMismatch {
        expected: Option<AttachTarget>,
        found: Option<AttachTarget>,
    },
    #[error("attachment command references an unknown host object {0:?}")]
    UnknownHost(ObjectId),
}

/// One exact CR 603.7 delayed-triggered-ability installation.
///
/// CR 603.7a: the ability is created during the resolution of a spell or
/// ability, as the result of a replacement effect, or from a static ability that
/// let a player take an action — never re-derived from the board. The whole
/// `DelayedTrigger` is therefore recorded verbatim: its condition, its already
/// bound `ResolvedAbility` (targets included, per CR 603.7c), its CR 603.7d/e
/// controller, and its source. Replay installs those values; it never re-runs
/// target selection or re-reads the source object.
///
/// `expected_installed_count` is the length of `GameState::delayed_triggers`
/// immediately before the push. Installed triggers are consumed by
/// `check_delayed_triggers` (CR 603.7b, one firing) and pruned at cleanup, so
/// the live length at install time is a genuine function of everything the
/// replayed prefix did. Verifying it fails a replay closed the moment journal
/// order stops matching execution order. (It is a storage-position check only:
/// the rules order in which simultaneously firing triggers reach the stack is
/// chosen by their controller under CR 603.3b, not by this index.)
///
/// No allocator value is drawn: unlike a continuous effect (CR 613.7b) a
/// delayed triggered ability takes no timestamp, because it does not
/// participate in the CR 613 layer system until it actually triggers and goes
/// on the stack as an ordinary triggered ability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedDelayedTriggerCommand {
    pub trigger: DelayedTrigger,
    /// CR 603.7: The exact installation identity, minted live and replayed verbatim.
    #[serde(default)]
    pub token: DelayedTriggerToken,
    pub expected_installed_count: usize,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved delayed-trigger install.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedDelayedTriggerReplayInvariantError {
    #[error("delayed-trigger install precondition mismatch: expected {expected} already installed, found {found}")]
    InstalledCountPreconditionMismatch { expected: usize, found: usize },
    #[error("delayed-trigger install command {token:?} has no provenance")]
    MissingProvenance { token: DelayedTriggerToken },
    #[error(
        "delayed-trigger provenance token {provenance:?} does not match command token {command:?}"
    )]
    ProvenanceTokenMismatch {
        command: DelayedTriggerToken,
        provenance: DelayedTriggerToken,
    },
    #[error("delayed-trigger provenance source {provenance:?} does not match trigger source {trigger:?}")]
    ProvenanceSourceMismatch {
        trigger: ObjectId,
        provenance: ObjectId,
    },
    #[error("delayed-trigger provenance must use nonzero token and instance")]
    ZeroProvenance,
    #[error("delayed-trigger provenance token {token:?} is already installed")]
    DuplicateProvenanceToken { token: DelayedTriggerToken },
    #[error("delayed-trigger provenance instance {instance:?} is already installed")]
    DuplicateProvenanceInstance { instance: DelayedTriggerInstanceId },
}

/// One exact CR 611.2a transient continuous-effect installation.
///
/// A continuous effect generated by the resolution of a spell or ability lasts
/// as long as that spell or ability stated (CR 611.2a) and, per CR 611.2c, the
/// set of objects it affects is fixed when it begins. Both of those decisions
/// are already baked into the `TransientContinuousEffect` the authority built,
/// so the effect is recorded whole rather than as a recipe to re-evaluate.
///
/// Two allocator draws live inside that value and MUST be installed rather than
/// re-drawn:
/// - `effect.timestamp`, taken from `GameState::next_timestamp` per CR 613.7b
///   ("a continuous effect generated by the resolution of a spell or ability
///   receives a timestamp at the time it's created"). Re-drawing it at replay
///   would reorder the effect against every other effect in its CR 613 layer.
/// - `effect.id`, taken from `GameState::next_continuous_effect_id`, which is
///   the handle later duration/recipient binding addresses the effect by.
///
/// All allocator draws are carried as post-draw high-water marks
/// (`resulting_next_continuous_effect_id`,
/// `resulting_next_end_effect_group_id`, and `resulting_next_timestamp`) so
/// replay advances the allocators past the installed values exactly the way the
/// token-birth family advances `next_object_id`, rather than leaving a
/// replayed state that would hand the same identity or timestamp out twice.
///
/// `expected_installed_count` mirrors the delayed-trigger command: the live
/// length of `GameState::transient_continuous_effects` before the push, which
/// duration expiry continuously shortens.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedContinuousEffectCommand {
    pub effect: TransientContinuousEffect,
    pub expected_installed_count: usize,
    pub resulting_next_continuous_effect_id: u64,
    /// CR 116.2c: post-draw high-water for the optional termination group
    /// carried by `effect`. Defaults to `0` for journals written before
    /// pay-to-end permissions existed; those commands cannot carry a group.
    #[serde(default)]
    pub resulting_next_end_effect_group_id: u64,
    pub resulting_next_timestamp: u64,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved continuous-effect install.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedContinuousEffectReplayInvariantError {
    #[error("continuous-effect install precondition mismatch: expected {expected} already installed, found {found}")]
    InstalledCountPreconditionMismatch { expected: usize, found: usize },
    /// Engine invariant (not a CR rule): `id` is the handle every later lookup
    /// addresses an effect by — recipient binding, duration binding, expiry — so
    /// two live effects sharing one id would be indistinguishable to all of them.
    #[error("continuous-effect install would duplicate the live effect id {0}")]
    DuplicateEffectId(u64),
    #[error("continuous-effect id {id} is not below its recorded high-water {high_water}")]
    IdAboveHighWater { id: u64, high_water: u64 },
    #[error(
        "continuous-effect termination group {group} is not below its recorded high-water {high_water}"
    )]
    EndEffectGroupAboveHighWater { group: u64, high_water: u64 },
    #[error(
        "continuous-effect timestamp {timestamp} is not below its recorded high-water {high_water}"
    )]
    TimestampAboveHighWater { timestamp: u64, high_water: u64 },
}
/// One exact effect-driven combat-membership edit (CR 506.3 / CR 506.4).
///
/// The five production authorities — `enter_attacking`,
/// `place_attacking_alongside`, `place_blocking`, `mark_attacker_blocked`, and
/// `remove_object_from_combat` — all edit the one membership structure
/// (`CombatState.attackers` plus the two blocker maps), so they share a single
/// parameterized command rather than five sibling variants.
///
/// The parameterization axis stays inside one CR section, as the categorical
/// boundary rule requires: CR 506.3a-g govern putting a permanent onto the
/// battlefield "attacking or blocking" in one breath, CR 506.4 governs removal,
/// and the declaration-side rules delegate back to it — CR 509.1g ends with
/// "See rule 506.4." CR 508.4 and CR 509.1g/h are the entry points; CR 506 is
/// the section that actually defines membership.
///
/// Turn-based declaration (CR 508.1 / CR 509.1) is deliberately NOT part of this
/// family: it does not use the stack and validates before mutating.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedCombatMembershipCommand {
    pub object: ObjectIncarnationRef,
    pub edit: ResolvedCombatMembershipEdit,
    pub cause: RulesExecutionNodeRef,
}

/// Which membership edit one [`ResolvedCombatMembershipCommand`] settled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedCombatMembershipEdit {
    /// CR 508.4: the object became an attacking creature.
    ///
    /// CR 508.4 assigns the defender to a CHOICE ("its controller chooses which
    /// defending player, planeswalker a defending player controls, or battle a
    /// defending player protects it's attacking"). The resolve-time authority
    /// derives it from ambient state — `state.current_trigger_event`, the
    /// source's own attacker entry, and a controller-scan of the live attacker
    /// list. None of that is reconstructible at replay time, so both halves of
    /// the chosen pair are RECORDED and installed verbatim. Re-deriving would
    /// silently seat the creature against a different defender.
    Attack {
        resulting_defending_player: PlayerId,
        resulting_attack_target: AttackTarget,
    },
    /// CR 509.1g + CR 506.3e: the object became a blocking creature for the
    /// recorded attacker, which becomes blocked per CR 509.1h.
    ///
    /// `expected_attacker_blocked` pins the attacker's sticky blocked bit as it
    /// stood before this block, so a replay installing a second blocker onto an
    /// already-blocked attacker is distinguishable from the first one.
    Block {
        resulting_attacker: ObjectId,
        expected_attacker_blocked: bool,
    },
    /// CR 509.1h: the object became a blocked creature purely by effect, with no
    /// blocking creature assigned. Recorded only on a false-to-true transition,
    /// so the applier can require the bit is still clear.
    MarkBlocked,
    /// CR 506.4: the object stopped being an attacking, blocking, and/or blocked
    /// creature. Records the exact roles it held so the applier can verify it is
    /// pruning the same edges, then re-runs the structural prune — which is a
    /// consequence of the recorded participation, not a re-selection.
    Remove {
        expected_participation: CombatParticipation,
    },
}

/// Typed failure while applying one already-resolved combat-membership edit.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedCombatMembershipReplayInvariantError {
    #[error("combat-membership command references an unknown object {0:?}")]
    UnknownObject(ObjectId),
    #[error("combat-membership occurrence mismatch: expected {expected:?}, found {found:?}")]
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("combat-membership command applies to a state with no combat")]
    NoCombat,
    #[error("combat-membership command would attack twice with {0:?}")]
    AlreadyAttacking(ObjectId),
    #[error("combat-membership command references a non-attacking creature {0:?}")]
    NotAttacking(ObjectId),
    #[error(
        "combat-membership blocked precondition mismatch for {attacker:?}: expected {expected}, found {found}"
    )]
    BlockedPreconditionMismatch {
        attacker: ObjectId,
        expected: bool,
        found: bool,
    },
    #[error("combat-membership command would repeat the block of {attacker:?} by {blocker:?}")]
    DuplicateBlock {
        attacker: ObjectId,
        blocker: ObjectId,
    },
    #[error(
        "combat-membership participation mismatch for {object:?}: expected {expected:?}, found {found:?}"
    )]
    ParticipationMismatch {
        object: ObjectId,
        expected: Box<CombatParticipation>,
        found: Box<CombatParticipation>,
    },
}

/// One exact CR 110.2a + CR 603.6a "under your control" battlefield-entry
/// controller override.
///
/// The override retags the live object AND the two turn-record snapshots the
/// entry created, so a replay that installed only the object's controller would
/// leave "entered under whose control" look-back queries answering with the
/// pre-override controller.
///
/// The record positions are RECORDED rather than re-found at replay time,
/// mirroring `ResolvedZoneChangeCommand::turn_zone_change_index`: the resolve-time
/// authority knows exactly which snapshot it retagged, and re-running a
/// last-match scan against a replayed board could land on a different entry when
/// the same object entered twice in one turn. They are `Option` because the
/// override also runs for entries whose snapshots are absent (CR 603.6a
/// leaves-the-battlefield reconstruction, and the elimination path).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedControllerOverrideCommand {
    pub object: ObjectIncarnationRef,
    pub expected_old_base_controller: Option<PlayerId>,
    pub expected_old_controller: PlayerId,
    pub resulting_controller: PlayerId,
    pub zone_change_index: Option<usize>,
    pub battlefield_entry_index: Option<usize>,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved controller override.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedControllerOverrideReplayInvariantError {
    #[error("controller-override command references an unknown object {0:?}")]
    UnknownObject(ObjectId),
    #[error("controller-override occurrence mismatch: expected {expected:?}, found {found:?}")]
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("controller-override base-controller precondition mismatch: expected {expected:?}, found {found:?}")]
    BaseControllerPreconditionMismatch {
        expected: Option<PlayerId>,
        found: Option<PlayerId>,
    },
    #[error("controller-override controller precondition mismatch: expected {expected:?}, found {found:?}")]
    ControllerPreconditionMismatch { expected: PlayerId, found: PlayerId },
    #[error("controller-override references a missing zone-change record at {0}")]
    MissingZoneChangeRecord(usize),
    #[error("controller-override references a missing battlefield-entry record at {0}")]
    MissingBattlefieldEntryRecord(usize),
}

/// One exact CR 603.6a battlefield-entry provenance stamp.
///
/// The entering permanent records which ability put it there so anti-recursion
/// intervening-ifs ("if it wasn't put onto the battlefield with this ability")
/// can exclude the permanents that very ability placed. A replay that dropped the
/// stamp would let those abilities re-trigger off their own output.
///
/// `resulting_source` is not an `Option`: the delivery tail stamps only
/// ability-driven entries, and `reset_for_battlefield_entry` has already cleared
/// the field, so a recorded stamp always installs a concrete source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEntryProvenanceCommand {
    pub object: ObjectIncarnationRef,
    pub expected_old_source: Option<ObjectId>,
    pub resulting_source: ObjectId,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved entry-provenance stamp.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedEntryProvenanceReplayInvariantError {
    #[error("entry-provenance command references an unknown object {0:?}")]
    UnknownObject(ObjectId),
    #[error("entry-provenance occurrence mismatch: expected {expected:?}, found {found:?}")]
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("entry-provenance precondition mismatch: expected {expected:?}, found {found:?}")]
    SourcePreconditionMismatch {
        expected: Option<ObjectId>,
        found: Option<ObjectId>,
    },
}

/// One exact CR 704.5d / CR 704.5e cease-to-exist removal.
///
/// Ceasing to exist is NOT a zone change (CR 400.7) — no event is emitted and no
/// "whenever exiled" trigger fires — so it cannot ride the zone-change family. It
/// is the only production path that deletes an object outright, and a replay that
/// omitted it would leave a token alive in a zone the rules already swept it from.
///
/// No characteristics are recorded: replay removes the object the retained prefix
/// already reconstructed rather than rebuilding a deleted one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedObjectCeaseCommand {
    pub object: ObjectIncarnationRef,
    pub expected_zone: Zone,
    pub owner: PlayerId,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved cease-to-exist removal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedObjectCeaseReplayInvariantError {
    #[error("cease command references an unknown object {0:?}")]
    UnknownObject(ObjectId),
    #[error("cease occurrence mismatch: expected {expected:?}, found {found:?}")]
    StaleObject {
        expected: ObjectIncarnationRef,
        found: ObjectIncarnationRef,
    },
    #[error("cease zone mismatch: expected {expected:?}, found {found:?}")]
    ZoneMismatch { expected: Zone, found: Zone },
    #[error("cease owner mismatch: expected {expected:?}, found {found:?}")]
    OwnerMismatch { expected: PlayerId, found: PlayerId },
}

/// One exact CR 800.4 player departure.
///
/// The departure itself is two writes — the player's `is_eliminated` flag and
/// their append to `eliminated_players` — that always move together, so they are
/// one command rather than two. Everything the CR 800.4 sweep does afterwards
/// (exiling owned objects, reverting control effects, clearing the stack)
/// already journals through its own family; this command carries the departure,
/// and the surrounding `PlayerLeave` node carries the causal grouping.
///
/// There is no `expected_old` field: "was still in the game" is the precondition,
/// and a stored copy could only ever hold one value.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedPlayerLeaveCommand {
    pub player: PlayerId,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved player departure.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedPlayerLeaveReplayInvariantError {
    #[error("player-leave command references an unknown player {0:?}")]
    UnknownPlayer(PlayerId),
    #[error("player-leave command re-eliminates player {0:?}, who had already left")]
    AlreadyEliminated(PlayerId),
}

/// How one copy token's CR 707.9 "except ..." exceptions relate to its birth.
///
/// The two production copy seams complete the body at different moments, and a
/// replay has to know which one it is looking at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedCopyBodyModifications {
    /// CR 707.2: no copy exceptions — the copiable values are the whole body.
    NoExceptions,
    /// CR 707.9: exceptions folded into the copiable values BEFORE the token
    /// entered (the liminal seam), so replay reapplies them from this record.
    ///
    /// `all_creature_types` is recorded rather than re-read because
    /// `remove_subtype_set` consults the live list, which changeling and other
    /// type-changing effects mutate (CR 205.3 + CR 702.73a).
    Folded {
        modifications: Vec<ContinuousModification>,
        all_creature_types: Vec<String>,
    },
    /// Exceptions applied AFTER the birth by `apply_token_modifications`
    /// (`game/effects/token_copy.rs`) — a pausable, state-level seam that has no
    /// resolved family of its own yet. The birth is still journaled, but replay
    /// REFUSES rather than installing a body that is missing them.
    ///
    /// Delete this variant when that seam gets its own family; the refusal in
    /// `apply_resolved_token_creation` disappears with it.
    DeferredToUnjournaledSeam {
        modifications: Vec<ContinuousModification>,
    },
}

/// How the entering object's body was built for one journaled token birth.
///
/// CR 111.1 is the journaled axis for BOTH variants — an object came into
/// existence and its id and timestamp were drawn. CR 707.2 governs only how a
/// copy body was derived upstream of this seam, so it parameterizes the body
/// rather than forking the family.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedTokenBody {
    /// CR 111.1: an ordinary token minted from a `TokenSpec`.
    Spec {
        /// The effect's token spec — the existing replacement-visible payload
        /// type rather than a hand-rolled field list.
        spec: Box<TokenSpec>,
        /// `token_presets::find_exact_token_ref` READS game state to resolve
        /// this, so it is a re-derivation rather than a spec field.
        token_image_ref: Option<TokenImageRef>,
    },
    /// CR 707.2: a token that entered the battlefield as a copy of an object.
    /// The copy's own art pointer and printed ref already live on `copy`.
    Copy {
        copy: Box<CopyTokenSpec>,
        modifications: ResolvedCopyBodyModifications,
    },
}

/// One exact CR 111.1 token creation, ordinary or copy.
///
/// This is the first family whose replay MATERIALIZES an object rather than
/// verifying and installing into one that already exists, so its precondition is
/// inverted: the applier requires the id to be ABSENT.
///
/// Two allocator draws are recorded because both would otherwise be re-drawn:
/// the `ObjectId` (from `next_object_id`) and the CR 613.7d entry timestamp.
///
/// SCOPE: token births only. Meld is NOT here — `finish_meld_entry` reuses the
/// existing component object's id and moves it through the ordinary zone
/// pipeline, so it materializes nothing and belongs with transform/frame
/// semantics instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedTokenCreationCommand {
    pub object: ObjectIncarnationRef,
    pub owner: PlayerId,
    pub entry_timestamp: u64,
    /// CR 302.6: the turn the token entered, which backs "has been under its
    /// controller's control continuously since their most recent turn began"
    /// (summoning sickness). Recorded rather than re-read from
    /// `GameState::turn_number` so the command is self-contained: a replay that
    /// observed a different live turn would stamp the wrong entered-turn and let
    /// a replayed creature attack when it should not.
    pub entry_turn: u32,
    pub body: ResolvedTokenBody,
    /// CR 614.1: the post-replacement tapped state the token actually entered
    /// with, not the spec's pre-replacement request.
    pub resulting_tapped: bool,
    /// The `next_object_id` high-water after this token's id was drawn, so a
    /// replay resuming from a shorter prefix cannot hand the same id out twice.
    pub resulting_next_object_id: u64,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved token creation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedTokenCreationReplayInvariantError {
    #[error("token-creation command would overwrite live object {0:?}")]
    ObjectAlreadyExists(ObjectId),
    #[error("token-creation command references an unknown owner {0:?}")]
    UnknownOwner(PlayerId),
    #[error("token-creation id {id:?} is not below its recorded high-water {high_water}")]
    IdAboveHighWater { id: ObjectId, high_water: u64 },
    /// CR 707.9: the birth was journaled, but its copy exceptions were applied
    /// after the fact by `apply_token_modifications`, which has no resolved
    /// family yet. Refusing keeps the hole visible instead of installing a body
    /// that is silently missing them.
    #[error(
        "copy-token {object:?} has {count} post-birth copy modification(s) owned by the \
         unjournaled `apply_token_modifications` seam; replay cannot reproduce them"
    )]
    UnreplayableCopyModifications { object: ObjectId, count: usize },
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
    /// CR 700.13: Record the first committed crime of the turn after its
    /// targeting action is successfully placed on the stack.
    CrimeCommitted {
        player: PlayerId,
        expected_turn_count: u32,
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
    #[error("zone-change recorded-turn mismatch: expected {expected}, found {found}")]
    RecordedTurnMismatch { expected: u32, found: u32 },
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

/// Which rule put one object onto the stack.
///
/// This is a provenance discriminator, not an operand-set discriminator. Both
/// arms record the same fields (see [`ResolvedStackPushCommand`]); a copy stack
/// entry is structurally indistinguishable from an original, so the citing rule
/// is not recoverable from the entry alone and has to be carried.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ResolvedStackPushOrigin {
    /// CR 405.1 + CR 601.2a: an object was put onto the stack — a cast spell, or
    /// an activated or triggered ability going on without a card.
    Put,
    /// CR 707.10: a *copy* of a spell or ability was put onto the stack. The
    /// copy is not cast and not activated.
    Copy,
}

/// One exact object landing on the stack.
///
/// CR 405.2: the stack keeps the order objects were added in, and each new
/// object goes on top of everything already there. `resulting_position` is the
/// index this entry occupies after its push, which for a top-of-stack append is
/// also the live stack depth the applier must find before installing. It is
/// RECORDED rather than re-derived at replay time for the same reason
/// `ResolvedControllerOverrideCommand` records its snapshot indices: the
/// resolve-time authority knows exactly where the entry landed, and trusting a
/// replayed board's own depth would install at whatever position that board
/// happens to have reached.
///
/// `resulting_position` is therefore a STORAGE-POSITION CANARY, and its
/// `StackDepthMismatch` is a feature. It fails a replay closed the moment
/// journal order stops matching execution order. That moment is currently
/// reachable, because **stack POPS are not journaled yet**: CR 608.1
/// resolve-pop, CR 603.3c/d abort-pop, CR 701.6a counter-removal, and
/// CR 601.2a cast-abort each remove an entry with no corresponding record. As
/// soon as any of those runs, the replayed depth diverges from every later
/// recorded position and this precondition refuses rather than installing an
/// entry at a position the recording never described.
///
/// **The stack family is consequently NOT end-to-end replayable until the pop
/// units land.** That is a known, scheduled gap, not a defect, and the
/// precondition must not be weakened to paper over it — a canary that has been
/// silenced cannot warn. Until then this family is exact for prefixes that
/// contain no pop, which is what its tests replay.
///
/// This is ONE parameterized command rather than a CR 405.1 sibling and a
/// CR 707.10 sibling because the two authorities' divergence is entirely
/// upstream of this record. Both stamp their source-referential values *into*
/// `entry` before pushing — `push_to_stack` stamps the CR 701.27f generation
/// only when unset, additionally stamps the CR 400.7 incarnation, and binds the
/// CR 509.1c force-block source; `push_copy_to_stack` stamps the generation
/// unconditionally and deliberately leaves the force-block binding alone. Every
/// one of those differences is already resolved into a field value by the time
/// the entry is recorded, so the two arms record identical operand sets and the
/// only real difference is which rule to cite. `origin` carries that.
///
/// Nothing here is re-derived on replay: the applier installs the recorded
/// entry verbatim, so the stamped generation, incarnation, and force-block
/// referent survive exactly rather than being recomputed from a live rescan.
///
/// SCOPE: the push itself, which for a cast spell is only the first half of the
/// cast. `announce_spell_on_stack` pushes at CR 601.2a with `ability: None` and
/// `actual_mana_spent: 0`; the finalized ability and mana are retagged onto that
/// same entry later at CR 601.2i (`casting_costs.rs`). So a recorded `Put` for a
/// spell is the *announcement* snapshot, not the finalized spell.
///
/// That CR 601.2i retag is NOT a lone special case. It is one of roughly ten
/// production sites that mutate an entry IN PLACE after it is on the stack,
/// spread across cast finalization, triggers, copy retargeting, planechase, and
/// the engine's own entry fix-ups. In-place mutation is a third mutation class
/// alongside pushes and pops, and it is invisible to any census keyed on
/// container verbs (`push_back` / `pop_back` / `retain`) because it reaches the
/// element through `iter_mut()`. Whoever journals it is building a class, not
/// patching a card, and none of it is journaled today.
///
/// `stack_paid_facts` is written immediately after that retag, so it moves
/// atomically with cast FINALIZATION rather than with this push, and belongs to
/// the same future unit. `stack_trigger_event_batches` likewise belongs to the
/// trigger authorities. Neither side table has a writer inside either stack
/// authority, which is why neither is part of this record.
///
/// An activated or triggered ability has no CR 601.2i phase: its entry is
/// complete when it is pushed, so for those kinds the record IS the finished
/// entry.
///
/// There is no allocator receipt because neither authority allocates: both are
/// handed an already-built entry, and the CR 400.7 incarnation is read from the
/// source rather than drawn. A recorded high-water here would have nothing
/// behind it to validate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedStackPushCommand {
    /// Boxed because `StackEntryKind` embeds a whole `ResolvedAbility`, which
    /// would otherwise widen every `ResolvedRulesCommand` in the journal.
    pub entry: Box<StackEntry>,
    /// Private CR 603.7 firing classification; present exactly for a triggered
    /// ability, allowing replay to restore the stack side-map atomically.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) trigger_firing: Option<TriggerFiring>,
    pub origin: ResolvedStackPushOrigin,
    /// Zero-based index the entry occupies after the push (CR 405.2).
    pub resulting_position: usize,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved stack push.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedStackPushReplayInvariantError {
    #[error("stack-push command targets depth {expected}, found {found}")]
    StackDepthMismatch { expected: usize, found: usize },
    #[error("stack-push command would duplicate stack entry {0:?}")]
    DuplicateStackEntry(ObjectId),
    #[error("stack-push command references an unknown controller {0:?}")]
    UnknownController(PlayerId),
    #[error("stack-push trigger firing does not match entry kind")]
    TriggerFiringShapeMismatch,
}

/// One exact CR 601.2i cast finalization, retagging an announced stack entry.
///
/// CR 601.2a puts the spell on the stack at announcement, but the entry that
/// lands there is a stub: `ability: None` and `actual_mana_spent: 0`, because
/// neither is known until costs are chosen and paid. CR 601.2i is where "the
/// spell becomes cast" — the point the finalized ability and the mana actually
/// spent are written back onto that same entry, together with the paid-facts
/// snapshot the rest of the engine reads for X, kicker, and convoke questions.
///
/// The two mutations are ONE command rather than two families because they
/// settle together: nothing observes the retagged entry without also observing
/// the snapshot, and a replay that installed one without the other would leave a
/// finalized spell whose paid facts are missing (or vice versa).
///
/// `entry_position` is recorded rather than re-found. The authority locates its
/// entry with `rfind`, a LAST-match scan, so a replay that re-derived the target
/// could retag a different entry than the original execution did — the same
/// hazard `ResolvedZoneChangeCommand::turn_zone_change_index` and the
/// battlefield-entry retags already record positions to avoid.
///
/// `expected_old_paid_facts` is an `Option` rather than an absence assertion.
/// The authority is re-entered from the top by its resume callers (Phyrexian
/// shard choices, paused mana abilities, prepaid casts), so "no snapshot is
/// present yet" is not a property this record can assert without proving no
/// resume path re-reaches the insert. Recording the prior value instead makes
/// the precondition exact under either reading and fails closed on a replay
/// whose predecessor disagrees.
///
/// SCOPE: the CR 601.2i retag of an already-announced entry. The CR 601.2a push
/// that created the entry is a separate family and a separate seam — it does not
/// move atomically with this retag, which is precisely why the announcement
/// snapshot and the finalized entry are two records rather than one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedStackEntryFinalizeCommand {
    /// The stack entry's own id. This family keys on the ENTRY rather than on
    /// an `ObjectIncarnationRef` because the retag targets a stack entry, not an
    /// object record — the same reason `ResolvedStackPushCommand` identifies its
    /// subject by `entry.id`.
    pub object: ObjectId,
    /// Zero-based index of the retagged entry (CR 405.2), recorded so replay
    /// never repeats the authority's `rfind`.
    pub entry_position: usize,
    /// Boxed because `StackEntryKind` embeds a whole `ResolvedAbility`, which
    /// would otherwise widen every `ResolvedRulesCommand` in the journal.
    pub expected_old_kind: Box<StackEntryKind>,
    pub resulting_kind: Box<StackEntryKind>,
    pub expected_old_paid_facts: Option<Box<StackPaidSnapshot>>,
    pub resulting_paid_facts: Box<StackPaidSnapshot>,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved CR 601.2i finalization.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedStackEntryFinalizeReplayInvariantError {
    #[error("stack-entry finalize targets position {position}, but the stack holds {depth}")]
    PositionOutOfRange { position: usize, depth: usize },
    #[error("stack-entry finalize targets {expected:?} at position {position}, found {found:?}")]
    EntryIdentityMismatch {
        position: usize,
        expected: ObjectId,
        found: ObjectId,
    },
    #[error("stack-entry finalize expected a different pre-finalize entry at position {0}")]
    EntryKindMismatch(usize),
    #[error("stack-entry finalize expected different pre-existing paid facts for {0:?}")]
    PaidFactsMismatch(ObjectId),
}

/// One exact CR 603.3d removal of an uncommitted triggered ability.
///
/// The "push first, choose second" invariant puts a triggered ability on the
/// stack BEFORE its choices are gathered, so the entry is live while a
/// `WaitingFor` fills its slots. CR 603.3d: if no legal choices can be made for
/// it, "the ability is simply removed from the stack."
///
/// TWO OUTCOMES, both mutating, which is why `removed` is an `Option` rather
/// than a plain entry. `stack::pop_uncommitted_pending_trigger_entry` consumes
/// `pending_trigger_entry` UNCONDITIONALLY and only then decides whether to pop:
///
/// * guard holds — the cursor is consumed AND the entry leaves the stack with
///   both per-entry side tables.
/// * guard fails — the cursor is consumed and NOTHING else, because the cursor
///   outlived its entry (another path already removed it).
///
/// A command that modelled only the first would leave a replay of the second
/// holding `pending_trigger_entry == Some(id)` while the real execution cleared
/// it — a divergence needing no forged journal, only an honest replay.
///
/// The removed side-table VALUES are deliberately not recorded. Contrast
/// [`ResolvedStackEntryFinalizeCommand`], which records `expected_old_paid_facts`
/// because it INSTALLS a value and must verify the predecessor it overwrites.
/// This command only removes rows keyed on the recorded entry's own id — nothing
/// is installed and nothing is re-derived, so there is no invariant a recorded
/// value would pin, and carrying `Vec<GameEvent>` batches would widen every
/// journal entry for nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedUncommittedTriggerRemovalCommand {
    /// The cursor value consumed by the `.take()`. Always present, because the
    /// take is unconditional.
    pub consumed_entry_id: ObjectId,
    /// The entry actually removed, recorded verbatim so replay verifies the
    /// whole object rather than trusting the id. `None` when the guard declined
    /// to pop.
    pub removed: Option<Box<StackEntry>>,
    /// Stack depth AFTER the operation (CR 405.2).
    pub resulting_depth: usize,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved CR 603.3d removal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedUncommittedTriggerRemovalReplayInvariantError {
    #[error("uncommitted-trigger removal expected pending cursor {expected:?}, found {found:?}")]
    CursorMismatch {
        expected: ObjectId,
        found: Option<ObjectId>,
    },
    #[error("uncommitted-trigger removal targets depth {expected}, found {found}")]
    DepthMismatch { expected: usize, found: usize },
    #[error("uncommitted-trigger removal expected a different entry on top of the stack")]
    RemovedEntryMismatch,
    #[error(
        "uncommitted-trigger removal recorded no pop, but {0:?} is on top and would have been popped"
    )]
    UnexpectedRemovableEntry(ObjectId),
}

/// One exact CR 405.2 removal of a single object from the stack.
///
/// Recorded by `stack::remove_stack_entry_at`, the single authority behind every
/// one-entry stack removal: the CR 405.5 resolution pop, the drain loops
/// (batched resolution, inert no-op batches, CR 724.1b stack exile), the
/// CR 701.6a counter, and the CR 601.2a/601.2i cast rollbacks.
///
/// PARAMETERIZED BY `index` RATHER THAN SPLIT INTO POP/REMOVE-AT SIBLINGS. A
/// top-of-stack pop is exactly the removal at `index == resulting_depth`, so a
/// separate pop command would be this one with a field the caller could derive.
/// Adding that sibling is what the enum's existing `StackPush` /
/// `StackEntryFinalize` / `UncommittedTriggerRemoval` cluster makes tempting and
/// is precisely the debt to avoid.
///
/// A drain of N entries records N of these rather than one bulk removal, so a
/// replay reproduces the removal ORDER and not merely the final depth.
///
/// NOT unified with [`ResolvedUncommittedTriggerRemovalCommand`], despite both
/// removing a stack entry. That axis would have to span CR 603.3d
/// trigger-construction and CR 405.5 resolution — different rule sections the
/// engine resolves separately — and the operand sets genuinely differ: the
/// CR 603.3d removal consumes a cursor and may legitimately remove NOTHING, an
/// outcome that has no analogue here. Unifying them would buy one enum variant
/// at the cost of an applier that checks preconditions belonging to whichever
/// family it was not handed.
///
/// The removed side-table VALUES are deliberately not recorded, for the same
/// reason the CR 603.3d removal omits them: this command installs nothing. It
/// drops rows keyed on the recorded entry's own id, so no recorded value would
/// pin an invariant, and carrying `Vec<GameEvent>` batches would widen every
/// journal entry on the hottest path in the engine for nothing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResolvedStackRemovalCommand {
    /// The removed entry, recorded verbatim so replay verifies the whole object
    /// rather than trusting the id.
    pub entry: Box<StackEntry>,
    /// CR 405.2: the index the entry occupied. Recorded rather than re-found,
    /// because the production sites locate it by a `position`/`rposition` scan
    /// whose predicate can match a DIFFERENT entry on a stack that has since
    /// diverged — `counter.rs` in particular scans on `id OR source_id`, which
    /// matches every ability sharing a source permanent.
    pub index: usize,
    /// Stack depth AFTER the removal (CR 405.2).
    pub resulting_depth: usize,
    pub cause: RulesExecutionNodeRef,
}

/// Typed failure while applying one already-resolved CR 405.2 stack removal.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ResolvedStackRemovalReplayInvariantError {
    #[error("stack removal expected depth {expected} before removal, found {found}")]
    DepthMismatch { expected: usize, found: usize },
    #[error("stack removal targets index {index}, but the stack holds only {depth} entries")]
    IndexOutOfRange { index: usize, depth: usize },
    #[error("stack removal expected a different entry at the recorded index")]
    RemovedEntryMismatch,
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
    ObjectTransform(ResolvedObjectTransformCommand),
    Attachment(ResolvedAttachmentCommand),
    DelayedTriggerInstall(Box<ResolvedDelayedTriggerCommand>),
    ContinuousEffectInstall(Box<ResolvedContinuousEffectCommand>),
    CombatMembership(ResolvedCombatMembershipCommand),
    ControllerOverride(ResolvedControllerOverrideCommand),
    EntryProvenance(ResolvedEntryProvenanceCommand),
    ObjectCease(ResolvedObjectCeaseCommand),
    PlayerLeave(ResolvedPlayerLeaveCommand),
    TokenCreation(Box<ResolvedTokenCreationCommand>),
    Information(ResolvedInformationCommand),
    LedgerEdit(ResolvedLedgerEditCommand),
    LibraryShuffle(ResolvedLibraryShuffleCommand),
    ZoneChange(Box<ResolvedZoneChangeCommand>),
    FrameTransition(Box<ResolvedFrameTransitionCommand>),
    TriggerCollection(ResolvedTriggerCollectionCommand),
    StackPush(Box<ResolvedStackPushCommand>),
    StackEntryFinalize(Box<ResolvedStackEntryFinalizeCommand>),
    UncommittedTriggerRemoval(Box<ResolvedUncommittedTriggerRemovalCommand>),
    StackRemoval(Box<ResolvedStackRemovalCommand>),
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
    CrimeCommittedPreconditionMismatch,
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
            Self::CrimeCommittedPreconditionMismatch => {
                write!(f, "resolved crime command does not match its ledger prefix")
            }
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

    /// CR 800.4: Begin the distinct execution node for one player leaving the
    /// game.
    ///
    /// A leave is not a proposal: every mutation the CR 800.4 sweep performs —
    /// the owned-object exiles, the control-effect reversions, the stack
    /// removals — is caused by the leave itself, not by whatever rules work
    /// happened to be in flight when the state-based action fired. Giving the
    /// leave its own node keeps those commands attributed to it, so a replay can
    /// identify the sweep as one causal unit.
    pub fn begin_player_leave(
        &mut self,
    ) -> Result<RulesExecutionNodeRef, ResolvedRulesJournalError> {
        self.ensure_command_capacity()?;
        self.ensure_node_capacity()?;
        let command = self.allocate_command();
        let ordinal = self.allocate_node();
        let identity = RulesExecutionNodeRef::PlayerLeave(command);
        self.entries.push(ResolvedCommandJournalEntry {
            ordinal: command,
            node: identity,
            command: None,
        });
        self.nodes.push(SettlementNode {
            ordinal,
            identity,
            kind: RulesExecutionNodeKind::PlayerLeave,
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

    /// Records one exact CR 701.27a transform under its causal node.
    pub fn record_object_transform(
        &mut self,
        command: ResolvedObjectTransformCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::ObjectTransform(command),
        )
    }

    /// Records one exact CR 701.3 attachment-graph edit under its causal node.
    pub fn record_attachment(
        &mut self,
        command: ResolvedAttachmentCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::Attachment(command))
    }

    /// Records one exact CR 603.7 delayed-trigger install under its causal node.
    pub fn record_delayed_trigger_install(
        &mut self,
        command: ResolvedDelayedTriggerCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::DelayedTriggerInstall(Box::new(command)),
        )
    }

    /// Records one exact CR 611.2a continuous-effect install under its causal node.
    pub fn record_continuous_effect_install(
        &mut self,
        command: ResolvedContinuousEffectCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::ContinuousEffectInstall(Box::new(command)),
        )
    }
    /// Records one exact CR 506.3 / CR 506.4 combat-membership edit under its
    /// causal node.
    pub fn record_combat_membership(
        &mut self,
        command: ResolvedCombatMembershipCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::CombatMembership(command),
        )
    }

    /// Records one exact CR 110.2a controller override under its causal node.
    pub fn record_controller_override(
        &mut self,
        command: ResolvedControllerOverrideCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::ControllerOverride(command),
        )
    }

    /// Records one exact CR 603.6a entry-provenance stamp under its causal node.
    pub fn record_entry_provenance(
        &mut self,
        command: ResolvedEntryProvenanceCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::EntryProvenance(command),
        )
    }

    /// Records one exact CR 704.5d cease-to-exist removal under its causal node.
    pub fn record_object_cease(
        &mut self,
        command: ResolvedObjectCeaseCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::ObjectCease(command))
    }

    /// Records one exact CR 800.4 player departure under its causal node.
    pub fn record_player_leave(
        &mut self,
        command: ResolvedPlayerLeaveCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(command.cause, ResolvedRulesCommand::PlayerLeave(command))
    }

    /// Records one exact CR 111.1 token creation under its causal node.
    pub fn record_token_creation(
        &mut self,
        command: ResolvedTokenCreationCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::TokenCreation(Box::new(command)),
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

    /// Records one exact object landing on the stack under its causal node.
    pub fn record_stack_push(
        &mut self,
        command: ResolvedStackPushCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::StackPush(Box::new(command)),
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

    /// Records one exact CR 601.2i cast finalization under its causal node.
    pub fn record_stack_entry_finalize(
        &mut self,
        command: ResolvedStackEntryFinalizeCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::StackEntryFinalize(Box::new(command)),
        )
    }

    /// Records one exact CR 603.3d uncommitted-trigger removal under its cause.
    pub fn record_uncommitted_trigger_removal(
        &mut self,
        command: ResolvedUncommittedTriggerRemovalCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::UncommittedTriggerRemoval(Box::new(command)),
        )
    }

    /// Records one exact CR 405.2 top-of-stack removal under its cause.
    pub fn record_stack_removal(
        &mut self,
        command: ResolvedStackRemovalCommand,
    ) -> Result<ResolvedCommandOrdinal, ResolvedRulesJournalError> {
        self.append_command(
            command.cause,
            ResolvedRulesCommand::StackRemoval(Box::new(command)),
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
                | ResolvedRulesCommand::ObjectTransform(_)
                | ResolvedRulesCommand::Attachment(_)
                | ResolvedRulesCommand::DelayedTriggerInstall(_)
                | ResolvedRulesCommand::ContinuousEffectInstall(_)
                | ResolvedRulesCommand::CombatMembership(_)
                | ResolvedRulesCommand::ControllerOverride(_)
                | ResolvedRulesCommand::EntryProvenance(_)
                | ResolvedRulesCommand::ObjectCease(_)
                | ResolvedRulesCommand::PlayerLeave(_)
                | ResolvedRulesCommand::TokenCreation(_)
                | ResolvedRulesCommand::Information(_)
                | ResolvedRulesCommand::LedgerEdit(_)
                | ResolvedRulesCommand::LibraryShuffle(_)
                | ResolvedRulesCommand::ZoneChange(_)
                | ResolvedRulesCommand::FrameTransition(_)
                | ResolvedRulesCommand::TriggerCollection(_)
                | ResolvedRulesCommand::StackPush(_)
                | ResolvedRulesCommand::StackEntryFinalize(_)
                | ResolvedRulesCommand::UncommittedTriggerRemoval(_)
                | ResolvedRulesCommand::StackRemoval(_) => {}
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
            ResolvedRulesCommand::ObjectTransform(command) => {
                // CR 701.27a: a transform turns the permanent to its OTHER face,
                // so a recorded transform that leaves `transformed` unchanged is
                // not a transform that ever happened.
                if entry.node != command.cause
                    || command.expected_old_transformed == command.resulting_transformed
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "transform command does not change the displayed face, or has an \
                         unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::Attachment(command) => {
                // CR 701.3b: re-attaching to the host it is already attached to
                // does nothing, so a recorded edit that leaves the host unchanged
                // is not an edit that ever happened.
                // CR 613.7e + CR 701.3c/d: a move to a new host draws a timestamp
                // and an unattach does not, so the drawn value is present on
                // exactly the commands that installed a host.
                if entry.node != command.cause
                    || command.expected_old_host == command.resulting_host
                    || command.resulting_timestamp.is_some() != command.resulting_host.is_some()
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "attachment command does not change the host, mismatches its timestamp \
                         draw, or has an unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::DelayedTriggerInstall(command) => {
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "delayed-trigger install command has an unrelated cause".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ContinuousEffectInstall(command) => {
                // CR 613.7b: the effect's timestamp was drawn when it was
                // created, so it — the effect id drawn alongside it, and any CR
                // 116.2c termination-group identity — must lie strictly below
                // the high-water the draw left behind, or the receipt describes
                // an allocation that never happened.
                let end_group_above_high_water = command
                    .effect
                    .end_permission
                    .as_ref()
                    .is_some_and(|permission| {
                        permission.group.0 >= command.resulting_next_end_effect_group_id
                    });
                if entry.node != command.cause
                    || command.effect.id >= command.resulting_next_continuous_effect_id
                    || command.effect.timestamp >= command.resulting_next_timestamp
                    || end_group_above_high_water
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "continuous-effect install command has an impossible allocator receipt, \
                         or an unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::CombatMembership(command) => {
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "combat-membership command has an unrelated cause".to_string(),
                    ));
                }
                // CR 400.7: combat membership is per-incarnation, so a re-entered
                // object must never satisfy a command recorded for its predecessor.
                if command.object.incarnation == LEGACY_INCARNATION {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "combat-membership command cannot use a legacy object identity".to_string(),
                    ));
                }
                match &command.edit {
                    // CR 509.1a: a creature is chosen to block an attacking
                    // creature, which is never itself.
                    ResolvedCombatMembershipEdit::Block {
                        resulting_attacker, ..
                    } if *resulting_attacker == command.object.object_id => {
                        return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                            "combat-membership command blocks its own blocker".to_string(),
                        ));
                    }
                    // CR 506.4: removing an object that held no combat role
                    // pruned nothing, so it is not a removal that ever happened.
                    ResolvedCombatMembershipEdit::Remove {
                        expected_participation,
                    } if expected_participation.is_empty() => {
                        return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                            "combat-membership removal prunes no combat role".to_string(),
                        ));
                    }
                    ResolvedCombatMembershipEdit::Attack { .. }
                    | ResolvedCombatMembershipEdit::Block { .. }
                    | ResolvedCombatMembershipEdit::MarkBlocked
                    | ResolvedCombatMembershipEdit::Remove { .. } => {}
                }
            }
            ResolvedRulesCommand::ControllerOverride(command) => {
                // CR 110.2a: an override that leaves both the derived and the
                // pinned controller exactly as they were retagged nothing, so it
                // is not an override that ever happened.
                if entry.node != command.cause
                    || (command.expected_old_base_controller == Some(command.resulting_controller)
                        && command.expected_old_controller == command.resulting_controller)
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "controller-override command changes no controller, or has an unrelated \
                         cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::EntryProvenance(command) => {
                // CR 603.6a: a stamp that names the source the object was already
                // stamped with recorded nothing.
                if entry.node != command.cause
                    || command.expected_old_source == Some(command.resulting_source)
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "entry-provenance command re-stamps the same source, or has an unrelated \
                         cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::ObjectCease(command) => {
                // CR 704.5d/e: only a token or a copy of a card ceases to exist,
                // and never from the battlefield or the stack.
                if entry.node != command.cause
                    || matches!(command.expected_zone, Zone::Battlefield | Zone::Stack)
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "cease command names a zone objects never cease from, or has an \
                         unrelated cause"
                            .to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::PlayerLeave(command) => {
                // CR 800.4: a departure is caused by the leave node opened for
                // it, never by an unrelated proposal that happened to be live.
                if entry.node != command.cause
                    || !matches!(command.cause, RulesExecutionNodeRef::PlayerLeave(_))
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "player-leave command is not attributed to a player-leave node".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::TokenCreation(command) => {
                // CR 111.1: the token's id must be below the high-water its own
                // draw established, or the record cannot describe an allocation
                // that actually happened.
                if entry.node != command.cause
                    || command.object.object_id.0 >= command.resulting_next_object_id
                {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "token-creation command has an impossible id high-water, or an \
                         unrelated cause"
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
            ResolvedRulesCommand::StackPush(command) => {
                // Cause-only, like the frame-transition and trigger-collection
                // arms. There is no allocator receipt to cross-check: neither
                // stack authority draws an id or a timestamp, so this record
                // holds no high-water that could be forged past. Its remaining
                // preconditions (CR 405.2 depth, duplicate entry id, live
                // controller) are all state-dependent and are enforced by
                // `stack::apply_resolved_stack_push` where the state exists.
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "stack-push command has an unrelated cause".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::StackEntryFinalize(command) => {
                // Cause-only. There is no allocator receipt to cross-check:
                // CR 601.2i retags an entry that CR 601.2a already created, so
                // this authority draws no id and no timestamp and holds no
                // high-water a forged journal could jump. Its remaining
                // preconditions (CR 405.2 position, entry identity, the
                // pre-finalize kind, and the prior paid facts) are all
                // state-dependent and are enforced by
                // `stack::apply_resolved_stack_entry_finalize`, where
                // the state exists to check them against.
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "stack-entry finalize command has an unrelated cause".to_string(),
                    ));
                }
            }
            ResolvedRulesCommand::UncommittedTriggerRemoval(command) => {
                // Cause-only, plus the one invariant checkable without state: a
                // recorded pop must name the entry it removed. Everything else
                // (the live cursor, the CR 405.2 depth, the exact entry on top)
                // is state-dependent and is enforced by
                // `stack::apply_resolved_uncommitted_trigger_removal`. There is
                // no allocator receipt — a removal draws nothing.
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "uncommitted-trigger removal command has an unrelated cause".to_string(),
                    ));
                }
                if let Some(removed) = command.removed.as_ref() {
                    if removed.id != command.consumed_entry_id {
                        return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                            "uncommitted-trigger removal popped an entry other than its cursor"
                                .to_string(),
                        ));
                    }
                }
            }
            ResolvedRulesCommand::StackRemoval(command) => {
                // Cause-only. A pop draws no id and no timestamp, so there is no
                // allocator receipt to cross-check. Both of its preconditions
                // (the CR 405.2 depth and the exact entry on top) are
                // state-dependent and are enforced by
                // `stack::apply_resolved_stack_removal`, where the state exists to
                // check them against.
                if entry.node != command.cause {
                    return Err(ResolvedRulesJournalError::InvalidSerializedAuthority(
                        "stack pop command has an unrelated cause".to_string(),
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
        ResolvedLedgerEdit::CrimeCommitted {
            expected_turn_count,
            ..
        } => *expected_turn_count != 0,
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

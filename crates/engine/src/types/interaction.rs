//! Self-contained DTOs for the engine-authored interaction contract.
//!
//! These types intentionally contain no `GameState`, `WaitingFor`, `GameAction`,
//! `ObjectId`, `PlayerId`, mana, zone, or card-model types. That keeps generated
//! bindings narrow and prevents a second generated copy of the existing engine
//! wire graph. All display text is supplied by consumers from the semantic codes
//! below; the engine never places localized UI prose in this contract.

use serde::{Deserialize, Serialize};

pub const MAX_INTERACTION_LIST_LEN: usize = 10_000;

macro_rules! opaque_string_id {
    ($name:ident) => {
        #[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
        #[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
        pub struct $name(pub String);

        impl $name {
            pub fn as_str(&self) -> &str {
                &self.0
            }
        }
    };
}

opaque_string_id!(InteractionSessionId);
opaque_string_id!(InteractionId);
opaque_string_id!(InteractionChoiceId);
opaque_string_id!(PreviewRequestId);

/// Persistence slot semantics. Simultaneous pregame decisions deliberately
/// retain one capability per semantic owner instead of sharing one global ID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionSlotKind {
    Single,
    Mulligan,
    OpeningBottom,
}

/// Trusted, persistence-only binding between one semantic decision owner and
/// the opaque interaction capability currently naming that decision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct ActiveInteractionSlot {
    pub semantic_owner: u8,
    pub slot_kind: InteractionSlotKind,
    pub interaction_id: InteractionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum SimultaneousDecisionKind {
    Mulligan,
    OpeningBottom,
}

/// Stable protocol classification of an engine prompt. This deliberately
/// describes the interaction shape instead of mirroring `WaitingFor` variant
/// names into the wire contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionWaitingForCode {
    Terminal,
    Mulligan,
    OpeningBottom,
    Choose,
    Select,
    Sequence,
    Relations,
    ManaGroups,
    Text,
    DeckPartition,
    Number,
    Shortcut,
    AssignAmounts,
    AssignDamage,
}

/// Parameterized description of the current state-machine surface. It is not a
/// mirror of the large `WaitingFor` enum: consumers use it for
/// simultaneous/terminal semantics and stable prompt identity, while the
/// opportunity response variant is the sole response-shape discriminator.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionWaitingForKind {
    pub simultaneous: Option<SimultaneousDecisionKind>,
    pub terminal: bool,
    pub code: InteractionWaitingForCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionReasonCode {
    AuthorityUnbound,
    InvalidAuthorityState,
    NotAuthorized,
    StaleInteraction,
    UnknownChoice,
    MalformedResponse,
    PayloadTooLarge,
    ConstraintUnsatisfied,
    NoLegalResponse,
    CancelOnly,
    ReducerRejected,
    UnsupportedResponse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionOutcomeCode {
    Preserved,
    Advanced,
    Replaced,
    Cleared,
    Terminal,
    Rejected,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionSummaryCode {
    Decision,
    Candidate,
    Source,
    SelectionBounds,
    AggregateConstraint,
    ConfirmAvailable,
    ConfirmUnavailable,
    Cancel,
    Progress,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionZoneCode {
    Battlefield,
    Hand,
    Library,
    Graveyard,
    Exile,
    Stack,
    Command,
    OutsideGame,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionIntentCode {
    Choose,
    Keep,
    Sacrifice,
    Return,
    Exile,
    Tap,
    Crew,
    Saddle,
    Station,
    RingBearer,
    Blight,
    Pay,
    Attack,
    Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum AggregateComparator {
    GreaterThan,
    LessThan,
    AtLeast,
    AtMost,
    Equal,
    NotEqual,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionAggregateFunction {
    Max,
    Min,
    Sum,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionManaColor {
    White,
    Blue,
    Black,
    Red,
    Green,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionObjectProperty {
    Power,
    Toughness,
    ManaValue,
    ManaSymbolCount { color: InteractionManaColor },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum SelectionConstraint {
    Count {
        min: u32,
        max: u32,
    },
    Aggregate {
        function: InteractionAggregateFunction,
        property: InteractionObjectProperty,
        comparator: AggregateComparator,
        amount: i32,
    },
    EngineValidatedCount {
        min: u32,
        max: u32,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum ConfirmSemantics {
    Immediate,
    Explicit,
}

/// Protocol-owned action discriminators. Mapping from `GameAction` is explicit
/// and exhaustive in the interaction projector, so internal Rust variant-name
/// formatting cannot silently change this wire vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionActionCode {
    PassPriority,
    ChooseMeldPair,
    ChooseEntryAttackTarget,
    PlayLand,
    CastSpell,
    Foretell,
    ActivateAbility,
    DeclareAttackers,
    DeclareBlockers,
    ChooseUntap,
    ChooseExert,
    ChooseEnlist,
    ChooseClashOpponent,
    ChooseZoneOpponentChooser,
    ChoosePileOpponent,
    ChooseAnnouncingOpponent,
    ChooseAssistPlayer,
    CommitAssistPayment,
    MulliganDecision,
    ReorderHand,
    TapLandForMana,
    UntapLandForMana,
    SpendPoolMana,
    UnspendPoolMana,
    SelectCards,
    ChooseRemoveCounterCostDistribution,
    SelectCoinFlips,
    ChooseOutsideGameCards,
    SelectTargets,
    ChooseTarget,
    ChooseReplacement,
    OrderTriggers,
    CancelCast,
    Equip,
    CrewVehicle,
    ActivateStation,
    SaddleMount,
    Transform,
    PlayFaceDown,
    TurnFaceUp,
    SubmitSideboard,
    ChoosePlayDraw,
    ChooseOption,
    SubmitVoteCandidate,
    SubmitSpellbookDraft,
    SubmitPilePartition,
    ChoosePile,
    ChooseBranch,
    SubmitLifeRedistribution,
    ChooseDamageSource,
    SelectModes,
    DecideOptionalCost,
    ChooseAdventureFace,
    ChooseModalFace,
    ChooseAlternativeCast,
    ChooseCastingVariant,
    KeepAllCopyTargets,
    ChoosePermanentTypeSlot,
    ActivateNinjutsu,
    CastSpellAsSneak,
    CastSpellAsWebSlinging,
    CastSpellForFree,
    CastSpellAsMiracle,
    CastSpellAsMadness,
    DecideOptionalEffect,
    RespondToSpliceOffer,
    DecideOptionalEffectAndRemember,
    PayUnlessCost,
    ChooseUnlessCostBranch,
    ChooseActivationCostBranch,
    PayCombatTax,
    ChooseRingBearer,
    ChoosePair,
    ChooseDungeon,
    ChooseDungeonRoom,
    UnlockRoomDoor,
    RollPlanarDie,
    ChooseRoomDoor,
    TapForConvoke,
    HarmonizeTap,
    DeclareCompanion,
    CompanionToHand,
    DiscoverChoice,
    GraveyardPaidCastChoice,
    CascadeChoice,
    RippleChoice,
    FreeCastWindowChoice,
    ChooseTopOrBottom,
    ChooseMutateMergeSide,
    CipherEncode,
    ChooseLegend,
    ChooseBattleProtector,
    SetAutoPass,
    CancelAutoPass,
    SetPhaseStops,
    SetPriorityPassingMode,
    SetPriorityYield,
    SetMayTriggerAutoChoice,
    SetTriggerOrderTemplate,
    AssignCombatDamage,
    AssignBlockerDamage,
    DistributeAmong,
    ChooseCounterMoveDistribution,
    ChooseCountersToRemove,
    SubmitPayAmount,
    RetargetSpell,
    LearnDecision,
    SelectCategoryPermanents,
    ChooseKeptCreatures,
    ChooseKeptPermanents,
    ChooseX,
    SubmitPhyrexianChoices,
    ChooseManaColor,
    PayManaAbilityMana,
    CastPreparedCopy,
    ChooseSpecializeColor,
    CastParadigmCopy,
    PassParadigmOffer,
    GrantDebugPermission,
    RevokeDebugPermission,
    Concede,
    DeclareShortcut,
    RespondToShortcut,
    DeclineShortcut,
    PrecastCopyShortcut,
    Debug,
}

/// Semantic role for one player, object, value, mana, or zone surface. Indexed
/// repetitions carry their ordinal separately, keeping the role vocabulary
/// finite and generated.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionRoleCode {
    Source,
    Candidate,
    Partner,
    AttackTarget,
    Target,
    PaymentMode,
    AbilityIndex,
    Attacker,
    BandCount,
    Blocker,
    Blocked,
    Untap,
    Exert,
    EnlistTarget,
    Enlist,
    Opponent,
    AssistPlayer,
    Assist,
    GenericMana,
    Mulligan,
    SerumPowder,
    HandCard,
    Selected,
    CounterSource,
    CounterType,
    Amount,
    CoinFlipIndex,
    SideboardIndex,
    FaceUpExile,
    OptionIndex,
    TriggerIndex,
    CrewMember,
    StationCrew,
    X,
    MainCard,
    SideboardCard,
    PlayFirst,
    Option,
    CandidateIndex,
    CardName,
    PileA,
    Pile,
    ModeIndex,
    Pay,
    Face,
    CastCost,
    PermanentType,
    ReturnCreature,
    PermissionSource,
    Accept,
    SpliceCard,
    Splice,
    Choice,
    CostBranch,
    CostBranchIndex,
    Pair,
    Dungeon,
    RoomIndex,
    Door,
    Operation,
    ConvokeMana,
    HarmonizeCreature,
    Harmonize,
    Companion,
    CastChoice,
    CastCard,
    Placement,
    MergeSide,
    EncodeCreature,
    Encode,
    Defender,
    Protector,
    AssignmentMode,
    DamageTarget,
    DamageAmount,
    TrampleDamage,
    ControllerDamage,
    Destination,
    DiscardCard,
    Learn,
    Category,
    Kept,
    PhyrexianPayment,
    ManaChoice,
    Count,
    ManaPayment,
    Color,
    Player,
    CastingVariant,
    Mode,
    ModeCost,
    CastingCost,
    VoteOption,
    VoteCandidate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionShortcutResponseCode {
    Propose,
    Accept,
    Decline,
    Shorten,
}

/// Composable semantic surfaces. `name` is copied only from the viewer-filtered
/// state and may therefore contain a redacted public placeholder.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionPresentationSurface {
    Summary {
        code: InteractionSummaryCode,
    },
    Action {
        code: InteractionActionCode,
    },
    Player {
        role: InteractionRoleCode,
        index: Option<u32>,
        seat: u8,
    },
    Object {
        role: InteractionRoleCode,
        index: Option<u32>,
        reference: String,
        name: Option<String>,
        zone: Option<InteractionZoneCode>,
        controller: Option<u8>,
        power: Option<i32>,
        tapped: Option<bool>,
    },
    Zone {
        role: InteractionRoleCode,
        index: Option<u32>,
        zone: InteractionZoneCode,
    },
    Value {
        role: InteractionRoleCode,
        index: Option<u32>,
        value: String,
    },
    Selection {
        intent: InteractionIntentCode,
        constraint: SelectionConstraint,
        confirm: ConfirmSemantics,
    },
    Amount {
        min: u32,
        max: u32,
        total: Option<u32>,
    },
    Mana {
        role: InteractionRoleCode,
        index: Option<u32>,
        symbols: Vec<String>,
    },
    Counter {
        counter_type: String,
        available: u32,
    },
    ShortcutResponse {
        response: InteractionShortcutResponseCode,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionChoiceStatus {
    Available,
    Rejected { reason: InteractionReasonCode },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionChoice {
    pub id: InteractionChoiceId,
    pub surfaces: Vec<InteractionPresentationSurface>,
    pub status: InteractionChoiceStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionGroupConstraint {
    pub group: u32,
    pub min: u32,
    pub max: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionRelationConstraint {
    pub source_id: InteractionChoiceId,
    pub target_ids: Vec<InteractionChoiceId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionRelationSourceConstraint {
    AtMostOne,
    EngineValidated,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionShortcutPointKind {
    Targets,
    ConvokeTaps,
    Mode,
    MayChoice,
    UnlessBreak,
    ManaColor,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionShortcutPoint {
    pub group: u32,
    pub kind: InteractionShortcutPointKind,
    pub min: u32,
    pub max: u32,
    pub unique: bool,
    pub ordered: bool,
    pub read_only: bool,
    pub candidate_ids: Vec<InteractionChoiceId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionShortcutPin {
    pub group: u32,
    pub choice_ids: Vec<InteractionChoiceId>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionResponseSpec {
    Select {
        constraint: SelectionConstraint,
        confirm: ConfirmSemantics,
    },
    AssignAmounts {
        min_total: u32,
        max_total: u32,
        exact_total: Option<u32>,
    },
    AssignDamage {
        total: u32,
        modes: Vec<InteractionDamageAssignmentMode>,
        confirm: ConfirmSemantics,
    },
    Sequence {
        min: u32,
        max: u32,
        unique: bool,
        include_all: bool,
        engine_validated: bool,
        escape: Option<InteractionChoiceId>,
        confirm: ConfirmSemantics,
    },
    GroupedSequence {
        groups: Vec<InteractionGroupConstraint>,
        unique: bool,
        confirm: ConfirmSemantics,
    },
    ManaGroups {
        groups: Vec<InteractionGroupConstraint>,
        max_batch: u32,
        escape: Option<InteractionChoiceId>,
        confirm: ConfirmSemantics,
    },
    Text {
        allow_arbitrary: bool,
        max_len: u32,
        confirm: ConfirmSemantics,
    },
    DeckPartition {
        main_total: u32,
        confirm: ConfirmSemantics,
    },
    Relations {
        edges: Vec<InteractionRelationConstraint>,
        min: u32,
        max: u32,
        source_constraint: InteractionRelationSourceConstraint,
        allow_groups: bool,
        confirm: ConfirmSemantics,
    },
    Number {
        min: u32,
        max: u32,
        confirm: ConfirmSemantics,
    },
    Shortcut {
        count: InteractionShortcutCountSpec,
        points: Vec<InteractionShortcutPoint>,
        allow_decline: bool,
        confirm: ConfirmSemantics,
    },
    ShortcutReply {
        min_iteration: u32,
        max_iteration: u32,
        confirm: ConfirmSemantics,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionShortcutCountSpec {
    Fixed { min: u32, max: u32, suggested: u32 },
    UntilLethal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionShortcutDecision {
    Decline,
    AcceptSuggested,
    Fixed { iterations: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionShortcutReply {
    Accept,
    Shorten { at_iteration: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionDamageAssignmentMode {
    Normal,
    AsThoughUnblocked,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionOpportunityResponse {
    ExactChoices {
        choices: Vec<InteractionChoice>,
    },
    Schema {
        spec: InteractionResponseSpec,
        candidates: Vec<InteractionChoice>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionProgress {
    pub selected: u32,
    pub minimum: u32,
    pub maximum: Option<u32>,
    pub aggregate: Option<i32>,
    pub confirmable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionOpportunity {
    pub interaction_id: InteractionId,
    pub response: InteractionOpportunityResponse,
    pub surfaces: Vec<InteractionPresentationSurface>,
    pub progress: InteractionProgress,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionAvailability {
    ProgressAvailable { witness: InteractionSubmission },
    InputRequired,
    EscapeOnly { reason: InteractionReasonCode },
    Waiting,
    Terminal { outcome: InteractionOutcomeCode },
    Unsupported { reason: InteractionReasonCode },
    Stuck { reason: InteractionReasonCode },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct ViewerInteraction {
    pub waiting_for_kind: InteractionWaitingForKind,
    pub authorized_submitters: Vec<u8>,
    pub can_submit: bool,
    pub auto_pass_recommended: bool,
    pub opportunities: Vec<InteractionOpportunity>,
    pub availability: InteractionAvailability,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct AmountAssignment {
    pub choice_id: InteractionChoiceId,
    pub amount: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionRelation {
    pub source_id: InteractionChoiceId,
    pub target_id: InteractionChoiceId,
    pub group: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionResponse {
    Choose {
        choice_id: InteractionChoiceId,
    },
    Select {
        choice_ids: Vec<InteractionChoiceId>,
    },
    AssignAmounts {
        assignments: Vec<AmountAssignment>,
    },
    AssignDamage {
        mode: InteractionDamageAssignmentMode,
        assignments: Vec<AmountAssignment>,
    },
    Sequence {
        choice_ids: Vec<InteractionChoiceId>,
    },
    Relations {
        relations: Vec<InteractionRelation>,
    },
    ManaGroups {
        choice_ids: Vec<InteractionChoiceId>,
        count: u32,
    },
    Text {
        value: String,
    },
    DeckPartition {
        main: Vec<AmountAssignment>,
    },
    Number {
        value: u32,
    },
    Shortcut {
        decision: InteractionShortcutDecision,
        pins: Vec<InteractionShortcutPin>,
    },
    ShortcutReply {
        reply: InteractionShortcutReply,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionSubmission {
    pub interaction_id: InteractionId,
    pub response: InteractionResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionPreviewRequest {
    pub request_id: PreviewRequestId,
    pub interaction_id: InteractionId,
    pub response: InteractionResponse,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
#[cfg_attr(
    feature = "interaction-bindings",
    ts(rename_all = "camelCase", rename_all_fields = "camelCase")
)]
pub enum InteractionPreviewStatus {
    Confirmable,
    Rejected { reason: InteractionReasonCode },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionPreview {
    pub request_id: PreviewRequestId,
    pub interaction_id: InteractionId,
    pub status: InteractionPreviewStatus,
    pub progress: InteractionProgress,
    pub outcome: InteractionOutcomeCode,
    pub summaries: Vec<InteractionSummaryCode>,
}

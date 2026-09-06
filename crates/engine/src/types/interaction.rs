//! Self-contained DTOs for the engine-authored interaction contract.
//!
//! These types intentionally contain no `GameState`, `WaitingFor`, `GameAction`,
//! `ObjectId`, `PlayerId`, mana, zone, or card-model types. That keeps generated
//! bindings narrow and prevents a second generated copy of the existing engine
//! wire graph. All display text is supplied by consumers from the semantic codes
//! below; the engine never places localized UI prose in this contract.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

pub const MAX_INTERACTION_LIST_LEN: usize = 10_000;

/// CR 732.2a: the most count-axis elements one loop-shortcut offer publishes magnitudes for.
///
/// Two grounds, both bounds rather than preferences. It must be at least 3, because the
/// published sample always carries the count window's own `min`, `suggested` and `max`. And it
/// must stay small, because every element is charged to the outbound ceiling — once for the
/// element and once more for each of its entry and allocation lists — and a count axis reaching
/// the shortcut cycle ceiling would otherwise publish thousands of them.
pub const MAX_SHORTCUT_PREVIEW_ELEMENTS: usize = 16;

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
opaque_string_id!(InteractionActionId);
opaque_string_id!(PreviewRequestId);
// Viewer-safe object reference. Only the engine maps this opaque interaction
// value back to an in-game object.
opaque_string_id!(InteractionObjectReference);

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
    ResolveAllConsent,
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
    // CR 115.1 targeting vocabulary. Each of these names a distinct game
    // action in its own CR section, so they stay flat siblings rather than one
    // parameterized variant: unifying e.g. Destroy (CR 701.8), Counter
    // (CR 701.6) and Mill (CR 701.17) under a single code with a "which
    // action" axis would conflate rule sections the engine resolves
    // separately, which the workspace categorical-boundary rule forbids.
    /// CR 120.1: damage dealt to the chosen target.
    Damage,
    /// CR 701.8: destroy the chosen permanent.
    Destroy,
    /// CR 701.19: put a regeneration shield on the chosen permanent.
    Regenerate,
    /// CR 701.6: counter the chosen spell.
    Counter,
    /// CR 701.26: untap the chosen permanent.
    Untap,
    /// CR 701.17: mill from the chosen player's library.
    Mill,
    /// CR 701.9: the chosen player discards.
    Discard,
    /// CR 121.1: the chosen player draws.
    Draw,
    /// CR 119.3: the chosen player gains life.
    GainLife,
    /// CR 119.3: the chosen player loses life.
    LoseLife,
    /// CR 701.14: the chosen creature fights.
    Fight,
    /// CR 701.3: attach to the chosen permanent.
    Attach,
    /// CR 707: copy the chosen object.
    Copy,
    /// CR 613.1b: take control of the chosen permanent.
    GainControl,
    /// CR 701.20: reveal the chosen card.
    Reveal,
    /// CR 613.4: change the chosen object's characteristics (power/toughness,
    /// counters, types) with NO claim about direction. Used when no single
    /// direction is true — a dynamic magnitude (X / count-based) or a genuinely
    /// opposing modification such as "+2/-2".
    Modify,
    /// CR 613.4: a modification that raises the chosen object's power and/or
    /// toughness. Split from `Modify` because `TargetSelectionSlot` stamps the
    /// direction read off the effect payload at construction; the unit
    /// `EffectKind` tag alone cannot distinguish these three.
    Buff,
    /// CR 613.4: a modification that lowers the chosen object's power and/or
    /// toughness.
    Debuff,
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

/// Stable comparison axis used by mana-spend restrictions.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionManaComparator {
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
pub enum InteractionManaAbilityActivationScope {
    OfSpellType,
    Any,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionManaZoneSpendPolarity {
    From,
    NotFrom,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionManaSpecialAction {
    CompanionToHand,
    UnlockDoor,
    Plot,
    TurnFaceUp,
    RollPlanarDie,
    /// CR 116.2c: pay a continuous effect's printed termination cost to end it.
    EndContinuousEffect,
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
pub enum InteractionManaSpellCostCriterion {
    ManaValue {
        comparator: InteractionManaComparator,
        value: u32,
    },
    HasXInCost,
}

/// Viewer-safe, lossless projection of a runtime mana-spend restriction.
///
/// Type and keyword names intentionally stay semantic strings: they come from
/// card text and are already the canonical engine vocabulary. Every runtime
/// `ManaRestriction` variant has a corresponding case here, including nested
/// `OnlyForAny` restrictions.
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
pub enum InteractionManaRestriction {
    OnlyForSpell,
    OnlyForSpellType {
        spell_type: String,
    },
    OnlyForCreatureType {
        creature_type: String,
    },
    OnlyForTypeSpellsOrAbilities {
        spell_type: String,
        ability: InteractionManaAbilityActivationScope,
    },
    OnlyForActivation,
    OnlyForTaggedActivation {
        tag: String,
    },
    OnlyForXCosts,
    OnlyForSpellWithKeywordKind {
        keyword: String,
    },
    OnlyForSpellWithKeywordKindFromZone {
        keyword: String,
        zone: InteractionZoneCode,
    },
    OnlyForSpellWithManaValue {
        comparator: InteractionManaComparator,
        value: u32,
    },
    OnlyForSpellMatchingCostCriteria {
        spell_type: Option<String>,
        criteria: Vec<InteractionManaSpellCostCriterion>,
    },
    OnlyForSpellWithColorCount {
        comparator: InteractionManaComparator,
        count: u32,
    },
    OnlyForSpellColor {
        color: InteractionManaColor,
    },
    OnlyForSpellFromZone {
        zone: InteractionZoneCode,
        polarity: InteractionManaZoneSpendPolarity,
    },
    CannotCastSpellFromZone {
        zone: InteractionZoneCode,
    },
    OnlyForFaceDownSpell,
    OnlyForAny {
        restrictions: Vec<InteractionManaRestriction>,
    },
    OnlyForSpecialAction {
        action: InteractionManaSpecialAction,
    },
    Impossible,
    ConvokePayment,
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
    ChooseGiftRecipient,
    ChooseAssistPlayer,
    CommitAssistPayment,
    MulliganDecision,
    ReorderHand,
    TapLandForMana,
    ActivateManaSource,
    BackToManaPayment,
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
    ChooseEntryController,
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
    ChooseResolutionOptionalPaymentBranch,
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
    /// CR 116.2c: pay a continuous effect's printed termination cost to end it.
    EndContinuousEffect,
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
    ProducedMana,
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
        /// Opaque deterministic identity for the exact action payload.
        action_id: Option<InteractionActionId>,
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
        restrictions: Vec<InteractionManaRestriction>,
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
    /// CR 732.2a + CR 601.2c: the SEGMENT LENGTHS of the announcement sequence
    /// `choice_ids` names, positionally and one-for-one — how many repetitions each
    /// announced subject takes. Empty on every pin that answers its point per
    /// position.
    ///
    /// The lengths are a DECLARATION of how the count is spread, not a claim about what
    /// each subject realizes. On a drive whose first cycle resolves a target announced
    /// before the drive begins, the realized split is shifted one cycle late at each
    /// boundary while the total stays exact — so a segment starting at the last index is
    /// admitted and stays announced, yet realizes no repetition at all.
    ///
    /// CR 732.2a + CR 732.2c: a proposal describes the choices acceptance then takes,
    /// so a sequence is admissible only where the count it partitions is already known.
    /// A pin is SEQUENCED on either limb — it carries `amounts`, or its `choice_ids`
    /// outnumber the point's published `max` — and both limbs bind the same partition,
    /// which only an `IterationCount::Fixed` can fill. Under a fixed count that
    /// partition is `amounts`: one part per announced subject, in the sequence's own
    /// order, every part at least 1, summing to the DECLARED count. An until-lethal
    /// count has nothing to partition and refuses BOTH limbs — an empty `amounts` does
    /// not rescue a longer `choice_ids` list — so no announcement order past the head
    /// is declarable through this ingress.
    ///
    /// A sequenced pin is admissible only on a `Targets` point whose published `max`
    /// is 1: a multi-position slot needs a per-position carrier a flat list cannot
    /// express, so it is refused rather than mis-read.
    ///
    /// `#[serde(default)]` keeps the field additive on the wire; `skip_serializing_if`
    /// keeps a pin carrying none byte-identical to the pre-field shape. Neither makes
    /// it additive at CONSTRUCTION — every struct literal must still name it, and
    /// nothing here uses `..Default::default()`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub amounts: Vec<AmountAssignment>,
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
    /// CR 100.2a / CR 100.4a / CR 100.5: a between-games main/sideboard split.
    ///
    /// The card pool is invariant, so `sideboard = pool - main` and both the
    /// minimum deck size and the sideboard cap collapse into one closed
    /// interval on the main-deck total. `min_main_total` is a *minimum* — there
    /// is no maximum deck size, so a client must not require an exact match.
    DeckPartition {
        min_main_total: u32,
        max_main_total: u32,
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
    /// CR 732.2a: the loop-shortcut declaration. `count` is the picker's window and
    /// `preview` states what the counts inside that window actually DO, per axis — see
    /// [`InteractionShortcutPreview`] for why each element's count travels with its
    /// magnitudes.
    ///
    /// `preview` is KEYED ON COUNT: one element per count the offer states magnitudes for,
    /// never more than one per count, and a renderer picks by exact `count` match. The engine
    /// publishes the window's own endpoints plus a bounded interior sample
    /// (`MAX_SHORTCUT_PREVIEW_ELEMENTS`), so a count inside the window may legitimately have
    /// no element. The list is EMPTY when the offer states no per-period signature to multiply
    /// or no finite count to multiply it by.
    ///
    /// The doc lives on the VARIANT rather than on `preview`: ts_rs emits field docs into
    /// the generated bindings as JSDoc but drops variant docs, and a comment block in the
    /// middle of a union keeps that file from being one declaration per line.
    Shortcut {
        count: InteractionShortcutCountSpec,
        points: Vec<InteractionShortcutPoint>,
        allow_decline: bool,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        preview: Vec<InteractionShortcutPreview>,
        confirm: ConfirmSemantics,
    },
    /// CR 732.2b + CR 732.2c: the proposal this player is being asked to accept, published so
    /// the responder can judge the object CR 732.2b gives them the right to shorten.
    ///
    /// `points` are READ-ONLY statement points — one per decision the declaration answered AND
    /// this vocabulary can state, in the declaration's own order. `read_only` is `true` on every
    /// one: the responder's only outbound values are Accept and Shorten, so nothing here is an
    /// option set to pick from. A `mayChoice` statement point publishes EXACTLY TWO candidate
    /// ids, read in order as SUBJECT then ANSWER — an optional decision whose subject cannot be
    /// minted publishes NO point rather than a shorter one, so the positional read is total over
    /// what is published. EMPTY when the proposal carries no declaration: every count-only offer,
    /// every proposal the viewer's redaction dropped, and a declaration whose unstatable
    /// announced-target decision would hand its own allocation domain to a LATER one that IS
    /// stated — an unstatable decision with no such successor is simply skipped, and the
    /// statements beside it publish (see `game::interaction::declared_shortcut_projection`).
    ///
    /// `declared` is what the DECLARED count does: the same element vocabulary the offer's own
    /// published list carries, minted by the same producer over the allocation the proposer
    /// actually declared. Absent only on a declaration whose PARTITION cannot be stated: an
    /// order-only one (CR 732.1b: an until-lethal proposal names no count to partition, so a
    /// magnitude there could only be invented), and one whose segments cannot be read back
    /// against the announced-target decision's own published ids and total. A declaration whose
    /// partition IS stated and whose magnitudes are not — a proposal carrying no per-period
    /// signature, or one whose per-slot life charge resolves to a seat the declaration never
    /// announces (CR 119.3) — is published with its allocation and an EMPTY entry list: segment
    /// lengths are not magnitudes, and a responder judging accept-or-shorten against half the
    /// proposal is the partial statement this projection rules out. See
    /// `game::interaction::declared_sequence_preview`.
    ///
    /// CR 601.2c: `allocation_group` names the `points` group `declared`'s allocation is stated
    /// over — the announced-target decision the declared count is spread across. It is minted
    /// from the same domain lookup as the allocation itself, so the two are published together
    /// or not at all, and the allocated decision is identified by its published group rather
    /// than by its position among the points. That decision's announcement order is the order
    /// of the allocation's own entries; every other statement point states its order itself.
    ///
    /// The docs live on the VARIANT rather than on the fields, for the reason the `Shortcut`
    /// variant above records: ts_rs emits field docs into the generated bindings as JSDoc but
    /// drops variant docs, and a comment block in the middle of a union keeps that file from
    /// being one declaration per line.
    ShortcutReply {
        min_iteration: u32,
        max_iteration: u32,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        points: Vec<InteractionShortcutPoint>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        declared: Option<InteractionShortcutPreview>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        allocation_group: Option<u32>,
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

/// The display family one shortcut-preview magnitude belongs to — the projection-layer code
/// for `game::derived_views::UnboundedFamily`, mapped by an exhaustive `match` in
/// `game::interaction`.
///
/// A code rather than a mirror of `analysis::resource::ResourceAxis`, for this module's own
/// stated reason: `ResourceAxis` carries `PlayerId`, `ManaType`, `CounterClass`,
/// `ObjectClass` and `TriggerKind` payloads, and generating those would be the "second
/// generated copy of the existing engine wire graph" this file exists to avoid. The client
/// already labels these eleven families (glyph + i18n key per family), so a code is
/// everything a renderer needs.
///
/// No CR governs a display grouping — the grouping authority is `derived_views::family_of`,
/// and this enum tracks it variant-for-variant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub enum InteractionShortcutPreviewFamily {
    Mana,
    Life,
    Damage,
    Mill,
    Counters,
    Tokens,
    Cards,
    Casts,
    Combats,
    Turns,
    Triggers,
}

/// One axis of what a declared shortcut count finishes with: a signed magnitude, already
/// multiplied out by the engine.
///
/// `amount` is the FINISHED total, not a per-cycle rate, and it is signed — a drain loop
/// states its victim's life as negative. `player` is the seat the magnitude lands on for the
/// per-seat families (life, damage, mill, and the poison term of counters) and `None` for the
/// whole-game ones (mana, tokens, cards, casts, combats, turns, triggers).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionShortcutPreviewEntry {
    pub family: InteractionShortcutPreviewFamily,
    pub player: Option<u8>,
    pub amount: i32,
}

/// CR 732.2a: the engine-computed consequence of repeating a certified loop a stated number
/// of times — "the predictable results of the sequence of choices", published as numbers.
///
/// `count` is carried WITH the entries, and that pairing is the point: every magnitude here
/// is stated for exactly this count and for no other, so a renderer can never attach these
/// numbers to a different one. The engine multiplies; the display layer reads.
///
/// One of these per published count; the spec's list is empty when the offer states no
/// per-period signature to multiply, or no finite count to multiply it by.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionShortcutPreview {
    pub count: u32,
    pub entries: Vec<InteractionShortcutPreviewEntry>,
    /// CR 732.2a + CR 601.2c: the DECLARATION's shape over this element's `count` — which
    /// announced choices the count is spread across, and how many repetitions each takes.
    /// It is not a magnitude claim about any axis.
    ///
    /// The `choice_id`s are the offer's own published candidate ids, taken from the first
    /// `Targets` point in published order. A later `Targets` point holding candidates does not
    /// fill that domain: it is the first point or nothing, because silently moving it to a
    /// second point would state the split over choices the reader cannot identify.
    ///
    /// The amounts come from whichever producer minted the element. In the offer's published list
    /// (`loop_shortcut_preview`) they are the canonical even split of `count`, remainder on the
    /// earliest ids, empty exactly when that first point holds no candidate. In a preview of a
    /// player's own declaration (`declared_shortcut_preview`) they are the amounts that player
    /// authored — positive parts summing to `count` over a duplicate-free subset of those ids,
    /// enforced by the declaration ingress rather than by this type — and never empty: that
    /// producer states no element at all rather than one carrying an empty split. In the
    /// responder's view of a declaration (`declared_sequence_preview`) they are the segment
    /// LENGTHS the declared count partitions into over the iterations that decision's
    /// announcements start at: successive differences summing to `count`, one per published id,
    /// never empty — and NOT necessarily positive, since a step starting exactly at the count
    /// takes a zero-length segment.
    ///
    /// CR 119.3: `entries` follow this allocation ONLY when the period's life map names
    /// exactly one losing seat that this allocation itself announces and the slot's announced
    /// magnitude is the whole of that seat's per-period loss, which is what makes it positive —
    /// per-seat life magnitudes are then this split multiplied by that rate, and they still
    /// total the period. On every other offer a non-empty allocation still ships beside entries
    /// folded from the raw period, because the allocation states the declaration and the
    /// entries state what the engine can attribute. On the RESPOND side that separation goes one
    /// step further: a declaration whose partition is stated and whose magnitudes are not ships
    /// this allocation beside an EMPTY `entries`, rather than withholding the whole element
    /// (`game::interaction::declared_sequence_preview`).
    ///
    /// The magnitudes are this declaration's arithmetic. On a drive whose first cycle resolves
    /// a target announced before the drive begins, the realized split is shifted one cycle at
    /// each boundary while the total stays exact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allocation: Vec<AmountAssignment>,
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

/// A direct, engine-authored interaction submission for one attachment.
///
/// The UI must echo this opaque response rather than deriving an action or a
/// response envelope from the opportunity schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionAttachmentFanChild {
    #[cfg_attr(feature = "interaction-bindings", ts(type = "number"))]
    pub object_id: u64,
    pub submission: InteractionSubmission,
}

/// Viewer-scoped attachment affordance for a single interaction opportunity.
/// It is derived from the filtered projection, not by consumers scanning game
/// state that may carry authority-only relationship information.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionAttachmentFan {
    #[cfg_attr(feature = "interaction-bindings", ts(type = "number"))]
    pub host_id: u64,
    pub children: Vec<InteractionAttachmentFanChild>,
}

/// One card in a host's attachment view, with the engine's own submission when
/// a one-step pick was published for it and `None` when it was not.
///
/// `None` is not "unavailable": it means this projection publishes no direct
/// pick, and the card's remaining affordances stay on the normal interaction
/// surface. Membership does not depend on it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionAttachmentViewCard {
    #[cfg_attr(feature = "interaction-bindings", ts(type = "number"))]
    pub object_id: u64,
    pub submission: Option<InteractionSubmission>,
}

/// Viewer-scoped membership of one host's attachment subtree: what is attached
/// to this object, in the order the engine lays it out, whatever the viewer may
/// currently do about it.
///
/// This is deliberately a different question from [`InteractionAttachmentFan`],
/// which publishes the picks the viewer is *authorized to submit right now*. An
/// attached permanent is an object on the battlefield (CR 301.5 / CR 303.4), so
/// its membership follows visibility, not authorization — it must survive an
/// opponent's turn, a prompt that owns the waiting state, and a terminal game.
/// Consumers render and count this list; they must never rebuild it by scanning
/// `attachments`, which carries authority-only relationship data.
///
/// Every card here is validated in both directions (the host lists the child and
/// the child points back at the host) and read only from the filtered
/// projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "interaction-bindings", derive(ts_rs::TS))]
#[serde(rename_all = "camelCase")]
#[cfg_attr(feature = "interaction-bindings", ts(rename_all = "camelCase"))]
pub struct InteractionAttachmentView {
    #[cfg_attr(feature = "interaction-bindings", ts(type = "number"))]
    pub host_id: u64,
    pub cards: Vec<InteractionAttachmentViewCard>,
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
    #[serde(default)]
    #[cfg_attr(
        feature = "interaction-bindings",
        ts(type = "Record<number, InteractionAttachmentFan>")
    )]
    pub attachment_fans: BTreeMap<u64, InteractionAttachmentFan>,
    /// What is attached to each visible object, keyed by that object. Published
    /// on every projection, including the ones that carry no opportunity at all.
    #[serde(default)]
    #[cfg_attr(
        feature = "interaction-bindings",
        ts(type = "Record<number, InteractionAttachmentView>")
    )]
    pub attachment_views: BTreeMap<u64, InteractionAttachmentView>,
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
    /// CR 732.2a: the engine-computed consequence of the DECLARATION this request carries — one
    /// previewed element in the same shape and vocabulary as an element of the offer's published
    /// list, minted by the same producer. Absent unless the request declares a shortcut split.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shortcut_preview: Option<InteractionShortcutPreview>,
}

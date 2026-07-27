//! Serialization adapter between this repo's MTG engine (`GameState` /
//! `GameAction`) and the external ManaBrew wire protocol.
//!
//! Pinned upstream: `manabrew-protocol` **2.0.0** (crates.io, 2026-07-24).
//! [`PROTOCOL_VERSION`] is the crate major, which is how upstream defines the
//! wire version.
//!
//! This crate is a pure serialization boundary: it never computes, derives, or
//! re-interprets game state. Anything the engine does not supply is recorded in
//! [`unsupported_protocol_capabilities`] rather than inferred here.

use std::collections::{BTreeMap, HashMap};

use engine::ai_support::legal_actions_for_viewer;
use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::derived::derive_display_state;
use engine::game::derived_views::{derive_views, DerivedViews};
use engine::game::filter_state_for_viewer;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::interaction::{derive_viewer_interaction, resolve_interaction_response};
use engine::game::turn_control;
use engine::types::ability::TargetRef;
use engine::types::card::CardFace;
use engine::types::game_state::{
    GameState, ManaChoice, ManaChoicePrompt, MulliganDecisionPhase, PendingMulliganAction,
    ShardChoice, StackEntryKind, WaitingFor,
};
use engine::types::interaction::{
    InteractionChoice, InteractionOpportunity, InteractionOpportunityResponse,
    InteractionPresentationSurface, InteractionResponse, InteractionResponseSpec,
    InteractionRoleCode, InteractionSubmission, SelectionConstraint, ViewerInteraction,
};
use engine::types::mana::{ManaColor as EngineManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;
use engine::types::{GameAction, ObjectId};
use serde::{Deserialize, Serialize};

/// Wire version of the pinned upstream protocol. Upstream defines the wire
/// version as the `manabrew-protocol` crate major, so 2.0.0 => 2.
pub const PROTOCOL_VERSION: u32 = 2;

pub type Result<T> = std::result::Result<T, AdapterError>;

/// Why a [`PromptOutput`] is not a legal answer to a given [`PromptInput`].
///
/// Distinct from [`AdapterError`]: this is the formal prompt/response contract
/// check, and maps onto exactly two of the five wire [`ProtocolErrorCode`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResponseViolation {
    /// The output's prompt family is not the open prompt's family.
    WrongPromptType,
    /// The echoed action id was never advertised by the open prompt.
    UnknownActionId(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AdapterError {
    UnsupportedPlayerCount {
        count: usize,
    },
    UnsupportedPrompt {
        waiting_for_type: &'static str,
        code: &'static str,
    },
    UnsupportedProtocolFeature {
        code: &'static str,
    },
    MissingCardText {
        object_id: ObjectId,
    },
    MalformedId {
        expected_prefix: &'static str,
        value: String,
    },
    StaleOrInvalidActionId {
        action_id: String,
    },
    PromptIdMismatch {
        expected: u32,
        actual: u32,
    },
    NoAuthorizedPrompt {
        viewer: PlayerId,
    },
    IllegalResponseForPrompt {
        response_kind: &'static str,
    },
    ObjectNotFound {
        object_id: ObjectId,
    },
}

pub trait CardTextLookup {
    fn text_for(&self, object: &GameObject) -> Option<String>;
}

impl CardTextLookup for CardDatabase {
    fn text_for(&self, object: &GameObject) -> Option<String> {
        let printed_ref = object.printed_ref.as_ref()?;
        text_from_face(self.get_face_by_printed_ref(printed_ref)?)
    }
}

impl<F> CardTextLookup for F
where
    F: Fn(&GameObject) -> Option<String>,
{
    fn text_for(&self, object: &GameObject) -> Option<String> {
        self(object)
    }
}

fn text_from_face(face: &CardFace) -> Option<String> {
    face.oracle_text
        .as_ref()
        .or(face.non_ability_text.as_ref())
        .cloned()
}

#[derive(Debug, Clone)]
pub struct PreparedManabrewSnapshot {
    pub game_id: String,
    pub viewer: PlayerId,
    pub prompt_id: u32,
    pub state: GameState,
    pub derived: DerivedViews,
    pub actions: Vec<GameAction>,
    pub spell_costs: HashMap<ObjectId, ManaCost>,
    pub legal_actions_by_object: HashMap<ObjectId, Vec<GameAction>>,
    /// The prompt's source object, cloned from **raw** (pre-viewer-filter)
    /// state.
    ///
    /// v2 moved `AgentPrompt.sourceCardId` to a full `sourceCard: CardDto`
    /// precisely so the source survives when it lies outside the recipient's
    /// visible state — building it from `state` (which is filtered, see
    /// `prepare_snapshot_with_prompt_id`) would defeat that. Capturing the raw
    /// object here is what lets `build_prompt` construct the `CardDto` later,
    /// where a `CardTextLookup` is finally in scope.
    pub source_card_object: Option<GameObject>,
    /// The engine's own projection of what this viewer may answer right now.
    ///
    /// Captured here because it is derivable only from **raw** state, which
    /// `build_prompt_input` no longer has: `derive_viewer_interaction` reads
    /// authorization and capability identity from the authoritative state and
    /// every presentation surface from the filtered one, and collapsing that to
    /// a single filtered state would silently change what the viewer is told.
    ///
    /// Derived unconditionally rather than on demand. One projection per prompt
    /// is proportionate — a prompt is a human decision point, not a search-tree
    /// node — and making it conditional would mean deciding *here* which waiting
    /// states the generic path serves, which is precisely the per-variant
    /// bookkeeping this projection exists to remove.
    pub interaction: ViewerInteraction,
}

impl PreparedManabrewSnapshot {
    pub fn prompt_context(&self) -> PromptContext {
        PromptContext {
            prompt_id: self.prompt_id,
            deciding_player: self.viewer,
            action_table: action_table(&self.actions),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    pub prompt_id: u32,
    pub deciding_player: PlayerId,
    pub action_table: Vec<ActionTableEntry>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ActionTableEntry {
    pub id: String,
    pub action: GameAction,
}

/// Prepare a **state-only** snapshot.
///
/// Prompt id `0` is reserved by the protocol for engine-synthesized
/// absent-player defaults (timeout / disconnect) and must never be accepted as
/// a real answer, so a prompt built from this snapshot would be unanswerable.
/// Use [`prepare_snapshot_with_prompt_id`] with a non-zero id for anything that
/// builds an [`AgentPrompt`]; [`build_prompt`] rejects id `0`.
pub fn prepare_snapshot(
    raw_state: &GameState,
    viewer: PlayerId,
    game_id: impl Into<String>,
) -> Result<PreparedManabrewSnapshot> {
    prepare_snapshot_with_prompt_id(raw_state, viewer, game_id, 0)
}

pub fn prepare_snapshot_with_prompt_id(
    raw_state: &GameState,
    viewer: PlayerId,
    game_id: impl Into<String>,
    prompt_id: u32,
) -> Result<PreparedManabrewSnapshot> {
    if raw_state.players.len() != 2 {
        return Err(AdapterError::UnsupportedPlayerCount {
            count: raw_state.players.len(),
        });
    }

    let (actions, spell_costs, legal_actions_by_object) =
        legal_actions_for_viewer(raw_state, viewer);
    // Capture the prompt source from RAW state, before the viewer filter runs —
    // see `PreparedManabrewSnapshot::source_card_object`.
    let source_card_object = source_object_id(&raw_state.waiting_for)
        .and_then(|id| raw_state.objects.get(&id))
        .cloned();
    let mut state = filter_state_for_viewer(raw_state, viewer);
    // Projected from the plain viewer filter, before `derive_display_state`, so
    // the adapter sees exactly what every other interaction consumer sees.
    let interaction = derive_viewer_interaction(raw_state, &state, viewer);
    derive_display_state(&mut state);
    let derived = derive_views(&state, Some(viewer));

    Ok(PreparedManabrewSnapshot {
        game_id: game_id.into(),
        viewer,
        prompt_id,
        state,
        derived,
        actions,
        spell_costs,
        legal_actions_by_object,
        source_card_object,
        interaction,
    })
}

/// CR 500: turn steps and phases, as the protocol enumerates them.
///
/// Thirteen variants against the engine's twelve `Phase`s. The extra one is
/// `CombatFirstStrikeDamage`, and the engine's twelve is not a gap: CR 510.4
/// gives the phase a *second* combat damage step rather than a differently
/// named one, so one `Phase::CombatDamage` entered twice is the faithful
/// model. The adapter still never produces this variant — see
/// [`phase_step`] and `local.first-strike-damage-step-unproducible`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum StepKind {
    #[default]
    Untap,
    Upkeep,
    Draw,
    Main1,
    CombatBegin,
    CombatDeclareAttackers,
    CombatDeclareBlockers,
    CombatFirstStrikeDamage,
    CombatDamage,
    CombatEnd,
    Main2,
    EndOfTurn,
    Cleanup,
}

/// CR 400.1: the six zones the protocol models. The engine's `Zone::Stack` has
/// no counterpart — stack contents travel as `GameViewDto.stack`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ZoneKind {
    Battlefield,
    Hand,
    Library,
    Graveyard,
    Exile,
    Command,
}

/// CR 731.1: the day/night designation. The engine models this as
/// `Option<DayNight>`, where `None` means neither.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum DayTime {
    #[default]
    Neither,
    Day,
    Night,
}

/// A seat's standing in the game.
///
/// The engine records only `Player::is_eliminated` — it never persists *why* a
/// player left — so this adapter emits `Playing` or `Lost` and **never**
/// `Conceded`. See `local.player-concede-status-unsourceable`.
#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PlayerStatus {
    #[default]
    Playing,
    Lost,
    Conceded,
}

/// CR 122: player-borne counters. Named `Dto` to avoid colliding with the
/// engine's own `PlayerCounterKind`, whose variant set differs (the engine
/// tracks energy as a plain field, and spells `Radiation` as `Rad`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[serde(rename_all = "camelCase")]
pub enum PlayerCounterKindDto {
    Poison,
    Energy,
    Experience,
    Radiation,
    Ticket,
}

/// A non-mana resource tapped or released to help pay a cost.
///
/// Only `Convoke` is reachable from this engine (`GameAction::TapForConvoke`);
/// there is no engine action for Delve or Improvise, and none for releasing any
/// of the three. See `local.payment-resource-actions-missing`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum PaymentResourceKind {
    Convoke,
    Improvise,
    Delve,
}

/// The five conformance failure modes a conforming engine must be able to
/// report (`conformance.mdx:28-33, :50`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Hash)]
#[serde(rename_all = "camelCase")]
pub enum ProtocolErrorCode {
    StalePrompt,
    WrongPlayer,
    WrongPromptType,
    UnknownActionId,
    InvalidShape,
}

/// A wire-level rejection sent back to one client.
///
/// Distinct from [`AdapterError`], which is this crate's internal Rust failure
/// type; [`protocol_error_for`] maps one onto the other.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ProtocolError {
    pub code: ProtocolErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_id: Option<u32>,
}

/// CR 118.9 / 601.2b: alternative costs a spell may be cast for.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AlternativeCostKind {
    Flashback,
    Spectacle,
    Evoke,
    Dash,
    Blitz,
    Escape,
    Overload,
    Madness,
    Foretell,
    Emerge,
    Suspend,
    Morph,
    Megamorph,
    Bestow,
    Warp,
    SacrificeAlt,
    Plot,
    Awaken,
    Disturb,
    Harmonize,
    Freerunning,
    Impending,
    Mayhem,
    #[serde(rename = "moreThanMeetsTheEye")]
    MTMtE,
    Mutate,
    Prowl,
    Sneak,
    Surge,
    WebSlinging,
    Plotted,
}

/// How a card is being put onto the stack or the battlefield.
///
/// Display-only: the client echoes just `action_id`, which resolves through
/// [`ActionTableEntry`] back to the original `GameAction`, so `mode` never
/// round-trips. `BackFaceLand` is **unproducible** here — `GameAction::PlayLand`
/// carries no face discriminator (the MDFC face is a separate, later
/// `ChooseModalFace`), and inferring it from card data would be game logic in a
/// serialization boundary.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PlayCardMode {
    Normal,
    BackFaceLand,
    RoomRightSplit,
    Alternative { cost: AlternativeCostKind },
    StaticAlternative,
    ForetellExile,
    UnlockDoor,
}

/// A client decision that is not an answer to any open prompt.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum DirectiveInput {
    Concede,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct StateUpdate {
    pub game_view: GameViewDto,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct AgentPrompt {
    pub prompt_id: u32,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub deciding_player_id: String,
    /// The full source card, not just its id — so the recipient can render it
    /// even when the source lies outside their visible state. Built from raw
    /// engine state; see `PreparedManabrewSnapshot::source_card_object`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_card: Option<CardDto>,
    pub input: PromptInput,
}

/// One `(zone, owner)` bucket. Battlefield entries are bucketed by
/// **controller** rather than owner (CR 110.2), matching upstream.
///
/// `count` is the truthful total and may exceed `cards.len()` when the
/// recipient may not identify every card in the zone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ZoneDto {
    pub zone: ZoneKind,
    pub owner_id: String,
    /// Engine order. The library is top-first (the engine stores it front-first
    /// and the top card is `library.front()`), so index 0 is the top card.
    /// Every other zone is passed through in the engine's own order — notably
    /// the graveyard, which the engine appends to, so index 0 is the *oldest*
    /// card rather than the top of the pile.
    pub cards: Vec<CardView>,
    pub count: usize,
}

/// A card as one recipient may see it.
///
/// `Hidden` is for cards in a **hidden zone** whose identity the recipient may
/// not learn (a face-down exile, CR 406.3). A face-down *battlefield* permanent
/// is never `Hidden` — the permanent itself is public (CR 400.2 / CR 708.2), so
/// it travels as a `Visible` entry whose identity fields are redacted while its
/// public state (tapped, counters, damage) survives.
// `Visible` is the dominant variant — most cards in most zones are visible —
// and these views are built once per state update, serialized, and dropped.
// Boxing to even out the variants would trade one `Vec` allocation for a heap
// allocation per card, so the flat layout is deliberate.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "visibility",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum CardView {
    Visible(CardDto),
    Hidden { id: String },
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GameViewDto {
    pub game_id: String,
    pub turn: u32,
    pub step: StepKind,
    pub combat_assignments: Vec<CombatAssignmentDto>,
    pub active_player_id: String,
    pub priority_player_id: String,
    pub players: Vec<PlayerDto>,
    pub zones: Vec<ZoneDto>,
    pub stack: Vec<StackObjectDto>,
    pub game_over: bool,
    pub winner_id: Option<String>,
    pub monarch_id: Option<String>,
    pub initiative_holder_id: Option<String>,
    pub day_time: DayTime,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CombatAssignmentDto {
    pub blocker_id: String,
    pub attacker_id: String,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PlayerDto {
    pub id: String,
    pub name: String,
    pub status: PlayerStatus,
    pub is_human: bool,
    pub life: i32,
    pub counters: BTreeMap<PlayerCounterKindDto, u32>,
    pub mana_pool: BTreeMap<ManaColorDto, u32>,
    pub commander_damage: HashMap<String, i32>,
    pub has_city_blessing: bool,
    pub ring_level: i32,
    pub speed: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CardIdentity {
    pub name: String,
    pub set_code: String,
    pub card_number: String,
    pub is_token: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct CardDto {
    pub id: String,
    pub identity: CardIdentity,
    pub color: String,
    pub mana_cost: String,
    pub cmc: i32,
    pub types: Vec<String>,
    pub subtypes: Vec<String>,
    pub supertypes: Vec<String>,
    pub power: Option<String>,
    pub toughness: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_power: Option<i32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_toughness: Option<i32>,
    pub text: String,
    pub controller_id: String,
    pub owner_id: String,
    pub tapped: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_crewed: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_attacking: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attacking_player_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attack_target_id: Option<String>,
    pub keywords: Vec<String>,
    /// Keyed by the engine's canonical `CounterType` serialization key
    /// ("P1P1", "M1M1", "loyalty", …) — **not** its `display_phrase()` prose
    /// form ("+1/+1"), which is for player-facing text.
    pub counters: BTreeMap<String, u32>,
    pub damage: i32,
    pub summoning_sick: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_copy: bool,
    pub is_double_faced: bool,
    pub is_transformed: bool,
    pub is_face_down: bool,
    pub is_bestowed: bool,
    pub phased_out: bool,
    pub exerted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_ring_bearer: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    /// CR 712.4a / CR 730.2: the card ids merged under this top card — the
    /// engine's `GameObject::merged_components`, covering mutate and meld.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub merged_card_ids: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flashback_cost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kicker_cost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effective_mana_cost: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub madness_cost: Option<String>,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_madness_exiled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_plotted: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_warp_exiled: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub foil: bool,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub would_die_in_combat: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", default)]
pub struct StackObjectDto {
    pub id: String,
    pub source_id: String,
    pub controller_id: String,
    pub identity: CardIdentity,
    pub text: String,
    pub is_permanent_spell: bool,
    pub is_casting: bool,
    pub targets: Vec<TargetRefDto>,
}

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TargetingIntent {
    #[default]
    Damage,
    Destroy,
    Sacrifice,
    Exile,
    Bounce,
    Mill,
    Discard,
    Counter,
    Tap,
    Untap,
    Copy,
    Buff,
    Debuff,
    Heal,
    LoseLife,
    Reveal,
    Draw,
    Fetch,
    GainControl,
    Fight,
    Attach,
    Attack,
    Block,
    Hostile,
    Friendly,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum TargetKindDto {
    Player,
    Card,
    Spell,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct TargetRefDto {
    pub kind: TargetKindDto,
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<TargetingIntent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum PromptInput {
    ChooseAction(ChooseActionInput),
    PayManaCost(PayManaCostInput),
    Mulligan(MulliganInput),
    MulliganPutBack(MulliganPutBackInput),
    ChooseAttackers(ChooseAttackersInput),
    ChooseBlockers(ChooseBlockersInput),
    ChooseBoardTargets(ChooseBoardTargetsInput),
    ChooseBoolean(ChooseBooleanInput),
    ChooseCards(ChooseCardsInput),
    ChooseColor(ChooseColorInput),
    ChooseCombatDamageAssignment(ChooseCombatDamageAssignmentInput),
    ChooseDamageAssignmentOrder(ChooseDamageAssignmentOrderInput),
    ChooseFromSelection(ChooseFromSelectionInput),
    ChooseNumber(ChooseNumberInput),
    RevealCards(RevealCardsInput),
    Scry(ScryInput),
    Reorder(ReorderInput),
    DiceRolled(DiceRolledInput),
    GameOver(GameOverInput),
}

/// A client's answer to an open prompt, as a **two-level** union: the outer tag
/// names the prompt family, and the family's own output nests under `output`.
///
/// Wire form: `{"type":"chooseNumber","output":{"type":"numberDecision","chosenNumber":3}}`
///
/// The nesting is deliberate and **asymmetric with [`PromptInput`]**, which is
/// internally tagged with no `content` and therefore *flattens*
/// (`{"type":"chooseAction","actions":[…]}`). Adding `content` to `PromptInput`
/// for symmetry would silently break every prompt.
///
/// Carrying the family in the tag is also what removes the old
/// `state.waiting_for` sniffing: an `act` output no longer has to be guessed
/// between priority and mana payment.
///
/// There is no `GameOver` arm — that prompt is terminal and takes no response.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", content = "output", rename_all = "camelCase")]
pub enum PromptOutput {
    Mulligan(MulliganOutput),
    MulliganPutBack(MulliganPutBackOutput),
    ChooseAction(ChooseActionOutput),
    ChooseAttackers(ChooseAttackersOutput),
    ChooseBlockers(ChooseBlockersOutput),
    ChooseBoardTargets(ChooseBoardTargetsOutput),
    ChooseBoolean(ChooseBooleanOutput),
    ChooseFromSelection(ChooseFromSelectionOutput),
    RevealCards(RevealCardsOutput),
    Scry(ScryOutput),
    ChooseColor(ChooseColorOutput),
    ChooseNumber(ChooseNumberOutput),
    ChooseDamageAssignmentOrder(ChooseDamageAssignmentOrderOutput),
    ChooseCombatDamageAssignment(ChooseCombatDamageAssignmentOutput),
    PayManaCost(PayManaCostOutput),
    ChooseCards(ChooseCardsOutput),
    Reorder(ReorderOutput),
    DiceRolled(DiceRolledOutput),
}

/// Everything a client can send the engine.
///
/// This is the single client→engine union: a prompt answer, or a directive that
/// belongs to no prompt.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ClientToServerMessage {
    Response {
        prompt_id: u32,
        /// Upstream names this field `action`, not `output`.
        action: PromptOutput,
    },
    Directive {
        directive: DirectiveInput,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptPresentation {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default)]
    pub targets: Vec<TargetRefDto>,
}

/// CR 105.1: the five colors plus colorless. Ordered/hashable because it keys
/// `PlayerDto::mana_pool`'s `BTreeMap`.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ManaColorDto {
    #[serde(rename = "W")]
    White,
    #[serde(rename = "U")]
    Blue,
    #[serde(rename = "B")]
    Black,
    #[serde(rename = "R")]
    Red,
    #[serde(rename = "G")]
    Green,
    #[serde(rename = "C")]
    Colorless,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ManaDto {
    pub color: ManaColorDto,
    pub amount: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ActivatableAbilityInfo {
    pub card_id: String,
    pub ability_index: usize,
    pub description: String,
    pub is_mana_ability: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub produced_mana: Option<Vec<ManaDto>>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum AvailableActionKind {
    Cast {
        card_id: String,
        mode: PlayCardMode,
        label: String,
    },
    ActivateAbility(ActivatableAbilityInfo),
    UndoMana {
        card_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailableAction {
    pub id: String,
    #[serde(flatten)]
    pub kind: AvailableActionKind,
}

/// A single move available *while paying a cost* — the mana-payment analogue of
/// [`AvailableActionKind`].
///
/// `PayLife` is emitted for exactly one thing: a Phyrexian payment route that
/// spends life (CR 107.4f), advertised from the engine's own
/// `SubmitPhyrexianChoices` legal actions so the echoed id always resolves.
/// `UseResource` for Delve or Improvise and every `ReleaseResource` form stay
/// unemitted — no engine action backs them, and advertising an id the engine
/// would then reject violates the `UnknownActionId` obligation. See
/// `local.payment-resource-actions-missing`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PaymentActionKind {
    ActivateManaAbility(ActivatableAbilityInfo),
    UndoMana {
        card_id: String,
    },
    UseResource {
        card_id: String,
        resource: PaymentResourceKind,
    },
    ReleaseResource {
        card_id: String,
        resource: PaymentResourceKind,
    },
    PayLife {
        amount: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PaymentAction {
    pub id: String,
    #[serde(flatten)]
    pub kind: PaymentActionKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum AttackTargetKind {
    Player,
    Planeswalker,
    Battle,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttackTargetDto {
    pub id: String,
    pub label: String,
    pub kind: AttackTargetKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttackAssignment {
    pub attacker_id: String,
    pub target_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlockAssignment {
    pub blocker_id: String,
    pub attacker_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CombatDamageAssignmentEntry {
    pub assignee_id: String,
    pub damage: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseActionInput {
    pub actions: Vec<AvailableAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PassUntil {
    pub player_id: String,
    pub phase: StepKind,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseActionOutput {
    Pass {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<PassUntil>,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        exhaust_stack: bool,
    },
    RestoreSnapshot {
        checkpoint_id: u64,
    },
    Act {
        action_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PayManaCostInput {
    pub presentation: PromptPresentation,
    pub card_id: String,
    pub card_name: String,
    pub mana_cost: String,
    pub can_confirm_from_pool: bool,
    pub actions: Vec<PaymentAction>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PayManaCostOutput {
    Act {
        action_id: String,
    },
    Pay {
        #[serde(default)]
        auto: bool,
    },
    Cancel,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MulliganInput {
    pub hand_card_ids: Vec<String>,
    pub mulligan_count: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MulliganOutput {
    MulliganDecision { keep: bool },
    MulliganUseSerumPowder { card_id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct MulliganPutBackInput {
    pub hand_card_ids: Vec<String>,
    pub cards: Vec<CardDto>,
    pub count: usize,
    /// The earmarked Serum Powder object committed to a pending
    /// `UseSerumPowder` continuation, if any — the client must not offer it
    /// as selectable in the bottom-cards picker. `None` for both `Keep`
    /// resolutions and the (unrelated) `OpeningHandBottomCards` phase.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub excluded_card_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum MulliganPutBackOutput {
    MulliganPutBackDecision { card_ids: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttackerOptionDto {
    pub attacker_id: String,
    pub valid_target_ids: Vec<String>,
    pub must_attack: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseAttackersInput {
    pub attackers: Vec<AttackerOptionDto>,
    pub attack_targets: Vec<AttackTargetDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseAttackersOutput {
    DeclareAttackers { assignments: Vec<AttackAssignment> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlockableAttackerDto {
    pub attacker_id: String,
    pub valid_blocker_ids: Vec<String>,
    pub min_blockers: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_blockers: Option<u32>,
    pub must_be_blocked: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseBlockersInput {
    pub attackers: Vec<BlockableAttackerDto>,
    pub available_blocker_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseBlockersOutput {
    DeclareBlockers { assignments: Vec<BlockAssignment> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseBoardTargetsInput {
    pub presentation: PromptPresentation,
    pub candidates: Vec<TargetRefDto>,
    #[serde(default)]
    pub hostile: bool,
    pub intent: TargetingIntent,
    pub min_targets: i32,
    pub max_targets: i32,
    pub chosen_targets: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseBoardTargetsOutput {
    BoardTargets { chosen: Vec<TargetRefDto> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseBooleanInput {
    pub presentation: PromptPresentation,
    pub confirm_label: String,
    pub deny_label: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseBooleanOutput {
    Decision { value: bool },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseCardsInput {
    pub presentation: PromptPresentation,
    pub cards: Vec<CardDto>,
    pub min: usize,
    pub max: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseCardsOutput {
    ChooseCardsDecision { chosen_card_ids: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseColorInput {
    pub presentation: PromptPresentation,
    pub valid_colors: Vec<String>,
    pub amount: u32,
    pub repeat_allowed: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseColorOutput {
    ColorDecision {
        chosen_colors: BTreeMap<String, u32>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseCombatDamageAssignmentInput {
    pub attacker_id: String,
    pub blocker_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub defender_id: Option<String>,
    pub total_damage: i32,
    pub attacker_has_deathtouch: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseCombatDamageAssignmentOutput {
    CombatDamageAssignmentDecision {
        assignments: Vec<CombatDamageAssignmentEntry>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseDamageAssignmentOrderInput {
    pub attacker_id: String,
    pub blocker_ids: Vec<String>,
    pub blocker_cards: Vec<CardDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseDamageAssignmentOrderOutput {
    DamageAssignmentOrderDecision { ordered_blocker_ids: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct SelectionOption {
    pub label: String,
    pub weight: usize,
    pub can_repeat: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseFromSelectionInput {
    pub presentation: PromptPresentation,
    pub options: Vec<SelectionOption>,
    pub min_total: usize,
    pub max_total: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseFromSelectionOutput {
    SelectionDecision { chosen_indices: Vec<usize> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ChooseNumberInput {
    pub presentation: PromptPresentation,
    pub min: i32,
    pub max: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ChooseNumberOutput {
    NumberDecision { chosen_number: Option<i32> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct RevealCardsInput {
    pub presentation: PromptPresentation,
    pub cards: Vec<CardDto>,
    pub zone: ZoneKind,
    pub owner_player_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RevealCardsOutput {
    RevealCardsAcknowledged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum ScryDestination {
    LibraryTop,
    LibraryBottom,
    Graveyard,
    Exile,
    Hand,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ScryInput {
    pub presentation: PromptPresentation,
    pub cards: Vec<CardDto>,
    pub zones: Vec<ScryDestination>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ScryOutput {
    ScryDecision { zone_card_ids: Vec<Vec<String>> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReorderItem {
    pub id: String,
    pub card: CardDto,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oracle: Option<String>,
}

/// Renamed from `ReorderCardsInput`: the wire tag changed from `reorderCards`
/// to `reorder` in v2, and the Rust name should not contradict it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ReorderInput {
    pub presentation: PromptPresentation,
    pub items: Vec<ReorderItem>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ReorderOutput {
    ReorderDecision { ordered_ids: Vec<String> },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiceRollEntry {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub player_id: Option<String>,
    pub natural_results: Vec<i32>,
    pub final_results: Vec<i32>,
    pub ignored_rolls: Vec<i32>,
    #[serde(default)]
    pub highlighted: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DiceRolledInput {
    pub presentation: PromptPresentation,
    pub sides: i32,
    pub rolls: Vec<DiceRollEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_card_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DiceRolledOutput {
    DiceRolledAcknowledged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct GameOverInput {}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct UnsupportedCapability {
    pub code: &'static str,
    pub area: &'static str,
    pub reason: &'static str,
    pub suggested_protocol_extension: &'static str,
}

pub fn unsupported_protocol_capabilities() -> &'static [UnsupportedCapability] {
    &UNSUPPORTED_PROTOCOL_CAPABILITIES
}

/// Gaps between this engine and protocol 2.0.0, machine-readable.
///
/// `upstream.` = the protocol has no primitive for something the engine can do.
/// `local.` = the protocol has the primitive but this engine cannot source it.
static UNSUPPORTED_PROTOCOL_CAPABILITIES: [UnsupportedCapability; 82] = [
    UnsupportedCapability {
        code: "upstream.object-selection-missing",
        area: "prompts",
        reason: "The protocol has TargetRef for rules targets but no generic ObjectRef selection primitive for non-target choices.",
        suggested_protocol_extension: "Add ObjectRef plus ChooseObjectsInput/objectsChosen with a purpose field.",
    },
    UnsupportedCapability {
        code: "upstream.multi-destination-partition-missing",
        area: "prompts",
        reason: "Re-derived; the arity framing was wrong twice over, and the code name is kept only because renaming a published capability code is itself a contract break. (a) Arity is not the constraint: ScryInput::zones is an unbounded Vec<ScryDestination> and ScryOutput::ScryDecision::zone_card_ids is a Vec<Vec<String>> positional against it, with no length validation anywhere in the pinned crate, so N destinations are already expressible. (b) Phase has no 3+-destination prompt to express: its only partitioning pauses are WaitingFor::SearchPartitionChoice { primary_destination: Zone, rest_destination: Zone } (CR 701.23a + CR 608.2c, cultivate-class) and WaitingFor::EffectZoneChoice { zone: Zone, destination: Option<Zone> }, both binary. The real gap is the destination VOCABULARY: ScryDestination has five values (LibraryTop | LibraryBottom | Graveyard | Exile | Hand) while the engine's Zone has seven (Library, Hand, Battlefield, Graveyard, Stack, Exile, Command). Cultivate-class searches send the primary cards to the BATTLEFIELD, which ScryDestination cannot name — so SearchPartitionChoice cannot ride Scry regardless of arity. (The earlier surveil citation here read CR 701.42a; 701.42 is Meld. Surveil is CR 701.25a.)",
        suggested_protocol_extension: "Widen ScryDestination to cover Battlefield (and state whether an entering-tapped rider belongs on the destination or on a sibling field), rather than defining new arity rules that nothing needs. Battlefield is the one destination that turns a look-then-distribute prompt into an unrepresentable one.",
    },
    UnsupportedCapability {
        code: "upstream.mana-pool-entries-missing",
        area: "mana",
        reason: "v2's PaymentAction covers activating and undoing mana abilities, but the pool is still a per-color count: individual pool entries carrying restriction metadata (and therefore pin/unpin of a specific entry) cannot be represented.",
        suggested_protocol_extension: "Add PoolMana state objects with restriction metadata, plus pin/unpin payment actions keyed on a pool entry id.",
    },
    UnsupportedCapability {
        code: "upstream.controlled-turn-subject-missing",
        area: "authorization",
        reason: "AgentPrompt has decidingPlayerId for the submitter but no metadata for the controlled/semantic player.",
        suggested_protocol_extension: "Add optional subjectPlayerId/controlledPlayerId to AgentPrompt.",
    },
    UnsupportedCapability {
        code: "upstream.display-sequencing-missing",
        area: "display",
        reason: "Display/log/snapshot protocol messages do not define stable event ids, state sequence numbers, audience, or version negotiation.",
        suggested_protocol_extension: "Add display event ids, stateSeq, audience fields, and capability negotiation.",
    },
    UnsupportedCapability {
        code: "local.deck-dto-not-implemented",
        area: "deck",
        reason: "This compatibility crate only adapts live game state and prompts today.",
        suggested_protocol_extension: "Implement the pinned deck DTO import/export separately.",
    },
    UnsupportedCapability {
        code: "local.room-relay-not-implemented",
        area: "transport",
        reason: "RelayMessage models the documented envelope kinds, but this crate drives no room: roomRelay payloads are implementation-defined, and snapshot restore (ChooseActionOutput::RestoreSnapshot) has no engine counterpart.",
        suggested_protocol_extension: "Define a roomRelay payload contract, and specify whether restoreSnapshot requires an engine-backed checkpoint store.",
    },
    UnsupportedCapability {
        code: "local.prompt-family-display-acks-unsupported",
        area: "prompts",
        reason: "Corrected: the previous text claimed Phase has no matching WaitingFor for RevealCards. It does — WaitingFor::RevealChoice { cards, filter, optional, decline_runs_continuation } (game_state.rs). The mismatch is the response payload, not the state's existence. RevealChoice is answered by GameAction::SelectCards { cards }: a normal reveal picks exactly one card, and under `optional` an EMPTY selection is not a no-op but an explicit decline that runs the source's decline branch (CR 701.20a). RevealCardsOutput has exactly one variant, RevealCardsAcknowledged, a bare ack with no card payload — so routing RevealChoice through RevealCards would submit an empty selection every time, silently declining every optional reveal and submitting an illegal count for every mandatory one. The correct home is ChooseCards (min 0 when optional else 1, max 1); that mapping is unwritten, so RevealChoice currently falls to local.prompt-unsupported. DiceRolled is a separate case and is genuinely unreachable: none of the 127 WaitingFor variants reports a die roll, and game/effects/roll_die.rs sets no waiting_for at all — die results are applied inline, so there is no decision point to acknowledge.",
        suggested_protocol_extension: "None needed upstream for reveals — ChooseCards already fits, and closing it is adapter work. For DiceRolled, treat it as a display event with audience and sequencing metadata rather than a prompt, since no engine pause backs it.",
    },
    UnsupportedCapability {
        code: "local.library-arrangement-reorder-unsupported",
        area: "prompts",
        reason: "Narrowed after verification: Reorder IS emitted — trigger ordering (CR 603.3b) maps to ReorderInput with the item id carrying the trigger's index. Still unmapped are library-arrangement reorders (ArrangePlanarDeckTopChoice, RevealUntilKeptChoice), which combine an ordering with a keep/discard split that ReorderOutput's single ordered_ids list cannot express.",
        suggested_protocol_extension: "None needed for pure orderings. For ordering-plus-partition, clarify whether Reorder may be composed with a preceding ChooseCards rather than growing a new family.",
    },
    UnsupportedCapability {
        code: "local.non-target-selection-unsupported",
        area: "prompts",
        reason: "Corrected after auditing each named prompt against the protocol rather than against this list. Surveil, discard, optional triggers (CR 603.12), and unless-costs (CR 118.12) DID have exact upstream shapes — Scry+zones, ChooseCards, and ChooseBoolean respectively — and are now mapped. What genuinely lacks a shape is selection over battlefield permanents by an aggregate constraint (keep-with-total-power, keep-exact-permanents) and pay-combat-cost, because ChooseBoardTargets carries only min/max counts, not a summed-attribute bound.",
        suggested_protocol_extension: "Give ChooseBoardTargets an optional aggregate constraint (attribute + comparator + value) so 'keep creatures with total power N or less' is expressible without a new family.",
    },
    UnsupportedCapability {
        code: "local.blocker-damage-banding-unsupported",
        area: "combat",
        reason: "Current upstream combat damage assignment input is attacker-oriented and cannot safely express blocker/banding damage assignment.",
        suggested_protocol_extension: "Generalize combat damage assignment around damageSourceId, assigneeIds, assignmentControllerId, and reason.",
    },
    UnsupportedCapability {
        code: "local.pass-until-unsupported",
        area: "responses",
        reason: "Phase can pass current priority through this adapter but does not yet map Manabrew pass-until stops to engine auto-pass settings.",
        suggested_protocol_extension: "Clarify whether pass.until is advisory or requires an engine-backed phase-stop/auto-pass contract.",
    },
    UnsupportedCapability {
        code: "local.auto-pay-unsupported",
        area: "mana",
        reason: "Phase requires explicit mana payment finalization; pay.auto asks the client's peer to choose which sources to tap, which is a planning decision this adapter must not make.",
        suggested_protocol_extension: "Define auto-pay as a separate engine-planner request that returns the chosen PaymentAction sequence.",
    },
    UnsupportedCapability {
        code: "local.exhaust-stack-pass-unsupported",
        area: "responses",
        reason: "v2 added ChooseActionOutput::Pass.exhaustStack (pass until the stack empties). Like pass.until it is a multi-window intent, and Phase's PassPriority yields exactly one priority window.",
        suggested_protocol_extension: "Clarify whether exhaustStack is advisory or requires an engine-backed auto-pass contract, alongside pass.until.",
    },
    UnsupportedCapability {
        code: "local.meld-pair-choice-unsupported",
        area: "prompts",
        reason: "The pinned protocol has no typed choice for selecting one physical meld pair from multiple live-name candidates.",
        suggested_protocol_extension: "Add a non-target object-pair choice carrying stable card ids.",
    },
    UnsupportedCapability {
        code: "local.entry-attack-target-choice-unsupported",
        area: "combat",
        reason: "The pinned protocol has no response shape for choosing the player, planeswalker, or battle attacked by an entering creature.",
        suggested_protocol_extension: "Add an entry-attack destination choice using the existing attack-target reference shape.",
    },
    UnsupportedCapability {
        code: "local.zone-opponent-chooser-unsupported",
        area: "prompts",
        reason: "The pinned protocol has no typed choice for the controller picking which opponent makes a zone choice (CR 608.2d, e.g. Plargg and Nassari's 'an opponent chooses').",
        suggested_protocol_extension: "Add a non-target opponent-picker choice carrying candidate player ids, mirroring the clash opponent selection shape.",
    },
    // --- Gaps introduced by, or first surfaced during, the 2.0.0 migration ---
    UnsupportedCapability {
        code: "local.player-concede-status-unsourceable",
        area: "state",
        reason: "PlayerStatus distinguishes lost from conceded, but Phase records only Player::is_eliminated and never persists why a player left. Every eliminated player is therefore reported as Lost; Conceded is never emitted rather than guessed.",
        suggested_protocol_extension: "None needed upstream — closing this requires Phase to persist an elimination reason, which is an engine change out of scope for a serialization adapter.",
    },
    UnsupportedCapability {
        code: "local.first-strike-damage-step-unproducible",
        area: "state",
        reason: "Corrected: the previous text said Phase models the whole of CR 510 as one step and inferred the first-strike step is unmodelled. Phase models it. CR 510.4 does not define a distinct step — when a first/double striker is in combat the phase gets a SECOND combat damage step, i.e. two instances of the same step — and the engine mirrors that exactly: one Phase::CombatDamage entered twice, discriminated by CombatState::first_strike_done (combat.rs, pub, reachable via the pub GameState::combat) plus a private SubStep::FirstStrike. What blocks emission is narrower: phase_step() receives only a Phase and cannot see that flag, and deciding whether a first-strike step is PENDING needs the participant set from combat_first_strike_participants(), which is private. Re-deriving participants here would be game logic in a serialization boundary.",
        suggested_protocol_extension: "None needed upstream — and no Phase split either. Closing this needs one engine accessor exposing the current combat-damage sub-step (the state already exists), after which phase_step's Phase-only signature is the last thing in the way.",
    },
    UnsupportedCapability {
        code: "local.play-card-mode-fidelity-gaps",
        area: "actions",
        reason: "Labelling only — these plays are reachable. CastSpellForFree, CastSpellAsMiracle, and PlayFaceDown carry PlayCardMode::Normal because v2 has no free-cast mode and no Miracle alternative cost, and PlayFaceDown carries no discriminator between morph, megamorph, and disguise. The human-facing semantic is not lost: AvailableActionKind::Cast::label is free text and already reads 'Cast with miracle'. Only programmatic mode discrimination is unavailable.",
        suggested_protocol_extension: "Add AlternativeCostKind::Miracle and a free-cast PlayCardMode; give face-down plays a mode discriminator (disguise also has no AlternativeCostKind).",
    },
    UnsupportedCapability {
        code: "local.back-face-land-mode-unproducible",
        area: "actions",
        reason: "PlayCardMode::BackFaceLand cannot be produced: GameAction::PlayLand carries no face field, and the MDFC front/back decision is a separate later action (ChooseModalFace, CR 712.12). Inferring the face from card data would be game logic in a serialization boundary, so every land play is advertised as Normal.",
        suggested_protocol_extension: "Clarify whether backFaceLand is meant to be decided at advertisement time; if so, the engine would need to resolve the face before offering the play.",
    },
    UnsupportedCapability {
        code: "local.mdfc-face-choice-unsupported",
        area: "prompts",
        reason: "Advertising PlayLand (previously suppressed, making every land play invisible) opens a path to WaitingFor::ModalFaceChoice, for which no prompt family exists — CR 712.12's front/back choice has no counterpart in the nineteen PromptInput families.",
        suggested_protocol_extension: "Add a modal-face choice prompt carrying the two candidate faces.",
    },
    UnsupportedCapability {
        code: "local.harmonize-tap-unsupported",
        area: "mana",
        reason: "Scope note: this covers only the TAP, not harmonize as a whole. The harmonize CAST (CR 702.180a, Phase's CastingVariant::Harmonize) has an exact counterpart in AlternativeCostKind::Harmonize and needs nothing added. What has no home is HarmonizeTap (CR 702.180b), a cost-reduction tap during payment structurally analogous to convoke, where PaymentResourceKind is exactly Convoke | Improvise | Delve.",
        suggested_protocol_extension: "Add PaymentResourceKind::Harmonize for the tap. The cast side needs no extension.",
    },
    UnsupportedCapability {
        code: "local.payment-resource-actions-missing",
        area: "mana",
        reason: "Of PaymentResourceKind's three resources only Convoke has an engine action (TapForConvoke). There is no GameAction for Delve or Improvise, and no release/undo action for any of the three, so UseResource{delve|improvise} and every ReleaseResource form are defined for wire completeness and never advertised.",
        suggested_protocol_extension: "None needed upstream — closing this requires Phase to add delve, improvise, and release actions.",
    },
    UnsupportedCapability {
        code: "local.dungeon-room-unsupported",
        area: "actions",
        reason: "ChooseDungeon, ChooseDungeonRoom, UnlockRoomDoor, and ChooseRoomDoor are all unsupported, and available_actions filters unsupported actions out — so a Room's door can never be unlocked through this adapter. PlayCardMode::UnlockDoor is consequently never produced either. Deferred with the Rooms/dungeon feature rather than partially mapped.",
        suggested_protocol_extension: "None needed upstream — v2 already models the UnlockDoor mode; closing this is adapter work once the Rooms feature lands.",
    },
    UnsupportedCapability {
        code: "local.room-right-split-mode-unproducible",
        area: "actions",
        reason: "PlayCardMode::RoomRightSplit cannot be produced: no Phase cast action carries a discriminator for which half of a split Room is being cast, the same structural gap that makes BackFaceLand unproducible. Phase does model the halves (RoomDoor::Left/Right), but only on UnlockRoomDoor and ChooseRoomDoor — neither of which is a cast, and both of which are themselves unsupported — so the half is never known at advertisement time and every cast is advertised as Normal rather than guessed.",
        suggested_protocol_extension: "Clarify whether roomRightSplit is decided at advertisement time; if so the engine must resolve the half before offering the play.",
    },
    UnsupportedCapability {
        code: "local.counter-key-vocabulary-unverifiable",
        area: "state",
        reason: "CardDto.counters keys are only partially verifiable against upstream. P1P1 and M1M1 are confirmed aligned. Every other key is unverifiable: upstream derives its keys with format!(\"{k:?}\") over a CounterType enum that is not published, and that enum carries a Named(String) variant plus further unnamed variants, so its documented example key form contradicts what its own producer emits. Phase emits its canonical CounterType::as_str() rather than guessing upstream identifiers or reproducing a Debug-formatted wrapper.",
        suggested_protocol_extension: "Give CardDto.counters a typed key (or a documented string vocabulary) instead of Debug-formatting a private enum, so both ends can agree on counter names beyond +1/+1 and -1/-1.",
    },
    // --- Codes the adapter emits that were previously undeclared -------------
    //
    // Every entry below was measured, not guessed: `rg -o '"(local|upstream)\.
    // [a-z0-9-]+"'` over this file found 67 codes emitted at live call sites
    // against 29 declared, leaving 51 that a client could receive and then fail
    // to look up. An undeclared code is worse than no code — it resolves to
    // nothing at the far end.
    //
    // Two facts hold for the whole block and are not repeated in each reason.
    // (1) `AvailableActionKind` has exactly three variants (Cast,
    //     ActivateAbility, UndoMana) and `PromptInput` exactly nineteen
    //     families; "no home" below always means "not among those".
    // (2) `available_actions()` is built only for `WaitingFor::Priority`, and it
    //     drops Unsupported conversions. So a code from a `convert_available_
    //     action` arm whose action answers a NON-priority decision reaches a
    //     client only through `advertised_action_by_id` — i.e. when a stale or
    //     invented action id is echoed. Codes whose action IS a priority-window
    //     play (equip/crew/station/saddle, the planar die, the companion special
    //     action, and the two copy-casts) say so explicitly, because for those
    //     the filtering is real functional loss rather than a stale-id guard.
    //
    // None of these say "Phase does not support X". Each names the population
    // searched. Where Phase has a `GameAction` for a mechanic, that action's
    // existence is itself the proof Phase models it.
    UnsupportedCapability {
        code: "local.prompt-unsupported",
        area: "prompts",
        reason: "Narrowed: this is no longer the wildcard arm's blanket answer. The wildcard now routes to interaction_prompt(), which serves any waiting state the engine projects as a finite ExactChoices list — the largest response class by far — so an unnamed WaitingFor is no longer unmapped by default. What remains here is the degenerate projection: no opportunity for this viewer, or an opportunity whose choice list is empty. Neither is a missing protocol shape; both mean there is nothing for this seat to answer, which is an engine or sequencing condition rather than a capability gap.",
        suggested_protocol_extension: "None needed upstream. If this is observed while the seat genuinely owes a decision, it is an interaction-projection defect to fix, not a family to add.",
    },
    UnsupportedCapability {
        code: "local.interaction-simultaneous-decisions-unmapped",
        area: "prompts",
        reason: "The engine opens one interaction slot per semantic owner, and a single viewer can be the authorized submitter for more than one of them — a decision both seats owe at once, answered independently. The protocol carries one prompt per message and one answer per prompt, so there is no shape for 'here are two decisions, answer both'. Serving only the first would answer one seat and silently drop the other, which is why this fails closed instead. Note this is a projection-level count, not a mechanic: the same waiting state produces one opportunity in the ordinary case and lands here only when authority for several owners collapses onto one viewer.",
        suggested_protocol_extension: "Either allow a batch of prompts to be outstanding for one recipient with independent prompt ids, or state that the server must serialize simultaneous decisions into successive prompts. The second needs no wire change and is likely the cheaper answer.",
    },
    UnsupportedCapability {
        code: "local.interaction-schema-response-unmapped",
        area: "prompts",
        reason: "The engine projects a decision either as a finite ExactChoices list or as a schema: a response spec plus candidates. interaction_prompt() now maps four generically — ExactChoices (a one-of list), Select (an unordered subset, whose count bounds ChooseFromSelection's min/max totals express exactly), Sequence (an ordered subset; chosen_indices is itself ordered, so the order survives), and Number (a range, which is ChooseNumber verbatim). What remains fails closed because its payload is none of those things: not a count over a list, not an order over a list, not a scalar. AssignAmounts distributes a total across candidates — a per-candidate amount, which ChooseCombatDamageAssignment shapes but names as damage, so reusing it would misdescribe counter distribution. GroupedSequence carries per-group min/max, DeckPartition splits a pool in two, and ManaGroups, Text, Shortcut and ShortcutReply have no current family at all. Flattening any of them into a selection would drop the very constraint that makes the answer legal.",
        suggested_protocol_extension: "Two shapes would close most of it: a per-candidate amount distribution with a required total (covers AssignAmounts, and generalizes ChooseCombatDamageAssignment rather than competing with it), and per-option group constraints on ChooseFromSelection (covers GroupedSequence, and DeckPartition as the two-group case). The Text and Shortcut families are genuinely absent and need their own design conversation.",
    },
    UnsupportedCapability {
        code: "local.interaction-aggregate-bound-unmapped",
        area: "prompts",
        reason: "A Select schema whose SelectionConstraint is Aggregate rather than Count. The bound is a sum over a chosen attribute of the selected objects — 'keep permanents with total power 4 or less' — not a number of objects, so no min/max count is equivalent to it. ChooseFromSelection carries min_total/max_total as counts only, and rendering an aggregate bound as an unbounded count would advertise illegal selections as legal, which is worse than refusing. Distinct from local.interaction-schema-response-unmapped because the spec IS mapped: only this one constraint variant within it is not.",
        suggested_protocol_extension: "Give ChooseFromSelection an optional aggregate bound over a named option weight — SelectionOption already carries `weight`, so the wire is one comparator and one amount away from expressing this without a new family.",
    },
    UnsupportedCapability {
        code: "local.target-slot-missing",
        area: "prompts",
        reason: "Structural guard, not a gap. TargetSelection/TriggerTargetSelection advance one slot at a time and the prompt is built for target_slots[selection.selected_slots.len()]. This code fires only if that index is out of range, which means the engine handed the adapter a selection already past its slot list. No protocol shape is missing.",
        suggested_protocol_extension: "None needed upstream — if this is ever observed it is an engine or ordering defect to fix, not a capability to add.",
    },
    UnsupportedCapability {
        code: "local.reserved-prompt-id-zero",
        area: "prompts",
        reason: "Protocol conformance guard. Prompt id 0 is reserved upstream for engine-synthesized absent-player defaults (timeout/disconnect) and may never be accepted as a real answer, so build_prompt() refuses to emit a prompt carrying it rather than emitting one no client could answer. Callers using prepare_snapshot() (which defaults to id 0) get this; prepare_snapshot_with_prompt_id() with a non-zero id does not.",
        suggested_protocol_extension: "None needed upstream — the reservation is upstream's and this honors it.",
    },
    UnsupportedCapability {
        code: "local.named-choice-unsupported",
        area: "prompts",
        reason: "Split by vocabulary after reading the enum rather than assuming. Both WaitingFor::NamedChoice and WaitingFor::CostTypeChoice carry { choice_type: ChoiceType, options: Vec<String> }, and ChoiceType has eighteen variants. Most are CLOSED sets the engine already enumerates into `options` — CreatureType, Color, CardType, LandType, BasicLandType, Keyword, CounterKind, Opponent, Player, OddOrEven, TwoColors, Labeled, NumberRange — and every one of those fits ChooseFromSelection (or ChooseColor / ChooseNumber) with no extension at all; CostTypeChoice is entirely in this group. Only CardName, Word, Artist, CardPredicate, and CardPredicateGuess are open vocabularies, and for those none of the nineteen families carries a free-text answer: ChooseCards needs CardDtos, ChooseFromSelection needs enumerated labels, ChooseBoardTargets needs TargetRefs. So this is mostly unwritten mapping and only partly a missing shape.",
        suggested_protocol_extension: "None needed for the closed-vocabulary majority — that is adapter work. For CardName / Word / Artist, add a text-answer family (or specify that the producer must supply a bounded candidate list, which is only possible where the card's Oracle text restricts the name set).",
    },
    UnsupportedCapability {
        code: "local.dig-unsupported",
        area: "prompts",
        reason: "WaitingFor::DigChoice (look at the top N, keep up to keep_count, the rest go elsewhere) is a look-then-distribute decision, which is the Scry family's shape. Whether it maps depends on the two destination fields, both typed as the full engine Zone: kept_destination: Option<Zone> and rest_destination: Option<Zone>. A dig whose destinations fall inside ScryDestination's five values maps today with no extension; a dig whose kept_destination is Battlefield (the enter_tapped field exists precisely for those) does not, because ScryDestination cannot name it. Same root cause as upstream.multi-destination-partition-missing. `selectable_cards` is an additional wrinkle: Scry has no per-card selectability flag, so an unfiltered client could pick a greyed-out card.",
        suggested_protocol_extension: "Widen ScryDestination to cover Battlefield (see upstream.multi-destination-partition-missing) and give ScryInput a per-card selectable flag so filtered digs cannot be answered illegally.",
    },
    UnsupportedCapability {
        code: "local.keep-with-total-power-unsupported",
        area: "prompts",
        reason: "WaitingFor::KeepWithinTotalPowerChoice selects permanents under an AGGREGATE bound (keep creatures with total power N or less). ChooseBoardTargets and ChooseCards both carry only min/max COUNTS, so a client cannot be told the constraint it must satisfy and the engine would have to reject otherwise well-formed answers. Same root cause as local.non-target-selection-unsupported, which is the entry carrying the proposed extension.",
        suggested_protocol_extension: "Give ChooseBoardTargets an optional aggregate constraint (attribute + comparator + value); see local.non-target-selection-unsupported.",
    },
    UnsupportedCapability {
        code: "local.keep-exact-permanents-unsupported",
        area: "prompts",
        reason: "WaitingFor::KeepExactPermanentsChoice is the count-exact sibling of the aggregate case above. It is listed separately because it emits a separate code, not because it is a separate gap: both are selection-under-constraint over battlefield permanents.",
        suggested_protocol_extension: "Covered by the aggregate-constraint extension proposed on local.non-target-selection-unsupported.",
    },
    UnsupportedCapability {
        code: "local.cost-prevention-unsupported",
        area: "prompts",
        reason: "Emitted from two sites for one gap: the WaitingFor::UnlessPaymentChooseCost prompt and the GameAction::ChooseUnlessCostBranch answer. CR 118.12's plain form IS mapped — it is a yes/no and rides ChooseBoolean. What is not is the branching form, where the player picks AMONG several offered costs. That is a selection, and folding it into ChooseBoolean would misreport the question by silently collapsing three or more branches into two.",
        suggested_protocol_extension: "None needed upstream — ChooseFromSelection already takes labelled options with min/max totals and is the right home. This is adapter work.",
    },
    UnsupportedCapability {
        code: "local.pay-combat-cost-unsupported",
        area: "combat",
        reason: "Emitted from two sites for one gap: the WaitingFor::CombatTaxPayment prompt and the GameAction::PayCombatTax answer. Phase models the attack/block tax pause; the protocol's payment vocabulary (PaymentActionKind, five variants) is reachable only from the PayManaCost family, which upstream scopes to a spell's cost via its required cardId/cardName/manaCost fields. A combat tax has no spell to name.",
        suggested_protocol_extension: "Make PayManaCost's card fields optional so the payment family can carry a non-spell cost, or specify that combat taxes are presented as ChooseBoolean plus an ordinary payment round.",
    },
    UnsupportedCapability {
        code: "local.mana-combination-choice-unsupported",
        area: "mana",
        reason: "ManaChoicePrompt has three forms and two map exactly: SingleColor and AnyCombination both become ChooseColor (amount plus repeat_allowed covers them). The third, Combination, constrains WHICH multisets are legal rather than just how many picks are allowed, and ChooseColorInput carries only { valid_colors, amount, repeat_allowed } — there is nowhere to express the permitted combinations, so a client would be free to answer with an illegal one.",
        suggested_protocol_extension: "Let ChooseColorInput carry an explicit list of legal combinations (or reuse ChooseFromSelection with one option per legal combination, which needs no upstream change).",
    },
    UnsupportedCapability {
        code: "local.invalid-color-decision",
        area: "mana",
        reason: "Inbound validation, not a gap. A colorDecision answer is parsed against the six mana symbols W/U/B/R/G/C; anything else is rejected here rather than being mapped to a guess. The wire has no closed color enum, so this is the boundary check that a closed engine type requires.",
        suggested_protocol_extension: "Give the color fields a closed enum on the wire so an invalid symbol fails at deserialization rather than in translation.",
    },
    UnsupportedCapability {
        code: "local.cancel-mana-payment-unavailable",
        area: "mana",
        reason: "Emitted when a client sends PayManaCostOutput::Cancel but the engine's current legal-action set contains no GameAction::CancelCast — i.e. the cast is past the point where CR 601.2 rollback is offered. The adapter refuses rather than synthesizing a cancel the engine would reject. The protocol models cancel unconditionally; whether it is legal is engine state.",
        suggested_protocol_extension: "Let PayManaCostInput advertise whether cancel is currently available (a `canCancel` sibling to the existing canConfirmFromPool), so a conforming client never offers an illegal cancel.",
    },
    UnsupportedCapability {
        code: "local.stack-target-ref-unsupported",
        area: "responses",
        reason: "TargetKindDto has three kinds (Player, Card, Spell) but the engine's TargetRef has exactly two variants, Object(ObjectId) and Player(PlayerId) — a spell on the stack is an Object there. Inbound Spell refs are refused rather than silently coerced to Object, because the two id spaces are different wire prefixes (`stack-` vs `card-`) and a mis-coerced ref would resolve against the wrong permanent. Outbound is unaffected: encode_stack_id already emits the `stack-` prefix upstream's parser expects.",
        suggested_protocol_extension: "None needed upstream — this is adapter work: accept Spell by parsing the `stack-` prefix into the same ObjectId space Card uses.",
    },
    UnsupportedCapability {
        code: "local.choose-untap-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseUntap { object_id, untap } is a per-permanent boolean, and WaitingFor::UntapChoice carries a candidate list the engine projects as candidates.len() x 2 separate one-at-a-time answers (interaction.rs). ChooseBoolean is the matching family, one prompt per candidate; what is unmapped is the sequencing, not the shape.",
        suggested_protocol_extension: "None needed upstream — ChooseBoolean fits. This is adapter work.",
    },
    UnsupportedCapability {
        code: "local.enlist-unsupported",
        area: "combat",
        reason: "CR 702.154: enlist taps an untapped non-attacking creature as an attacker is declared. Phase models it (GameAction::ChooseEnlist). It is a choice of one permanent from a candidate set, which is ChooseCards or ChooseBoardTargets depending on whether it is a CR 115 target (it is not — enlist chooses, it does not target). Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — ChooseCards fits a non-targeting permanent choice. This is adapter work.",
    },
    UnsupportedCapability {
        code: "local.clash-unsupported",
        area: "prompts",
        reason: "CR 701.30a: clashing reveals the top card and its owner may bottom it. Phase models the opponent-picking half (GameAction::ChooseClashOpponent). Choosing which opponent clashes is a player choice, which is ChooseBoardTargets with TargetKind::Player — the same shape local.zone-opponent-chooser-unsupported describes for CR 608.2d. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — ChooseBoardTargets carries player candidates. This is adapter work.",
    },
    UnsupportedCapability {
        code: "local.announcing-opponent-unsupported",
        area: "prompts",
        reason: "CR 601.2c + CR 115.1: GameAction::ChooseAnnouncingOpponent { opponent } is the caster's answer to which opponent announces an 'of an opponent's choice' target slot. Structurally identical to local.clash-unsupported and local.zone-opponent-chooser-unsupported: a choice over player candidates, which ChooseBoardTargets already carries via TargetKind::Player. Three codes, one shape — they are listed separately only because three separate emit sites exist.",
        suggested_protocol_extension: "None needed upstream — this is adapter work, and the three opponent-picker codes should close together.",
    },
    UnsupportedCapability {
        code: "local.gift-recipient-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseGiftRecipient { opponent } picks which opponent receives the gift. Same player-choice shape as the other opponent pickers above; ChooseBoardTargets with TargetKind::Player is the home.",
        suggested_protocol_extension: "None needed upstream — adapter work.",
    },
    UnsupportedCapability {
        code: "local.pile-opponent-unsupported",
        area: "prompts",
        reason: "GameAction::ChoosePileOpponent picks which opponent separates the piles (CR 608.2d division of labour). Player choice again — ChooseBoardTargets with TargetKind::Player. Note this is distinct from the pile decisions themselves: separating is SubmitPilePartition and picking a pile is ChoosePile, both covered by their own codes.",
        suggested_protocol_extension: "None needed upstream — adapter work.",
    },
    UnsupportedCapability {
        code: "local.assist-unsupported",
        area: "mana",
        reason: "CR 702.132a: assist lets another player pay part of a spell's generic cost. Phase models both halves and both prompts: WaitingFor::AssistChoosePlayer { candidates, max_generic } (the CASTER picks, answered by ChooseAssistPlayer) and WaitingFor::AssistPayment { chosen, max_generic } (the CHOSEN player decides how much, answered by CommitAssistPayment). Both fit existing families without any extension — the first is ChooseBoardTargets over player candidates, the second is ChooseNumber with min 0 and max max_generic. Nor is authorization the obstacle: AssistPayment's acting_player() returns `chosen`, so decidingPlayerId already routes the step to the right seat, and this is NOT the submitter-vs-subject gap recorded as upstream.controlled-turn-subject-missing. Purely unwritten mapping.",
        suggested_protocol_extension: "None needed upstream — ChooseBoardTargets then ChooseNumber. This is adapter work.",
    },
    UnsupportedCapability {
        code: "local.reorder-hand-unsupported",
        area: "prompts",
        reason: "GameAction::ReorderHand is a pure ordering, which is exactly the Reorder family — the same family trigger ordering (CR 603.3b) already uses. It is unmapped rather than unrepresentable, and it is low value: hand order is not game state any rule reads, so a client's local ordering is normally sufficient.",
        suggested_protocol_extension: "None needed upstream — Reorder fits. Adapter work, and arguably not worth doing.",
    },
    UnsupportedCapability {
        code: "local.counter-cost-distribution-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseRemoveCounterCostDistribution spreads a counter-removal COST across several permanents. The protocol's only distribution shapes are the two combat-damage families, which are damage-specific (attacker/blocker ids, total_damage). There is no generic 'assign N units across these objects' family, which is the same hole recorded for local.distribution-unsupported and local.counter-move-distribution-unsupported.",
        suggested_protocol_extension: "Add one generic amount-distribution family (objects + total + per-object min/max) and retire the special-casing; see local.distribution-unsupported.",
    },
    UnsupportedCapability {
        code: "local.counter-removal-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseCountersToRemove picks WHICH counters (by kind and quantity) come off. The wire's counter vocabulary is itself unsettled — see local.counter-key-vocabulary-unverifiable, where only P1P1 and M1M1 are confirmed aligned — so even a correct family choice could not name the counter kinds unambiguously today.",
        suggested_protocol_extension: "Settle CardDto.counters' key vocabulary first (see local.counter-key-vocabulary-unverifiable); the selection itself then fits ChooseFromSelection.",
    },
    UnsupportedCapability {
        code: "local.coin-flip-unsupported",
        area: "prompts",
        reason: "CR 705: Phase models coin flips and the re-flip/keep decision (GameAction::SelectCoinFlips, WaitingFor::CoinFlipKeepChoice). Choosing which flips to keep is a bounded subset selection over abstract items — ChooseFromSelection's shape — but its options carry only a label, so the flips would be distinguished by prose alone. That is the general prompt-discriminator problem noted under upstream.display-sequencing-missing rather than a coin-specific gap.",
        suggested_protocol_extension: "None needed upstream — ChooseFromSelection fits. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.outside-game-selection-unsupported",
        area: "prompts",
        reason: "Half of this is a real id gap and half is not, so it is stated per branch. CR 400.11 / CR 701.23j: Phase models the choice as WaitingFor::OutsideGameChoice { choices: Vec<OutsideGameChoiceEntry>, count, up_to, destination }, and OutsideGameChoiceSource is exactly two variants. FaceUpExile { object_id } already carries an ObjectId and is encodable as a `card-` id today — for that branch ChooseCards fits with nothing missing. Sideboard { sideboard_index, card: CardFace } carries no ObjectId, and every card-carrying family (ChooseCards, ChooseBoardTargets, Scry, Reorder) is keyed on the `card-` id space, so only the sideboard branch is blocked. Related: local.deck-dto-not-implemented.",
        suggested_protocol_extension: "None needed upstream — the FaceUpExile branch fits ChooseCards now, and closing the sideboard branch needs a stable id for sideboard entries, which is a Phase-side decision rather than a wire shape.",
    },
    UnsupportedCapability {
        code: "local.replacement-choice-unsupported",
        area: "prompts",
        reason: "CR 616.1: when two or more replacement effects would apply to the same event, the affected object's controller (or the affected player) chooses one to apply first. Phase models it (GameAction::ChooseReplacement). It is a pick-one from a labelled list — ChooseFromSelection — but each option is an effect, not a card or a target, so the label is the only handle a client gets. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — ChooseFromSelection fits. Adapter work, and worth ranking high: unlike most entries here it is not tied to one keyword, so any board with two interacting replacement effects can reach it.",
    },
    UnsupportedCapability {
        code: "local.selection-unsupported",
        area: "prompts",
        reason: "One code shared by seven engine actions that are all pick-one-or-more from a labelled set: ChooseOption, SubmitVoteCandidate (CR 701.38 voting), SubmitSpellbookDraft, ChoosePile (PileSide = A|B), ChooseBranch, SubmitLifeRedistribution, ChooseDamageSource. Every one of them is ChooseFromSelection's shape — labelled options with min/max totals. They are collapsed under one code because they share one cause (no mapping written), not because they share one obstacle.",
        suggested_protocol_extension: "None needed upstream — ChooseFromSelection is the generic escape hatch and covers all seven. This is the largest single adapter-work item in this registry.",
    },
    UnsupportedCapability {
        code: "local.pile-partition-unsupported",
        area: "prompts",
        reason: "GameAction::SubmitPilePartition { pile_a } is NOT a partition primitive despite the name: the engine derives pile B as (eligible \\ pile_a), so the decision is 'pick a subset', which is exactly ChooseCards with min 0 and max eligible.len(). Recorded as a gap only because the mapping is unwritten. The sibling decision — choosing which pile to take — is ChoosePile and rides local.selection-unsupported.",
        suggested_protocol_extension: "None needed upstream — ChooseCards fits exactly. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.optional-trigger-unsupported",
        area: "prompts",
        reason: "Narrow scope. CR 603.12's plain 'you may' IS mapped — GameAction::DecideOptionalEffect answers a ChooseBoolean. This code covers only two siblings that carry extra payload: DecideOptionalCost and DecideOptionalEffectAndRemember. Both are still yes/no questions, so ChooseBoolean is the right family; what is unwritten is the response-translation dispatch that would tell them apart from the plain form, which per this crate's rules must key on the current WaitingFor.",
        suggested_protocol_extension: "None needed upstream — adapter work in translate_response, not a new family.",
    },
    UnsupportedCapability {
        code: "local.cast-choice-unsupported",
        area: "prompts",
        reason: "Five cast-time sub-decisions share this code: ChooseAdventureFace, ChooseModalFace (CR 712.12), ChooseAlternativeCast, ChooseCastingVariant, ChoosePermanentTypeSlot. All are pick-one-of-a-few, so ChooseFromSelection fits every one on shape. They are grouped with local.mdfc-face-choice-unsupported, which records the same hole from the prompt side for the modal-face case specifically.",
        suggested_protocol_extension: "None needed upstream — ChooseFromSelection covers all five; a namespaced prompt `kind` discriminator (see the crate docs' one upstream ask) would let a programmatic client tell them apart without parsing prose.",
    },
    UnsupportedCapability {
        code: "local.retarget-unsupported",
        area: "prompts",
        reason: "CR 707.10c / CR 722.3c: a copied spell's controller may change its targets. Phase models both the keep-all shortcut (GameAction::KeepAllCopyTargets) and the per-slot change (GameAction::RetargetSpell). ChooseBoardTargets is the family for the change, and ChooseBoolean for the shortcut; the pair is unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — adapter work.",
    },
    UnsupportedCapability {
        code: "local.splice-unsupported",
        area: "prompts",
        reason: "CR 702.47: splice reveals a card in hand and adds its text to a spell being cast. Phase models the offer (GameAction::RespondToSpliceOffer). It is a yes/no per offered card, so ChooseBoolean fits, with the spliced card carried as the prompt's sourceCard. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — adapter work.",
    },
    UnsupportedCapability {
        code: "local.activation-cost-choice-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseActivationCostBranch picks among several costs an activated ability offers. Same shape and same cause as local.cost-prevention-unsupported's branching half: a pick-one over labelled costs, which is ChooseFromSelection. Two codes exist because the engine has two states; the gap is one.",
        suggested_protocol_extension: "None needed upstream — adapter work; close it together with local.cost-prevention-unsupported.",
    },
    UnsupportedCapability {
        code: "local.board-action-unsupported",
        area: "actions",
        reason: "REAL FUNCTIONAL LOSS, not a stale-id guard: these are priority-window plays, so available_actions() filtering them out means a ManaBrew client can never equip (CR 702.6), crew (CR 702.122), station (CR 702.184), saddle (CR 702.171), transform, or turn a face-down permanent face up. Phase models all six as dedicated GameActions rather than as indexed ability activations, and AvailableActionKind::ActivateAbility requires an ability_index the action does not carry — which is the same shape of blocker that kept ninjutsu unadvertised until GameState was threaded into convert_available_action. That threading now exists, so the index is sourceable the same way; the work is simply not done.",
        suggested_protocol_extension: "None needed upstream — CR 702.6a and CR 702.122a make equip and crew activated abilities, so ActivateAbility is already the rules-correct home. This is adapter work now unblocked by the threaded GameState.",
    },
    UnsupportedCapability {
        code: "local.play-draw-unsupported",
        area: "actions",
        reason: "CR 103.1: before the first turn a player chooses whether to play or draw. Phase models it (GameAction::ChoosePlayDraw). It is a two-option choice and ChooseBoolean fits, but the framing matters — a boolean's confirm/deny labels would have to read 'Play'/'Draw', which is exactly what confirm_label and deny_label are for. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — ChooseBoolean with explicit labels. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.planar-die-unsupported",
        area: "actions",
        reason: "CR 901.9: rolling the planar die is a special action the active player may take at priority with an empty stack during their main phase. It is therefore a priority-window play, and filtering it out means Planechase cannot be played through this adapter. The obstacle is that AvailableActionKind has no 'special action' kind — the three variants are Cast, ActivateAbility, and UndoMana, and the roll is neither a cast nor an ability activation (CR 901.9d explicitly distinguishes the roll from abilities that trigger on it).",
        suggested_protocol_extension: "Add a special-action kind to AvailableActionKind (or a generic labelled action). CR 116.1 defines special actions as priority-window actions that do not use the stack, and CR 116.2 lists twelve of them — so this is a class, not one card.",
    },
    UnsupportedCapability {
        code: "local.companion-unsupported",
        area: "actions",
        reason: "CR 702.139: Phase models both halves (GameAction::DeclareCompanion at the start of the game, GameAction::CompanionToHand for the {3} special action). CompanionToHand is a priority-window special action and hits the same hole as the planar die: no special-action kind exists in AvailableActionKind's three variants. DeclareCompanion is a pre-game declaration and has no prompt family either.",
        suggested_protocol_extension: "Covered by the special-action kind proposed on local.planar-die-unsupported; the pre-game declaration additionally needs a prompt point before the first turn.",
    },
    UnsupportedCapability {
        code: "local.end-continuous-effect-unsupported",
        area: "actions",
        reason: "Phase exposes GameAction::EndContinuousEffect with the exact effect group and cost, but AvailableActionKind has no special-action kind, so the adapter cannot advertise that choice without misclassifying it as a cast or activated ability.",
        suggested_protocol_extension: "Add a special-action available-action kind carrying the effect-group identity and displayed cost, or a generic labelled special action with equivalent typed payload.",
    },
    UnsupportedCapability {
        code: "local.cast-offer-unsupported",
        area: "actions",
        reason: "Five 'you may cast this now' offers share this code: DiscoverChoice (CR 701.57), CascadeChoice (CR 702.85), RippleChoice (CR 702.60), GraveyardPaidCastChoice, and FreeCastWindowChoice. Every one is a yes/no on casting a specific revealed card, so ChooseBoolean is the family and the card rides the prompt's sourceCard. They are grouped because they are one shape with one cause. Note the sibling that IS mapped: the miracle offer (CR 702.94a) takes exactly this treatment already, which is the proof the shape works.",
        suggested_protocol_extension: "None needed upstream — ChooseBoolean, exactly as the miracle offer already does. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.top-bottom-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseTopOrBottom { top: bool } sends a revealed card to the top or the bottom of a library. Two homes fit and neither needs an extension: ChooseBoolean, because the payload is literally a bool; or Scry with zones [libraryTop, libraryBottom] and one card, the identical treatment scry and surveil (CR 701.25a) already receive. Unmapped, and the cheapest item in this block to close.",
        suggested_protocol_extension: "None needed upstream — ChooseBoolean or a one-card Scry. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.mutate-unsupported",
        area: "prompts",
        reason: "CR 702.140: mutate merges a creature over or under a target creature, and the controller picks which. Phase models it (GameAction::ChooseMutateMergeSide). Pick-one-of-two, so ChooseBoolean with 'Over'/'Under' labels fits. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — adapter work.",
    },
    UnsupportedCapability {
        code: "local.cipher-unsupported",
        area: "prompts",
        reason: "CR 702.99: ciphering exiles the spell card encoded on a creature the caster controls. Phase models it (GameAction::CipherEncode). Choosing which creature is a non-targeting permanent choice — ChooseCards. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — adapter work.",
    },
    UnsupportedCapability {
        code: "local.autopass-settings-unsupported",
        area: "responses",
        reason: "Deliberate and permanent, not a coverage gap. SetAutoPass, CancelAutoPass, SetPhaseStops, SetPriorityPassingMode, SetPriorityYield, SetMayTriggerAutoChoice, and SetTriggerOrderTemplate are client PREFERENCES that happen to travel as GameActions in Phase; none of them changes game state or answers a rules decision. Advertising them as protocol actions would invite a client to treat UI configuration as a play. The related protocol-side intents (pass.until, pass.exhaustStack) have their own entries.",
        suggested_protocol_extension: "None wanted upstream — see local.pass-until-unsupported and local.exhaust-stack-pass-unsupported for the two intents that DO need a contract decision.",
    },
    UnsupportedCapability {
        code: "local.distribution-unsupported",
        area: "prompts",
        reason: "GameAction::DistributeAmong assigns N units (damage, counters, life) across chosen objects. The protocol's only distribution families are ChooseCombatDamageAssignment and ChooseDamageAssignmentOrder, both hard-wired to combat (attacker id, blocker ids, total_damage). A generic 'divide N as you choose' (CR 601.2d) has no family, and encoding it as repeated single-target prompts would change the decision's semantics, since CR 601.2d fixes the division at announcement.",
        suggested_protocol_extension: "Add one generic amount-distribution family — objects, total, and per-object min/max — and let the combat families become uses of it. This closes local.counter-cost-distribution-unsupported and local.counter-move-distribution-unsupported at the same time.",
    },
    UnsupportedCapability {
        code: "local.counter-move-distribution-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseCounterMoveDistribution moves counters between permanents in chosen quantities. Same missing primitive as local.distribution-unsupported: an amount-per-object assignment with no combat framing.",
        suggested_protocol_extension: "Covered by the generic amount-distribution family proposed on local.distribution-unsupported.",
    },
    UnsupportedCapability {
        code: "local.pay-amount-unsupported",
        area: "prompts",
        reason: "GameAction::SubmitPayAmount answers 'pay any amount of X'. The value itself is ChooseNumber's shape (min/max), and the reason it is unmapped is that the bounds are engine-computed per effect; the adapter must read them from the state rather than derive them, which the current mapping does not do for this state.",
        suggested_protocol_extension: "None needed upstream — ChooseNumber fits. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.learn-unsupported",
        area: "prompts",
        reason: "CR 701.48: learning is a choice between fetching a Lesson from outside the game and discarding-then-drawing (or doing nothing). Phase models it (GameAction::LearnDecision). The branch choice is ChooseFromSelection, but the sideboard branch runs into local.outside-game-selection-unsupported: Lesson cards outside the game are DeckEntry values with no ObjectId to encode.",
        suggested_protocol_extension: "Covered by local.outside-game-selection-unsupported — the branch choice itself needs no extension.",
    },
    UnsupportedCapability {
        code: "local.copy-cast-unsupported",
        area: "actions",
        reason: "Priority-window plays, so this is functional loss like local.board-action-unsupported — and, checked rather than assumed, there is no id obstacle. Both GameAction::CastPreparedCopy { source: ObjectId } and CastParadigmCopy { source: ObjectId } name an ordinary object (a prepared battlefield permanent; an exiled card), which encode_object_id already renders under the `card-` prefix, so AvailableActionKind::Cast's cardId is satisfiable today. What is genuinely absent is a mode: PlayCardMode's seven variants have nothing for 'cast a token copy of this object's other face / of this exiled card', so the play would have to be advertised as Normal. That is the same labelling-only situation as local.play-card-mode-fidelity-gaps, not an unrepresentable one — and unlike the fidelity gaps these are still filtered out entirely, which is the actual defect.",
        suggested_protocol_extension: "None needed upstream to make the plays reachable — Cast with mode Normal and a descriptive label is enough, exactly as CastSpellAsMiracle is handled. A cast-a-copy PlayCardMode would restore programmatic mode discrimination; note both keywords are pre-CR (the engine annotates them 'CR 702.xxx, assign when WotC publishes the SOS update'), so an upstream ask should wait for the rules text.",
    },
    UnsupportedCapability {
        code: "local.specialize-unsupported",
        area: "prompts",
        reason: "GameAction::ChooseSpecializeColor picks the color a specializing permanent takes. A pick-one over at most five labelled colors — ChooseColor's shape, with amount 1 and repeat_allowed false. Unmapped, not unrepresentable.",
        suggested_protocol_extension: "None needed upstream — ChooseColor fits. Adapter work.",
    },
    UnsupportedCapability {
        code: "local.paradigm-offer-unsupported",
        area: "actions",
        reason: "GameAction::PassParadigmOffer declines a paradigm-copy offer. It is the decline half of the pair whose accept half is local.copy-cast-unsupported, and like the cast offers above it is a yes/no that ChooseBoolean expresses. It is recorded separately only because it emits a separate code from a separate arm.",
        suggested_protocol_extension: "None needed upstream — close with local.copy-cast-unsupported.",
    },
    UnsupportedCapability {
        code: "local.debug-action-unsupported",
        area: "actions",
        reason: "Deliberate and permanent. GameAction::Debug, GrantDebugPermission, and RevokeDebugPermission are development affordances that mutate state outside the rules; advertising them to an external client would hand it a cheat channel. This code exists so that an echoed debug id is refused with a named reason rather than silently ignored.",
        suggested_protocol_extension: "None wanted upstream — this must stay unsupported.",
    },
    UnsupportedCapability {
        code: "local.loop-shortcut-unsupported",
        area: "responses",
        reason: "CR 732: the interactive loop-shortcut protocol (DeclareShortcut, RespondToShortcut, DeclineShortcut, PrecastCopyShortcut) is opt-in behind Phase's LoopDetectionMode::Interactive, which a ManaBrew client never sets — so these actions are not reachable through this adapter rather than being unmappable. Left unsupported deliberately: mapping a shortcut negotiation a client cannot opt into would advertise a play it can never legally make.",
        suggested_protocol_extension: "None needed upstream until a client can opt into interactive loop detection; CR 732.1 shortcuts are a table convention the protocol has no reason to model first.",
    },
];

pub enum AvailableActionConversion {
    Available(AvailableAction),
    Skip,
    Unsupported(&'static str),
}

pub fn build_state_update(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<StateUpdate> {
    Ok(StateUpdate {
        game_view: build_game_view(prepared, card_lookup)?,
    })
}

pub fn build_game_view(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<GameViewDto> {
    let state = &prepared.state;
    let cards = CardBuildContext { card_lookup };
    let (game_over, winner_id) = match &state.waiting_for {
        WaitingFor::GameOver { winner } => (true, winner.map(encode_player_id)),
        _ => (false, None),
    };

    Ok(GameViewDto {
        game_id: prepared.game_id.clone(),
        turn: state.turn_number,
        step: phase_step(state.phase),
        combat_assignments: combat_assignments(state),
        active_player_id: encode_player_id(state.active_player),
        priority_player_id: encode_player_id(state.priority_player),
        players: state
            .players
            .iter()
            .map(|player| build_player_dto(state, player.id, prepared.viewer, &prepared.derived))
            .collect::<Result<Vec<_>>>()?,
        zones: build_zones(state, &cards)?,
        stack: build_stack(state, &prepared.derived),
        game_over,
        winner_id,
        monarch_id: state.monarch.map(encode_player_id),
        initiative_holder_id: state.initiative.map(encode_player_id),
        // CR 731.1: "The game starts with neither designation", so `None` is
        // `Neither` rather than a missing value.
        day_time: match state.day_night {
            None => DayTime::Neither,
            Some(engine::types::game_state::DayNight::Day) => DayTime::Day,
            Some(engine::types::game_state::DayNight::Night) => DayTime::Night,
        },
    })
}

/// Build the prompt for `prepared`'s viewer.
///
/// The display events a caller has accumulated are relayed separately, as
/// `display` envelopes ([`RelayMessage::Display`]) — `AgentPrompt` carries
/// `deny_unknown_fields`, so no extra field could be attached to it anyway.
pub fn build_prompt(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<AgentPrompt> {
    if !turn_control::is_authorized_submitter(&prepared.state, prepared.viewer)
        && !matches!(prepared.state.waiting_for, WaitingFor::GameOver { .. })
    {
        return Err(AdapterError::NoAuthorizedPrompt {
            viewer: prepared.viewer,
        });
    }
    // Prompt id 0 is reserved for engine-synthesized absent-player defaults and
    // may never be accepted as a real answer, so emitting a prompt with it would
    // produce an unanswerable prompt.
    if prepared.prompt_id == RESERVED_ABSENT_PLAYER_PROMPT_ID {
        return Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.reserved-prompt-id-zero",
        });
    }

    let cards = CardBuildContext { card_lookup };
    Ok(AgentPrompt {
        prompt_id: prepared.prompt_id,
        deciding_player_id: encode_player_id(prepared.viewer),
        // Built from the RAW source object captured in `prepare_snapshot`, so an
        // out-of-view source still renders. `&prepared.state` supplies only
        // battlefield-combat facts, which are public whenever non-default.
        source_card: prepared
            .source_card_object
            .as_ref()
            .map(|object| build_card_dto(&prepared.state, object, &cards))
            .transpose()?,
        input: build_prompt_input(prepared, card_lookup)?,
    })
}

/// Prompt id reserved by the protocol for engine-synthesized absent-player
/// defaults (timeout / disconnect). It must never be accepted as a real answer.
pub const RESERVED_ABSENT_PLAYER_PROMPT_ID: u32 = 0;

/// A narration event broadcast to every seat. Purely informational — no
/// response is expected.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum DisplayEvent {
    CardPlayed {
        card_id: String,
        card_name: String,
        set_code: String,
        player_id: String,
    },
    TurnChanged {
        active_player_id: String,
        active_player_name: String,
        turn_number: u32,
    },
}

fn build_prompt_input(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<PromptInput> {
    let waiting_for = &prepared.state.waiting_for;
    match waiting_for {
        WaitingFor::Priority { .. } => Ok(PromptInput::ChooseAction(ChooseActionInput {
            actions: available_actions(&prepared.state, &prepared.actions),
        })),
        WaitingFor::MulliganDecision { pending, .. } => {
            let entry = pending_entry_for_viewer(&prepared.state, prepared.viewer, pending)?;
            match &entry.phase {
                MulliganDecisionPhase::Declare => {
                    let hand =
                        &prepared.state.players[player_index(&prepared.state, entry.player)?].hand;
                    Ok(PromptInput::Mulligan(MulliganInput {
                        hand_card_ids: hand.iter().copied().map(encode_object_id).collect(),
                        mulligan_count: u32::from(entry.mulligan_count),
                    }))
                }
                MulliganDecisionPhase::BottomCards { count, then } => {
                    let cards = CardBuildContext { card_lookup };
                    let hand =
                        &prepared.state.players[player_index(&prepared.state, entry.player)?].hand;
                    Ok(PromptInput::MulliganPutBack(MulliganPutBackInput {
                        hand_card_ids: hand.iter().copied().map(encode_object_id).collect(),
                        cards: objects_from_ids(&prepared.state, hand, &cards)?,
                        count: usize::from(*count),
                        excluded_card_id: match then {
                            PendingMulliganAction::Keep => None,
                            PendingMulliganAction::UseSerumPowder { object_id } => {
                                Some(encode_object_id(*object_id))
                            }
                        },
                    }))
                }
            }
        }
        WaitingFor::OpeningHandBottomCards { pending, .. } => {
            let entry = pending_bottom_entry_for_viewer(&prepared.state, prepared.viewer, pending)?;
            let cards = CardBuildContext { card_lookup };
            let hand = &prepared.state.players[player_index(&prepared.state, entry.player)?].hand;
            Ok(PromptInput::MulliganPutBack(MulliganPutBackInput {
                hand_card_ids: hand.iter().copied().map(encode_object_id).collect(),
                cards: objects_from_ids(&prepared.state, hand, &cards)?,
                count: usize::from(entry.count),
                excluded_card_id: None,
            }))
        }
        WaitingFor::DeclareAttackers {
            player: _,
            valid_attacker_ids,
            valid_attack_targets,
            valid_attack_targets_by_attacker,
            attacker_constraints,
        } => Ok(PromptInput::ChooseAttackers(ChooseAttackersInput {
            attackers: valid_attacker_ids
                .iter()
                .copied()
                .map(|attacker_id| {
                    // CR 508.1a–d: each attacker's own legal targets come from the
                    // engine per-attacker map; the aggregate list is used only for a
                    // legacy (`None`) payload. `Some(map)` with a missing key means
                    // "no legal targets", so absent-vs-empty is preserved.
                    let target_slice: &[engine::game::combat::AttackTarget] =
                        match valid_attack_targets_by_attacker {
                            Some(map) => map.get(&attacker_id).map(Vec::as_slice).unwrap_or(&[]),
                            None => valid_attack_targets.as_slice(),
                        };
                    AttackerOptionDto {
                        attacker_id: encode_object_id(attacker_id),
                        valid_target_ids: target_slice.iter().map(attack_target_ref_id).collect(),
                        // CR 508.1d: surface the must-attack requirement from the
                        // engine display constraints instead of hardcoding false.
                        must_attack: matches!(
                            attacker_constraints.get(&attacker_id),
                            Some(engine::game::combat::CombatRequirement::MustAttack { .. })
                        ),
                    }
                })
                .collect(),
            attack_targets: valid_attack_targets.iter().map(attack_target_dto).collect(),
        })),
        WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            block_requirements,
            ..
        } => Ok(PromptInput::ChooseBlockers(ChooseBlockersInput {
            attackers: valid_block_targets
                .iter()
                .map(|(attacker_id, blocker_ids)| BlockableAttackerDto {
                    attacker_id: encode_object_id(*attacker_id),
                    valid_blocker_ids: blocker_ids.iter().copied().map(encode_object_id).collect(),
                    min_blockers: block_requirements
                        .get(attacker_id)
                        .map(|r| r.count)
                        .unwrap_or(0),
                    max_blockers: None,
                    must_be_blocked: block_requirements.contains_key(attacker_id),
                })
                .collect(),
            available_blocker_ids: valid_blocker_ids
                .iter()
                .copied()
                .map(encode_object_id)
                .collect(),
            error: None,
        })),
        WaitingFor::TargetSelection {
            target_slots,
            selection,
            mode_labels,
            ..
        }
        | WaitingFor::TriggerTargetSelection {
            target_slots,
            selection,
            mode_labels,
            ..
        } => {
            let current = selection.selected_slots.len();
            let slot = target_slots
                .get(current)
                .ok_or(AdapterError::UnsupportedPrompt {
                    waiting_for_type: waiting_for_type(waiting_for),
                    code: "local.target-slot-missing",
                })?;
            Ok(PromptInput::ChooseBoardTargets(ChooseBoardTargetsInput {
                // v2 removed the flat `label`; the slot's mode label is the
                // presentation title.
                presentation: presentation(
                    mode_labels
                        .get(current)
                        .and_then(Clone::clone)
                        .unwrap_or_else(|| "Choose target".to_string()),
                ),
                candidates: target_refs(&slot.legal_targets),
                hostile: false,
                intent: TargetingIntent::Hostile,
                min_targets: if slot.optional { 0 } else { 1 },
                max_targets: 1,
                chosen_targets: 0,
            }))
        }
        WaitingFor::ManaPayment { .. } => {
            Ok(PromptInput::PayManaCost(pay_mana_cost_input(prepared)))
        }
        WaitingFor::ChooseXValue { min, max, .. } => {
            Ok(PromptInput::ChooseNumber(ChooseNumberInput {
                presentation: presentation("Choose X"),
                min: *min as i32,
                max: *max as i32,
            }))
        }
        WaitingFor::ModeChoice {
            modal,
            unavailable_modes,
            ..
        } => Ok(PromptInput::ChooseFromSelection(ChooseFromSelectionInput {
            presentation: presentation("Choose mode"),
            options: modal_options(modal)
                .into_iter()
                .enumerate()
                .map(|(index, label)| {
                    if unavailable_modes.contains(&index) {
                        format!("{label} (unavailable)")
                    } else {
                        label
                    }
                })
                .map(selection_option)
                .collect(),
            min_total: modal.min_choices,
            max_total: modal.max_choices,
        })),
        WaitingFor::AbilityModeChoice { modal, .. } => {
            Ok(PromptInput::ChooseFromSelection(ChooseFromSelectionInput {
                presentation: presentation("Choose mode"),
                options: modal_options(modal)
                    .into_iter()
                    .map(selection_option)
                    .collect(),
                min_total: modal.min_choices,
                max_total: modal.max_choices,
            }))
        }
        WaitingFor::ChooseManaColor { choice, .. } => {
            choose_mana_color_input(choice).map(PromptInput::ChooseColor)
        }
        WaitingFor::ModalFaceChoice { .. } => {
            unsupported_prompt(waiting_for, "local.mdfc-face-choice-unsupported")
        }
        WaitingFor::NamedChoice { .. } | WaitingFor::CostTypeChoice { .. } => {
            unsupported_prompt(waiting_for, "local.named-choice-unsupported")
        }
        WaitingFor::AssignCombatDamage {
            attacker_id,
            blockers,
            total_damage,
            defending_player,
            ..
        } => Ok(PromptInput::ChooseCombatDamageAssignment(
            ChooseCombatDamageAssignmentInput {
                attacker_id: encode_object_id(*attacker_id),
                blocker_ids: blockers
                    .iter()
                    .map(|slot| encode_object_id(slot.blocker_id))
                    .collect(),
                defender_id: Some(encode_player_id(*defending_player)),
                total_damage: *total_damage as i32,
                attacker_has_deathtouch: false,
            },
        )),
        WaitingFor::ScryChoice { cards, .. } => {
            let ctx = CardBuildContext { card_lookup };
            Ok(PromptInput::Scry(ScryInput {
                presentation: presentation("Scry"),
                cards: object_vec_from_slice(&prepared.state, cards, &ctx)?,
                zones: vec![ScryDestination::LibraryTop, ScryDestination::LibraryBottom],
            }))
        }
        WaitingFor::GameOver { .. } => Ok(PromptInput::GameOver(GameOverInput {})),
        // CR 701.25a: Surveil puts each looked-at card on top of the library or
        // into the graveyard — the same "partition these cards across ordered
        // destinations" shape as scry, differing only in the second destination.
        // `ScryInput::zones` is that parameter, so surveil needs no new prompt
        // family: the engine answers both with the identical `SelectCards`
        // projection (`interaction.rs`, one match arm for both), where the
        // second zone list is the non-default destination.
        WaitingFor::SurveilChoice { cards, .. } => {
            let ctx = CardBuildContext { card_lookup };
            Ok(PromptInput::Scry(ScryInput {
                presentation: presentation("Surveil"),
                cards: object_vec_from_slice(&prepared.state, cards, &ctx)?,
                zones: vec![ScryDestination::LibraryTop, ScryDestination::Graveyard],
            }))
        }
        WaitingFor::DigChoice { .. } => unsupported_prompt(waiting_for, "local.dig-unsupported"),
        // CR 701.9a: Discard N cards from hand — a bounded selection over a
        // known card set, which is exactly `ChooseCardsInput`. `up_to` (CR
        // 701.9b "discard up to N") lowers the floor to zero rather than
        // needing a distinct prompt family.
        WaitingFor::DiscardChoice {
            count,
            cards,
            up_to,
            ..
        } => {
            let ctx = CardBuildContext { card_lookup };
            Ok(PromptInput::ChooseCards(ChooseCardsInput {
                presentation: presentation("Discard"),
                cards: object_vec_from_slice(&prepared.state, cards, &ctx)?,
                min: if *up_to { 0 } else { *count },
                max: *count,
            }))
        }
        WaitingFor::KeepWithinTotalPowerChoice { .. } => {
            unsupported_prompt(waiting_for, "local.keep-with-total-power-unsupported")
        }
        WaitingFor::KeepExactPermanentsChoice { .. } => {
            unsupported_prompt(waiting_for, "local.keep-exact-permanents-unsupported")
        }
        // CR 603.12: A "you may" trigger asks its controller a single yes/no
        // question, which is exactly `ChooseBoolean`. `OpponentMayChoice` is the
        // same question addressed to a non-controller (CR 608.2), so it shares
        // the shape; `decidingPlayerId` on the envelope already distinguishes
        // who is being asked.
        WaitingFor::OptionalEffectChoice { description, .. }
        | WaitingFor::OpponentMayChoice { description, .. } => {
            Ok(PromptInput::ChooseBoolean(ChooseBooleanInput {
                presentation: presentation(
                    description
                        .clone()
                        .unwrap_or_else(|| "Use ability?".to_string()),
                ),
                confirm_label: "Yes".to_string(),
                deny_label: "No".to_string(),
            }))
        }
        // CR 702.94a + CR 603.11: The miracle offer is a yes/no on casting the
        // revealed card for its miracle cost. The cast itself is already
        // advertised as an `AvailableAction`; without this prompt the offer was
        // unreachable and the advertised cast could never be taken.
        WaitingFor::MiracleReveal { cost, .. } => {
            Ok(PromptInput::ChooseBoolean(ChooseBooleanInput {
                presentation: presentation(format!(
                    "Cast for its miracle cost {}?",
                    mana_cost_string(cost)
                )),
                confirm_label: "Cast".to_string(),
                deny_label: "Decline".to_string(),
            }))
        }
        // CR 701.43d: Exerting is an optional cost declared as the creature
        // attacks — a yes/no per attacker.
        WaitingFor::ExertChoice { .. } => Ok(PromptInput::ChooseBoolean(ChooseBooleanInput {
            presentation: presentation("Exert this creature as it attacks?"),
            confirm_label: "Exert".to_string(),
            deny_label: "Decline".to_string(),
        })),
        // CR 118.12 ("unless" costs): pay the stated cost or let the effect
        // happen — a yes/no. `UnlessPaymentChooseCost` is deliberately NOT
        // folded in: it picks *among several costs*, which is a selection, not
        // a boolean, and mapping it here would misreport the question.
        WaitingFor::UnlessPayment {
            effect_description, ..
        } => Ok(PromptInput::ChooseBoolean(ChooseBooleanInput {
            presentation: presentation(
                effect_description
                    .clone()
                    .unwrap_or_else(|| "Pay the cost?".to_string()),
            ),
            confirm_label: "Pay".to_string(),
            deny_label: "Decline".to_string(),
        })),
        WaitingFor::UnlessPaymentChooseCost { .. } => {
            unsupported_prompt(waiting_for, "local.cost-prevention-unsupported")
        }
        // CR 603.3b: The controller orders simultaneous triggers on the stack.
        // `ReorderInput` is exactly an ordered list of items; each trigger is
        // rendered by its source card.
        WaitingFor::OrderTriggers { triggers, .. } => {
            let ctx = CardBuildContext { card_lookup };
            let source_ids: Vec<ObjectId> = triggers.iter().map(|t| t.source_id).collect();
            let cards = object_vec_from_slice(&prepared.state, &source_ids, &ctx)?;
            Ok(PromptInput::Reorder(ReorderInput {
                presentation: presentation("Order triggers"),
                // `GameAction::OrderTriggers { order: Vec<usize> }` indexes into
                // `triggers`, so the item id must be that index — NOT the source
                // object id, which collides when one permanent contributes two
                // simultaneous triggers (CR 603.3b).
                items: triggers
                    .iter()
                    .zip(cards)
                    .enumerate()
                    .map(|(index, (trigger, card))| ReorderItem {
                        id: index.to_string(),
                        card,
                        oracle: Some(trigger.description.clone()),
                    })
                    .collect(),
            }))
        }
        WaitingFor::AssignBlockerDamage { .. } => {
            unsupported_prompt(waiting_for, "local.blocker-damage-banding-unsupported")
        }
        WaitingFor::CombatTaxPayment { .. } => {
            unsupported_prompt(waiting_for, "local.pay-combat-cost-unsupported")
        }
        _ => interaction_prompt(prepared, card_lookup),
    }
}

fn unsupported_prompt<T>(waiting_for: &WaitingFor, code: &'static str) -> Result<T> {
    Err(AdapterError::UnsupportedPrompt {
        waiting_for_type: waiting_for_type(waiting_for),
        code,
    })
}

/// Build a prompt from the engine's own interaction projection.
///
/// The fallback for every waiting state with no bespoke arm above, and
/// deliberately generic. The engine classifies all of its waiting states into a
/// small set of response models, and for the finite ones it hands back concrete
/// labelled choices it has already validated. Hand-writing one mapping per
/// waiting state instead would re-derive bounds the engine has computed — game
/// logic duplicated inside a serialization boundary, and the exact drift the
/// interaction subsystem exists to prevent.
///
/// Scope is `ExactChoices` only. A finite, pre-materialized candidate list is
/// precisely `ChooseFromSelection`'s shape, so the mapping is total and needs no
/// per-variant judgement. The schema-valued specs (sequences, numbers, amount
/// assignments, relations) carry an unbounded response space that no single
/// prompt family expresses; they still fail closed under a declared code.
///
/// One projection shape leaves the labelled-option family: an unordered subset
/// over a list of objects is a card selection, and [`card_selection_objects`]
/// routes it to `ChooseCards` so the client renders the cards themselves.
fn interaction_prompt(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<PromptInput> {
    let waiting_for = &prepared.state.waiting_for;
    // One opportunity per interaction slot this viewer may submit for, and a
    // viewer can be the authorized submitter for more than one semantic owner —
    // a simultaneous decision both seats owe. The wire carries a single prompt,
    // so serving `.first()` would answer one seat and silently drop the other.
    let [opportunity] = prepared.interaction.opportunities.as_slice() else {
        return unsupported_prompt(
            waiting_for,
            if prepared.interaction.opportunities.is_empty() {
                "local.prompt-unsupported"
            } else {
                "local.interaction-simultaneous-decisions-unmapped"
            },
        );
    };
    // A numeric range is not a choice over candidates at all, so it leaves the
    // selection family entirely: `ChooseNumber` carries exactly this and nothing
    // else. Handled before the candidate branches because its candidate list is
    // empty by construction, which the emptiness guard below would reject.
    if let InteractionOpportunityResponse::Schema {
        spec: InteractionResponseSpec::Number { min, max, .. },
        ..
    } = &opportunity.response
    {
        return Ok(PromptInput::ChooseNumber(ChooseNumberInput {
            presentation: presentation("Choose a number"),
            // The engine's bounds are unsigned and the wire's are signed. Every
            // engine bound is representable, and the widening keeps the protocol
            // free to express a negative range this engine never produces.
            min: *min as i32,
            max: *max as i32,
        }));
    }
    let (choices, min_total, max_total) = match &opportunity.response {
        // A one-of list: the engine materialized each entry as a complete answer
        // to the whole decision, so exactly one is chosen.
        InteractionOpportunityResponse::ExactChoices { choices } => (choices, 1, 1),
        // A subset choice over the same kind of candidate list, differing only
        // in how many may be taken — which the constraint carries, so
        // `ChooseFromSelection`'s min/max totals express it exactly.
        InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Select { constraint, .. },
            candidates,
        } => match constraint {
            // `EngineValidatedCount` bounds the count identically. The extra
            // legality the engine reserves to itself is rechecked on submit and
            // is not expressible to a client either way, so advertising the
            // count is the whole of what this family can honestly say.
            SelectionConstraint::Count { min, max }
            | SelectionConstraint::EngineValidatedCount { min, max } => {
                (candidates, *min as usize, *max as usize)
            }
            // An aggregate bound — "keep permanents with total power 4 or less"
            // — constrains a sum over a chosen attribute, not a count. No family
            // carries it, and flattening it to an unbounded count would
            // advertise illegal answers as legal.
            SelectionConstraint::Aggregate { .. } => {
                return unsupported_prompt(
                    waiting_for,
                    "local.interaction-aggregate-bound-unmapped",
                )
            }
        },
        // An ordered subset of the same candidate list. `chosen_indices` is a
        // sequence, so the order the client sends survives to the engine, which
        // fills its target slots in exactly that order.
        //
        // Fidelity gap, recorded rather than hidden: this family cannot *tell*
        // the client that order is significant — it renders as a selection. The
        // ordering family, `Reorder`, is not a substitute, because it orders the
        // whole list and a target sequence is usually a proper subset.
        InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Sequence { min, max, .. },
            candidates,
        } => (candidates, *min as usize, *max as usize),
        InteractionOpportunityResponse::Schema { .. } => {
            return unsupported_prompt(waiting_for, "local.interaction-schema-response-unmapped")
        }
    };
    if choices.is_empty() {
        return unsupported_prompt(waiting_for, "local.prompt-unsupported");
    }
    // A subset over a list of objects is a card selection, which `ChooseCards`
    // renders as the cards themselves rather than as opaque labels. The bounds
    // are the same ones the labelled family would have carried, so nothing the
    // engine computed is re-derived to get here.
    if let Some(object_ids) = card_selection_objects(&opportunity.response) {
        let ctx = CardBuildContext { card_lookup };
        return Ok(PromptInput::ChooseCards(ChooseCardsInput {
            presentation: presentation("Choose cards"),
            cards: object_vec_from_slice(&prepared.state, &object_ids, &ctx)?,
            min: min_total,
            max: max_total,
        }));
    }
    Ok(PromptInput::ChooseFromSelection(ChooseFromSelectionInput {
        presentation: presentation("Choose"),
        options: choices
            .iter()
            .map(|choice| selection_option(choice_label(choice)))
            .collect(),
        min_total,
        max_total,
    }))
}

/// Answer a generically-projected prompt by handing the pick back to the engine.
///
/// The index is positional into the same `ExactChoices` list `interaction_prompt`
/// rendered. That list is re-derived here rather than carried through
/// `PromptContext` because the projection is a pure function of state, and
/// staleness is already the prompt id's obligation — [`translate_response`]
/// rejects a mismatched id before reaching this point.
///
/// The engine, not a local index→action table, turns the pick into a
/// `GameAction`. Response→action is game logic, and the engine's matcher is
/// exhaustive over its response models; a table built here would keep compiling
/// while silently going stale as models are added.
fn interaction_selection_action(
    state: &GameState,
    actor: PlayerId,
    chosen_indices: &[usize],
) -> Result<GameAction> {
    let illegal = |kind: &'static str| AdapterError::IllegalResponseForPrompt {
        response_kind: kind,
    };
    let opportunity = sole_open_opportunity(state, actor)?;
    // The response variant is not interchangeable with the spec: a `Select`
    // schema submitted as `Choose` is rejected as malformed and vice versa, so
    // this must mirror whichever shape `interaction_prompt` rendered.
    let id_at = |index: &usize, list: &[InteractionChoice]| {
        list.get(*index)
            .map(|choice| choice.id.clone())
            .ok_or_else(|| illegal("selectionDecision index outside the offered choices"))
    };
    let response = match &opportunity.response {
        InteractionOpportunityResponse::ExactChoices { choices } => {
            let [index] = chosen_indices else {
                return Err(illegal(
                    "selectionDecision over a one-of list expects exactly one pick",
                ));
            };
            InteractionResponse::Choose {
                choice_id: id_at(index, choices)?,
            }
        }
        InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Select { .. },
            candidates,
        } => InteractionResponse::Select {
            // Count bounds are not rechecked here. The engine owns them and
            // rejects a violating submission; duplicating the check would put a
            // second, drifting authority on the same constraint.
            choice_ids: chosen_indices
                .iter()
                .map(|index| id_at(index, candidates))
                .collect::<Result<Vec<_>>>()?,
        },
        // Distinct from `Select` on the wire even though the prompt looks the
        // same: the engine fills its slots in the order given, so the indices
        // must stay in the order the client sent them.
        InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Sequence { .. },
            candidates,
        } => InteractionResponse::Sequence {
            choice_ids: chosen_indices
                .iter()
                .map(|index| id_at(index, candidates))
                .collect::<Result<Vec<_>>>()?,
        },
        InteractionOpportunityResponse::Schema { .. } => {
            return Err(illegal(
                "selectionDecision against a schema this family cannot express",
            ))
        }
    };
    resolve_interaction_response(
        state,
        actor,
        &InteractionSubmission {
            interaction_id: opportunity.interaction_id.clone(),
            response,
        },
    )
    .map_err(|_| illegal("selectionDecision the engine refused to materialize"))
}

/// The one interaction this viewer may answer right now, re-derived from
/// authoritative state.
///
/// Shared by every generic response path. Each prompt is built for a lone
/// opportunity — see [`interaction_prompt`] — so anything else means the
/// projection moved and the client's answer no longer denotes what it was shown.
fn sole_open_opportunity(state: &GameState, actor: PlayerId) -> Result<InteractionOpportunity> {
    let filtered = filter_state_for_viewer(state, actor);
    let mut view = derive_viewer_interaction(state, &filtered, actor);
    if view.opportunities.len() != 1 {
        return Err(AdapterError::IllegalResponseForPrompt {
            response_kind: "a response without exactly one open interaction",
        });
    }
    Ok(view.opportunities.remove(0))
}

/// Answer a generically-projected numeric prompt.
///
/// Split from the selection path because the two share no payload: this one
/// carries a value, not indices into a candidate list. What they do share —
/// finding the lone open opportunity, and letting the engine name the answering
/// action — lives in [`sole_open_opportunity`] and
/// [`resolve_interaction_response`].
fn interaction_number_action(state: &GameState, actor: PlayerId, value: u32) -> Result<GameAction> {
    let illegal = |kind: &'static str| AdapterError::IllegalResponseForPrompt {
        response_kind: kind,
    };
    let opportunity = sole_open_opportunity(state, actor)?;
    if !matches!(
        opportunity.response,
        InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Number { .. },
            ..
        }
    ) {
        return Err(illegal(
            "numberDecision against an interaction that is not a numeric range",
        ));
    }
    // The engine range-checks the value; re-checking it here would be a second
    // authority on the same bound, free to drift from the one that decides.
    resolve_interaction_response(
        state,
        actor,
        &InteractionSubmission {
            interaction_id: opportunity.interaction_id,
            response: InteractionResponse::Number { value },
        },
    )
    .map_err(|_| illegal("numberDecision the engine refused to materialize"))
}

/// The objects a projected response is a selection *of*, when it is one.
///
/// `Some` identifies the decision as a non-targeting card selection — the
/// `ChooseCards` shape — and carries the object behind each candidate, in the
/// order the engine offered them.
///
/// Both halves of the classification are read from the projection, never from a
/// `WaitingFor` list:
///
/// - **Not targeting.** CR 601.2c announces targets one slot at a time, and the
///   engine projects that ordered fill as the `Sequence` schema. `Select` is the
///   unordered subset schema, which cannot express a slot order, so a target
///   choice never arrives in this shape.
/// - **Cards.** Every candidate must be exactly one object and nothing else, per
///   [`candidate_object`]. A candidate carrying a player, an extra discriminator,
///   or a concealed object (whose object surface the engine withholds) fails the
///   whole list back to the labelled-option family rather than rendering a
///   partial or misleading card list.
fn card_selection_objects(response: &InteractionOpportunityResponse) -> Option<Vec<ObjectId>> {
    card_selection_candidates(response)?
        .iter()
        .map(candidate_object)
        .collect()
}

/// The candidate list of a card selection, unresolved.
///
/// The half of [`card_selection_objects`] that identifies the *schema*. The
/// response path needs the choices themselves (it answers by choice id, not by
/// object), so the two halves are separated rather than duplicated.
fn card_selection_candidates(
    response: &InteractionOpportunityResponse,
) -> Option<&[InteractionChoice]> {
    match response {
        InteractionOpportunityResponse::Schema {
            spec: InteractionResponseSpec::Select { .. },
            candidates,
        } => Some(candidates),
        _ => None,
    }
}

/// The one object a projected choice denotes, when the choice *is* that object.
///
/// `Summary` surfaces are skipped because they carry a classification code, not
/// an identity. Everything else must amount to a single `Object` in the
/// `Candidate` role: any second identity surface means the choice denotes an
/// object *plus* something the card list cannot show, and the caller must not
/// treat it as a card.
fn candidate_object(choice: &InteractionChoice) -> Option<ObjectId> {
    let mut identities = choice
        .surfaces
        .iter()
        .filter(|surface| !matches!(surface, InteractionPresentationSurface::Summary { .. }));
    let InteractionPresentationSurface::Object {
        role: InteractionRoleCode::Candidate,
        reference,
        ..
    } = identities.next()?
    else {
        return None;
    };
    if identities.next().is_some() {
        return None;
    }
    // The engine writes the raw `ObjectId` here; the wire's `card-` prefix is
    // this crate's encoding and is applied on the way out.
    reference.parse().ok().map(ObjectId)
}

/// Answer a generically-projected card prompt.
///
/// Split from [`interaction_selection_action`] because `ChooseCards` answers by
/// card id, not by position: the ids are resolved back through the very
/// [`candidate_object`] surface the prompt rendered them from, so a card the
/// prompt did not offer cannot be smuggled in by index arithmetic.
///
/// The submitted response is `Select`, matching the schema
/// [`card_selection_objects`] required — the engine rejects a `Choose` or
/// `Sequence` against a `Select` schema as malformed.
fn interaction_cards_action(
    state: &GameState,
    actor: PlayerId,
    chosen_card_ids: &[String],
) -> Result<GameAction> {
    let illegal = |kind: &'static str| AdapterError::IllegalResponseForPrompt {
        response_kind: kind,
    };
    let opportunity = sole_open_opportunity(state, actor)?;
    let Some(candidates) = card_selection_candidates(&opportunity.response) else {
        return Err(illegal(
            "chooseCardsDecision against an interaction that is not a card selection",
        ));
    };
    // Bounds are not rechecked. The engine owns them and rejects a violating
    // submission; a second check here would be a drifting authority.
    let choice_ids = chosen_card_ids
        .iter()
        .map(|card_id| {
            let object_id = parse_object_id(card_id)?;
            candidates
                .iter()
                .find(|candidate| candidate_object(candidate) == Some(object_id))
                .map(|candidate| candidate.id.clone())
                .ok_or_else(|| illegal("chooseCardsDecision naming an unoffered card"))
        })
        .collect::<Result<Vec<_>>>()?;
    resolve_interaction_response(
        state,
        actor,
        &InteractionSubmission {
            interaction_id: opportunity.interaction_id,
            response: InteractionResponse::Select { choice_ids },
        },
    )
    .map_err(|_| illegal("chooseCardsDecision the engine refused to materialize"))
}

/// Label one projected choice, from the strings the engine already put on it.
///
/// Every naming surface is joined rather than taking the first, because choices
/// in one list can share an object and differ only in a `Value` surface — the
/// priority projection offers auto-payment and manual-payment casts of the same
/// spell that way. Taking only the object name would render those two as the
/// same label, and the client picks by label even though it answers by index.
fn choice_label(choice: &InteractionChoice) -> String {
    let parts = choice
        .surfaces
        .iter()
        .filter_map(|surface| match surface {
            InteractionPresentationSurface::Object {
                name, reference, ..
            } => Some(name.clone().unwrap_or_else(|| reference.clone())),
            InteractionPresentationSurface::Value { value, .. } => Some(value.clone()),
            InteractionPresentationSurface::Player { seat, .. } => Some(format!("Player {seat}")),
            _ => None,
        })
        .collect::<Vec<_>>();
    if parts.is_empty() {
        // No naming surface at all. The opaque id is a poor label but a correct
        // one; it is never empty, so the option stays distinguishable.
        choice.id.as_str().to_string()
    } else {
        parts.join(" — ")
    }
}

impl PromptInput {
    /// The formal prompt/response contract: a response is valid only if its
    /// output family matches this prompt **and** every echoed action id was
    /// advertised by it.
    ///
    /// These are two of the five conformance obligations; the other three
    /// (stale prompt id, wrong player, unparseable shape) are enforced by
    /// [`translate_client_message`] and by deserialization respectively.
    pub fn validate_response(
        &self,
        output: &PromptOutput,
    ) -> std::result::Result<(), ResponseViolation> {
        match (self, output) {
            (PromptInput::ChooseAction(input), PromptOutput::ChooseAction(out)) => match out {
                ChooseActionOutput::Act { action_id }
                    if !input.actions.iter().any(|a| a.id == *action_id) =>
                {
                    Err(ResponseViolation::UnknownActionId(action_id.clone()))
                }
                _ => Ok(()),
            },
            (PromptInput::PayManaCost(input), PromptOutput::PayManaCost(out)) => match out {
                PayManaCostOutput::Act { action_id }
                    if !input.actions.iter().any(|a| a.id == *action_id) =>
                {
                    Err(ResponseViolation::UnknownActionId(action_id.clone()))
                }
                _ => Ok(()),
            },
            (PromptInput::Mulligan(_), PromptOutput::Mulligan(_))
            | (PromptInput::MulliganPutBack(_), PromptOutput::MulliganPutBack(_))
            | (PromptInput::ChooseAttackers(_), PromptOutput::ChooseAttackers(_))
            | (PromptInput::ChooseBlockers(_), PromptOutput::ChooseBlockers(_))
            | (PromptInput::ChooseBoardTargets(_), PromptOutput::ChooseBoardTargets(_))
            | (PromptInput::ChooseBoolean(_), PromptOutput::ChooseBoolean(_))
            | (PromptInput::ChooseFromSelection(_), PromptOutput::ChooseFromSelection(_))
            | (PromptInput::RevealCards(_), PromptOutput::RevealCards(_))
            | (PromptInput::Scry(_), PromptOutput::Scry(_))
            | (PromptInput::ChooseColor(_), PromptOutput::ChooseColor(_))
            | (PromptInput::ChooseNumber(_), PromptOutput::ChooseNumber(_))
            | (
                PromptInput::ChooseDamageAssignmentOrder(_),
                PromptOutput::ChooseDamageAssignmentOrder(_),
            )
            | (
                PromptInput::ChooseCombatDamageAssignment(_),
                PromptOutput::ChooseCombatDamageAssignment(_),
            )
            | (PromptInput::ChooseCards(_), PromptOutput::ChooseCards(_))
            | (PromptInput::Reorder(_), PromptOutput::Reorder(_))
            | (PromptInput::DiceRolled(_), PromptOutput::DiceRolled(_)) => Ok(()),
            // Includes every `GameOver` pairing: that prompt is terminal and
            // `PromptOutput` has no matching arm.
            _ => Err(ResponseViolation::WrongPromptType),
        }
    }
}

/// Map an internal [`AdapterError`] onto the wire [`ProtocolError`] a client
/// receives. The two stay distinct types: one is this crate's failure mode, the
/// other is a protocol message.
///
/// A rejected response is never applied; the caller re-sends the open prompt so
/// the player can answer again.
pub fn protocol_error_for(error: &AdapterError, prompt_id: Option<u32>) -> ProtocolError {
    let (code, message) = match error {
        AdapterError::PromptIdMismatch { expected, actual } => (
            ProtocolErrorCode::StalePrompt,
            format!("expected prompt {expected}, got {actual}"),
        ),
        AdapterError::NoAuthorizedPrompt { viewer } => (
            ProtocolErrorCode::WrongPlayer,
            format!("player {} is not the deciding player", viewer.0),
        ),
        AdapterError::IllegalResponseForPrompt { response_kind } => (
            ProtocolErrorCode::WrongPromptType,
            format!("`{response_kind}` does not answer the open prompt"),
        ),
        AdapterError::StaleOrInvalidActionId { action_id } => (
            ProtocolErrorCode::UnknownActionId,
            format!("action id `{action_id}` was not advertised"),
        ),
        AdapterError::MalformedId {
            expected_prefix,
            value,
        } => (
            ProtocolErrorCode::InvalidShape,
            format!("id `{value}` is not a valid `{expected_prefix}` reference"),
        ),
        // Everything else is a capability or state gap on our side rather than a
        // malformed client message; `InvalidShape` is the protocol's catch-all.
        AdapterError::UnsupportedPlayerCount { count } => (
            ProtocolErrorCode::InvalidShape,
            format!("unsupported player count {count}"),
        ),
        AdapterError::UnsupportedPrompt { code, .. }
        | AdapterError::UnsupportedProtocolFeature { code } => (
            ProtocolErrorCode::InvalidShape,
            format!("unsupported protocol capability `{code}`"),
        ),
        AdapterError::MissingCardText { object_id } => (
            ProtocolErrorCode::InvalidShape,
            format!("no card text for object {}", object_id.0),
        ),
        AdapterError::ObjectNotFound { object_id } => (
            ProtocolErrorCode::InvalidShape,
            format!("object {} not found", object_id.0),
        ),
    };
    ProtocolError {
        code,
        message,
        prompt_id,
    }
}

/// Map a [`ResponseViolation`] onto its wire error.
pub fn protocol_error_for_violation(
    violation: &ResponseViolation,
    prompt_id: Option<u32>,
) -> ProtocolError {
    let (code, message) = match violation {
        ResponseViolation::WrongPromptType => (
            ProtocolErrorCode::WrongPromptType,
            "response family does not match the open prompt".to_string(),
        ),
        ResponseViolation::UnknownActionId(action_id) => (
            ProtocolErrorCode::UnknownActionId,
            format!("action id `{action_id}` was not advertised"),
        ),
    };
    ProtocolError {
        code,
        message,
        prompt_id,
    }
}

/// Translate anything a client sent into the engine action it means.
///
/// This is the single client→engine entry point.
pub fn translate_client_message(
    message: ClientToServerMessage,
    context: &PromptContext,
    state: &GameState,
) -> Result<GameAction> {
    match message {
        ClientToServerMessage::Directive { directive } => match directive {
            // A concede belongs to no prompt, so it needs no prompt-id or
            // family check — only that the sender owns the seat.
            DirectiveInput::Concede => Ok(GameAction::Concede {
                player_id: context.deciding_player,
            }),
        },
        ClientToServerMessage::Response { prompt_id, action } => {
            translate_response(prompt_id, action, context, state)
        }
    }
}

/// Translate one prompt answer, enforcing the stale-prompt and wrong-player
/// obligations before dispatching on the output's family tag.
pub fn translate_response(
    prompt_id: u32,
    output: PromptOutput,
    context: &PromptContext,
    state: &GameState,
) -> Result<GameAction> {
    // Prompt id 0 is reserved for engine-synthesized absent-player defaults and
    // must never be accepted as a real answer.
    if prompt_id == RESERVED_ABSENT_PLAYER_PROMPT_ID || prompt_id != context.prompt_id {
        return Err(AdapterError::PromptIdMismatch {
            expected: context.prompt_id,
            actual: prompt_id,
        });
    }
    if !turn_control::is_authorized_submitter(state, context.deciding_player)
        && !matches!(state.waiting_for, WaitingFor::GameOver { .. })
    {
        return Err(AdapterError::NoAuthorizedPrompt {
            viewer: context.deciding_player,
        });
    }
    if !output_family_matches_waiting(&output, state, context.deciding_player) {
        return Err(AdapterError::IllegalResponseForPrompt {
            response_kind: output_family(&output),
        });
    }

    match output {
        PromptOutput::ChooseAction(out) => translate_choose_action_output(out, context, state),
        PromptOutput::PayManaCost(out) => translate_pay_mana_output(out, context),
        PromptOutput::Mulligan(MulliganOutput::MulliganDecision { keep }) => {
            Ok(GameAction::MulliganDecision {
                choice: if keep {
                    engine::types::actions::MulliganChoice::Keep
                } else {
                    engine::types::actions::MulliganChoice::Mulligan
                },
            })
        }
        PromptOutput::Mulligan(MulliganOutput::MulliganUseSerumPowder { card_id }) => {
            Ok(GameAction::MulliganDecision {
                choice: engine::types::actions::MulliganChoice::UseSerumPowder {
                    object_id: parse_object_id(&card_id)?,
                },
            })
        }
        PromptOutput::MulliganPutBack(MulliganPutBackOutput::MulliganPutBackDecision {
            card_ids,
        }) => Ok(GameAction::SelectCards {
            cards: parse_object_ids(&card_ids)?,
        }),
        PromptOutput::ChooseAttackers(ChooseAttackersOutput::DeclareAttackers { assignments }) => {
            Ok(GameAction::DeclareAttackers {
                attacks: assignments
                    .iter()
                    .map(|assignment| {
                        Ok((
                            parse_object_id(&assignment.attacker_id)?,
                            parse_attack_target_id(&assignment.target_id)?,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?,
                bands: Vec::new(),
            })
        }
        PromptOutput::ChooseBlockers(ChooseBlockersOutput::DeclareBlockers { assignments }) => {
            Ok(GameAction::DeclareBlockers {
                assignments: assignments
                    .iter()
                    .map(|assignment| {
                        Ok((
                            parse_object_id(&assignment.blocker_id)?,
                            parse_object_id(&assignment.attacker_id)?,
                        ))
                    })
                    .collect::<Result<Vec<_>>>()?,
            })
        }
        PromptOutput::ChooseBoardTargets(ChooseBoardTargetsOutput::BoardTargets { chosen }) => {
            Ok(GameAction::SelectTargets {
                targets: chosen
                    .iter()
                    .map(target_ref_from_dto)
                    .collect::<Result<Vec<_>>>()?,
            })
        }
        PromptOutput::ChooseNumber(ChooseNumberOutput::NumberDecision { chosen_number }) => {
            match chosen_number {
                // CR 107.3 + CR 107.1b: X is a value its controller chooses, and
                // a negative number can never be chosen — so a declined or
                // negative answer is not a legal X.
                Some(value) if value >= 0 => match &state.waiting_for {
                    WaitingFor::ChooseXValue { .. } => Ok(GameAction::ChooseX {
                        value: value as u32,
                    }),
                    // Every other numeric pause reaches the client through the
                    // projection, and its answering action is the engine's to
                    // name — `ChooseX` is specific to X, not to numbers.
                    _ => interaction_number_action(state, context.deciding_player, value as u32),
                },
                _ => Err(AdapterError::IllegalResponseForPrompt {
                    response_kind: "numberDecision",
                }),
            }
        }
        PromptOutput::ChooseFromSelection(ChooseFromSelectionOutput::SelectionDecision {
            chosen_indices,
        }) => match &state.waiting_for {
            // The two bespoke producers of this family. Their answer is a list
            // of mode indices — one response covering several picks — which is
            // not the one-choice-per-answer shape the projection returns, so it
            // cannot route through `ExactChoices`.
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. } => {
                Ok(GameAction::SelectModes {
                    indices: chosen_indices,
                })
            }
            _ => interaction_selection_action(state, context.deciding_player, &chosen_indices),
        },
        PromptOutput::ChooseColor(ChooseColorOutput::ColorDecision { chosen_colors }) => {
            translate_color_decision(&state.waiting_for, chosen_colors)
        }
        PromptOutput::ChooseCombatDamageAssignment(
            ChooseCombatDamageAssignmentOutput::CombatDamageAssignmentDecision { assignments },
        ) => Ok(GameAction::AssignCombatDamage {
            mode: Default::default(),
            assignments: assignments
                .iter()
                .map(|assignment| {
                    Ok((
                        parse_object_id(&assignment.assignee_id)?,
                        assignment.damage.max(0) as u32,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
            trample_damage: 0,
            controller_damage: 0,
        }),
        PromptOutput::Scry(ScryOutput::ScryDecision { zone_card_ids }) => {
            let bottom = zone_card_ids.get(1).cloned().unwrap_or_default();
            Ok(GameAction::SelectCards {
                cards: parse_object_ids(&bottom)?,
            })
        }
        // A yes/no answer is meaningless without knowing which question was
        // asked, so the engine action is selected by the pending `WaitingFor`.
        // `output_family_matches_waiting` has already established that the
        // pairing is legal, so any other state here is unreachable rather than
        // merely unhandled.
        PromptOutput::ChooseBoolean(ChooseBooleanOutput::Decision { value }) => {
            match &state.waiting_for {
                // CR 603.12: accept or decline the optional trigger.
                WaitingFor::OptionalEffectChoice { .. } | WaitingFor::OpponentMayChoice { .. } => {
                    Ok(GameAction::DecideOptionalEffect { accept: value })
                }
                // CR 702.94a: accepting casts for the miracle cost; declining
                // routes through the shared optional-effect decline.
                WaitingFor::MiracleReveal { object_id, .. } => {
                    if value {
                        let object =
                            state
                                .objects
                                .get(object_id)
                                .ok_or(AdapterError::ObjectNotFound {
                                    object_id: *object_id,
                                })?;
                        Ok(GameAction::CastSpellAsMiracle {
                            object_id: *object_id,
                            card_id: object.card_id,
                            payment_mode: Default::default(),
                        })
                    } else {
                        Ok(GameAction::DecideOptionalEffect { accept: false })
                    }
                }
                // CR 701.43d: pay or decline the exert cost.
                WaitingFor::ExertChoice { .. } => Ok(GameAction::ChooseExert { exert: value }),
                // CR 118.12: pay the unless-cost or let the effect happen.
                WaitingFor::UnlessPayment { .. } => Ok(GameAction::PayUnlessCost { pay: value }),
                _ => Err(AdapterError::IllegalResponseForPrompt {
                    response_kind: "chooseBoolean",
                }),
            }
        }
        PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision { chosen_card_ids }) => {
            match &state.waiting_for {
                // CR 701.9b: an effect that causes a discard lets the affected
                // player choose which cards, so the chosen cards are exactly the
                // discarded ones. The one bespoke producer of this family, whose
                // answer is already the card list the engine's action wants.
                WaitingFor::DiscardChoice { .. } => Ok(GameAction::SelectCards {
                    cards: parse_object_ids(&chosen_card_ids)?,
                }),
                // Every other card selection reaches the client through the
                // projection, and its answering action is the engine's to name —
                // `SelectCards` is one of several the `Select` schema
                // materializes into.
                _ => interaction_cards_action(state, context.deciding_player, &chosen_card_ids),
            }
        }
        // CR 603.3b: `ReorderItem::id` is the trigger's index in the prompt's
        // list (see the `OrderTriggers` prompt arm), so the answer parses back
        // into `GameAction::OrderTriggers { order: Vec<usize> }` directly.
        PromptOutput::Reorder(ReorderOutput::ReorderDecision { ordered_ids }) => {
            let order = ordered_ids
                .iter()
                .map(|id| {
                    id.parse::<usize>()
                        .map_err(|_| AdapterError::IllegalResponseForPrompt {
                            response_kind: "reorder",
                        })
                })
                .collect::<Result<Vec<_>>>()?;
            Ok(GameAction::OrderTriggers { order })
        }
        // Families the adapter models on the wire but cannot yet drive into the
        // engine. `output_family_matches_waiting` already rejects these, so this
        // arm is the belt-and-braces leg of the same contract.
        PromptOutput::ChooseDamageAssignmentOrder(_)
        | PromptOutput::RevealCards(_)
        | PromptOutput::DiceRolled(_) => Err(AdapterError::IllegalResponseForPrompt {
            response_kind: "unsupportedOutput",
        }),
    }
}

/// Convert one engine action into the protocol action that advertises it.
///
/// `state` is the snapshot the action was drawn from
/// ([`PreparedManabrewSnapshot::state`]). It is read, never interpreted: the
/// only questions asked of it are "what is this object's name" and "which
/// ability slot holds this object's ninjutsu marker", both of which are lookups
/// the engine already answers.
pub fn convert_available_action(
    state: &GameState,
    action: &GameAction,
    id: String,
) -> AvailableActionConversion {
    match action {
        GameAction::CastSpell { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, PlayCardMode::Normal, "Cast"),
        ),
        // CR 305.1: a land play is not a spell, but `AvailableActionKind::Cast`
        // is the only kind a card play can travel as (upstream has exactly
        // `Cast`, `ActivateAbility`, `UndoMana`). `PlayLand` carries no face
        // discriminator, so the mode is always `Normal` — never `BackFaceLand`.
        GameAction::PlayLand { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, PlayCardMode::Normal, "Play land"),
        ),
        // No free-cast `PlayCardMode` and no `Miracle` alternative cost exist
        // upstream. `Normal` asserts nothing, whereas `StaticAlternative` would
        // assert semantics we cannot verify — and suppressing these entirely
        // would remove legal plays. Recorded as fidelity gaps.
        GameAction::CastSpellForFree { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, PlayCardMode::Normal, "Cast for free"),
        ),
        GameAction::CastSpellAsMiracle { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, PlayCardMode::Normal, "Cast with miracle"),
        ),
        GameAction::CastSpellAsMadness { object_id, .. } => {
            AvailableActionConversion::Available(cast_available_action(
                id,
                *object_id,
                PlayCardMode::Alternative {
                    cost: AlternativeCostKind::Madness,
                },
                "Cast with madness",
            ))
        }
        // CR 702.188 / CR 702.190: exact `AlternativeCostKind` counterparts.
        // Note the engine field is `hand_object`, not `object_id`.
        GameAction::CastSpellAsSneak { hand_object, .. } => {
            AvailableActionConversion::Available(cast_available_action(
                id,
                *hand_object,
                PlayCardMode::Alternative {
                    cost: AlternativeCostKind::Sneak,
                },
                "Cast with sneak",
            ))
        }
        GameAction::CastSpellAsWebSlinging { hand_object, .. } => {
            AvailableActionConversion::Available(cast_available_action(
                id,
                *hand_object,
                PlayCardMode::Alternative {
                    cost: AlternativeCostKind::WebSlinging,
                },
                "Cast with web-slinging",
            ))
        }
        // CR 702.143: foretelling exiles the card face down; the later cast from
        // exile is a separate action carrying `AlternativeCostKind::Foretell`.
        GameAction::Foretell { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, PlayCardMode::ForetellExile, "Foretell"),
        ),
        // `PlayFaceDown` carries no mode discriminator: morph, megamorph, and
        // disguise are indistinguishable at the action level (and disguise has no
        // `AlternativeCostKind` at all), so it cannot be mapped to either
        // `Morph` or `Megamorph` without guessing.
        GameAction::PlayFaceDown { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, PlayCardMode::Normal, "Play face down"),
        ),
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => AvailableActionConversion::Available(AvailableAction {
            id,
            kind: AvailableActionKind::ActivateAbility(ActivatableAbilityInfo {
                card_id: encode_object_id(*source_id),
                ability_index: *ability_index,
                description: String::new(),
                is_mana_ability: false,
                cost: None,
                produced_mana: None,
            }),
        }),
        GameAction::TapLandForMana { selection } => {
            AvailableActionConversion::Available(AvailableAction {
                id,
                kind: AvailableActionKind::ActivateAbility(ActivatableAbilityInfo {
                    card_id: encode_object_id(selection.source.object_id),
                    ability_index: selection.ability_index.unwrap_or(0),
                    description: "Activate mana ability".to_string(),
                    is_mana_ability: true,
                    cost: None,
                    produced_mana: None,
                }),
            })
        }
        GameAction::UntapLandForMana { object_id } => {
            AvailableActionConversion::Available(AvailableAction {
                id,
                kind: AvailableActionKind::UndoMana {
                    card_id: encode_object_id(*object_id),
                },
            })
        }
        GameAction::PassPriority | GameAction::CancelCast | GameAction::Concede { .. } => {
            AvailableActionConversion::Skip
        }
        GameAction::DeclareAttackers { .. } => AvailableActionConversion::Skip,
        GameAction::DeclareBlockers { .. } => AvailableActionConversion::Skip,
        GameAction::ChooseUntap { .. } => {
            AvailableActionConversion::Unsupported("local.choose-untap-unsupported")
        }
        // Answered through the ChooseBoolean prompt for `WaitingFor::ExertChoice`,
        // not by echoing an action id — same contract as SelectTargets.
        GameAction::ChooseExert { .. } => AvailableActionConversion::Skip,
        GameAction::ChooseEnlist { .. } => {
            AvailableActionConversion::Unsupported("local.enlist-unsupported")
        }
        GameAction::ChooseMeldPair { .. } => {
            AvailableActionConversion::Unsupported("local.meld-pair-choice-unsupported")
        }
        GameAction::ChooseEntryAttackTarget { .. } => {
            AvailableActionConversion::Unsupported("local.entry-attack-target-choice-unsupported")
        }
        GameAction::ChooseClashOpponent { .. } => {
            AvailableActionConversion::Unsupported("local.clash-unsupported")
        }
        GameAction::ChooseZoneOpponentChooser { .. } => {
            AvailableActionConversion::Unsupported("local.zone-opponent-chooser-unsupported")
        }
        GameAction::ChooseAnnouncingOpponent { .. } => {
            AvailableActionConversion::Unsupported("local.announcing-opponent-unsupported")
        }
        GameAction::ChooseGiftRecipient { .. } => {
            AvailableActionConversion::Unsupported("local.gift-recipient-unsupported")
        }
        GameAction::ChoosePileOpponent { .. } => {
            AvailableActionConversion::Unsupported("local.pile-opponent-unsupported")
        }
        GameAction::ChooseAssistPlayer { .. } | GameAction::CommitAssistPayment { .. } => {
            AvailableActionConversion::Unsupported("local.assist-unsupported")
        }
        GameAction::MulliganDecision { .. } => AvailableActionConversion::Skip,
        GameAction::ReorderHand { .. } => {
            AvailableActionConversion::Unsupported("local.reorder-hand-unsupported")
        }
        // Spending or unspending a specific pool entry needs pool entries to
        // exist on the wire; v2's pool is a per-color count.
        GameAction::SpendPoolMana { .. } | GameAction::UnspendPoolMana { .. } => {
            AvailableActionConversion::Unsupported("upstream.mana-pool-entries-missing")
        }
        GameAction::SelectCards { .. } => AvailableActionConversion::Skip,
        GameAction::ChooseRemoveCounterCostDistribution { .. } => {
            AvailableActionConversion::Unsupported("local.counter-cost-distribution-unsupported")
        }
        GameAction::ChooseCountersToRemove { .. } => {
            AvailableActionConversion::Unsupported("local.counter-removal-unsupported")
        }
        GameAction::SelectCoinFlips { .. } => {
            AvailableActionConversion::Unsupported("local.coin-flip-unsupported")
        }
        GameAction::ChooseOutsideGameCards { .. } => {
            AvailableActionConversion::Unsupported("local.outside-game-selection-unsupported")
        }
        GameAction::SelectTargets { .. } | GameAction::ChooseTarget { .. } => {
            AvailableActionConversion::Skip
        }
        GameAction::ChooseReplacement { .. } => {
            AvailableActionConversion::Unsupported("local.replacement-choice-unsupported")
        }
        // Answered through the Reorder prompt for `WaitingFor::OrderTriggers`.
        GameAction::OrderTriggers { .. } => AvailableActionConversion::Skip,
        GameAction::Equip { .. }
        | GameAction::CrewVehicle { .. }
        | GameAction::ActivateStation { .. }
        | GameAction::SaddleMount { .. }
        | GameAction::Transform { .. }
        | GameAction::TurnFaceUp { .. } => {
            AvailableActionConversion::Unsupported("local.board-action-unsupported")
        }
        GameAction::SubmitSideboard { .. } => {
            AvailableActionConversion::Unsupported("local.deck-dto-not-implemented")
        }
        GameAction::ChoosePlayDraw { .. } => {
            AvailableActionConversion::Unsupported("local.play-draw-unsupported")
        }
        GameAction::ChooseOption { .. }
        | GameAction::SubmitVoteCandidate { .. }
        | GameAction::SubmitSpellbookDraft { .. }
        | GameAction::ChoosePile { .. }
        | GameAction::ChooseBranch { .. }
        | GameAction::SubmitLifeRedistribution { .. }
        | GameAction::ChooseDamageSource { .. } => {
            AvailableActionConversion::Unsupported("local.selection-unsupported")
        }
        GameAction::SubmitPilePartition { .. } => {
            AvailableActionConversion::Unsupported("local.pile-partition-unsupported")
        }
        GameAction::SelectModes { .. } => AvailableActionConversion::Skip,
        // `DecideOptionalEffect` answers the ChooseBoolean prompt emitted for
        // `WaitingFor::OptionalEffectChoice` / `OpponentMayChoice` / `MiracleReveal`.
        GameAction::DecideOptionalEffect { .. } => AvailableActionConversion::Skip,
        GameAction::DecideOptionalCost { .. }
        | GameAction::DecideOptionalEffectAndRemember { .. } => {
            AvailableActionConversion::Unsupported("local.optional-trigger-unsupported")
        }
        GameAction::ChooseAdventureFace { .. }
        | GameAction::ChooseModalFace { .. }
        | GameAction::ChooseAlternativeCast { .. }
        | GameAction::ChooseCastingVariant { .. }
        | GameAction::ChoosePermanentTypeSlot { .. } => {
            AvailableActionConversion::Unsupported("local.cast-choice-unsupported")
        }
        GameAction::KeepAllCopyTargets | GameAction::RetargetSpell { .. } => {
            AvailableActionConversion::Unsupported("local.retarget-unsupported")
        }
        // CR 702.49a: ninjutsu is an ACTIVATED ABILITY, not an alternative cost,
        // so its absence from `AlternativeCostKind` says nothing — the home is
        // `ActivateAbility`, which already exists. The engine agrees: the
        // keyword is synthesized as an `AbilityKind::Activated` carrying
        // `AbilityCost::NinjutsuFamily`, and the engine enumerates one
        // `ActivateNinjutsu` per (ninjutsu card, returned attacker) pair
        // (CR 702.49d covers the commander variant with the same action), so
        // this arm converts pairs one-for-one rather than fanning out.
        //
        // `ability_index` is descriptive metadata only: the answer round-trips
        // by echoed action id through `advertised_action_by_id`, which hands
        // back the original `ActivateNinjutsu`. That matters, because the
        // engine explicitly forbids driving the marker slot through
        // `GameAction::ActivateAbility` — its `NinjutsuFamily` cost arm is a
        // no-op in `pay_ability_cost`, so that route would stack the ability
        // without paying mana.
        GameAction::ActivateNinjutsu {
            ninjutsu_object_id,
            creature_to_return,
        } => AvailableActionConversion::Available(AvailableAction {
            id,
            kind: AvailableActionKind::ActivateAbility(ActivatableAbilityInfo {
                card_id: encode_object_id(*ninjutsu_object_id),
                ability_index: ninjutsu_marker_ability_index(state, *ninjutsu_object_id),
                // CR 702.49c: the returned creature fixes what the ninja enters
                // attacking, so naming it is what distinguishes the pairs.
                description: format!(
                    "Ninjutsu — return {}",
                    object_name(state, *creature_to_return)
                ),
                is_mana_ability: false,
                cost: None,
                produced_mana: None,
            }),
        }),
        GameAction::RespondToSpliceOffer { .. } => {
            AvailableActionConversion::Unsupported("local.splice-unsupported")
        }
        // Answered through the ChooseBoolean prompt for `WaitingFor::UnlessPayment`.
        GameAction::PayUnlessCost { .. } => AvailableActionConversion::Skip,
        // Still unmapped: picking among several sub-costs is a selection, not a
        // boolean (CR 118.12a), so it does not share the UnlessPayment prompt.
        GameAction::ChooseUnlessCostBranch { .. } => {
            AvailableActionConversion::Unsupported("local.cost-prevention-unsupported")
        }
        GameAction::ChooseActivationCostBranch { .. } => {
            AvailableActionConversion::Unsupported("local.activation-cost-choice-unsupported")
        }
        GameAction::PayCombatTax { .. } => {
            AvailableActionConversion::Unsupported("local.pay-combat-cost-unsupported")
        }
        GameAction::ChooseRingBearer { .. }
        | GameAction::ChoosePair { .. }
        | GameAction::ChooseLegend { .. }
        | GameAction::ChooseBattleProtector { .. }
        | GameAction::SelectCategoryPermanents { .. }
        | GameAction::ChooseKeptCreatures { .. }
        | GameAction::ChooseKeptPermanents { .. } => {
            AvailableActionConversion::Unsupported("local.non-target-selection-unsupported")
        }
        GameAction::ChooseDungeon { .. }
        | GameAction::ChooseDungeonRoom { .. }
        | GameAction::UnlockRoomDoor { .. }
        | GameAction::ChooseRoomDoor { .. } => {
            AvailableActionConversion::Unsupported("local.dungeon-room-unsupported")
        }
        GameAction::RollPlanarDie => {
            AvailableActionConversion::Unsupported("local.planar-die-unsupported")
        }
        // CR 702.51 (convoke): a payment action, not a priority action — it is
        // advertised through `PaymentActionKind::UseResource` during mana
        // payment. See `convert_payment_action`.
        GameAction::TapForConvoke { .. } => AvailableActionConversion::Skip,
        // CR 702.180: harmonize is structurally the analogue of convoke — a
        // cost-reduction tap during payment, carrying the creature being tapped
        // rather than a card being cast — but `PaymentResourceKind` is exactly
        // `Convoke | Improvise | Delve`, so it has no counterpart either way.
        GameAction::HarmonizeTap { .. } => {
            AvailableActionConversion::Unsupported("local.harmonize-tap-unsupported")
        }
        GameAction::DeclareCompanion { .. } | GameAction::CompanionToHand => {
            AvailableActionConversion::Unsupported("local.companion-unsupported")
        }
        // CR 116.2c: the pay-to-end special action has no Manabrew counterpart.
        GameAction::EndContinuousEffect { .. } => {
            AvailableActionConversion::Unsupported("local.end-continuous-effect-unsupported")
        }
        GameAction::DiscoverChoice { .. }
        | GameAction::GraveyardPaidCastChoice { .. }
        | GameAction::CascadeChoice { .. }
        | GameAction::RippleChoice { .. }
        | GameAction::FreeCastWindowChoice { .. } => {
            AvailableActionConversion::Unsupported("local.cast-offer-unsupported")
        }
        GameAction::ChooseTopOrBottom { .. } => {
            AvailableActionConversion::Unsupported("local.top-bottom-unsupported")
        }
        GameAction::ChooseMutateMergeSide { .. } => {
            AvailableActionConversion::Unsupported("local.mutate-unsupported")
        }
        GameAction::CipherEncode { .. } => {
            AvailableActionConversion::Unsupported("local.cipher-unsupported")
        }
        GameAction::SetAutoPass { .. }
        | GameAction::CancelAutoPass
        | GameAction::SetPhaseStops { .. }
        | GameAction::SetPriorityPassingMode { .. }
        | GameAction::SetPriorityYield { .. }
        | GameAction::SetMayTriggerAutoChoice { .. }
        | GameAction::SetTriggerOrderTemplate { .. } => {
            AvailableActionConversion::Unsupported("local.autopass-settings-unsupported")
        }
        GameAction::AssignCombatDamage { .. } => AvailableActionConversion::Skip,
        GameAction::AssignBlockerDamage { .. } => {
            AvailableActionConversion::Unsupported("local.blocker-damage-banding-unsupported")
        }
        GameAction::DistributeAmong { .. } => {
            AvailableActionConversion::Unsupported("local.distribution-unsupported")
        }
        GameAction::ChooseCounterMoveDistribution { .. } => {
            AvailableActionConversion::Unsupported("local.counter-move-distribution-unsupported")
        }
        GameAction::SubmitPayAmount { .. } => {
            AvailableActionConversion::Unsupported("local.pay-amount-unsupported")
        }
        GameAction::LearnDecision { .. } => {
            AvailableActionConversion::Unsupported("local.learn-unsupported")
        }
        GameAction::ChooseX { .. } => AvailableActionConversion::Skip,
        // CR 107.4f + CR 601.2h: a Phyrexian shard is a payment move, not a
        // priority action — it is advertised through `PaymentActionKind::PayLife`
        // while the payment prompt is open. Same contract as `TapForConvoke`.
        // See `convert_payment_action` / `payment_actions`.
        GameAction::SubmitPhyrexianChoices { .. } => AvailableActionConversion::Skip,
        GameAction::ChooseManaColor { .. } | GameAction::PayManaAbilityMana { .. } => {
            AvailableActionConversion::Skip
        }
        GameAction::CastPreparedCopy { .. } | GameAction::CastParadigmCopy { .. } => {
            AvailableActionConversion::Unsupported("local.copy-cast-unsupported")
        }
        GameAction::ChooseSpecializeColor { .. } => {
            AvailableActionConversion::Unsupported("local.specialize-unsupported")
        }
        GameAction::PassParadigmOffer => {
            AvailableActionConversion::Unsupported("local.paradigm-offer-unsupported")
        }
        GameAction::Debug(_)
        | GameAction::GrantDebugPermission { .. }
        | GameAction::RevokeDebugPermission { .. } => {
            AvailableActionConversion::Unsupported("local.debug-action-unsupported")
        }
        // CR 732.2a/b/c: the interactive loop-shortcut protocol is opt-in
        // (`LoopDetectionMode::Interactive`) and never reached on the legacy manabrew
        // protocol — a legacy client never sets that mode.
        GameAction::DeclareShortcut { .. }
        | GameAction::RespondToShortcut { .. }
        | GameAction::DeclineShortcut
        | GameAction::PrecastCopyShortcut { .. } => {
            AvailableActionConversion::Unsupported("local.loop-shortcut-unsupported")
        }
    }
}

// The three id prefixes are wire vocabulary, not local convention: upstream's
// own producer parses exactly `card-`, `player-`, and `stack-`. A stack id sent
// under any other prefix fails upstream's `parse_stack_id`, so a `TargetRef`
// naming a spell resolves against nothing.
pub fn encode_object_id(id: ObjectId) -> String {
    format!("card-{}", id.0)
}

pub fn encode_player_id(id: PlayerId) -> String {
    format!("player-{}", id.0)
}

pub fn encode_stack_id(id: ObjectId) -> String {
    format!("stack-{}", id.0)
}

pub fn parse_object_id(value: &str) -> Result<ObjectId> {
    value
        .strip_prefix("card-")
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(ObjectId)
        .ok_or_else(|| AdapterError::MalformedId {
            expected_prefix: "card-",
            value: value.to_string(),
        })
}

pub fn parse_player_id(value: &str) -> Result<PlayerId> {
    value
        .strip_prefix("player-")
        .and_then(|raw| raw.parse::<u8>().ok())
        .map(PlayerId)
        .ok_or_else(|| AdapterError::MalformedId {
            expected_prefix: "player-",
            value: value.to_string(),
        })
}

pub fn parse_stack_id(value: &str) -> Result<ObjectId> {
    value
        .strip_prefix("stack-")
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(ObjectId)
        .ok_or_else(|| AdapterError::MalformedId {
            expected_prefix: "stack-",
            value: value.to_string(),
        })
}

fn player_index(state: &GameState, player_id: PlayerId) -> Result<usize> {
    state
        .players
        .iter()
        .position(|player| player.id == player_id)
        .ok_or(AdapterError::UnsupportedPlayerCount {
            count: state.players.len(),
        })
}

/// CR 500–514: the engine's twelve `Phase`s onto the protocol's thirteen
/// `StepKind`s.
///
/// `StepKind::CombatFirstStrikeDamage` is the unmatched thirteenth, but not
/// because the engine leaves CR 510.4 unmodelled. It models it as a second
/// entry into `Phase::CombatDamage`, discriminated by
/// `CombatState::first_strike_done` — which is exactly what CR 510.4
/// describes ("the phase gets a second combat damage step").
///
/// This signature is what makes the variant unproducible: a `Phase` alone
/// cannot carry that flag, and deciding whether a first-strike step is
/// *pending* additionally needs the private participant set. Computing it
/// here would put game logic in a serialization boundary.
fn phase_step(phase: Phase) -> StepKind {
    match phase {
        Phase::Untap => StepKind::Untap,
        Phase::Upkeep => StepKind::Upkeep,
        Phase::Draw => StepKind::Draw,
        Phase::PreCombatMain => StepKind::Main1,
        Phase::BeginCombat => StepKind::CombatBegin,
        Phase::DeclareAttackers => StepKind::CombatDeclareAttackers,
        Phase::DeclareBlockers => StepKind::CombatDeclareBlockers,
        Phase::CombatDamage => StepKind::CombatDamage,
        Phase::EndCombat => StepKind::CombatEnd,
        Phase::PostCombatMain => StepKind::Main2,
        Phase::End => StepKind::EndOfTurn,
        Phase::Cleanup => StepKind::Cleanup,
    }
}

struct CardBuildContext<'a, L> {
    card_lookup: &'a L,
}

fn objects_from_ids<L: CardTextLookup>(
    state: &GameState,
    ids: &engine::im::Vector<ObjectId>,
    ctx: &CardBuildContext<'_, L>,
) -> Result<Vec<CardDto>> {
    ids.iter()
        .map(|id| {
            let object = state
                .objects
                .get(id)
                .ok_or(AdapterError::ObjectNotFound { object_id: *id })?;
            build_card_dto(state, object, ctx)
        })
        .collect()
}

fn object_vec_from_slice<L: CardTextLookup>(
    state: &GameState,
    ids: &[ObjectId],
    ctx: &CardBuildContext<'_, L>,
) -> Result<Vec<CardDto>> {
    ids.iter()
        .map(|id| {
            let object = state
                .objects
                .get(id)
                .ok_or(AdapterError::ObjectNotFound { object_id: *id })?;
            build_card_dto(state, object, ctx)
        })
        .collect()
}

/// Is this object's identity concealed from the snapshot's viewer?
///
/// `filter_state_for_viewer` conceals by rewriting the object in place — name
/// becomes `"Hidden Card"` and `face_down` is set — rather than by removing it,
/// which is what lets `ZoneDto::count` stay truthful.
fn is_concealed(object: &GameObject) -> bool {
    object.name == HIDDEN_CARD_NAME || object.face_down
}

const HIDDEN_CARD_NAME: &str = "Hidden Card";

/// How much of an object the snapshot's viewer may be told.
///
/// The two restricted cases are genuinely different and must not collapse into
/// one "redacted" flag: a face-down *permanent* is a public object with a
/// private face (CR 400.2 / CR 708.2), whereas a card concealed in a hidden zone must leak
/// nothing at all.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CardVisibility {
    /// Every characteristic is visible.
    Full,
    /// CR 708.2: identity, text, and costs are withheld; board state is not.
    FaceDownPermanent,
    /// CR 406.3 / hidden zones: nothing may leak.
    Concealed,
}

impl CardVisibility {
    fn of(object: &GameObject) -> Self {
        match (is_concealed(object), object.zone) {
            (false, _) => Self::Full,
            (true, Zone::Battlefield) => Self::FaceDownPermanent,
            (true, _) => Self::Concealed,
        }
    }
}

/// Build every `(zone, owner)` bucket for the view.
///
/// Implements the four visibility rules, which differ per zone:
///
/// 1. **Hand** — entries only for cards the recipient may identify; other seats
///    get `count` alone.
/// 2. **Library** — `count` alone, *plus* the top card as a visible entry when
///    the recipient may look at it (CR 701.20e).
/// 3. **Face-down exile** — a `hidden` entry per card (CR 406.3), so the client
///    renders an anonymous face-down card without learning its identity.
/// 4. **Face-down battlefield permanents** — **never** `hidden`. The permanent
///    itself is public (CR 400.2 / CR 708.2); the recipient gets a redacted *visible*
///    entry whose public state (tapped, counters, damage) survives.
///
/// Rule 4 is the trap: `Hidden` is right for rule 3 and wrong for rule 4, and
/// both produce wire-plausible output.
fn build_zones<L: CardTextLookup>(
    state: &GameState,
    ctx: &CardBuildContext<'_, L>,
) -> Result<Vec<ZoneDto>> {
    let mut zones = Vec::new();

    // CR 110.2: a permanent is bucketed by its CONTROLLER, not its owner.
    for player in &state.players {
        let cards = state
            .battlefield
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.controller == player.id)
            .map(|object| build_card_dto(state, object, ctx).map(CardView::Visible))
            .collect::<Result<Vec<_>>>()?;
        zones.push(ZoneDto {
            zone: ZoneKind::Battlefield,
            owner_id: encode_player_id(player.id),
            count: cards.len(),
            cards,
        });
    }

    for player in &state.players {
        // Rule 1 + rule 2: concealed cards are dropped from `cards` but still
        // counted, so `count` remains the truthful total.
        for (zone, ids) in [
            (ZoneKind::Hand, &player.hand),
            (ZoneKind::Library, &player.library),
            (ZoneKind::Graveyard, &player.graveyard),
        ] {
            let mut cards = Vec::new();
            for object in ids.iter().filter_map(|id| state.objects.get(id)) {
                if !is_concealed(object) {
                    cards.push(CardView::Visible(build_card_dto(state, object, ctx)?));
                }
            }
            zones.push(ZoneDto {
                zone,
                owner_id: encode_player_id(player.id),
                cards,
                count: ids.len(),
            });
        }
    }

    // Rule 3: exile is a public zone, so a concealed card is present but
    // anonymous — a `hidden` entry rather than an omission.
    for player in &state.players {
        let cards = state
            .exile
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.owner == player.id)
            .map(|object| {
                if is_concealed(object) {
                    Ok(CardView::Hidden {
                        id: encode_object_id(object.id),
                    })
                } else {
                    build_card_dto(state, object, ctx).map(CardView::Visible)
                }
            })
            .collect::<Result<Vec<_>>>()?;
        zones.push(ZoneDto {
            zone: ZoneKind::Exile,
            owner_id: encode_player_id(player.id),
            count: cards.len(),
            cards,
        });
    }

    for player in &state.players {
        let cards = state
            .command_zone
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.owner == player.id)
            .map(|object| build_card_dto(state, object, ctx).map(CardView::Visible))
            .collect::<Result<Vec<_>>>()?;
        zones.push(ZoneDto {
            zone: ZoneKind::Command,
            owner_id: encode_player_id(player.id),
            count: cards.len(),
            cards,
        });
    }

    Ok(zones)
}

fn build_card_dto<L: CardTextLookup>(
    state: &GameState,
    object: &GameObject,
    ctx: &CardBuildContext<'_, L>,
) -> Result<CardDto> {
    let visibility = CardVisibility::of(object);
    let identity_visible = matches!(visibility, CardVisibility::Full);
    // CR 400.2: the battlefield is a public zone, with an explicit carve-out for
    // cards a rule or effect allows to be face down — so the permanent is public
    // even though CR 708.2 withholds its face. Its board state survives; a card
    // concealed in a hidden zone leaks nothing.
    let board_state_visible = !matches!(visibility, CardVisibility::Concealed);

    let text = if identity_visible {
        if let Some(text) = &object.token_rules_text {
            text.clone()
        } else {
            ctx.card_lookup
                .text_for(object)
                .ok_or(AdapterError::MissingCardText {
                    object_id: object.id,
                })?
        }
    } else {
        String::new()
    };
    let attack_target = attack_target_id(state, object.id);

    Ok(CardDto {
        id: encode_object_id(object.id),
        identity: CardIdentity {
            // Blank rather than "Hidden Card": clients render an empty
            // `identity.name` as a card back.
            name: if identity_visible {
                object.name.clone()
            } else {
                String::new()
            },
            set_code: String::new(),
            card_number: String::new(),
            is_token: identity_visible && object.is_token,
        },
        color: if identity_visible {
            colors_string(&object.color)
        } else {
            String::new()
        },
        mana_cost: if identity_visible {
            mana_cost_string(&object.mana_cost)
        } else {
            String::new()
        },
        cmc: if identity_visible {
            object.mana_cost.mana_value() as i32
        } else {
            0
        },
        // CR 708.2a: a face-down permanent still HAS a card type (it is a 2/2
        // creature), which the engine has already computed — so core types
        // follow board-state visibility, while the creature types and
        // supertypes it explicitly loses follow identity visibility.
        types: if board_state_visible {
            object
                .card_types
                .core_types
                .iter()
                .map(ToString::to_string)
                .collect()
        } else {
            Vec::new()
        },
        subtypes: if identity_visible {
            object.card_types.subtypes.clone()
        } else {
            Vec::new()
        },
        supertypes: if identity_visible {
            object
                .card_types
                .supertypes
                .iter()
                .map(ToString::to_string)
                .collect()
        } else {
            Vec::new()
        },
        power: board_state_visible
            .then(|| object.power.map(|value| value.to_string()))
            .flatten(),
        toughness: board_state_visible
            .then(|| object.toughness.map(|value| value.to_string()))
            .flatten(),
        base_power: board_state_visible.then_some(object.base_power).flatten(),
        base_toughness: board_state_visible
            .then_some(object.base_toughness)
            .flatten(),
        text,
        controller_id: encode_player_id(object.controller),
        owner_id: encode_player_id(object.owner),
        tapped: object.tapped,
        is_crewed: false,
        is_attacking: attack_target.is_some(),
        attacking_player_id: attacking_player_id(state, object.id).map(encode_player_id),
        attack_target_id: attack_target,
        // CR 708.2: a face-down permanent has no abilities.
        keywords: if identity_visible {
            object.keywords.iter().map(ToString::to_string).collect()
        } else {
            Vec::new()
        },
        counters: if board_state_visible {
            object
                .counters
                .iter()
                .map(|(kind, count)| (kind.as_str().into_owned(), *count))
                .collect()
        } else {
            BTreeMap::new()
        },
        damage: if board_state_visible {
            object.damage_marked as i32
        } else {
            0
        },
        summoning_sick: board_state_visible && object.has_summoning_sickness,
        is_copy: false,
        // CR 712.1 + CR 710.1b: the engine owns "is this permanent
        // double-faced?". `back_face.is_some()` is not that predicate — a CR 710
        // flip card parks its alternative half in the same slot (as do Adventure
        // and Omen cards), so the raw check reports every flip card as a DFC.
        is_double_faced: identity_visible
            && engine::game::transform::is_double_faced_permanent(object),
        is_transformed: identity_visible && object.transformed,
        is_face_down: object.face_down,
        is_bestowed: identity_visible && object.bestow_form.is_some(),
        phased_out: object.is_phased_out(),
        exerted: board_state_visible && state.exerted_this_turn.contains(&object.id),
        is_ring_bearer: board_state_visible
            && state
                .ring_bearer
                .values()
                .any(|bearer| *bearer == Some(object.id)),
        attached_to: board_state_visible
            .then(|| object.attached_to.as_ref().and_then(attach_target_id))
            .flatten(),
        attachment_ids: if board_state_visible {
            object
                .attachments
                .iter()
                .copied()
                .map(encode_object_id)
                .collect()
        } else {
            Vec::new()
        },
        // CR 712.4a / CR 730.2: mutate and meld piles.
        merged_card_ids: if board_state_visible {
            object
                .merged_components
                .iter()
                .copied()
                .map(encode_object_id)
                .collect()
        } else {
            Vec::new()
        },
        flashback_cost: None,
        kicker_cost: None,
        effective_mana_cost: None,
        madness_cost: None,
        is_madness_exiled: false,
        is_plotted: false,
        is_warp_exiled: false,
        foil: false,
        would_die_in_combat: false,
    })
}

/// v2 moved every zone list out of `PlayerDto` and into `GameViewDto::zones`,
/// so building a player no longer needs a card-text lookup.
fn build_player_dto(
    state: &GameState,
    player_id: PlayerId,
    viewer: PlayerId,
    derived: &DerivedViews,
) -> Result<PlayerDto> {
    let index = player_index(state, player_id)?;
    let player = &state.players[index];
    let commander_damage = derived
        .commander_damage_by_attacker
        .values()
        .flat_map(|entries| entries.iter())
        .filter(|entry| entry.victim == player_id)
        .map(|entry| (encode_object_id(entry.commander), entry.damage as i32))
        .collect();

    // CR 122: only non-zero counters are carried, matching how the engine
    // reports them.
    let counters = [
        (PlayerCounterKindDto::Poison, player.poison_counters),
        (PlayerCounterKindDto::Energy, player.energy),
        (
            PlayerCounterKindDto::Experience,
            player.player_counter(&PlayerCounterKind::Experience),
        ),
        (
            PlayerCounterKindDto::Radiation,
            player.player_counter(&PlayerCounterKind::Rad),
        ),
        (
            PlayerCounterKindDto::Ticket,
            player.player_counter(&PlayerCounterKind::Ticket),
        ),
    ]
    .into_iter()
    .filter(|(_, count)| *count > 0)
    .collect();

    Ok(PlayerDto {
        id: encode_player_id(player.id),
        name: state
            .log_player_names
            .get(player.id.0 as usize)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Player {}", player.id.0)),
        // The engine records only THAT a player is out, never why, so a
        // conceding player is indistinguishable from any other eliminated one.
        // `PlayerStatus::Conceded` is therefore never emitted — see
        // `local.player-concede-status-unsourceable`.
        status: if player.is_eliminated {
            PlayerStatus::Lost
        } else {
            PlayerStatus::Playing
        },
        is_human: player.id == viewer,
        life: player.life,
        counters,
        mana_pool: mana_pool_counts(&player.mana_pool.mana),
        commander_damage,
        has_city_blessing: state.city_blessing.contains(&player_id),
        ring_level: state.ring_level.get(&player_id).copied().unwrap_or(0) as i32,
        speed: player.speed.unwrap_or(0) as i32,
    })
}

fn build_stack(state: &GameState, derived: &DerivedViews) -> Vec<StackObjectDto> {
    state
        .stack
        .iter()
        .map(|entry| {
            let source = state.objects.get(&entry.source_id);
            let details = derived.stack_entry_details.get(&entry.id);
            StackObjectDto {
                id: encode_stack_id(entry.id),
                source_id: encode_object_id(entry.source_id),
                controller_id: encode_player_id(entry.controller),
                identity: CardIdentity {
                    name: details
                        .map(|details| details.source_name.clone())
                        .or_else(|| source.map(|source| source.name.clone()))
                        .unwrap_or_default(),
                    set_code: String::new(),
                    card_number: String::new(),
                    is_token: source.is_some_and(|object| object.is_token),
                },
                text: details
                    .and_then(|details| details.ability_description.clone())
                    .unwrap_or_default(),
                is_permanent_spell: matches!(&entry.kind, StackEntryKind::Spell { .. })
                    && source.is_some_and(|object| {
                        object
                            .card_types
                            .core_types
                            .iter()
                            .any(|core| core.is_permanent_type())
                    }),
                is_casting: false,
                targets: details
                    .map(|details| {
                        details
                            .targets
                            .iter()
                            .filter_map(|target| target_ref_dto(&target.target))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn target_ref_dto(target: &TargetRef) -> Option<TargetRefDto> {
    let (kind, id) = match target {
        TargetRef::Object(id) => (TargetKindDto::Card, encode_object_id(*id)),
        TargetRef::Player(id) => (TargetKindDto::Player, encode_player_id(*id)),
    };
    Some(TargetRefDto {
        kind,
        id,
        intent: None,
        oracle: None,
    })
}

fn target_refs(targets: &[TargetRef]) -> Vec<TargetRefDto> {
    targets.iter().filter_map(target_ref_dto).collect()
}

fn combat_assignments(state: &GameState) -> Vec<CombatAssignmentDto> {
    state
        .combat
        .as_ref()
        .map(|combat| {
            combat
                .blocker_to_attacker
                .iter()
                .flat_map(|(blocker, attackers)| {
                    attackers.iter().map(|attacker| CombatAssignmentDto {
                        blocker_id: encode_object_id(*blocker),
                        attacker_id: encode_object_id(*attacker),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

fn attacking_player_id(state: &GameState, object_id: ObjectId) -> Option<PlayerId> {
    state
        .combat
        .as_ref()?
        .attackers
        .iter()
        .find_map(|attacker| {
            (attacker.object_id == object_id).then_some(match attacker.attack_target {
                AttackTarget::Player(player) => player,
                AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => state
                    .objects
                    .get(&id)
                    .map(|object| object.controller)
                    .unwrap_or(attacker.defending_player),
            })
        })
}

fn attack_target_id(state: &GameState, object_id: ObjectId) -> Option<String> {
    state
        .combat
        .as_ref()?
        .attackers
        .iter()
        .find_map(|attacker| {
            (attacker.object_id == object_id).then_some(match attacker.attack_target {
                AttackTarget::Player(player) => encode_player_id(player),
                AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => encode_object_id(id),
            })
        })
}

/// Display name for an object, read straight from the snapshot.
///
/// An id the viewer-filtered state does not carry falls back to the wire id
/// rather than to a guess, so a description never invents a card.
fn object_name(state: &GameState, object_id: ObjectId) -> String {
    state
        .objects
        .get(&object_id)
        .map(|object| object.name.clone())
        .unwrap_or_else(|| encode_object_id(object_id))
}

/// CR 702.49a: index of the object's synthesized ninjutsu-family marker ability.
///
/// The predicate is the engine's (`game::keywords::is_ninjutsu_family_marker_ability`),
/// not a local re-derivation. `0` when the object is out of the viewer's
/// filtered state: the field is descriptive only — see the `ActivateNinjutsu`
/// arm of [`convert_available_action`] for why the round-trip does not use it.
fn ninjutsu_marker_ability_index(state: &GameState, object_id: ObjectId) -> usize {
    state
        .objects
        .get(&object_id)
        .and_then(|object| {
            object
                .abilities
                .iter()
                .position(engine::game::keywords::is_ninjutsu_family_marker_ability)
        })
        .unwrap_or(0)
}

fn available_actions(state: &GameState, actions: &[GameAction]) -> Vec<AvailableAction> {
    actions
        .iter()
        .enumerate()
        .filter_map(|(index, action)| {
            match convert_available_action(state, action, action_id(index)) {
                AvailableActionConversion::Available(action) => Some(action),
                AvailableActionConversion::Skip | AvailableActionConversion::Unsupported(_) => None,
            }
        })
        .collect()
}

fn action_table(actions: &[GameAction]) -> Vec<ActionTableEntry> {
    actions
        .iter()
        .enumerate()
        .map(|(index, action)| ActionTableEntry {
            id: action_id(index),
            action: action.clone(),
        })
        .collect()
}

fn action_id(index: usize) -> String {
    format!("action-{index}")
}

fn advertised_action_by_id(
    context: &PromptContext,
    state: &GameState,
    action_id: &str,
) -> Result<GameAction> {
    let entry = context
        .action_table
        .iter()
        .find(|entry| entry.id == action_id)
        .ok_or_else(|| AdapterError::StaleOrInvalidActionId {
            action_id: action_id.to_string(),
        })?;

    match convert_available_action(state, &entry.action, entry.id.clone()) {
        AvailableActionConversion::Available(_) => Ok(entry.action.clone()),
        AvailableActionConversion::Skip => Err(AdapterError::IllegalResponseForPrompt {
            response_kind: "act",
        }),
        AvailableActionConversion::Unsupported(code) => {
            Err(AdapterError::UnsupportedProtocolFeature { code })
        }
    }
}

fn cast_available_action(
    id: String,
    object_id: ObjectId,
    mode: PlayCardMode,
    label: &'static str,
) -> AvailableAction {
    AvailableAction {
        id,
        kind: AvailableActionKind::Cast {
            card_id: encode_object_id(object_id),
            mode,
            label: label.to_string(),
        },
    }
}

pub enum PaymentActionConversion {
    Available(PaymentAction),
    Skip,
    Unsupported(&'static str),
}

/// Convert one engine action into the payment move it represents.
///
/// The mana-payment analogue of [`convert_available_action`], for the actions
/// the engine offers while `WaitingFor::ManaPayment` or
/// `WaitingFor::PhyrexianPayment` is open.
///
/// `UseResource` for Delve or Improvise and every `ReleaseResource` form stay
/// unproduced: no engine action exists for any of them, so advertising one
/// would hand the client an id the engine then rejects.
pub fn convert_payment_action(action: &GameAction, id: String) -> PaymentActionConversion {
    match action {
        // CR 107.4f: a Phyrexian shard is payable with one mana of its color or
        // with 2 life, so a route's life price is exactly `2 × PayLife shards`.
        // The engine enumerates the routes (`WaitingFor::PhyrexianPayment` legal
        // actions are one `SubmitPhyrexianChoices` per combination), so each
        // advertised entry is a complete, already-legal answer — the adapter
        // never assembles a route of its own. A single pending shard therefore
        // advertises exactly one `PayLife { amount: 2 }`.
        //
        // The all-mana route is skipped rather than advertised as
        // `PayLife { amount: 0 }`: paying no life is not a pay-life move, and
        // `PaymentActionKind::PayLife` carries no other discriminator.
        GameAction::SubmitPhyrexianChoices { choices } => {
            let amount: u32 = choices
                .iter()
                .filter(|choice| matches!(choice, ShardChoice::PayLife))
                .map(|_| 2)
                .sum();
            if amount == 0 {
                PaymentActionConversion::Skip
            } else {
                PaymentActionConversion::Available(PaymentAction {
                    id,
                    kind: PaymentActionKind::PayLife { amount },
                })
            }
        }
        GameAction::TapLandForMana { selection } => {
            PaymentActionConversion::Available(PaymentAction {
                id,
                kind: PaymentActionKind::ActivateManaAbility(ActivatableAbilityInfo {
                    card_id: encode_object_id(selection.source.object_id),
                    ability_index: selection.ability_index.unwrap_or(0),
                    description: "Activate mana ability".to_string(),
                    is_mana_ability: true,
                    cost: None,
                    produced_mana: None,
                }),
            })
        }
        GameAction::UntapLandForMana { object_id } => {
            PaymentActionConversion::Available(PaymentAction {
                id,
                kind: PaymentActionKind::UndoMana {
                    card_id: encode_object_id(*object_id),
                },
            })
        }
        // CR 605.1a: an ability offered during mana payment is a mana ability.
        GameAction::ActivateAbility {
            source_id,
            ability_index,
        } => PaymentActionConversion::Available(PaymentAction {
            id,
            kind: PaymentActionKind::ActivateManaAbility(ActivatableAbilityInfo {
                card_id: encode_object_id(*source_id),
                ability_index: *ability_index,
                description: String::new(),
                is_mana_ability: true,
                cost: None,
                produced_mana: None,
            }),
        }),
        // CR 702.51a: convoke taps a creature to help pay. The only payment
        // resource this engine has an action for.
        GameAction::TapForConvoke { object_id, .. } => {
            PaymentActionConversion::Available(PaymentAction {
                id,
                kind: PaymentActionKind::UseResource {
                    card_id: encode_object_id(*object_id),
                    resource: PaymentResourceKind::Convoke,
                },
            })
        }
        // Prompt-level controls, carried by `canConfirmFromPool` and `cancel`
        // rather than as list entries.
        GameAction::PassPriority | GameAction::CancelCast => PaymentActionConversion::Skip,
        // Fail closed. The engine's legal-action set during `ManaPayment` is
        // narrow, and anything outside the forms above is simply not offered as
        // a payment move rather than being guessed at.
        _ => PaymentActionConversion::Skip,
    }
}

/// Advertise the payment moves for the open mana payment.
///
/// **Invariant:** ids come from `action_id(index)` over the same
/// `prepared.actions` slice that [`action_table`] enumerates. That shared index
/// space is the only reason an echoed `PayManaCostOutput::Act { action_id }`
/// can be resolved back to a `GameAction`. An independent scheme
/// (`mana-{i}` over a filtered list, or a `"tap:perm-9:0"` composite) compiles,
/// passes clippy, and breaks every mana payment against a live client.
fn payment_actions(actions: &[GameAction]) -> Vec<PaymentAction> {
    actions
        .iter()
        .enumerate()
        .filter_map(
            |(index, action)| match convert_payment_action(action, action_id(index)) {
                PaymentActionConversion::Available(action) => Some(action),
                PaymentActionConversion::Skip | PaymentActionConversion::Unsupported(_) => None,
            },
        )
        .collect()
}

/// Resolve an echoed payment action id back to its engine action, rejecting any
/// id that was not advertised as a payment move.
fn advertised_payment_action_by_id(context: &PromptContext, action_id: &str) -> Result<GameAction> {
    let entry = context
        .action_table
        .iter()
        .find(|entry| entry.id == action_id)
        .ok_or_else(|| AdapterError::StaleOrInvalidActionId {
            action_id: action_id.to_string(),
        })?;

    match convert_payment_action(&entry.action, entry.id.clone()) {
        PaymentActionConversion::Available(_) => Ok(entry.action.clone()),
        PaymentActionConversion::Skip => Err(AdapterError::IllegalResponseForPrompt {
            response_kind: "act",
        }),
        PaymentActionConversion::Unsupported(code) => {
            Err(AdapterError::UnsupportedProtocolFeature { code })
        }
    }
}

fn pay_mana_cost_input(prepared: &PreparedManabrewSnapshot) -> PayManaCostInput {
    let card_id = prepared
        .state
        .pending_cast
        .as_ref()
        .map(|pending| encode_object_id(pending.object_id))
        .unwrap_or_default();
    let card_name = prepared
        .state
        .pending_cast
        .as_ref()
        .and_then(|pending| prepared.state.objects.get(&pending.object_id))
        .map(|object| object.name.clone())
        .unwrap_or_default();
    let mana_cost = prepared
        .state
        .pending_cast
        .as_ref()
        .map(|pending| mana_cost_string(&pending.cost))
        .unwrap_or_default();

    PayManaCostInput {
        presentation: presentation(if card_name.is_empty() {
            "Pay mana cost".to_string()
        } else {
            format!("Pay for {card_name}")
        }),
        card_id,
        card_name,
        mana_cost,
        can_confirm_from_pool: prepared
            .actions
            .iter()
            .any(|action| matches!(action, GameAction::PassPriority)),
        actions: payment_actions(&prepared.actions),
    }
}

fn choose_mana_color_input(choice: &ManaChoicePrompt) -> Result<ChooseColorInput> {
    match choice {
        ManaChoicePrompt::SingleColor { options } => Ok(ChooseColorInput {
            presentation: presentation("Choose a color"),
            valid_colors: options
                .iter()
                .copied()
                .map(mana_type_symbol)
                .map(str::to_string)
                .collect(),
            amount: 1,
            repeat_allowed: false,
        }),
        ManaChoicePrompt::AnyCombination { count, options } => Ok(ChooseColorInput {
            presentation: presentation("Choose colors"),
            valid_colors: options
                .iter()
                .copied()
                .map(mana_type_symbol)
                .map(str::to_string)
                .collect(),
            amount: *count as u32,
            repeat_allowed: true,
        }),
        ManaChoicePrompt::Combination { .. } => Err(AdapterError::UnsupportedPrompt {
            waiting_for_type: "ChooseManaColor",
            code: "local.mana-combination-choice-unsupported",
        }),
    }
}

/// Does this output's prompt **family** answer the currently open prompt?
///
/// Under v1 this function also had to disambiguate a bare `act` between the
/// priority and mana-payment families by inspecting `waiting_for`. v2's
/// two-level [`PromptOutput`] carries the family in its tag, so that guess is
/// gone: this is now a straight family-to-`WaitingFor` correspondence check.
fn output_family_matches_waiting(
    output: &PromptOutput,
    state: &GameState,
    viewer: PlayerId,
) -> bool {
    let waiting_for = &state.waiting_for;
    match output {
        PromptOutput::ChooseAction(_) => matches!(waiting_for, WaitingFor::Priority { .. }),
        PromptOutput::PayManaCost(_) => matches!(waiting_for, WaitingFor::ManaPayment { .. }),
        // A declare-point response (keep/mulligan or use Serum Powder) is only
        // legal while the viewer's own entry is in the `Declare` phase.
        PromptOutput::Mulligan(_) => match waiting_for {
            WaitingFor::MulliganDecision { pending, .. } => {
                pending_entry_for_viewer(state, viewer, pending)
                    .is_ok_and(|entry| matches!(entry.phase, MulliganDecisionPhase::Declare))
            }
            _ => false,
        },
        // A bottom-cards selection is legal while the viewer's own entry is in
        // the `BottomCards` sub-phase, or during the unrelated
        // `OpeningHandBottomCards` phase.
        PromptOutput::MulliganPutBack(_) => match waiting_for {
            WaitingFor::MulliganDecision { pending, .. } => {
                pending_entry_for_viewer(state, viewer, pending).is_ok_and(|entry| {
                    matches!(entry.phase, MulliganDecisionPhase::BottomCards { .. })
                })
            }
            WaitingFor::OpeningHandBottomCards { pending, .. } => {
                pending_bottom_entry_for_viewer(state, viewer, pending).is_ok()
            }
            _ => false,
        },
        PromptOutput::ChooseAttackers(_) => {
            matches!(waiting_for, WaitingFor::DeclareAttackers { .. })
        }
        PromptOutput::ChooseBlockers(_) => {
            matches!(waiting_for, WaitingFor::DeclareBlockers { .. })
        }
        PromptOutput::ChooseBoardTargets(_) => matches!(
            waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        // Like `ChooseFromSelection`, reachable both bespoke (X, CR 107.3) and
        // generically, so the open-prompt check carries it rather than a list.
        PromptOutput::ChooseNumber(_) => {
            matches!(waiting_for, WaitingFor::ChooseXValue { .. })
                || open_prompt_is_generic_number(state, viewer)
        }
        // The one family with no fixed `WaitingFor` list, because it is now
        // reachable two ways: the two bespoke modal arms, and the generic
        // projection path that serves any state the engine renders as a finite
        // choice list. Enumerating the latter would reintroduce exactly the
        // per-variant bookkeeping the projection removes, and would rot the
        // moment the engine reclassifies a state.
        //
        // So ask the real question — would the prompt currently open be a
        // `ChooseFromSelection`? — by consulting the builder itself. It cannot
        // drift from the builder because it *is* the builder. Checking the
        // projection alone would be wrong: `WaitingFor::Priority` also projects
        // a finite list, and would then accept a `chooseFromSelection` answer to
        // a `chooseAction` prompt.
        PromptOutput::ChooseFromSelection(_) => open_prompt_is_generic_selection(state, viewer),
        PromptOutput::ChooseColor(_) => matches!(waiting_for, WaitingFor::ChooseManaColor { .. }),
        PromptOutput::ChooseCombatDamageAssignment(_) => {
            matches!(waiting_for, WaitingFor::AssignCombatDamage { .. })
        }
        // CR 701.42a: surveil shares scry's partition shape, differing only in
        // the second destination carried by `ScryInput::zones`.
        PromptOutput::Scry(_) => matches!(
            waiting_for,
            WaitingFor::ScryChoice { .. } | WaitingFor::SurveilChoice { .. }
        ),
        PromptOutput::ChooseBoolean(_) => matches!(
            waiting_for,
            WaitingFor::OptionalEffectChoice { .. }
                | WaitingFor::OpponentMayChoice { .. }
                | WaitingFor::MiracleReveal { .. }
                | WaitingFor::ExertChoice { .. }
                | WaitingFor::UnlessPayment { .. }
        ),
        // Reachable both bespoke (discard, CR 701.9b) and generically, so the
        // bespoke match stays primary and the open-prompt check carries the rest
        // rather than a list that would rot as the engine reclassifies states.
        PromptOutput::ChooseCards(_) => {
            matches!(waiting_for, WaitingFor::DiscardChoice { .. })
                || open_prompt_is_generic_cards(state, viewer)
        }
        PromptOutput::Reorder(_) => matches!(waiting_for, WaitingFor::OrderTriggers { .. }),
        // Modeled on the wire, but this adapter emits no prompt that accepts
        // them, so no `WaitingFor` can legally receive one.
        PromptOutput::ChooseDamageAssignmentOrder(_)
        | PromptOutput::RevealCards(_)
        | PromptOutput::DiceRolled(_) => false,
    }
}

/// Rebuild the prompt currently open for this viewer, to ask which family it is.
///
/// The gate for every family the generic path can emit. Those families have no
/// fixed `WaitingFor` list — the projection decides — and enumerating one would
/// reintroduce exactly the per-variant bookkeeping the projection removes.
/// Rebuilding cannot drift from the builder because it *is* the builder. One
/// extra prompt build per answer is proportionate: this runs once per decision.
///
/// The lookup yields *empty* card text on purpose. Which family a state builds
/// into never depends on the text, only on whether text can be had at all — so
/// supplying an empty string keeps every state answerable here while leaking
/// nothing. Yielding `None` instead would make `MissingCardText` swallow the
/// card-bearing families (`ChooseCards`, mulligan, scry, reorder) into "no
/// family", silently gating a legal answer to a card prompt as illegal.
fn open_prompt(state: &GameState, viewer: PlayerId) -> Option<PromptInput> {
    let prepared = prepare_snapshot(state, viewer, "").ok()?;
    build_prompt_input(
        &prepared,
        &(|_: &GameObject| -> Option<String> { Some(String::new()) }),
    )
    .ok()
}

fn open_prompt_is_generic_selection(state: &GameState, viewer: PlayerId) -> bool {
    matches!(
        open_prompt(state, viewer),
        Some(PromptInput::ChooseFromSelection(_))
    )
}

fn open_prompt_is_generic_cards(state: &GameState, viewer: PlayerId) -> bool {
    matches!(
        open_prompt(state, viewer),
        Some(PromptInput::ChooseCards(_))
    )
}

fn open_prompt_is_generic_number(state: &GameState, viewer: PlayerId) -> bool {
    matches!(
        open_prompt(state, viewer),
        Some(PromptInput::ChooseNumber(_))
    )
}

/// The output's family tag, for diagnostics.
fn output_family(output: &PromptOutput) -> &'static str {
    match output {
        PromptOutput::Mulligan(_) => "mulligan",
        PromptOutput::MulliganPutBack(_) => "mulliganPutBack",
        PromptOutput::ChooseAction(_) => "chooseAction",
        PromptOutput::ChooseAttackers(_) => "chooseAttackers",
        PromptOutput::ChooseBlockers(_) => "chooseBlockers",
        PromptOutput::ChooseBoardTargets(_) => "chooseBoardTargets",
        PromptOutput::ChooseBoolean(_) => "chooseBoolean",
        PromptOutput::ChooseFromSelection(_) => "chooseFromSelection",
        PromptOutput::RevealCards(_) => "revealCards",
        PromptOutput::Scry(_) => "scry",
        PromptOutput::ChooseColor(_) => "chooseColor",
        PromptOutput::ChooseNumber(_) => "chooseNumber",
        PromptOutput::ChooseDamageAssignmentOrder(_) => "chooseDamageAssignmentOrder",
        PromptOutput::ChooseCombatDamageAssignment(_) => "chooseCombatDamageAssignment",
        PromptOutput::PayManaCost(_) => "payManaCost",
        PromptOutput::ChooseCards(_) => "chooseCards",
        PromptOutput::Reorder(_) => "reorder",
        PromptOutput::DiceRolled(_) => "diceRolled",
    }
}

fn translate_choose_action_output(
    output: ChooseActionOutput,
    context: &PromptContext,
    state: &GameState,
) -> Result<GameAction> {
    match output {
        ChooseActionOutput::Pass {
            until: None,
            exhaust_stack: false,
        } => Ok(GameAction::PassPriority),
        // Both modifiers ask the engine to keep passing past this priority
        // window; neither maps onto a single `GameAction`.
        ChooseActionOutput::Pass { until: Some(_), .. } => {
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.pass-until-unsupported",
            })
        }
        ChooseActionOutput::Pass {
            exhaust_stack: true,
            ..
        } => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.exhaust-stack-pass-unsupported",
        }),
        ChooseActionOutput::RestoreSnapshot { .. } => {
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.room-relay-not-implemented",
            })
        }
        ChooseActionOutput::Act { action_id } => {
            advertised_action_by_id(context, state, &action_id)
        }
    }
}

fn translate_pay_mana_output(
    output: PayManaCostOutput,
    context: &PromptContext,
) -> Result<GameAction> {
    match output {
        // Resolves through the SAME `action-{index}` id space the payment
        // actions were advertised from — see `advertised_payment_action_by_id`.
        PayManaCostOutput::Act { action_id } => {
            advertised_payment_action_by_id(context, &action_id)
        }
        PayManaCostOutput::Pay { auto: true } => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.auto-pay-unsupported",
        }),
        PayManaCostOutput::Pay { auto: false } => prompt_level_action(
            context,
            |action| matches!(action, GameAction::PassPriority),
            "upstream.mana-pool-entries-missing",
        ),
        PayManaCostOutput::Cancel => prompt_level_action(
            context,
            |action| matches!(action, GameAction::CancelCast),
            "local.cancel-mana-payment-unavailable",
        ),
    }
}

fn prompt_level_action(
    context: &PromptContext,
    predicate: impl Fn(&GameAction) -> bool,
    code: &'static str,
) -> Result<GameAction> {
    context
        .action_table
        .iter()
        .find(|entry| predicate(&entry.action))
        .map(|entry| entry.action.clone())
        .ok_or(AdapterError::UnsupportedProtocolFeature { code })
}

fn translate_color_decision(
    waiting_for: &WaitingFor,
    chosen_colors: BTreeMap<String, u32>,
) -> Result<GameAction> {
    if !matches!(waiting_for, WaitingFor::ChooseManaColor { .. }) {
        return Err(AdapterError::IllegalResponseForPrompt {
            response_kind: "colorDecision",
        });
    }

    let payment = chosen_colors
        .iter()
        .flat_map(|(color, count)| {
            std::iter::repeat_n(color.as_str(), *count as usize).map(mana_type_from_symbol)
        })
        .collect::<Result<Vec<_>>>()?;

    if payment.len() == 1 {
        Ok(GameAction::ChooseManaColor {
            choice: ManaChoice::SingleColor(payment[0]),
            count: 1,
        })
    } else {
        Ok(GameAction::ChooseManaColor {
            choice: ManaChoice::Combination(payment),
            count: 1,
        })
    }
}

fn target_ref_from_dto(target: &TargetRefDto) -> Result<TargetRef> {
    match target.kind {
        TargetKindDto::Player => parse_player_id(&target.id).map(TargetRef::Player),
        TargetKindDto::Card => parse_object_id(&target.id).map(TargetRef::Object),
        TargetKindDto::Spell => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.stack-target-ref-unsupported",
        }),
    }
}

fn parse_object_ids(card_ids: &[String]) -> Result<Vec<ObjectId>> {
    card_ids.iter().map(|id| parse_object_id(id)).collect()
}

fn pending_entry_for_viewer<'a>(
    state: &GameState,
    viewer: PlayerId,
    pending: &'a [engine::types::game_state::MulliganDecisionEntry],
) -> Result<&'a engine::types::game_state::MulliganDecisionEntry> {
    pending
        .iter()
        .find(|entry| turn_control::authorized_submitter_for_player(state, entry.player) == viewer)
        .ok_or(AdapterError::NoAuthorizedPrompt { viewer })
}

fn pending_bottom_entry_for_viewer<'a>(
    state: &GameState,
    viewer: PlayerId,
    pending: &'a [engine::types::game_state::MulliganBottomEntry],
) -> Result<&'a engine::types::game_state::MulliganBottomEntry> {
    pending
        .iter()
        .find(|entry| turn_control::authorized_submitter_for_player(state, entry.player) == viewer)
        .ok_or(AdapterError::NoAuthorizedPrompt { viewer })
}

/// v2 dropped `PromptPresentation::source_card_id` — the source now travels as
/// `AgentPrompt::source_card`, a full `CardDto`.
fn presentation(title: impl Into<String>) -> PromptPresentation {
    PromptPresentation {
        title: title.into(),
        description: None,
        text: None,
        targets: Vec::new(),
    }
}

/// A modal choice offered exactly once and weighted equally — the engine's
/// `ModalChoice` carries no per-mode weight or repetition allowance.
fn selection_option(label: String) -> SelectionOption {
    SelectionOption {
        label,
        weight: 1,
        can_repeat: false,
    }
}

fn attack_target_ref_id(target: &AttackTarget) -> String {
    match target {
        AttackTarget::Player(player) => encode_player_id(*player),
        AttackTarget::Planeswalker(id) | AttackTarget::Battle(id) => encode_object_id(*id),
    }
}

fn attack_target_dto(target: &AttackTarget) -> AttackTargetDto {
    match target {
        AttackTarget::Player(player) => AttackTargetDto {
            id: encode_player_id(*player),
            label: format!("Player {}", player.0),
            kind: AttackTargetKind::Player,
        },
        AttackTarget::Planeswalker(id) => AttackTargetDto {
            id: encode_object_id(*id),
            label: encode_object_id(*id),
            kind: AttackTargetKind::Planeswalker,
        },
        AttackTarget::Battle(id) => AttackTargetDto {
            id: encode_object_id(*id),
            label: encode_object_id(*id),
            kind: AttackTargetKind::Battle,
        },
    }
}

fn parse_attack_target_id(value: &str) -> Result<AttackTarget> {
    if value.starts_with("player-") {
        parse_player_id(value).map(AttackTarget::Player)
    } else {
        parse_object_id(value).map(AttackTarget::Planeswalker)
    }
}

/// CR 106.4: the viewer's floating mana, one entry per color actually held.
fn mana_pool_counts(units: &[engine::types::mana::ManaUnit]) -> BTreeMap<ManaColorDto, u32> {
    let mut counts = BTreeMap::new();
    for unit in units {
        *counts.entry(mana_color_dto(unit.color)).or_insert(0) += 1;
    }
    counts
}

fn mana_color_dto(mana_type: ManaType) -> ManaColorDto {
    match mana_type {
        ManaType::White => ManaColorDto::White,
        ManaType::Blue => ManaColorDto::Blue,
        ManaType::Black => ManaColorDto::Black,
        ManaType::Red => ManaColorDto::Red,
        ManaType::Green => ManaColorDto::Green,
        ManaType::Colorless => ManaColorDto::Colorless,
    }
}

fn colors_string(colors: &[EngineManaColor]) -> String {
    colors
        .iter()
        .map(|color| mana_color_symbol(*color))
        .collect()
}

fn mana_color_symbol(color: EngineManaColor) -> &'static str {
    match color {
        EngineManaColor::White => "W",
        EngineManaColor::Blue => "U",
        EngineManaColor::Black => "B",
        EngineManaColor::Red => "R",
        EngineManaColor::Green => "G",
    }
}

fn mana_type_symbol(mana_type: ManaType) -> &'static str {
    match mana_type {
        ManaType::White => "W",
        ManaType::Blue => "U",
        ManaType::Black => "B",
        ManaType::Red => "R",
        ManaType::Green => "G",
        ManaType::Colorless => "C",
    }
}

fn mana_type_from_symbol(symbol: &str) -> Result<ManaType> {
    match symbol {
        "W" => Ok(ManaType::White),
        "U" => Ok(ManaType::Blue),
        "B" => Ok(ManaType::Black),
        "R" => Ok(ManaType::Red),
        "G" => Ok(ManaType::Green),
        "C" => Ok(ManaType::Colorless),
        _ => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.invalid-color-decision",
        }),
    }
}

fn mana_cost_string(cost: &ManaCost) -> String {
    match cost {
        ManaCost::NoCost => String::new(),
        ManaCost::SelfManaCost => "its mana cost".to_string(),
        ManaCost::SelfManaValue => "its mana value".to_string(),
        ManaCost::SelfManaCostReduced { reduction } => {
            format!("its mana cost reduced by {{{reduction}}}")
        }
        ManaCost::Cost { shards, generic } => {
            let mut out = String::new();
            if *generic > 0 {
                out.push_str(&format!("{{{generic}}}"));
            }
            for shard in shards {
                out.push_str(&format!("{{{}}}", mana_shard_symbol(shard)));
            }
            out
        }
    }
}

fn mana_shard_symbol(shard: &ManaCostShard) -> &'static str {
    match shard {
        ManaCostShard::White => "W",
        ManaCostShard::Blue => "U",
        ManaCostShard::Black => "B",
        ManaCostShard::Red => "R",
        ManaCostShard::Green => "G",
        ManaCostShard::Colorless => "C",
        ManaCostShard::Snow => "S",
        ManaCostShard::X => "X",
        ManaCostShard::TwoOrMoreColorSource => "Z",
        ManaCostShard::WhiteBlue => "W/U",
        ManaCostShard::WhiteBlack => "W/B",
        ManaCostShard::BlueBlack => "U/B",
        ManaCostShard::BlueRed => "U/R",
        ManaCostShard::BlackRed => "B/R",
        ManaCostShard::BlackGreen => "B/G",
        ManaCostShard::RedWhite => "R/W",
        ManaCostShard::RedGreen => "R/G",
        ManaCostShard::GreenWhite => "G/W",
        ManaCostShard::GreenBlue => "G/U",
        ManaCostShard::TwoWhite => "2/W",
        ManaCostShard::TwoBlue => "2/U",
        ManaCostShard::TwoBlack => "2/B",
        ManaCostShard::TwoRed => "2/R",
        ManaCostShard::TwoGreen => "2/G",
        ManaCostShard::PhyrexianWhite => "W/P",
        ManaCostShard::PhyrexianBlue => "U/P",
        ManaCostShard::PhyrexianBlack => "B/P",
        ManaCostShard::PhyrexianRed => "R/P",
        ManaCostShard::PhyrexianGreen => "G/P",
        ManaCostShard::PhyrexianWhiteBlue => "W/U/P",
        ManaCostShard::PhyrexianWhiteBlack => "W/B/P",
        ManaCostShard::PhyrexianBlueBlack => "U/B/P",
        ManaCostShard::PhyrexianBlueRed => "U/R/P",
        ManaCostShard::PhyrexianBlackRed => "B/R/P",
        ManaCostShard::PhyrexianBlackGreen => "B/G/P",
        ManaCostShard::PhyrexianRedWhite => "R/W/P",
        ManaCostShard::PhyrexianRedGreen => "R/G/P",
        ManaCostShard::PhyrexianGreenWhite => "G/W/P",
        ManaCostShard::PhyrexianGreenBlue => "G/U/P",
        ManaCostShard::ColorlessWhite => "C/W",
        ManaCostShard::ColorlessBlue => "C/U",
        ManaCostShard::ColorlessBlack => "C/B",
        ManaCostShard::ColorlessRed => "C/R",
        ManaCostShard::ColorlessGreen => "C/G",
    }
}

/// One message on the relay, in either direction.
///
/// The payload key differs per kind and is **not** derivable from the kind name
/// (`display` carries `event`, `log` and `snapshot` carry `entry`), so the
/// mapping is spelled out variant by variant.
///
/// A `state` payload is a [`StateUpdate`] wrapper, not a bare [`GameViewDto`].
///
/// Addressing: `for_player` is *optional* on `state` (absent = the public view)
/// but required on `prompt` and `error`. That asymmetry is what makes the
/// audience rule work — a client applies state addressed to its own seat,
/// ignores state addressed to another, and once it has received any state
/// addressed to it, ignores public views for the rest of the game.
///
/// `for_player` identifies the engine seat for dispatch and replay; it is not
/// itself a transport privacy boundary.
///
/// An unknown `kind` MUST be ignored rather than treated as an error: a
/// deserialization failure here means "not a kind we handle", not "malformed
/// stream". `roomRelay` is deliberately not modeled — its payload is
/// implementation-defined, so there is no shape to agree on.
// One envelope is moved at a time rather than held in bulk, so evening out the
// variants would only add indirection to the payload the relay is about to
// serialize anyway. Upstream makes the same call on its `AgentMessage`.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum RelayMessage {
    State {
        state: StateUpdate,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        for_player: Option<String>,
    },
    Display {
        event: DisplayEvent,
    },
    Prompt {
        prompt: AgentPrompt,
        for_player: String,
    },
    Error {
        error: ProtocolError,
        for_player: String,
    },
    Response {
        prompt_id: u32,
        action: PromptOutput,
        from_player: String,
    },
    Directive {
        directive: DirectiveInput,
        from_player: String,
    },
    /// `GameLogEntry` lives in upstream's engine-coupled
    /// `manabrew-agent-interface`, not in the wire-protocol crate, so its shape
    /// is not verifiable here and is passed through opaquely rather than
    /// invented.
    Log {
        entry: serde_json::Value,
        from_player: String,
    },
    /// `GameSnapshot`, same provenance and same treatment as `Log`.
    Snapshot {
        entry: serde_json::Value,
        from_player: String,
    },
    Fatal {
        message: String,
    },
}

fn attach_target_id(target: &AttachTarget) -> Option<String> {
    match target {
        AttachTarget::Object(id) => Some(encode_object_id(*id)),
        AttachTarget::Player(id) => Some(encode_player_id(*id)),
    }
}

fn modal_options(modal: &engine::types::ability::ModalChoice) -> Vec<String> {
    (0..modal.mode_count)
        .map(|index| {
            modal
                .mode_descriptions
                .get(index)
                .cloned()
                .unwrap_or_else(|| format!("Mode {}", index + 1))
        })
        .collect()
}

/// The object a prompt originates from, if any.
///
/// Resolved against **raw** state in `prepare_snapshot`, before viewer
/// filtering, so the source survives even when it sits outside the recipient's
/// visible state — which is the whole point of v2's `sourceCard`.
fn source_object_id(waiting_for: &WaitingFor) -> Option<ObjectId> {
    match waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. }
        | WaitingFor::ModeChoice { pending_cast, .. }
        | WaitingFor::ChooseXValue { pending_cast, .. }
        | WaitingFor::CostTypeChoice { pending_cast, .. } => Some(pending_cast.object_id),
        WaitingFor::TriggerTargetSelection { source_id, .. } => *source_id,
        WaitingFor::OptionalEffectChoice { source_id, .. }
        | WaitingFor::OpponentMayChoice { source_id, .. } => Some(*source_id),
        _ => None,
    }
}

fn waiting_for_type(waiting_for: &WaitingFor) -> &'static str {
    match waiting_for {
        WaitingFor::Priority { .. } => "Priority",
        WaitingFor::MulliganDecision { .. } => "MulliganDecision",
        WaitingFor::OpeningHandBottomCards { .. } => "OpeningHandBottomCards",
        WaitingFor::ManaPayment { .. } => "ManaPayment",
        WaitingFor::ChooseXValue { .. } => "ChooseXValue",
        WaitingFor::TargetSelection { .. } => "TargetSelection",
        WaitingFor::DeclareAttackers { .. } => "DeclareAttackers",
        WaitingFor::DeclareBlockers { .. } => "DeclareBlockers",
        WaitingFor::ScryChoice { .. } => "ScryChoice",
        WaitingFor::DigChoice { .. } => "DigChoice",
        WaitingFor::SurveilChoice { .. } => "SurveilChoice",
        WaitingFor::DiscardChoice { .. } => "DiscardChoice",
        WaitingFor::TriggerTargetSelection { .. } => "TriggerTargetSelection",
        WaitingFor::ModeChoice { .. } => "ModeChoice",
        WaitingFor::AbilityModeChoice { .. } => "AbilityModeChoice",
        WaitingFor::OptionalEffectChoice { .. } => "OptionalEffectChoice",
        WaitingFor::OpponentMayChoice { .. } => "OpponentMayChoice",
        WaitingFor::UnlessPayment { .. } => "UnlessPayment",
        WaitingFor::UnlessPaymentChooseCost { .. } => "UnlessPaymentChooseCost",
        WaitingFor::NamedChoice { .. } => "NamedChoice",
        WaitingFor::CostTypeChoice { .. } => "CostTypeChoice",
        WaitingFor::AssignCombatDamage { .. } => "AssignCombatDamage",
        WaitingFor::AssignBlockerDamage { .. } => "AssignBlockerDamage",
        WaitingFor::CombatTaxPayment { .. } => "CombatTaxPayment",
        WaitingFor::ChooseManaColor { .. } => "ChooseManaColor",
        WaitingFor::PayManaAbilityMana { .. } => "PayManaAbilityMana",
        WaitingFor::GameOver { .. } => "GameOver",
        _ => "Unsupported",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    use engine::game::interaction::bind_interaction_authority;
    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use engine::types::counter::CounterType;
    use engine::types::game_state::{
        MulliganDecisionEntry, MulliganDecisionPhase, OutsideGameChoiceEntry,
        OutsideGameChoiceSource, PayableResource, PendingCast, PendingMulliganAction,
        TargetSelectionProgress, TargetSelectionSlot,
    };
    use engine::types::identifiers::CardId;
    use engine::types::interaction::InteractionSessionId;
    use pretty_assertions::assert_eq;

    fn lookup(_: &GameObject) -> Option<String> {
        Some("Test oracle text.".to_string())
    }

    fn dummy_ability() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::unimplemented("Dummy", "dummy effect"),
            vec![],
            ObjectId(1),
            PlayerId(0),
        )
    }

    fn dummy_pending_cast() -> Box<PendingCast> {
        Box::new(PendingCast::new(
            ObjectId(1),
            CardId(1),
            dummy_ability(),
            ManaCost::NoCost,
        ))
    }

    /// A snapshot with a real (non-reserved) prompt id, built the way production
    /// does — through `prepare_snapshot_with_prompt_id`, so `source_card_object`
    /// is captured from raw state.
    fn prepared_for(waiting_for: WaitingFor) -> PreparedManabrewSnapshot {
        let mut state = GameState::new_two_player(7);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Prompt Source".to_string(),
            Zone::Hand,
        );
        state.waiting_for = waiting_for;
        prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap()
    }

    /// A snapshot with no objects, for conversions that read nothing from it.
    ///
    /// Named for what it asserts: any arm that needs a real board must build
    /// one, so a test using this is declaring that its conversion is
    /// state-independent.
    fn empty_state() -> GameState {
        GameState::new_two_player(7)
    }

    fn context_with(actions: Vec<GameAction>) -> PromptContext {
        PromptContext {
            prompt_id: 7,
            deciding_player: PlayerId(0),
            action_table: action_table(&actions),
        }
    }

    // ---------------------------------------------------------------- ids ---

    #[test]
    fn id_codecs_roundtrip() {
        assert_eq!(encode_object_id(ObjectId(42)), "card-42");
        assert_eq!(encode_stack_id(ObjectId(42)), "stack-42");
        assert_eq!(parse_object_id("card-42").unwrap(), ObjectId(42));
        assert_eq!(parse_stack_id("stack-42").unwrap(), ObjectId(42));
        assert!(matches!(
            parse_object_id("player-42"),
            Err(AdapterError::MalformedId { .. })
        ));
    }

    #[test]
    fn player_and_stack_id_codecs_reject_wrong_prefixes() {
        assert_eq!(encode_player_id(PlayerId(3)), "player-3");
        assert_eq!(parse_player_id("player-3").unwrap(), PlayerId(3));

        match parse_player_id("card-3") {
            Err(AdapterError::MalformedId {
                expected_prefix,
                value,
            }) => {
                assert_eq!(expected_prefix, "player-");
                assert_eq!(value, "card-3");
            }
            other => panic!("expected MalformedId, got {other:?}"),
        }

        match parse_stack_id("card-3") {
            Err(AdapterError::MalformedId {
                expected_prefix, ..
            }) => assert_eq!(expected_prefix, "stack-"),
            other => panic!("expected MalformedId, got {other:?}"),
        }

        assert!(matches!(
            parse_object_id("card-abc"),
            Err(AdapterError::MalformedId {
                expected_prefix: "card-",
                ..
            })
        ));
    }

    #[test]
    fn protocol_version_is_the_pinned_crate_major() {
        assert_eq!(PROTOCOL_VERSION, 2);
    }

    // -------------------------------------------------------------- state ---

    #[test]
    fn state_update_uses_zone_buckets_and_day_time() {
        let mut state = GameState::new_two_player(7);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let json = serde_json::to_value(build_state_update(&prepared, &lookup).unwrap()).unwrap();
        let view = &json["gameView"];

        // v2 replaced the flat `battlefield` list with `(zone, owner)` buckets.
        assert!(view.get("battlefield").is_none());
        assert!(view.get("concededPlayerIds").is_none());
        assert_eq!(view["dayTime"], "neither");

        let battlefield = view["zones"]
            .as_array()
            .unwrap()
            .iter()
            .find(|zone| zone["zone"] == "battlefield" && zone["ownerId"] == "player-0")
            .expect("player 0 battlefield bucket");
        assert_eq!(battlefield["cards"][0]["identity"]["name"], "Test Creature");
        assert_eq!(battlefield["cards"][0]["visibility"], "visible");
        assert_eq!(battlefield["count"], 1);
    }

    /// Player counters moved from five flat `*Counters` fields into one
    /// `counters` map keyed by `PlayerCounterKind`, and only non-zero entries
    /// are carried.
    #[test]
    fn player_counters_use_the_typed_counter_map() {
        let mut state = GameState::new_two_player(7);
        state.players[0].add_player_counters(&PlayerCounterKind::Rad, 2);
        state.players[0].add_player_counters(&PlayerCounterKind::Experience, 3);
        state.players[0].add_player_counters(&PlayerCounterKind::Ticket, 4);

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let json = serde_json::to_value(build_state_update(&prepared, &lookup).unwrap()).unwrap();
        let player = &json["gameView"]["players"][0];

        assert!(player.get("radiationCounters").is_none());
        assert_eq!(player["counters"]["radiation"], 2);
        assert_eq!(player["counters"]["experience"], 3);
        assert_eq!(player["counters"]["ticket"], 4);
        assert!(
            player["counters"].get("poison").is_none(),
            "zero counters are omitted rather than sent as 0"
        );
        assert_eq!(player["status"], "playing");
    }

    /// The engine records only THAT a player is out, so an eliminated player is
    /// `lost`. `conceded` must never be emitted — doing so would assert a reason
    /// the engine never stored.
    #[test]
    fn eliminated_player_is_lost_never_conceded() {
        let mut state = GameState::new_two_player(7);
        state.players[1].is_eliminated = true;

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let json = serde_json::to_value(build_state_update(&prepared, &lookup).unwrap()).unwrap();

        assert_eq!(json["gameView"]["players"][0]["status"], "playing");
        assert_eq!(json["gameView"]["players"][1]["status"], "lost");
    }

    /// Step 0a's 12→13 table, on the wire. Every combat step and the end step
    /// was previously emitted as a snake_case string that is not a valid
    /// `StepKind` at all.
    #[test]
    fn every_phase_maps_to_its_step_kind() {
        let cases = [
            (Phase::Untap, "untap"),
            (Phase::Upkeep, "upkeep"),
            (Phase::Draw, "draw"),
            (Phase::PreCombatMain, "main1"),
            (Phase::BeginCombat, "combatBegin"),
            (Phase::DeclareAttackers, "combatDeclareAttackers"),
            (Phase::DeclareBlockers, "combatDeclareBlockers"),
            (Phase::CombatDamage, "combatDamage"),
            (Phase::EndCombat, "combatEnd"),
            (Phase::PostCombatMain, "main2"),
            (Phase::End, "endOfTurn"),
            (Phase::Cleanup, "cleanup"),
        ];

        for (phase, expected) in cases {
            let mut state = GameState::new_two_player(7);
            state.phase = phase;
            let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
            let json =
                serde_json::to_value(build_state_update(&prepared, &lookup).unwrap()).unwrap();
            assert_eq!(
                json["gameView"]["step"], expected,
                "wrong StepKind for {phase:?}"
            );

            // The same six corrected values must also round-trip through
            // `PassUntil.phase`, which is the easy-to-miss second StepKind site.
            let until = PassUntil {
                player_id: "player-0".to_string(),
                phase: phase_step(phase),
            };
            let until_json = serde_json::to_value(&until).unwrap();
            assert_eq!(until_json["phase"], expected);
            assert_eq!(
                serde_json::from_value::<PassUntil>(until_json).unwrap(),
                until
            );
        }
    }

    /// `combatFirstStrikeDamage` is the unmatched thirteenth `StepKind`: it is a
    /// valid wire value but no engine `Phase` produces it.
    #[test]
    fn first_strike_damage_step_is_never_produced() {
        let produced: HashSet<_> = [
            Phase::Untap,
            Phase::Upkeep,
            Phase::Draw,
            Phase::PreCombatMain,
            Phase::BeginCombat,
            Phase::DeclareAttackers,
            Phase::DeclareBlockers,
            Phase::CombatDamage,
            Phase::EndCombat,
            Phase::PostCombatMain,
            Phase::End,
            Phase::Cleanup,
        ]
        .into_iter()
        .map(phase_step)
        .collect();

        assert_eq!(produced.len(), 12, "all twelve phases map distinctly");
        assert!(!produced.contains(&StepKind::CombatFirstStrikeDamage));
        assert_eq!(
            serde_json::to_value(StepKind::CombatFirstStrikeDamage).unwrap(),
            "combatFirstStrikeDamage",
            "it is still a legal wire value we must be able to parse"
        );
    }

    // --------------------------------------------------- zone visibility ---

    /// Rule 1: a hand is visible to its owner, and to every other seat it is a
    /// truthful `count` with no entries.
    #[test]
    fn hand_is_visible_to_owner_and_counted_for_opponents() {
        let mut state = GameState::new_two_player(7);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Opponent Card".to_string(),
            Zone::Hand,
        );

        let owner_view = zones_of(&state, PlayerId(1));
        let owner_hand = find_zone(&owner_view, "hand", "player-1");
        assert_eq!(owner_hand["cards"].as_array().unwrap().len(), 1);
        assert_eq!(owner_hand["count"], 1);

        let opponent_view = zones_of(&state, PlayerId(0));
        let opponent_hand = find_zone(&opponent_view, "hand", "player-1");
        assert!(
            opponent_hand["cards"].as_array().unwrap().is_empty(),
            "an opponent learns nothing about which cards are in the hand"
        );
        assert_eq!(
            opponent_hand["count"], 1,
            "but the count stays truthful — count may exceed cards.len()"
        );
    }

    /// Rule 2: a library is a count alone. (The top card becomes a visible entry
    /// only under a look-at-top permission, which the engine grants by leaving
    /// that one object unconcealed.)
    #[test]
    fn library_is_count_only_without_a_look_permission() {
        let mut state = GameState::new_two_player(7);
        for _ in 0..3 {
            // `create_object` already files the object into its zone.
            create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Deck Card".to_string(),
                Zone::Library,
            );
        }

        let view = zones_of(&state, PlayerId(0));
        let library = find_zone(&view, "library", "player-0");
        assert!(library["cards"].as_array().unwrap().is_empty());
        assert_eq!(library["count"], 3);
    }

    /// Rule 2's other half (CR 701.20e): under a "you may look at the top card
    /// of your library" permission the top card becomes a visible entry, while
    /// the rest of the library stays a bare count.
    #[test]
    fn library_top_card_is_visible_under_a_look_permission() {
        let mut state = GameState::new_two_player(7);
        let top = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Top Card".to_string(),
            Zone::Library,
        );
        for _ in 0..2 {
            create_object(
                &mut state,
                CardId(1),
                PlayerId(0),
                "Buried Card".to_string(),
                Zone::Library,
            );
        }
        state.players[0].can_look_at_top_of_library = true;

        let own_view = zones_of(&state, PlayerId(0));
        let library = find_zone(&own_view, "library", "player-0");
        assert_eq!(
            library["cards"].as_array().unwrap().len(),
            1,
            "only the top card is exposed"
        );
        assert_eq!(library["cards"][0]["visibility"], "visible");
        assert_eq!(library["cards"][0]["id"], encode_object_id(top));
        assert_eq!(library["cards"][0]["identity"]["name"], "Top Card");
        assert_eq!(library["count"], 3, "the count still covers the whole zone");

        // The permission is the viewer's own; an opponent learns nothing.
        let opponent_view = zones_of(&state, PlayerId(1));
        let opponent = find_zone(&opponent_view, "library", "player-0");
        assert!(opponent["cards"].as_array().unwrap().is_empty());
        assert_eq!(opponent["count"], 3);
    }

    /// Rule 3: a face-down exiled card is present but anonymous — a `hidden`
    /// entry, so the client can render a face-down card without learning it.
    #[test]
    fn face_down_exile_is_a_hidden_entry() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Foretold Card".to_string(),
            Zone::Exile,
        );
        state.objects.get_mut(&id).unwrap().face_down = true;

        let view = zones_of(&state, PlayerId(0));
        let exile = find_zone(&view, "exile", "player-0");
        assert_eq!(exile["cards"][0]["visibility"], "hidden");
        assert_eq!(exile["cards"][0]["id"], encode_object_id(id));
        assert!(
            exile["cards"][0].get("card").is_none(),
            "a hidden entry carries an id and nothing else"
        );
        assert_eq!(exile["count"], 1);
    }

    /// Rule 4 — the trap. A face-down permanent is public even though its face
    /// is not, so it must be a REDACTED VISIBLE entry, never `hidden`: identity
    /// blanks out, but tapped/counters/damage survive.
    #[test]
    fn face_down_battlefield_permanent_is_redacted_visible_not_hidden() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Morph Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let object = state.objects.get_mut(&id).unwrap();
            object.face_down = true;
            object.tapped = true;
            object.damage_marked = 2;
            object.counters.insert(CounterType::Plus1Plus1, 3);
        }

        let view = zones_of(&state, PlayerId(0));
        let battlefield = find_zone(&view, "battlefield", "player-0");
        let entry = &battlefield["cards"][0];

        assert_eq!(
            entry["visibility"], "visible",
            "the permanent itself is public — CardView::Hidden is rule 3's shape, not rule 4's"
        );
        assert_eq!(
            entry["identity"]["name"], "",
            "an empty identity.name is what clients render as a card back"
        );
        assert_eq!(entry["text"], "");
        assert_eq!(entry["manaCost"], "");
        // Public board state survives redaction.
        assert_eq!(entry["tapped"], true);
        assert_eq!(entry["damage"], 2);
        assert_eq!(entry["counters"]["P1P1"], 3);
        assert_eq!(entry["isFaceDown"], true);
    }

    /// Counter keys are the engine's serialization form ("P1P1"), not the
    /// player-facing prose form ("+1/+1") that `display_phrase()` renders.
    #[test]
    fn counter_keys_use_the_serialization_form() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Holder".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Plus1Plus1, 2);

        let view = zones_of(&state, PlayerId(0));
        let card = &find_zone(&view, "battlefield", "player-0")["cards"][0];
        assert_eq!(card["counters"]["P1P1"], 2);
        assert!(card["counters"].get("+1/+1").is_none());
    }

    /// Battlefield buckets are keyed by CONTROLLER (CR 110.2), not owner, so a
    /// stolen permanent moves buckets.
    #[test]
    fn battlefield_is_bucketed_by_controller() {
        let mut state = GameState::new_two_player(7);
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Stolen Creature".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().controller = PlayerId(1);

        let view = zones_of(&state, PlayerId(0));
        assert!(
            find_zone(&view, "battlefield", "player-0")["cards"]
                .as_array()
                .unwrap()
                .is_empty(),
            "the owner's bucket is empty"
        );
        assert_eq!(
            find_zone(&view, "battlefield", "player-1")["cards"][0]["ownerId"],
            "player-0",
            "the controller's bucket holds it, and it still reports its true owner"
        );
    }

    fn zones_of(state: &GameState, viewer: PlayerId) -> serde_json::Value {
        let prepared = prepare_snapshot(state, viewer, "game-a").unwrap();
        let json = serde_json::to_value(build_state_update(&prepared, &lookup).unwrap()).unwrap();
        json["gameView"]["zones"].clone()
    }

    fn find_zone<'a>(
        zones: &'a serde_json::Value,
        zone: &str,
        owner: &str,
    ) -> &'a serde_json::Value {
        zones
            .as_array()
            .unwrap()
            .iter()
            .find(|entry| entry["zone"] == zone && entry["ownerId"] == owner)
            .unwrap_or_else(|| panic!("no {zone} bucket for {owner}"))
    }

    // ------------------------------------------------------------ prompts ---

    #[test]
    fn prompt_uses_prompt_id_deciding_player_and_input() {
        let prompt = build_prompt(
            &prepared_for(WaitingFor::Priority {
                player: PlayerId(0),
            }),
            &lookup,
        )
        .unwrap();
        let json = serde_json::to_value(prompt).unwrap();

        assert_eq!(json["promptId"], 42);
        assert_eq!(json["decidingPlayerId"], "player-0");
        assert_eq!(json["input"]["type"], "chooseAction");
        assert!(json.get("gameView").is_none());
    }

    #[test]
    fn unauthorized_viewer_does_not_receive_prompt() {
        let mut prepared = prepared_for(WaitingFor::Priority {
            player: PlayerId(0),
        });
        prepared.viewer = PlayerId(1);

        assert!(matches!(
            build_prompt(&prepared, &lookup),
            Err(AdapterError::NoAuthorizedPrompt {
                viewer: PlayerId(1)
            })
        ));
    }

    /// Prompt id 0 is reserved for engine-synthesized absent-player defaults, so
    /// a prompt carrying it could never be answered. `prepare_snapshot` uses it,
    /// which is exactly why that entry point is state-only.
    #[test]
    fn reserved_prompt_id_zero_is_never_emitted_as_a_real_prompt() {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        assert_eq!(prepared.prompt_id, RESERVED_ABSENT_PLAYER_PROMPT_ID);

        assert!(matches!(
            build_prompt(&prepared, &lookup),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.reserved-prompt-id-zero"
            })
        ));
    }

    /// v2 replaced `sourceCardId` with a full `sourceCard`, whose whole point is
    /// surviving when the source is outside the recipient's visible state — so
    /// it must be built from RAW state, not the viewer-filtered projection.
    ///
    /// Revert guard: building it from `prepared.state` would find the source
    /// concealed (it is an opponent's hand card here) and emit a blank identity.
    #[test]
    fn source_card_is_built_from_raw_not_viewer_filtered_state() {
        let mut state = GameState::new_two_player(7);
        // The source lives in the OPPONENT's hand, so the viewer's filtered
        // state conceals it entirely.
        let source = create_object(
            &mut state,
            CardId(1),
            PlayerId(1),
            "Hidden Trigger Source".to_string(),
            Zone::Hand,
        );
        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            trigger_controller: None,
            trigger_event: None,
            trigger_events: Vec::new(),
            target_slots: vec![TargetSelectionSlot {
                legal_targets: vec![TargetRef::Player(PlayerId(1))],
                optional: false,
                chooser: None,
            }],
            mode_labels: Vec::new(),
            target_constraints: Vec::new(),
            selection: TargetSelectionProgress::default(),
            source_id: Some(source),
            description: None,
        };

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 5).unwrap();
        let json = serde_json::to_value(build_prompt(&prepared, &lookup).unwrap()).unwrap();

        assert_eq!(
            json["sourceCard"]["identity"]["name"], "Hidden Trigger Source",
            "the source survives even though the viewer cannot see its zone"
        );
        assert_eq!(json["sourceCard"]["id"], encode_object_id(source));
        assert!(
            json.get("sourceCardId").is_none(),
            "the v1 flat id field is gone"
        );
    }

    #[test]
    fn target_selection_uses_board_target_refs() {
        let prompt = build_prompt(
            &prepared_for(WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: dummy_pending_cast(),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![
                        TargetRef::Object(ObjectId(1)),
                        TargetRef::Player(PlayerId(1)),
                    ],
                    optional: false,
                    chooser: None,
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            }),
            &lookup,
        )
        .unwrap();

        let json = serde_json::to_value(prompt).unwrap();
        assert_eq!(json["input"]["type"], "chooseBoardTargets");
        assert_eq!(json["input"]["candidates"][0]["kind"], "card");
        assert_eq!(json["input"]["candidates"][1]["kind"], "player");
        // v2 removed the flat `label` in favour of `presentation`.
        assert!(json["input"].get("label").is_none());
        assert_eq!(json["input"]["presentation"]["title"], "Choose target");
    }

    /// Grounds the capability registry in behaviour rather than prose.
    ///
    /// Every claim of the form "X has no exact upstream shape" is falsifiable
    /// by exhibiting the mapping, and this test exhibits them. The v2.0.0
    /// registry asserted that surveil, discard, optional triggers, unless-costs
    /// and trigger ordering all lacked an upstream shape; each in fact maps
    /// onto a primitive the protocol already defines, so the entries were
    /// wrong, not merely pessimistic. Re-introducing such a claim now breaks a
    /// test instead of shipping as a confident, unfalsifiable comment.
    #[test]
    fn families_claimed_unmappable_are_actually_mappable() {
        // CR 701.42a: surveil is scry whose second destination is the
        // graveyard. `ScryInput::zones` parameterizes exactly that.
        let json = serde_json::to_value(
            build_prompt(
                &prepared_for(WaitingFor::SurveilChoice {
                    player: PlayerId(0),
                    cards: vec![],
                }),
                &lookup,
            )
            .unwrap(),
        )
        .unwrap();
        assert_eq!(json["input"]["type"], "scry");
        assert_eq!(json["input"]["zones"][0], "libraryTop");
        assert_eq!(json["input"]["zones"][1], "graveyard");

        // CR 603.12: an optional trigger is a yes/no, i.e. ChooseBoolean, and
        // its answer is DecideOptionalEffect.
        let prepared = prepared_for(WaitingFor::OptionalEffectChoice {
            player: PlayerId(0),
            source_id: ObjectId(1),
            description: Some("Draw a card?".to_string()),
            may_trigger_key: None,
        });
        let json = serde_json::to_value(build_prompt(&prepared, &lookup).unwrap()).unwrap();
        assert_eq!(json["input"]["type"], "chooseBoolean");
        assert_eq!(json["input"]["presentation"]["title"], "Draw a card?");

        let ctx = context_with(vec![]);
        for (answer, expected) in [(true, true), (false, false)] {
            let action = translate_response(
                ctx.prompt_id,
                PromptOutput::ChooseBoolean(ChooseBooleanOutput::Decision { value: answer }),
                &ctx,
                &prepared.state,
            )
            .unwrap();
            assert_eq!(
                action,
                GameAction::DecideOptionalEffect { accept: expected },
                "optional trigger must round-trip both answers"
            );
        }

        // CR 701.43d: exert reuses the same boolean family but must resolve to
        // a different engine action — proving the dispatch is on `WaitingFor`,
        // not hardcoded per family.
        let exert = prepared_for(WaitingFor::ExertChoice {
            player: PlayerId(0),
            attacker: ObjectId(1),
            remaining: vec![],
        });
        assert_eq!(
            translate_response(
                ctx.prompt_id,
                PromptOutput::ChooseBoolean(ChooseBooleanOutput::Decision { value: true }),
                &ctx,
                &exert.state,
            )
            .unwrap(),
            GameAction::ChooseExert { exert: true }
        );

        // CR 603.3b: trigger order round-trips through `Reorder`, and the item
        // id must be the trigger INDEX — using the source object id would
        // collide when one permanent contributes two simultaneous triggers.
        let triggers = prepared_for(WaitingFor::OrderTriggers {
            player: PlayerId(0),
            triggers: vec![],
        });
        assert_eq!(
            translate_response(
                ctx.prompt_id,
                PromptOutput::Reorder(ReorderOutput::ReorderDecision {
                    ordered_ids: vec!["2".to_string(), "0".to_string(), "1".to_string()],
                }),
                &ctx,
                &triggers.state,
            )
            .unwrap(),
            GameAction::OrderTriggers {
                order: vec![2, 0, 1]
            }
        );
    }

    #[test]
    fn representative_supported_prompts_build() {
        let cases = [
            (
                "mulligan",
                WaitingFor::MulliganDecision {
                    pending: vec![MulliganDecisionEntry {
                        player: PlayerId(0),
                        mulligan_count: 1,
                        phase: MulliganDecisionPhase::Declare,
                    }],
                    free_first_mulligan: false,
                },
            ),
            (
                "mulliganPutBack",
                WaitingFor::MulliganDecision {
                    pending: vec![MulliganDecisionEntry {
                        player: PlayerId(0),
                        mulligan_count: 1,
                        phase: MulliganDecisionPhase::BottomCards {
                            count: 1,
                            then: PendingMulliganAction::Keep,
                        },
                    }],
                    free_first_mulligan: false,
                },
            ),
            (
                "chooseAttackers",
                WaitingFor::DeclareAttackers {
                    player: PlayerId(0),
                    valid_attacker_ids: vec![ObjectId(1)],
                    valid_attack_targets: vec![AttackTarget::Player(PlayerId(1))],
                    valid_attack_targets_by_attacker: None,
                    attacker_constraints: Default::default(),
                },
            ),
            (
                "chooseBlockers",
                WaitingFor::DeclareBlockers {
                    player: PlayerId(0),
                    valid_blocker_ids: vec![ObjectId(1)],
                    valid_block_targets: HashMap::from([(ObjectId(2), vec![ObjectId(1)])]),
                    block_requirements: HashMap::new(),
                    blocker_constraints: Default::default(),
                },
            ),
            (
                "chooseNumber",
                WaitingFor::ChooseXValue {
                    player: PlayerId(0),
                    min: 0,
                    max: 3,
                    pending_cast: dummy_pending_cast(),
                    convoke_mode: None,
                    x_cost_previews: vec![],
                },
            ),
            (
                "chooseCombatDamageAssignment",
                WaitingFor::AssignCombatDamage {
                    player: PlayerId(0),
                    attacker_id: ObjectId(1),
                    total_damage: 1,
                    blockers: vec![],
                    assignment_modes: vec![],
                    trample: None,
                    defending_player: PlayerId(1),
                    attack_target: AttackTarget::Player(PlayerId(1)),
                    pw_loyalty: None,
                    pw_controller: None,
                },
            ),
            ("gameOver", WaitingFor::GameOver { winner: None }),
        ];

        for (expected_type, waiting_for) in cases {
            let prompt = build_prompt(&prepared_for(waiting_for), &lookup).unwrap();
            let json = serde_json::to_value(prompt).unwrap();
            assert_eq!(json["input"]["type"], expected_type);
        }
    }

    /// CR 508.1a–d wire contract: each attacker's `validTargetIds` comes from the
    /// engine per-attacker map when it is `Some`, falling back to the aggregate
    /// list only when the map is `None` (legacy). An explicit empty entry yields
    /// NO targets, so absent and empty stay distinguishable.
    #[test]
    fn declare_attackers_dto_follows_per_attacker_map() {
        let some_map = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![ObjectId(1), ObjectId(2)],
            valid_attack_targets: vec![
                AttackTarget::Player(PlayerId(1)),
                AttackTarget::Player(PlayerId(2)),
            ],
            valid_attack_targets_by_attacker: Some(HashMap::from([
                (ObjectId(1), vec![AttackTarget::Player(PlayerId(1))]),
                (ObjectId(2), vec![]),
            ])),
            attacker_constraints: Default::default(),
        };
        let json =
            serde_json::to_value(build_prompt(&prepared_for(some_map), &lookup).unwrap()).unwrap();
        let attackers = json["input"]["attackers"].as_array().unwrap();
        assert_eq!(attackers.len(), 2);
        assert_eq!(
            attackers[0]["validTargetIds"].as_array().unwrap().len(),
            1,
            "attacker 1 follows its own map entry ([P1])"
        );
        assert_eq!(
            attackers[1]["validTargetIds"].as_array().unwrap().len(),
            0,
            "attacker 2's explicit-empty map entry yields no targets — the aggregate is NOT reused"
        );

        let none_map = WaitingFor::DeclareAttackers {
            player: PlayerId(0),
            valid_attacker_ids: vec![ObjectId(1)],
            valid_attack_targets: vec![
                AttackTarget::Player(PlayerId(1)),
                AttackTarget::Player(PlayerId(2)),
            ],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
        let json =
            serde_json::to_value(build_prompt(&prepared_for(none_map), &lookup).unwrap()).unwrap();
        let attackers = json["input"]["attackers"].as_array().unwrap();
        assert_eq!(
            attackers[0]["validTargetIds"].as_array().unwrap().len(),
            2,
            "a None map falls back to the aggregate list (2 targets)"
        );
    }

    #[test]
    fn unsupported_prompt_returns_stable_code() {
        let result = build_prompt(
            &prepared_for(WaitingFor::KeepWithinTotalPowerChoice {
                player: PlayerId(0),
                target_player: PlayerId(0),
                eligible: vec![ObjectId(1), ObjectId(2)],
                cap: 4,
                choose_filter: TargetFilter::Any,
                sacrifice_filter: TargetFilter::Any,
                chooser_scope: engine::types::ability::CategoryChooserScope::EachPlayerSelf,
                source_id: ObjectId(1),
                source_controller: PlayerId(0),
                remaining_players: vec![],
                all_kept: vec![],
                scoped_players: vec![PlayerId(0)],
            }),
            &lookup,
        );

        assert!(matches!(
            result,
            Err(AdapterError::UnsupportedPrompt {
                code: "local.keep-with-total-power-unsupported",
                ..
            })
        ));
    }

    /// The generic path: a waiting state with no bespoke arm is now prompted
    /// from the engine's own projection instead of being refused.
    ///
    /// `TopOrBottomChoice` is chosen deliberately. It is one of the 85 variants
    /// this adapter never names, and its projected choices differ only by a
    /// `Value` surface — so this also pins that `choice_label` reads the
    /// surfaces rather than falling back to the opaque choice id.
    ///
    /// Indices are compared by looking the label up rather than by assuming a
    /// candidate order the engine never promised; the assertion that matters is
    /// that the index the client echoes round-trips to the action that label
    /// stands for.
    #[test]
    fn an_unmapped_waiting_state_prompts_from_the_interaction_projection() {
        let mut state = GameState::new_two_player(7);
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Scried Card".to_string(),
            Zone::Library,
        );
        state.waiting_for = WaitingFor::TopOrBottomChoice {
            player: PlayerId(0),
            object_id,
        };
        bind_interaction_authority(&mut state, InteractionSessionId("generic-path".to_string()))
            .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("an unmapped waiting state is served by the projection, not refused");
        let PromptInput::ChooseFromSelection(input) = prompt else {
            panic!("a finite candidate list is ChooseFromSelection's shape, got {prompt:?}");
        };
        let labels = input
            .options
            .iter()
            .map(|option| option.label.clone())
            .collect::<Vec<_>>();
        assert!(
            labels.iter().any(|label| label == "top")
                && labels.iter().any(|label| label == "bottom"),
            "the projection labels each choice from its Value surface, got {labels:?}"
        );
        assert_eq!((input.min_total, input.max_total), (1, 1));

        let top_index = labels.iter().position(|label| label == "top").unwrap();
        let action = translate_response(
            42,
            PromptOutput::ChooseFromSelection(ChooseFromSelectionOutput::SelectionDecision {
                chosen_indices: vec![top_index],
            }),
            &prepared.prompt_context(),
            &state,
        )
        .expect("the echoed index resolves back through the engine");
        assert_eq!(action, GameAction::ChooseTopOrBottom { top: true });
    }

    /// The `Select` half of the generic path: a subset choice, not a one-of.
    ///
    /// `DiscardToHandSize` (CR 514.1) is the clearest case — discard exactly
    /// `count` of the cards in hand — so it pins the two things that distinguish
    /// this from the `ExactChoices` path: the count bounds reach the prompt as
    /// the family's own min/max instead of the hardcoded 1/1, and the answer must
    /// go back as `InteractionResponse::Select`, since the engine rejects a
    /// `Choose` against a `Select` schema as malformed.
    ///
    /// The family is `ChooseCards`, not `ChooseFromSelection`: an unordered
    /// subset over a list of objects is a card selection, and the client is owed
    /// the cards rather than three opaque labels.
    #[test]
    fn a_select_schema_carries_its_count_bounds_and_answers_as_a_subset() {
        let mut state = GameState::new_two_player(7);
        let cards = ["Discard A", "Discard B", "Discard C"]
            .into_iter()
            .map(|name| {
                create_object(
                    &mut state,
                    CardId(1),
                    PlayerId(0),
                    name.to_string(),
                    Zone::Hand,
                )
            })
            .collect::<Vec<_>>();
        state.waiting_for = WaitingFor::DiscardToHandSize {
            player: PlayerId(0),
            count: 2,
            cards: cards.clone(),
        };
        bind_interaction_authority(&mut state, InteractionSessionId("select-path".to_string()))
            .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("a Select schema is served by the projection");
        let PromptInput::ChooseCards(input) = prompt else {
            panic!("a subset choice over objects is ChooseCards, got {prompt:?}");
        };
        assert_eq!(
            (input.min, input.max),
            (2, 2),
            "the engine's count bounds must survive, not the one-of path's 1/1"
        );
        assert_eq!(
            input
                .cards
                .iter()
                .map(|card| card.identity.name.as_str())
                .collect::<Vec<_>>(),
            vec!["Discard A", "Discard B", "Discard C"],
            "every hand card is a candidate, named — not a labelled option"
        );

        let action = translate_response(
            42,
            PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision {
                chosen_card_ids: vec![input.cards[0].id.clone(), input.cards[1].id.clone()],
            }),
            &prepared.prompt_context(),
            &state,
        )
        .expect("a two-card subset resolves back through the engine");
        assert_eq!(
            action,
            GameAction::SelectCards {
                cards: vec![cards[0], cards[1]],
            },
            "a discard subset answers with the engine-materialized SelectCards"
        );
    }

    /// The card family is wired at all three sites, not just at the prompt.
    ///
    /// `ChooseRingBearer` (CR 701.54a) is chosen over a discard because its
    /// answering action is *not* `SelectCards`: the bespoke discard arm, which
    /// this family already had, would have answered it with the wrong action
    /// entirely. So a green here means prompt construction, response
    /// translation, and the gate all reached the generic path.
    ///
    /// The gate is exercised by construction: `translate_response` runs
    /// `output_family_matches_waiting` first, and `ChooseRingBearer` is not in
    /// the bespoke `matches!`, so without `open_prompt_is_generic_cards` this
    /// legal answer is rejected as `IllegalResponseForPrompt` before any
    /// translation runs.
    #[test]
    fn a_card_selection_prompts_as_cards_and_answers_the_engines_own_action() {
        let mut state = GameState::new_two_player(7);
        let candidates = ["Frodo Baggins", "Samwise Gamgee"]
            .into_iter()
            .map(|name| {
                create_object(
                    &mut state,
                    CardId(1),
                    PlayerId(0),
                    name.to_string(),
                    Zone::Battlefield,
                )
            })
            .collect::<Vec<_>>();
        state.waiting_for = WaitingFor::ChooseRingBearer {
            player: PlayerId(0),
            candidates: candidates.clone(),
        };
        bind_interaction_authority(&mut state, InteractionSessionId("ring-bearer".to_string()))
            .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("a Select schema over objects is served by the projection");
        let PromptInput::ChooseCards(input) = prompt else {
            panic!("a non-targeting selection over objects is ChooseCards, got {prompt:?}");
        };
        assert_eq!(
            (input.min, input.max),
            (1, 1),
            "CR 701.54a: the Ring tempts you, so choose one creature you control"
        );
        assert_eq!(
            input
                .cards
                .iter()
                .map(|card| (card.id.clone(), card.identity.name.clone()))
                .collect::<Vec<_>>(),
            vec![
                (encode_object_id(candidates[0]), "Frodo Baggins".to_string()),
                (
                    encode_object_id(candidates[1]),
                    "Samwise Gamgee".to_string()
                ),
            ],
            "the candidates reach the client as cards, keyed by the wire id the answer echoes"
        );

        let action = translate_response(
            42,
            PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision {
                chosen_card_ids: vec![encode_object_id(candidates[1])],
            }),
            &prepared.prompt_context(),
            &state,
        )
        .expect("the echoed card id resolves back through the engine");
        assert_eq!(
            action,
            GameAction::ChooseRingBearer {
                target: candidates[1],
            },
            "the engine names the action; the bespoke discard arm would have said SelectCards"
        );
    }

    /// An unoffered card is refused, and the refusal is not vacuous.
    ///
    /// The positive leg proves the fixture reaches the generic card path at all
    /// — without it, a rejection could equally mean the prompt never became a
    /// `ChooseCards` in the first place.
    #[test]
    fn a_card_answer_naming_an_unoffered_card_is_refused() {
        let mut state = GameState::new_two_player(7);
        let offered = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Frodo Baggins".to_string(),
            Zone::Battlefield,
        );
        let bystander = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Gollum".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::ChooseRingBearer {
            player: PlayerId(0),
            candidates: vec![offered],
        };
        bind_interaction_authority(&mut state, InteractionSessionId("ring-guard".to_string()))
            .expect("valid interaction authority binding");
        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();

        // Reach-guard: the offered card really does answer this prompt.
        assert!(translate_response(
            42,
            PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision {
                chosen_card_ids: vec![encode_object_id(offered)],
            }),
            &prepared.prompt_context(),
            &state,
        )
        .is_ok());

        assert!(matches!(
            translate_response(
                42,
                PromptOutput::ChooseCards(ChooseCardsOutput::ChooseCardsDecision {
                    chosen_card_ids: vec![encode_object_id(bystander)],
                }),
                &prepared.prompt_context(),
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt { .. })
        ));
    }

    /// An ordered sequence over objects is **not** reclassified as cards.
    ///
    /// The schema leg of the classifier, isolated: `ProliferateChoice` (CR
    /// 701.29a) projects every eligible permanent through the same
    /// `Object`/`Candidate` surface a card selection uses, so the candidates
    /// alone would pass. Only the schema keeps it out — widening
    /// [`card_selection_candidates`] to accept `Sequence` turns this red.
    ///
    /// The bounds and candidate count are the reach-guard: a state that failed
    /// to build at all would produce nothing to count.
    #[test]
    fn an_ordered_sequence_over_objects_is_not_a_card_selection() {
        let mut state = GameState::new_two_player(7);
        let permanent = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Bearer".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::ProliferateChoice {
            player: PlayerId(0),
            eligible: vec![TargetRef::Object(permanent)],
        };
        bind_interaction_authority(&mut state, InteractionSessionId("proliferate".to_string()))
            .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("a Sequence schema is served by the projection");
        let PromptInput::ChooseFromSelection(input) = prompt else {
            panic!("an ordered sequence stays in the labelled family, got {prompt:?}");
        };
        assert_eq!(
            (input.min_total, input.max_total, input.options.len()),
            (0, 1, 1),
            "the sequence bounds and the candidate survive"
        );
    }

    /// A subset whose candidates are not plain candidates is **not** cards.
    ///
    /// The surface leg, isolated: `OutsideGameChoice` (CR 400.11a / CR 406.3) is
    /// a `Select` schema — the very schema the card classifier keys on — but its
    /// candidates are projected in the `FaceUpExile` role, because a card
    /// outside the game is not interchangeable with one the client can render
    /// from the battlefield snapshot. Dropping the role test in
    /// [`candidate_object`] turns this red.
    #[test]
    fn a_subset_whose_candidates_are_not_plain_objects_is_not_a_card_selection() {
        let mut state = GameState::new_two_player(7);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Learn Source".to_string(),
            Zone::Battlefield,
        );
        let exiled = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Lesson Card".to_string(),
            Zone::Exile,
        );
        state.waiting_for = WaitingFor::OutsideGameChoice {
            player: PlayerId(0),
            source_id,
            choices: vec![OutsideGameChoiceEntry {
                source: OutsideGameChoiceSource::FaceUpExile { object_id: exiled },
                count: 1,
                name: "Lesson Card".to_string(),
            }],
            count: 1,
            reveal: false,
            up_to: true,
            destination: Zone::Hand,
        };
        bind_interaction_authority(&mut state, InteractionSessionId("outside-game".to_string()))
            .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("a Select schema is served by the projection");
        let PromptInput::ChooseFromSelection(input) = prompt else {
            panic!("a non-candidate role stays in the labelled family, got {prompt:?}");
        };
        assert_eq!(
            (input.min_total, input.max_total, input.options.len()),
            (0, 1, 1),
            "the selection bounds and the candidate survive"
        );
    }

    /// A `Number` schema leaves the selection family entirely.
    ///
    /// `PayAmountChoice` (CR 107.14 — pay any amount of `{E}`) is the only
    /// unmapped numeric pause. It pins two things: the engine's range reaches
    /// the client as `ChooseNumber`'s bounds, and the answer resolves to the
    /// action the *engine* names. That second half is the point —
    /// `GameAction::ChooseX` is specific to X (CR 107.3), so the bespoke arm
    /// would have answered this pause with the wrong action entirely.
    #[test]
    fn a_number_schema_becomes_choose_number_and_resolves_to_the_engines_action() {
        let mut state = GameState::new_two_player(7);
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Energy Sink".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::PayAmountChoice {
            player: PlayerId(0),
            resource: PayableResource::Energy,
            min: 0,
            max: 3,
            accumulated: 0,
            source_id,
            pending_mana_ability: None,
        };
        bind_interaction_authority(&mut state, InteractionSessionId("number-path".to_string()))
            .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("a Number schema is served by the projection");
        let PromptInput::ChooseNumber(input) = prompt else {
            panic!("a numeric range is ChooseNumber, not a selection, got {prompt:?}");
        };
        assert_eq!(
            (input.min, input.max),
            (0, 3),
            "the engine's range must survive into the prompt"
        );

        let action = translate_response(
            42,
            PromptOutput::ChooseNumber(ChooseNumberOutput::NumberDecision {
                chosen_number: Some(2),
            }),
            &prepared.prompt_context(),
            &state,
        )
        .expect("the chosen number resolves back through the engine");
        assert_eq!(action, GameAction::SubmitPayAmount { amount: 2 });
    }

    /// A `Sequence` schema is an *ordered* subset, and the order must survive.
    ///
    /// `ProliferateChoice` (CR 701.27) projects min 0 / max = eligible count, so
    /// it also pins that a zero minimum reaches the prompt intact rather than
    /// being coerced to the one-of path's 1.
    ///
    /// The answer deliberately reverses the offered order. That is the whole
    /// assertion: the engine fills its slots in the order the client sent, so a
    /// path that collected indices into a set — or sorted them — would return
    /// the targets the other way round and fail here.
    #[test]
    fn a_sequence_schema_preserves_the_order_the_client_sent() {
        let mut state = GameState::new_two_player(7);
        let first = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Holder A".to_string(),
            Zone::Battlefield,
        );
        let second = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Counter Holder B".to_string(),
            Zone::Battlefield,
        );
        state.waiting_for = WaitingFor::ProliferateChoice {
            player: PlayerId(0),
            eligible: vec![TargetRef::Object(first), TargetRef::Object(second)],
        };
        bind_interaction_authority(
            &mut state,
            InteractionSessionId("sequence-path".to_string()),
        )
        .expect("valid interaction authority binding");

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        let prompt = build_prompt_input(&prepared, &lookup)
            .expect("a Sequence schema is served by the projection");
        let PromptInput::ChooseFromSelection(input) = prompt else {
            panic!("an ordered subset still renders as ChooseFromSelection, got {prompt:?}");
        };
        assert_eq!(
            (input.min_total, input.max_total),
            (0, 2),
            "proliferate is optional, so the zero minimum must survive"
        );

        let action = translate_response(
            42,
            PromptOutput::ChooseFromSelection(ChooseFromSelectionOutput::SelectionDecision {
                chosen_indices: vec![1, 0],
            }),
            &prepared.prompt_context(),
            &state,
        )
        .expect("an ordered subset resolves back through the engine");
        assert_eq!(
            action,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(second), TargetRef::Object(first)],
            },
            "the engine must receive the targets in the order the client chose"
        );
    }

    /// Without a bound interaction authority the projection is empty, so the
    /// generic path cannot serve the prompt and the adapter must say so rather
    /// than emit an option-less selection. This is also the non-vacuity guard
    /// for the test above: it is the same waiting state, differing only in the
    /// binding, so that test cannot be passing for an unrelated reason.
    #[test]
    fn an_unbound_interaction_authority_leaves_the_generic_path_unsupported() {
        let mut state = GameState::new_two_player(7);
        let object_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Scried Card".to_string(),
            Zone::Library,
        );
        state.waiting_for = WaitingFor::TopOrBottomChoice {
            player: PlayerId(0),
            object_id,
        };

        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-a", 42).unwrap();
        assert!(matches!(
            build_prompt_input(&prepared, &lookup),
            Err(AdapterError::UnsupportedPrompt {
                code: "local.prompt-unsupported",
                ..
            })
        ));
    }

    // ------------------------------------------------------- wire shapes ---

    /// The core v2 change: `PromptOutput` is ADJACENTLY tagged, so the family's
    /// own output nests under `output`.
    #[test]
    fn prompt_output_nests_under_an_output_key() {
        let output = PromptOutput::ChooseNumber(ChooseNumberOutput::NumberDecision {
            chosen_number: Some(3),
        });

        assert_eq!(
            serde_json::to_value(&output).unwrap(),
            serde_json::json!({
                "type": "chooseNumber",
                "output": { "type": "numberDecision", "chosenNumber": 3 }
            })
        );
    }

    /// The counterpart guard: `PromptInput` is INTERNALLY tagged with no
    /// `content`, so it FLATTENS. Adding `content = "input"` for symmetry with
    /// `PromptOutput` would silently break every prompt.
    #[test]
    fn prompt_input_stays_flat() {
        let input = PromptInput::ChooseAction(ChooseActionInput { actions: vec![] });
        let json = serde_json::to_value(&input).unwrap();

        assert_eq!(
            json,
            serde_json::json!({ "type": "chooseAction", "actions": [] })
        );
        assert!(json.get("input").is_none(), "no nesting key");
        assert!(json.get("content").is_none());
    }

    #[test]
    fn client_to_server_response_uses_action_not_output() {
        let message = ClientToServerMessage::Response {
            prompt_id: 7,
            action: PromptOutput::ChooseAction(ChooseActionOutput::Act {
                action_id: "action-0".to_string(),
            }),
        };

        let json = serde_json::to_value(&message).unwrap();
        assert_eq!(
            json,
            serde_json::json!({
                "kind": "response",
                "promptId": 7,
                "action": {
                    "type": "chooseAction",
                    "output": { "type": "act", "actionId": "action-0" }
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<ClientToServerMessage>(json).unwrap(),
            message
        );
    }

    #[test]
    fn client_to_server_directive_carries_concede() {
        let message = ClientToServerMessage::Directive {
            directive: DirectiveInput::Concede,
        };

        assert_eq!(
            serde_json::to_value(&message).unwrap(),
            serde_json::json!({
                "kind": "directive",
                "directive": { "type": "concede" }
            })
        );
    }

    #[test]
    fn card_view_hidden_carries_only_an_id() {
        let zone = ZoneDto {
            zone: ZoneKind::Library,
            owner_id: "player-0".to_string(),
            cards: vec![CardView::Hidden {
                id: "card-3".to_string(),
            }],
            count: 7,
        };

        let json = serde_json::to_value(&zone).unwrap();
        assert_eq!(
            json["cards"][0],
            serde_json::json!({ "visibility": "hidden", "id": "card-3" })
        );
        assert_eq!(json["count"], 7);
        assert!(
            json["count"].as_u64().unwrap() > json["cards"].as_array().unwrap().len() as u64,
            "count may legitimately exceed cards.len()"
        );
    }

    #[test]
    fn play_card_mode_renames_more_than_meets_the_eye() {
        assert_eq!(
            serde_json::to_value(PlayCardMode::Alternative {
                cost: AlternativeCostKind::MTMtE,
            })
            .unwrap(),
            serde_json::json!({ "type": "alternative", "cost": "moreThanMeetsTheEye" })
        );
        assert_eq!(
            serde_json::to_value(PlayCardMode::ForetellExile).unwrap(),
            serde_json::json!({ "type": "foretellExile" })
        );
    }

    /// `PaymentAction` flattens its kind, so `id` and the kind's fields sit at
    /// the same level.
    #[test]
    fn payment_action_flattens_its_kind() {
        let action = PaymentAction {
            id: "action-2".to_string(),
            kind: PaymentActionKind::UseResource {
                card_id: "card-9".to_string(),
                resource: PaymentResourceKind::Delve,
            },
        };

        assert_eq!(
            serde_json::to_value(&action).unwrap(),
            serde_json::json!({
                "id": "action-2",
                "type": "useResource",
                "cardId": "card-9",
                "resource": "delve"
            })
        );
    }

    /// Guards the `rename_all_fields` omission: without it these serialize as
    /// snake_case and Rust round-trips still pass.
    #[test]
    fn display_event_fields_are_camel_case() {
        let event = DisplayEvent::CardPlayed {
            card_id: "card-1".to_string(),
            card_name: "Lightning Bolt".to_string(),
            set_code: "LEA".to_string(),
            player_id: "player-0".to_string(),
        };

        assert_eq!(
            serde_json::to_value(&event).unwrap(),
            serde_json::json!({
                "kind": "cardPlayed",
                "cardId": "card-1",
                "cardName": "Lightning Bolt",
                "setCode": "LEA",
                "playerId": "player-0"
            })
        );

        let turn = DisplayEvent::TurnChanged {
            active_player_id: "player-1".to_string(),
            active_player_name: "Bob".to_string(),
            turn_number: 3,
        };
        let json = serde_json::to_value(&turn).unwrap();
        assert_eq!(json["activePlayerId"], "player-1");
        assert_eq!(json["activePlayerName"], "Bob");
        assert_eq!(json["turnNumber"], 3);
    }

    /// Relay payload keys are not derivable from the kind name: `display`
    /// carries `event`, and `log`/`snapshot` carry `entry`.
    #[test]
    fn relay_envelope_payload_keys_match_the_transport_table() {
        let state = GameState::new_two_player(7);
        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let update = build_state_update(&prepared, &lookup).unwrap();

        let json = serde_json::to_value(RelayMessage::State {
            state: update,
            for_player: None,
        })
        .unwrap();
        assert_eq!(json["kind"], "state");
        assert!(
            json["state"].get("gameView").is_some(),
            "`state` nests a StateUpdate wrapper, not a bare GameViewDto"
        );
        assert!(
            json.get("forPlayer").is_none(),
            "forPlayer is optional on state — absent means the public view"
        );

        let display = serde_json::to_value(RelayMessage::Display {
            event: DisplayEvent::TurnChanged {
                active_player_id: "player-0".to_string(),
                active_player_name: "Alice".to_string(),
                turn_number: 1,
            },
        })
        .unwrap();
        assert!(
            display.get("event").is_some(),
            "display's payload key is `event`"
        );
        assert!(display.get("display").is_none());

        for message in [
            RelayMessage::Log {
                entry: serde_json::json!({ "opaque": true }),
                from_player: "player-0".to_string(),
            },
            RelayMessage::Snapshot {
                entry: serde_json::json!({ "opaque": true }),
                from_player: "player-0".to_string(),
            },
        ] {
            let json = serde_json::to_value(&message).unwrap();
            assert!(
                json.get("entry").is_some(),
                "log/snapshot payload key is `entry`, got {json}"
            );
        }

        let error = serde_json::to_value(RelayMessage::Error {
            error: ProtocolError {
                code: ProtocolErrorCode::StalePrompt,
                message: "stale".to_string(),
                prompt_id: Some(4),
            },
            for_player: "player-0".to_string(),
        })
        .unwrap();
        assert_eq!(error["error"]["code"], "stalePrompt");
        assert_eq!(error["forPlayer"], "player-0");
    }

    #[test]
    fn state_update_round_trips_and_rejects_unknown_fields() {
        let state = GameState::new_two_player(7);
        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let update = build_state_update(&prepared, &lookup).unwrap();

        let mut value = serde_json::to_value(&update).unwrap();
        assert_eq!(
            serde_json::from_value::<StateUpdate>(value.clone()).unwrap(),
            update
        );

        value
            .as_object_mut()
            .unwrap()
            .insert("bogusField".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<StateUpdate>(value).is_err());
    }

    #[test]
    fn agent_prompt_round_trips_and_rejects_unknown_fields() {
        let prompt = build_prompt(
            &prepared_for(WaitingFor::Priority {
                player: PlayerId(0),
            }),
            &lookup,
        )
        .unwrap();

        let mut value = serde_json::to_value(&prompt).unwrap();
        assert_eq!(
            serde_json::from_value::<AgentPrompt>(value.clone()).unwrap(),
            prompt
        );

        value
            .as_object_mut()
            .unwrap()
            .insert("bogusField".to_string(), serde_json::json!(true));
        assert!(
            serde_json::from_value::<AgentPrompt>(value).is_err(),
            "AgentPrompt is deny_unknown_fields — no vendor field may ever be added to it"
        );
    }

    #[test]
    fn default_card_dto_omits_optional_fields_and_round_trips() {
        let card = CardDto::default();
        let value = serde_json::to_value(&card).unwrap();
        let object = value.as_object().unwrap();

        for omitted in [
            "isCopy",
            "foil",
            "isCrewed",
            "isAttacking",
            "isRingBearer",
            "isMadnessExiled",
            "isPlotted",
            "isWarpExiled",
            "wouldDieInCombat",
            "basePower",
            "baseToughness",
            "attackingPlayerId",
            "attackTargetId",
            "attachedTo",
            "attachmentIds",
            "mergedCardIds",
            "flashbackCost",
            "kickerCost",
            "effectiveManaCost",
            "madnessCost",
        ] {
            assert!(
                !object.contains_key(omitted),
                "default CardDto should omit `{omitted}`"
            );
        }
        assert!(
            !object.contains_key("zoneId"),
            "zoneId was removed in v2 — the zone is carried by ZoneDto"
        );

        assert_eq!(serde_json::from_value::<CardDto>(value).unwrap(), card);
    }

    /// One representative instance of every `PromptInput` variant, paired with
    /// its expected camelCase discriminant tag.
    fn prompt_input_cases() -> Vec<(&'static str, PromptInput)> {
        let card = CardDto::default;
        vec![
            (
                "chooseAction",
                PromptInput::ChooseAction(ChooseActionInput { actions: vec![] }),
            ),
            (
                "payManaCost",
                PromptInput::PayManaCost(PayManaCostInput {
                    presentation: presentation("Pay for Lightning Bolt"),
                    card_id: "card-1".to_string(),
                    card_name: "Lightning Bolt".to_string(),
                    mana_cost: "{R}".to_string(),
                    can_confirm_from_pool: true,
                    actions: vec![],
                }),
            ),
            (
                "mulligan",
                PromptInput::Mulligan(MulliganInput {
                    hand_card_ids: vec!["card-1".to_string(), "card-2".to_string()],
                    mulligan_count: 2,
                }),
            ),
            (
                "mulliganPutBack",
                PromptInput::MulliganPutBack(MulliganPutBackInput {
                    hand_card_ids: vec!["card-1".to_string()],
                    cards: vec![card()],
                    count: 1,
                    excluded_card_id: None,
                }),
            ),
            (
                "chooseAttackers",
                PromptInput::ChooseAttackers(ChooseAttackersInput {
                    attackers: vec![AttackerOptionDto {
                        attacker_id: "card-1".to_string(),
                        valid_target_ids: vec!["player-1".to_string()],
                        must_attack: false,
                    }],
                    attack_targets: vec![AttackTargetDto {
                        id: "player-1".to_string(),
                        label: "Player 1".to_string(),
                        kind: AttackTargetKind::Player,
                    }],
                }),
            ),
            (
                "chooseBlockers",
                PromptInput::ChooseBlockers(ChooseBlockersInput {
                    attackers: vec![BlockableAttackerDto {
                        attacker_id: "card-1".to_string(),
                        valid_blocker_ids: vec!["card-2".to_string()],
                        min_blockers: 0,
                        max_blockers: Some(1),
                        must_be_blocked: false,
                    }],
                    available_blocker_ids: vec!["card-2".to_string()],
                    error: None,
                }),
            ),
            (
                "chooseBoardTargets",
                PromptInput::ChooseBoardTargets(ChooseBoardTargetsInput {
                    presentation: presentation("Choose target"),
                    candidates: vec![TargetRefDto {
                        kind: TargetKindDto::Card,
                        id: "card-1".to_string(),
                        intent: None,
                        oracle: None,
                    }],
                    hostile: true,
                    intent: TargetingIntent::Damage,
                    min_targets: 1,
                    max_targets: 1,
                    chosen_targets: 0,
                }),
            ),
            (
                "chooseBoolean",
                PromptInput::ChooseBoolean(ChooseBooleanInput {
                    presentation: presentation("Question"),
                    confirm_label: "Yes".to_string(),
                    deny_label: "No".to_string(),
                }),
            ),
            (
                "chooseCards",
                PromptInput::ChooseCards(ChooseCardsInput {
                    presentation: presentation("Pick cards"),
                    cards: vec![card()],
                    min: 0,
                    max: 1,
                }),
            ),
            (
                "chooseColor",
                PromptInput::ChooseColor(ChooseColorInput {
                    presentation: presentation("Choose a color"),
                    valid_colors: vec!["R".to_string(), "G".to_string()],
                    amount: 1,
                    repeat_allowed: false,
                }),
            ),
            (
                "chooseCombatDamageAssignment",
                PromptInput::ChooseCombatDamageAssignment(ChooseCombatDamageAssignmentInput {
                    attacker_id: "card-1".to_string(),
                    blocker_ids: vec!["card-2".to_string()],
                    defender_id: Some("player-1".to_string()),
                    total_damage: 3,
                    attacker_has_deathtouch: true,
                }),
            ),
            (
                "chooseDamageAssignmentOrder",
                PromptInput::ChooseDamageAssignmentOrder(ChooseDamageAssignmentOrderInput {
                    attacker_id: "card-1".to_string(),
                    blocker_ids: vec!["card-2".to_string()],
                    blocker_cards: vec![card()],
                }),
            ),
            (
                "chooseFromSelection",
                PromptInput::ChooseFromSelection(ChooseFromSelectionInput {
                    presentation: presentation("Choose mode"),
                    options: vec![
                        selection_option("Mode A".to_string()),
                        selection_option("Mode B".to_string()),
                    ],
                    min_total: 1,
                    max_total: 1,
                }),
            ),
            (
                "chooseNumber",
                PromptInput::ChooseNumber(ChooseNumberInput {
                    presentation: presentation("Choose X"),
                    min: 0,
                    max: 3,
                }),
            ),
            (
                "revealCards",
                PromptInput::RevealCards(RevealCardsInput {
                    presentation: presentation("Revealed cards"),
                    cards: vec![card()],
                    zone: ZoneKind::Hand,
                    owner_player_id: "player-0".to_string(),
                }),
            ),
            (
                "scry",
                PromptInput::Scry(ScryInput {
                    presentation: presentation("Scry"),
                    cards: vec![card()],
                    zones: vec![ScryDestination::LibraryTop, ScryDestination::LibraryBottom],
                }),
            ),
            (
                "reorder",
                PromptInput::Reorder(ReorderInput {
                    presentation: presentation("Reorder"),
                    items: vec![ReorderItem {
                        id: "card-1".to_string(),
                        card: card(),
                        oracle: None,
                    }],
                }),
            ),
            (
                "diceRolled",
                PromptInput::DiceRolled(DiceRolledInput {
                    presentation: presentation("Roll"),
                    sides: 6,
                    rolls: vec![DiceRollEntry {
                        label: Some("d6".to_string()),
                        player_id: Some("player-0".to_string()),
                        natural_results: vec![4],
                        final_results: vec![4],
                        ignored_rolls: vec![],
                        highlighted: false,
                    }],
                    source_card_name: None,
                }),
            ),
            ("gameOver", PromptInput::GameOver(GameOverInput {})),
        ]
    }

    #[test]
    fn every_prompt_input_family_round_trips_with_camel_case_tag() {
        let cases = prompt_input_cases();

        assert_eq!(cases.len(), 19);
        let tags: HashSet<_> = cases.iter().map(|(tag, _)| *tag).collect();
        assert_eq!(tags.len(), 19, "discriminant tags must be unique");

        for (tag, input) in &cases {
            let value = serde_json::to_value(input).unwrap();
            assert_eq!(value["type"], *tag, "wrong discriminant tag for {tag}");
            let back: PromptInput = serde_json::from_value(value).unwrap();
            assert_eq!(&back, input, "round-trip mismatch for {tag}");
        }
    }

    #[test]
    fn prompt_input_fields_serialize_as_camel_case() {
        let value = serde_json::to_value(PromptInput::PayManaCost(PayManaCostInput {
            presentation: presentation("Pay for Bolt"),
            card_id: "card-1".to_string(),
            card_name: "Bolt".to_string(),
            mana_cost: "{R}".to_string(),
            can_confirm_from_pool: true,
            actions: vec![],
        }))
        .unwrap();
        assert_eq!(value["cardId"], "card-1");
        assert_eq!(value["cardName"], "Bolt");
        assert_eq!(value["manaCost"], "{R}");
        assert_eq!(value["canConfirmFromPool"], true);
        assert!(
            value.get("description").is_none(),
            "the flat `description` was replaced by `presentation`"
        );

        let targets =
            serde_json::to_value(PromptInput::ChooseBoardTargets(ChooseBoardTargetsInput {
                presentation: presentation("Choose"),
                candidates: vec![],
                hostile: false,
                intent: TargetingIntent::Damage,
                min_targets: 1,
                max_targets: 2,
                chosen_targets: 0,
            }))
            .unwrap();
        assert_eq!(targets["minTargets"], 1);
        assert_eq!(targets["maxTargets"], 2);
        assert_eq!(targets["intent"], "damage");
    }

    #[test]
    fn reorder_output_field_is_ordered_ids() {
        let output = PromptOutput::Reorder(ReorderOutput::ReorderDecision {
            ordered_ids: vec!["card-1".to_string()],
        });
        let json = serde_json::to_value(&output).unwrap();

        assert_eq!(json["type"], "reorder");
        assert_eq!(json["output"]["orderedIds"][0], "card-1");
        assert!(json["output"].get("orderedCardIds").is_none());
    }

    // -------------------------------------------------------- conformance ---

    /// Two of the five obligations: the output family must match the prompt, and
    /// every echoed action id must have been advertised.
    #[test]
    fn validate_response_rejects_wrong_family_and_unadvertised_id() {
        let prompt = PromptInput::ChooseAction(ChooseActionInput {
            actions: vec![AvailableAction {
                id: "action-0".to_string(),
                kind: AvailableActionKind::Cast {
                    card_id: "card-1".to_string(),
                    mode: PlayCardMode::Normal,
                    label: "Cast".to_string(),
                },
            }],
        });

        assert_eq!(
            prompt.validate_response(&PromptOutput::ChooseAction(ChooseActionOutput::Act {
                action_id: "action-0".to_string(),
            })),
            Ok(())
        );

        assert_eq!(
            prompt.validate_response(&PromptOutput::ChooseAction(ChooseActionOutput::Act {
                action_id: "action-99".to_string(),
            })),
            Err(ResponseViolation::UnknownActionId("action-99".to_string()))
        );

        assert_eq!(
            prompt.validate_response(&PromptOutput::Mulligan(MulliganOutput::MulliganDecision {
                keep: true
            })),
            Err(ResponseViolation::WrongPromptType)
        );
    }

    /// A `gameOver` prompt is terminal: `PromptOutput` has no matching arm, so
    /// every response to it is a family mismatch.
    #[test]
    fn game_over_prompt_accepts_no_response() {
        let prompt = PromptInput::GameOver(GameOverInput {});
        assert_eq!(
            prompt.validate_response(&PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                until: None,
                exhaust_stack: false,
            })),
            Err(ResponseViolation::WrongPromptType)
        );
    }

    /// All five `ProtocolErrorCode` variants must have a wire producer.
    #[test]
    fn every_protocol_error_code_has_a_producer() {
        let produced: HashSet<_> = [
            protocol_error_for(
                &AdapterError::PromptIdMismatch {
                    expected: 1,
                    actual: 2,
                },
                Some(1),
            ),
            protocol_error_for(
                &AdapterError::NoAuthorizedPrompt {
                    viewer: PlayerId(0),
                },
                Some(1),
            ),
            protocol_error_for_violation(&ResponseViolation::WrongPromptType, Some(1)),
            protocol_error_for_violation(
                &ResponseViolation::UnknownActionId("action-9".to_string()),
                Some(1),
            ),
            protocol_error_for(
                &AdapterError::MalformedId {
                    expected_prefix: "card-",
                    value: "nope".to_string(),
                },
                None,
            ),
        ]
        .iter()
        .map(|error| error.code)
        .collect();

        assert_eq!(
            produced.len(),
            5,
            "each of the five conformance failures must map to a distinct code"
        );
        for code in [
            ProtocolErrorCode::StalePrompt,
            ProtocolErrorCode::WrongPlayer,
            ProtocolErrorCode::WrongPromptType,
            ProtocolErrorCode::UnknownActionId,
            ProtocolErrorCode::InvalidShape,
        ] {
            assert!(produced.contains(&code), "no producer for {code:?}");
        }
    }

    /// An unknown prompt `type` is a SOFT error — deserialization returns `Err`
    /// rather than panicking, so a conforming engine can answer `invalidShape`.
    #[test]
    fn unknown_output_type_is_a_soft_error() {
        let result = serde_json::from_value::<PromptOutput>(serde_json::json!({
            "type": "somethingFromTheFuture",
            "output": { "type": "whatever" }
        }));

        assert!(result.is_err(), "unknown tags must not deserialize");
        assert_eq!(
            protocol_error_for(
                &AdapterError::UnsupportedProtocolFeature {
                    code: "local.unknown-output",
                },
                Some(3),
            )
            .code,
            ProtocolErrorCode::InvalidShape
        );
    }

    #[test]
    fn protocol_error_round_trips_and_rejects_unknown_fields() {
        let error = ProtocolError {
            code: ProtocolErrorCode::WrongPlayer,
            message: "not your seat".to_string(),
            prompt_id: Some(9),
        };

        let mut value = serde_json::to_value(&error).unwrap();
        assert_eq!(value["code"], "wrongPlayer");
        assert_eq!(value["promptId"], 9);
        assert_eq!(
            serde_json::from_value::<ProtocolError>(value.clone()).unwrap(),
            error
        );

        value
            .as_object_mut()
            .unwrap()
            .insert("extra".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<ProtocolError>(value).is_err());
    }

    // --------------------------------------------------------- responses ---

    #[test]
    fn response_checks_prompt_id_and_resolves_action_id() {
        let context = context_with(vec![GameAction::CastSpell {
            object_id: ObjectId(1),
            card_id: CardId(1),
            targets: Vec::new(),
            payment_mode: Default::default(),
        }]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                8,
                PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                    until: None,
                    exhaust_stack: false,
                }),
                &context,
                &state,
            ),
            Err(AdapterError::PromptIdMismatch {
                expected: 7,
                actual: 8
            })
        ));

        assert_eq!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Act {
                    action_id: "action-0".to_string(),
                }),
                &context,
                &state,
            )
            .unwrap(),
            GameAction::CastSpell {
                object_id: ObjectId(1),
                card_id: CardId(1),
                targets: Vec::new(),
                payment_mode: Default::default(),
            }
        );
    }

    /// Prompt id 0 is reserved and must never be accepted as a real answer, even
    /// if the context somehow carries it.
    #[test]
    fn reserved_prompt_id_zero_is_never_accepted_as_an_answer() {
        let mut context = context_with(vec![GameAction::PassPriority]);
        context.prompt_id = RESERVED_ABSENT_PLAYER_PROMPT_ID;
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                RESERVED_ABSENT_PLAYER_PROMPT_ID,
                PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                    until: None,
                    exhaust_stack: false,
                }),
                &context,
                &state,
            ),
            Err(AdapterError::PromptIdMismatch { .. })
        ));
    }

    /// Concede moved out of `ChooseActionOutput` and into a directive, which
    /// belongs to no prompt.
    #[test]
    fn concede_directive_translates_without_a_prompt() {
        let context = context_with(vec![]);
        let state = GameState::new_two_player(7);

        assert_eq!(
            translate_client_message(
                ClientToServerMessage::Directive {
                    directive: DirectiveInput::Concede,
                },
                &context,
                &state,
            )
            .unwrap(),
            GameAction::Concede {
                player_id: PlayerId(0),
            }
        );
    }

    #[test]
    fn client_message_response_routes_to_translate_response() {
        let context = context_with(vec![GameAction::PassPriority]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert_eq!(
            translate_client_message(
                ClientToServerMessage::Response {
                    prompt_id: 7,
                    action: PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                        until: None,
                        exhaust_stack: false,
                    }),
                },
                &context,
                &state,
            )
            .unwrap(),
            GameAction::PassPriority
        );
    }

    #[test]
    fn mulligan_and_scry_responses_translate_to_engine_actions() {
        let context = context_with(vec![]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            }],
            free_first_mulligan: false,
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::Mulligan(MulliganOutput::MulliganDecision { keep: true }),
                &context,
                &state,
            )
            .unwrap(),
            GameAction::MulliganDecision {
                choice: engine::types::actions::MulliganChoice::Keep
            }
        ));

        state.waiting_for = WaitingFor::ScryChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1), ObjectId(2)],
        };
        assert_eq!(
            translate_response(
                7,
                PromptOutput::Scry(ScryOutput::ScryDecision {
                    zone_card_ids: vec![vec!["card-1".to_string()], vec!["card-2".to_string()]],
                }),
                &context,
                &state,
            )
            .unwrap(),
            GameAction::SelectCards {
                cards: vec![ObjectId(2)]
            }
        );
    }

    /// CR 103.5b: a Serum Powder response is a `Mulligan` family output.
    #[test]
    fn mulligan_use_serum_powder_response_translates() {
        let context = context_with(vec![]);
        let mut state = GameState::new_two_player(7);
        let powder = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Serum Powder".to_string(),
            Zone::Hand,
        );
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            }],
            free_first_mulligan: false,
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::Mulligan(MulliganOutput::MulliganUseSerumPowder {
                    card_id: encode_object_id(powder),
                }),
                &context,
                &state,
            )
            .unwrap(),
            GameAction::MulliganDecision {
                choice: engine::types::actions::MulliganChoice::UseSerumPowder { object_id },
            } if object_id == powder
        ));
    }

    #[test]
    fn response_family_must_match_current_prompt() {
        let context = context_with(vec![]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::Mulligan(MulliganOutput::MulliganDecision { keep: true }),
                &context,
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt {
                response_kind: "mulligan"
            })
        ));
    }

    #[test]
    fn response_translation_rechecks_authorized_submitter() {
        let context = context_with(vec![]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                    until: None,
                    exhaust_stack: false,
                }),
                &context,
                &state,
            ),
            Err(AdapterError::NoAuthorizedPrompt {
                viewer: PlayerId(0)
            })
        ));
    }

    #[test]
    fn unsupported_response_modifiers_are_rejected() {
        let context = context_with(vec![GameAction::PassPriority]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                    until: Some(PassUntil {
                        player_id: "player-0".to_string(),
                        phase: StepKind::Main1,
                    }),
                    exhaust_stack: false,
                }),
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.pass-until-unsupported"
            })
        ));

        // v2's new `exhaustStack` is the same class of multi-window intent.
        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Pass {
                    until: None,
                    exhaust_stack: true,
                }),
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.exhaust-stack-pass-unsupported"
            })
        ));

        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };
        assert!(matches!(
            translate_response(
                7,
                PromptOutput::PayManaCost(PayManaCostOutput::Pay { auto: true }),
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.auto-pay-unsupported"
            })
        ));
    }

    #[test]
    fn act_with_unknown_action_id_is_stale_or_invalid() {
        let context = context_with(vec![GameAction::CastSpell {
            object_id: ObjectId(1),
            card_id: CardId(1),
            targets: Vec::new(),
            payment_mode: Default::default(),
        }]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Act {
                    action_id: "action-99".to_string(),
                }),
                &context,
                &state,
            ),
            Err(AdapterError::StaleOrInvalidActionId { action_id }) if action_id == "action-99"
        ));
    }

    #[test]
    fn act_response_cannot_execute_unadvertised_unsupported_action() {
        let context = context_with(vec![GameAction::ChooseKeptCreatures {
            kept: vec![ObjectId(1)],
        }]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Act {
                    action_id: "action-0".to_string(),
                }),
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.non-target-selection-unsupported"
            })
        ));
    }

    #[test]
    fn act_on_advertised_prompt_level_action_is_illegal() {
        let context = context_with(vec![GameAction::PassPriority]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseAction(ChooseActionOutput::Act {
                    action_id: "action-0".to_string(),
                }),
                &context,
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt {
                response_kind: "act"
            })
        ));
    }

    #[test]
    fn color_response_only_translates_for_mana_color_prompt() {
        let context = context_with(vec![]);
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::ChooseManaColor {
            player: PlayerId(0),
            choice: ManaChoicePrompt::SingleColor {
                options: vec![ManaType::Red],
            },
            context: engine::types::game_state::ManaChoiceContext::ResolvingEffect(Box::new(
                dummy_ability(),
            )),
        };

        assert_eq!(
            translate_response(
                7,
                PromptOutput::ChooseColor(ChooseColorOutput::ColorDecision {
                    chosen_colors: BTreeMap::from([("R".to_string(), 1)]),
                }),
                &context,
                &state,
            )
            .unwrap(),
            GameAction::ChooseManaColor {
                choice: ManaChoice::SingleColor(ManaType::Red),
                count: 1,
            }
        );

        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert!(matches!(
            translate_response(
                7,
                PromptOutput::ChooseColor(ChooseColorOutput::ColorDecision {
                    chosen_colors: BTreeMap::from([("R".to_string(), 1)]),
                }),
                &context,
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt {
                response_kind: "chooseColor"
            })
        ));
    }

    // ------------------------------------------------- advertised actions ---

    /// The highest-risk silent break: payment actions must be advertised from
    /// the SAME `action-{index}` id space `action_table` enumerates, or the
    /// echoed id resolves to nothing and every mana payment fails.
    ///
    /// Revert guard: a `mana-{i}`-style scheme over a filtered list compiles and
    /// passes clippy, and flips the `advertised_payment_action_by_id` assertion.
    #[test]
    fn advertised_payment_action_id_resolves_through_the_action_table() {
        let actions = vec![
            // A Skip, so the payment list is NOT index-aligned with the table —
            // which is exactly what a separate id space would get wrong.
            GameAction::PassPriority,
            GameAction::UntapLandForMana {
                object_id: ObjectId(4),
            },
        ];
        let state = GameState::new_two_player(7);
        let filtered = filter_state_for_viewer(&state, PlayerId(0));
        let prepared = PreparedManabrewSnapshot {
            game_id: "game-a".to_string(),
            viewer: PlayerId(0),
            prompt_id: 7,
            // A real projection rather than a stand-in. This state has no bound
            // interaction authority, so it comes back empty — which is correct
            // and irrelevant here: the assertions below concern the payment
            // action id space, which `pay_mana_cost_input` reads from `actions`.
            interaction: derive_viewer_interaction(&state, &filtered, PlayerId(0)),
            state,
            derived: DerivedViews::default(),
            actions: actions.clone(),
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
            source_card_object: None,
        };

        let input = pay_mana_cost_input(&prepared);
        assert_eq!(
            input.actions.len(),
            1,
            "PassPriority is a prompt-level Skip"
        );
        let advertised = &input.actions[0];
        assert_eq!(
            advertised.id, "action-1",
            "the id is the index in `prepared.actions`, not in the filtered payment list"
        );

        let context = PromptContext {
            prompt_id: 7,
            deciding_player: PlayerId(0),
            action_table: action_table(&actions),
        };
        assert_eq!(
            advertised_payment_action_by_id(&context, &advertised.id).unwrap(),
            GameAction::UntapLandForMana {
                object_id: ObjectId(4)
            },
            "an advertised payment id must resolve back to its GameAction"
        );
    }

    /// CR 702.51a: convoke is a payment resource, and the only one this engine
    /// has an action for.
    #[test]
    fn convoke_is_advertised_as_a_payment_resource() {
        let actions = vec![GameAction::TapForConvoke {
            object_id: ObjectId(5),
            mana_type: ManaType::Green,
        }];
        let payments = payment_actions(&actions);

        assert_eq!(payments.len(), 1);
        assert_eq!(
            serde_json::to_value(&payments[0]).unwrap(),
            serde_json::json!({
                "id": "action-0",
                "type": "useResource",
                "cardId": "card-5",
                "resource": "convoke"
            })
        );
    }

    /// `PayLife` is advertised for exactly one thing — a Phyrexian route that
    /// actually spends life (CR 107.4f) — and for nothing else.
    ///
    /// `SubmitLifeRedistribution` is the trap this pins: it is the other engine
    /// action with "life" in its name, and it is a pick-one among precomputed
    /// options (`local.selection-unsupported`), not a payment.
    #[test]
    fn pay_life_is_advertised_only_for_a_life_paying_phyrexian_route() {
        let non_payments = vec![
            // CR 107.4f: an all-mana route spends no life, so it is not a
            // pay-life move and must not be advertised as `PayLife { 0 }`.
            GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayMana],
            },
            GameAction::SubmitLifeRedistribution { option_index: 0 },
        ];
        assert!(
            payment_actions(&non_payments).is_empty(),
            "only a life-paying Phyrexian route may be advertised as PayLife"
        );
    }

    /// Land plays were previously mapped to `Unsupported` and therefore filtered
    /// out entirely — meaning no land was playable by a ManaBrew client at all.
    ///
    /// Revert guard: reinstating the `Unsupported` arm empties `available_actions`.
    #[test]
    fn land_play_is_advertised_as_a_normal_cast() {
        let actions = vec![GameAction::PlayLand {
            object_id: ObjectId(3),
            card_id: CardId(1),
        }];
        let advertised = available_actions(&empty_state(), &actions);

        assert_eq!(advertised.len(), 1, "a land play must reach the client");
        assert_eq!(
            serde_json::to_value(&advertised[0]).unwrap(),
            serde_json::json!({
                "id": "action-0",
                "type": "cast",
                "cardId": "card-3",
                "mode": { "type": "normal" },
                "label": "Play land"
            })
        );
    }

    /// `PlayLand` carries no face discriminator, so `backFaceLand` can never be
    /// produced — inferring the face from card data would be game logic in a
    /// serialization boundary.
    #[test]
    fn back_face_land_mode_is_never_produced() {
        let modes: Vec<_> = [
            GameAction::PlayLand {
                object_id: ObjectId(3),
                card_id: CardId(1),
            },
            GameAction::CastSpell {
                object_id: ObjectId(4),
                card_id: CardId(1),
                targets: Vec::new(),
                payment_mode: Default::default(),
            },
        ]
        .iter()
        .filter_map(|action| {
            match convert_available_action(&empty_state(), action, "action-0".to_string()) {
                AvailableActionConversion::Available(AvailableAction {
                    kind: AvailableActionKind::Cast { mode, .. },
                    ..
                }) => Some(mode),
                _ => None,
            }
        })
        .collect();

        assert_eq!(modes, vec![PlayCardMode::Normal, PlayCardMode::Normal]);
    }

    /// Sneak, web-slinging, and foretell have exact v2 counterparts and were
    /// previously dropped as unsupported — each was a lost legal play.
    #[test]
    fn alternative_cast_actions_map_to_their_exact_counterparts() {
        let cases = [
            (
                GameAction::CastSpellAsSneak {
                    hand_object: ObjectId(7),
                    card_id: CardId(1),
                    creature_to_return: ObjectId(8),
                    payment_mode: Default::default(),
                },
                serde_json::json!({ "type": "alternative", "cost": "sneak" }),
                "card-7",
            ),
            (
                GameAction::CastSpellAsWebSlinging {
                    hand_object: ObjectId(9),
                    card_id: CardId(1),
                    creature_to_return: ObjectId(10),
                    payment_mode: Default::default(),
                },
                serde_json::json!({ "type": "alternative", "cost": "webSlinging" }),
                "card-9",
            ),
            (
                GameAction::Foretell {
                    object_id: ObjectId(11),
                    card_id: CardId(1),
                },
                serde_json::json!({ "type": "foretellExile" }),
                "card-11",
            ),
            (
                GameAction::CastSpellAsMadness {
                    object_id: ObjectId(12),
                    card_id: CardId(1),
                    payment_mode: Default::default(),
                },
                serde_json::json!({ "type": "alternative", "cost": "madness" }),
                "card-12",
            ),
        ];

        for (action, expected_mode, expected_card) in cases {
            let advertised = available_actions(&empty_state(), std::slice::from_ref(&action));
            assert_eq!(advertised.len(), 1, "{action:?} must reach the client");
            let json = serde_json::to_value(&advertised[0]).unwrap();
            assert_eq!(json["mode"], expected_mode);
            assert_eq!(json["cardId"], expected_card);
        }
    }

    /// CR 702.180b: the harmonize TAP is a cost-reduction tap during payment,
    /// structurally convoke's analogue, and `PaymentResourceKind` is exactly
    /// `Convoke | Improvise | Delve`. It stays unsupported rather than being
    /// mapped to a near-miss variant. (Ninjutsu used to be pinned here on the
    /// false premise that it needed an `AlternativeCostKind`; CR 702.49a makes
    /// it an activated ability, and it is now advertised — see
    /// `ninjutsu_is_advertised_as_an_activated_ability`.)
    #[test]
    fn actions_without_exact_counterparts_stay_unsupported() {
        assert!(matches!(
            convert_available_action(
                &empty_state(),
                &GameAction::HarmonizeTap {
                    creature_id: Some(ObjectId(1)),
                },
                "action-0".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.harmonize-tap-unsupported")
        ));
    }

    /// CR 702.49a: ninjutsu is an ACTIVATED ABILITY, so it belongs on
    /// `AvailableActionKind::ActivateAbility` — its absence from
    /// `AlternativeCostKind` was never evidence of anything.
    ///
    /// CR 702.49c: the returned creature fixes what the ninja enters attacking,
    /// so each (ninja, attacker) pair is a distinct play and must be
    /// distinguishable by more than its opaque action id.
    #[test]
    fn ninjutsu_is_advertised_as_an_activated_ability() {
        use engine::types::ability::{
            AbilityCost, AbilityDefinition, AbilityKind, Effect, NinjutsuVariant, RuntimeHandler,
        };

        let mut state = GameState::new_two_player(7);
        let ninja = engine::game::zones::create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Ninja of the Deep Hours".to_string(),
            Zone::Hand,
        );
        let ornithopter = engine::game::zones::create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Ornithopter".to_string(),
            Zone::Battlefield,
        );
        let hasten = engine::game::zones::create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Memnite".to_string(),
            Zone::Battlefield,
        );

        // Slot 0 is an ordinary activated ability so a naive `0` cannot pass.
        state.objects.get_mut(&ninja).unwrap().abilities = std::sync::Arc::new(vec![
            AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate)
                .cost(AbilityCost::Tap),
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::RuntimeHandled {
                    handler: RuntimeHandler::NinjutsuFamily,
                },
            )
            .cost(AbilityCost::NinjutsuFamily {
                variant: NinjutsuVariant::Ninjutsu,
                mana_cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 1,
                },
            }),
        ]);

        let pairs = [ornithopter, hasten].map(|attacker| GameAction::ActivateNinjutsu {
            ninjutsu_object_id: ninja,
            creature_to_return: attacker,
        });
        let advertised = available_actions(&state, &pairs);

        assert_eq!(
            advertised.len(),
            2,
            "one advertised action per (ninjutsu card, returned attacker) pair"
        );
        assert_eq!(
            serde_json::to_value(&advertised[0]).unwrap(),
            serde_json::json!({
                "id": "action-0",
                "type": "activateAbility",
                "cardId": encode_object_id(ninja),
                "abilityIndex": 1,
                "description": "Ninjutsu — return Ornithopter",
                "isManaAbility": false
            })
        );
        assert_eq!(
            serde_json::to_value(&advertised[1]).unwrap()["description"],
            "Ninjutsu — return Memnite",
            "the returned attacker is what distinguishes the pairs (CR 702.49c)"
        );

        // The echoed id resolves back to the ninjutsu action itself, not to a
        // reconstructed `ActivateAbility` — which the engine forbids, because
        // its `NinjutsuFamily` cost arm is a no-op in `pay_ability_cost`.
        let context = PromptContext {
            prompt_id: 7,
            deciding_player: PlayerId(0),
            action_table: action_table(&pairs),
        };
        assert_eq!(
            advertised_action_by_id(&context, &state, "action-0").unwrap(),
            GameAction::ActivateNinjutsu {
                ninjutsu_object_id: ninja,
                creature_to_return: ornithopter,
            }
        );
    }

    /// CR 107.4f: a Phyrexian shard costs one mana of its color **or 2 life**,
    /// so a route's life price is exactly `2 x` its `PayLife` shards.
    ///
    /// The advertised entries come from the engine's own enumerated routes, so
    /// every id an echo can carry resolves — the adapter never assembles a
    /// route of its own.
    #[test]
    fn phyrexian_route_is_advertised_as_a_pay_life_payment() {
        let actions = vec![
            GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayMana],
            },
            GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayLife],
            },
            GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayLife, ShardChoice::PayLife],
            },
        ];
        let payments = payment_actions(&actions);

        assert_eq!(
            payments.len(),
            2,
            "the all-mana route spends no life and is not a pay-life move"
        );
        assert_eq!(
            serde_json::to_value(&payments[0]).unwrap(),
            serde_json::json!({ "id": "action-1", "type": "payLife", "amount": 2 }),
            "a single pending shard advertises exactly one PayLife of 2"
        );
        assert_eq!(
            serde_json::to_value(&payments[1]).unwrap(),
            serde_json::json!({ "id": "action-2", "type": "payLife", "amount": 4 }),
            "two life-paying shards cost 4, not 2 — the amount is per route"
        );

        // Ids live in the same `action-{index}` space `action_table` enumerates,
        // which is the only reason an echoed payment id resolves at all.
        let context = PromptContext {
            prompt_id: 7,
            deciding_player: PlayerId(0),
            action_table: action_table(&actions),
        };
        assert_eq!(
            advertised_payment_action_by_id(&context, "action-1").unwrap(),
            GameAction::SubmitPhyrexianChoices {
                choices: vec![ShardChoice::PayLife],
            },
        );
    }

    /// A Phyrexian shard is a payment move, not a priority action — so it must
    /// be `Skip` at the priority layer (like convoke), never `Unsupported`,
    /// which would make an echoed id fail with a capability code.
    #[test]
    fn phyrexian_choices_are_skipped_at_the_priority_layer() {
        assert!(matches!(
            convert_available_action(
                &empty_state(),
                &GameAction::SubmitPhyrexianChoices {
                    choices: vec![ShardChoice::PayLife],
                },
                "action-0".to_string(),
            ),
            AvailableActionConversion::Skip
        ));
    }

    #[test]
    fn unsupported_actions_are_not_serialized_as_custom_actions() {
        assert!(matches!(
            convert_available_action(
                &empty_state(),
                &GameAction::ChooseKeptCreatures {
                    kept: vec![ObjectId(1)]
                },
                "action-0".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.non-target-selection-unsupported")
        ));
        assert!(available_actions(
            &empty_state(),
            &[GameAction::ChooseKeptCreatures {
                kept: vec![ObjectId(1)]
            }]
        )
        .is_empty());

        assert!(matches!(
            convert_available_action(
                &empty_state(),
                &GameAction::ChooseAnnouncingOpponent {
                    opponent: PlayerId(1),
                },
                "action-1".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.announcing-opponent-unsupported")
        ));
    }

    #[test]
    fn meld_actions_return_stable_unsupported_capability_codes() {
        assert!(matches!(
            convert_available_action(
                &empty_state(),
                &GameAction::ChooseMeldPair {
                    source_id: ObjectId(1),
                    partner_id: ObjectId(2),
                },
                "action-0".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.meld-pair-choice-unsupported")
        ));
        assert!(matches!(
            convert_available_action(
                &empty_state(),
                &GameAction::ChooseEntryAttackTarget {
                    target: AttackTarget::Battle(ObjectId(3)),
                },
                "action-1".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.entry-attack-target-choice-unsupported")
        ));
        assert!(
            available_actions(
                &empty_state(),
                &[
                    GameAction::ChooseMeldPair {
                        source_id: ObjectId(1),
                        partner_id: ObjectId(2),
                    },
                    GameAction::ChooseEntryAttackTarget {
                        target: AttackTarget::Player(PlayerId(1)),
                    },
                ]
            )
            .is_empty(),
            "unsupported meld decisions must never be serialized as generic custom actions"
        );
    }

    // ------------------------------------------------------- capabilities ---

    #[test]
    fn unsupported_capability_registry_is_well_formed() {
        let capabilities = unsupported_protocol_capabilities();
        assert_eq!(capabilities.len(), 82);

        let codes: HashSet<_> = capabilities
            .iter()
            .map(|capability| capability.code)
            .collect();
        assert_eq!(codes.len(), 82, "capability codes must be unique");

        for capability in capabilities {
            assert!(
                capability.code.starts_with("upstream.") || capability.code.starts_with("local."),
                "code `{}` must be namespaced upstream./local.",
                capability.code
            );
            assert!(!capability.reason.is_empty());
            assert!(!capability.suggested_protocol_extension.is_empty());
        }
    }

    /// Behavioural pin: a representative action per still-unsupported family
    /// converts to a code the registry declares.
    ///
    /// This walks a hand-written list, so on its own it cannot close the class —
    /// that is what [`no_emitted_capability_code_is_undeclared`] is for. Its
    /// value is the inverse assertion: each row must still BE `Unsupported`, so
    /// a family that quietly becomes supported fails here instead of leaving a
    /// stale registry entry behind.
    #[test]
    fn every_declared_capability_code_regression_pin() {
        let declared: HashSet<_> = unsupported_protocol_capabilities()
            .iter()
            .map(|capability| capability.code)
            .collect();
        let state = GameState::new_two_player(7);

        let actions = [
            // Stands in for the whole dungeon/room family, which shares one code.
            GameAction::ChooseDungeonRoom { room_index: 0 },
            GameAction::HarmonizeTap {
                creature_id: Some(ObjectId(1)),
            },
            GameAction::SpendPoolMana {
                pip_id: engine::types::mana::ManaPipId(1),
            },
            GameAction::UnspendPoolMana {
                pip_id: engine::types::mana::ManaPipId(1),
            },
            GameAction::ChooseMeldPair {
                source_id: ObjectId(1),
                partner_id: ObjectId(2),
            },
            GameAction::ChooseKeptCreatures {
                kept: vec![ObjectId(1)],
            },
        ];

        for action in actions {
            // Assert the conversion IS `Unsupported` rather than testing inside
            // an `if let`: were one of these to become supported later, an
            // `if let` would skip its body and this pin would quietly cover one
            // action fewer while still reporting green.
            match convert_available_action(&state, &action, "action-0".to_string()) {
                AvailableActionConversion::Unsupported(code) => assert!(
                    declared.contains(code),
                    "`{code}` is emitted for {action:?} but not declared in \
                     unsupported_protocol_capabilities()"
                ),
                AvailableActionConversion::Available(_) | AvailableActionConversion::Skip => {
                    panic!(
                        "{action:?} is no longer Unsupported — this pin has lost \
                         its subject. Replace it with an action that still \
                         exercises the code it was pinning, or drop the row."
                    )
                }
            }
        }
    }

    /// Closes the class the per-action pin cannot: **every** capability code
    /// this crate can emit is declared in the registry.
    ///
    /// An undeclared code is a silent lie — the registry is the machine-readable
    /// contract a client queries to learn what we cannot do, so a code that
    /// resolves to nothing at the far end is worse than no code. The registry
    /// was a curated design document that covered 29 of the 67 codes then
    /// emitted; it is now exhaustive, and this keeps it that way without
    /// requiring anyone to re-run the audit by hand.
    ///
    /// It scans the source rather than the `GameAction` enum because the codes
    /// are `&'static str` literals at ~65 scattered call sites, several of them
    /// outside `convert_available_action` entirely (prompt construction,
    /// response translation, id parsing). Only the production half is scanned:
    /// the test module names retired codes on purpose, to assert they are gone.
    #[test]
    fn no_emitted_capability_code_is_undeclared() {
        let declared: HashSet<_> = unsupported_protocol_capabilities()
            .iter()
            .map(|capability| capability.code)
            .collect();

        let source = include_str!("lib.rs");
        // The first `mod tests {` in the file is the module header itself, which
        // precedes this literal, so the split lands on the real boundary.
        let (production, _) = source
            .split_once("mod tests {")
            .expect("lib.rs always contains its test module");

        let mut emitted: Vec<&str> = Vec::new();
        for prefix in ["\"local.", "\"upstream."] {
            let mut rest = production;
            // Read each `"<namespace>.<code>"` literal whole. Codes are
            // `[a-z0-9-]`, so the charset filter drops any prose that happens to
            // open a quote with the same prefix without silently dropping a real
            // code.
            while let Some(open) = rest.find(prefix) {
                let after = &rest[open + 1..];
                let Some(close) = after.find('"') else { break };
                let literal = &after[..close];
                if literal
                    .chars()
                    .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '.')
                {
                    emitted.push(literal);
                }
                rest = &after[close + 1..];
            }
        }

        // Nonvacuity floor. A scanner that silently stops matching reports green
        // for the wrong reason, so assert it still sees roughly the population
        // it saw when written (65 distinct codes at 65 live call sites). The
        // floor only ever needs raising; a drop means the scanner broke, not
        // that the adapter shrank.
        let distinct: HashSet<_> = emitted.iter().copied().collect();
        assert!(
            distinct.len() >= 50,
            "the scanner found only {} distinct codes (65 when written) — it has \
             stopped measuring the population, which reads as a pass but proves \
             nothing",
            distinct.len()
        );
        assert!(
            distinct.contains("local.prompt-unsupported"),
            "the scanner missed a code emitted at a known site — it is not \
             reading the production half of the file"
        );

        let undeclared: Vec<&str> = emitted
            .iter()
            .copied()
            .filter(|code| !declared.contains(code))
            .collect();
        assert!(
            undeclared.is_empty(),
            "these codes are emitted but not declared in \
             unsupported_protocol_capabilities(): {undeclared:?}"
        );
    }

    /// Every gap this migration introduced or surfaced must be recorded, and
    /// every entry v2 made obsolete must be gone.
    #[test]
    fn capability_registry_reflects_v2_reality() {
        let codes: HashSet<_> = unsupported_protocol_capabilities()
            .iter()
            .map(|capability| capability.code)
            .collect();

        for expected in [
            "local.player-concede-status-unsourceable",
            "local.first-strike-damage-step-unproducible",
            "local.play-card-mode-fidelity-gaps",
            "local.back-face-land-mode-unproducible",
            "local.mdfc-face-choice-unsupported",
            "local.harmonize-tap-unsupported",
            "local.payment-resource-actions-missing",
            "local.exhaust-stack-pass-unsupported",
            // Every code the adapter can emit must be declared here, or a
            // client that receives it looks it up and finds nothing.
            "local.dungeon-room-unsupported",
            "local.room-right-split-mode-unproducible",
            "local.counter-key-vocabulary-unverifiable",
        ] {
            assert!(codes.contains(expected), "missing new gap `{expected}`");
        }

        for retained in [
            "local.meld-pair-choice-unsupported",
            "local.entry-attack-target-choice-unsupported",
            "local.zone-opponent-chooser-unsupported",
        ] {
            assert!(codes.contains(retained), "dropped genuine gap `{retained}`");
        }

        for obsolete in [
            // v2 defines the PromptId/response envelope this described.
            "upstream.response-envelope-mismatch",
            // v2's PaymentAction supplies the payment primitives.
            "upstream.mana-payment-primitives-insufficient",
            // v2 replaced the legacy engine-action wrapper with ClientToServerMessage.
            "local.legacy-engine-action-unsupported",
            "local.legacy-choose-target-card-removed",
            // Both were adapter-side signature limits, and both are now fixed:
            // `GameState` is threaded into available-action conversion, so
            // ninjutsu is advertised as `ActivateAbility` (CR 702.49a), and a
            // Phyrexian route is advertised as `PayLife` (CR 107.4f).
            "local.ninjutsu-cast-unsupported",
            "local.phyrexian-payment-unsupported",
        ] {
            assert!(
                !codes.contains(obsolete),
                "`{obsolete}` was made obsolete by v2 and must be removed"
            );
        }
    }

    /// `prepare_snapshot` guards the two-player assumption, and
    /// `prepare_snapshot_with_prompt_id` carries a real id through.
    #[test]
    fn prepare_snapshot_requires_exactly_two_players() {
        let state = GameState::new_two_player(7);
        let prepared = prepare_snapshot_with_prompt_id(&state, PlayerId(0), "game-x", 99).unwrap();
        assert_eq!(prepared.prompt_id, 99);
        assert_eq!(prepared.viewer, PlayerId(0));
        assert_eq!(prepared.prompt_context().prompt_id, 99);

        let mut solo = GameState::new_two_player(7);
        solo.players.truncate(1);
        assert!(matches!(
            prepare_snapshot(&solo, PlayerId(0), "game-x"),
            Err(AdapterError::UnsupportedPlayerCount { count: 1 })
        ));
    }

    /// Both vendor extensions are deliberate, but their safety arguments differ.
    ///
    /// `excludedCardId` is genuinely additive: `MulliganPutBackInput` has no
    /// `deny_unknown_fields`, so a conforming peer ignores it. The extra
    /// `MulliganOutput` variant is NOT additive in that sense — a conforming
    /// peer's deserializer errors on an unknown tag. It is safe only because the
    /// enum flows client→engine and both ends are ours, so a third-party client
    /// never emits it.
    #[test]
    fn vendor_extensions_are_deliberate_and_isolated() {
        let json = serde_json::to_value(MulliganPutBackInput {
            hand_card_ids: vec![],
            cards: vec![],
            count: 1,
            excluded_card_id: Some("card-1".to_string()),
        })
        .unwrap();
        assert_eq!(json["excludedCardId"], "card-1");

        // A peer that does not know the field simply drops it.
        let mut without = json.clone();
        without.as_object_mut().unwrap().remove("excludedCardId");
        assert!(serde_json::from_value::<MulliganPutBackInput>(without).is_ok());

        assert_eq!(
            serde_json::to_value(MulliganOutput::MulliganUseSerumPowder {
                card_id: "card-1".to_string(),
            })
            .unwrap()["type"],
            "mulliganUseSerumPowder"
        );
    }
}

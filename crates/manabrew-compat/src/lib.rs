use std::collections::{BTreeMap, HashMap};

use engine::ai_support::legal_actions_for_viewer;
use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::derived::derive_display_state;
use engine::game::derived_views::{derive_views, DerivedViews};
use engine::game::filter_state_for_viewer;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::game::turn_control;
use engine::types::ability::TargetRef;
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
use engine::types::game_state::{
    GameState, ManaChoice, ManaChoicePrompt, MulliganDecisionPhase, PendingMulliganAction,
    StackEntryKind, WaitingFor,
};
use engine::types::mana::{ManaColor as EngineManaColor, ManaCost, ManaCostShard, ManaType};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;
use engine::types::{GameAction, ObjectId};
use serde::{Deserialize, Serialize};

pub type Result<T> = std::result::Result<T, AdapterError>;

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
    let mut state = filter_state_for_viewer(raw_state, viewer);
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
    })
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_card_id: Option<String>,
    pub input: PromptInput,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct GameViewDto {
    pub game_id: String,
    pub turn: u32,
    pub step: String,
    pub combat_assignments: Vec<CombatAssignmentDto>,
    pub active_player_id: String,
    pub priority_player_id: String,
    pub players: Vec<PlayerDto>,
    pub battlefield: Vec<CardDto>,
    pub stack: Vec<StackObjectDto>,
    pub game_over: bool,
    pub winner_id: Option<String>,
    #[serde(default)]
    pub conceded_player_ids: Vec<String>,
    pub monarch_id: Option<String>,
    pub initiative_holder_id: Option<String>,
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
    pub is_human: bool,
    pub life: i32,
    pub poison: i32,
    pub hand: Vec<CardDto>,
    pub graveyard: Vec<CardDto>,
    pub exile: Vec<CardDto>,
    pub command_zone: Vec<CardDto>,
    pub library_count: usize,
    pub mana_pool: HashMap<String, i32>,
    pub commander_damage: HashMap<String, i32>,
    pub energy_counters: i32,
    pub radiation_counters: i32,
    pub experience_counters: i32,
    pub ticket_counters: i32,
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
    pub zone_id: String,
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
    pub counters: HashMap<String, i32>,
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
    ReorderCards(ReorderCardsInput),
    DiceRolled(DiceRolledInput),
    GameOver(GameOverInput),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum PromptOutput {
    Pass {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until: Option<PassUntil>,
    },
    Concede,
    RestoreSnapshot {
        checkpoint_id: u64,
    },
    Act {
        action_id: String,
    },
    Pay {
        #[serde(default)]
        auto: bool,
    },
    PayLife,
    Cancel,
    MulliganDecision {
        keep: bool,
    },
    MulliganUseSerumPowder {
        card_id: String,
    },
    MulliganPutBackDecision {
        card_ids: Vec<String>,
    },
    DeclareAttackers {
        assignments: Vec<AttackAssignment>,
    },
    DeclareBlockers {
        assignments: Vec<BlockAssignment>,
    },
    BoardTargets {
        chosen: Vec<TargetRefDto>,
    },
    Decision {
        value: bool,
    },
    ChooseCardsDecision {
        chosen_card_ids: Vec<String>,
    },
    ColorDecision {
        chosen_colors: BTreeMap<String, u32>,
    },
    CombatDamageAssignmentDecision {
        assignments: Vec<CombatDamageAssignmentEntry>,
    },
    DamageAssignmentOrderDecision {
        ordered_blocker_ids: Vec<String>,
    },
    SelectionDecision {
        chosen_indices: Vec<usize>,
    },
    NumberDecision {
        chosen_number: Option<i32>,
    },
    RevealCardsAcknowledged,
    ScryDecision {
        zone_card_ids: Vec<Vec<String>>,
    },
    ReorderDecision {
        ordered_card_ids: Vec<String>,
    },
    DiceRolledAcknowledged,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptResponse {
    pub prompt_id: u32,
    pub output: PromptOutput,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct PromptPresentation {
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_card_id: Option<String>,
    #[serde(default)]
    pub targets: Vec<TargetRefDto>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
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
        mode: String,
        mode_label: String,
    },
    ActivateAbility(ActivatableAbilityInfo),
    UndoMana {
        card_id: String,
    },
    Delve {
        card_id: String,
    },
    Undelve {
        card_id: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AvailableAction {
    pub id: String,
    #[serde(flatten)]
    pub kind: AvailableActionKind,
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
    pub phase: String,
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
    },
    Concede,
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
    pub card_id: String,
    pub card_name: String,
    pub mana_cost: String,
    pub can_confirm_from_pool: bool,
    pub actions: Vec<AvailableAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
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
    PayLife,
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
    pub candidates: Vec<TargetRefDto>,
    #[serde(default)]
    pub hostile: bool,
    pub intent: TargetingIntent,
    pub min_targets: i32,
    pub max_targets: i32,
    pub chosen_targets: i32,
    pub label: String,
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
pub struct ChooseFromSelectionInput {
    pub presentation: PromptPresentation,
    pub options: Vec<String>,
    pub min_choices: usize,
    pub max_choices: usize,
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
    pub cards: Vec<CardDto>,
    pub zone: String,
    pub owner_player_id: String,
    pub message: String,
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
pub struct ReorderCardsInput {
    pub presentation: PromptPresentation,
    pub cards: Vec<CardDto>,
    pub target_label: String,
    pub top_of_deck: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(
    tag = "type",
    rename_all = "camelCase",
    rename_all_fields = "camelCase"
)]
pub enum ReorderCardsOutput {
    ReorderDecision { ordered_card_ids: Vec<String> },
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
    pub sides: i32,
    pub rolls: Vec<DiceRollEntry>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
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

static UNSUPPORTED_PROTOCOL_CAPABILITIES: [UnsupportedCapability; 18] = [
    UnsupportedCapability {
        code: "upstream.response-envelope-mismatch",
        area: "transport",
        reason: "The pinned Rust protocol uses a u32 prompt_id, while generated TypeScript helpers and relay examples use partly divergent response/envelope shapes.",
        suggested_protocol_extension: "Define PromptId = number plus canonical PromptResponse and RelayResponse wrappers.",
    },
    UnsupportedCapability {
        code: "upstream.object-selection-missing",
        area: "prompts",
        reason: "The protocol has TargetRef for rules targets but no generic ObjectRef selection primitive for non-target choices.",
        suggested_protocol_extension: "Add ObjectRef plus ChooseObjectsInput/objectsChosen with a purpose field.",
    },
    UnsupportedCapability {
        code: "upstream.card-destination-workflows-missing",
        area: "prompts",
        reason: "Scry exists, but surveil, dig, discard, reorder, and put-back workflows need structured destination metadata to avoid overloading one shape.",
        suggested_protocol_extension: "Add CardDestination and DistributeCardsInput primitives.",
    },
    UnsupportedCapability {
        code: "upstream.mana-payment-primitives-insufficient",
        area: "mana",
        reason: "Pool entries, restricted mana metadata, and pin/unpin payment choices cannot be represented.",
        suggested_protocol_extension: "Add PoolMana state objects and PaymentAction primitives before pinPoolMana/unpinPoolMana/restricted payments.",
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
        reason: "Room relay, multiplayer envelopes, log envelopes, snapshots, and restore are not implemented by this crate.",
        suggested_protocol_extension: "Negotiate relay capabilities and keep direct PromptResponse separate from relay envelopes.",
    },
    UnsupportedCapability {
        code: "local.prompt-family-display-acks-unsupported",
        area: "prompts",
        reason: "RevealCards and DiceRolled acknowledgements are modeled but not emitted unless Phase has a matching WaitingFor state.",
        suggested_protocol_extension: "Treat acknowledgement prompts as display events with audience and sequencing metadata.",
    },
    UnsupportedCapability {
        code: "local.generic-reorder-unsupported",
        area: "prompts",
        reason: "Generic reorderCards is not emitted because Phase only safely maps the current ScryChoice top/bottom workflow.",
        suggested_protocol_extension: "Use structured destination workflows rather than a single reorder primitive.",
    },
    UnsupportedCapability {
        code: "local.non-target-selection-unsupported",
        area: "prompts",
        reason: "Surveil, dig, discard, keep-with-total-power, optional trigger, cost-prevention, and pay-combat-cost prompts have no exact upstream shape.",
        suggested_protocol_extension: "Add ObjectRef/ChooseObjects and CardDestination workflows.",
    },
    UnsupportedCapability {
        code: "local.legacy-choose-target-card-removed",
        area: "prompts",
        reason: "The old chooseTargetCard adapter shape is intentionally not serialized by the updated protocol.",
        suggested_protocol_extension: "Use chooseBoardTargets with TargetRef candidates for genuine rules targets.",
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
        reason: "Phase requires explicit mana payment finalization here; Manabrew auto-pay responses are not safely represented by the adapter.",
        suggested_protocol_extension: "Add PaymentAction/pool-mana primitives or define auto-pay as a separate engine-planner request.",
    },
    UnsupportedCapability {
        code: "local.legacy-engine-action-unsupported",
        area: "responses",
        reason: "The previous adapter accepted direct engine action ids, but the updated protocol requires prompt-id-correlated PromptResponse payloads.",
        suggested_protocol_extension: "Use canonical PromptResponse for all client decisions and keep engine action tables adapter-private.",
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
        step: phase_step(state.phase).to_string(),
        combat_assignments: combat_assignments(state),
        active_player_id: encode_player_id(state.active_player),
        priority_player_id: encode_player_id(state.priority_player),
        players: state
            .players
            .iter()
            .map(|player| {
                build_player_dto(state, player.id, prepared.viewer, &prepared.derived, &cards)
            })
            .collect::<Result<Vec<_>>>()?,
        battlefield: objects_from_ids(state, &state.battlefield, &cards)?,
        stack: build_stack(state, &prepared.derived),
        game_over,
        winner_id,
        conceded_player_ids: state
            .players
            .iter()
            .filter(|player| player.is_eliminated)
            .map(|player| encode_player_id(player.id))
            .collect(),
        monarch_id: state.monarch.map(encode_player_id),
        initiative_holder_id: state.initiative.map(encode_player_id),
    })
}

pub fn build_prompt(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
    _display_events: Vec<DisplayEvent>,
) -> Result<AgentPrompt> {
    if !turn_control::is_authorized_submitter(&prepared.state, prepared.viewer)
        && !matches!(prepared.state.waiting_for, WaitingFor::GameOver { .. })
    {
        return Err(AdapterError::NoAuthorizedPrompt {
            viewer: prepared.viewer,
        });
    }

    Ok(AgentPrompt {
        prompt_id: prepared.prompt_id,
        deciding_player_id: encode_player_id(prepared.viewer),
        source_card_id: source_card_id(&prepared.state.waiting_for),
        input: build_prompt_input(prepared, card_lookup)?,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum DisplayEvent {
    Unsupported { code: String, message: String },
}

fn build_prompt_input(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<PromptInput> {
    let waiting_for = &prepared.state.waiting_for;
    match waiting_for {
        WaitingFor::Priority { .. } => Ok(PromptInput::ChooseAction(ChooseActionInput {
            actions: available_actions(&prepared.actions),
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
            valid_attacker_ids,
            valid_attack_targets,
            ..
        } => Ok(PromptInput::ChooseAttackers(ChooseAttackersInput {
            attackers: valid_attacker_ids
                .iter()
                .copied()
                .map(|attacker_id| AttackerOptionDto {
                    attacker_id: encode_object_id(attacker_id),
                    valid_target_ids: valid_attack_targets
                        .iter()
                        .map(attack_target_ref_id)
                        .collect(),
                    must_attack: false,
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
                    min_blockers: block_requirements.get(attacker_id).copied().unwrap_or(0),
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
                candidates: target_refs(&slot.legal_targets),
                hostile: false,
                intent: TargetingIntent::Hostile,
                min_targets: if slot.optional { 0 } else { 1 },
                max_targets: 1,
                chosen_targets: 0,
                label: mode_labels
                    .get(current)
                    .and_then(Clone::clone)
                    .unwrap_or_else(|| "Choose target".to_string()),
            }))
        }
        WaitingFor::ManaPayment { .. } => {
            Ok(PromptInput::PayManaCost(pay_mana_cost_input(prepared)))
        }
        WaitingFor::ChooseXValue { min, max, .. } => {
            Ok(PromptInput::ChooseNumber(ChooseNumberInput {
                presentation: presentation("Choose X", source_card_id(waiting_for)),
                min: *min as i32,
                max: *max as i32,
            }))
        }
        WaitingFor::ModeChoice {
            modal,
            unavailable_modes,
            ..
        } => Ok(PromptInput::ChooseFromSelection(ChooseFromSelectionInput {
            presentation: presentation("Choose mode", source_card_id(waiting_for)),
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
                .collect(),
            min_choices: modal.min_choices,
            max_choices: modal.max_choices,
        })),
        WaitingFor::AbilityModeChoice { modal, .. } => {
            Ok(PromptInput::ChooseFromSelection(ChooseFromSelectionInput {
                presentation: presentation("Choose mode", source_card_id(waiting_for)),
                options: modal_options(modal),
                min_choices: modal.min_choices,
                max_choices: modal.max_choices,
            }))
        }
        WaitingFor::ChooseManaColor { choice, .. } => {
            choose_mana_color_input(choice).map(PromptInput::ChooseColor)
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
                presentation: presentation("Scry", source_card_id(waiting_for)),
                cards: object_vec_from_slice(&prepared.state, cards, &ctx)?,
                zones: vec![ScryDestination::LibraryTop, ScryDestination::LibraryBottom],
            }))
        }
        WaitingFor::GameOver { .. } => Ok(PromptInput::GameOver(GameOverInput {})),
        WaitingFor::SurveilChoice { .. } => {
            unsupported_prompt(waiting_for, "local.surveil-unsupported")
        }
        WaitingFor::DigChoice { .. } => unsupported_prompt(waiting_for, "local.dig-unsupported"),
        WaitingFor::DiscardChoice { .. } => {
            unsupported_prompt(waiting_for, "local.discard-unsupported")
        }
        WaitingFor::KeepWithinTotalPowerChoice { .. } => {
            unsupported_prompt(waiting_for, "local.keep-with-total-power-unsupported")
        }
        WaitingFor::KeepExactPermanentsChoice { .. } => {
            unsupported_prompt(waiting_for, "local.keep-exact-permanents-unsupported")
        }
        WaitingFor::OptionalEffectChoice { .. } | WaitingFor::OpponentMayChoice { .. } => {
            unsupported_prompt(waiting_for, "local.optional-trigger-unsupported")
        }
        WaitingFor::UnlessPayment { .. } | WaitingFor::UnlessPaymentChooseCost { .. } => {
            unsupported_prompt(waiting_for, "local.cost-prevention-unsupported")
        }
        WaitingFor::AssignBlockerDamage { .. } => {
            unsupported_prompt(waiting_for, "local.blocker-damage-banding-unsupported")
        }
        WaitingFor::CombatTaxPayment { .. } => {
            unsupported_prompt(waiting_for, "local.pay-combat-cost-unsupported")
        }
        _ => unsupported_prompt(waiting_for, "local.prompt-unsupported"),
    }
}

fn unsupported_prompt<T>(waiting_for: &WaitingFor, code: &'static str) -> Result<T> {
    Err(AdapterError::UnsupportedPrompt {
        waiting_for_type: waiting_for_type(waiting_for),
        code,
    })
}

pub fn translate_response(
    response: PromptResponse,
    context: &PromptContext,
    state: &GameState,
) -> Result<GameAction> {
    if response.prompt_id != context.prompt_id {
        return Err(AdapterError::PromptIdMismatch {
            expected: context.prompt_id,
            actual: response.prompt_id,
        });
    }
    if !turn_control::is_authorized_submitter(state, context.deciding_player)
        && !matches!(state.waiting_for, WaitingFor::GameOver { .. })
    {
        return Err(AdapterError::NoAuthorizedPrompt {
            viewer: context.deciding_player,
        });
    }
    if !response_output_matches_waiting(&response.output, state, context.deciding_player) {
        return Err(AdapterError::IllegalResponseForPrompt {
            response_kind: response_output_type(&response.output),
        });
    }

    match response.output {
        PromptOutput::Pass { until } => {
            translate_choose_action_output(ChooseActionOutput::Pass { until }, context)
        }
        PromptOutput::Concede => {
            translate_choose_action_output(ChooseActionOutput::Concede, context)
        }
        PromptOutput::RestoreSnapshot { checkpoint_id } => translate_choose_action_output(
            ChooseActionOutput::RestoreSnapshot { checkpoint_id },
            context,
        ),
        PromptOutput::Act { action_id } => {
            if matches!(state.waiting_for, WaitingFor::ManaPayment { .. }) {
                translate_pay_mana_output(PayManaCostOutput::Act { action_id }, context)
            } else {
                translate_choose_action_output(ChooseActionOutput::Act { action_id }, context)
            }
        }
        PromptOutput::Pay { auto } => {
            translate_pay_mana_output(PayManaCostOutput::Pay { auto }, context)
        }
        PromptOutput::PayLife => translate_pay_mana_output(PayManaCostOutput::PayLife, context),
        PromptOutput::Cancel => translate_pay_mana_output(PayManaCostOutput::Cancel, context),
        PromptOutput::MulliganDecision { keep } => Ok(GameAction::MulliganDecision {
            choice: if keep {
                engine::types::actions::MulliganChoice::Keep
            } else {
                engine::types::actions::MulliganChoice::Mulligan
            },
        }),
        PromptOutput::MulliganUseSerumPowder { card_id } => Ok(GameAction::MulliganDecision {
            choice: engine::types::actions::MulliganChoice::UseSerumPowder {
                object_id: parse_object_id(&card_id)?,
            },
        }),
        PromptOutput::MulliganPutBackDecision { card_ids } => Ok(GameAction::SelectCards {
            cards: parse_object_ids(&card_ids)?,
        }),
        PromptOutput::DeclareAttackers { assignments } => Ok(GameAction::DeclareAttackers {
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
        }),
        PromptOutput::DeclareBlockers { assignments } => Ok(GameAction::DeclareBlockers {
            assignments: assignments
                .iter()
                .map(|assignment| {
                    Ok((
                        parse_object_id(&assignment.blocker_id)?,
                        parse_object_id(&assignment.attacker_id)?,
                    ))
                })
                .collect::<Result<Vec<_>>>()?,
        }),
        PromptOutput::BoardTargets { chosen } => Ok(GameAction::SelectTargets {
            targets: chosen
                .iter()
                .map(target_ref_from_dto)
                .collect::<Result<Vec<_>>>()?,
        }),
        PromptOutput::NumberDecision {
            chosen_number: Some(value),
        } if value >= 0 => Ok(GameAction::ChooseX {
            value: value as u32,
        }),
        PromptOutput::SelectionDecision { chosen_indices } => Ok(GameAction::SelectModes {
            indices: chosen_indices,
        }),
        PromptOutput::ColorDecision { chosen_colors } => {
            translate_color_decision(&state.waiting_for, chosen_colors)
        }
        PromptOutput::CombatDamageAssignmentDecision { assignments } => {
            Ok(GameAction::AssignCombatDamage {
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
            })
        }
        PromptOutput::ScryDecision { zone_card_ids } => {
            let bottom = zone_card_ids.get(1).cloned().unwrap_or_default();
            Ok(GameAction::SelectCards {
                cards: parse_object_ids(&bottom)?,
            })
        }
        PromptOutput::Decision { .. }
        | PromptOutput::ChooseCardsDecision { .. }
        | PromptOutput::DamageAssignmentOrderDecision { .. }
        | PromptOutput::RevealCardsAcknowledged
        | PromptOutput::ReorderDecision { .. }
        | PromptOutput::DiceRolledAcknowledged
        | PromptOutput::NumberDecision {
            chosen_number: None,
        }
        | PromptOutput::NumberDecision {
            chosen_number: Some(_),
        } => Err(AdapterError::IllegalResponseForPrompt {
            response_kind: "unsupportedOutput",
        }),
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PlayerAction {
    PromptResponse(PromptResponse),
    #[serde(rename = "engineAction")]
    EngineAction {
        action_id: String,
    },
}

pub fn translate_player_action(
    action: PlayerAction,
    context: &PromptContext,
    state: &GameState,
) -> Result<GameAction> {
    match action {
        PlayerAction::PromptResponse(response) => translate_response(response, context, state),
        PlayerAction::EngineAction { .. } => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.legacy-engine-action-unsupported",
        }),
    }
}

pub fn convert_available_action(action: &GameAction, id: String) -> AvailableActionConversion {
    match action {
        GameAction::CastSpell { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, "cast", "Cast"),
        ),
        GameAction::CastSpellForFree { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, "castFree", "Cast for free"),
        ),
        GameAction::CastSpellAsMiracle { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, "miracle", "Cast with miracle"),
        ),
        GameAction::CastSpellAsMadness { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, "madness", "Cast with madness"),
        ),
        GameAction::PlayFaceDown { object_id, .. } => AvailableActionConversion::Available(
            cast_available_action(id, *object_id, "faceDown", "Play face down"),
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
        GameAction::TapLandForMana { object_id } => {
            AvailableActionConversion::Available(AvailableAction {
                id,
                kind: AvailableActionKind::ActivateAbility(ActivatableAbilityInfo {
                    card_id: encode_object_id(*object_id),
                    ability_index: 0,
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
        GameAction::PlayLand { .. } => {
            AvailableActionConversion::Unsupported("upstream.play-land-action-missing")
        }
        GameAction::Foretell { .. } => {
            AvailableActionConversion::Unsupported("local.priority-action-unsupported")
        }
        GameAction::DeclareAttackers { .. } => AvailableActionConversion::Skip,
        GameAction::DeclareBlockers { .. } => AvailableActionConversion::Skip,
        GameAction::ChooseUntap { .. } => {
            AvailableActionConversion::Unsupported("local.choose-untap-unsupported")
        }
        GameAction::ChooseExert { .. } => {
            AvailableActionConversion::Unsupported("local.exert-unsupported")
        }
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
        GameAction::SpendPoolMana { .. } | GameAction::UnspendPoolMana { .. } => {
            AvailableActionConversion::Unsupported("upstream.mana-payment-primitives-insufficient")
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
        GameAction::OrderTriggers { .. } => {
            AvailableActionConversion::Unsupported("local.order-triggers-unsupported")
        }
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
        GameAction::DecideOptionalCost { .. }
        | GameAction::DecideOptionalEffect { .. }
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
        GameAction::ActivateNinjutsu { .. }
        | GameAction::CastSpellAsSneak { .. }
        | GameAction::CastSpellAsWebSlinging { .. } => {
            AvailableActionConversion::Unsupported("local.alternative-combat-cast-unsupported")
        }
        GameAction::RespondToSpliceOffer { .. } => {
            AvailableActionConversion::Unsupported("local.splice-unsupported")
        }
        GameAction::PayUnlessCost { .. } | GameAction::ChooseUnlessCostBranch { .. } => {
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
        GameAction::TapForConvoke { .. } | GameAction::HarmonizeTap { .. } => {
            AvailableActionConversion::Unsupported("local.convoke-harmonize-unsupported")
        }
        GameAction::DeclareCompanion { .. } | GameAction::CompanionToHand => {
            AvailableActionConversion::Unsupported("local.companion-unsupported")
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
        GameAction::SubmitPhyrexianChoices { .. } => {
            AvailableActionConversion::Unsupported("local.phyrexian-payment-unsupported")
        }
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

pub fn encode_object_id(id: ObjectId) -> String {
    format!("card-{}", id.0)
}

pub fn encode_player_id(id: PlayerId) -> String {
    format!("player-{}", id.0)
}

pub fn encode_stack_id(id: ObjectId) -> String {
    format!("spell-{}", id.0)
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
        .strip_prefix("spell-")
        .and_then(|raw| raw.parse::<u64>().ok())
        .map(ObjectId)
        .ok_or_else(|| AdapterError::MalformedId {
            expected_prefix: "spell-",
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

fn phase_step(phase: Phase) -> &'static str {
    match phase {
        Phase::Untap => "untap",
        Phase::Upkeep => "upkeep",
        Phase::Draw => "draw",
        Phase::PreCombatMain => "main1",
        Phase::BeginCombat => "begin_combat",
        Phase::DeclareAttackers => "declare_attackers",
        Phase::DeclareBlockers => "declare_blockers",
        Phase::CombatDamage => "combat_damage",
        Phase::EndCombat => "end_combat",
        Phase::PostCombatMain => "main2",
        Phase::End => "end",
        Phase::Cleanup => "cleanup",
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

fn zone_objects_for_player<L: CardTextLookup>(
    state: &GameState,
    zone: Zone,
    player: PlayerId,
    ctx: &CardBuildContext<'_, L>,
) -> Result<Vec<CardDto>> {
    state
        .objects
        .values()
        .filter(|object| object.zone == zone && object.owner == player)
        .map(|object| build_card_dto(state, object, ctx))
        .collect()
}

fn command_zone_for_player<L: CardTextLookup>(
    state: &GameState,
    player: PlayerId,
    ctx: &CardBuildContext<'_, L>,
) -> Result<Vec<CardDto>> {
    state
        .command_zone
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|object| object.owner == player)
        .map(|object| build_card_dto(state, object, ctx))
        .collect()
}

fn build_card_dto<L: CardTextLookup>(
    state: &GameState,
    object: &GameObject,
    ctx: &CardBuildContext<'_, L>,
) -> Result<CardDto> {
    let redacted = object.name == "Hidden Card" || object.face_down;
    let text = if redacted {
        String::new()
    } else if let Some(text) = &object.token_rules_text {
        text.clone()
    } else {
        ctx.card_lookup
            .text_for(object)
            .ok_or(AdapterError::MissingCardText {
                object_id: object.id,
            })?
    };
    let attack_target = attack_target_id(state, object.id);

    Ok(CardDto {
        id: encode_object_id(object.id),
        identity: CardIdentity {
            name: object.name.clone(),
            set_code: String::new(),
            card_number: String::new(),
            is_token: !redacted && object.is_token,
        },
        color: if redacted {
            String::new()
        } else {
            colors_string(&object.color)
        },
        mana_cost: if redacted {
            String::new()
        } else {
            mana_cost_string(&object.mana_cost)
        },
        cmc: if redacted {
            0
        } else {
            object.mana_cost.mana_value() as i32
        },
        types: if redacted {
            Vec::new()
        } else {
            object
                .card_types
                .core_types
                .iter()
                .map(ToString::to_string)
                .collect()
        },
        subtypes: if redacted {
            Vec::new()
        } else {
            object.card_types.subtypes.clone()
        },
        supertypes: if redacted {
            Vec::new()
        } else {
            object
                .card_types
                .supertypes
                .iter()
                .map(ToString::to_string)
                .collect()
        },
        power: (!redacted)
            .then(|| object.power.map(|value| value.to_string()))
            .flatten(),
        toughness: (!redacted)
            .then(|| object.toughness.map(|value| value.to_string()))
            .flatten(),
        base_power: (!redacted).then_some(object.base_power).flatten(),
        base_toughness: (!redacted).then_some(object.base_toughness).flatten(),
        text,
        controller_id: encode_player_id(object.controller),
        owner_id: encode_player_id(object.owner),
        zone_id: zone_string(object.zone).to_string(),
        tapped: object.tapped,
        is_crewed: false,
        is_attacking: attack_target.is_some(),
        attacking_player_id: attacking_player_id(state, object.id).map(encode_player_id),
        attack_target_id: attack_target,
        keywords: if redacted {
            Vec::new()
        } else {
            object.keywords.iter().map(ToString::to_string).collect()
        },
        counters: if redacted {
            HashMap::new()
        } else {
            object
                .counters
                .iter()
                .map(|(kind, count)| (counter_string(kind), *count as i32))
                .collect()
        },
        damage: if redacted {
            0
        } else {
            object.damage_marked as i32
        },
        summoning_sick: !redacted && object.has_summoning_sickness,
        is_copy: false,
        is_double_faced: !redacted && object.back_face.is_some(),
        is_transformed: !redacted && object.transformed,
        is_face_down: object.face_down,
        is_bestowed: !redacted && object.bestow_form.is_some(),
        phased_out: object.is_phased_out(),
        exerted: !redacted && state.exerted_this_turn.contains(&object.id),
        is_ring_bearer: !redacted
            && state
                .ring_bearer
                .values()
                .any(|bearer| *bearer == Some(object.id)),
        attached_to: (!redacted)
            .then(|| object.attached_to.as_ref().and_then(attach_target_id))
            .flatten(),
        attachment_ids: if redacted {
            Vec::new()
        } else {
            object
                .attachments
                .iter()
                .copied()
                .map(encode_object_id)
                .collect()
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

fn build_player_dto<L: CardTextLookup>(
    state: &GameState,
    player_id: PlayerId,
    viewer: PlayerId,
    derived: &DerivedViews,
    ctx: &CardBuildContext<'_, L>,
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

    Ok(PlayerDto {
        id: encode_player_id(player.id),
        name: state
            .log_player_names
            .get(player.id.0 as usize)
            .filter(|name| !name.is_empty())
            .cloned()
            .unwrap_or_else(|| format!("Player {}", player.id.0)),
        is_human: player.id == viewer,
        life: player.life,
        poison: player.poison_counters as i32,
        hand: objects_from_ids(state, &player.hand, ctx)?,
        graveyard: objects_from_ids(state, &player.graveyard, ctx)?,
        exile: zone_objects_for_player(state, Zone::Exile, player_id, ctx)?,
        command_zone: command_zone_for_player(state, player_id, ctx)?,
        library_count: player.library.len(),
        mana_pool: mana_pool_counts(&player.mana_pool.mana),
        commander_damage,
        energy_counters: player.energy as i32,
        radiation_counters: player.player_counter(&PlayerCounterKind::Rad) as i32,
        experience_counters: player.player_counter(&PlayerCounterKind::Experience) as i32,
        ticket_counters: player.player_counter(&PlayerCounterKind::Ticket) as i32,
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

fn available_actions(actions: &[GameAction]) -> Vec<AvailableAction> {
    actions
        .iter()
        .enumerate()
        .filter_map(
            |(index, action)| match convert_available_action(action, action_id(index)) {
                AvailableActionConversion::Available(action) => Some(action),
                AvailableActionConversion::Skip | AvailableActionConversion::Unsupported(_) => None,
            },
        )
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

fn advertised_action_by_id(context: &PromptContext, action_id: &str) -> Result<GameAction> {
    let entry = context
        .action_table
        .iter()
        .find(|entry| entry.id == action_id)
        .ok_or_else(|| AdapterError::StaleOrInvalidActionId {
            action_id: action_id.to_string(),
        })?;

    match convert_available_action(&entry.action, entry.id.clone()) {
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
    mode: &'static str,
    mode_label: &'static str,
) -> AvailableAction {
    AvailableAction {
        id,
        kind: AvailableActionKind::Cast {
            card_id: encode_object_id(object_id),
            mode: mode.to_string(),
            mode_label: mode_label.to_string(),
        },
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
        card_id,
        card_name,
        mana_cost,
        can_confirm_from_pool: prepared
            .actions
            .iter()
            .any(|action| matches!(action, GameAction::PassPriority)),
        actions: available_actions(&prepared.actions),
        description: None,
    }
}

fn choose_mana_color_input(choice: &ManaChoicePrompt) -> Result<ChooseColorInput> {
    match choice {
        ManaChoicePrompt::SingleColor { options } => Ok(ChooseColorInput {
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

fn response_output_matches_waiting(
    output: &PromptOutput,
    state: &GameState,
    viewer: PlayerId,
) -> bool {
    let waiting_for = &state.waiting_for;
    match output {
        PromptOutput::Pass { .. }
        | PromptOutput::Concede
        | PromptOutput::RestoreSnapshot { .. } => {
            matches!(waiting_for, WaitingFor::Priority { .. })
        }
        PromptOutput::Act { .. } => matches!(
            waiting_for,
            WaitingFor::Priority { .. } | WaitingFor::ManaPayment { .. }
        ),
        PromptOutput::Pay { .. } | PromptOutput::PayLife | PromptOutput::Cancel => {
            matches!(waiting_for, WaitingFor::ManaPayment { .. })
        }
        // A declare-point response (keep/mulligan or use Serum Powder) is only
        // legal while the viewer's own entry is in the `Declare` phase.
        PromptOutput::MulliganDecision { .. } | PromptOutput::MulliganUseSerumPowder { .. } => {
            match waiting_for {
                WaitingFor::MulliganDecision { pending, .. } => {
                    pending_entry_for_viewer(state, viewer, pending)
                        .is_ok_and(|entry| matches!(entry.phase, MulliganDecisionPhase::Declare))
                }
                _ => false,
            }
        }
        // A bottom-cards selection is legal while the viewer's own entry is in
        // the `BottomCards` sub-phase, or during the unrelated
        // `OpeningHandBottomCards` phase.
        PromptOutput::MulliganPutBackDecision { .. } => match waiting_for {
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
        PromptOutput::DeclareAttackers { .. } => {
            matches!(waiting_for, WaitingFor::DeclareAttackers { .. })
        }
        PromptOutput::DeclareBlockers { .. } => {
            matches!(waiting_for, WaitingFor::DeclareBlockers { .. })
        }
        PromptOutput::BoardTargets { .. } => matches!(
            waiting_for,
            WaitingFor::TargetSelection { .. } | WaitingFor::TriggerTargetSelection { .. }
        ),
        PromptOutput::NumberDecision { .. } => {
            matches!(waiting_for, WaitingFor::ChooseXValue { .. })
        }
        PromptOutput::SelectionDecision { .. } => matches!(
            waiting_for,
            WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. }
        ),
        PromptOutput::ColorDecision { .. } => {
            matches!(waiting_for, WaitingFor::ChooseManaColor { .. })
        }
        PromptOutput::CombatDamageAssignmentDecision { .. } => {
            matches!(waiting_for, WaitingFor::AssignCombatDamage { .. })
        }
        PromptOutput::ScryDecision { .. } => {
            matches!(waiting_for, WaitingFor::ScryChoice { .. })
        }
        PromptOutput::Decision { .. }
        | PromptOutput::ChooseCardsDecision { .. }
        | PromptOutput::DamageAssignmentOrderDecision { .. }
        | PromptOutput::RevealCardsAcknowledged
        | PromptOutput::ReorderDecision { .. }
        | PromptOutput::DiceRolledAcknowledged => false,
    }
}

fn response_output_type(output: &PromptOutput) -> &'static str {
    match output {
        PromptOutput::Pass { .. } => "pass",
        PromptOutput::Concede => "concede",
        PromptOutput::RestoreSnapshot { .. } => "restoreSnapshot",
        PromptOutput::Act { .. } => "act",
        PromptOutput::Pay { .. } => "pay",
        PromptOutput::PayLife => "payLife",
        PromptOutput::Cancel => "cancel",
        PromptOutput::MulliganDecision { .. } => "mulliganDecision",
        PromptOutput::MulliganUseSerumPowder { .. } => "mulliganUseSerumPowder",
        PromptOutput::MulliganPutBackDecision { .. } => "mulliganPutBackDecision",
        PromptOutput::DeclareAttackers { .. } => "declareAttackers",
        PromptOutput::DeclareBlockers { .. } => "declareBlockers",
        PromptOutput::BoardTargets { .. } => "boardTargets",
        PromptOutput::Decision { .. } => "decision",
        PromptOutput::ChooseCardsDecision { .. } => "chooseCardsDecision",
        PromptOutput::ColorDecision { .. } => "colorDecision",
        PromptOutput::CombatDamageAssignmentDecision { .. } => "combatDamageAssignmentDecision",
        PromptOutput::DamageAssignmentOrderDecision { .. } => "damageAssignmentOrderDecision",
        PromptOutput::SelectionDecision { .. } => "selectionDecision",
        PromptOutput::NumberDecision { .. } => "numberDecision",
        PromptOutput::RevealCardsAcknowledged => "revealCardsAcknowledged",
        PromptOutput::ScryDecision { .. } => "scryDecision",
        PromptOutput::ReorderDecision { .. } => "reorderDecision",
        PromptOutput::DiceRolledAcknowledged => "diceRolledAcknowledged",
    }
}

fn translate_choose_action_output(
    output: ChooseActionOutput,
    context: &PromptContext,
) -> Result<GameAction> {
    match output {
        ChooseActionOutput::Pass { until: None } => Ok(GameAction::PassPriority),
        ChooseActionOutput::Pass { until: Some(_) } => {
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.pass-until-unsupported",
            })
        }
        ChooseActionOutput::Concede => Ok(GameAction::Concede {
            player_id: context.deciding_player,
        }),
        ChooseActionOutput::RestoreSnapshot { .. } => {
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.room-relay-not-implemented",
            })
        }
        ChooseActionOutput::Act { action_id } => advertised_action_by_id(context, &action_id),
    }
}

fn translate_pay_mana_output(
    output: PayManaCostOutput,
    context: &PromptContext,
) -> Result<GameAction> {
    match output {
        PayManaCostOutput::Act { action_id } => advertised_action_by_id(context, &action_id),
        PayManaCostOutput::Pay { auto: true } => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.auto-pay-unsupported",
        }),
        PayManaCostOutput::Pay { auto: false } => prompt_level_action(
            context,
            |action| matches!(action, GameAction::PassPriority),
            "upstream.mana-payment-primitives-insufficient",
        ),
        PayManaCostOutput::Cancel => prompt_level_action(
            context,
            |action| matches!(action, GameAction::CancelCast),
            "local.cancel-mana-payment-unavailable",
        ),
        PayManaCostOutput::PayLife => Err(AdapterError::UnsupportedProtocolFeature {
            code: "local.phyrexian-payment-unsupported",
        }),
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

fn presentation(title: &'static str, source_card_id: Option<String>) -> PromptPresentation {
    PromptPresentation {
        title: title.to_string(),
        description: None,
        text: None,
        source_card_id,
        targets: Vec::new(),
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

fn mana_pool_counts(units: &[engine::types::mana::ManaUnit]) -> HashMap<String, i32> {
    let mut counts = HashMap::from([
        ("W".to_string(), 0),
        ("U".to_string(), 0),
        ("B".to_string(), 0),
        ("R".to_string(), 0),
        ("G".to_string(), 0),
        ("C".to_string(), 0),
    ]);
    for unit in units {
        *counts
            .entry(mana_type_symbol(unit.color).to_string())
            .or_insert(0) += 1;
    }
    counts
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

fn zone_string(zone: Zone) -> &'static str {
    match zone {
        Zone::Library => "library",
        Zone::Hand => "hand",
        Zone::Battlefield => "battlefield",
        Zone::Graveyard => "graveyard",
        Zone::Stack => "stack",
        Zone::Exile => "exile",
        Zone::Command => "command",
    }
}

fn counter_string(counter: &CounterType) -> String {
    counter.display_phrase().into_owned()
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

fn source_card_id(waiting_for: &WaitingFor) -> Option<String> {
    match waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. }
        | WaitingFor::ModeChoice { pending_cast, .. }
        | WaitingFor::ChooseXValue { pending_cast, .. }
        | WaitingFor::CostTypeChoice { pending_cast, .. } => {
            Some(encode_object_id(pending_cast.object_id))
        }
        WaitingFor::TriggerTargetSelection { source_id, .. } => source_id.map(encode_object_id),
        WaitingFor::OptionalEffectChoice { source_id, .. }
        | WaitingFor::OpponentMayChoice { source_id, .. } => Some(encode_object_id(*source_id)),
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

    use engine::game::zones::create_object;
    use engine::types::ability::{Effect, ResolvedAbility, TargetFilter};
    use engine::types::game_state::{
        MulliganDecisionEntry, MulliganDecisionPhase, PendingCast, PendingMulliganAction,
        TargetSelectionProgress, TargetSelectionSlot,
    };
    use engine::types::identifiers::CardId;
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
        PreparedManabrewSnapshot {
            game_id: "game-a".to_string(),
            viewer: PlayerId(0),
            prompt_id: 42,
            state,
            derived: DerivedViews::default(),
            actions: Vec::new(),
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
        }
    }

    #[test]
    fn id_codecs_roundtrip() {
        assert_eq!(encode_object_id(ObjectId(42)), "card-42");
        assert_eq!(encode_stack_id(ObjectId(42)), "spell-42");
        assert_eq!(parse_object_id("card-42").unwrap(), ObjectId(42));
        assert_eq!(parse_stack_id("spell-42").unwrap(), ObjectId(42));
        assert!(matches!(
            parse_object_id("player-42"),
            Err(AdapterError::MalformedId { .. })
        ));
    }

    #[test]
    fn state_update_and_card_shape_use_current_protocol() {
        let mut state = GameState::new_two_player(7);
        state.players[0].add_player_counters(&PlayerCounterKind::Rad, 2);
        state.players[0].add_player_counters(&PlayerCounterKind::Experience, 3);
        state.players[0].add_player_counters(&PlayerCounterKind::Ticket, 4);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let update = build_state_update(&prepared, &lookup).unwrap();
        let json = serde_json::to_value(update).unwrap();

        assert!(json.get("gameView").is_some());
        assert!(json["gameView"].get("myHand").is_none());
        assert_eq!(
            json["gameView"]["battlefield"][0]["identity"]["name"],
            "Test Creature"
        );
        assert!(json["gameView"]["battlefield"][0]
            .get("isPlayable")
            .is_none());
        assert!(json["gameView"]["battlefield"][0]
            .get("isSelected")
            .is_none());
        assert!(json["gameView"]["battlefield"][0]
            .get("isChoosable")
            .is_none());
        assert_eq!(json["gameView"]["players"][0]["radiationCounters"], 2);
        assert_eq!(json["gameView"]["players"][0]["experienceCounters"], 3);
        assert_eq!(json["gameView"]["players"][0]["ticketCounters"], 4);
    }

    #[test]
    fn prompt_uses_prompt_id_deciding_player_and_input() {
        let prepared = prepared_for(WaitingFor::Priority {
            player: PlayerId(0),
        });

        let prompt = build_prompt(&prepared, &lookup, vec![]).unwrap();
        let json = serde_json::to_value(prompt).unwrap();

        assert_eq!(json["promptId"], 42);
        assert_eq!(json["decidingPlayerId"], "player-0");
        assert_eq!(json["input"]["type"], "chooseAction");
        assert!(json.get("promptType").is_none());
        assert!(json.get("gameView").is_none());
    }

    #[test]
    fn unauthorized_viewer_does_not_receive_prompt() {
        let mut prepared = prepared_for(WaitingFor::Priority {
            player: PlayerId(0),
        });
        prepared.viewer = PlayerId(1);

        assert!(matches!(
            build_prompt(&prepared, &lookup, vec![]),
            Err(AdapterError::NoAuthorizedPrompt {
                viewer: PlayerId(1)
            })
        ));
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
                }],
                mode_labels: Vec::new(),
                selection: TargetSelectionProgress::default(),
            }),
            &lookup,
            vec![],
        )
        .unwrap();

        let json = serde_json::to_value(prompt).unwrap();
        assert_eq!(json["input"]["type"], "chooseBoardTargets");
        assert_eq!(json["input"]["candidates"][0]["kind"], "card");
        assert_eq!(json["input"]["candidates"][1]["kind"], "player");
        assert!(json["input"].get("validCardIds").is_none());
    }

    #[test]
    fn prompt_response_checks_prompt_id_and_action_id() {
        let context = PromptContext {
            prompt_id: 7,
            deciding_player: PlayerId(0),
            action_table: vec![ActionTableEntry {
                id: "action-0".to_string(),
                action: GameAction::CastSpell {
                    object_id: ObjectId(1),
                    card_id: CardId(1),
                    targets: Vec::new(),
                    payment_mode: Default::default(),
                },
            }],
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 8,
                    output: PromptOutput::Pass { until: None },
                },
                &context,
                &state,
            ),
            Err(AdapterError::PromptIdMismatch {
                expected: 7,
                actual: 8
            })
        ));

        let translated = translate_response(
            PromptResponse {
                prompt_id: 7,
                output: PromptOutput::Act {
                    action_id: "action-0".to_string(),
                },
            },
            &context,
            &state,
        )
        .unwrap();
        assert_eq!(
            translated,
            GameAction::CastSpell {
                object_id: ObjectId(1),
                card_id: CardId(1),
                targets: Vec::new(),
                payment_mode: Default::default(),
            }
        );
    }

    #[test]
    fn prompt_response_serializes_docs_style_leaf_output() {
        let response = PromptResponse {
            prompt_id: 7,
            output: PromptOutput::Pass { until: None },
        };

        let json = serde_json::to_value(response).unwrap();

        assert_eq!(
            json,
            serde_json::json!({
                "promptId": 7,
                "output": { "type": "pass" }
            })
        );
        assert!(json["output"].get("output").is_none());
    }

    #[test]
    fn mulligan_and_scry_responses_translate_to_engine_actions() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: Vec::new(),
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::MulliganDecision {
            pending: vec![MulliganDecisionEntry {
                player: PlayerId(0),
                mulligan_count: 0,
                phase: MulliganDecisionPhase::Declare,
            }],
            free_first_mulligan: false,
        };

        let keep = translate_response(
            PromptResponse {
                prompt_id: 1,
                output: PromptOutput::MulliganDecision { keep: true },
            },
            &context,
            &state,
        )
        .unwrap();
        assert!(matches!(
            keep,
            GameAction::MulliganDecision {
                choice: engine::types::actions::MulliganChoice::Keep
            }
        ));

        state.waiting_for = WaitingFor::ScryChoice {
            player: PlayerId(0),
            cards: vec![ObjectId(1), ObjectId(2)],
        };
        let scry = translate_response(
            PromptResponse {
                prompt_id: 1,
                output: PromptOutput::ScryDecision {
                    zone_card_ids: vec![vec!["card-1".to_string()], vec!["card-2".to_string()]],
                },
            },
            &context,
            &state,
        )
        .unwrap();
        assert_eq!(
            scry,
            GameAction::SelectCards {
                cards: vec![ObjectId(2)]
            }
        );
    }

    #[test]
    fn response_family_must_match_current_prompt() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: Vec::new(),
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::MulliganDecision { keep: true },
                },
                &context,
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt {
                response_kind: "mulliganDecision"
            })
        ));
    }

    /// Round-trip (CR 103.5b): a `MulliganUseSerumPowder` response submitted
    /// while the viewer's entry is in the `Declare` phase translates to
    /// `MulliganChoice::UseSerumPowder` carrying the referenced object id.
    #[test]
    fn mulligan_use_serum_powder_response_translates() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: Vec::new(),
        };
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

        let action = translate_response(
            PromptResponse {
                prompt_id: 1,
                output: PromptOutput::MulliganUseSerumPowder {
                    card_id: encode_object_id(powder),
                },
            },
            &context,
            &state,
        )
        .unwrap();
        assert!(matches!(
            action,
            GameAction::MulliganDecision {
                choice: engine::types::actions::MulliganChoice::UseSerumPowder { object_id },
            } if object_id == powder
        ));
    }

    #[test]
    fn unsupported_response_modifiers_are_rejected() {
        let mut context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: vec![ActionTableEntry {
                id: "action-0".to_string(),
                action: GameAction::PassPriority,
            }],
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::Pass {
                        until: Some(PassUntil {
                            player_id: "player-0".to_string(),
                            phase: "main1".to_string(),
                        }),
                    },
                },
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.pass-until-unsupported"
            })
        ));

        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
            convoke_mode: None,
        };
        context.action_table = vec![ActionTableEntry {
            id: "action-0".to_string(),
            action: GameAction::PassPriority,
        }];
        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::Pay { auto: true },
                },
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.auto-pay-unsupported"
            })
        ));
    }

    #[test]
    fn act_response_cannot_execute_unadvertised_unsupported_action() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: vec![ActionTableEntry {
                id: "action-0".to_string(),
                action: GameAction::ChooseKeptCreatures {
                    kept: vec![ObjectId(1)],
                },
            }],
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::Act {
                        action_id: "action-0".to_string(),
                    },
                },
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.non-target-selection-unsupported"
            })
        ));
    }

    #[test]
    fn response_translation_rechecks_authorized_submitter() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: Vec::new(),
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::Pass { until: None },
                },
                &context,
                &state,
            ),
            Err(AdapterError::NoAuthorizedPrompt {
                viewer: PlayerId(0)
            })
        ));
    }

    #[test]
    fn legacy_engine_action_wrapper_is_unsupported() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: vec![ActionTableEntry {
                id: "action-0".to_string(),
                action: GameAction::PassPriority,
            }],
        };
        let state = GameState::new_two_player(7);

        assert!(matches!(
            translate_player_action(
                PlayerAction::EngineAction {
                    action_id: "action-0".to_string(),
                },
                &context,
                &state,
            ),
            Err(AdapterError::UnsupportedProtocolFeature {
                code: "local.legacy-engine-action-unsupported"
            })
        ));
    }

    #[test]
    fn color_response_only_translates_for_mana_color_prompt() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: Vec::new(),
        };
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

        let translated = translate_response(
            PromptResponse {
                prompt_id: 1,
                output: PromptOutput::ColorDecision {
                    chosen_colors: BTreeMap::from([("R".to_string(), 1)]),
                },
            },
            &context,
            &state,
        )
        .unwrap();

        assert_eq!(
            translated,
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
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::ColorDecision {
                        chosen_colors: BTreeMap::from([("R".to_string(), 1)]),
                    },
                },
                &context,
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt {
                response_kind: "colorDecision"
            })
        ));
    }

    #[test]
    fn unsupported_capabilities_are_machine_readable() {
        let codes = unsupported_protocol_capabilities()
            .iter()
            .map(|capability| capability.code)
            .collect::<HashSet<_>>();

        assert!(codes.contains("upstream.response-envelope-mismatch"));
        assert!(codes.contains("upstream.controlled-turn-subject-missing"));
        assert!(codes.contains("local.deck-dto-not-implemented"));
        assert!(codes.contains("local.blocker-damage-banding-unsupported"));
        assert!(codes.contains("local.pass-until-unsupported"));
        assert!(codes.contains("local.auto-pay-unsupported"));
        assert!(codes.contains("local.legacy-engine-action-unsupported"));
        assert!(codes.contains("local.meld-pair-choice-unsupported"));
        assert!(codes.contains("local.entry-attack-target-choice-unsupported"));
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
            vec![],
        );

        assert!(matches!(
            result,
            Err(AdapterError::UnsupportedPrompt {
                code: "local.keep-with-total-power-unsupported",
                ..
            })
        ));
    }

    #[test]
    fn unsupported_actions_are_not_serialized_as_custom_actions() {
        assert!(matches!(
            convert_available_action(
                &GameAction::ChooseKeptCreatures {
                    kept: vec![ObjectId(1)]
                },
                "action-0".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.non-target-selection-unsupported")
        ));

        assert!(available_actions(&[GameAction::ChooseKeptCreatures {
            kept: vec![ObjectId(1)]
        }])
        .is_empty());
    }

    #[test]
    fn meld_actions_return_stable_unsupported_capability_codes() {
        assert!(matches!(
            convert_available_action(
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
                &GameAction::ChooseEntryAttackTarget {
                    target: AttackTarget::Battle(ObjectId(3)),
                },
                "action-1".to_string(),
            ),
            AvailableActionConversion::Unsupported("local.entry-attack-target-choice-unsupported")
        ));
        assert!(
            available_actions(&[
                GameAction::ChooseMeldPair {
                    source_id: ObjectId(1),
                    partner_id: ObjectId(2),
                },
                GameAction::ChooseEntryAttackTarget {
                    target: AttackTarget::Player(PlayerId(1)),
                },
            ])
            .is_empty(),
            "unsupported meld decisions must never be serialized as generic custom actions"
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
            let prompt = build_prompt(&prepared_for(waiting_for), &lookup, vec![]).unwrap();
            let json = serde_json::to_value(prompt).unwrap();
            assert_eq!(json["input"]["type"], expected_type);
        }
    }
}

/// Wire-shape and validation coverage complementary to `mod tests`.
///
/// These focus on the pieces the primary suite leaves implicit: full
/// serialize→deserialize symmetry across every `PromptInput` family,
/// `deny_unknown_fields`/`skip_serializing_if` behaviour, the remaining
/// `translate_response` error arms, `prepare_snapshot`'s player-count guard,
/// and registry / id-codec invariants.
#[cfg(test)]
mod protocol_wire_tests {
    use super::*;
    use std::collections::HashSet;

    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;

    fn lookup(_: &GameObject) -> Option<String> {
        Some("Test oracle text.".to_string())
    }

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
        PreparedManabrewSnapshot {
            game_id: "game-a".to_string(),
            viewer: PlayerId(0),
            prompt_id: 42,
            state,
            derived: DerivedViews::default(),
            actions: Vec::new(),
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
        }
    }

    /// One representative instance of every `PromptInput` variant, paired with
    /// its expected camelCase discriminant tag.
    fn prompt_input_cases() -> Vec<(&'static str, PromptInput)> {
        let card = || CardDto::default();
        vec![
            (
                "chooseAction",
                PromptInput::ChooseAction(ChooseActionInput { actions: vec![] }),
            ),
            (
                "payManaCost",
                PromptInput::PayManaCost(PayManaCostInput {
                    card_id: "card-1".to_string(),
                    card_name: "Lightning Bolt".to_string(),
                    mana_cost: "{R}".to_string(),
                    can_confirm_from_pool: true,
                    actions: vec![],
                    description: Some("Pay for the spell".to_string()),
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
                    label: "Choose target".to_string(),
                }),
            ),
            (
                "chooseBoolean",
                PromptInput::ChooseBoolean(ChooseBooleanInput {
                    presentation: presentation("Question", None),
                    confirm_label: "Yes".to_string(),
                    deny_label: "No".to_string(),
                }),
            ),
            (
                "chooseCards",
                PromptInput::ChooseCards(ChooseCardsInput {
                    presentation: presentation("Pick cards", None),
                    cards: vec![card()],
                    min: 0,
                    max: 1,
                }),
            ),
            (
                "chooseColor",
                PromptInput::ChooseColor(ChooseColorInput {
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
                    presentation: presentation("Choose mode", None),
                    options: vec!["Mode A".to_string(), "Mode B".to_string()],
                    min_choices: 1,
                    max_choices: 1,
                }),
            ),
            (
                "chooseNumber",
                PromptInput::ChooseNumber(ChooseNumberInput {
                    presentation: presentation("Choose X", None),
                    min: 0,
                    max: 3,
                }),
            ),
            (
                "revealCards",
                PromptInput::RevealCards(RevealCardsInput {
                    cards: vec![card()],
                    zone: "hand".to_string(),
                    owner_player_id: "player-0".to_string(),
                    message: "Revealed cards".to_string(),
                }),
            ),
            (
                "scry",
                PromptInput::Scry(ScryInput {
                    presentation: presentation("Scry", None),
                    cards: vec![card()],
                    zones: vec![ScryDestination::LibraryTop, ScryDestination::LibraryBottom],
                }),
            ),
            (
                "reorderCards",
                PromptInput::ReorderCards(ReorderCardsInput {
                    presentation: presentation("Reorder", None),
                    cards: vec![card()],
                    target_label: "library".to_string(),
                    top_of_deck: true,
                }),
            ),
            (
                "diceRolled",
                PromptInput::DiceRolled(DiceRolledInput {
                    sides: 6,
                    rolls: vec![DiceRollEntry {
                        label: Some("d6".to_string()),
                        player_id: Some("player-0".to_string()),
                        natural_results: vec![4],
                        final_results: vec![4],
                        ignored_rolls: vec![],
                        highlighted: false,
                    }],
                    title: Some("Roll".to_string()),
                    source_card_name: None,
                }),
            ),
            ("gameOver", PromptInput::GameOver(GameOverInput {})),
        ]
    }

    #[test]
    fn every_prompt_input_family_round_trips_with_camel_case_tag() {
        let cases = prompt_input_cases();

        // Sanity: every variant is represented exactly once, tags are distinct.
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
            card_id: "card-1".to_string(),
            card_name: "Bolt".to_string(),
            mana_cost: "{R}".to_string(),
            can_confirm_from_pool: true,
            actions: vec![],
            description: None,
        }))
        .unwrap();
        assert_eq!(value["cardId"], "card-1");
        assert_eq!(value["cardName"], "Bolt");
        assert_eq!(value["manaCost"], "{R}");
        assert_eq!(value["canConfirmFromPool"], true);
        // `description` is None -> skip_serializing_if drops it entirely.
        assert!(value.get("description").is_none());

        let targets =
            serde_json::to_value(PromptInput::ChooseBoardTargets(ChooseBoardTargetsInput {
                candidates: vec![],
                hostile: false,
                intent: TargetingIntent::Damage,
                min_targets: 1,
                max_targets: 2,
                chosen_targets: 0,
                label: "Choose".to_string(),
            }))
            .unwrap();
        assert_eq!(targets["minTargets"], 1);
        assert_eq!(targets["maxTargets"], 2);
        assert_eq!(targets["chosenTargets"], 0);
        assert_eq!(targets["intent"], "damage");
    }

    #[test]
    fn state_update_round_trips_and_rejects_unknown_fields() {
        let state = GameState::new_two_player(7);
        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let update = build_state_update(&prepared, &lookup).unwrap();

        let mut value = serde_json::to_value(&update).unwrap();
        let back: StateUpdate = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back, update);

        // deny_unknown_fields: an extra top-level key must be rejected.
        value
            .as_object_mut()
            .unwrap()
            .insert("bogusField".to_string(), serde_json::json!(1));
        assert!(serde_json::from_value::<StateUpdate>(value).is_err());
    }

    #[test]
    fn agent_prompt_round_trips_and_rejects_unknown_fields() {
        let prepared = prepared_for(WaitingFor::Priority {
            player: PlayerId(0),
        });
        let prompt = build_prompt(&prepared, &lookup, vec![]).unwrap();

        let mut value = serde_json::to_value(&prompt).unwrap();
        let back: AgentPrompt = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(back, prompt);

        value
            .as_object_mut()
            .unwrap()
            .insert("bogusField".to_string(), serde_json::json!(true));
        assert!(serde_json::from_value::<AgentPrompt>(value).is_err());
    }

    #[test]
    fn default_card_dto_omits_optional_fields_and_round_trips() {
        let card = CardDto::default();
        let value = serde_json::to_value(&card).unwrap();
        let object = value.as_object().unwrap();

        // Every field guarded by skip_serializing_if is absent at its default.
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

        // The trimmed wire form still deserializes back to an identical value.
        let back: CardDto = serde_json::from_value(value).unwrap();
        assert_eq!(back, card);
    }

    #[test]
    fn act_with_unknown_action_id_is_stale_or_invalid() {
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: vec![ActionTableEntry {
                id: "action-0".to_string(),
                action: GameAction::CastSpell {
                    object_id: ObjectId(1),
                    card_id: CardId(1),
                    targets: Vec::new(),
                    payment_mode: Default::default(),
                },
            }],
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::Act {
                        action_id: "action-99".to_string(),
                    },
                },
                &context,
                &state,
            ),
            Err(AdapterError::StaleOrInvalidActionId { action_id }) if action_id == "action-99"
        ));
    }

    #[test]
    fn act_on_advertised_prompt_level_action_is_illegal() {
        // `PassPriority` is advertised in the table but converts to `Skip`
        // (a prompt-level control, not an actionable board move), so an
        // explicit `act` referencing it must be rejected.
        let context = PromptContext {
            prompt_id: 1,
            deciding_player: PlayerId(0),
            action_table: vec![ActionTableEntry {
                id: "action-0".to_string(),
                action: GameAction::PassPriority,
            }],
        };
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        assert!(matches!(
            translate_response(
                PromptResponse {
                    prompt_id: 1,
                    output: PromptOutput::Act {
                        action_id: "action-0".to_string(),
                    },
                },
                &context,
                &state,
            ),
            Err(AdapterError::IllegalResponseForPrompt {
                response_kind: "act"
            })
        ));
    }

    #[test]
    fn build_prompt_maps_mana_payment_and_scry_families() {
        let mana = build_prompt(
            &prepared_for(WaitingFor::ManaPayment {
                player: PlayerId(0),
                convoke_mode: None,
            }),
            &lookup,
            vec![],
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(mana).unwrap()["input"]["type"],
            "payManaCost"
        );

        let scry = build_prompt(
            &prepared_for(WaitingFor::ScryChoice {
                player: PlayerId(0),
                cards: vec![],
            }),
            &lookup,
            vec![],
        )
        .unwrap();
        let scry_json = serde_json::to_value(scry).unwrap();
        assert_eq!(scry_json["input"]["type"], "scry");
        assert_eq!(scry_json["input"]["zones"][0], "libraryTop");
        assert_eq!(scry_json["input"]["zones"][1], "libraryBottom");
    }

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

    #[test]
    fn unsupported_capability_registry_is_well_formed() {
        let capabilities = unsupported_protocol_capabilities();
        assert_eq!(capabilities.len(), 18);

        let codes: HashSet<_> = capabilities
            .iter()
            .map(|capability| capability.code)
            .collect();
        assert_eq!(codes.len(), 18, "capability codes must be unique");

        for capability in capabilities {
            assert!(
                capability.code.starts_with("upstream.") || capability.code.starts_with("local."),
                "code `{}` must be namespaced upstream./local.",
                capability.code
            );
        }
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
            }) => assert_eq!(expected_prefix, "spell-"),
            other => panic!("expected MalformedId, got {other:?}"),
        }

        // Correct prefix, non-numeric payload is still malformed.
        assert!(matches!(
            parse_object_id("card-abc"),
            Err(AdapterError::MalformedId {
                expected_prefix: "card-",
                ..
            })
        ));
    }
}

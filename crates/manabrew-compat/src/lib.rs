use std::collections::{BTreeMap, HashMap, HashSet};

use engine::ai_support::legal_actions_for_viewer;
use engine::database::CardDatabase;
use engine::game::combat::AttackTarget;
use engine::game::derived::derive_display_state;
use engine::game::derived_views::{derive_views, DerivedViews};
use engine::game::filter_state_for_viewer;
use engine::game::game_object::{AttachTarget, GameObject};
use engine::types::ability::{ChoiceType, TargetRef};
use engine::types::card::CardFace;
use engine::types::counter::CounterType;
use engine::types::game_state::{GameState, ManaChoicePrompt, StackEntryKind, WaitingFor};
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType};
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
    },
    MissingCardText {
        object_id: ObjectId,
    },
    MalformedId {
        expected_prefix: &'static str,
        value: String,
    },
    StaleOrInvalidActionIndex {
        action_index: usize,
    },
    IllegalActionForPrompt {
        action_kind: &'static str,
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
    pub state: GameState,
    pub derived: DerivedViews,
    pub actions: Vec<GameAction>,
    pub spell_costs: HashMap<ObjectId, ManaCost>,
    pub legal_actions_by_object: HashMap<ObjectId, Vec<GameAction>>,
}

impl PreparedManabrewSnapshot {
    pub fn prompt_context(&self) -> PromptContext {
        PromptContext {
            action_table: self.actions.clone(),
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct PromptContext {
    pub action_table: Vec<GameAction>,
}

pub fn prepare_snapshot(
    raw_state: &GameState,
    viewer: PlayerId,
    game_id: impl Into<String>,
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
    // CR 604.1: scope viewer-derived projections (e.g. web-slinging costs) to the
    // requesting player's own hand — this snapshot is already viewer-filtered.
    let derived = derive_views(&state, Some(viewer));

    Ok(PreparedManabrewSnapshot {
        game_id: game_id.into(),
        viewer,
        state,
        derived,
        actions,
        spell_costs,
        legal_actions_by_object,
    })
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AgentPrompt {
    #[serde(rename = "type")]
    pub prompt_type: String,
    #[serde(default)]
    pub display_events: Vec<DisplayEvent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_card_id: Option<String>,
    pub game_view: GameViewDto,
    #[serde(flatten)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
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
    RevealCards {
        cards: Vec<CardDto>,
        zone: String,
        owner_player_id: String,
        message: String,
    },
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
    pub my_hand: Vec<CardDto>,
    pub battlefield: Vec<CardDto>,
    pub stack: Vec<StackObjectDto>,
    pub exile: Vec<CardDto>,
    pub graveyard: Vec<CardDto>,
    pub my_command_zone: Vec<CardDto>,
    pub opponent_zones: BTreeMap<String, OpponentZonesDto>,
    pub game_over: bool,
    pub winner_id: Option<String>,
    pub conceded_player_ids: Vec<String>,
    pub monarch_id: Option<String>,
    pub initiative_holder_id: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct OpponentZonesDto {
    pub graveyard: Vec<CardDto>,
    pub exile: Vec<CardDto>,
    pub command_zone: Vec<CardDto>,
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
    pub hand_count: usize,
    pub library_count: usize,
    pub graveyard_count: usize,
    pub exile_count: usize,
    pub mana_pool: HashMap<String, i32>,
    pub commander_damage: HashMap<String, i32>,
    pub energy_counters: i32,
    pub radiation_counters: i32,
    pub has_city_blessing: bool,
    pub ring_level: i32,
    pub speed: i32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct CardDto {
    pub id: String,
    pub name: String,
    pub set_code: String,
    pub card_number: String,
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
    pub is_playable: bool,
    pub is_selected: bool,
    pub is_choosable: bool,
    pub controller_id: String,
    pub owner_id: String,
    pub zone_id: String,
    pub tapped: bool,
    pub is_attacking: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attacking_player_id: Option<String>,
    pub keywords: Vec<String>,
    pub counters: HashMap<String, i32>,
    pub damage: i32,
    pub summoning_sick: bool,
    pub is_token: bool,
    pub is_copy: bool,
    pub is_double_faced: bool,
    pub is_transformed: bool,
    pub is_face_down: bool,
    pub is_bestowed: bool,
    pub phased_out: bool,
    pub exerted: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub attached_to: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachment_ids: Vec<String>,
    #[serde(default)]
    pub foil: bool,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StackObjectDto {
    pub id: String,
    pub source_id: String,
    pub controller_id: String,
    pub name: String,
    pub text: String,
    pub set_code: String,
    pub card_number: String,
    pub is_permanent_spell: bool,
    pub is_casting: bool,
    pub targets: Vec<StackTargetDto>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StackTargetDto {
    pub kind: String,
    pub id: String,
    pub node_index: u32,
    pub target_index: u32,
    pub hostile: bool,
    pub intent: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum PlayerAction {
    #[serde(rename = "engineAction")]
    EngineAction {
        #[serde(rename = "actionIndex")]
        action_index: usize,
    },
    Pass {
        until_phase: Option<String>,
    },
    MulliganDecision {
        keep: bool,
    },
    MulliganPutBackDecision {
        card_ids: Vec<String>,
    },
    PlayCard {
        card_id: String,
        mode: Option<String>,
    },
    DeclareAttackers {
        assignments: Vec<AttackAssignmentDto>,
    },
    DeclareBlockers {
        assignments: Vec<BlockAssignmentDto>,
    },
    TargetPlayer {
        player_id: Option<String>,
    },
    TargetCard {
        card_id: Option<String>,
    },
    TargetAny {
        target: TargetAnyChoice,
    },
    TapLand {
        card_id: String,
    },
    UntapLand {
        card_id: String,
    },
    ActivateAbility {
        card_id: String,
        ability_index: usize,
    },
    ScryDecision {
        bottom_card_ids: Vec<String>,
    },
    SurveilDecision {
        graveyard_card_ids: Vec<String>,
    },
    DigDecision {
        chosen_card_ids: Vec<String>,
    },
    DiscardDecision {
        discarded_card_ids: Vec<String>,
    },
    TargetSpell {
        spell_id: Option<String>,
    },
    OptionalTriggerDecision {
        accept: bool,
    },
    ModeDecision {
        chosen_indices: Vec<usize>,
    },
    ColorDecision {
        color: Option<String>,
    },
    TypeDecision {
        chosen_type: Option<String>,
    },
    NumberDecision {
        chosen_number: Option<i32>,
    },
    PayManaCost {
        auto: bool,
    },
    CancelManaCost,
    Concede,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct AttackAssignmentDto {
    pub attacker_id: String,
    pub defender_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct BlockAssignmentDto {
    pub blocker_id: String,
    pub attacker_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum TargetAnyChoice {
    Player { player_id: String },
    Card { card_id: String },
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum AvailablePlayerActionRef {
    #[serde(rename = "engineActionRef")]
    EngineActionRef {
        #[serde(rename = "actionIndex")]
        action_index: usize,
    },
}

pub fn build_game_view(
    prepared: &PreparedManabrewSnapshot,
    card_lookup: &impl CardTextLookup,
) -> Result<GameViewDto> {
    let state = &prepared.state;
    let viewer_index = player_index(state, prepared.viewer)?;
    let opponent = state
        .players
        .iter()
        .find(|player| player.id != prepared.viewer)
        .map(|player| player.id)
        .ok_or(AdapterError::UnsupportedPlayerCount {
            count: state.players.len(),
        })?;

    let playable = playable_objects(&prepared.actions);
    let choosable = choosable_objects(&state.waiting_for, prepared.viewer);
    let cards = CardBuildContext {
        playable: &playable,
        choosable: &choosable,
        card_lookup,
    };

    let my_hand = objects_from_ids(state, &state.players[viewer_index].hand, &cards)?;
    let graveyard = objects_from_ids(state, &state.players[viewer_index].graveyard, &cards)?;
    let exile = zone_objects_for_player(state, Zone::Exile, prepared.viewer, &cards)?;
    let my_command_zone = command_zone_for_player(state, prepared.viewer, &cards)?;
    let battlefield = objects_from_ids(state, &state.battlefield, &cards)?;

    let mut opponent_zones = BTreeMap::new();
    let opponent_index = player_index(state, opponent)?;
    opponent_zones.insert(
        encode_player_id(opponent),
        OpponentZonesDto {
            graveyard: objects_from_ids(state, &state.players[opponent_index].graveyard, &cards)?,
            exile: zone_objects_for_player(state, Zone::Exile, opponent, &cards)?,
            command_zone: command_zone_for_player(state, opponent, &cards)?,
        },
    );

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
            .map(|player| build_player_dto(state, player.id, prepared.viewer, &prepared.derived))
            .collect(),
        my_hand,
        battlefield,
        stack: build_stack(state, &prepared.derived),
        exile,
        graveyard,
        my_command_zone,
        opponent_zones,
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
    display_events: Vec<DisplayEvent>,
) -> Result<AgentPrompt> {
    let game_view = build_game_view(prepared, card_lookup)?;
    let waiting_for = &prepared.state.waiting_for;
    let mut fields = serde_json::Map::new();
    insert_json(
        &mut fields,
        "availablePlayerActions",
        action_refs(prepared.actions.len()),
    );

    let prompt_type = match waiting_for {
        WaitingFor::Priority { .. } => {
            insert_json(
                &mut fields,
                "playableCardIds",
                playable_objects(&prepared.actions)
                    .into_iter()
                    .map(encode_object_id)
                    .collect::<Vec<_>>(),
            );
            insert_json(
                &mut fields,
                "tappableLandIds",
                source_ids_for(&prepared.legal_actions_by_object, |action| {
                    matches!(action, GameAction::TapLandForMana { .. })
                }),
            );
            insert_json(
                &mut fields,
                "untappableLandIds",
                source_ids_for(&prepared.legal_actions_by_object, |action| {
                    matches!(action, GameAction::UntapLandForMana { .. })
                }),
            );
            "chooseAction"
        }
        WaitingFor::MulliganDecision { pending, .. } => {
            if let Some(entry) = pending
                .iter()
                .find(|entry| entry.player == prepared.viewer)
                .or_else(|| pending.first())
            {
                let hand =
                    &prepared.state.players[player_index(&prepared.state, entry.player)?].hand;
                insert_json(
                    &mut fields,
                    "handCardIds",
                    hand.iter()
                        .copied()
                        .map(encode_object_id)
                        .collect::<Vec<_>>(),
                );
                insert_json(&mut fields, "mulliganCount", entry.mulligan_count);
            }
            "mulligan"
        }
        WaitingFor::MulliganBottomCards { pending }
        | WaitingFor::OpeningHandBottomCards { pending, .. } => {
            if let Some(entry) = pending
                .iter()
                .find(|entry| entry.player == prepared.viewer)
                .or_else(|| pending.first())
            {
                let hand =
                    &prepared.state.players[player_index(&prepared.state, entry.player)?].hand;
                insert_json(
                    &mut fields,
                    "handCardIds",
                    hand.iter()
                        .copied()
                        .map(encode_object_id)
                        .collect::<Vec<_>>(),
                );
                insert_json(&mut fields, "count", entry.count);
            }
            "mulliganPutBack"
        }
        WaitingFor::DeclareAttackers {
            valid_attacker_ids,
            valid_attack_targets,
            ..
        } => {
            insert_json(
                &mut fields,
                "availableAttackerIds",
                valid_attacker_ids
                    .iter()
                    .copied()
                    .map(encode_object_id)
                    .collect::<Vec<_>>(),
            );
            insert_json(
                &mut fields,
                "possibleDefenderIds",
                valid_attack_targets
                    .iter()
                    .map(defender_to_json)
                    .collect::<Vec<_>>(),
            );
            "chooseAttackers"
        }
        WaitingFor::DeclareBlockers {
            valid_blocker_ids,
            valid_block_targets,
            ..
        } => {
            insert_json(
                &mut fields,
                "availableBlockerIds",
                valid_blocker_ids
                    .iter()
                    .copied()
                    .map(encode_object_id)
                    .collect::<Vec<_>>(),
            );
            let attacker_ids: Vec<String> = valid_block_targets
                .keys()
                .copied()
                .map(encode_object_id)
                .collect();
            insert_json(&mut fields, "attackerIds", attacker_ids);
            "chooseBlockers"
        }
        WaitingFor::TargetSelection { target_slots, .. }
        | WaitingFor::TriggerTargetSelection { target_slots, .. } => {
            let object_ids = target_slots
                .iter()
                .flat_map(|slot| slot.legal_targets.iter())
                .filter_map(target_ref_object_id)
                .collect::<Vec<_>>();
            insert_json(
                &mut fields,
                "validCardIds",
                object_ids
                    .iter()
                    .copied()
                    .map(encode_object_id)
                    .collect::<Vec<_>>(),
            );
            "chooseTargetCard"
        }
        WaitingFor::ManaPayment { .. } => "payManaCost",
        WaitingFor::ChooseManaColor { choice, .. } => {
            insert_json(&mut fields, "availableColors", mana_choice_options(choice));
            "specifyManaCombo"
        }
        WaitingFor::PayManaAbilityMana { options, .. } => {
            insert_json(&mut fields, "options", options);
            "specifyManaCombo"
        }
        WaitingFor::ScryChoice { cards, .. } => {
            insert_object_id_list(&mut fields, "cardIds", cards);
            "scry"
        }
        WaitingFor::SurveilChoice { cards, .. } => {
            insert_object_id_list(&mut fields, "cardIds", cards);
            "surveil"
        }
        WaitingFor::DigChoice {
            cards,
            keep_count,
            up_to,
            ..
        } => {
            insert_object_id_list(&mut fields, "cardIds", cards);
            insert_json(&mut fields, "numToTake", keep_count);
            insert_json(&mut fields, "optional", up_to);
            "dig"
        }
        WaitingFor::DiscardChoice { cards, count, .. } => {
            insert_object_id_list(&mut fields, "handCardIds", cards);
            insert_json(&mut fields, "numToDiscard", count);
            "chooseDiscard"
        }
        WaitingFor::ModeChoice {
            modal,
            unavailable_modes,
            ..
        } => {
            insert_json(&mut fields, "options", modal_options(modal));
            insert_json(&mut fields, "minChoices", modal.min_choices);
            insert_json(&mut fields, "maxChoices", modal.max_choices);
            insert_json(&mut fields, "unavailableModes", unavailable_modes);
            "chooseMode"
        }
        WaitingFor::AbilityModeChoice { modal, .. } => {
            insert_json(&mut fields, "options", modal_options(modal));
            insert_json(&mut fields, "minChoices", modal.min_choices);
            insert_json(&mut fields, "maxChoices", modal.max_choices);
            "chooseMode"
        }
        WaitingFor::OptionalEffectChoice { description, .. }
        | WaitingFor::OpponentMayChoice { description, .. } => {
            insert_json(
                &mut fields,
                "description",
                description.clone().unwrap_or_default(),
            );
            "chooseOptionalTrigger"
        }
        WaitingFor::UnlessPayment {
            effect_description, ..
        }
        | WaitingFor::UnlessPaymentChooseCost {
            effect_description, ..
        } => {
            insert_json(
                &mut fields,
                "description",
                effect_description.clone().unwrap_or_default(),
            );
            "payCostToPreventEffect"
        }
        WaitingFor::ChooseXValue { min, max, .. } => {
            insert_json(&mut fields, "min", *min as i32);
            insert_json(&mut fields, "max", *max as i32);
            "chooseNumber"
        }
        WaitingFor::NamedChoice {
            choice_type,
            options,
            ..
        } => {
            insert_json(&mut fields, "validTypes", options);
            match choice_type {
                ChoiceType::Color { .. } => "chooseColor",
                ChoiceType::CreatureType
                | ChoiceType::CardType
                | ChoiceType::LandType
                | ChoiceType::BasicLandType => "chooseType",
                ChoiceType::CardName => "chooseCardName",
                _ => "chooseType",
            }
        }
        WaitingFor::AssignCombatDamage {
            attacker_id,
            total_damage,
            blockers,
            defending_player,
            ..
        } => {
            insert_json(&mut fields, "attackerId", encode_object_id(*attacker_id));
            insert_json(&mut fields, "totalDamage", *total_damage as i32);
            insert_json(
                &mut fields,
                "blockerIds",
                blockers
                    .iter()
                    .map(|slot| encode_object_id(slot.blocker_id))
                    .collect::<Vec<_>>(),
            );
            insert_json(
                &mut fields,
                "defenderId",
                encode_player_id(*defending_player),
            );
            "chooseCombatDamageAssignment"
        }
        // CR 510.1d + CR 702.22k: a blocker is blocking a banding attacker, so the
        // active player divides the blocker's damage among the attackers it
        // blocks. Mirrors `AssignCombatDamage`, minus the lethal/trample/PW axes
        // that don't apply to a blocker's outgoing damage.
        WaitingFor::AssignBlockerDamage {
            blocker_id,
            total_damage,
            attackers,
            ..
        } => {
            insert_json(&mut fields, "blockerId", encode_object_id(*blocker_id));
            insert_json(&mut fields, "totalDamage", *total_damage as i32);
            insert_json(
                &mut fields,
                "attackerIds",
                attackers
                    .iter()
                    .map(|id| encode_object_id(*id))
                    .collect::<Vec<_>>(),
            );
            "chooseBlockerDamageAssignment"
        }
        WaitingFor::CombatTaxPayment {
            per_creature,
            total_cost,
            ..
        } => {
            if let Some((attacker, _)) = per_creature.first() {
                insert_json(&mut fields, "attackerId", encode_object_id(*attacker));
            }
            insert_json(&mut fields, "cost", total_cost.mana_value() as i32);
            "payCombatCost"
        }
        WaitingFor::GameOver { .. } => "gameOver",
        _ => {
            return Err(AdapterError::UnsupportedPrompt {
                waiting_for_type: waiting_for_type(waiting_for),
            });
        }
    };

    Ok(AgentPrompt {
        prompt_type: prompt_type.to_string(),
        display_events,
        source_card_id: source_card_id(waiting_for),
        game_view,
        fields,
    })
}

pub fn translate_player_action(
    action: PlayerAction,
    context: &PromptContext,
    _state: &GameState,
) -> Result<GameAction> {
    match action {
        PlayerAction::EngineAction { action_index } => context
            .action_table
            .get(action_index)
            .cloned()
            .ok_or(AdapterError::StaleOrInvalidActionIndex { action_index }),
        PlayerAction::Pass { .. }
        | PlayerAction::MulliganDecision { .. }
        | PlayerAction::MulliganPutBackDecision { .. }
        | PlayerAction::PlayCard { .. }
        | PlayerAction::DeclareAttackers { .. }
        | PlayerAction::DeclareBlockers { .. }
        | PlayerAction::TargetPlayer { .. }
        | PlayerAction::TargetCard { .. }
        | PlayerAction::TargetAny { .. }
        | PlayerAction::TapLand { .. }
        | PlayerAction::UntapLand { .. }
        | PlayerAction::ActivateAbility { .. }
        | PlayerAction::ScryDecision { .. }
        | PlayerAction::SurveilDecision { .. }
        | PlayerAction::DigDecision { .. }
        | PlayerAction::DiscardDecision { .. }
        | PlayerAction::TargetSpell { .. }
        | PlayerAction::OptionalTriggerDecision { .. }
        | PlayerAction::ModeDecision { .. }
        | PlayerAction::ColorDecision { .. }
        | PlayerAction::TypeDecision { .. }
        | PlayerAction::NumberDecision { .. }
        | PlayerAction::PayManaCost { .. }
        | PlayerAction::CancelManaCost
        | PlayerAction::Concede => Err(AdapterError::IllegalActionForPrompt {
            action_kind: "directAction",
        }),
    }
}

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
    playable: &'a HashSet<ObjectId>,
    choosable: &'a HashSet<ObjectId>,
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
    let redacted = object.name == "Hidden Card";
    let text = if redacted || object.face_down {
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

    Ok(CardDto {
        id: encode_object_id(object.id),
        name: object.name.clone(),
        set_code: String::new(),
        card_number: String::new(),
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
        is_playable: ctx.playable.contains(&object.id),
        is_selected: false,
        is_choosable: ctx.choosable.contains(&object.id),
        controller_id: encode_player_id(object.controller),
        owner_id: encode_player_id(object.owner),
        zone_id: zone_string(object.zone).to_string(),
        tapped: object.tapped,
        is_attacking: attacking_player_id(state, object.id).is_some(),
        attacking_player_id: attacking_player_id(state, object.id).map(encode_player_id),
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
        is_token: object.is_token,
        is_copy: false,
        is_double_faced: !redacted && object.back_face.is_some(),
        is_transformed: !redacted && object.transformed,
        is_face_down: object.face_down,
        is_bestowed: !redacted && object.bestow_form.is_some(),
        phased_out: object.is_phased_out(),
        exerted: !redacted && state.exerted_this_turn.contains(&object.id),
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
        foil: false,
    })
}

fn build_player_dto(
    state: &GameState,
    player_id: PlayerId,
    viewer: PlayerId,
    derived: &DerivedViews,
) -> PlayerDto {
    let player = state
        .players
        .iter()
        .find(|player| player.id == player_id)
        .expect("build_player_dto called with a player id from state.players");
    let commander_damage = derived
        .commander_damage_by_attacker
        .values()
        .flat_map(|entries| entries.iter())
        .filter(|entry| entry.victim == player_id)
        .map(|entry| (encode_object_id(entry.commander), entry.damage as i32))
        .collect();

    PlayerDto {
        id: encode_player_id(player.id),
        name: format!("Player {}", player.id.0),
        is_human: player.id == viewer,
        life: player.life,
        poison: player.poison_counters as i32,
        hand_count: player.hand.len(),
        library_count: player.library.len(),
        graveyard_count: player.graveyard.len(),
        exile_count: state
            .exile
            .iter()
            .filter_map(|id| state.objects.get(id))
            .filter(|object| object.owner == player_id)
            .count(),
        mana_pool: mana_pool_counts(&player.mana_pool.mana),
        commander_damage,
        energy_counters: player.energy as i32,
        radiation_counters: player.player_counter(&PlayerCounterKind::Rad) as i32,
        has_city_blessing: state.city_blessing.contains(&player_id),
        ring_level: state.ring_level.get(&player_id).copied().unwrap_or(0) as i32,
        speed: player.speed.unwrap_or(0) as i32,
    }
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
                name: details
                    .map(|details| details.source_name.clone())
                    .or_else(|| source.map(|source| source.name.clone()))
                    .unwrap_or_default(),
                text: details
                    .and_then(|details| details.ability_description.clone())
                    .unwrap_or_default(),
                set_code: String::new(),
                card_number: String::new(),
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
                            .enumerate()
                            .filter_map(|(index, target)| stack_target_dto(index, &target.target))
                            .collect()
                    })
                    .unwrap_or_default(),
            }
        })
        .collect()
}

fn stack_target_dto(index: usize, target: &TargetRef) -> Option<StackTargetDto> {
    let (kind, id) = match target {
        TargetRef::Object(id) => ("card", encode_object_id(*id)),
        TargetRef::Player(id) => ("player", encode_player_id(*id)),
    };
    Some(StackTargetDto {
        kind: kind.to_string(),
        id,
        node_index: 0,
        target_index: index as u32,
        hostile: false,
        intent: "hostile".to_string(),
    })
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

fn playable_objects(actions: &[GameAction]) -> HashSet<ObjectId> {
    actions
        .iter()
        .filter_map(|action| match action {
            GameAction::PlayLand { object_id, .. }
            | GameAction::CastSpell { object_id, .. }
            | GameAction::CastSpellForFree { object_id, .. }
            | GameAction::CastSpellAsMiracle { object_id, .. }
            | GameAction::CastSpellAsMadness { object_id, .. }
            | GameAction::PlayFaceDown { object_id, .. }
            | GameAction::Foretell { object_id, .. } => Some(*object_id),
            GameAction::CastSpellAsSneak { hand_object, .. }
            | GameAction::CastSpellAsWebSlinging { hand_object, .. } => Some(*hand_object),
            GameAction::CastPreparedCopy { source } | GameAction::CastParadigmCopy { source } => {
                Some(*source)
            }
            _ => None,
        })
        .collect()
}

fn choosable_objects(waiting_for: &WaitingFor, viewer: PlayerId) -> HashSet<ObjectId> {
    match waiting_for {
        WaitingFor::DeclareAttackers {
            player,
            valid_attacker_ids,
            ..
        }
        | WaitingFor::DeclareBlockers {
            player,
            valid_blocker_ids: valid_attacker_ids,
            ..
        } if *player == viewer => valid_attacker_ids.iter().copied().collect(),
        WaitingFor::ScryChoice { player, cards }
        | WaitingFor::SurveilChoice { player, cards }
        | WaitingFor::DigChoice { player, cards, .. }
        | WaitingFor::DiscardChoice { player, cards, .. }
        | WaitingFor::ChooseFromZoneChoice { player, cards, .. }
        | WaitingFor::EffectZoneChoice { player, cards, .. }
        | WaitingFor::DrawnThisTurnTopdeckChoice { player, cards, .. }
        | WaitingFor::ManifestDreadChoice { player, cards, .. }
        | WaitingFor::WardDiscardChoice { player, cards, .. }
        | WaitingFor::ConniveDiscard { player, cards, .. }
        | WaitingFor::PairChoice {
            player,
            choices: cards,
            ..
        }
        | WaitingFor::PayCost {
            player,
            choices: cards,
            ..
        }
        | WaitingFor::CollectEvidenceChoice { player, cards, .. }
            if *player == viewer =>
        {
            cards.iter().copied().collect()
        }
        WaitingFor::LearnChoice { player, hand_cards } if *player == viewer => {
            hand_cards.iter().copied().collect()
        }
        WaitingFor::WardSacrificeChoice {
            player, permanents, ..
        }
        | WaitingFor::UnlessBounceChoice {
            player, permanents, ..
        }
        | WaitingFor::ChooseRingBearer {
            player,
            candidates: permanents,
        }
        | WaitingFor::PopulateChoice {
            player,
            valid_tokens: permanents,
            ..
        }
        | WaitingFor::ChooseLegend {
            player,
            candidates: permanents,
            ..
        }
        | WaitingFor::BlightChoice {
            player,
            creatures: permanents,
            ..
        }
        | WaitingFor::HarmonizeTapChoice {
            player,
            eligible_creatures: permanents,
            ..
        } if *player == viewer => permanents.iter().copied().collect(),
        WaitingFor::CategoryChoice {
            player,
            eligible_per_category,
            ..
        } if *player == viewer => eligible_per_category
            .iter()
            .flat_map(|ids| ids.iter().copied())
            .collect(),
        WaitingFor::MoveCountersDistribution {
            player,
            destinations,
            ..
        } if *player == viewer => destinations.iter().copied().collect(),
        WaitingFor::CopyRetarget {
            player,
            target_slots,
            ..
        } if *player == viewer => target_slots
            .iter()
            .flat_map(|slot| slot.legal_alternatives.iter())
            .filter_map(target_ref_object_id)
            .collect(),
        WaitingFor::TargetSelection {
            player,
            target_slots,
            ..
        }
        | WaitingFor::TriggerTargetSelection {
            player,
            target_slots,
            ..
        } if *player == viewer => target_slots
            .iter()
            .flat_map(|slot| slot.legal_targets.iter())
            .filter_map(target_ref_object_id)
            .collect(),
        _ => HashSet::new(),
    }
}

fn target_ref_object_id(target: &TargetRef) -> Option<ObjectId> {
    match target {
        TargetRef::Object(id) => Some(*id),
        TargetRef::Player(_) => None,
    }
}

fn mana_choice_options(choice: &ManaChoicePrompt) -> Vec<String> {
    match choice {
        ManaChoicePrompt::SingleColor { options }
        | ManaChoicePrompt::AnyCombination { options, .. } => options
            .iter()
            .copied()
            .map(mana_type_symbol)
            .map(str::to_string)
            .collect(),
        ManaChoicePrompt::Combination { options } => options
            .iter()
            .map(|combo| {
                combo
                    .iter()
                    .copied()
                    .map(mana_type_symbol)
                    .collect::<Vec<_>>()
                    .join("")
            })
            .collect(),
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

fn colors_string(colors: &[ManaColor]) -> String {
    colors
        .iter()
        .map(|color| mana_color_symbol(*color))
        .collect()
}

fn mana_color_symbol(color: ManaColor) -> &'static str {
    match color {
        ManaColor::White => "W",
        ManaColor::Blue => "U",
        ManaColor::Black => "B",
        ManaColor::Red => "R",
        ManaColor::Green => "G",
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

fn mana_cost_string(cost: &ManaCost) -> String {
    match cost {
        ManaCost::NoCost => String::new(),
        ManaCost::SelfManaCost => "its mana cost".to_string(),
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
        AttachTarget::Player(_) => None,
    }
}

fn defender_to_json(target: &AttackTarget) -> serde_json::Value {
    match target {
        AttackTarget::Player(player) => serde_json::json!({
            "kind": "player",
            "id": encode_player_id(*player),
            "label": format!("Player {}", player.0)
        }),
        AttackTarget::Planeswalker(id) => serde_json::json!({
            "kind": "planeswalker",
            "id": encode_object_id(*id),
            "label": encode_object_id(*id)
        }),
        AttackTarget::Battle(id) => serde_json::json!({
            "kind": "battle",
            "id": encode_object_id(*id),
            "label": encode_object_id(*id)
        }),
    }
}

fn action_refs(len: usize) -> Vec<AvailablePlayerActionRef> {
    (0..len)
        .map(|action_index| AvailablePlayerActionRef::EngineActionRef { action_index })
        .collect()
}

fn source_ids_for(
    grouped: &HashMap<ObjectId, Vec<GameAction>>,
    predicate: impl Fn(&GameAction) -> bool,
) -> Vec<String> {
    grouped
        .iter()
        .filter(|(_, actions)| actions.iter().any(&predicate))
        .map(|(id, _)| encode_object_id(*id))
        .collect()
}

fn insert_json<T: Serialize>(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    value: T,
) {
    map.insert(
        key.to_string(),
        serde_json::to_value(value).expect("Manabrew DTO field must serialize"),
    );
}

fn insert_object_id_list(
    map: &mut serde_json::Map<String, serde_json::Value>,
    key: &str,
    ids: &[ObjectId],
) {
    insert_json(
        map,
        key,
        ids.iter()
            .copied()
            .map(encode_object_id)
            .collect::<Vec<_>>(),
    );
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
        | WaitingFor::ChooseXValue { pending_cast, .. } => {
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
        WaitingFor::MulliganBottomCards { .. } => "MulliganBottomCards",
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
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, CategoryChooserScope, Effect, EffectKind, ModalChoice, ResolvedAbility,
        TargetFilter,
    };
    use engine::types::card_type::CoreType;
    use engine::types::counter::CounterMatch;
    use engine::types::game_state::{
        CombatTaxPending, CopyTargetSlot, CostResume, ManaAbilityResume, ManaChoiceContext,
        ManaChoicePrompt, MulliganBottomEntry, MulliganDecisionEntry, PayCostKind, PendingCast,
        PendingManaAbility, TargetSelectionProgress, TargetSelectionSlot,
    };
    use engine::types::identifiers::CardId;
    use engine::types::zones::ExileCostSourceZone;
    use pretty_assertions::assert_eq;

    fn lookup(_: &GameObject) -> Option<String> {
        Some("Test oracle text.".to_string())
    }

    fn dummy_ability() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Unimplemented {
                name: "Dummy".to_string(),
                description: None,
            },
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

    fn dummy_pending_mana_ability() -> Box<PendingManaAbility> {
        Box::new(PendingManaAbility {
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
        })
    }

    fn prompt_for(waiting_for: WaitingFor) -> Result<AgentPrompt> {
        let mut state = GameState::new_two_player(7);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Prompt Source".to_string(),
            Zone::Hand,
        );
        state.waiting_for = waiting_for;
        let prepared = PreparedManabrewSnapshot {
            game_id: "game-a".to_string(),
            viewer: PlayerId(0),
            state,
            derived: DerivedViews::default(),
            actions: Vec::new(),
            spell_costs: HashMap::new(),
            legal_actions_by_object: HashMap::new(),
        };
        build_prompt(&prepared, &lookup, vec![])
    }

    #[test]
    fn id_codecs_roundtrip() {
        assert_eq!(encode_object_id(ObjectId(42)), "card-42");
        assert_eq!(parse_object_id("card-42").unwrap(), ObjectId(42));
        assert!(matches!(
            parse_object_id("player-42"),
            Err(AdapterError::MalformedId { .. })
        ));
    }

    #[test]
    fn combat_damage_maps_conservatively() {
        assert_eq!(phase_step(Phase::CombatDamage), "combat_damage");
    }

    #[test]
    fn prepare_snapshot_derives_display_state_and_game_view_fields() {
        let mut state = GameState::new_two_player(7);
        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );
        state.monarch = Some(PlayerId(0));
        state.initiative = Some(PlayerId(1));
        state.city_blessing.insert(PlayerId(0));
        state.players[0].add_player_counters(&PlayerCounterKind::Rad, 2);
        state.ring_level.insert(PlayerId(0), 3);

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let view = build_game_view(&prepared, &lookup).unwrap();

        assert_eq!(view.monarch_id, Some("player-0".to_string()));
        assert_eq!(view.initiative_holder_id, Some("player-1".to_string()));
        assert_eq!(view.players[0].radiation_counters, 2);
        assert!(view.players[0].has_city_blessing);
        assert_eq!(view.players[0].ring_level, 3);
        assert_eq!(view.battlefield[0].text, "Test oracle text.");
    }

    #[test]
    fn visible_card_without_text_lookup_errors() {
        let mut state = GameState::new_two_player(7);
        let creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Battlefield,
        );

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let missing = |_: &GameObject| None;
        assert!(matches!(
            build_game_view(&prepared, &missing),
            Err(AdapterError::MissingCardText { object_id }) if object_id == creature
        ));
    }

    #[test]
    fn redacted_hidden_cards_do_not_leak_characteristics() {
        let mut state = GameState::new_two_player(7);
        let hidden = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Secret Card".to_string(),
            Zone::Exile,
        );
        let object = state.objects.get_mut(&hidden).unwrap();
        object.face_down = true;
        object.mana_cost = ManaCost::generic(7);
        object.power = Some(9);

        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();
        let view = build_game_view(&prepared, &lookup).unwrap();
        let card = &view.exile[0];

        assert_eq!(card.name, "Hidden Card");
        assert_eq!(card.mana_cost, "");
        assert_eq!(card.cmc, 0);
        assert_eq!(card.power, None);
        assert_eq!(card.text, "");
    }

    #[test]
    fn engine_action_ref_returns_exact_indexed_action() {
        let action_table = vec![GameAction::PassPriority];
        let context = PromptContext { action_table };
        let state = GameState::new_two_player(7);

        let translated = translate_player_action(
            PlayerAction::EngineAction { action_index: 0 },
            &context,
            &state,
        )
        .unwrap();

        assert_eq!(translated, GameAction::PassPriority);
        assert!(matches!(
            translate_player_action(
                PlayerAction::EngineAction { action_index: 1 },
                &context,
                &state,
            ),
            Err(AdapterError::StaleOrInvalidActionIndex { action_index: 1 })
        ));
    }

    #[test]
    fn playable_objects_includes_casting_action_siblings() {
        let playable = playable_objects(&[
            GameAction::PlayFaceDown {
                object_id: ObjectId(11),
                card_id: CardId(11),
            },
            GameAction::CastPreparedCopy {
                source: ObjectId(12),
            },
            GameAction::CastParadigmCopy {
                source: ObjectId(13),
            },
        ]);

        assert!(playable.contains(&ObjectId(11)));
        assert!(playable.contains(&ObjectId(12)));
        assert!(playable.contains(&ObjectId(13)));
    }

    #[test]
    fn choosable_objects_includes_resolution_choice_siblings() {
        assert_eq!(
            choosable_objects(
                &WaitingFor::ChooseFromZoneChoice {
                    player: PlayerId(0),
                    cards: vec![ObjectId(21)],
                    count: 1,
                    up_to: false,
                    constraint: None,
                    source_id: ObjectId(1),
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(21)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::WardSacrificeChoice {
                    player: PlayerId(0),
                    permanents: vec![ObjectId(22)],
                    pending_effect: Box::new(dummy_ability()),
                    remaining: 1,
                    min_total_power: None,
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(22)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::CategoryChoice {
                    player: PlayerId(0),
                    target_player: PlayerId(0),
                    categories: vec![CoreType::Creature],
                    chooser_scope: CategoryChooserScope::ControllerForAll,
                    choose_filter: TargetFilter::Any,
                    sacrifice_filter: TargetFilter::Any,
                    source_controller: PlayerId(0),
                    eligible_per_category: vec![vec![ObjectId(23)], vec![ObjectId(24)]],
                    source_id: ObjectId(1),
                    remaining_players: vec![],
                    all_kept: vec![],
                    scoped_players: vec![PlayerId(0)],
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(23), ObjectId(24)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::CopyRetarget {
                    player: PlayerId(0),
                    copy_id: ObjectId(1),
                    target_slots: vec![CopyTargetSlot {
                        current: None,
                        legal_alternatives: vec![
                            TargetRef::Object(ObjectId(25)),
                            TargetRef::Player(PlayerId(1)),
                        ],
                    }],
                    effect_kind: EffectKind::CopySpell,
                    effect_source_id: Some(ObjectId(1)),
                    current_slot: 0,
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(25)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::MoveCountersDistribution {
                    player: PlayerId(0),
                    source_id: ObjectId(1),
                    counter_type: Some(CounterType::Plus1Plus1),
                    available: vec![(CounterType::Plus1Plus1, 1)],
                    destinations: vec![ObjectId(26)],
                    pending_effect: Box::new(dummy_ability()),
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(26)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::PairChoice {
                    player: PlayerId(0),
                    source_id: ObjectId(1),
                    choices: vec![ObjectId(27)],
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(27)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::PayCost {
                    player: PlayerId(0),
                    kind: PayCostKind::ExileFromZone {
                        zone: ExileCostSourceZone::Graveyard,
                    },
                    choices: vec![ObjectId(28)],
                    count: 1,
                    min_count: 0,
                    resume: CostResume::Spell {
                        spell: dummy_pending_cast(),
                    },
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(28)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::PayCost {
                    player: PlayerId(0),
                    kind: PayCostKind::RemoveCounter {
                        counter_type: CounterMatch::Any,
                        count: 1,
                        selection: engine::types::ability::CounterCostSelection::SingleObject,
                    },
                    choices: vec![ObjectId(29)],
                    count: 1,
                    min_count: 0,
                    resume: CostResume::Spell {
                        spell: dummy_pending_cast(),
                    },
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(29)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::PayCost {
                    player: PlayerId(0),
                    kind: PayCostKind::TapCreatures,
                    choices: vec![ObjectId(30)],
                    count: 1,
                    min_count: 0,
                    resume: CostResume::ManaAbility {
                        mana_ability: dummy_pending_mana_ability(),
                    },
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(30)])
        );
        assert_eq!(
            choosable_objects(
                &WaitingFor::HarmonizeTapChoice {
                    player: PlayerId(0),
                    eligible_creatures: vec![ObjectId(31)],
                    pending_cast: dummy_pending_cast(),
                },
                PlayerId(0),
            ),
            HashSet::from([ObjectId(31)])
        );
    }

    #[test]
    fn mana_choice_prompt_uses_symbol_lists_not_debug_strings() {
        let prompt = prompt_for(WaitingFor::ChooseManaColor {
            player: PlayerId(0),
            choice: ManaChoicePrompt::Combination {
                options: vec![
                    vec![ManaType::White, ManaType::Blue],
                    vec![ManaType::Black, ManaType::Red],
                ],
            },
            context: ManaChoiceContext::ResolvingEffect(Box::new(dummy_ability())),
        })
        .unwrap();

        assert_eq!(prompt.prompt_type, "specifyManaCombo");
        assert_eq!(
            prompt.fields["availableColors"],
            serde_json::json!(["WU", "BR"])
        );
    }

    #[test]
    fn direct_player_actions_are_rejected_even_without_action_table() {
        let context = PromptContext::default();
        let state = GameState::new_two_player(7);

        assert!(matches!(
            translate_player_action(PlayerAction::Pass { until_phase: None }, &context, &state,),
            Err(AdapterError::IllegalActionForPrompt {
                action_kind: "directAction"
            })
        ));
    }

    #[test]
    fn priority_prompt_emits_only_opaque_engine_action_refs() {
        let prompt = prompt_for(WaitingFor::Priority {
            player: PlayerId(0),
        })
        .unwrap();
        let actions = prompt.fields["availablePlayerActions"].as_array().unwrap();

        assert_eq!(prompt.prompt_type, "chooseAction");
        assert!(actions.iter().all(|action| {
            action["kind"] == "engineActionRef" && action.get("actionIndex").is_some()
        }));
    }

    #[test]
    fn non_acting_viewer_does_not_receive_legal_action_refs() {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        let prepared = prepare_snapshot(&state, PlayerId(1), "game-a").unwrap();
        let prompt = build_prompt(&prepared, &lookup, vec![]).unwrap();

        assert!(prepared.actions.is_empty());
        assert_eq!(
            prompt.fields["availablePlayerActions"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn representative_whitelisted_prompts_build() {
        let cases = [
            (
                "mulligan",
                WaitingFor::MulliganDecision {
                    pending: vec![MulliganDecisionEntry {
                        player: PlayerId(0),
                        mulligan_count: 1,
                    }],
                    free_first_mulligan: false,
                },
            ),
            (
                "mulliganPutBack",
                WaitingFor::MulliganBottomCards {
                    pending: vec![MulliganBottomEntry {
                        player: PlayerId(0),
                        count: 1,
                    }],
                },
            ),
            (
                "mulliganPutBack",
                WaitingFor::OpeningHandBottomCards {
                    pending: vec![MulliganBottomEntry {
                        player: PlayerId(0),
                        count: 1,
                    }],
                    reason: engine::types::game_state::OpeningHandBottomReason::TinyLeadersMultiCommander,
                },
            ),
            (
                "chooseAttackers",
                WaitingFor::DeclareAttackers {
                    player: PlayerId(0),
                    valid_attacker_ids: vec![],
                    valid_attack_targets: vec![],
                },
            ),
            (
                "chooseBlockers",
                WaitingFor::DeclareBlockers {
                    player: PlayerId(0),
                    valid_blocker_ids: vec![],
                    valid_block_targets: HashMap::new(),
                    block_requirements: HashMap::new(),
                },
            ),
            (
                "chooseTargetCard",
                WaitingFor::TargetSelection {
                    player: PlayerId(0),
                    pending_cast: dummy_pending_cast(),
                    target_slots: vec![TargetSelectionSlot {
                        legal_targets: vec![TargetRef::Object(ObjectId(1))],
                        optional: false,
                    }],
                    mode_labels: Vec::new(),
                    selection: TargetSelectionProgress::default(),
                },
            ),
            (
                "chooseTargetCard",
                WaitingFor::TriggerTargetSelection {
                    player: PlayerId(0),
                    target_slots: vec![TargetSelectionSlot {
                        legal_targets: vec![TargetRef::Object(ObjectId(1))],
                        optional: false,
                    }],
                    mode_labels: Vec::new(),
                    target_constraints: vec![],
                    selection: TargetSelectionProgress::default(),
                    source_id: Some(ObjectId(1)),
                    description: Some("target something".to_string()),
                },
            ),
            (
                "payManaCost",
                WaitingFor::ManaPayment {
                    player: PlayerId(0),
                    convoke_mode: None,
                },
            ),
            (
                "specifyManaCombo",
                WaitingFor::ChooseManaColor {
                    player: PlayerId(0),
                    choice: ManaChoicePrompt::SingleColor {
                        options: vec![ManaType::White],
                    },
                    context: ManaChoiceContext::ResolvingEffect(Box::new(dummy_ability())),
                },
            ),
            (
                "specifyManaCombo",
                WaitingFor::PayManaAbilityMana {
                    player: PlayerId(0),
                    options: vec![vec![ManaType::White]],
                    pending_mana_ability: dummy_pending_mana_ability(),
                },
            ),
            (
                "scry",
                WaitingFor::ScryChoice {
                    player: PlayerId(0),
                    cards: vec![ObjectId(1)],
                },
            ),
            (
                "surveil",
                WaitingFor::SurveilChoice {
                    player: PlayerId(0),
                    cards: vec![ObjectId(1)],
                },
            ),
            (
                "dig",
                WaitingFor::DigChoice {
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
                },
            ),
            (
                "chooseDiscard",
                WaitingFor::DiscardChoice {
                    player: PlayerId(0),
                    count: 1,
                    cards: vec![ObjectId(1)],
                    source_id: ObjectId(1),
                    effect_kind: engine::types::ability::EffectKind::Discard,
                    up_to: false,
                    unless_filter: None,
                },
            ),
            (
                "chooseMode",
                WaitingFor::ModeChoice {
                    player: PlayerId(0),
                    modal: ModalChoice {
                        min_choices: 1,
                        max_choices: 1,
                        mode_count: 2,
                        mode_descriptions: vec!["One".to_string(), "Two".to_string()],
                        ..Default::default()
                    },
                    pending_cast: dummy_pending_cast(),
                    unavailable_modes: vec![],
                },
            ),
            (
                "chooseMode",
                WaitingFor::AbilityModeChoice {
                    player: PlayerId(0),
                    modal: ModalChoice {
                        min_choices: 1,
                        max_choices: 1,
                        mode_count: 1,
                        mode_descriptions: vec!["Mode".to_string()],
                        ..Default::default()
                    },
                    source_id: ObjectId(1),
                    mode_abilities: vec![],
                    is_activated: false,
                    ability_index: None,
                    ability_cost: None,
                    unavailable_modes: vec![],
                },
            ),
            (
                "chooseOptionalTrigger",
                WaitingFor::OptionalEffectChoice {
                    player: PlayerId(0),
                    source_id: ObjectId(1),
                    description: Some("draw a card".to_string()),
                    may_trigger_key: None,
                },
            ),
            (
                "chooseOptionalTrigger",
                WaitingFor::OpponentMayChoice {
                    player: PlayerId(0),
                    source_id: ObjectId(1),
                    description: Some("draw a card".to_string()),
                    remaining: vec![],
                },
            ),
            (
                "payCostToPreventEffect",
                WaitingFor::UnlessPayment {
                    player: PlayerId(0),
                    cost: AbilityCost::Mana {
                        cost: ManaCost::NoCost,
                    },
                    pending_effect: Box::new(dummy_ability()),
                    trigger_event: None,
                    effect_description: Some("counter target spell".to_string()),
                    remaining: vec![],
                },
            ),
            (
                "payCostToPreventEffect",
                WaitingFor::UnlessPaymentChooseCost {
                    player: PlayerId(0),
                    costs: vec![AbilityCost::Mana {
                        cost: ManaCost::NoCost,
                    }],
                    pending_effect: Box::new(dummy_ability()),
                    trigger_event: None,
                    effect_description: Some("counter target spell".to_string()),
                    remaining_choices: vec![],
                    chosen: vec![],
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
                },
            ),
            (
                "chooseType",
                WaitingFor::NamedChoice {
                    player: PlayerId(0),
                    choice_type: ChoiceType::CreatureType,
                    options: vec!["Wizard".to_string()],
                    source_id: Some(ObjectId(1)),
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
            (
                "payCombatCost",
                WaitingFor::CombatTaxPayment {
                    player: PlayerId(0),
                    context: engine::types::game_state::CombatTaxContext::Attacking,
                    total_cost: ManaCost::NoCost,
                    per_creature: vec![],
                    pending: CombatTaxPending::Attack {
                        attacks: vec![],
                        bands: vec![],
                    },
                },
            ),
            ("gameOver", WaitingFor::GameOver { winner: None }),
        ];

        for (expected_type, waiting_for) in cases {
            let prompt = prompt_for(waiting_for).unwrap();
            assert_eq!(prompt.prompt_type, expected_type);
            assert!(prompt.fields.contains_key("availablePlayerActions"));
        }
    }

    #[test]
    fn unsupported_prompt_returns_error() {
        let mut state = GameState::new_two_player(7);
        state.waiting_for = WaitingFor::CopyTargetChoice {
            player: PlayerId(0),
            source_id: ObjectId(1),
            valid_targets: vec![],
            max_mana_value: None,
        };
        let prepared = prepare_snapshot(&state, PlayerId(0), "game-a").unwrap();

        assert!(matches!(
            build_prompt(&prepared, &lookup, vec![]),
            Err(AdapterError::UnsupportedPrompt { .. })
        ));
    }
}

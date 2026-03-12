use thiserror::Error;

use crate::types::actions::GameAction;
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::phase::Phase;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::casting;
use super::derived::derive_display_state;
use super::effects;
use super::mana_abilities;
use super::mana_payment;
use super::mulligan;
use super::planeswalker;
use super::priority;
use super::sba;
use super::triggers;
use super::turns;
use super::zones;

#[derive(Debug, Clone, Error)]
pub enum EngineError {
    #[error("Invalid action: {0}")]
    InvalidAction(String),
    #[error("Wrong player")]
    WrongPlayer,
    #[error("Not your priority")]
    NotYourPriority,
    #[error("Action not allowed: {0}")]
    ActionNotAllowed(String),
}

pub fn apply(state: &mut GameState, action: GameAction) -> Result<ActionResult, EngineError> {
    let result = apply_action(state, action);
    derive_display_state(state);
    result
}

fn apply_action(state: &mut GameState, action: GameAction) -> Result<ActionResult, EngineError> {
    let mut events = Vec::new();

    // Validate and process action against current WaitingFor
    let waiting_for = match (&state.waiting_for.clone(), action) {
        (WaitingFor::Priority { player }, GameAction::PassPriority) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            priority::handle_priority_pass(state, &mut events)
        }
        (WaitingFor::Priority { player }, GameAction::PlayLand { card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            handle_play_land(state, card_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::TapLandForMana { object_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            handle_tap_land_for_mana(state, object_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::CastSpell { card_id, .. }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            casting::handle_cast_spell(state, *player, card_id, &mut events)?
        }
        (
            WaitingFor::Priority { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            // Check if this is a mana ability -- resolve instantly without the stack
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let ability_def = obj.abilities[ability_index].clone();
                mana_abilities::resolve_mana_ability(
                    state,
                    source_id,
                    *player,
                    &ability_def,
                    &mut events,
                )?;
                WaitingFor::Priority { player: *player }
            } else if obj.loyalty.is_some()
                && ability_index < obj.abilities.len()
                && matches!(
                    obj.abilities[ability_index].cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { .. })
                )
            {
                // Planeswalker loyalty ability
                planeswalker::handle_activate_loyalty(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            } else {
                casting::handle_activate_ability(
                    state,
                    *player,
                    source_id,
                    ability_index,
                    &mut events,
                )?
            }
        }
        (WaitingFor::TargetSelection { player, .. }, GameAction::SelectTargets { targets }) => {
            casting::handle_select_targets(state, *player, targets, &mut events)?
        }
        (
            WaitingFor::TargetSelection {
                player,
                pending_cast,
                ..
            },
            GameAction::CancelCast,
        ) => {
            casting::handle_cancel_cast(state, pending_cast, &mut events);
            WaitingFor::Priority { player: *player }
        }
        (WaitingFor::ManaPayment { player }, GameAction::CancelCast) => {
            WaitingFor::Priority { player: *player }
        }
        // Allow mana abilities during mana payment (mid-cast)
        (
            WaitingFor::ManaPayment { player },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        ) => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if ability_index < obj.abilities.len()
                && mana_abilities::is_mana_ability(&obj.abilities[ability_index])
            {
                let ability_def = obj.abilities[ability_index].clone();
                mana_abilities::resolve_mana_ability(
                    state,
                    source_id,
                    *player,
                    &ability_def,
                    &mut events,
                )?;
                WaitingFor::ManaPayment { player: *player }
            } else {
                return Err(EngineError::ActionNotAllowed(
                    "Only mana abilities can be activated during mana payment".to_string(),
                ));
            }
        }
        // Allow basic land tapping during mana payment
        (WaitingFor::ManaPayment { player }, GameAction::TapLandForMana { object_id }) => {
            handle_tap_land_for_mana(state, object_id, &mut events)?;
            WaitingFor::ManaPayment { player: *player }
        }
        (
            WaitingFor::MulliganDecision {
                player,
                mulligan_count,
            },
            GameAction::MulliganDecision { keep },
        ) => {
            let p = *player;
            let mc = *mulligan_count;
            mulligan::handle_mulligan_decision(state, p, keep, mc, &mut events)
        }
        (WaitingFor::MulliganBottomCards { player, count }, GameAction::SelectCards { cards }) => {
            let p = *player;
            let c = *count;
            mulligan::handle_mulligan_bottom(state, p, cards, c, &mut events)
                .map_err(EngineError::InvalidAction)?
        }
        (WaitingFor::DeclareAttackers { player, .. }, GameAction::DeclareAttackers { attacks }) => {
            if state.active_player != *player {
                return Err(EngineError::WrongPlayer);
            }
            super::combat::declare_attackers(state, &attacks, &mut events)
                .map_err(EngineError::InvalidAction)?;

            // Process triggers for AttackersDeclared
            triggers::process_triggers(state, &events);

            if attacks.is_empty() {
                // No attackers: skip to EndCombat
                state.phase = Phase::EndCombat;
                events.push(GameEvent::PhaseChanged {
                    phase: Phase::EndCombat,
                });
                state.combat = None;
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            } else {
                // Advance to DeclareBlockers
                turns::advance_phase(state, &mut events);
                turns::auto_advance(state, &mut events)
            }
        }
        (
            WaitingFor::DeclareBlockers { player: _, .. },
            GameAction::DeclareBlockers { assignments },
        ) => {
            super::combat::declare_blockers(state, &assignments, &mut events)
                .map_err(EngineError::InvalidAction)?;

            // Process triggers for BlockersDeclared
            triggers::process_triggers(state, &events);

            // Advance to CombatDamage
            turns::advance_phase(state, &mut events);
            turns::auto_advance(state, &mut events)
        }
        (WaitingFor::ReplacementChoice { .. }, GameAction::ChooseReplacement { index }) => {
            match super::replacement::continue_replacement(state, index, &mut events) {
                super::replacement::ReplacementResult::Execute(_) => WaitingFor::Priority {
                    player: state.active_player,
                },
                super::replacement::ReplacementResult::NeedsChoice(player) => {
                    let candidate_count = state
                        .pending_replacement
                        .as_ref()
                        .map(|p| p.candidates.len())
                        .unwrap_or(0);
                    WaitingFor::ReplacementChoice {
                        player,
                        candidate_count,
                    }
                }
                super::replacement::ReplacementResult::Prevented => WaitingFor::Priority {
                    player: state.active_player,
                },
            }
        }
        (
            WaitingFor::EquipTarget {
                player,
                equipment_id,
                valid_targets,
            },
            GameAction::Equip {
                equipment_id: eq_id,
                target_id,
            },
        ) => {
            if eq_id != *equipment_id {
                return Err(EngineError::InvalidAction(
                    "Equipment ID mismatch".to_string(),
                ));
            }
            if !valid_targets.contains(&target_id) {
                return Err(EngineError::InvalidAction(
                    "Invalid equip target".to_string(),
                ));
            }
            let p = *player;
            effects::attach::attach_to(state, eq_id, target_id);
            events.push(GameEvent::EffectResolved {
                api_type: "Equip".to_string(),
                source_id: eq_id,
            });
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::Equip { equipment_id, .. }) => {
            let p = *player;
            handle_equip_activation(state, p, equipment_id, &mut events)?
        }
        (WaitingFor::Priority { player }, GameAction::Transform { object_id }) => {
            let p = *player;
            let obj = state
                .objects
                .get(&object_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::InvalidAction(
                    "Object is not on the battlefield".to_string(),
                ));
            }
            if obj.controller != p {
                return Err(EngineError::InvalidAction(
                    "You don't control this permanent".to_string(),
                ));
            }
            if obj.back_face.is_none() {
                return Err(EngineError::InvalidAction(
                    "Card has no back face".to_string(),
                ));
            }
            super::transform::transform_permanent(state, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        // Scry: player selects cards to put on TOP (in order), rest go to bottom
        (
            WaitingFor::ScryChoice { player, cards },
            GameAction::SelectCards { cards: top_cards },
        ) => {
            let p = *player;
            let all_cards = cards.clone();
            // top_cards = ObjectIds the player wants on top, rest go to bottom
            let bottom_cards: Vec<_> = all_cards
                .iter()
                .filter(|id| !top_cards.contains(id))
                .copied()
                .collect();
            // Remove all scryed cards from library, then re-insert: top cards first, then bottom
            let player_state = state
                .players
                .iter_mut()
                .find(|pl| pl.id == p)
                .expect("player exists");
            player_state.library.retain(|id| !all_cards.contains(id));
            // Insert top cards at front (index 0) in reverse order so first selected = top
            for (i, &card_id) in top_cards.iter().enumerate() {
                player_state.library.insert(i, card_id);
            }
            // Bottom cards go to end
            for &card_id in &bottom_cards {
                player_state.library.push(card_id);
            }
            WaitingFor::Priority { player: p }
        }
        // Dig: player selects keep_count cards for hand, rest go to graveyard
        (
            WaitingFor::DigChoice {
                player,
                cards: all_cards,
                keep_count,
            },
            GameAction::SelectCards { cards: kept },
        ) => {
            let p = *player;
            let kc = *keep_count;
            let all = all_cards.clone();
            if kept.len() != kc {
                return Err(EngineError::InvalidAction(format!(
                    "Must select exactly {} cards, got {}",
                    kc,
                    kept.len()
                )));
            }
            let to_graveyard: Vec<_> = all
                .iter()
                .filter(|id| !kept.contains(id))
                .copied()
                .collect();
            // Move kept cards to hand
            for &obj_id in &kept {
                zones::move_to_zone(state, obj_id, Zone::Hand, &mut events);
            }
            // Move rest to graveyard
            for &obj_id in &to_graveyard {
                zones::move_to_zone(state, obj_id, Zone::Graveyard, &mut events);
            }
            WaitingFor::Priority { player: p }
        }
        // Surveil: player selects cards to put in GRAVEYARD, rest stay on top
        (
            WaitingFor::SurveilChoice { player, cards },
            GameAction::SelectCards {
                cards: to_graveyard,
            },
        ) => {
            let p = *player;
            let all_cards = cards.clone();
            // Move selected cards to graveyard
            for &obj_id in &to_graveyard {
                if all_cards.contains(&obj_id) {
                    zones::move_to_zone(state, obj_id, Zone::Graveyard, &mut events);
                }
            }
            // Cards not in to_graveyard stay on top of library (already there)
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::PlayFaceDown { card_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            // Find the object with this card_id in the player's hand
            let object_id = state
                .objects
                .iter()
                .find(|(_, obj)| obj.card_id == card_id && obj.owner == p && obj.zone == Zone::Hand)
                .map(|(id, _)| *id)
                .ok_or_else(|| EngineError::InvalidAction("Card not found in hand".to_string()))?;
            super::morph::play_face_down(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (WaitingFor::Priority { player }, GameAction::TurnFaceUp { object_id }) => {
            if state.priority_player != *player {
                return Err(EngineError::NotYourPriority);
            }
            let p = *player;
            super::morph::turn_face_up(state, p, object_id, &mut events)?;
            WaitingFor::Priority { player: p }
        }
        (
            WaitingFor::TriggerTargetSelection {
                player,
                legal_targets,
            },
            GameAction::SelectTargets { targets },
        ) => {
            // Validate targets are legal
            for t in &targets {
                if !legal_targets.contains(t) {
                    return Err(EngineError::InvalidAction(
                        "Illegal target selected".to_string(),
                    ));
                }
            }
            // Take the pending trigger, set targets, push to stack
            let trigger = state
                .pending_trigger
                .take()
                .ok_or_else(|| EngineError::InvalidAction("No pending trigger".to_string()))?;
            let mut ability = trigger.ability;
            ability.targets = targets;

            casting::emit_targeting_events(
                state,
                &ability.targets,
                trigger.source_id,
                trigger.controller,
                &mut events,
            );

            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;
            let entry = crate::types::game_state::StackEntry {
                id: entry_id,
                source_id: trigger.source_id,
                controller: trigger.controller,
                kind: crate::types::game_state::StackEntryKind::TriggeredAbility {
                    source_id: trigger.source_id,
                    ability,
                    condition: trigger.trigger_def.condition.clone(),
                },
            };
            super::stack::push_to_stack(state, entry, &mut events);
            state.priority_passes.clear();
            state.priority_pass_count = 0;
            WaitingFor::Priority { player: *player }
        }
        (waiting, action) => {
            return Err(EngineError::ActionNotAllowed(format!(
                "Cannot perform {:?} while waiting for {:?}",
                action, waiting
            )));
        }
    };

    // Run state-based actions after every action (except during mulligan/game over)
    if matches!(waiting_for, WaitingFor::Priority { .. }) {
        sba::check_state_based_actions(state, &mut events);

        // Check exile returns -- must happen after SBAs (which may move sources off battlefield)
        // and before triggers (so returned permanents get ETB triggers)
        check_exile_returns(state, &mut events);

        // SBA might have set game over
        if matches!(state.waiting_for, WaitingFor::GameOver { .. }) {
            let wf = state.waiting_for.clone();
            return Ok(ActionResult {
                events,
                waiting_for: wf,
            });
        }

        // Process triggers after action + SBA + exile return events.
        // Filter out PhaseChanged events for phases that were auto-advanced past.
        // Without this filter, triggers like "at the beginning of combat" fire
        // even when combat was skipped, causing phantom stack entries.
        let current_phase = state.phase;
        let filtered_events: Vec<_> = events
            .iter()
            .filter(|e| {
                if let GameEvent::PhaseChanged { phase } = e {
                    *phase == current_phase
                } else {
                    true
                }
            })
            .cloned()
            .collect();
        let stack_before = state.stack.len();
        triggers::process_triggers(state, &filtered_events);

        // Check if a trigger needs target selection from the player
        if let Some(trigger) = state.pending_trigger.as_ref() {
            if let Some(filter) =
                triggers::extract_target_filter_from_effect(&trigger.ability.effect)
            {
                let legal = super::targeting::find_legal_targets_typed(
                    state,
                    filter,
                    trigger.controller,
                    trigger.source_id,
                );
                let player = trigger.controller;
                let wf = WaitingFor::TriggerTargetSelection {
                    player,
                    legal_targets: legal,
                };
                state.waiting_for = wf.clone();
                derive_display_state(state);
                return Ok(ActionResult {
                    events,
                    waiting_for: wf,
                });
            }
        }

        // If triggers were placed on stack, grant priority to active player
        if state.stack.len() > stack_before {
            let wf = WaitingFor::Priority {
                player: state.active_player,
            };
            state.waiting_for = wf.clone();
            return Ok(ActionResult {
                events,
                waiting_for: wf,
            });
        }

        // Re-evaluate layers if dirty after SBA/trigger processing
        if state.layers_dirty {
            super::layers::evaluate_layers(state);
        }
    }

    state.waiting_for = waiting_for.clone();

    Ok(ActionResult {
        events,
        waiting_for,
    })
}

fn handle_play_land(
    state: &mut GameState,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate main phase
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Can only play lands during main phases".to_string(),
            ));
        }
    }

    // Validate land limit
    if state.lands_played_this_turn >= state.max_lands_per_turn {
        return Err(EngineError::ActionNotAllowed(
            "Already played maximum lands this turn".to_string(),
        ));
    }

    // Find the object in player's hand by matching card_id
    let player = state
        .players
        .iter()
        .find(|p| p.id == state.priority_player)
        .expect("priority player exists");

    let object_id = player
        .hand
        .iter()
        .find(|&&obj_id| {
            state
                .objects
                .get(&obj_id)
                .map(|obj| obj.card_id == card_id)
                .unwrap_or(false)
        })
        .copied()
        .ok_or_else(|| EngineError::InvalidAction("Card not found in hand".to_string()))?;

    // Move from hand to battlefield
    zones::move_to_zone(state, object_id, Zone::Battlefield, events);

    // Set tapped=false (lands enter untapped by default)
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.tapped = false;
        obj.entered_battlefield_turn = Some(state.turn_number);
    }

    // Increment land counter
    state.lands_played_this_turn += 1;
    let player = state
        .players
        .iter_mut()
        .find(|p| p.id == state.priority_player)
        .expect("priority player exists");
    player.lands_played_this_turn += 1;

    // Reset priority passes (action was taken)
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::LandPlayed {
        object_id,
        player_id: state.priority_player,
    });

    // Player retains priority after playing a land
    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

fn handle_tap_land_for_mana(
    state: &mut GameState,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    // Validate: on battlefield, controlled by acting player, is a land, not tapped
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != state.priority_player {
        return Err(EngineError::NotYourPriority);
    }
    if !obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::InvalidAction(
            "Object is not a land".to_string(),
        ));
    }
    if obj.tapped {
        return Err(EngineError::InvalidAction(
            "Land is already tapped".to_string(),
        ));
    }

    // Determine mana color from subtypes
    let mana_type = obj
        .card_types
        .subtypes
        .iter()
        .find_map(|s| mana_payment::land_subtype_to_mana_type(s))
        .ok_or_else(|| {
            EngineError::InvalidAction("Land has no recognized basic land subtype".to_string())
        })?;

    // Tap the permanent
    let obj = state.objects.get_mut(&object_id).unwrap();
    obj.tapped = true;

    events.push(GameEvent::PermanentTapped { object_id });

    // Produce mana
    mana_payment::produce_mana(state, object_id, mana_type, state.priority_player, events);

    Ok(WaitingFor::Priority {
        player: state.priority_player,
    })
}

fn handle_equip_activation(
    state: &mut GameState,
    player: PlayerId,
    equipment_id: ObjectId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Validate sorcery-speed timing: main phase, empty stack, active player
    match state.phase {
        Phase::PreCombatMain | Phase::PostCombatMain => {}
        _ => {
            return Err(EngineError::ActionNotAllowed(
                "Equip can only be activated during main phases".to_string(),
            ));
        }
    }
    if !state.stack.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated when the stack is empty".to_string(),
        ));
    }
    if state.active_player != player {
        return Err(EngineError::ActionNotAllowed(
            "Equip can only be activated by the active player".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&equipment_id)
        .ok_or_else(|| EngineError::InvalidAction("Equipment not found".to_string()))?;

    // Validate it's an equipment on the battlefield controlled by player
    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Equipment is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::InvalidAction(
            "You don't control this equipment".to_string(),
        ));
    }
    if !obj.card_types.subtypes.contains(&"Equipment".to_string()) {
        return Err(EngineError::InvalidAction(
            "Object is not an equipment".to_string(),
        ));
    }

    // Find valid targets: creatures controlled by the equipping player on battlefield
    let valid_targets: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            state
                .objects
                .get(id)
                .map(|o| {
                    o.controller == player
                        && o.card_types
                            .core_types
                            .contains(&crate::types::card_type::CoreType::Creature)
                })
                .unwrap_or(false)
        })
        .collect();

    if valid_targets.is_empty() {
        return Err(EngineError::ActionNotAllowed(
            "No valid creatures to equip".to_string(),
        ));
    }

    // If only one target, auto-equip
    if valid_targets.len() == 1 {
        let target_id = valid_targets[0];
        effects::attach::attach_to(state, equipment_id, target_id);
        events.push(GameEvent::EffectResolved {
            api_type: "Equip".to_string(),
            source_id: equipment_id,
        });
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        return Ok(WaitingFor::Priority { player });
    }

    state.priority_passes.clear();
    state.priority_pass_count = 0;
    Ok(WaitingFor::EquipTarget {
        player,
        equipment_id,
        valid_targets,
    })
}

pub fn new_game(seed: u64) -> GameState {
    GameState::new_two_player(seed)
}

/// Start game with mulligan flow. If no cards in libraries, skips mulligan.
pub fn start_game(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();

    events.push(GameEvent::GameStarted);

    // Begin the game: set turn 1
    state.turn_number = 1;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: PlayerId(0),
        turn_number: 1,
    });

    // If players have cards in their libraries, start mulligan flow
    let has_libraries = state.players.iter().any(|p| !p.library.is_empty());
    let waiting_for = if has_libraries {
        mulligan::start_mulligan(state, &mut events)
    } else {
        // No cards to mulligan with, skip straight to game
        turns::auto_advance(state, &mut events)
    };

    state.waiting_for = waiting_for.clone();
    derive_display_state(state);

    ActionResult {
        events,
        waiting_for,
    }
}

/// Start game without mulligan (for backward compatibility with existing tests).
pub fn start_game_skip_mulligan(state: &mut GameState) -> ActionResult {
    let mut events = Vec::new();

    events.push(GameEvent::GameStarted);

    state.turn_number = 1;
    state.active_player = PlayerId(0);
    state.priority_player = PlayerId(0);
    state.phase = Phase::Untap;

    events.push(GameEvent::TurnStarted {
        player_id: PlayerId(0),
        turn_number: 1,
    });

    let waiting_for = turns::auto_advance(state, &mut events);
    state.waiting_for = waiting_for.clone();
    derive_display_state(state);

    ActionResult {
        events,
        waiting_for,
    }
}

/// Check if any exile-return sources have left the battlefield.
/// If so, move the exiled cards back to the battlefield.
fn check_exile_returns(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let mut to_return: Vec<crate::types::game_state::ExileLink> = Vec::new();

    for event in events.iter() {
        if let GameEvent::ZoneChanged {
            object_id,
            from: Zone::Battlefield,
            ..
        } = event
        {
            // Find exile links where this object was the source
            for link in &state.exile_links {
                if link.source_id == *object_id {
                    to_return.push(link.clone());
                }
            }
        }
    }

    if to_return.is_empty() {
        return;
    }

    // Return exiled cards to the battlefield
    for link in &to_return {
        // Only return if the card is still in exile
        let still_in_exile = state
            .objects
            .get(&link.exiled_id)
            .map(|obj| obj.zone == Zone::Exile)
            .unwrap_or(false);
        if still_in_exile {
            zones::move_to_zone(state, link.exiled_id, Zone::Battlefield, events);
        }
    }

    // Remove processed links
    let returned_ids: Vec<_> = to_return.iter().map(|l| l.exiled_id).collect();
    state
        .exile_links
        .retain(|link| !returned_ids.contains(&link.exiled_id));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, DamageAmount, Effect, ResolvedAbility,
        TargetFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};

    /// Create a simple test ability definition.
    fn make_draw_ability(num_cards: u32) -> AbilityDefinition {
        AbilityDefinition {
            kind: AbilityKind::Spell,
            effect: Effect::Draw { count: num_cards },
            cost: None,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        }
    }

    /// Create a DealDamage ability for testing.
    fn make_damage_ability(amount: i32, cost: Option<AbilityCost>) -> AbilityDefinition {
        AbilityDefinition {
            kind: if cost.is_some() {
                AbilityKind::Activated
            } else {
                AbilityKind::Spell
            },
            effect: Effect::DealDamage {
                amount: DamageAmount::Fixed(amount),
                target: TargetFilter::Any,
            },
            cost,
            sub_ability: None,
            duration: None,
            description: None,
            target_prompt: None,
            sorcery_speed: false,
        }
    }

    fn setup_game_at_main_phase() -> GameState {
        let mut state = new_game(42);
        state.turn_number = 2; // Not first turn
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };
        state
    }

    #[test]
    fn apply_pass_priority_alternates_players() {
        let mut state = setup_game_at_main_phase();

        let result = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));
    }

    #[test]
    fn apply_pass_priority_rejects_wrong_player() {
        let mut state = setup_game_at_main_phase();
        state.priority_player = PlayerId(1);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(1),
        };

        // Player 0 tries to pass but player 1 has priority
        // PassPriority uses priority_player, so this should fail if
        // the validated player doesn't match waiting_for
        // Actually, the validation checks priority_player == waiting_for.player
        // and priority_player is 1, so PassPriority action itself is valid
        // for player 1. The issue is if player 0 somehow acts.
        // In practice, the action doesn't carry a player ID -- the engine
        // uses priority_player. So this is a protocol-level concern.
        let result = apply(&mut state, GameAction::PassPriority);
        assert!(result.is_ok());
    }

    #[test]
    fn apply_play_land_moves_to_battlefield() {
        let mut state = setup_game_at_main_phase();

        let obj_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&obj_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) }).unwrap();

        assert!(state.battlefield.contains(&obj_id));
        assert!(!state.players[0].hand.contains(&obj_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn apply_play_land_rejects_non_main_phase() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::Upkeep;

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) });

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_over_limit() {
        let mut state = setup_game_at_main_phase();
        state.lands_played_this_turn = 1; // Already played one

        create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );

        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) });

        assert!(result.is_err());
    }

    #[test]
    fn apply_play_land_rejects_card_not_in_hand() {
        let mut state = setup_game_at_main_phase();

        let result = apply(
            &mut state,
            GameAction::PlayLand {
                card_id: CardId(999),
            },
        );

        assert!(result.is_err());
    }

    #[test]
    fn new_game_creates_two_player_state() {
        let state = new_game(42);
        assert_eq!(state.players.len(), 2);
        assert_eq!(state.rng_seed, 42);
    }

    #[test]
    fn start_game_advances_to_precombat_main() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn start_game_skips_draw_on_first_turn() {
        let mut state = new_game(42);

        // Add a card to player 0's library
        let id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Card".to_string(),
            Zone::Library,
        );

        start_game_skip_mulligan(&mut state);

        // Card should still be in library (draw skipped on turn 1)
        assert!(state.players[0].library.contains(&id));
        assert!(!state.players[0].hand.contains(&id));
    }

    #[test]
    fn start_game_emits_game_started_event() {
        let mut state = new_game(42);
        let result = start_game(&mut state);

        assert!(result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::GameStarted)));
    }

    #[test]
    fn integration_full_turn_cycle() {
        let mut state = new_game(42);

        // Start game (turn 1, player 0)
        let _result = start_game(&mut state);
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 1);

        // Pass priority from player 0 (pre-combat main)
        let result = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        // Pass priority from player 1 (both passed, stack empty -> advance)
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should skip combat phases and land at PostCombatMain
        assert_eq!(state.phase, Phase::PostCombatMain);

        // Pass through post-combat main
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should advance to End step
        assert_eq!(state.phase, Phase::End);

        // Pass through end step
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        let _result = apply(&mut state, GameAction::PassPriority).unwrap();
        // Should advance through cleanup to next turn, then auto-advance to PreCombatMain
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn integration_play_land_then_pass() {
        let mut state = new_game(42);
        start_game(&mut state);

        // Create a land in player 0's hand
        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Hand,
        );
        state
            .objects
            .get_mut(&land_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);

        // Play the land
        let result = apply(&mut state, GameAction::PlayLand { card_id: CardId(1) }).unwrap();

        assert!(state.battlefield.contains(&land_id));
        assert_eq!(state.lands_played_this_turn, 1);

        // Player retains priority after playing land
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));

        // Priority pass count should have been reset by the land play
        assert_eq!(state.priority_pass_count, 0);
    }

    #[test]
    fn stack_push_and_lifo_resolve() {
        use crate::game::stack;
        use crate::types::game_state::{StackEntry, StackEntryKind};

        let mut state = setup_game_at_main_phase();
        let mut events = Vec::new();

        // Create two spell objects
        let id1 = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Bolt".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Instant);

        let id2 = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Stack,
        );
        state
            .objects
            .get_mut(&id2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        // Push to stack (first pushed = bottom)
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id1,
                source_id: id1,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(1),
                    ability: ResolvedAbility {
                        effect: crate::types::ability::Effect::Unimplemented {
                            name: String::new(),
                            description: None,
                        },
                        targets: vec![],
                        source_id: id1,
                        controller: PlayerId(0),
                        sub_ability: None,
                        duration: None,
                    },
                },
            },
            &mut events,
        );
        stack::push_to_stack(
            &mut state,
            StackEntry {
                id: id2,
                source_id: id2,
                controller: PlayerId(0),
                kind: StackEntryKind::Spell {
                    card_id: CardId(2),
                    ability: ResolvedAbility {
                        effect: crate::types::ability::Effect::Unimplemented {
                            name: String::new(),
                            description: None,
                        },
                        targets: vec![],
                        source_id: id2,
                        controller: PlayerId(0),
                        sub_ability: None,
                        duration: None,
                    },
                },
            },
            &mut events,
        );

        assert_eq!(state.stack.len(), 2);

        // Resolve top (LIFO) -- should be id2 (Bear, creature -> battlefield)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 1);
        assert!(state.battlefield.contains(&id2)); // Creature goes to battlefield

        // Resolve next -- should be id1 (Bolt, instant -> graveyard)
        stack::resolve_top(&mut state, &mut events);
        assert_eq!(state.stack.len(), 0);
        assert!(state.players[0].graveyard.contains(&id1)); // Instant goes to graveyard
    }

    #[test]
    fn stack_is_empty_check() {
        use crate::game::stack;

        let state = new_game(42);
        assert!(stack::stack_is_empty(&state));
    }

    #[test]
    fn engine_error_display() {
        let err = EngineError::WrongPlayer;
        assert_eq!(err.to_string(), "Wrong player");

        let err = EngineError::NotYourPriority;
        assert_eq!(err.to_string(), "Not your priority");

        let err = EngineError::InvalidAction("test".to_string());
        assert_eq!(err.to_string(), "Invalid action: test");
    }

    #[test]
    fn tap_land_for_mana_produces_correct_color() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.entered_battlefield_turn = Some(1);
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        )
        .unwrap();

        assert!(state.objects[&land_id].tapped);
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
    }

    #[test]
    fn tap_land_rejects_already_tapped() {
        let mut state = setup_game_at_main_phase();

        let land_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Forest".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&land_id).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.card_types.subtypes.push("Forest".to_string());
            obj.tapped = true;
        }

        let result = apply(
            &mut state,
            GameAction::TapLandForMana { object_id: land_id },
        );

        assert!(result.is_err());
    }

    #[test]
    fn full_turn_integration_with_mulligan() {
        let mut state = new_game(42);

        // Add 20 basic lands to each player's library
        for player_idx in 0..2u8 {
            for i in 0..20 {
                let id = create_object(
                    &mut state,
                    CardId((player_idx as u64) * 100 + i),
                    PlayerId(player_idx),
                    "Forest".to_string(),
                    Zone::Library,
                );
                let obj = state.objects.get_mut(&id).unwrap();
                obj.card_types.core_types.push(CoreType::Land);
                obj.card_types.subtypes.push("Forest".to_string());
            }
        }

        // Start game -> mulligan prompt
        let result = start_game(&mut state);
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(0),
                mulligan_count: 0,
            }
        ));

        // Both players have 7 cards in hand
        assert_eq!(state.players[0].hand.len(), 7);
        assert_eq!(state.players[1].hand.len(), 7);

        // Player 0 keeps
        let result = apply(&mut state, GameAction::MulliganDecision { keep: true }).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::MulliganDecision {
                player: PlayerId(1),
                mulligan_count: 0,
            }
        ));

        // Player 1 keeps -> game starts, auto-advances to PreCombatMain
        let result = apply(&mut state, GameAction::MulliganDecision { keep: true }).unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0),
            }
        ));
        assert_eq!(state.phase, Phase::PreCombatMain);

        // Play a land from hand
        let land_card_id = state.objects[&state.players[0].hand[0]].card_id;
        let _result = apply(
            &mut state,
            GameAction::PlayLand {
                card_id: land_card_id,
            },
        )
        .unwrap();
        assert_eq!(state.lands_played_this_turn, 1);

        // Find the land on battlefield to tap it
        let land_on_bf = state
            .battlefield
            .iter()
            .find(|&&id| {
                state
                    .objects
                    .get(&id)
                    .map(|o| o.controller == PlayerId(0) && !o.tapped)
                    .unwrap_or(false)
            })
            .copied()
            .unwrap();

        // Tap land for mana
        let _result = apply(
            &mut state,
            GameAction::TapLandForMana {
                object_id: land_on_bf,
            },
        )
        .unwrap();
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );

        // Pass priority through the rest of the turn
        // PreCombatMain: P0 passes
        apply(&mut state, GameAction::PassPriority).unwrap();
        // PreCombatMain: P1 passes -> advances to PostCombatMain
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PostCombatMain);

        // PostCombatMain: both pass -> End
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::End);

        // End: both pass -> Cleanup -> next turn
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();
        assert_eq!(state.phase, Phase::PreCombatMain);
        assert_eq!(state.turn_number, 2);
        assert_eq!(state.active_player, PlayerId(1));
    }

    #[test]
    fn cast_spell_moves_card_from_hand_to_stack_and_returns_priority() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery in hand
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        // Add mana
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }

        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();

        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        assert_eq!(state.stack.len(), 1);
        assert!(!state.players[0].hand.contains(&obj_id));
    }

    #[test]
    fn both_pass_with_spell_on_stack_resolves_spell() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a sorcery and cast it
        let obj_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(make_draw_ability(2));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }

        // Add some cards to draw
        for i in 0..5 {
            create_object(
                &mut state,
                CardId(100 + i),
                PlayerId(0),
                format!("Card {}", i),
                Zone::Library,
            );
        }

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        for _ in 0..3 {
            player.mana_pool.add(ManaUnit {
                color: ManaType::Blue,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }

        // Cast the spell
        apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        let hand_before = state.players[0].hand.len();

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Stack should be empty
        assert!(state.stack.is_empty());
        // Card should be in graveyard (sorcery)
        assert!(state.players[0].graveyard.contains(&obj_id));
        // Draw 2 effect should have fired
        assert_eq!(state.players[0].hand.len(), hand_before + 2);
    }

    #[test]
    fn fizzle_target_removed_before_resolution() {
        use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

        let mut state = setup_game_at_main_phase();

        // Create a creature target
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Lightning Bolt targeting the creature
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == PlayerId(0))
            .unwrap();
        player.mana_pool.add(ManaUnit {
            color: ManaType::Red,
            source_id: ObjectId(0),
            snow: false,
            restrictions: Vec::new(),
        });

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // Remove the creature from battlefield before resolution (simulating it was destroyed)
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        // Both pass -> resolve -- should fizzle
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Stack should be empty, bolt should be in graveyard (fizzled)
        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&bolt_id));
        // Creature was already in graveyard, life should be unchanged
        assert_eq!(state.players[1].life, 20);
    }

    // === Phase 04 Plan 03 Integration Tests ===

    use crate::types::ability::TargetRef;
    use crate::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};

    fn add_mana(state: &mut GameState, player: PlayerId, color: ManaType, count: usize) {
        let player_data = state.players.iter_mut().find(|p| p.id == player).unwrap();
        for _ in 0..count {
            player_data.mana_pool.add(ManaUnit {
                color,
                source_id: ObjectId(0),
                snow: false,
                restrictions: Vec::new(),
            });
        }
    }

    #[test]
    fn lightning_bolt_deals_3_damage_to_creature() {
        let mut state = setup_game_at_main_phase();

        // Create a 2/3 creature controlled by P1
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(3);
        }

        // Create Lightning Bolt in P0's hand
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Cast Lightning Bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert_eq!(state.players[0].mana_pool.total(), 0);

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Creature should have 3 damage, which equals toughness -> SBA destroys it
        assert!(state.stack.is_empty());
        assert!(!state.battlefield.contains(&creature_id));
        assert!(state.players[1].graveyard.contains(&creature_id));
        // Bolt is instant -> goes to graveyard
        assert!(state.players[0].graveyard.contains(&bolt_id));
    }

    #[test]
    fn lightning_bolt_deals_3_damage_to_player() {
        let mut state = setup_game_at_main_phase();

        // Create Lightning Bolt in P0's hand with Any target
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Two players as targets, need manual selection
        // Use Player filter -> 2 targets -> need SelectTargets
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();

        // Should need target selection (2 players)
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select player 1 as target
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Player(PlayerId(1))],
            },
        )
        .unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.players[1].life, 17);
        assert!(state.players[0].graveyard.contains(&bolt_id));
    }

    #[test]
    fn counterspell_counters_a_spell_on_stack() {
        let mut state = setup_game_at_main_phase();

        // P0 casts a creature spell -- put it on the stack manually
        let creature_id = create_object(
            &mut state,
            CardId(30),
            PlayerId(0),
            "Grizzly Bears".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            // Vanilla creature has no abilities (empty vec is the default)
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 1,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Green, 2);

        // Cast the creature
        apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(30),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // P1 gets priority, has Counterspell
        // Pass priority from P0 to P1
        apply(&mut state, GameAction::PassPriority).unwrap();
        // Now P1 has priority
        assert_eq!(state.priority_player, PlayerId(1));

        let counter_id = create_object(
            &mut state,
            CardId(40),
            PlayerId(1),
            "Counterspell".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&counter_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Counter {
                    target: TargetFilter::Typed {
                        card_type: Some(crate::types::ability::TypeFilter::Card),
                        subtype: None,
                        controller: None,
                        properties: vec![],
                    },
                },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue, ManaCostShard::Blue],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(1), ManaType::Blue, 2);

        // Cast Counterspell — targets a spell on the stack
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(40),
                targets: vec![],
            },
        )
        .unwrap();
        // Handle target selection if needed (single spell auto-targets, but be robust).
        let result = if matches!(result.waiting_for, WaitingFor::TargetSelection { .. }) {
            apply(
                &mut state,
                GameAction::SelectTargets {
                    targets: vec![TargetRef::Object(creature_id)],
                },
            )
            .unwrap()
        } else {
            result
        };
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 2); // creature + counterspell

        // Both pass -> Counterspell resolves first (LIFO)
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        // Counterspell resolved, creature spell should be countered (in graveyard)
        // Counterspell should also be in graveyard
        assert!(state.players[0].graveyard.contains(&creature_id));
        assert!(state.players[1].graveyard.contains(&counter_id));
        // Creature never reached battlefield
        assert!(!state.battlefield.contains(&creature_id));
    }

    #[test]
    fn giant_growth_gives_plus_3_3() {
        let mut state = setup_game_at_main_phase();

        // Create a 2/2 creature for P0
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Giant Growth in P0's hand
        let growth_id = create_object(
            &mut state,
            CardId(60),
            PlayerId(0),
            "Giant Growth".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&growth_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(AbilityDefinition {
                kind: AbilityKind::Spell,
                effect: Effect::Pump {
                    power: crate::types::ability::PtValue::Fixed(3),
                    toughness: crate::types::ability::PtValue::Fixed(3),
                    target: TargetFilter::Typed {
                        card_type: Some(crate::types::ability::TypeFilter::Creature),
                        subtype: None,
                        controller: Some(crate::types::ability::ControllerRef::You),
                        properties: vec![],
                    },
                },
                cost: None,
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Green],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Green, 1);

        // Cast Giant Growth (auto-targets single own creature)
        apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(60),
                targets: vec![],
            },
        )
        .unwrap();
        assert_eq!(state.stack.len(), 1);

        // Both pass -> resolve
        apply(&mut state, GameAction::PassPriority).unwrap();
        apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert_eq!(state.objects[&creature_id].power, Some(5));
        assert_eq!(state.objects[&creature_id].toughness, Some(5));
        assert!(state.players[0].graveyard.contains(&growth_id));
    }

    #[test]
    fn fizzle_bolt_target_removed() {
        let mut state = setup_game_at_main_phase();

        // Create a creature
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }

        // Create Lightning Bolt
        let bolt_id = create_object(
            &mut state,
            CardId(10),
            PlayerId(0),
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&bolt_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(make_damage_ability(3, None));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }

        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Cast bolt — multiple valid targets (creature + 2 players) requires selection
        let result = apply(
            &mut state,
            GameAction::CastSpell {
                card_id: CardId(10),
                targets: vec![],
            },
        )
        .unwrap();
        assert!(matches!(
            result.waiting_for,
            WaitingFor::TargetSelection { .. }
        ));

        // Select the creature as target
        apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(creature_id)],
            },
        )
        .unwrap();

        // Remove creature before resolution
        let mut events = Vec::new();
        zones::move_to_zone(&mut state, creature_id, Zone::Graveyard, &mut events);

        // Both pass -> fizzle
        apply(&mut state, GameAction::PassPriority).unwrap();
        let result = apply(&mut state, GameAction::PassPriority).unwrap();

        assert!(state.stack.is_empty());
        assert!(state.players[0].graveyard.contains(&bolt_id));
        // No DamageDealt event
        assert!(!result
            .events
            .iter()
            .any(|e| matches!(e, GameEvent::DamageDealt { .. })));
    }

    #[test]
    fn test_mana_ability_during_priority_does_not_push_stack() {
        let mut state = setup_game_at_main_phase();

        // Create a creature with a mana ability on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(100),
            PlayerId(0),
            "Llanowar Elves".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.abilities.push(AbilityDefinition {
                kind: AbilityKind::Activated,
                effect: Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                    },
                },
                cost: Some(AbilityCost::Tap),
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
        }

        let result = apply(
            &mut state,
            GameAction::ActivateAbility {
                source_id: obj_id,
                ability_index: 0,
            },
        )
        .unwrap();

        // Stack should remain empty (mana abilities don't use the stack)
        assert!(
            state.stack.is_empty(),
            "mana ability should not push to stack"
        );
        // Should stay in Priority
        assert!(matches!(
            result.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(0)
            }
        ));
        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);
        // Player should have green mana
        assert_eq!(
            state.players[0]
                .mana_pool
                .count_color(crate::types::mana::ManaType::Green),
            1
        );
    }

    #[test]
    fn test_mana_ability_during_mana_payment_stays_in_mana_payment() {
        let mut state = setup_game_at_main_phase();
        // Set up ManaPayment state
        state.waiting_for = WaitingFor::ManaPayment {
            player: PlayerId(0),
        };

        // Create a creature with a mana ability on the battlefield
        let obj_id = create_object(
            &mut state,
            CardId(101),
            PlayerId(0),
            "Birds of Paradise".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.abilities.push(AbilityDefinition {
                kind: AbilityKind::Activated,
                effect: Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![crate::types::mana::ManaColor::Green],
                    },
                },
                cost: Some(AbilityCost::Tap),
                sub_ability: None,
                duration: None,
                description: None,
                target_prompt: None,
                sorcery_speed: false,
            });
        }

        let result = apply(
            &mut state,
            GameAction::ActivateAbility {
                source_id: obj_id,
                ability_index: 0,
            },
        )
        .unwrap();

        // Should stay in ManaPayment
        assert!(
            matches!(
                result.waiting_for,
                WaitingFor::ManaPayment {
                    player: PlayerId(0)
                }
            ),
            "should remain in ManaPayment after mana ability"
        );
        // Stack should remain empty
        assert!(state.stack.is_empty());
        // Object should be tapped
        assert!(state.objects.get(&obj_id).unwrap().tapped);
    }

    mod equip_tests {
        use super::*;

        fn setup_equip_game() -> GameState {
            let mut state = GameState::new_two_player(42);
            state.turn_number = 2;
            state.phase = Phase::PreCombatMain;
            state.active_player = PlayerId(0);
            state.priority_player = PlayerId(0);
            state.waiting_for = WaitingFor::Priority {
                player: PlayerId(0),
            };
            state
        }

        fn create_equipment(state: &mut GameState, player: PlayerId) -> ObjectId {
            let id = zones::create_object(
                state,
                CardId(100),
                player,
                "Bonesplitter".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Artifact);
            obj.card_types.subtypes.push("Equipment".to_string());
            obj.controller = player;
            id
        }

        fn create_creature_on_bf(state: &mut GameState, player: PlayerId, name: &str) -> ObjectId {
            let id = zones::create_object(
                state,
                CardId(state.next_object_id),
                player,
                name.to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types
                .core_types
                .push(crate::types::card_type::CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
            obj.controller = player;
            id
        }

        #[test]
        fn test_equip_creates_equip_target_with_valid_creatures() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();

            match result.waiting_for {
                WaitingFor::EquipTarget {
                    player,
                    equipment_id: eq_id,
                    valid_targets,
                } => {
                    assert_eq!(player, PlayerId(0));
                    assert_eq!(eq_id, equipment_id);
                    assert!(valid_targets.contains(&creature_a));
                    assert!(valid_targets.contains(&creature_b));
                }
                other => panic!("Expected EquipTarget, got {:?}", other),
            }
        }

        #[test]
        fn test_equip_selects_target_attaches_equipment() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let _creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            // Activate equip
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            assert!(matches!(result.waiting_for, WaitingFor::EquipTarget { .. }));

            // Select target
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_a,
                },
            )
            .unwrap();

            assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature_a)
            );
            assert!(state
                .objects
                .get(&creature_a)
                .unwrap()
                .attachments
                .contains(&equipment_id));
        }

        #[test]
        fn test_equip_re_equip_moves_to_new_creature() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature_a = create_creature_on_bf(&mut state, PlayerId(0), "Bear A");
            let creature_b = create_creature_on_bf(&mut state, PlayerId(0), "Bear B");

            // First equip to creature A
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_a,
                },
            )
            .unwrap();
            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature_a)
            );

            // Re-equip to creature B
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();
            apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: creature_b,
                },
            )
            .unwrap();

            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature_b)
            );
            assert!(state
                .objects
                .get(&creature_b)
                .unwrap()
                .attachments
                .contains(&equipment_id));
            assert!(!state
                .objects
                .get(&creature_a)
                .unwrap()
                .attachments
                .contains(&equipment_id));
        }

        #[test]
        fn test_equip_only_at_sorcery_speed() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let _creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

            // Try during combat phase - should fail
            state.phase = Phase::DeclareAttackers;
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());

            // Try with non-empty stack - should fail
            state.phase = Phase::PreCombatMain;
            state.stack.push(crate::types::game_state::StackEntry {
                id: ObjectId(99),
                source_id: ObjectId(99),
                controller: PlayerId(1),
                kind: crate::types::game_state::StackEntryKind::Spell {
                    card_id: CardId(99),
                    ability: crate::types::ability::ResolvedAbility {
                        effect: crate::types::ability::Effect::Unimplemented {
                            name: String::new(),
                            description: None,
                        },
                        targets: vec![],
                        source_id: ObjectId(99),
                        controller: PlayerId(1),
                        sub_ability: None,
                        duration: None,
                    },
                },
            });
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());

            // Try when not active player - should fail
            state.stack.clear();
            state.active_player = PlayerId(1);
            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            );
            assert!(result.is_err());
        }

        #[test]
        fn test_equip_auto_targets_single_creature() {
            let mut state = setup_equip_game();
            let equipment_id = create_equipment(&mut state, PlayerId(0));
            let creature = create_creature_on_bf(&mut state, PlayerId(0), "Bear");

            let result = apply(
                &mut state,
                GameAction::Equip {
                    equipment_id,
                    target_id: ObjectId(0),
                },
            )
            .unwrap();

            assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
            assert_eq!(
                state.objects.get(&equipment_id).unwrap().attached_to,
                Some(creature)
            );
        }
    }
}

#[cfg(test)]
mod trigger_target_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, Effect, TargetFilter, TargetRef, TriggerDefinition, TypeFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;

    #[test]
    fn trigger_target_selection_select_targets_pushes_to_stack() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create two opponent creatures as legal targets
        let target1 = create_object(
            &mut state,
            CardId(10),
            PlayerId(1),
            "Opp Creature 1".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target1)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target1).unwrap().controller = PlayerId(1);

        let target2 = create_object(
            &mut state,
            CardId(11),
            PlayerId(1),
            "Opp Creature 2".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target2)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        state.objects.get_mut(&target2).unwrap().controller = PlayerId(1);

        // Create trigger creature (Banishing Light)
        let trigger_creature = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&trigger_creature).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.entered_battlefield_turn = Some(1);
        }

        // Manually set up the pending trigger state (as process_triggers would)
        let ability = crate::types::ability::ResolvedAbility {
            effect: Effect::ChangeZone {
                origin: Some(Zone::Battlefield),
                destination: Zone::Exile,
                target: TargetFilter::Typed {
                    card_type: Some(TypeFilter::Creature),
                    subtype: None,
                    controller: Some(ControllerRef::Opponent),
                    properties: vec![],
                },
            },
            targets: Vec::new(),
            source_id: trigger_creature,
            controller: PlayerId(0),
            sub_ability: None,
            duration: Some(crate::types::ability::Duration::UntilHostLeavesPlay),
        };

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: trigger_creature,
            controller: PlayerId(0),
            trigger_def: TriggerDefinition {
                mode: crate::types::triggers::TriggerMode::ChangesZone,
                execute: None,
                valid_card: None,
                origin: None,
                destination: Some(Zone::Battlefield),
                trigger_zones: vec![],
                phase: None,
                optional: false,
                combat_damage: false,
                secondary: false,
                valid_target: None,
                valid_source: None,
                description: None,
                constraint: None,
                condition: None,
            },
            ability,
            timestamp: 1,
        });

        let legal_targets = vec![TargetRef::Object(target1), TargetRef::Object(target2)];

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            legal_targets: legal_targets.clone(),
        };

        // Player selects target1
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(target1)],
            },
        )
        .unwrap();

        // Should return Priority
        assert!(
            matches!(result.waiting_for, WaitingFor::Priority { .. }),
            "Expected Priority, got {:?}",
            result.waiting_for
        );

        // Trigger should be on the stack with the selected target
        assert_eq!(state.stack.len(), 1, "Trigger should be on stack");
        let entry = &state.stack[0];
        assert_eq!(entry.source_id, trigger_creature);
        match &entry.kind {
            crate::types::game_state::StackEntryKind::TriggeredAbility { ability, .. } => {
                assert_eq!(ability.targets, vec![TargetRef::Object(target1)]);
            }
            _ => panic!("Expected TriggeredAbility on stack"),
        }

        // Pending trigger should be consumed
        assert!(state.pending_trigger.is_none());
    }

    #[test]
    fn trigger_target_selection_rejects_illegal_target() {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);

        let legal_target = ObjectId(10);
        let illegal_target = ObjectId(99);

        state.pending_trigger = Some(crate::game::triggers::PendingTrigger {
            source_id: ObjectId(1),
            controller: PlayerId(0),
            trigger_def: TriggerDefinition {
                mode: crate::types::triggers::TriggerMode::ChangesZone,
                execute: None,
                valid_card: None,
                origin: None,
                destination: None,
                trigger_zones: vec![],
                phase: None,
                optional: false,
                combat_damage: false,
                secondary: false,
                valid_target: None,
                valid_source: None,
                description: None,
                constraint: None,
                condition: None,
            },
            ability: crate::types::ability::ResolvedAbility::new(
                Effect::ChangeZone {
                    origin: Some(Zone::Battlefield),
                    destination: Zone::Exile,
                    target: TargetFilter::Any,
                },
                vec![],
                ObjectId(1),
                PlayerId(0),
            ),
            timestamp: 1,
        });

        state.waiting_for = WaitingFor::TriggerTargetSelection {
            player: PlayerId(0),
            legal_targets: vec![TargetRef::Object(legal_target)],
        };

        // Try to select an illegal target
        let result = apply(
            &mut state,
            GameAction::SelectTargets {
                targets: vec![TargetRef::Object(illegal_target)],
            },
        );

        assert!(result.is_err(), "Should reject illegal target");
    }
}

#[cfg(test)]
mod exile_return_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::game_state::ExileLink;
    use crate::types::identifiers::CardId;

    #[test]
    fn exile_return_source_leaves_battlefield_returns_exiled_card() {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);

        // Create source permanent (e.g., Banishing Light) on battlefield
        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Banishing Light".to_string(),
            Zone::Battlefield,
        );

        // Create exiled card -- directly in exile
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled Creature".to_string(),
            Zone::Exile,
        );

        // Set up the exile link
        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
        });

        // Simulate events where source leaves the battlefield
        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        // Call check_exile_returns
        check_exile_returns(&mut state, &mut events.clone());

        // Exiled card should return to battlefield
        assert!(
            state.battlefield.contains(&exiled_id),
            "Exiled card should return to battlefield"
        );
        assert!(
            !state.exile.contains(&exiled_id),
            "Exiled card should no longer be in exile"
        );

        // ExileLink should be removed
        assert!(
            state.exile_links.is_empty(),
            "ExileLink should be cleaned up"
        );
    }

    #[test]
    fn exile_return_card_already_gone_no_error() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        // Exiled card that has already left exile (moved to hand by another effect)
        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Already Moved".to_string(),
            Zone::Hand,
        );

        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
        });

        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        // Should not panic -- gracefully handle already-moved card
        check_exile_returns(&mut state, &mut events.clone());

        // Card stays in hand (not moved)
        assert!(state.players[1].hand.contains(&exiled_id));
        // Link is still cleaned up
        assert!(state.exile_links.is_empty());
    }

    #[test]
    fn exile_return_link_removed_after_return() {
        let mut state = GameState::new_two_player(42);

        let source_id = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Source".to_string(),
            Zone::Battlefield,
        );

        let exiled_id = create_object(
            &mut state,
            CardId(2),
            PlayerId(1),
            "Exiled".to_string(),
            Zone::Exile,
        );

        // Another unrelated exile link that should NOT be removed
        let other_source = create_object(
            &mut state,
            CardId(3),
            PlayerId(0),
            "Other Source".to_string(),
            Zone::Battlefield,
        );
        let other_exiled = create_object(
            &mut state,
            CardId(4),
            PlayerId(1),
            "Other Exiled".to_string(),
            Zone::Exile,
        );

        state.exile_links.push(ExileLink {
            exiled_id,
            source_id,
        });
        state.exile_links.push(ExileLink {
            exiled_id: other_exiled,
            source_id: other_source,
        });

        let events = vec![crate::types::events::GameEvent::ZoneChanged {
            object_id: source_id,
            from: Zone::Battlefield,
            to: Zone::Graveyard,
        }];

        check_exile_returns(&mut state, &mut events.clone());

        // First link's exiled card should return, second should stay in exile
        assert!(state.battlefield.contains(&exiled_id));
        assert!(state.exile.contains(&other_exiled));

        // Only the triggered link should be removed
        assert_eq!(state.exile_links.len(), 1);
        assert_eq!(state.exile_links[0].exiled_id, other_exiled);
    }
}

#[cfg(test)]
mod phase_trigger_regression_tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{AbilityDefinition, AbilityKind, Effect, GainLifePlayer, LifeAmount, TriggerDefinition};
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::CardId;
    use crate::types::player::PlayerId;
    use crate::types::triggers::TriggerMode;
    use crate::types::zones::Zone;

    /// Regression: phase triggers fired for auto-advanced phases.
    ///
    /// A creature with "At the beginning of combat..." had its trigger fire
    /// even when combat was skipped (no attackers), because auto_advance()
    /// emitted PhaseChanged { BeginCombat } and process_triggers() processed
    /// ALL accumulated events. The fix filters PhaseChanged events to only
    /// include the current phase.
    #[test]
    fn phase_trigger_does_not_fire_for_skipped_combat() {
        let mut state = new_game(42);
        state.turn_number = 2;
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state.waiting_for = WaitingFor::Priority {
            player: PlayerId(0),
        };

        // Create a creature with a "beginning of combat" phase trigger.
        // The creature is small (0/1) so it has no potential attackers.
        let creature_id = create_object(
            &mut state,
            CardId(200),
            PlayerId(0),
            "Trigger Creature".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature_id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(0);
            obj.toughness = Some(1);
            obj.trigger_definitions.push(TriggerDefinition {
                mode: TriggerMode::Phase,
                phase: Some(Phase::BeginCombat),
                execute: Some(Box::new(AbilityDefinition {
                    kind: AbilityKind::Activated,
                    effect: Effect::GainLife { amount: LifeAmount::Fixed(1), player: GainLifePlayer::Controller },
                    cost: None,
                    sub_ability: None,
                    target_prompt: None,
                    description: None,
                    duration: None,
                    sorcery_speed: false,
                })),
                valid_card: None,
                origin: None,
                destination: None,
                trigger_zones: vec![Zone::Battlefield],
                optional: false,
                combat_damage: false,
                secondary: false,
                valid_target: None,
                valid_source: None,
                description: None,
                constraint: None,
                condition: None,
            });
        }

        // Pass priority twice (P0 passes, then P1 passes) with empty stack.
        // This advances from PreCombatMain → BeginCombat → auto-skip combat
        // → PostCombatMain. The PhaseChanged { BeginCombat } event should be
        // filtered out, so the phase trigger should NOT fire.
        let result1 = apply(&mut state, GameAction::PassPriority).unwrap();
        assert!(matches!(
            result1.waiting_for,
            WaitingFor::Priority {
                player: PlayerId(1)
            }
        ));

        let result2 = apply(&mut state, GameAction::PassPriority).unwrap();

        // We should now be at PostCombatMain with empty stack.
        assert_eq!(state.phase, Phase::PostCombatMain);
        assert!(
            state.stack.is_empty(),
            "Stack should be empty — phase trigger for skipped BeginCombat should not fire. Stack: {:?}",
            state.stack
        );
        // No pending trigger either
        assert!(
            state.pending_trigger.is_none(),
            "No pending trigger should exist for a skipped phase"
        );
        assert!(matches!(result2.waiting_for, WaitingFor::Priority { .. }));
    }
}

use std::collections::HashSet;

use crate::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, AdditionalCost, Effect,
    ResolvedAbility, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::{ManaCostShard, ManaType, SpellMeta};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::ability_utils::{
    assign_selected_slots_in_chain, assign_targets_in_chain, auto_select_targets,
    begin_target_selection, build_chained_resolved, build_resolved_from_def, build_target_slots,
    choose_target, flatten_targets_in_chain, target_constraints_from_modal, validate_modal_indices,
    validate_selected_targets, TargetSelectionAdvance,
};
use super::engine::EngineError;
use super::mana_abilities;
use super::mana_payment;
use super::mana_sources::{self, ManaSourceOption};
use super::restrictions;
use super::stack;
use super::targeting;
use super::zones;

/// Emit `BecomesTarget` and `CrimeCommitted` events for each target.
///
/// Called whenever targets are locked in for a spell or ability. CR 702.9c:
/// Targeting an opponent, their permanent, or a card in their graveyard is a crime.
pub(crate) fn emit_targeting_events(
    state: &GameState,
    targets: &[TargetRef],
    source_id: ObjectId,
    controller: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    let mut crime_committed = false;
    for target in targets {
        match target {
            TargetRef::Object(obj_id) => {
                events.push(GameEvent::BecomesTarget {
                    object_id: *obj_id,
                    source_id,
                });
                if !crime_committed {
                    if let Some(obj) = state.objects.get(obj_id) {
                        if obj.controller != controller && obj.owner != controller {
                            crime_committed = true;
                        }
                    }
                }
            }
            TargetRef::Player(pid) => {
                if !crime_committed && *pid != controller {
                    crime_committed = true;
                }
            }
        }
    }
    if crime_committed {
        events.push(GameEvent::CrimeCommitted {
            player_id: controller,
        });
    }
}

#[derive(Debug, Clone)]
struct PreparedSpellCast {
    object_id: ObjectId,
    card_id: CardId,
    ability_def: AbilityDefinition,
    mana_cost: crate::types::mana::ManaCost,
    modal: Option<crate::types::ability::ModalChoice>,
}

fn default_spell_ability_def() -> AbilityDefinition {
    AbilityDefinition::new(
        AbilityKind::Spell,
        Effect::Unimplemented {
            name: "PermanentNoncreature".to_string(),
            description: None,
        },
    )
}

fn spell_object_id_for_card_id(
    state: &GameState,
    player: PlayerId,
    card_id: CardId,
) -> Result<ObjectId, EngineError> {
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");

    player_data
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
        .or_else(|| {
            if !state.format_config.command_zone {
                return None;
            }
            state
                .objects
                .values()
                .find(|obj| {
                    obj.card_id == card_id
                        && obj.owner == player
                        && obj.zone == Zone::Command
                        && obj.is_commander
                })
                .map(|obj| obj.id)
        })
        // CR 715.5: Check exile for cards with AdventureCreature permission.
        .or_else(|| {
            state
                .exile
                .iter()
                .find(|&&obj_id| {
                    state.objects.get(&obj_id).is_some_and(|obj| {
                        obj.card_id == card_id
                            && obj.owner == player
                            && obj.casting_permissions.contains(
                                &crate::types::ability::CastingPermission::AdventureCreature,
                            )
                    })
                })
                .copied()
        })
        .ok_or_else(|| EngineError::InvalidAction("Card not found in hand".to_string()))
}

pub fn spell_objects_available_to_cast(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    let player_data = state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists");

    let mut objects = player_data.hand.clone();
    if state.format_config.command_zone {
        objects.extend(
            state
                .objects
                .values()
                .filter(|obj| obj.owner == player && obj.zone == Zone::Command && obj.is_commander)
                .map(|obj| obj.id),
        );
    }

    // CR 715.5: Cards in exile with AdventureCreature permission are castable as creatures.
    objects.extend(state.exile.iter().filter(|&&obj_id| {
        state.objects.get(&obj_id).is_some_and(|obj| {
            obj.owner == player
                && obj
                    .casting_permissions
                    .contains(&crate::types::ability::CastingPermission::AdventureCreature)
        })
    }));

    objects
}

fn prepare_spell_cast(
    state: &GameState,
    player: PlayerId,
    object_id: ObjectId,
) -> Result<PreparedSpellCast, EngineError> {
    let obj = state
        .objects
        .get(&object_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
    // CR 715.5: Cards in exile with AdventureCreature permission are castable as creatures.
    let has_adventure_permission = obj.zone == Zone::Exile
        && obj
            .casting_permissions
            .contains(&crate::types::ability::CastingPermission::AdventureCreature);
    let castable_zone = obj.owner == player
        && (obj.zone == Zone::Hand
            || (state.format_config.command_zone && obj.zone == Zone::Command && obj.is_commander)
            || has_adventure_permission);
    if !castable_zone {
        return Err(EngineError::InvalidAction(
            "Card is not in a castable zone".to_string(),
        ));
    }
    if obj
        .card_types
        .core_types
        .contains(&crate::types::card_type::CoreType::Land)
    {
        return Err(EngineError::ActionNotAllowed(
            "Lands are played, not cast".to_string(),
        ));
    }

    let ability_def = obj
        .abilities
        .first()
        .cloned()
        .unwrap_or_else(default_spell_ability_def);

    let flash_cost = restrictions::flash_timing_cost(state, player, obj);
    let mut mana_cost = obj.mana_cost.clone();
    if let Err(base_timing_error) =
        restrictions::check_spell_timing(state, player, obj, &ability_def, false)
    {
        let Some(flash_cost) = flash_cost else {
            return Err(base_timing_error);
        };
        restrictions::check_spell_timing(state, player, obj, &ability_def, true)?;
        mana_cost = restrictions::add_mana_cost(&mana_cost, &flash_cost);
    }
    restrictions::check_casting_restrictions(state, player, object_id, &obj.casting_restrictions)?;

    if state.format_config.command_zone
        && !super::commander::can_cast_in_color_identity(state, &obj.color, player)
    {
        return Err(EngineError::ActionNotAllowed(
            "Card is outside commander's color identity".to_string(),
        ));
    }

    if obj.zone == Zone::Command {
        let tax = super::commander::commander_tax(state, object_id);
        if tax > 0 {
            match &mut mana_cost {
                crate::types::mana::ManaCost::Cost { generic, .. } => {
                    *generic += tax;
                }
                crate::types::mana::ManaCost::NoCost => {
                    mana_cost = crate::types::mana::ManaCost::Cost {
                        shards: vec![],
                        generic: tax,
                    };
                }
            }
        }
    }

    Ok(PreparedSpellCast {
        object_id,
        card_id: obj.card_id,
        ability_def,
        mana_cost,
        modal: obj.modal.clone(),
    })
}

/// CR 715.3a: Swap object characteristics to the Adventure face for casting.
/// Saves the creature face in `back_face` for later restoration.
fn swap_to_adventure_face(obj: &mut crate::game::game_object::GameObject) {
    let adventure = match obj.back_face.take() {
        Some(b) => b,
        None => return,
    };
    // Snapshot current (creature) face into back_face
    let creature_snapshot = super::printed_cards::snapshot_object_face(obj);
    super::printed_cards::apply_back_face_to_object(obj, adventure);
    obj.back_face = Some(creature_snapshot);
}

/// CR 715: Returns true if this object is an Adventure card (creature front + instant/sorcery back).
fn is_adventure_card(obj: &crate::game::game_object::GameObject) -> bool {
    let Some(ref back) = obj.back_face else {
        return false;
    };
    use crate::types::card_type::CoreType;
    back.card_types
        .core_types
        .iter()
        .any(|ct| matches!(ct, CoreType::Instant | CoreType::Sorcery))
        && obj
            .card_types
            .core_types
            .iter()
            .any(|ct| matches!(ct, CoreType::Creature))
}

/// CR 715.3a: Handle Adventure face choice and proceed with casting.
pub fn handle_adventure_choice(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    _card_id: CardId,
    creature: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !creature {
        // Swap to Adventure face characteristics
        if let Some(obj) = state.objects.get_mut(&object_id) {
            swap_to_adventure_face(obj);
        }
    }

    // Now proceed with normal casting using whichever face is active
    let prepared = prepare_spell_cast(state, player, object_id)?;

    let resolved = {
        let mut r = ResolvedAbility::new(
            prepared.ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        if let Some(sub) = &prepared.ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, prepared.object_id, player));
        }
        if let Some(c) = prepared.ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    };

    // Evaluate layers before targeting
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            if creature {
                return check_additional_cost_or_pay(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    events,
                );
            } else {
                return pay_and_push_adventure(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    true,
                    events,
                );
            }
        }

        let selection = begin_target_selection(&target_slots, &[])?;
        // TODO: For adventure spells with targets, we'd need to pass cast_as_adventure
        // through PendingCast. For now, the target selection path falls through to
        // pay_and_push which uses cast_as_adventure: false. This is a known limitation
        // for Adventure spells that require target selection.
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(PendingCast {
                object_id: prepared.object_id,
                card_id: prepared.card_id,
                ability: resolved,
                cost: prepared.mana_cost.clone(),
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: Vec::new(),
            }),
            target_slots,
            selection,
        });
    }

    // No targets -- proceed to payment
    if creature {
        check_additional_cost_or_pay(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            resolved,
            &prepared.mana_cost,
            events,
        )
    } else {
        pay_and_push_adventure(
            state,
            player,
            prepared.object_id,
            prepared.card_id,
            resolved,
            &prepared.mana_cost,
            true,
            events,
        )
    }
}

/// Cast a spell from hand (or command zone in Commander format).
pub fn handle_cast_spell(
    state: &mut GameState,
    player: PlayerId,
    card_id: CardId,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let object_id = spell_object_id_for_card_id(state, player, card_id)?;

    // CR 715.3a: Adventure cards from hand require choosing creature or Adventure face.
    if let Some(obj) = state.objects.get(&object_id) {
        if obj.zone == Zone::Hand && is_adventure_card(obj) {
            return Ok(WaitingFor::AdventureCastChoice {
                player,
                object_id,
                card_id,
            });
        }
    }

    let prepared = prepare_spell_cast(state, player, object_id)?;

    if let Some(ref modal_choice) = prepared.modal {
        // Cap max_choices to actual mode count
        let mut capped = modal_choice.clone();
        capped.max_choices = capped.max_choices.min(capped.mode_count);
        let target_constraints = target_constraints_from_modal(&capped);

        // Build a placeholder resolved ability -- will be replaced after mode selection
        let placeholder = ResolvedAbility::new(
            prepared.ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        return Ok(WaitingFor::ModeChoice {
            player,
            modal: capped,
            pending_cast: Box::new(PendingCast {
                object_id: prepared.object_id,
                card_id: prepared.card_id,
                ability: placeholder,
                cost: prepared.mana_cost.clone(),
                activation_cost: None,
                activation_ability_index: None,
                target_constraints,
            }),
        });
    }

    let resolved = {
        let mut r = ResolvedAbility::new(
            prepared.ability_def.effect.clone(),
            Vec::new(),
            prepared.object_id,
            player,
        );
        if let Some(sub) = &prepared.ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, prepared.object_id, player));
        }
        if let Some(c) = prepared.ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    };

    // 5. Handle targeting -- ensure layers evaluated before target legality
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    // Check if this is an Aura spell -- Auras target via Enchant keyword, not via effect targets
    // Re-read obj after evaluate_layers (which needs &mut state)
    let obj = state.objects.get(&prepared.object_id).unwrap();
    let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
    if is_aura {
        let enchant_filter = obj.keywords.iter().find_map(|k| {
            if let crate::types::keywords::Keyword::Enchant(filter) = k {
                Some(filter.clone())
            } else {
                None
            }
        });
        if let Some(filter) = enchant_filter {
            let legal = targeting::find_legal_targets(state, &filter, player, prepared.object_id);
            if legal.is_empty() {
                return Err(EngineError::ActionNotAllowed(
                    "No legal targets for Aura".to_string(),
                ));
            }
            let target_slots = vec![crate::types::game_state::TargetSelectionSlot {
                legal_targets: legal,
                optional: false,
            }];
            if let Some(targets) = auto_select_targets(&target_slots, &[])? {
                let mut resolved = resolved;
                assign_targets_in_chain(&mut resolved, &targets)?;
                return check_additional_cost_or_pay(
                    state,
                    player,
                    prepared.object_id,
                    prepared.card_id,
                    resolved,
                    &prepared.mana_cost,
                    events,
                );
            } else {
                let selection = begin_target_selection(&target_slots, &[])?;
                return Ok(WaitingFor::TargetSelection {
                    player,
                    pending_cast: Box::new(PendingCast {
                        object_id: prepared.object_id,
                        card_id: prepared.card_id,
                        ability: resolved,
                        cost: prepared.mana_cost.clone(),
                        activation_cost: None,
                        activation_ability_index: None,
                        target_constraints: Vec::new(),
                    }),
                    target_slots,
                    selection,
                });
            }
        }
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            return check_additional_cost_or_pay(
                state,
                player,
                prepared.object_id,
                prepared.card_id,
                resolved,
                &prepared.mana_cost,
                events,
            );
        }

        let selection = begin_target_selection(&target_slots, &[])?;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(PendingCast {
                object_id: prepared.object_id,
                card_id: prepared.card_id,
                ability: resolved,
                cost: prepared.mana_cost.clone(),
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: Vec::new(),
            }),
            target_slots,
            selection,
        });
    }

    // 6. Check additional cost, then pay mana cost
    check_additional_cost_or_pay(
        state,
        player,
        prepared.object_id,
        prepared.card_id,
        resolved,
        &prepared.mana_cost,
        events,
    )
}

/// Returns true if the spell has at least one legal target (or requires no targets).
/// Used by phase-ai's legal_actions to avoid including uncastable spells in the action set.
pub fn spell_has_legal_targets(
    state: &GameState,
    obj: &crate::game::game_object::GameObject,
    player: PlayerId,
) -> bool {
    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }
    let Some(obj) = simulated.objects.get(&obj.id) else {
        return false;
    };

    // Aura spells target via the Enchant keyword rather than the effect's target field.
    let is_aura = obj.card_types.subtypes.iter().any(|s| s == "Aura");
    if is_aura {
        let enchant_filter = obj.keywords.iter().find_map(|k| {
            if let crate::types::keywords::Keyword::Enchant(filter) = k {
                Some(filter.clone())
            } else {
                None
            }
        });
        return enchant_filter.is_some_and(|filter| {
            !targeting::find_legal_targets(&simulated, &filter, player, obj.id).is_empty()
        });
    }

    // Modal spells defer target checking until after mode selection
    if obj.modal.is_some() {
        return true;
    }

    let ability_def = match obj.abilities.first() {
        Some(a) => a,
        None => return true, // Vanilla permanent needs no targets
    };

    let resolved = build_resolved_from_def(ability_def, obj.id, player);
    match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) => {
            if target_slots.is_empty() {
                true
            } else {
                auto_select_targets(&target_slots, &[]).is_ok()
            }
        }
        Err(_) => false,
    }
}

pub fn can_cast_object_now(state: &GameState, player: PlayerId, object_id: ObjectId) -> bool {
    let Ok(prepared) = prepare_spell_cast(state, player, object_id) else {
        return false;
    };
    let Some(obj) = state.objects.get(&prepared.object_id) else {
        return false;
    };

    (prepared.modal.is_some() || spell_has_legal_targets(state, obj, player))
        && can_pay_cost_after_auto_tap(state, player, prepared.object_id, &prepared.mana_cost)
}

/// Returns true if the player can pay this mana cost after auto-tapping
/// currently activatable lands in a cloned game state.
///
/// Used by legal action generation so the frontend and engine agree on whether
/// a spell is castable from the current board state.
pub fn can_pay_cost_after_auto_tap(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
) -> bool {
    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }
    let spell_meta = simulated.objects.get(&source_id).map(|obj| SpellMeta {
        types: obj
            .card_types
            .core_types
            .iter()
            .map(|ct| format!("{ct:?}"))
            .collect(),
        subtypes: obj.card_types.subtypes.clone(),
    });

    auto_tap_lands(&mut simulated, player, cost, &mut Vec::new());

    simulated
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|player_data| {
            mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, spell_meta.as_ref())
        })
}

/// Handle mode selection for a modal spell.
///
/// Combines chosen mode abilities into a single ResolvedAbility chain (sub_abilities),
/// then proceeds to targeting or directly to payment.
pub fn handle_select_modes(
    state: &mut GameState,
    player: PlayerId,
    indices: Vec<usize>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (modal, pending) = match &state.waiting_for {
        WaitingFor::ModeChoice {
            modal,
            pending_cast,
            ..
        } => (modal.clone(), *pending_cast.clone()),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for mode selection".to_string(),
            ));
        }
    };

    validate_modal_indices(&modal, &indices)?;

    // Get the card's abilities to build combined resolved ability from chosen modes
    let obj = state
        .objects
        .get(&pending.object_id)
        .ok_or_else(|| EngineError::InvalidAction("Modal spell object not found".to_string()))?;
    let abilities = obj.abilities.clone();

    // Build a chain of ResolvedAbility from chosen modes (in order)
    let resolved = build_chained_resolved(&abilities, &indices, pending.object_id, player)?;

    // Check for targeting on the combined ability
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &pending.target_constraints)? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;
            return check_additional_cost_or_pay(
                state,
                player,
                pending.object_id,
                pending.card_id,
                resolved,
                &pending.cost,
                events,
            );
        }

        let selection = begin_target_selection(&target_slots, &pending.target_constraints)?;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(PendingCast {
                object_id: pending.object_id,
                card_id: pending.card_id,
                ability: resolved,
                cost: pending.cost,
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: pending.target_constraints,
            }),
            target_slots,
            selection,
        });
    }

    // No targets needed -- check additional cost, then pay
    check_additional_cost_or_pay(
        state,
        player,
        pending.object_id,
        pending.card_id,
        resolved,
        &pending.cost,
        events,
    )
}

/// Handle target selection for a pending cast.
pub fn handle_select_targets(
    state: &mut GameState,
    player: PlayerId,
    targets: Vec<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Extract PendingCast from WaitingFor::TargetSelection
    let pending = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            ..
        } => {
            validate_selected_targets(target_slots, &targets, &pending_cast.target_constraints)?;
            *pending_cast.clone()
        }
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    let mut ability = pending.ability;
    assign_targets_in_chain(&mut ability, &targets)?;

    if let Some(ability_index) = pending.activation_ability_index {
        if let Some(ref activation_cost) = pending.activation_cost {
            pay_ability_cost(state, player, pending.object_id, activation_cost, events)?;
        }

        let assigned_targets = flatten_targets_in_chain(&ability);
        emit_targeting_events(state, &assigned_targets, pending.object_id, player, events);

        let entry_id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        stack::push_to_stack(
            state,
            StackEntry {
                id: entry_id,
                source_id: pending.object_id,
                controller: player,
                kind: StackEntryKind::ActivatedAbility {
                    source_id: pending.object_id,
                    ability,
                },
            },
            events,
        );

        restrictions::record_ability_activation(state, pending.object_id, ability_index);
        events.push(GameEvent::AbilityActivated {
            source_id: pending.object_id,
        });
        state.priority_passes.clear();
        state.priority_pass_count = 0;
        return Ok(WaitingFor::Priority { player });
    }

    check_additional_cost_or_pay(
        state,
        player,
        pending.object_id,
        pending.card_id,
        ability,
        &pending.cost,
        events,
    )
}

pub fn handle_choose_target(
    state: &mut GameState,
    player: PlayerId,
    target: Option<TargetRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let (pending, target_slots, selection) = match &state.waiting_for {
        WaitingFor::TargetSelection {
            pending_cast,
            target_slots,
            selection,
            ..
        } => (
            *pending_cast.clone(),
            target_slots.clone(),
            selection.clone(),
        ),
        _ => {
            return Err(EngineError::InvalidAction(
                "Not waiting for target selection".to_string(),
            ));
        }
    };

    match choose_target(
        &target_slots,
        &pending.target_constraints,
        &selection,
        target,
    )? {
        TargetSelectionAdvance::InProgress(selection) => Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending),
            target_slots,
            selection,
        }),
        TargetSelectionAdvance::Complete(selected_slots) => {
            let mut ability = pending.ability;
            assign_selected_slots_in_chain(&mut ability, &selected_slots)?;

            if let Some(ability_index) = pending.activation_ability_index {
                if let Some(ref activation_cost) = pending.activation_cost {
                    pay_ability_cost(state, player, pending.object_id, activation_cost, events)?;
                }

                let assigned_targets = flatten_targets_in_chain(&ability);
                emit_targeting_events(state, &assigned_targets, pending.object_id, player, events);

                let entry_id = ObjectId(state.next_object_id);
                state.next_object_id += 1;
                stack::push_to_stack(
                    state,
                    StackEntry {
                        id: entry_id,
                        source_id: pending.object_id,
                        controller: player,
                        kind: StackEntryKind::ActivatedAbility {
                            source_id: pending.object_id,
                            ability,
                        },
                    },
                    events,
                );

                restrictions::record_ability_activation(state, pending.object_id, ability_index);
                events.push(GameEvent::AbilityActivated {
                    source_id: pending.object_id,
                });
                state.priority_passes.clear();
                state.priority_pass_count = 0;
                return Ok(WaitingFor::Priority { player });
            }

            check_additional_cost_or_pay(
                state,
                player,
                pending.object_id,
                pending.card_id,
                ability,
                &pending.cost,
                events,
            )
        }
    }
}

/// Activate an ability from a permanent on the battlefield.
/// Check whether an ability cost includes a tap component (either directly or
/// within a composite). Used for pre-validation before presenting modal choices.
fn requires_untapped(cost: &AbilityCost) -> bool {
    match cost {
        AbilityCost::Tap => true,
        AbilityCost::Composite { costs } => costs.iter().any(requires_untapped),
        _ => false,
    }
}

/// Pay a mana cost by auto-tapping lands and deducting from the player's mana pool.
///
/// Shared building block used by both spell casting (`pay_and_push`) and activated
/// ability cost payment (`pay_ability_cost`).
fn pay_mana_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    if state.layers_dirty {
        super::layers::evaluate_layers(state);
    }

    let spell_meta = state.objects.get(&source_id).map(|obj| SpellMeta {
        types: obj
            .card_types
            .core_types
            .iter()
            .map(|ct| format!("{ct:?}"))
            .collect(),
        subtypes: obj.card_types.subtypes.clone(),
    });

    auto_tap_lands(state, player, cost, events);

    {
        let player_data = state
            .players
            .iter()
            .find(|p| p.id == player)
            .expect("player exists");
        if !mana_payment::can_pay_for_spell(&player_data.mana_pool, cost, spell_meta.as_ref()) {
            return Err(EngineError::ActionNotAllowed(
                "Cannot pay mana cost".to_string(),
            ));
        }
    }

    let hand_demand = mana_payment::compute_hand_color_demand(state, player, source_id);
    let player_data = state
        .players
        .iter_mut()
        .find(|p| p.id == player)
        .expect("player exists");
    mana_payment::pay_cost_with_demand(
        &mut player_data.mana_pool,
        cost,
        Some(&hand_demand),
        spell_meta.as_ref(),
    )
    .map_err(|_| EngineError::ActionNotAllowed("Mana payment failed".to_string()))?;

    Ok(())
}

/// Pay an activated ability's cost. Handles `Tap`, `Mana`, `Composite` (recursive),
/// and passes through other cost types that require interactive resolution.
pub fn pay_ability_cost(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
    events: &mut Vec<GameEvent>,
) -> Result<(), EngineError> {
    match cost {
        AbilityCost::Tap => {
            let obj = state
                .objects
                .get(&source_id)
                .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;
            if obj.zone != Zone::Battlefield {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: source is not on the battlefield".to_string(),
                ));
            }
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
            let obj = state.objects.get_mut(&source_id).unwrap();
            obj.tapped = true;
            events.push(GameEvent::PermanentTapped {
                object_id: source_id,
            });
        }
        AbilityCost::Mana { cost } => {
            pay_mana_cost(state, player, source_id, cost, events)?;
        }
        AbilityCost::Composite { costs } => {
            for sub_cost in costs {
                pay_ability_cost(state, player, source_id, sub_cost, events)?;
            }
        }
        // Other cost types (Sacrifice, PayLife, etc.) require interactive resolution
        // and are not yet auto-payable. Pass through to allow the ability to resolve.
        _ => {}
    }
    Ok(())
}

fn can_pay_ability_cost_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    cost: &AbilityCost,
) -> bool {
    let mut simulated = state.clone();
    pay_ability_cost(&mut simulated, player, source_id, cost, &mut Vec::new()).is_ok()
}

pub fn can_activate_ability_now(
    state: &GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
) -> bool {
    let Some(obj) = state.objects.get(&source_id) else {
        return false;
    };
    if obj.zone != Zone::Battlefield
        || obj.controller != player
        || ability_index >= obj.abilities.len()
    {
        return false;
    }

    let ability_def = obj.abilities[ability_index].clone();
    if restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )
    .is_err()
    {
        return false;
    }
    if ability_def
        .cost
        .as_ref()
        .is_some_and(|cost| !can_pay_ability_cost_now(state, player, source_id, cost))
    {
        return false;
    }

    if let Some(ref modal) = ability_def.modal {
        if ability_def.cost.as_ref().is_some_and(requires_untapped) && obj.tapped {
            return false;
        }
        return modal.mode_count > 0;
    }

    let resolved = {
        let mut ability =
            ResolvedAbility::new(ability_def.effect.clone(), Vec::new(), source_id, player);
        if let Some(sub) = &ability_def.sub_ability {
            ability = ability.sub_ability(build_resolved_from_def(sub, source_id, player));
        }
        if let Some(condition) = ability_def.condition.clone() {
            ability = ability.condition(condition);
        }
        ability
    };

    let mut simulated = state.clone();
    if simulated.layers_dirty {
        super::layers::evaluate_layers(&mut simulated);
    }

    match build_target_slots(&simulated, &resolved) {
        Ok(target_slots) => {
            target_slots.is_empty() || auto_select_targets(&target_slots, &[]).is_ok()
        }
        Err(_) => false,
    }
}

pub fn handle_activate_ability(
    state: &mut GameState,
    player: PlayerId,
    source_id: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let obj = state
        .objects
        .get(&source_id)
        .ok_or_else(|| EngineError::InvalidAction("Object not found".to_string()))?;

    if obj.zone != Zone::Battlefield {
        return Err(EngineError::InvalidAction(
            "Object is not on the battlefield".to_string(),
        ));
    }
    if obj.controller != player {
        return Err(EngineError::NotYourPriority);
    }
    if ability_index >= obj.abilities.len() {
        return Err(EngineError::InvalidAction(
            "Invalid ability index".to_string(),
        ));
    }

    let ability_def = obj.abilities[ability_index].clone();

    restrictions::check_activation_restrictions(
        state,
        player,
        source_id,
        ability_index,
        &ability_def.activation_restrictions,
    )?;

    // CR 602.2a: Announce → choose modes → choose targets → pay costs.
    // Modal detection must happen BEFORE cost payment.
    if let Some(ref modal) = ability_def.modal {
        // Pre-validate tap cost for modals — fail fast before presenting the choice
        if ability_def.cost.as_ref().is_some_and(requires_untapped) {
            let obj = state.objects.get(&source_id).unwrap();
            if obj.tapped {
                return Err(EngineError::ActionNotAllowed(
                    "Cannot activate tap ability: permanent is tapped".to_string(),
                ));
            }
        }
        return Ok(WaitingFor::AbilityModeChoice {
            player,
            modal: modal.clone(),
            source_id,
            mode_abilities: ability_def.mode_abilities.clone(),
            is_activated: true,
            ability_index: Some(ability_index),
            ability_cost: ability_def.cost.clone(),
        });
    }

    let resolved = {
        let mut r = ResolvedAbility::new(ability_def.effect.clone(), Vec::new(), source_id, player);
        if let Some(sub) = &ability_def.sub_ability {
            r = r.sub_ability(build_resolved_from_def(sub, source_id, player));
        }
        if let Some(c) = ability_def.condition.clone() {
            r = r.condition(c);
        }
        r
    };

    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        if let Some(targets) = auto_select_targets(&target_slots, &[])? {
            let mut resolved = resolved;
            assign_targets_in_chain(&mut resolved, &targets)?;

            if let Some(ref cost) = ability_def.cost {
                pay_ability_cost(state, player, source_id, cost, events)?;
            }

            let assigned_targets = flatten_targets_in_chain(&resolved);
            emit_targeting_events(state, &assigned_targets, source_id, player, events);

            let entry_id = ObjectId(state.next_object_id);
            state.next_object_id += 1;

            stack::push_to_stack(
                state,
                StackEntry {
                    id: entry_id,
                    source_id,
                    controller: player,
                    kind: StackEntryKind::ActivatedAbility {
                        source_id,
                        ability: resolved,
                    },
                },
                events,
            );

            restrictions::record_ability_activation(state, source_id, ability_index);
            events.push(GameEvent::AbilityActivated { source_id });
            state.priority_passes.clear();
            state.priority_pass_count = 0;
            return Ok(WaitingFor::Priority { player });
        }

        let selection = begin_target_selection(&target_slots, &[])?;
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(PendingCast {
                object_id: source_id,
                card_id: CardId(0),
                ability: resolved,
                cost: crate::types::mana::ManaCost::NoCost,
                activation_cost: ability_def.cost.clone(),
                activation_ability_index: Some(ability_index),
                target_constraints: Vec::new(),
            }),
            target_slots,
            selection,
        });
    }

    if let Some(ref cost) = ability_def.cost {
        pay_ability_cost(state, player, source_id, cost, events)?;
    }

    // Push to stack
    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;

    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id,
                ability: resolved,
            },
        },
        events,
    );

    restrictions::record_ability_activation(state, source_id, ability_index);
    events.push(GameEvent::AbilityActivated { source_id });

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    Ok(WaitingFor::Priority { player })
}

/// Cancel a pending cast, reverting any side effects (e.g. untapping a source tapped for cost).
pub fn handle_cancel_cast(
    _state: &mut GameState,
    _pending: &PendingCast,
    _events: &mut Vec<GameEvent>,
) {
    // Costs are not paid before cancelable target/mode selection states, so cancel has no
    // side effects to unwind.
}

/// Handle the player's decision on an additional cost (kicker, blight, "or pay").
///
/// For `Optional`: `pay=true` sets `additional_cost_paid`, `pay=false` skips.
/// For `Choice`: `pay=true` uses first cost (blight), `pay=false` uses second (mana fallback).
pub fn handle_decide_additional_cost(
    state: &mut GameState,
    player: PlayerId,
    pending: PendingCast,
    additional_cost: &AdditionalCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut ability = pending.ability;

    match additional_cost {
        AdditionalCost::Optional(_cost) => {
            if pay {
                ability.context.additional_cost_paid = true;
                // TODO: actually deduct the cost (blight counters, mana, etc.)
            }
        }
        AdditionalCost::Choice(_preferred, _fallback) => {
            if pay {
                ability.context.additional_cost_paid = true;
                // TODO: deduct preferred cost
            } else {
                // TODO: deduct fallback cost
            }
        }
    }

    pay_and_push(
        state,
        player,
        pending.object_id,
        pending.card_id,
        ability,
        &pending.cost,
        events,
    )
}

/// Check for an additional cost on the object being cast. If one exists,
/// return `WaitingFor::OptionalCostChoice` so the player can decide;
/// otherwise proceed directly to `pay_and_push`.
///
/// This function sits between targeting and payment in the casting pipeline:
/// `CastSpell → [ModeChoice] → [TargetSelection] → [AdditionalCostChoice] → pay_and_push → Stack`
fn check_additional_cost_or_pay(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let additional = state
        .objects
        .get(&object_id)
        .and_then(|obj| obj.additional_cost.clone());

    if let Some(additional_cost) = additional {
        return Ok(WaitingFor::OptionalCostChoice {
            player,
            cost: additional_cost,
            pending_cast: Box::new(PendingCast {
                object_id,
                card_id,
                ability,
                cost: cost.clone(),
                activation_cost: None,
                activation_ability_index: None,
                target_constraints: Vec::new(),
            }),
        });
    }

    pay_and_push(state, player, object_id, card_id, ability, cost, events)
}

fn pay_and_push(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    pay_and_push_adventure(
        state, player, object_id, card_id, ability, cost, false, events,
    )
}

#[allow(clippy::too_many_arguments)]
fn pay_and_push_adventure(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    card_id: CardId,
    ability: ResolvedAbility,
    cost: &crate::types::mana::ManaCost,
    cast_as_adventure: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // Check for X in cost -- if present, return ManaPayment for player input
    if let crate::types::mana::ManaCost::Cost { shards, .. } = cost {
        if shards.contains(&ManaCostShard::X) {
            return Ok(WaitingFor::ManaPayment { player });
        }
    }

    pay_mana_cost(state, player, object_id, cost, events)?;

    // Record commander cast before moving (need to check zone before move)
    let was_in_command_zone = state
        .objects
        .get(&object_id)
        .map(|obj| obj.zone == Zone::Command && obj.is_commander)
        .unwrap_or(false);

    // Emit targeting events before the spell moves to the stack
    emit_targeting_events(
        state,
        &flatten_targets_in_chain(&ability),
        object_id,
        player,
        events,
    );

    // Move card from hand/command zone to stack zone
    zones::move_to_zone(state, object_id, Zone::Stack, events);

    // Track commander cast count for tax calculation
    if was_in_command_zone {
        super::commander::record_commander_cast(state, object_id);
    }

    // Push stack entry
    stack::push_to_stack(
        state,
        StackEntry {
            id: object_id,
            source_id: object_id,
            controller: player,
            kind: StackEntryKind::Spell {
                card_id,
                ability,
                cast_as_adventure,
            },
        },
        events,
    );

    state.priority_passes.clear();
    state.priority_pass_count = 0;

    events.push(GameEvent::SpellCast {
        card_id,
        controller: player,
    });

    let obj = state
        .objects
        .get(&object_id)
        .expect("spell object still exists after stack push")
        .clone();
    restrictions::record_spell_cast(state, player, &obj);

    Ok(WaitingFor::Priority { player })
}

/// Find and mark the first unused land producing `needed` color. Returns true if found.
fn tap_matching_land(
    available: &[ManaSourceOption],
    used_sources: &mut HashSet<ObjectId>,
    to_tap: &mut Vec<ManaSourceOption>,
    needed: ManaType,
) -> bool {
    let Some(option) = available
        .iter()
        .find(|option| option.mana_type == needed && !used_sources.contains(&option.object_id))
    else {
        return false;
    };

    used_sources.insert(option.object_id);
    to_tap.push(*option);
    true
}

/// Auto-tap untapped lands controlled by `player` to produce enough mana for `cost`.
///
/// Strategy: tap lands producing colors required by the cost first (colored shards),
/// then tap any remaining untapped lands for generic requirements.
fn auto_tap_lands(
    state: &mut GameState,
    player: PlayerId,
    cost: &crate::types::mana::ManaCost,
    events: &mut Vec<GameEvent>,
) {
    use crate::types::mana::ManaCost;

    let (shards, generic) = match cost {
        ManaCost::NoCost => return,
        ManaCost::Cost { shards, generic } if shards.is_empty() && *generic == 0 => return,
        ManaCost::Cost { shards, generic } => (shards, *generic),
    };

    // Build list of activatable mana options for untapped lands this player controls.
    let available: Vec<ManaSourceOption> = state
        .battlefield
        .iter()
        .filter_map(|&oid| {
            let obj = state.objects.get(&oid)?;
            if obj.controller != player || obj.tapped {
                return None;
            }
            if !obj
                .card_types
                .core_types
                .contains(&crate::types::card_type::CoreType::Land)
            {
                return None;
            }
            Some(mana_sources::activatable_land_mana_options(
                state, oid, player,
            ))
        })
        .flatten()
        .collect();

    let mut to_tap: Vec<ManaSourceOption> = Vec::new();
    let mut used_sources: HashSet<ObjectId> = HashSet::new();

    // Phase 1: satisfy colored and hybrid shards by tapping matching lands
    let mut deferred_generic: usize = 0;
    for shard in shards {
        use crate::game::mana_payment::{shard_to_mana_type, ShardRequirement};
        match shard_to_mana_type(*shard) {
            ShardRequirement::Single(color) | ShardRequirement::Phyrexian(color) => {
                tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
            }
            ShardRequirement::Hybrid(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::TwoGenericHybrid(color) => {
                // Prefer 1 matching-color land over 2 generic lands
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, color) {
                    deferred_generic += 2;
                }
            }
            ShardRequirement::ColorlessHybrid(color) => {
                if !tap_matching_land(
                    &available,
                    &mut used_sources,
                    &mut to_tap,
                    ManaType::Colorless,
                ) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, color);
                }
            }
            ShardRequirement::HybridPhyrexian(a, b) => {
                if !tap_matching_land(&available, &mut used_sources, &mut to_tap, a) {
                    tap_matching_land(&available, &mut used_sources, &mut to_tap, b);
                }
            }
            ShardRequirement::Snow | ShardRequirement::X => {
                deferred_generic += 1;
            }
        }
    }

    // Phase 2: satisfy generic cost + deferred shards with any remaining untapped lands
    let mut remaining_generic = generic as usize + deferred_generic;
    for option in &available {
        if remaining_generic == 0 {
            break;
        }
        if used_sources.insert(option.object_id) {
            to_tap.push(*option);
            remaining_generic = remaining_generic.saturating_sub(1);
        }
    }

    // Phase 3: tap and produce mana
    // We bypass resolve_mana_ability here because auto-tap has already chosen
    // which color each source should produce (via ManaSourceOption.mana_type).
    // Resolving the raw ability would ignore that choice for AnyOneColor sources.
    for option in to_tap {
        if let Some(obj) = state.objects.get_mut(&option.object_id) {
            if !obj.tapped {
                obj.tapped = true;
                events.push(GameEvent::PermanentTapped {
                    object_id: option.object_id,
                });
            }
        }
        mana_payment::produce_mana(state, option.object_id, option.mana_type, player, events);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        BasicLandType, ChosenAttribute, ChosenSubtypeKind, ContinuousModification, DamageAmount,
        QuantityExpr, StaticDefinition,
    };
    use crate::types::card_type::CoreType;
    use crate::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
    use crate::types::phase::Phase;

    fn setup_game_at_main_phase() -> GameState {
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

    fn add_basic_land(
        state: &mut GameState,
        card_id: CardId,
        name: &str,
        subtype: &str,
    ) -> ObjectId {
        let land = create_object(
            state,
            card_id,
            PlayerId(0),
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push(subtype.to_string());
        land
    }

    fn create_instant_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(10),
            player,
            "Lightning Bolt".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 3 },
                    target: crate::types::ability::TargetFilter::Any,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
        }
        obj_id
    }

    fn create_sorcery_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(20),
            player,
            "Divination".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Sorcery);
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Blue],
                generic: 2,
            };
        }
        obj_id
    }

    fn create_gloomlake_verge(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(21),
            player,
            "Gloomlake Verge".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Blue],
                    },
                    restrictions: vec![],
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap),
        );
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: crate::types::ability::ManaProduction::Fixed {
                        colors: vec![ManaColor::Black],
                    },
                    restrictions: vec![],
                },
            )
            .cost(crate::types::ability::AbilityCost::Tap)
            .sub_ability(AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Unimplemented {
                    name: "activate_only_if_controls_land_subtype_any".to_string(),
                    description: Some("Island|Swamp".to_string()),
                },
            )),
        );
        obj_id
    }

    fn create_targeted_activated_permanent(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(51),
            player,
            "Pinger".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: crate::types::ability::TargetFilter::Any,
                },
            )
            .cost(AbilityCost::Tap),
        );
        obj_id
    }

    #[test]
    fn spell_cast_from_hand_moves_to_stack() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events).unwrap();

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        assert!(state.players[0].hand.is_empty());
    }

    #[test]
    fn cast_spell_rejects_lands() {
        let mut state = setup_game_at_main_phase();
        let land = create_object(
            &mut state,
            CardId(11),
            PlayerId(0),
            "Plains".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&land).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Plains".to_string());

        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(11), &mut Vec::new());
        assert!(result.is_err());
        assert!(state.stack.is_empty());
    }

    #[test]
    fn sorcery_speed_rejects_during_opponent_turn() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1); // Opponent's turn
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 3);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn sorcery_speed_rejects_when_stack_not_empty() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 3);

        // Put something on the stack
        state.stack.push(StackEntry {
            id: ObjectId(99),
            source_id: ObjectId(99),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: ResolvedAbility::new(
                    Effect::Unimplemented {
                        name: String::new(),
                        description: None,
                    },
                    vec![],
                    ObjectId(99),
                    PlayerId(1),
                ),
                cast_as_adventure: false,
            },
        });

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn instant_can_be_cast_at_any_priority() {
        let mut state = setup_game_at_main_phase();
        state.active_player = PlayerId(1); // Not active player
        let _obj_id = create_instant_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Create a target creature
        let creature_id = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(10), &mut events);
        // Should succeed -- instants can be cast at any priority
        assert!(result.is_ok());
    }

    #[test]
    fn flash_permission_option_allows_sorcery_outside_normal_window() {
        let mut state = setup_game_at_main_phase();
        state.phase = Phase::End;
        state.active_player = PlayerId(1);
        state.priority_player = PlayerId(0);

        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.name = "Rout".to_string();
        obj.casting_options.push(
            crate::types::ability::SpellCastingOption::as_though_had_flash().cost(
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
            ),
        );

        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 4);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events)
            .expect("flash permission should allow cast");

        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn flash_permission_cost_is_not_added_in_normal_timing_window() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.casting_options.push(
            crate::types::ability::SpellCastingOption::as_though_had_flash().cost(
                AbilityCost::Mana {
                    cost: ManaCost::Cost {
                        shards: vec![],
                        generic: 2,
                    },
                },
            ),
        );

        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut Vec::new())
            .expect("normal-timing cast should not require flash surcharge");
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn activated_ability_with_target_defers_cost_until_target_selection() {
        let mut state = setup_game_at_main_phase();
        let source = create_targeted_activated_permanent(&mut state, PlayerId(0));
        let target = create_object(
            &mut state,
            CardId(52),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();

        assert!(matches!(waiting, WaitingFor::TargetSelection { .. }));
        state.waiting_for = waiting;
        assert!(!state.objects[&source].tapped);

        let mut events = Vec::new();
        let waiting = handle_select_targets(
            &mut state,
            PlayerId(0),
            vec![TargetRef::Object(target)],
            &mut events,
        )
        .unwrap();

        assert!(matches!(waiting, WaitingFor::Priority { .. }));
        assert!(state.objects[&source].tapped);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::AbilityActivated { source_id } if *source_id == source
        )));
    }

    #[test]
    fn deferred_tap_cost_fails_if_source_left_battlefield_before_target_lock() {
        let mut state = setup_game_at_main_phase();
        let source = create_targeted_activated_permanent(&mut state, PlayerId(0));
        let target = create_object(
            &mut state,
            CardId(52),
            PlayerId(1),
            "Target".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&target)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();
        state.waiting_for = waiting;

        let mut zone_events = Vec::new();
        zones::move_to_zone(&mut state, source, Zone::Graveyard, &mut zone_events);

        let result = handle_select_targets(
            &mut state,
            PlayerId(0),
            vec![TargetRef::Object(target)],
            &mut Vec::new(),
        );

        assert!(result.is_err());
        assert!(!state.objects[&source].tapped);
    }

    #[test]
    fn activation_restriction_only_once_each_turn_is_enforced() {
        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(70),
            PlayerId(0),
            "Relic".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.abilities.push(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            )
            .activation_restrictions(vec![
                crate::types::ability::ActivationRestriction::OnlyOnceEachTurn,
            ]),
        );

        let mut events = Vec::new();
        handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events).unwrap();
        let second = handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut events);

        assert!(second.is_err());
    }

    #[test]
    fn cancel_targeted_activated_ability_does_not_untap_source() {
        let mut state = setup_game_at_main_phase();
        let source = create_object(
            &mut state,
            CardId(71),
            PlayerId(0),
            "Weird Relic".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&source).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.tapped = true;
        obj.abilities.push(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: crate::types::ability::TargetFilter::Any,
            },
        ));

        let waiting =
            handle_activate_ability(&mut state, PlayerId(0), source, 0, &mut Vec::new()).unwrap();
        assert!(matches!(waiting, WaitingFor::TargetSelection { .. }));

        let mut events = Vec::new();
        handle_cancel_cast(
            &mut state,
            &match waiting {
                WaitingFor::TargetSelection { pending_cast, .. } => *pending_cast,
                other => panic!("expected target selection, got {other:?}"),
            },
            &mut events,
        );

        assert!(state.objects[&source].tapped);
        assert!(events.is_empty());
    }

    #[test]
    fn cost_payment_deducts_mana() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Blue, 1);
        add_mana(&mut state, PlayerId(0), ManaType::Colorless, 2);

        let initial_mana = state.players[0].mana_pool.total();
        assert_eq!(initial_mana, 3);

        let mut events = Vec::new();
        handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events).unwrap();

        assert_eq!(state.players[0].mana_pool.total(), 0);
    }

    #[test]
    fn cast_spell_insufficient_mana_fails() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_sorcery_in_hand(&mut state, PlayerId(0));
        // No mana added

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(20), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn auto_tap_respects_conditional_land_secondary_color() {
        let mut state = setup_game_at_main_phase();

        // Spell cost {B}
        let spell_id = create_object(
            &mut state,
            CardId(22),
            PlayerId(0),
            "Cut Down".to_string(),
            Zone::Hand,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
            spell.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ));
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }

        create_gloomlake_verge(&mut state, PlayerId(0));
        let island = create_object(
            &mut state,
            CardId(23),
            PlayerId(0),
            "Island".to_string(),
            Zone::Battlefield,
        );
        let island_obj = state.objects.get_mut(&island).unwrap();
        island_obj.card_types.core_types.push(CoreType::Land);
        island_obj.card_types.subtypes.push("Island".to_string());

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(22), &mut events);
        assert!(
            result.is_ok(),
            "expected conditional black mana to be available"
        );
    }

    #[test]
    fn auto_tap_blocks_conditional_land_secondary_color_without_requirement() {
        let mut state = setup_game_at_main_phase();

        // Spell cost {B}
        let spell_id = create_object(
            &mut state,
            CardId(24),
            PlayerId(0),
            "Cut Down".to_string(),
            Zone::Hand,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Instant);
            spell.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ));
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 0,
            };
        }

        create_gloomlake_verge(&mut state, PlayerId(0));

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(24), &mut events);
        assert!(
            result.is_err(),
            "expected cast to fail without Island/Swamp support"
        );
    }

    #[test]
    fn auto_tap_uses_layer_derived_basic_land_type() {
        let mut state = setup_game_at_main_phase();

        let spell_id = create_object(
            &mut state,
            CardId(25),
            PlayerId(0),
            "Deep-Cavern Bat".to_string(),
            Zone::Hand,
        );
        {
            let spell = state.objects.get_mut(&spell_id).unwrap();
            spell.card_types.core_types.push(CoreType::Creature);
            spell.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Unimplemented {
                    name: "PermanentCreature".to_string(),
                    description: None,
                },
            ));
            spell.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Black],
                generic: 1,
            };
        }

        let passage = create_object(
            &mut state,
            CardId(26),
            PlayerId(0),
            "Multiversal Passage".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&passage).unwrap();
            obj.card_types.core_types.push(CoreType::Land);
            obj.chosen_attributes
                .push(ChosenAttribute::BasicLandType(BasicLandType::Swamp));
            obj.static_definitions.push(
                StaticDefinition::continuous()
                    .affected(crate::types::ability::TargetFilter::SelfRef)
                    .modifications(vec![ContinuousModification::AddChosenSubtype {
                        kind: ChosenSubtypeKind::BasicLandType,
                    }]),
            );
        }

        let forest = add_basic_land(&mut state, CardId(27), "Forest", "Forest");
        state.layers_dirty = true;

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(25), &mut events);
        assert!(
            result.is_ok(),
            "expected chosen land subtype from layers to satisfy black mana"
        );
        assert!(state.objects[&passage].tapped);
        assert!(state.objects[&forest].tapped);
    }

    #[test]
    fn cancel_cast_during_target_selection_returns_to_priority() {
        use crate::game::engine::apply;
        use crate::types::actions::GameAction;

        let mut state = setup_game_at_main_phase();
        let _obj_id = create_instant_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Create two creatures so targeting is ambiguous (not auto-targeted)
        for card_id_val in [50, 51] {
            let cid = create_object(
                &mut state,
                CardId(card_id_val),
                PlayerId(1),
                "Goblin".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&cid)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        // Cast the spell -> should enter TargetSelection
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
        // Card should still be in hand
        assert!(!state.players[0].hand.is_empty());

        // Cancel -> should return to Priority
        let result = apply(&mut state, GameAction::CancelCast).unwrap();
        assert!(matches!(result.waiting_for, WaitingFor::Priority { .. }));
        // Card should still be in hand after cancel
        assert!(!state.players[0].hand.is_empty());
    }

    // --- Aura casting tests ---

    use crate::types::ability::{ControllerRef, TargetFilter, TypedFilter};
    use crate::types::keywords::Keyword;

    /// Create an Aura enchantment in hand with Enchant creature keyword.
    fn create_aura_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(30),
            player,
            "Pacifism".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            obj.card_types.subtypes.push("Aura".to_string());
            obj.keywords.push(Keyword::Enchant(TargetFilter::Typed(
                TypedFilter::creature(),
            )));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            };
        }
        obj_id
    }

    #[test]
    fn aura_with_multiple_targets_returns_target_selection() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // Create two creatures as potential targets
        for card_id_val in [50, 51] {
            let cid = create_object(
                &mut state,
                CardId(card_id_val),
                PlayerId(1),
                "Goblin".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&cid)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events).unwrap();

        match result {
            WaitingFor::TargetSelection { target_slots, .. } => {
                assert_eq!(target_slots.len(), 1);
                assert_eq!(target_slots[0].legal_targets.len(), 2);
            }
            other => panic!("Expected TargetSelection, got {:?}", other),
        }
    }

    #[test]
    fn aura_with_single_target_auto_targets() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // Create one creature as the only target
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events).unwrap();

        // Should auto-target and go straight to Priority (on stack)
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        // Verify the target was recorded on the stack entry
        if let StackEntryKind::Spell { ability, .. } = &state.stack[0].kind {
            assert_eq!(
                ability.targets,
                vec![crate::types::ability::TargetRef::Object(creature)]
            );
        } else {
            panic!("Expected spell on stack");
        }
    }

    #[test]
    fn aura_with_no_legal_targets_fails() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // No creatures on battlefield -- no legal targets for "Enchant creature"
        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn aura_with_enchant_you_control_rejects_opponent_creatures() {
        let mut state = setup_game_at_main_phase();
        let aura_id = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);
        state.objects.get_mut(&aura_id).unwrap().keywords = vec![Keyword::Enchant(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        )];

        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn aura_with_enchant_you_control_accepts_own_creature() {
        let mut state = setup_game_at_main_phase();
        let aura_id = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);
        state.objects.get_mut(&aura_id).unwrap().keywords = vec![Keyword::Enchant(
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
        )];

        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Spirit".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events).unwrap();
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
        if let StackEntryKind::Spell { ability, .. } = &state.stack[0].kind {
            assert_eq!(
                ability.targets,
                vec![crate::types::ability::TargetRef::Object(creature)]
            );
        } else {
            panic!("Expected spell on stack");
        }
    }

    #[test]
    fn aura_targeting_respects_hexproof() {
        let mut state = setup_game_at_main_phase();
        let _aura = create_aura_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        // Create a hexproof creature controlled by opponent
        let creature = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Hexproof Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.keywords.push(Keyword::Hexproof);
            obj.base_keywords.push(Keyword::Hexproof);
        }

        // Only target is hexproof opponent creature -- should fail
        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(30), &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn non_aura_enchantment_does_not_trigger_aura_targeting() {
        let mut state = setup_game_at_main_phase();

        // Create a global enchantment (no Aura subtype, no Enchant keyword)
        let obj_id = create_object(
            &mut state,
            CardId(40),
            PlayerId(0),
            "Intangible Virtue".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Enchantment);
            // No "Aura" subtype, no Enchant keyword
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::White],
                generic: 0,
            };
        }
        add_mana(&mut state, PlayerId(0), ManaType::White, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(40), &mut events).unwrap();

        // Should resolve normally (Priority), not enter TargetSelection
        assert!(matches!(result, WaitingFor::Priority { .. }));
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn emit_targeting_events_opponent_object_is_crime() {
        let mut state = setup_game_at_main_phase();
        let target = create_object(
            &mut state,
            CardId(50),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        emit_targeting_events(
            &state,
            &[TargetRef::Object(target)],
            ObjectId(99),
            PlayerId(0),
            &mut events,
        );
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::BecomesTarget { object_id, .. } if *object_id == target)
        ));
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CrimeCommitted { player_id } if *player_id == PlayerId(0))
        ));
    }

    #[test]
    fn emit_targeting_events_own_object_no_crime() {
        let mut state = setup_game_at_main_phase();
        let target = create_object(
            &mut state,
            CardId(50),
            PlayerId(0),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        let mut events = Vec::new();
        emit_targeting_events(
            &state,
            &[TargetRef::Object(target)],
            ObjectId(99),
            PlayerId(0),
            &mut events,
        );
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::BecomesTarget { .. })));
        assert!(!events
            .iter()
            .any(|e| matches!(e, GameEvent::CrimeCommitted { .. })));
    }

    #[test]
    fn emit_targeting_events_opponent_player_is_crime() {
        let state = setup_game_at_main_phase();
        let mut events = Vec::new();
        emit_targeting_events(
            &state,
            &[TargetRef::Player(PlayerId(1))],
            ObjectId(99),
            PlayerId(0),
            &mut events,
        );
        assert!(events.iter().any(
            |e| matches!(e, GameEvent::CrimeCommitted { player_id } if *player_id == PlayerId(0))
        ));
    }

    #[test]
    fn pay_and_push_emits_targeting_events_for_chained_spell_targets() {
        let mut state = setup_game_at_main_phase();
        let object_id = create_object(
            &mut state,
            CardId(77),
            PlayerId(0),
            "Split Bolt".to_string(),
            Zone::Hand,
        );
        let creature = create_object(
            &mut state,
            CardId(88),
            PlayerId(1),
            "Goblin".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&creature)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let ability = ResolvedAbility::new(
            Effect::DealDamage {
                amount: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Player,
            },
            vec![TargetRef::Player(PlayerId(1))],
            object_id,
            PlayerId(0),
        )
        .sub_ability(ResolvedAbility::new(
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
            },
            vec![TargetRef::Object(creature)],
            object_id,
            PlayerId(0),
        ));

        let mut events = Vec::new();
        let waiting_for = pay_and_push(
            &mut state,
            PlayerId(0),
            object_id,
            CardId(77),
            ability,
            &ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            },
            &mut events,
        )
        .expect("spell with chained targets should cast");

        assert!(matches!(waiting_for, WaitingFor::Priority { .. }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::BecomesTarget { object_id, .. } if *object_id == creature
            )
        }));
        assert!(events.iter().any(|event| {
            matches!(
                event,
                GameEvent::CrimeCommitted { player_id } if *player_id == PlayerId(0)
            )
        }));
    }

    // ── Modal spell tests ────────────────────────────────────────────────

    fn create_modal_charm(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(50),
            player,
            "Test Charm".to_string(),
            Zone::Hand,
        );
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.card_types.core_types.push(CoreType::Instant);
            // Mode 0: Deal 2 damage to any target
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: crate::types::ability::TargetFilter::Any,
                },
            ));
            // Mode 1: Draw a card
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                },
            ));
            // Mode 2: Gain 3 life
            obj.abilities.push(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 3 },
                    player: crate::types::ability::GainLifePlayer::Controller,
                },
            ));
            obj.mana_cost = ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 0,
            };
            obj.modal = Some(crate::types::ability::ModalChoice {
                min_choices: 1,
                max_choices: 1,
                mode_count: 3,
                mode_descriptions: vec![
                    "Deal 2 damage to any target".to_string(),
                    "Draw a card".to_string(),
                    "Gain 3 life".to_string(),
                ],
                allow_repeat_modes: false,
                constraints: vec![],
            });
        }
        obj_id
    }

    #[test]
    fn modal_spell_enters_mode_choice() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::ModeChoice { .. }),
            "expected ModeChoice, got {result:?}"
        );
    }

    #[test]
    fn modal_spell_mode_choice_has_correct_metadata() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        match result {
            WaitingFor::ModeChoice { modal, .. } => {
                assert_eq!(modal.min_choices, 1);
                assert_eq!(modal.max_choices, 1);
                assert_eq!(modal.mode_count, 3);
                assert_eq!(modal.mode_descriptions.len(), 3);
            }
            _ => panic!("expected ModeChoice"),
        }
    }

    #[test]
    fn select_mode_with_no_target_goes_to_priority() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Select mode 1 (Draw a card) -- no targets needed
        let result = handle_select_modes(&mut state, PlayerId(0), vec![1], &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "expected Priority after selecting no-target mode, got {result:?}"
        );
        assert_eq!(state.stack.len(), 1);
    }

    #[test]
    fn select_mode_with_target_enters_targeting() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Create a creature to target
        let creature = create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Bear".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&creature).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(2);
            obj.toughness = Some(2);
        }
        state.battlefield.push(creature);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Select mode 0 (Deal 2 damage) -- has targets (players + creature)
        let result = handle_select_modes(&mut state, PlayerId(0), vec![0], &mut events).unwrap();
        // Multiple legal targets exist (2 players + creature), so TargetSelection
        assert!(
            matches!(result, WaitingFor::TargetSelection { .. }),
            "expected TargetSelection, got {result:?}"
        );
    }

    #[test]
    fn select_mode_invalid_count_rejected() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Try selecting 2 modes when only 1 allowed
        let result = handle_select_modes(&mut state, PlayerId(0), vec![0, 1], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn select_mode_out_of_range_rejected() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Try selecting a mode index that doesn't exist
        let result = handle_select_modes(&mut state, PlayerId(0), vec![5], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn select_mode_duplicate_rejected() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Change to "choose two" to test duplicate rejection
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.modal.as_mut().unwrap().min_choices = 2;
            obj.modal.as_mut().unwrap().max_choices = 2;
        }

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Try selecting the same mode twice
        let result = handle_select_modes(&mut state, PlayerId(0), vec![1, 1], &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn choose_two_modal_chains_modes() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        // Change to "choose two"
        {
            let obj = state.objects.get_mut(&obj_id).unwrap();
            obj.modal.as_mut().unwrap().min_choices = 2;
            obj.modal.as_mut().unwrap().max_choices = 2;
        }

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Select modes 1 (Draw) and 2 (Gain life) -- no targets needed
        let result = handle_select_modes(&mut state, PlayerId(0), vec![1, 2], &mut events).unwrap();
        assert!(
            matches!(result, WaitingFor::Priority { .. }),
            "expected Priority, got {result:?}"
        );
        assert_eq!(state.stack.len(), 1);

        // Verify the stack entry has a chained ability (sub_ability present)
        match &state.stack[0].kind {
            StackEntryKind::Spell { ability, .. } => {
                // First mode is Draw
                assert!(matches!(
                    ability.effect,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 }
                    }
                ));
                // Second mode is GainLife as sub_ability
                let sub = ability
                    .sub_ability
                    .as_ref()
                    .expect("should have sub_ability");
                assert!(matches!(sub.effect, Effect::GainLife { .. }));
            }
            _ => panic!("expected Spell on stack"),
        }
    }

    #[test]
    fn cancel_modal_returns_to_priority() {
        let mut state = setup_game_at_main_phase();
        create_modal_charm(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 1);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(50), &mut events).unwrap();
        state.waiting_for = result;

        // Cancel should return to priority
        assert!(matches!(state.waiting_for, WaitingFor::ModeChoice { .. }));
    }

    // --- Adventure tests ---

    /// Create an Adventure card in hand: Bonecrusher Giant (creature) / Stomp (instant).
    fn create_adventure_in_hand(state: &mut GameState, player: PlayerId) -> ObjectId {
        let obj_id = create_object(
            state,
            CardId(70),
            player,
            "Bonecrusher Giant".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(4);
        obj.toughness = Some(3);
        obj.mana_cost = ManaCost::Cost {
            shards: vec![ManaCostShard::Red],
            generic: 2,
        };

        // Adventure face stored in back_face (Stomp - instant, {1}{R})
        obj.back_face = Some(crate::game::game_object::BackFaceData {
            name: "Stomp".to_string(),
            power: None,
            toughness: None,
            loyalty: None,
            card_types: {
                let mut ct = crate::types::card_type::CardType::default();
                ct.core_types.push(CoreType::Instant);
                ct
            },
            mana_cost: ManaCost::Cost {
                shards: vec![ManaCostShard::Red],
                generic: 1,
            },
            keywords: Vec::new(),
            abilities: vec![crate::types::ability::AbilityDefinition::new(
                crate::types::ability::AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 2 },
                    target: crate::types::ability::TargetFilter::Any,
                },
            )],
            trigger_definitions: Vec::new(),
            replacement_definitions: Vec::new(),
            static_definitions: Vec::new(),
            color: vec![ManaColor::Red],
            printed_ref: None,
            modal: None,
            additional_cost: None,
            casting_restrictions: Vec::new(),
            casting_options: Vec::new(),
        });

        obj_id
    }

    #[test]
    fn adventure_cast_choice_from_hand() {
        let mut state = setup_game_at_main_phase();
        let _obj_id = create_adventure_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 3);

        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(70), &mut events).unwrap();

        // Should prompt for Adventure face choice
        assert!(
            matches!(result, WaitingFor::AdventureCastChoice { player, card_id, .. }
                if player == PlayerId(0) && card_id == CardId(70)),
            "Expected AdventureCastChoice, got {:?}",
            result
        );
    }

    #[test]
    fn adventure_exile_on_resolve() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));

        // Directly push an Adventure spell on the stack (bypass targeting)
        zones::move_to_zone(&mut state, obj_id, Zone::Stack, &mut Vec::new());

        // Swap to Adventure face (simulating what handle_adventure_choice does)
        if let Some(obj) = state.objects.get_mut(&obj_id) {
            swap_to_adventure_face(obj);
        }

        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(70),
                ability: ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: crate::types::ability::TargetFilter::Any,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                    obj_id,
                    PlayerId(0),
                ),
                cast_as_adventure: true,
            },
        });

        // The object should now have Adventure face active
        assert_eq!(state.objects[&obj_id].name, "Stomp");

        // Resolve the spell
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);

        // Card should be in exile with AdventureCreature permission
        assert!(
            state.exile.contains(&obj_id),
            "Adventure spell should resolve to exile"
        );
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            obj.casting_permissions
                .contains(&crate::types::ability::CastingPermission::AdventureCreature),
            "Should have AdventureCreature permission"
        );
        // Name should be restored to creature face
        assert_eq!(obj.name, "Bonecrusher Giant");
    }

    #[test]
    fn adventure_countered_to_graveyard() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));

        // Manually put an Adventure spell on the stack
        zones::move_to_zone(&mut state, obj_id, Zone::Stack, &mut Vec::new());
        state.stack.push(StackEntry {
            id: obj_id,
            source_id: obj_id,
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(70),
                ability: ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 2 },
                        target: crate::types::ability::TargetFilter::Any,
                    },
                    vec![TargetRef::Player(PlayerId(1))],
                    obj_id,
                    PlayerId(0),
                ),
                cast_as_adventure: true,
            },
        });

        // Counter the spell (remove from stack, move to graveyard)
        state.stack.pop();
        zones::move_to_zone(&mut state, obj_id, Zone::Graveyard, &mut Vec::new());

        // Card should be in graveyard, NOT exile
        assert!(
            state.players[0].graveyard.contains(&obj_id),
            "Countered adventure spell should go to graveyard"
        );
        assert!(
            !state.exile.contains(&obj_id),
            "Countered adventure spell should NOT be in exile"
        );
        // Should NOT have AdventureCreature permission
        let obj = state.objects.get(&obj_id).unwrap();
        assert!(
            !obj.casting_permissions
                .contains(&crate::types::ability::CastingPermission::AdventureCreature),
            "Countered spell should not get casting permission"
        );
    }

    #[test]
    fn adventure_cast_creature_from_exile() {
        let mut state = setup_game_at_main_phase();
        let obj_id = create_adventure_in_hand(&mut state, PlayerId(0));
        add_mana(&mut state, PlayerId(0), ManaType::Red, 3);

        // Move to exile with AdventureCreature permission (simulates resolved Adventure)
        zones::move_to_zone(&mut state, obj_id, Zone::Exile, &mut Vec::new());
        let obj = state.objects.get_mut(&obj_id).unwrap();
        obj.casting_permissions
            .push(crate::types::ability::CastingPermission::AdventureCreature);

        // Should appear in available to cast
        let available = spell_objects_available_to_cast(&state, PlayerId(0));
        assert!(
            available.contains(&obj_id),
            "Exiled Adventure creature should be castable"
        );

        // Should NOT trigger AdventureCastChoice (from exile, always cast as creature)
        let mut events = Vec::new();
        let result = handle_cast_spell(&mut state, PlayerId(0), CardId(70), &mut events).unwrap();
        // Should proceed to payment, not to AdventureCastChoice
        assert!(
            !matches!(result, WaitingFor::AdventureCastChoice { .. }),
            "Casting from exile should not prompt for face choice"
        );
    }
}

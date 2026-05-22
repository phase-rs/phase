use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCast, StackEntry, StackEntryKind, WaitingFor};
use crate::types::identifiers::{CardId, ObjectId};
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;

use super::ability_utils::{
    assign_targets_in_chain, auto_select_targets_for_ability, begin_target_selection_for_ability,
    build_target_slots, flatten_targets_in_chain, random_select_targets_for_ability,
};
use super::casting::emit_targeting_events;
use super::engine::EngineError;
use super::stack;

use crate::types::ability::{AbilityCost, ResolvedAbility};

/// CR 306.5d + CR 606.3: Loyalty abilities may only be activated once per turn.
/// CR 606.1: Loyalty abilities are activated abilities with a loyalty symbol in their cost.
pub fn can_activate_loyalty(
    state: &GameState,
    planeswalker_id: ObjectId,
    player: PlayerId,
) -> bool {
    let Some(obj) = state.objects.get(&planeswalker_id) else {
        return false;
    };
    obj.abilities
        .iter()
        .enumerate()
        .any(|(ability_index, ability)| {
            matches!(ability.cost, Some(AbilityCost::Loyalty { .. }))
                && can_activate_loyalty_ability(state, planeswalker_id, player, ability_index)
        })
}

/// CR 306.5d + CR 606.3: Loyalty-specific gate for a concrete loyalty ability.
///
/// Timing is intentionally delegated to the shared activation restriction system:
/// normal loyalty abilities carry `ActivationRestriction::AsSorcery`, while static
/// permissions such as The Wandering Emperor's same-turn effect may override that
/// restriction to instant timing.
pub fn can_activate_loyalty_ability(
    state: &GameState,
    planeswalker_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
) -> bool {
    let obj = match state.objects.get(&planeswalker_id) {
        Some(o) => o,
        None => return false,
    };

    // CR 306.5d: Must be a planeswalker on the battlefield controlled by player.
    if !obj.card_types.core_types.contains(&CoreType::Planeswalker) {
        return false;
    }
    if obj.zone != crate::types::zones::Zone::Battlefield {
        return false;
    }
    if obj.controller != player {
        return false;
    }

    // CR 606.3: Only if no player has previously activated a loyalty ability of
    // that permanent that turn. The per-permanent activation count is the
    // authoritative gate; effects like `Effect::GrantExtraLoyaltyActivations`
    // (The Chain Veil class) raise the per-planeswalker cap for the controller
    // by adding to `extra_loyalty_activations_this_turn[controller]`.
    let extra_grants = state
        .extra_loyalty_activations_this_turn
        .get(&player)
        .copied()
        .unwrap_or(0);
    let cap = 1u32.saturating_add(extra_grants);
    if obj.loyalty_activations_this_turn >= cap {
        return false;
    }

    let Some(ability) = obj.abilities.get(ability_index) else {
        return false;
    };
    if !matches!(ability.cost, Some(AbilityCost::Loyalty { .. })) {
        return false;
    }

    super::restrictions::check_activation_restrictions(
        state,
        player,
        planeswalker_id,
        ability_index,
        &ability.activation_restrictions,
    )
    .is_ok()
}

/// CR 606.2: Activate a planeswalker loyalty ability.
///
/// CR 606.4: Parses the loyalty cost from the ability definition (e.g. "+1", "-3", "0"),
/// adjusts loyalty counters, marks activated this turn, and pushes
/// the ability onto the stack (CR 602.2a).
pub fn handle_activate_loyalty(
    state: &mut GameState,
    player: PlayerId,
    pw_id: ObjectId,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !can_activate_loyalty_ability(state, pw_id, player, ability_index) {
        return Err(EngineError::ActionNotAllowed(
            "Cannot activate loyalty ability".to_string(),
        ));
    }

    let obj = state
        .objects
        .get(&pw_id)
        .ok_or_else(|| EngineError::InvalidAction("Planeswalker not found".to_string()))?;

    if ability_index >= obj.abilities.len() {
        return Err(EngineError::InvalidAction(
            "Invalid ability index".to_string(),
        ));
    }

    let ability_def = &obj.abilities[ability_index];
    let loyalty_cost = parse_loyalty_cost(ability_def);
    let current_loyalty = obj.loyalty.unwrap_or(0) as i32;

    // CR 606.6: A loyalty ability with a negative loyalty cost can't be activated unless the
    // permanent has at least that many loyalty counters on it.
    if loyalty_cost < 0 && current_loyalty + loyalty_cost < 0 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough loyalty to activate ability".to_string(),
        ));
    }

    // Build a ResolvedAbility for the stack from the typed definition
    let resolved = build_pw_resolved(ability_def, pw_id, player);

    // CR 602.2b + CR 601.2c: Targets are announced before costs are paid.
    // If this ability requires targets, prompt for selection first.
    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        // CR 115.1 + CR 701.9b: Random-target loyalty abilities — game picks via
        // `state.rng`. Routes through finalize_loyalty_activation just like the
        // controller-choice degenerate path.
        let resolved_targets = if matches!(
            resolved.target_selection_mode,
            crate::types::ability::TargetSelectionMode::Random
        ) {
            Some(random_select_targets_for_ability(
                state,
                &target_slots,
                &[],
            )?)
        } else {
            auto_select_targets_for_ability(state, &resolved, &target_slots, &[])?
        };

        if let Some(targets) = resolved_targets {
            let mut resolved = resolved;
            assign_targets_in_chain(state, &mut resolved, &targets)?;
            return Ok(finalize_loyalty_activation(
                state,
                player,
                pw_id,
                loyalty_cost,
                resolved,
                ability_index,
                events,
            ));
        }

        state.lands_tapped_for_mana.remove(&player);

        let selection = begin_target_selection_for_ability(state, &resolved, &target_slots, &[])?;
        let mut pending = PendingCast::new(pw_id, CardId(0), resolved, ManaCost::NoCost);
        pending.activation_ability_index = Some(ability_index);
        // CR 606.4: Loyalty cost is paid after targets are chosen.
        // Stored here so handle_select_targets can call pay_ability_cost.
        pending.activation_cost = Some(crate::types::ability::AbilityCost::Loyalty {
            amount: loyalty_cost,
        });
        return Ok(WaitingFor::TargetSelection {
            player,
            pending_cast: Box::new(pending),
            target_slots,
            selection,
        });
    }

    Ok(finalize_loyalty_activation(
        state,
        player,
        pw_id,
        loyalty_cost,
        resolved,
        ability_index,
        events,
    ))
}

/// CR 606.3 + CR 606.1: Record a loyalty-ability activation against both the
/// per-permanent counter (`loyalty_activations_this_turn` — gates re-activation
/// of this planeswalker) and the per-player counter
/// (`loyalty_abilities_activated_this_turn` — read by
/// `QuantityRef::LoyaltyAbilitiesActivatedThisTurn` for The Chain Veil's
/// intervening-if trigger).
///
/// CR 606.3 + CR 602.5b: The per-permanent counter persists across same-turn
/// control changes (it's a property of the permanent, not the activator).
/// CR 603.4: The per-player counter is keyed by the player who activated —
/// "you activated a loyalty ability" reads as the *activator's* history.
pub(super) fn record_loyalty_activation(state: &mut GameState, pw_id: ObjectId, player: PlayerId) {
    if let Some(obj) = state.objects.get_mut(&pw_id) {
        obj.loyalty_activations_this_turn = obj.loyalty_activations_this_turn.saturating_add(1);
    }
    *state
        .loyalty_abilities_activated_this_turn
        .entry(player)
        .and_modify(|count| *count = count.saturating_add(1))
        .or_insert(1);
}

/// Extract the loyalty cost from a typed ability definition.
///
/// Uses `AbilityCost::Loyalty` (set by the JSON loader). Falls back to 0
/// if not present.
fn parse_loyalty_cost(ability_def: &crate::types::ability::AbilityDefinition) -> i32 {
    if let Some(crate::types::ability::AbilityCost::Loyalty { amount }) = &ability_def.cost {
        return *amount;
    }
    0
}

/// Build a ResolvedAbility from a typed AbilityDefinition for the stack.
fn build_pw_resolved(
    ability_def: &crate::types::ability::AbilityDefinition,
    source_id: ObjectId,
    controller: PlayerId,
) -> ResolvedAbility {
    super::ability_utils::build_resolved_from_def(ability_def, source_id, controller)
}

/// CR 606.4: Pay the loyalty cost, push the ability onto the stack, and return Priority.
/// Single exit point for non-targeted (and auto-target-resolved) loyalty activations.
///
/// Loyalty counter adjustment is delegated to `casting::pay_ability_cost` — the single
/// authority for all ability cost resolution — to avoid duplicating counter logic here.
fn finalize_loyalty_activation(
    state: &mut GameState,
    player: PlayerId,
    pw_id: ObjectId,
    loyalty_cost: i32,
    resolved: ResolvedAbility,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> WaitingFor {
    // CR 606.4: Single authority for loyalty cost payment.
    let cost = crate::types::ability::AbilityCost::Loyalty {
        amount: loyalty_cost,
    };
    super::casting::pay_ability_cost(state, player, pw_id, &cost, events)
        .expect("loyalty validation passed in handle_activate_loyalty");
    record_loyalty_activation(state, pw_id, player);

    let assigned_targets = flatten_targets_in_chain(&resolved);
    emit_targeting_events(state, &assigned_targets, pw_id, player, events);

    let entry_id = ObjectId(state.next_object_id);
    state.next_object_id += 1;
    // CR 603.4: Stamp the loyalty-ability index for per-turn resolution tracking.
    let mut resolved_with_idx = resolved;
    resolved_with_idx.ability_index = Some(ability_index);
    stack::push_to_stack(
        state,
        StackEntry {
            id: entry_id,
            source_id: pw_id,
            controller: player,
            kind: StackEntryKind::ActivatedAbility {
                source_id: pw_id,
                ability: resolved_with_idx,
            },
        },
        events,
    );

    super::restrictions::record_ability_activation(state, pw_id, ability_index);
    // CR 117.1b: Priority permits unbounded activation. `pending_activations`
    // is a per-priority-window AI-guard — see `GameState::pending_activations`.
    state.pending_activations.push((pw_id, ability_index));
    events.push(GameEvent::AbilityActivated { source_id: pw_id });
    state.lands_tapped_for_mana.remove(&player);
    state.priority_passes.clear();
    state.priority_pass_count = 0;

    WaitingFor::Priority { player }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Effect, QuantityExpr,
        TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::game_state::CastingVariant;
    use crate::types::identifiers::CardId;
    use crate::types::phase::Phase;
    use crate::types::zones::Zone;

    fn setup() -> GameState {
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

    /// Create a loyalty ability with the given cost and effect.
    fn make_loyalty_ability(loyalty_amount: i32, effect: Effect) -> AbilityDefinition {
        let mut ability =
            AbilityDefinition::new(AbilityKind::Activated, effect).cost(AbilityCost::Loyalty {
                amount: loyalty_amount,
            });
        ability = ability.sorcery_speed();
        ability
            .activation_restrictions
            .push(ActivationRestriction::OnlyOnceEachTurn);
        ability
    }

    fn create_planeswalker(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        loyalty: u32,
        abilities: Vec<crate::types::ability::AbilityDefinition>,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        // CR 306.5b: A planeswalker's loyalty IS the count of loyalty counters
        // on it. Seed both so the field and counter map start in sync.
        obj.loyalty = Some(loyalty);
        obj.counters
            .insert(crate::types::counter::CounterType::Loyalty, loyalty);
        obj.abilities = Arc::new(abilities);
        obj.entered_battlefield_turn = Some(state.turn_number);
        id
    }

    #[test]
    fn activate_plus_loyalty_adds_counter() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);

        assert!(result.is_ok());
        assert_eq!(state.objects[&pw].loyalty, Some(4)); // 3 + 1
        assert!(state.objects[&pw].loyalty_activations_this_turn > 0);
        assert!(!state.stack.is_empty()); // ability on stack
    }

    #[test]
    fn activate_minus_loyalty_removes_counters() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Liliana",
            5,
            vec![make_loyalty_ability(
                -3,
                // Use non-targeted effect so no target selection is needed.
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 2 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);

        assert!(result.is_ok());
        assert_eq!(state.objects[&pw].loyalty, Some(2)); // 5 - 3
    }

    #[test]
    fn second_activation_same_turn_rejected() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        let mut events = Vec::new();
        // First activation succeeds
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
        // Clear stack so sorcery speed check passes
        state.stack.clear();

        // Second activation fails
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn loyalty_activation_resets_at_turn_start() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        // Activate loyalty
        let mut events = Vec::new();
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
        assert!(state.objects[&pw].loyalty_activations_this_turn > 0);

        // CR 606.3: the per-permanent loyalty limit resets at the start of every
        // turn for every planeswalker regardless of controller. The reset is
        // global, so it fires on the very first `start_next_turn` (which makes
        // PlayerId(1) the active player) — not two turns later.
        crate::game::turns::start_next_turn(&mut state, &mut events);
        assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 0);
    }

    #[test]
    fn loyalty_activation_requires_sorcery_speed() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        // Not main phase
        state.phase = Phase::DeclareAttackers;
        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());

        // Not active player
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());

        // Stack not empty
        state.active_player = PlayerId(0);
        state.stack.push_back(crate::types::game_state::StackEntry {
            id: ObjectId(99),
            source_id: ObjectId(99),
            controller: PlayerId(1),
            kind: StackEntryKind::Spell {
                card_id: CardId(99),
                ability: None,
                casting_variant: CastingVariant::Normal,
                actual_mana_spent: 0,
            },
        });
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn minus_ability_insufficient_loyalty_rejected() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Liliana",
            2,
            vec![make_loyalty_ability(
                -3,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            )],
        );

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err());
    }

    #[test]
    fn parse_loyalty_cost_prefers_typed_ability_cost() {
        use crate::types::ability::{AbilityCost, AbilityKind, Effect};
        // When AbilityCost::Loyalty is set, it should be used
        let ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Loyalty { amount: -3 });
        assert_eq!(parse_loyalty_cost(&ability), -3);
    }

    #[test]
    fn parse_loyalty_cost_defaults_to_zero_without_loyalty_cost() {
        use crate::types::ability::{AbilityKind, Effect};
        // When no AbilityCost::Loyalty, fall back to 0
        let ability = crate::types::ability::AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        assert_eq!(parse_loyalty_cost(&ability), 0);
    }

    #[test]
    fn parse_loyalty_cost_extracts_values() {
        assert_eq!(
            parse_loyalty_cost(&make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                }
            )),
            1
        );
        assert_eq!(
            parse_loyalty_cost(&make_loyalty_ability(
                -3,
                Effect::Destroy {
                    target: crate::types::ability::TargetFilter::Any,
                    cant_regenerate: false,
                }
            )),
            -3
        );
        assert_eq!(
            parse_loyalty_cost(&make_loyalty_ability(
                0,
                Effect::Mill {
                    count: crate::types::ability::QuantityExpr::Fixed { value: 3 },
                    target: crate::types::ability::TargetFilter::Any,
                    destination: crate::types::zones::Zone::Graveyard,
                }
            )),
            0
        );
        // No loyalty cost
        let no_cost = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        assert_eq!(parse_loyalty_cost(&no_cost), 0);
    }

    /// CR 602.2b + CR 601.2c + CR 606.4: Targeted loyalty abilities must prompt for target selection
    /// before paying the loyalty cost. The cost is deferred into the PendingCast so
    /// handle_select_targets can call pay_ability_cost after the player chooses.
    #[test]
    fn targeted_loyalty_ability_returns_target_selection() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Kaito",
            4,
            vec![make_loyalty_ability(
                -2,
                Effect::Tap {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            )],
        );

        // Two creatures so auto_select_targets doesn't collapse to Priority.
        for card_id in [99usize, 100] {
            let c = create_object(
                &mut state,
                CardId(card_id.try_into().unwrap()),
                PlayerId(1),
                "Goblin".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&c)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }

        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events)
            .expect("activation should succeed with a legal target");

        // Loyalty is NOT deducted yet — cost is paid after target selection.
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(4),
            "loyalty unchanged before target selection"
        );
        // But activation is marked to prevent re-activation this turn.
        assert!(state.objects[&pw].loyalty_activations_this_turn > 0);
        // Engine waits for the player to select a target.
        assert!(
            matches!(result, WaitingFor::TargetSelection { .. }),
            "expected TargetSelection, got {result:?}"
        );
        // The pending cast carries the loyalty cost for deferred payment.
        if let WaitingFor::TargetSelection { pending_cast, .. } = result {
            assert!(
                matches!(
                    pending_cast.activation_cost,
                    Some(crate::types::ability::AbilityCost::Loyalty { amount: -2 })
                ),
                "loyalty cost must be stored for deferred payment"
            );
        }
    }

    /// CR 606.3 regression for issue #500: a loyalty ability is activatable
    /// after a control change when the *prior owner* activated a loyalty ability
    /// of the same planeswalker on a *prior* turn. This mirrors the issue
    /// scenario exactly: the planeswalker is owned & controlled by P1 (who
    /// activates it on P1's turn); then on P0's turn P0 gains control. With the
    /// old controller-scoped reset, P0's turn start skips the still-P1-controlled
    /// planeswalker and the stale flag blocks P0's activation. The global reset
    /// clears it for every planeswalker regardless of controller.
    #[test]
    fn loyalty_activatable_after_control_change_following_owners_prior_turn_activation() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(1),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        // setup() is P0's turn — advance to P1's turn so the owner can activate.
        let mut events = Vec::new();
        crate::game::turns::start_next_turn(&mut state, &mut events);
        assert_eq!(state.active_player, PlayerId(1));
        state.phase = Phase::PreCombatMain;
        state.priority_player = PlayerId(1);

        // P1 (owner & controller) activates the loyalty ability on P1's turn.
        handle_activate_loyalty(&mut state, PlayerId(1), pw, 0, &mut events).unwrap();
        assert!(state.objects[&pw].loyalty_activations_this_turn > 0);
        state.stack.clear();

        // P1's turn ends, P0's turn begins. The planeswalker is STILL controlled
        // by P1 at this turn start (the gain-control effect has not resolved).
        crate::game::turns::start_next_turn(&mut state, &mut events);
        assert_eq!(state.active_player, PlayerId(0));

        // A gain-control effect resolves this turn, handing the planeswalker to
        // the current active player (P0).
        let active = state.active_player;
        state.objects.get_mut(&pw).unwrap().controller = active;
        state.phase = Phase::PreCombatMain;
        state.priority_player = active;
        state.stack.clear();

        assert!(
            can_activate_loyalty_ability(&state, pw, active, 0),
            "new controller may activate after the prior owner's prior-turn activation"
        );
        let result = handle_activate_loyalty(&mut state, active, pw, 0, &mut events);
        assert!(result.is_ok());
    }

    /// CR 602.5b / CR 606.3: the once-per-turn limit is a property of the
    /// permanent and persists across a control change *within the same turn*.
    #[test]
    fn loyalty_limit_persists_across_same_turn_control_change() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );

        let mut events = Vec::new();
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
        state.stack.clear();

        // Control changes to P1 in the same turn.
        state.objects.get_mut(&pw).unwrap().controller = PlayerId(1);

        assert!(
            !can_activate_loyalty_ability(&state, pw, PlayerId(1), 0),
            "the per-permanent limit survives a same-turn control change"
        );
    }

    /// The latent always-present bug: a planeswalker controlled by a
    /// non-active player must still have its loyalty flag reset at turn start.
    #[test]
    fn loyalty_limit_resets_globally_for_non_active_players_planeswalker() {
        let mut state = setup();
        // It is P0's turn; the planeswalker is controlled by P1.
        let pw = create_planeswalker(
            &mut state,
            PlayerId(1),
            "Jace",
            3,
            vec![make_loyalty_ability(
                1,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );
        // Simulate activation on P1's previous turn.
        state
            .objects
            .get_mut(&pw)
            .unwrap()
            .loyalty_activations_this_turn = 1;

        let mut events = Vec::new();
        crate::game::turns::start_next_turn(&mut state, &mut events);

        assert_eq!(
            state.objects[&pw].loyalty_activations_this_turn, 0,
            "the loyalty limit resets globally, not just for the active player's permanents"
        );
    }

    /// CR 606.3: activating any loyalty ability locks out *all* loyalty
    /// abilities of the same planeswalker (per-permanent, not per-ability).
    #[test]
    fn activating_one_loyalty_ability_locks_all_on_same_planeswalker() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Jace",
            5,
            vec![
                make_loyalty_ability(
                    1,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 1 },
                        target: TargetFilter::Controller,
                    },
                ),
                make_loyalty_ability(
                    -2,
                    Effect::Draw {
                        count: QuantityExpr::Fixed { value: 2 },
                        target: TargetFilter::Controller,
                    },
                ),
            ],
        );

        let mut events = Vec::new();
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events).unwrap();
        state.stack.clear();

        assert!(
            !can_activate_loyalty_ability(&state, pw, PlayerId(0), 1),
            "activating ability 0 must lock out ability 1 on the same planeswalker"
        );
    }
}

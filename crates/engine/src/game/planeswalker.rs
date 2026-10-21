use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PendingCast, PendingCostMoveResume, StackEntry, StackEntryKind, WaitingFor,
};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaCost;
use crate::types::player::PlayerId;

use super::ability_utils::{
    assign_selected_slots_in_chain, auto_select_targets_for_ability,
    begin_target_selection_for_ability, build_target_slots, declared_targets_in_chain,
    random_select_targets_for_ability,
};
use super::casting::emit_targeting_events;
use super::engine::EngineError;
use super::priority;
use super::stack;

use crate::types::ability::{AbilityProvenance, ResolvedAbility};

use super::game_object::GameObject;

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
            ability
                .cost
                .as_ref()
                .is_some_and(crate::types::ability::is_loyalty_ability_cost)
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
    can_activate_loyalty_ability_impl(state, planeswalker_id, player, ability_index, None)
}

pub(crate) fn can_activate_loyalty_ability_with_restriction_gates(
    state: &GameState,
    planeswalker_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
    restriction_gates: &super::restrictions::ActivationRestrictionStaticGates,
) -> bool {
    can_activate_loyalty_ability_impl(
        state,
        planeswalker_id,
        player,
        ability_index,
        Some(restriction_gates),
    )
}

/// CR 606.3 + CR 101.1: The cap on `obj`'s loyalty-ability activations this turn for
/// the ability at `ability_index`: CR 606.3's "once per permanent per turn" limit,
/// adjusted by every card that overrides it (CR 101.1). The single composition point
/// for them: the base 1, which a `ModifyActivationLimit { keyword: "loyalty" }`
/// static SETS (Oath of Teferi class, max semantics, read live per CR 611.3a), plus
/// the player-wide grant (`Effect::GrantExtraLoyaltyActivations { target:
/// Controller }`, The Chain Veil class), plus the permanent-scoped grant
/// (`target: SelfRef`, Comet, Stellar Pup class).
///
/// The result is only meaningful against `obj.loyalty_activations_this_turn`: a
/// static only RAISES the cap above the grant-only base `1 + grants` (max
/// semantics, floor 1), so while that count is still below the base the static scan
/// cannot change whether the cap is reached and is skipped, returning the base.
/// Besides saving the walk, this keeps priority loyalty candidate production at a
/// single exact static scan (the ActivateAsInstant permission consult): the
/// pre-flush `all_present` presence default would otherwise fall through to a
/// redundant exact scan here.
fn loyalty_activation_cap(
    state: &GameState,
    obj: &GameObject,
    player: PlayerId,
    ability_index: usize,
    restriction_gates: Option<&super::restrictions::ActivationRestrictionStaticGates>,
) -> u32 {
    let player_extra = state
        .extra_loyalty_activations_this_turn
        .get(&player)
        .copied()
        .unwrap_or(0);
    // The permanent-scoped grant covers only the permanent's own ability: the
    // card's ruling (not CR text) says it does not apply to a loyalty ability the
    // permanent gained. CR 613.1f applies ability-adding effects in layer 6,
    // after the characteristic abilities, which is the distinction
    // `activated_ability_provenance` records.
    let permanent_extra = match obj.activated_ability_provenance(ability_index) {
        AbilityProvenance::Characteristic(_) => obj.extra_loyalty_activations_this_turn,
        AbilityProvenance::Granted => 0,
    };
    let extra_grants = player_extra.saturating_add(permanent_extra);
    let base_cap = 1u32.saturating_add(extra_grants);
    if obj.loyalty_activations_this_turn < base_cap {
        return base_cap;
    }
    super::restrictions::loyalty_activation_cap_from_statics(
        state,
        player,
        obj.id,
        restriction_gates,
    )
    .saturating_add(extra_grants)
}

fn can_activate_loyalty_ability_impl(
    state: &GameState,
    planeswalker_id: ObjectId,
    player: PlayerId,
    ability_index: usize,
    restriction_gates: Option<&super::restrictions::ActivationRestrictionStaticGates>,
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
    // authoritative gate; `ModifyActivationLimit { keyword: "loyalty" }` statics
    // (Oath of Teferi class, read live per CR 611.3a) SET the base cap via max
    // semantics, while effects like `Effect::GrantExtraLoyaltyActivations` ADD to
    // it: for the controller's planeswalkers via
    // `extra_loyalty_activations_this_turn[controller]` (The Chain Veil class),
    // for one permanent via its own counter (Comet's source-scoped grant).
    // `loyalty_activation_cap` composes them.
    if obj.loyalty_activations_this_turn
        >= loyalty_activation_cap(state, obj, player, ability_index, restriction_gates)
    {
        return false;
    }

    let Some(ability) = obj.abilities.get(ability_index) else {
        return false;
    };
    if !ability
        .cost
        .as_ref()
        .is_some_and(crate::types::ability::is_loyalty_ability_cost)
    {
        return false;
    }

    // CR 602.5 + CR 603.2a: A `CantBeActivated` prohibition scoped to loyalty
    // abilities (The Immortal Sun — `kind = Some(Loyalty)`) blocks activation on
    // the loyalty path too. The untaxed loyalty fast path in
    // `handle_activate_loyalty` does NOT route through
    // `casting::handle_activate_ability`, so the shared prohibition gate must be
    // consulted here — this function is the single legality authority for both the
    // available-actions enumeration and `handle_activate_loyalty` (CR 606.3).
    if super::casting::is_blocked_by_cant_be_activated(state, player, planeswalker_id, ability) {
        return false;
    }

    match restriction_gates {
        Some(gates) => super::restrictions::check_activation_restrictions_with_static_gates(
            state,
            player,
            planeswalker_id,
            ability_index,
            &ability.activation_restrictions,
            gates,
        ),
        None => super::restrictions::check_activation_restrictions(
            state,
            player,
            planeswalker_id,
            ability_index,
            &ability.activation_restrictions,
        ),
    }
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

    // Extract the loyalty cost + counters and clone the definition so the
    // immutable object borrow ends before any `&mut state` path below (the
    // tax-delegation branch calls `handle_activate_ability`).
    let (loyalty_cost, current_loyalty, ability_def) = {
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
        (
            parse_loyalty_cost(ability_def),
            obj.loyalty.unwrap_or(0) as i32,
            ability_def.clone(),
        )
    };

    // CR 606.6: A loyalty ability with a negative loyalty cost can't be activated unless the
    // permanent has at least that many loyalty counters on it. Checked here for
    // BOTH the fast path and the tax-delegation branch below, so a `[−N]` ability
    // the planeswalker can't afford is refused before either path proceeds.
    if loyalty_cost < 0 && current_loyalty + loyalty_cost < 0 {
        return Err(EngineError::ActionNotAllowed(
            "Not enough loyalty to activate ability".to_string(),
        ));
    }

    // CR 118.7 + CR 601.2f + CR 606.1: When a cost-raise static (Eidolon of
    // Obstruction) adds a mana component to this loyalty ability, the mana-free
    // loyalty fast path can't pay it. Defer to the general activated-ability
    // flow, which applies the tax (`apply_cost_reduction`), prompts for the added
    // mana, pays the loyalty counters, records the CR 606.3 activation, and
    // re-enforces the once-per-turn gate. Untaxed loyalty abilities fall through
    // to the unchanged fast path.
    if super::casting::loyalty_ability_gains_mana_tax(state, &ability_def, player, pw_id) {
        return super::casting::handle_activate_ability(
            state,
            player,
            pw_id,
            ability_index,
            events,
        );
    }

    // Build a ResolvedAbility for the stack from the typed definition
    let mut resolved = build_pw_resolved(&ability_def, pw_id, player);

    // CR 606.1 + CR 602.2b: a loyalty ability IS an activated ability, so activating it
    // follows rules 601.2b-i exactly as casting a spell does — including CR 601.2b's
    // announcement of X. This mana-free fast path is an optimization of that same
    // announcement (it bypasses `casting::handle_activate_ability`, which publishes for
    // every other activated ability), so a text-defined, announce-locked X ("where X is
    // <count> as you activate this ability" — Lukka, Bound to Ruin's [-4]) must be
    // published HERE too, or it is never announced at all and every `Variable("X")` on the
    // ability resolves to 0. Same single computation authority, same position: at
    // announcement, before targets are chosen (CR 601.2c, immediately below).
    super::ability_utils::publish_announced_x(state, &mut resolved, player, pw_id);
    // CR 602.2 + CR 601.2c (capture L): the activation's journal facts, now,
    // before its targets are chosen and before any loyalty is paid. Interactive
    // targets add theirs at target settlement; automatic ones just below.
    // CR 602.2a: provenance too.
    resolved.ability_index = Some(ability_index);
    super::casting::record_activation_announcement(
        state,
        player,
        pw_id,
        ability_index,
        &mut resolved,
    );

    // CR 602.2b + CR 601.2c: Targets are announced before costs are paid.
    // If this ability requires targets, prompt for selection first.
    let target_slots = build_target_slots(state, &resolved)?;
    if !target_slots.is_empty() {
        // CR 115.1 + CR 701.9b: Random-target loyalty abilities — game picks via
        // `state.rng`. Routes through finalize_loyalty_activation just like the
        // controller-choice degenerate path.
        let target_constraints = ability_def.target_constraints.clone();
        let resolved_targets = if matches!(
            resolved.target_selection_mode,
            crate::types::ability::TargetSelectionMode::Random
        ) {
            Some(random_select_targets_for_ability(
                state,
                &target_slots,
                &target_constraints,
            )?)
        } else {
            auto_select_targets_for_ability(state, &resolved, &target_slots, &target_constraints)?
        };

        if let Some(targets) = resolved_targets {
            let mut resolved = resolved;
            assign_selected_slots_in_chain(state, &mut resolved, &targets)?;
            // CR 601.2c: the automatically chosen targets, captured before the
            // loyalty cost is paid.
            let captured = super::casting::capture_activation_record(
                state,
                player,
                pw_id,
                ability_index,
                &resolved,
            );
            if let (Some(record), Some(captured)) =
                (resolved.activation_record.as_deref_mut(), captured)
            {
                record.targets = captured.targets;
            }
            return finalize_loyalty_activation(
                state,
                player,
                pw_id,
                loyalty_cost,
                resolved,
                ability_index,
                events,
            );
        }

        state.lands_tapped_for_mana.remove(&player);

        let selection = begin_target_selection_for_ability(
            state,
            &resolved,
            &target_slots,
            &target_constraints,
        )?;
        // CR 606.1: the mana-free loyalty fast path carries no cost-modifier
        // snapshot — no reduction can touch a bare loyalty cost, and any raise
        // routes the ability to the general flow instead.
        let mut pending =
            PendingCast::for_activation(pw_id, resolved, ManaCost::NoCost, ability_index, None);
        pending.target_constraints = target_constraints;
        // CR 606.4: Loyalty cost is paid after targets are chosen.
        // Stored here so handle_select_targets can call pay_ability_cost and
        // record the CR 606.3 activation only after target selection completes.
        pending.activation_cost = Some(crate::types::ability::AbilityCost::Loyalty {
            amount: loyalty_cost,
        });
        // CR 601.2c + CR 606.3: first slot's announcer (controller unless the slot
        // is "of an opponent's choice").
        let initial_player = target_slots
            .first()
            .and_then(|slot| slot.chooser)
            .unwrap_or(player);
        return Ok(WaitingFor::TargetSelection {
            player: initial_player,
            pending_cast: Box::new(pending),
            target_slots,
            mode_labels: Vec::new(),
            selection,
        });
    }

    finalize_loyalty_activation(
        state,
        player,
        pw_id,
        loyalty_cost,
        resolved,
        ability_index,
        events,
    )
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
    state
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
/// Loyalty counter adjustment is delegated to `casting::pay_ability_cost_for_activation` — the single
/// authority for all ability cost resolution — to avoid duplicating counter logic here.
fn finalize_loyalty_activation(
    state: &mut GameState,
    player: PlayerId,
    pw_id: ObjectId,
    loyalty_cost: i32,
    mut resolved: ResolvedAbility,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 602.2 + CR 601.2c: refuse before paying if the pre-payment record is
    // missing (the activation is then reversed, with nothing recorded).
    super::casting::require_activation_record(&resolved, player)?;
    // CR 606.4: Single authority for loyalty cost payment.
    let cost = crate::types::ability::AbilityCost::Loyalty {
        amount: loyalty_cost,
    };
    match super::casting::pay_ability_cost_for_activation(
        state,
        player,
        pw_id,
        &cost,
        Some(ability_index),
        &mut resolved.context.cost_removed_counter_amount,
        events,
    )
    .expect("loyalty validation passed in handle_activate_loyalty")
    {
        super::casting::PaymentOutcome::Paid => {
            complete_loyalty_activation(state, player, pw_id, resolved, ability_index, events)
        }
        // CR 606.4 + CR 614.1a + CR 616.1: a count-modifying counter replacement
        // (Doubling Season vs an opponent's Vorinclex) can require a CR 616.1
        // ordering choice while the loyalty counters are placed. Park the
        // activation tail and surface the replacement prompt the payment already
        // set on `state.waiting_for`; the tail runs once the choice is settled.
        super::casting::PaymentOutcome::Paused { .. } => {
            state.pending_cost_move_resume = Some(PendingCostMoveResume::LoyaltyActivation {
                player,
                pw_id,
                resolved: Box::new(resolved),
                ability_index,
            });
            Ok(state.waiting_for.clone())
        }
        super::casting::PaymentOutcome::Failed { .. } => {
            unreachable!("loyalty cost cannot fail after can_activate_loyalty_ability passed")
        }
    }
}

/// CR 606.4 / CR 606.5: Post-payment tail of a loyalty activation — records the
/// activation, emits targeting events, pushes the ability onto the stack, and
/// returns Priority. Single authority for the tail: reached either directly (cost
/// `Paid`) or via [`resume_loyalty_activation`] after a paused counter-replacement
/// ordering choice. Never re-pays the loyalty cost.
fn complete_loyalty_activation(
    state: &mut GameState,
    player: PlayerId,
    pw_id: ObjectId,
    resolved: ResolvedAbility,
    ability_index: usize,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut resolved = resolved;
    let record = super::casting::take_activation_record(&mut resolved, player)?;
    record_loyalty_activation(state, pw_id, player);

    let assigned_targets = declared_targets_in_chain(&resolved);
    let crime_candidate = super::casting::targets_commit_crime(state, &assigned_targets, player);
    emit_targeting_events(
        state,
        &assigned_targets,
        pw_id,
        player,
        Some(crate::types::events::Targeter::Ability(
            crate::types::ability::StackAbilityKind::Activated,
        )),
        events,
    );

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
                ability: Box::new(resolved_with_idx),
            },
        },
        events,
    );
    super::casting::commit_crime_after_stack_placement(state, crime_candidate, player, events);

    // CR 606.2: `record_activated_ability_placed` classifies this as the
    // non-targeted loyalty-activation path. A loyalty ability is never
    // boast-tagged, so its boast emission is a no-op here.
    super::casting::record_activated_ability_placed(
        state,
        player,
        pw_id,
        ability_index,
        entry_id,
        record,
        events,
    );
    state.lands_tapped_for_mana.remove(&player);
    priority::clear_priority_passes(state);

    Ok(WaitingFor::Priority { player })
}

/// CR 606.4 + CR 616.1: Resume a loyalty activation parked while its loyalty
/// counter cost waited on a replacement ordering choice. The counter has already
/// been applied by the replacement pipeline; only the activation tail remains, so
/// this dispatches straight to [`complete_loyalty_activation`] without re-paying.
pub(crate) fn resume_loyalty_activation(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(PendingCostMoveResume::LoyaltyActivation {
        player,
        pw_id,
        resolved,
        ability_index,
    }) = state.pending_cost_move_resume.take()
    else {
        unreachable!("loyalty-activation resume requires its typed continuation")
    };
    complete_loyalty_activation(state, player, pw_id, *resolved, ability_index, events)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, CounterCostSelection,
        Effect, EffectScope, QuantityExpr, QuantityRef, TapStateChange, TargetFilter, TypedFilter,
        REMOVE_COUNTER_COST_X,
    };
    use crate::types::card_type::CoreType;
    use crate::types::counter::{CounterMatch, CounterType};
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

    fn make_minus_x_loyalty_ability(effect: Effect) -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, effect)
            .cost(AbilityCost::RemoveCounter {
                count: REMOVE_COUNTER_COST_X,
                counter_type: CounterMatch::OfType(CounterType::Loyalty),
                target: None,
                selection: CounterCostSelection::SingleObject,
            })
            .sorcery_speed()
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

    /// CR 606.5 + CR 107.3: a `[−X]` loyalty ability (modeled as removing X
    /// loyalty counters) activates through the generic activated-ability path,
    /// which announces X capped at current loyalty, removes the chosen X loyalty,
    /// records the CR 606.3 once-per-turn activation, and binds the chosen X into
    /// the effect. Uses a non-targeted "gain X life" body so no target-selection
    /// round-trip is needed; the X-damage cards (Chandra Nalaar, Jeska, …) share
    /// this exact cost + X-binding path. Issues #653 / #1069 / #2851.
    #[test]
    fn minus_x_loyalty_removes_chosen_x_and_binds_x_into_effect() {
        use crate::game::engine::apply_as_current;
        use crate::types::GameAction;

        let mut state = setup();

        // "[−X]: You gain X life." — the loyalty-X cost is a chosen-X removal of
        // loyalty counters (exactly what the parser builds for `[−X]:` lines).
        let ability = make_minus_x_loyalty_ability(Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            player: TargetFilter::Controller,
        });

        let pw = create_planeswalker(&mut state, PlayerId(0), "Variable Walker", 6, vec![ability]);
        let life_before = state.players[0].life;

        // Activating must prompt for X, capped at the current loyalty (6).
        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: pw,
                ability_index: 0,
            },
        )
        .expect("activation must be accepted");
        match &state.waiting_for {
            WaitingFor::ChooseXValue { max, .. } => {
                assert_eq!(*max, 6, "X must be capped at current loyalty (6)")
            }
            other => panic!("expected ChooseXValue, got {other:?}"),
        }

        // Choosing X = 4 pays the cost: remove 4 loyalty counters, keeping
        // `loyalty` and the counter map in sync (CR 306.5b), and records the
        // CR 606.3 once-per-turn loyalty activation.
        apply_as_current(&mut state, GameAction::ChooseX { value: 4 })
            .expect("choosing X=4 must be accepted");
        assert_eq!(
            state.objects[&pw].loyalty,
            Some(2),
            "loyalty must drop by the chosen X (6 - 4)"
        );
        assert_eq!(
            state.objects[&pw]
                .counters
                .get(&CounterType::Loyalty)
                .copied()
                .unwrap_or(0),
            2,
            "loyalty counter map must stay in sync with the loyalty field"
        );
        assert!(
            state.objects[&pw].loyalty_activations_this_turn > 0,
            "CR 606.3: the loyalty activation must be recorded (once-per-turn gate)"
        );

        // Resolving binds the chosen X into the effect: gain X (= 4) life.
        let mut events = Vec::new();
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert_eq!(
            state.players[0].life,
            life_before + 4,
            "the effect must gain the chosen X (4) life"
        );
    }

    /// CR 606.3: The generic `GameAction::ActivateAbility` path must enforce
    /// the same once-per-turn loyalty gate before a `[−X]` cost can prompt for X.
    #[test]
    fn minus_x_loyalty_direct_activation_rejected_after_other_loyalty_ability() {
        use crate::game::engine::apply_as_current;
        use crate::types::GameAction;

        let mut state = setup();
        let plus_one = make_loyalty_ability(
            1,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        let minus_x = make_minus_x_loyalty_ability(Effect::GainLife {
            amount: QuantityExpr::Ref {
                qty: QuantityRef::CostXPaid,
            },
            player: TargetFilter::Controller,
        });
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Variable Walker",
            4,
            vec![plus_one, minus_x],
        );

        apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: pw,
                ability_index: 0,
            },
        )
        .expect("first loyalty activation must be accepted");
        state.stack.clear();
        let loyalty_after_first_activation = state.objects[&pw].loyalty;

        let result = apply_as_current(
            &mut state,
            GameAction::ActivateAbility {
                source_id: pw,
                ability_index: 1,
            },
        );

        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "second same-turn loyalty activation must be rejected, got {result:?}"
        );
        assert!(
            !matches!(state.waiting_for, WaitingFor::ChooseXValue { .. }),
            "the rejected `[−X]` activation must not prompt for X"
        );
        assert_eq!(
            state.objects[&pw].loyalty, loyalty_after_first_activation,
            "the rejected `[−X]` activation must not remove loyalty counters"
        );
        assert!(
            state.stack.is_empty(),
            "the rejected `[−X]` activation must not put an ability on the stack"
        );
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
        state.stack.clear();
        state.waiting_for = WaitingFor::Priority {
            player: state.priority_player,
        };
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

    /// Issue #9936 (P1-C4): a quoted-text granted loyalty ability carries
    /// sorcery-speed timing (CR 606.3 timing half), so it refuses off-turn /
    /// non-main / non-empty-stack and allows on own main phase with an empty
    /// stack — exactly like a printed loyalty ability. The def is extracted
    /// from the production quoted-grant path (the exact Phase-3-consumed
    /// shape), installed via `create_planeswalker` — NOT `make_loyalty_ability`
    /// (that helper adds `OnlyOnceEachTurn`, which would conflate the fixture).
    ///
    /// Leg discipline is load-bearing: refusals run BEFORE the allow on the
    /// shared walker, so the activation count stays 0 through the refusals and
    /// each refusal is attributable to `AsSorcery`, not the per-permanent cap
    /// (`planeswalker.rs:129` precedes the restriction consult at `:155-172`,
    /// behind the undiscriminating `ActionNotAllowed`). Revert the attach and
    /// the FIRST refusal leg returns Ok → red.
    #[test]
    fn quoted_granted_loyalty_ability_requires_sorcery_speed() {
        use crate::parser::oracle_static::parse_quoted_ability_modifications;
        use crate::types::ContinuousModification;

        let parsed = parse_quoted_ability_modifications("\"[0]: Proliferate\"");
        let quoted_def = match parsed.as_slice() {
            [ContinuousModification::GrantAbility { definition }] => (**definition).clone(),
            other => panic!("expected single GrantAbility, got {other:?}"),
        };
        assert_eq!(quoted_def.kind, AbilityKind::Activated);
        assert!(
            matches!(quoted_def.cost, Some(AbilityCost::Loyalty { amount: 0 })),
            "fixture def must be [0] loyalty, got {:?}",
            quoted_def.cost
        );
        assert!(
            matches!(*quoted_def.effect, Effect::Proliferate),
            "fixture def effect must be Proliferate, got {:?}",
            quoted_def.effect
        );

        let mut state = setup();
        let pw = create_planeswalker(&mut state, PlayerId(0), "Jace", 3, vec![quoted_def.clone()]);

        // REFUSALS FIRST. Count must stay 0 through all three legs so each
        // refusal is attributable to AsSorcery, not the cap.
        // Leg 1: not main phase.
        state.phase = Phase::DeclareAttackers;
        let mut events = Vec::new();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err(), "non-main-phase activation must refuse");
        assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 0);

        // Leg 2: not active player (opponent's turn).
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_err(), "off-turn activation must refuse");
        assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 0);

        // Leg 3: stack not empty.
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
        assert!(result.is_err(), "stack-nonempty activation must refuse");
        assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 0);

        // ALLOW LAST (reach-guard): own main phase, empty stack → Ok, with the
        // activation count recorded and the ability on the stack.
        state.stack.clear();
        let result = handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events);
        assert!(result.is_ok(), "own-main-empty activation must allow");
        assert!(state.objects[&pw].loyalty_activations_this_turn > 0);
        assert!(!state.stack.is_empty());

        // Non-walker holder pin (P1-R2 preservation): the same quoted def
        // direct-installed on a non-walker object refuses via the planeswalker
        // type check (`:88`) — which precedes the restriction consult
        // (`:155-172`) — both before and after the attach. Pins
        // gate-structural stability, not the fix; excluded from the revert-red
        // claim.
        let holder = create_object(
            &mut state,
            CardId(777),
            PlayerId(0),
            "Non-walker holder".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&holder).unwrap().abilities = Arc::new(vec![quoted_def]);
        state.stack.clear();
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        let result = handle_activate_loyalty(&mut state, PlayerId(0), holder, 0, &mut events);
        assert!(
            result.is_err(),
            "non-walker holder must refuse pre- and post-fix"
        );
    }

    /// Build the Phase 3 activation board: Bolas(4) with one hand-built printed
    /// +1 and hand-built grant statics (the verbatim line now refuses at
    /// parse, so the parser is bypassed here by design), plus a donor walker
    /// (3) with a +1 gain-life def and a targeted -3 destroy def, in the
    /// `setup()` position (P0's precombat main, P0 priority, empty stack),
    /// layers evaluated. Printed and donor defs carry `AsSorcery` ONLY —
    /// never `make_loyalty_ability` (that helper adds `OnlyOnceEachTurn`,
    /// which would conflate cap attribution, per P1-C4). When `timed` is
    /// false the DONOR defs are built restriction-free (the P3-C4
    /// carriage-drop probe); Bolas's own def stays timed.
    ///
    /// `create_planeswalker` installs abilities, types, and loyalty on the live
    /// object only, so the base fields are synced for the layer pass that
    /// follows. Returns `(state, bolas, donor, own_plus, donor_plus,
    /// donor_minus)`; the setup assertions make a runtime-expansion revert
    /// fail here (red: donated defs absent), not downstream.
    fn bolas_plus_donor_board(
        timed: bool,
    ) -> (
        GameState,
        ObjectId,
        ObjectId,
        AbilityDefinition,
        AbilityDefinition,
        AbilityDefinition,
    ) {
        use crate::game::layers::evaluate_layers;

        fn time(mut def: AbilityDefinition, timed: bool) -> AbilityDefinition {
            if timed {
                def = def.sorcery_speed();
            }
            def
        }

        let mut state = setup();
        let own_plus = time(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Loyalty { amount: 1 }),
            true,
        );
        let bolas = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Nicol Bolas, Dragon-God",
            4,
            vec![own_plus.clone()],
        );
        {
            let obj = state.objects.get_mut(&bolas).unwrap();
            obj.base_abilities = Arc::clone(&obj.abilities);
            obj.base_card_types = obj.card_types.clone();
            obj.base_loyalty = obj.loyalty;
            // Hand-built equivalent of the refused verbatim grant line (same
            // source filter the parser used to lower): the runtime authority
            // is tested in isolation from the parser refusal.
            use crate::types::ability::{
                ContinuousModification, FilterProp, StaticDefinition, TypeFilter,
            };
            use crate::types::events::ActivatedAbilityKind;
            obj.static_definitions = vec![StaticDefinition::continuous()
                .affected(TargetFilter::SelfRef)
                .modifications(vec![ContinuousModification::GrantAllActivatedAbilitiesOf {
                    source: TargetFilter::And {
                        filters: vec![
                            TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Planeswalker).properties(vec![
                                    FilterProp::InZone {
                                        zone: Zone::Battlefield,
                                    },
                                ]),
                            ),
                            TargetFilter::Not {
                                filter: Box::new(TargetFilter::SelfRef),
                            },
                        ],
                    },
                    cap: None,
                    kind: Some(ActivatedAbilityKind::Loyalty),
                }])]
            .into();
        }

        let donor_plus = time(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::GainLife {
                    amount: QuantityExpr::Fixed { value: 1 },
                    player: TargetFilter::Controller,
                },
            )
            .cost(AbilityCost::Loyalty { amount: 1 }),
            timed,
        );
        let donor_minus = time(
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            )
            .cost(AbilityCost::Loyalty { amount: -3 }),
            timed,
        );
        let donor = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Donor Walker",
            3,
            vec![donor_plus.clone(), donor_minus.clone()],
        );
        {
            let obj = state.objects.get_mut(&donor).unwrap();
            obj.base_abilities = Arc::clone(&obj.abilities);
            obj.base_card_types = obj.card_types.clone();
            obj.base_loyalty = obj.loyalty;
        }

        state.layers_dirty.mark_full();
        evaluate_layers(&mut state);

        // Setup assertions: Bolas holds its printed +1 plus both donated defs.
        for def in [&own_plus, &donor_plus, &donor_minus] {
            assert!(
                state.objects[&bolas].abilities.iter().any(|a| a == def),
                "Bolas must hold def {def:?} post-layers; got {:?}",
                state.objects[&bolas].abilities
            );
        }
        (state, bolas, donor, own_plus, donor_plus, donor_minus)
    }

    /// Index of `def` in `id`'s live abilities. Re-resolved after every layer
    /// pass rather than cached, so controller-change legs cannot go stale.
    fn ability_index_of(state: &GameState, id: ObjectId, def: &AbilityDefinition) -> usize {
        state.objects[&id]
            .abilities
            .iter()
            .position(|a| a == def)
            .unwrap_or_else(|| {
                panic!(
                    "expected def {def:?} on {id:?}; got {:?}",
                    state.objects[&id].abilities
                )
            })
    }

    /// CR 606.4 + CR 606.6: the donated +1 activates on the controller's main
    /// phase with an empty stack — Bolas loyalty 4→5, the donor unchanged, the
    /// ability on the stack with Bolas as source, and the gain-life effect
    /// resolves. Reverting the runtime expansion strands the board setup
    /// assertion (red: donated ability absent).
    #[test]
    fn bolas_donated_plus_one_activates_and_moves_counters_on_bolas() {
        let (mut state, bolas, donor, _, donor_plus, _) = bolas_plus_donor_board(true);
        let mut events = Vec::new();
        let life_before = state.players[0].life;

        let donated_plus_index = ability_index_of(&state, bolas, &donor_plus);
        let waiting = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_plus_index,
            &mut events,
        )
        .expect("donated +1 must activate on own main phase with empty stack");
        assert!(
            matches!(waiting, WaitingFor::Priority { .. }),
            "untargeted activation must return to priority, got {waiting:?}"
        );
        assert_eq!(
            state.objects[&bolas].loyalty,
            Some(5),
            "Bolas pays the donated +1 (4→5)"
        );
        assert_eq!(
            state.objects[&donor].loyalty,
            Some(3),
            "donor loyalty unchanged by Bolas's activation"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);
        assert_eq!(state.stack.len(), 1);
        assert_eq!(
            state.stack.back().expect("stacked ability").source_id,
            bolas,
            "stacked ability source is Bolas"
        );

        // The donated effect resolves.
        crate::game::stack::resolve_top(&mut state, &mut events);
        assert!(state.stack.is_empty());
        assert_eq!(
            state.players[0].life,
            life_before + 1,
            "the donated gain-life effect must resolve"
        );

        // Hostile (fresh board — the cap is spent above): the donated [-3]
        // destroy with a target announces the target BEFORE Bolas pays
        // (CR 602.2b + CR 601.2c): parks on TargetSelection with loyalty
        // untouched and the pending loyalty cost stored; completing the
        // selection pays on Bolas (4→1, donor unchanged) and stacks the
        // ability. Two victims so auto-select cannot collapse to Priority.
        use crate::types::ability::TargetRef;
        let (mut state, bolas, donor, _, _, donor_minus) = bolas_plus_donor_board(true);
        for card_id in [99u64, 100] {
            let c = create_object(
                &mut state,
                CardId(card_id),
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
        let victim_id = state
            .objects
            .iter()
            .find(|(_, o)| o.name == "Goblin")
            .map(|(id, _)| *id)
            .expect("a goblin victim");
        let donated_minus_index = ability_index_of(&state, bolas, &donor_minus);
        let waiting = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_minus_index,
            &mut events,
        )
        .expect("donated -3 must reach target selection");
        assert_eq!(
            state.objects[&bolas].loyalty,
            Some(4),
            "loyalty unchanged before target selection"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 0);
        assert!(
            matches!(waiting, WaitingFor::TargetSelection { .. }),
            "expected TargetSelection, got {waiting:?}"
        );
        if let WaitingFor::TargetSelection { pending_cast, .. } = &waiting {
            assert!(
                matches!(
                    pending_cast.activation_cost,
                    Some(AbilityCost::Loyalty { amount: -3 })
                ),
                "loyalty cost must be stored for deferred payment, got {:?}",
                pending_cast.activation_cost
            );
            assert_eq!(
                pending_cast.object_id, bolas,
                "deferred payment source is Bolas"
            );
        }
        state.waiting_for = waiting;
        crate::game::casting_targets::handle_select_targets(
            &mut state,
            PlayerId(0),
            vec![TargetRef::Object(victim_id)],
            &mut events,
        )
        .expect("target selection must complete");
        assert_eq!(
            state.objects[&bolas].loyalty,
            Some(1),
            "Bolas pays the donated -3 after targets are announced (4→1)"
        );
        assert_eq!(
            state.objects[&donor].loyalty,
            Some(3),
            "donor loyalty unchanged by Bolas's activation"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);
        assert_eq!(state.stack.len(), 1);
    }

    /// CR 606.3 (timing half): the donated +1 refuses off-turn / non-main /
    /// non-empty-stack via its carried `AsSorcery`, and allows on own main
    /// phase with an empty stack. Leg discipline is load-bearing (P1 F6):
    /// refusals run BEFORE the allow on the shared board, so the activation
    /// count stays 0 through the refusals and each refusal is attributable to
    /// `AsSorcery`, not the per-permanent cap. Reverting the runtime expansion
    /// strands the board setup assertion (red: donated ability absent).
    #[test]
    fn bolas_donated_ability_requires_sorcery_speed() {
        let (mut state, bolas, _, _, donor_plus, _) = bolas_plus_donor_board(true);
        let donated_plus_index = ability_index_of(&state, bolas, &donor_plus);

        // REFUSALS FIRST. Count must stay 0 through all three legs so each
        // refusal is attributable to AsSorcery, not the cap.
        // Leg 1: not main phase (combat).
        let mut events = Vec::new();
        state.phase = Phase::DeclareAttackers;
        let result = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_plus_index,
            &mut events,
        );
        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "combat-phase activation must refuse, got {result:?}"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 0);

        // Leg 2: not active player (opponent's turn, no instant permission).
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        let result = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_plus_index,
            &mut events,
        );
        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "off-turn activation must refuse, got {result:?}"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 0);

        // Leg 3: stack not empty.
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
        let result = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_plus_index,
            &mut events,
        );
        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "stack-nonempty activation must refuse, got {result:?}"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 0);

        // ALLOW LAST (reach-guard): own main phase, empty stack → Ok.
        state.stack.clear();
        let result = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_plus_index,
            &mut events,
        );
        assert!(result.is_ok(), "own-main-empty activation must allow");
        assert!(state.objects[&bolas].loyalty_activations_this_turn > 0);
        assert!(!state.stack.is_empty());

        // Carriage-drop probe (green-when-should-be-red): the SAME board with
        // the AsSorcery carriage dropped from the donor defs allows off-turn —
        // proving the refusals above ride on the donated def's carried
        // restriction, not on some ambient gate.
        let (mut state, bolas, _, _, donor_plus, _) = bolas_plus_donor_board(false);
        assert!(
            donor_plus.activation_restrictions.is_empty(),
            "probe board must drop the AsSorcery carriage; got {:?}",
            donor_plus.activation_restrictions
        );
        let donated_plus_index = ability_index_of(&state, bolas, &donor_plus);
        state.phase = Phase::PreCombatMain;
        state.active_player = PlayerId(1);
        let result = handle_activate_loyalty(
            &mut state,
            PlayerId(0),
            bolas,
            donated_plus_index,
            &mut events,
        );
        assert!(
            result.is_ok(),
            "carriage dropped → off-turn activation must allow (green-when-should-be-red), got {result:?}"
        );
        assert_eq!(
            state.objects[&bolas].loyalty,
            Some(5),
            "the uncarriaged activation still pays on Bolas (4→5)"
        );
    }

    /// CR 606.3 (frequency half): one loyalty activation per permanent per turn
    /// across printed + donated — in BOTH orders — and the cap is owned by the
    /// ACTIVATING permanent (`planeswalker.rs:129` reads `pw_id` = Bolas).
    /// Stack is cleared between same-board attempts so each refusal is
    /// attributable to the cap, not stack-nonempty AsSorcery. Reverting the
    /// runtime expansion strands the board setup assertions (red: donated defs
    /// absent, order-2 untestable).
    #[test]
    fn bolas_donated_and_printed_share_one_per_turn_cap() {
        // Order 1: own +1 first (allowed), donated second (refused).
        let (mut state, bolas, _, own_plus, donor_plus, _) = bolas_plus_donor_board(true);
        let mut events = Vec::new();
        let own_index = ability_index_of(&state, bolas, &own_plus);
        let donated_index = ability_index_of(&state, bolas, &donor_plus);
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, own_index, &mut events)
            .expect("first activation (own +1) must allow");
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);
        state.stack.clear();
        let result =
            handle_activate_loyalty(&mut state, PlayerId(0), bolas, donated_index, &mut events);
        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "second activation (donated) must refuse on the shared cap, got {result:?}"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);

        // Order 2 (fresh board): donated first (allowed), own +1 second
        // (refused).
        let (mut state, bolas, _, own_plus, donor_plus, _) = bolas_plus_donor_board(true);
        let own_index = ability_index_of(&state, bolas, &own_plus);
        let donated_index = ability_index_of(&state, bolas, &donor_plus);
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, donated_index, &mut events)
            .expect("first activation (donated) must allow");
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);
        state.stack.clear();
        let result =
            handle_activate_loyalty(&mut state, PlayerId(0), bolas, own_index, &mut events);
        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "second activation (own +1) must refuse on the shared cap, got {result:?}"
        );
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);

        // Multi-authority (1): the donor activates its own ability (donor
        // count 1), then Bolas activates the donated copy → ALLOWED (Bolas
        // 0→1). First branch: `loyalty_activations_this_turn >= cap` false on
        // the activating permanent in each case.
        let (mut state, bolas, donor, _, donor_plus, _) = bolas_plus_donor_board(true);
        handle_activate_loyalty(&mut state, PlayerId(0), donor, 0, &mut events)
            .expect("donor must activate its own +1 (index 0)");
        assert_eq!(state.objects[&donor].loyalty_activations_this_turn, 1);
        state.stack.clear();
        let donated_index = ability_index_of(&state, bolas, &donor_plus);
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, donated_index, &mut events)
            .expect("Bolas must activate the donated copy after the donor used its own");
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);

        // Multi-authority (2): Bolas activates the donated copy, then the
        // donor activates its printed original → ALLOWED.
        let (mut state, bolas, donor, _, donor_plus, _) = bolas_plus_donor_board(true);
        let donated_index = ability_index_of(&state, bolas, &donor_plus);
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, donated_index, &mut events)
            .expect("Bolas must activate the donated copy first");
        state.stack.clear();
        handle_activate_loyalty(&mut state, PlayerId(0), donor, 0, &mut events)
            .expect("donor must activate its printed original after Bolas used the copy");
        assert_eq!(state.objects[&donor].loyalty_activations_this_turn, 1);

        // Multi-authority (3): the donor changes controller mid-turn — still
        // donated (no controller axis) and Bolas's cap unaffected. Both
        // controller fields move: Layer 2 recomputes `controller` from
        // `base_controller` on every pass, so moving one alone would revert.
        let (mut state, bolas, donor, _, donor_plus, _) = bolas_plus_donor_board(true);
        {
            let obj = state.objects.get_mut(&donor).unwrap();
            obj.controller = PlayerId(1);
            obj.base_controller = Some(PlayerId(1));
        }
        state.layers_dirty.mark_full();
        crate::game::layers::evaluate_layers(&mut state);
        let donated_index = ability_index_of(&state, bolas, &donor_plus);
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, donated_index, &mut events)
            .expect("Bolas must activate the donated copy after the donor changed controller");
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 1);

        // Multi-authority (4): a Chain Veil extra activation raises the shared
        // cap (reaches the `base_cap` branch) → the second use is ALLOWED —
        // proving the refusals above are the shared counter, not a per-ability
        // flag (which the Veil does not touch). A third use is still refused
        // (proving +1, not +infinity).
        let (mut state, bolas, _, own_plus, donor_plus, _) = bolas_plus_donor_board(true);
        // Seed the Chain Veil +1 grant directly (the Chain Veil resolver's own
        // unit-test precedent writes this same map; cf. Oath of Teferi).
        state
            .extra_loyalty_activations_this_turn
            .insert(PlayerId(0), 1);
        let own_index = ability_index_of(&state, bolas, &own_plus);
        let donated_index = ability_index_of(&state, bolas, &donor_plus);
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, own_index, &mut events)
            .expect("first activation must allow under the Veil grant");
        state.stack.clear();
        handle_activate_loyalty(&mut state, PlayerId(0), bolas, donated_index, &mut events)
            .expect("second activation must allow under the Veil grant (cap 2)");
        assert_eq!(state.objects[&bolas].loyalty_activations_this_turn, 2);
        state.stack.clear();
        let result =
            handle_activate_loyalty(&mut state, PlayerId(0), bolas, own_index, &mut events);
        assert!(
            matches!(result, Err(EngineError::ActionNotAllowed(_))),
            "third activation must refuse (Veil grants +1, not +infinity), got {result:?}"
        );
    }

    /// CR 606.2: a targeted `[-X]` loyalty ability's printed cost is
    /// `RemoveCounter { X loyalty counters }`, which `is_loyalty_ability_cost`
    /// recognizes. The X-cost path clears `pending.activation_cost` before the
    /// targeted finalize (casting_costs.rs), so the kind MUST be derived from the
    /// stable printed cost via `ActivatedAbilityKind::of_definition` — reading the cleared
    /// `pending.activation_cost` would mis-classify it `Normal` and the
    /// "whenever you activate a loyalty ability" trigger would miss this subclass.
    #[test]
    fn activated_ability_kind_classifies_minus_x_loyalty() {
        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Vraska",
            5,
            vec![make_minus_x_loyalty_ability(Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            })],
        );
        assert_eq!(
            crate::types::events::ActivatedAbilityKind::of_definition(
                &state.objects[&pw].abilities[0]
            ),
            crate::types::events::ActivatedAbilityKind::Loyalty,
            "a [-X] loyalty ability's printed cost must classify as Loyalty"
        );

        // Sibling: a non-loyalty activated ability classifies as Normal.
        let normal_pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Tinkerer",
            0,
            vec![AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
            )],
        );
        assert_eq!(
            crate::types::events::ActivatedAbilityKind::of_definition(
                &state.objects[&normal_pw].abilities[0]
            ),
            crate::types::events::ActivatedAbilityKind::Normal,
        );
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
                Effect::SetTapState {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    scope: EffectScope::Single,
                    state: TapStateChange::Tap,
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
        // Activation is recorded only after target selection completes and the
        // loyalty cost is paid.
        assert_eq!(state.objects[&pw].loyalty_activations_this_turn, 0);
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

    /// Issue #878: Teferi +1 must be activatable alongside -3; mis-parsing the
    /// +1 as a targeted `CastFromZone` made only -3 legal, so the UI auto-fired
    /// the bounce when the player clicked Teferi.
    #[test]
    fn teferi_time_raveler_plus_one_and_minus_three_both_legal_at_four_loyalty() {
        use crate::game::casting::can_activate_ability_now;
        use crate::parser::oracle::parse_oracle_text;

        let parsed = parse_oracle_text(
            "Each opponent can cast spells only any time they could cast a sorcery.\n\
             [+1]: Until your next turn, you may cast sorcery spells as though they had flash.\n\
             [\u{2212}3]: Return up to one target artifact, creature, or enchantment to its owner's hand. Draw a card.",
            "Teferi, Time Raveler",
            &[],
            &["Planeswalker".to_string()],
            &["Teferi".to_string()],
        );
        assert_eq!(parsed.abilities.len(), 2);

        let mut state = setup();
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Teferi, Time Raveler",
            4,
            parsed.abilities,
        );

        assert!(
            can_activate_ability_now(&state, PlayerId(0), pw, 0),
            "+1 must be legal at 4 loyalty with an empty board"
        );
        assert!(
            can_activate_ability_now(&state, PlayerId(0), pw, 1),
            "-3 must be legal at 4 loyalty with an empty board (up-to-one target)"
        );

        // Activating the +1 must put exactly the +1 grant (a GenericEffect) on
        // the stack — never the -3 bounce. The original bug auto-dispatched the
        // sole-legal -3 because the +1 was mis-parsed as a targeted ability.
        let mut events = Vec::new();
        handle_activate_loyalty(&mut state, PlayerId(0), pw, 0, &mut events)
            .expect("+1 activation succeeds");
        assert_eq!(state.stack.len(), 1, "exactly one ability on the stack");
        let on_stack = state.stack.iter().next().unwrap();
        assert_eq!(on_stack.source_id, pw, "the stacked ability is Teferi's +1");
        assert!(
            matches!(
                on_stack.ability().map(|a| &a.effect),
                Some(Effect::GenericEffect { .. })
            ),
            "the +1 (flash-timing GenericEffect) was activated, not the -3 bounce: {:?}",
            on_stack.ability().map(|a| &a.effect)
        );
    }

    /// Cluster J2 (legal-action-seam regression guard): a planeswalker's
    /// negative-loyalty ability must NOT be offered when the permanent lacks
    /// enough loyalty counters to pay it. An Ob-Nixilis-shaped planeswalker
    /// (loyalty abilities +2 / -2 / -8) at 1 loyalty offers ONLY the +2 at the
    /// `can_activate_ability_now` legal-action seam — the exact layer whose
    /// leak the report ("Ob Nixilis used -2 at 1 loyalty") alleged.
    ///
    /// CR 606.6: A loyalty ability with a negative loyalty cost can't be
    /// activated unless the permanent has at least that many loyalty counters on
    /// it. The +2 reach-guard proves the enumerator reaches each ability and the
    /// gate is loyalty-sensitive (not blanket-false for negatives), and the
    /// 2-loyalty boundary case proves -2 flips to payable at exactly 2 (2 >= 2).
    #[test]
    fn negative_loyalty_ability_not_offered_below_cost() {
        use crate::game::casting::can_activate_ability_now;

        fn draw(n: i32) -> Effect {
            Effect::Draw {
                count: QuantityExpr::Fixed { value: n },
                target: TargetFilter::Controller,
            }
        }

        fn ob_nixilis_abilities() -> Vec<AbilityDefinition> {
            vec![
                make_loyalty_ability(2, draw(1)),
                make_loyalty_ability(-2, draw(2)),
                make_loyalty_ability(-8, draw(7)),
            ]
        }

        let mut state = setup();

        // At 1 loyalty: +2 is always payable; -2 and -8 are not (CR 606.6).
        let pw = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Ob Nixilis of the Black Oath",
            1,
            ob_nixilis_abilities(),
        );

        assert!(
            can_activate_ability_now(&state, PlayerId(0), pw, 0),
            "+2 must be offered at 1 loyalty (reach-guard: the enumerator reaches this ability)"
        );
        assert!(
            !can_activate_ability_now(&state, PlayerId(0), pw, 1),
            "CR 606.6: -2 must NOT be offered at 1 loyalty (1 < 2)"
        );
        assert!(
            !can_activate_ability_now(&state, PlayerId(0), pw, 2),
            "CR 606.6: -8 must NOT be offered at 1 loyalty (1 < 8)"
        );

        // Boundary: at exactly 2 loyalty, -2 becomes payable (2 >= 2), proving
        // the gate is loyalty-sensitive rather than blanket-false for negatives.
        let pw2 = create_planeswalker(
            &mut state,
            PlayerId(0),
            "Ob Nixilis of the Black Oath",
            2,
            ob_nixilis_abilities(),
        );
        assert!(
            can_activate_ability_now(&state, PlayerId(0), pw2, 1),
            "CR 606.6 boundary: -2 becomes payable at exactly 2 loyalty (2 >= 2)"
        );
        assert!(
            !can_activate_ability_now(&state, PlayerId(0), pw2, 2),
            "CR 606.6: -8 still not payable at 2 loyalty (2 < 8)"
        );
    }

    /// A Chain Veil-style loyalty ability: no `OnlyOnceEachTurn`, so the
    /// per-ability-index restriction cannot mask the CR 606.3 per-permanent cap
    /// these tests exercise.
    fn make_uncapped_loyalty_ability(loyalty_amount: i32) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        )
        .cost(AbilityCost::Loyalty {
            amount: loyalty_amount,
        })
        .sorcery_speed()
    }

    fn walker_with_grant(
        state: &mut GameState,
        owner: PlayerId,
        abilities: Vec<AbilityDefinition>,
        count: u32,
        extra: u32,
    ) -> ObjectId {
        let pw = create_planeswalker(state, owner, "Probe Walker", 5, abilities);
        let obj = state.objects.get_mut(&pw).unwrap();
        obj.loyalty_activations_this_turn = count;
        obj.extra_loyalty_activations_this_turn = extra;
        pw
    }

    /// CR 606.3 + CR 101.1: a per-permanent grant of N raises the cap to N + 1.
    /// The refusal at count 3 and the paired ungranted control (cap 1, count 1
    /// refused) show the cap, not the activation count, decides.
    #[test]
    fn permanent_grant_raises_the_cap_by_its_amount() {
        let mut state = setup();
        let granted = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![make_uncapped_loyalty_ability(1)],
            0,
            2,
        );
        for count in [0, 1, 2] {
            state
                .objects
                .get_mut(&granted)
                .unwrap()
                .loyalty_activations_this_turn = count;
            assert!(
                can_activate_loyalty_ability(&state, granted, PlayerId(0), 0),
                "count {count} is under a cap of 3"
            );
        }
        state
            .objects
            .get_mut(&granted)
            .unwrap()
            .loyalty_activations_this_turn = 3;
        assert!(
            !can_activate_loyalty_ability(&state, granted, PlayerId(0), 0),
            "count 3 reaches the cap of 3"
        );

        let ungranted = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![make_uncapped_loyalty_ability(1)],
            1,
            0,
        );
        assert!(
            !can_activate_loyalty_ability(&state, ungranted, PlayerId(0), 0),
            "paired control: without a grant, count 1 reaches the base cap of 1"
        );
    }

    /// CR 606.3: the permanent-scoped and player-wide grants compose additively
    /// in the one gate: 1 base + 1 player-wide + 2 permanent-scoped.
    #[test]
    fn permanent_grant_is_additive_with_the_player_wide_grant() {
        let mut state = setup();
        let pw = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![make_uncapped_loyalty_ability(1)],
            0,
            2,
        );
        let obj = &state.objects[&pw];
        assert_eq!(
            loyalty_activation_cap(&state, obj, PlayerId(0), 0, None),
            3,
            "reach-guard: the permanent grant alone gives 1 + 2"
        );

        state
            .extra_loyalty_activations_this_turn
            .insert(PlayerId(0), 1);
        let obj = &state.objects[&pw];
        assert_eq!(loyalty_activation_cap(&state, obj, PlayerId(0), 0, None), 4);
    }

    /// CR 606.3: neither grant reaches an opponent's planeswalker in the same
    /// turn. No turn rollover is involved, so the sweep cannot make this pass
    /// vacuously.
    #[test]
    fn grants_do_not_reach_an_opponents_planeswalker_in_the_same_turn() {
        let mut state = setup();
        let own = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![make_uncapped_loyalty_ability(1)],
            0,
            2,
        );
        let opposing = walker_with_grant(
            &mut state,
            PlayerId(1),
            vec![make_uncapped_loyalty_ability(1)],
            0,
            0,
        );
        state
            .extra_loyalty_activations_this_turn
            .insert(PlayerId(0), 1);

        assert_eq!(
            loyalty_activation_cap(&state, &state.objects[&own], PlayerId(0), 0, None),
            4,
            "reach-guard: P0's walker holds both grants"
        );
        assert_eq!(
            loyalty_activation_cap(&state, &state.objects[&opposing], PlayerId(1), 0, None),
            1,
            "P1's walker gets only the base cap"
        );
    }

    /// CR 613.1f + the card's ruling: the permanent-scoped grant covers the
    /// permanent's own loyalty ability, not one gained in layer 6. The paired
    /// control (`granted_abilities_from = None`) allows the same index, so
    /// provenance, not the activation count, is what refuses it.
    #[test]
    fn permanent_grant_does_not_cover_a_gained_loyalty_ability() {
        let mut state = setup();
        let pw = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![
                make_uncapped_loyalty_ability(1),
                make_uncapped_loyalty_ability(0),
            ],
            1,
            2,
        );
        state.objects.get_mut(&pw).unwrap().granted_abilities_from = Some(1);

        let obj = &state.objects[&pw];
        assert_eq!(
            obj.activated_ability_provenance(1),
            AbilityProvenance::Granted
        );
        assert!(matches!(
            obj.activated_ability_provenance(0),
            AbilityProvenance::Characteristic(_)
        ));
        assert!(
            can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
            "the printed ability is covered (count 1 < cap 3)"
        );
        assert!(
            !can_activate_loyalty_ability(&state, pw, PlayerId(0), 1),
            "the gained ability is not covered (count 1 >= cap 1)"
        );

        state.objects.get_mut(&pw).unwrap().granted_abilities_from = None;
        assert!(
            can_activate_loyalty_ability(&state, pw, PlayerId(0), 1),
            "paired control: the same index is covered when it is not gained"
        );
    }

    /// CR 707.2 + CR 613.1a: a loyalty ability a copy effect supplied is still a
    /// characteristic ability of the object, so a Clone or token copy of the
    /// granting planeswalker keeps its own grant. Narrowing the gate to
    /// `Characteristic(CharacteristicSetRef::Own)` would refuse it (cap 1).
    #[test]
    fn permanent_grant_covers_an_ability_supplied_by_a_copy_effect() {
        use crate::types::ability::{CharacteristicSetRef, CopyEffectInstanceRef};

        let mut state = setup();
        let pw = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![make_uncapped_loyalty_ability(1)],
            1,
            2,
        );
        assert!(
            matches!(
                state.objects[&pw].activated_ability_provenance(0),
                AbilityProvenance::Characteristic(CharacteristicSetRef::Own)
            ),
            "paired control: an uncopied ability is Characteristic(Own)"
        );
        assert!(can_activate_loyalty_ability(&state, pw, PlayerId(0), 0));

        state.objects.get_mut(&pw).unwrap().layer1_copy_effect = Some(CopyEffectInstanceRef {
            continuous_effect_id: 1,
            modification_index: 0,
        });
        assert!(
            matches!(
                state.objects[&pw].activated_ability_provenance(0),
                AbilityProvenance::Characteristic(CharacteristicSetRef::Copied(_))
            ),
            "reach-guard: the ability now comes from a layer-1 copy effect"
        );
        assert!(
            can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
            "the copied characteristic ability keeps the grant (count 1 < cap 3)"
        );
    }

    /// CR 606.3 + CR 611.3a: a `ModifyActivationLimit { keyword: "loyalty" }` static
    /// (Oath of Teferi class) SETS the base cap and every grant ADDS to it: 2 from
    /// the static + 1 player-wide + 2 permanent-scoped = 5. The static is consulted
    /// only once the count reaches the grant-only base (1 + 1 + 2 = 4), so the probes
    /// sit there. Dropping any one of the three terms refuses count 4, and the
    /// control without the static refuses it too.
    #[test]
    fn grants_stack_on_a_loyalty_limit_static() {
        use crate::types::ability::{ControllerRef, StaticDefinition, TypeFilter};
        use crate::types::statics::StaticMode;

        let mut state = setup();
        let pw = walker_with_grant(
            &mut state,
            PlayerId(0),
            vec![make_uncapped_loyalty_ability(1)],
            4,
            2,
        );
        state
            .extra_loyalty_activations_this_turn
            .insert(PlayerId(0), 1);
        assert!(
            !can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
            "paired control: without the static the grants alone cap at 4"
        );

        let limit = create_object(
            &mut state,
            CardId(9_001),
            PlayerId(0),
            "Loyalty Limit".to_string(),
            Zone::Battlefield,
        );
        let limit_obj = state.objects.get_mut(&limit).unwrap();
        limit_obj.card_types.core_types.push(CoreType::Enchantment);
        limit_obj.static_definitions.push(
            StaticDefinition::new(StaticMode::ModifyActivationLimit {
                keyword: "loyalty".to_string(),
                new_limit: 2,
            })
            .affected(TargetFilter::Typed(
                TypedFilter::new(TypeFilter::Planeswalker).controller(ControllerRef::You),
            )),
        );

        assert_eq!(
            loyalty_activation_cap(&state, &state.objects[&pw], PlayerId(0), 0, None),
            5,
            "reach-guard: the static's 2 plus both grants"
        );
        assert!(
            can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
            "count 4 is under a cap of 5"
        );
        state
            .objects
            .get_mut(&pw)
            .unwrap()
            .loyalty_activations_this_turn = 5;
        assert!(
            !can_activate_loyalty_ability(&state, pw, PlayerId(0), 0),
            "count 5 reaches the cap of 5"
        );
    }
}

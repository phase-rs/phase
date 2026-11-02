use crate::types::ability::{
    Effect, EffectError, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::zones::Zone;

/// CR 606.3 + CR 101.1: Grant extra loyalty-ability activations for the
/// remainder of this turn, on one of two recipient axes (`target`):
///
/// - `Controller` (the printed default): the resolved player may activate each
///   planeswalker they control `amount` additional times. Raises the
///   per-planeswalker CR 606.3 cap for every planeswalker the player controls.
///   The Chain Veil: "{4}, {T}: For each planeswalker you control, you may
///   activate one of its loyalty abilities once this turn as though none of its
///   loyalty abilities have been activated this turn." A larger or dynamic
///   `amount` resolves here unchanged via `QuantityExpr::resolve`, once a parser
///   arm produces it.
/// - `SelfRef`: only the resolving permanent may be activated `amount` additional
///   times (Comet, Stellar Pup: "you may activate ~'s loyalty ability two more
///   times this turn"). Stored on the permanent itself and read, for that
///   permanent's own characteristic loyalty abilities, by the same CR 606.3 gate
///   (`planeswalker::loyalty_activation_cap`).
///
/// The "may" in both wordings is the later activation permission (CR 608.2d),
/// not a choice made while this effect resolves, so the grant itself is
/// mandatory.
///
/// CR 400.7 + CR 113.7a: a `SelfRef` grant binds the battlefield permanent the
/// ability's `self_ref_binding` names. When the ability carries a captured source
/// incarnation (an activated ability's stack entry does), a source that left and
/// re-entered after activation is a new object and is not bound. A die-result
/// branch frame of an activated ability inherits the incarnation that activation
/// captured (CR 706.3b), so there too a source that re-entered after the ability
/// was put on the stack is not bound. A frame built outside such a stack entry (a
/// triggered ability's die rows, a coin-flip, vote or reveal branch) carries no
/// captured incarnation, so there a source that re-entered is still bound; that
/// gap is shared with every `SelfRef` resolver and is not specific to this grant.
///
/// CR 514.2: Both bonuses are bound to the current turn; they are cleared in
/// `start_next_turn` alongside other per-turn history. The per-permanent bonus
/// is also cleared when the permanent enters the battlefield (CR 400.7).
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let (amount, target) = match &ability.effect {
        Effect::GrantExtraLoyaltyActivations { amount, target } => (amount, target),
        _ => {
            return Err(EffectError::MissingParam(
                "expected GrantExtraLoyaltyActivations effect".into(),
            ));
        }
    };

    match target {
        // CR 400.7 + CR 606.3: the permanent-scoped grant. This arm precedes the
        // player path so a `SelfRef` recipient is never read through
        // `resolve_player_for_context_ref`, whose controller fallback would
        // silently widen the grant to every planeswalker the player controls.
        TargetFilter::SelfRef => {
            // CR 101.3: with no battlefield permanent to receive the permission
            // the instruction cannot be performed and is ignored, with no event.
            let Some(source) = ability.self_ref_binding(state).filter(|id| {
                state
                    .objects
                    .get(id)
                    .is_some_and(|obj| obj.zone == Zone::Battlefield)
            }) else {
                return Ok(());
            };
            let amount = grant_amount(state, ability, amount);
            if amount > 0 {
                if let Some(obj) = state.objects.get_mut(&source) {
                    obj.extra_loyalty_activations_this_turn = obj
                        .extra_loyalty_activations_this_turn
                        .saturating_add(amount);
                }
            }
        }
        // Every other recipient is a player. The parser emits only `Controller`
        // (the printed default) and `SelfRef`; the player authority resolves
        // whatever else a serialized definition carries.
        _ => {
            // CR 109.5 / CR 113.6: "you" in an effect text resolves to the
            // ability's controller. Cards with `target: Controller` (the printed
            // default) read their grantee directly off `ability.controller`;
            // targeted variants resolve through the one player-reference authority.
            let Some(player) = super::resolve_player_for_context_ref(state, ability, target) else {
                return Ok(());
            };
            let amount = grant_amount(state, ability, amount);
            if amount > 0 {
                *state
                    .extra_loyalty_activations_this_turn
                    .entry(player)
                    .or_insert(0) += amount;
            }
        }
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GrantExtraLoyaltyActivations,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

/// The number of extra activations the effect grants.
fn grant_amount(state: &GameState, ability: &ResolvedAbility, amount: &QuantityExpr) -> u32 {
    let amount = crate::game::quantity::resolve_quantity(
        state,
        amount,
        ability.controller,
        ability.source_id,
    );
    // CR 107.1b: A negative or zero count is a no-op (no cards in the printed
    // class ever go below 1, but the resolver must clamp defensively).
    u32::try_from(amount).unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityKind, QuantityExpr, SiblingCondition, SpellContext, SubAbilityLink, TargetFilter,
    };
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::zones::Zone;

    fn make_ability(amount: QuantityExpr, controller: PlayerId) -> ResolvedAbility {
        ResolvedAbility {
            declares_chosen_group: None,
            reads_chosen_group: None,
            declares_return_result: None,
            reads_return_result: None,
            detached_remainder: crate::types::ability::DetachedRemainder::NoProducer,
            effect: Effect::GrantExtraLoyaltyActivations {
                amount,
                target: TargetFilter::Controller,
            },
            controller,
            original_controller: None,
            scoped_player: None,
            target_chooser: None,
            source_id: ObjectId(1),
            cast_occurrence: None,
            source_incarnation: None,
            announced_source_incarnation: None,
            trigger_source: None,
            trigger_definition_ref: None,
            delayed_origin: None,
            force_block_attacker: None,
            target_incarnations: Vec::new(),
            target_pins: Vec::new(),
            legacy_selected_target_incarnations: Vec::new(),
            chosen_target_slots: Default::default(),
            illegal_target_slots: Vec::new(),
            unjudged_target_slots: Vec::new(),
            illegal_local_target_slots: Vec::new(),
            targets: vec![],
            kind: AbilityKind::Activated,
            sub_ability: None,
            else_ability: None,
            duration: None,
            condition: None,
            context: SpellContext::default(),
            optional_targeting: false,
            optional: false,
            optional_player: None,
            optional_for: None,
            multi_target: None,
            target_constraints: Vec::new(),
            target_choice_timing: crate::types::ability::TargetChoiceTiming::Stack,
            description: None,
            selected_mode_labels: Vec::new(),
            modal_instruction_ordinal: None,
            player_scope: None,
            starting_with: None,
            chosen_x: None,
            cost_paid_object: None,
            noted_mana_payment: None,
            cost_paid_objects: Vec::new(),
            effect_context_object: None,
            amassed_army_object: None,
            ability_index: None,
            may_trigger_origin: None,
            repeat_for: None,
            min_x_value: 0,
            announced_x: None,
            cant_be_copied: false,
            illegal_targets_disposition: Default::default(),
            copy_count_status: crate::types::ability::CopyCountStatus::Pending,
            forward_result: false,
            unless_pay: None,
            distribution: None,
            distribute: None,
            target_selection_mode: crate::types::ability::TargetSelectionMode::Chosen,
            chosen_players: Vec::new(),
            repeat_until: None,
            replacement_applied: Default::default(),
            sub_link: SubAbilityLink::ContinuationStep,
            target_reads: Default::default(),
            entry_onset: crate::types::ability::ContinuousEffectOnset::WhenFollowed,
            sibling_condition: SiblingCondition::Dependent,
            modal: None,
            mode_abilities: vec![],
            parent_target_missing_reason: None,
            activation_cost_reduction: None,
            activation_record: None,
            control_change_instruction: None,
        }
    }

    #[test]
    fn grants_one_extra_loyalty_activation_to_controller() {
        let mut state = GameState::new_two_player(0);
        let ability = make_ability(QuantityExpr::Fixed { value: 1 }, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state
                .extra_loyalty_activations_this_turn
                .get(&PlayerId(0))
                .copied(),
            Some(1)
        );
    }

    /// CR 606.3: Three activations of The Chain Veil grant a cumulative +3 per
    /// planeswalker for the rest of the turn.
    #[test]
    fn stacks_across_multiple_activations() {
        let mut state = GameState::new_two_player(0);
        let ability = make_ability(QuantityExpr::Fixed { value: 1 }, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state
                .extra_loyalty_activations_this_turn
                .get(&PlayerId(0))
                .copied(),
            Some(3),
        );
    }

    /// CR 107.1b: A zero amount is a no-op — the map stays empty (so the
    /// cap-raise predicate short-circuits cleanly).
    #[test]
    fn zero_amount_is_no_op() {
        let mut state = GameState::new_two_player(0);
        let ability = make_ability(QuantityExpr::Fixed { value: 0 }, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(state.extra_loyalty_activations_this_turn.is_empty());
    }

    fn add_walker(state: &mut GameState, owner: PlayerId, zone: Zone) -> ObjectId {
        let card_id = CardId(state.next_object_id);
        create_object(state, card_id, owner, "Probe Walker".to_string(), zone)
    }

    /// A `SelfRef` grant of `amount` resolving from `source`.
    fn make_source_ability(amount: i32, source: ObjectId, controller: PlayerId) -> ResolvedAbility {
        let mut ability = make_ability(QuantityExpr::Fixed { value: amount }, controller);
        ability.source_id = source;
        ability.effect = Effect::GrantExtraLoyaltyActivations {
            amount: QuantityExpr::Fixed { value: amount },
            target: TargetFilter::SelfRef,
        };
        ability
    }

    fn grant_events(events: &[GameEvent]) -> usize {
        events
            .iter()
            .filter(|event| {
                matches!(
                    event,
                    GameEvent::EffectResolved {
                        kind: EffectKind::GrantExtraLoyaltyActivations,
                        ..
                    }
                )
            })
            .count()
    }

    /// CR 606.3: a `SelfRef` grant raises only the source permanent's cap. A
    /// second own planeswalker and an opponent's planeswalker, both on the
    /// battlefield, keep 0, and the player-wide map stays empty (the resolver's
    /// controller fallback must not widen it). The source's 2 is the positive
    /// reach-guard that the `SelfRef` arm ran.
    #[test]
    fn source_grant_raises_only_the_source_permanent() {
        let mut state = GameState::new_two_player(0);
        let source = add_walker(&mut state, PlayerId(0), Zone::Battlefield);
        let other_own = add_walker(&mut state, PlayerId(0), Zone::Battlefield);
        let opposing = add_walker(&mut state, PlayerId(1), Zone::Battlefield);
        let ability = make_source_ability(2, source, PlayerId(0));
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(
            state.objects[&source].extra_loyalty_activations_this_turn,
            2
        );
        assert_eq!(
            state.objects[&other_own].extra_loyalty_activations_this_turn,
            0
        );
        assert_eq!(
            state.objects[&opposing].extra_loyalty_activations_this_turn,
            0
        );
        assert!(state.extra_loyalty_activations_this_turn.is_empty());
        assert_eq!(grant_events(&events), 1);
    }

    /// Two resolutions stack (4), and the `Controller` axis still writes the map
    /// and leaves the object field at 0.
    #[test]
    fn source_grants_stack_and_controller_grant_leaves_the_object_alone() {
        let mut state = GameState::new_two_player(0);
        let source = add_walker(&mut state, PlayerId(0), Zone::Battlefield);
        let ability = make_source_ability(2, source, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert_eq!(
            state.objects[&source].extra_loyalty_activations_this_turn,
            4
        );

        let mut controller_grant = make_ability(QuantityExpr::Fixed { value: 1 }, PlayerId(0));
        controller_grant.source_id = source;
        resolve(&mut state, &controller_grant, &mut events).unwrap();
        assert_eq!(
            state
                .extra_loyalty_activations_this_turn
                .get(&PlayerId(0))
                .copied(),
            Some(1)
        );
        assert_eq!(
            state.objects[&source].extra_loyalty_activations_this_turn, 4,
            "the Controller axis does not write the permanent's field"
        );
    }

    /// CR 107.1b: a zero `SelfRef` amount is a no-op; the paired amount of 1
    /// writes 1, proving the arm is reachable.
    #[test]
    fn source_grant_of_zero_is_a_no_op() {
        let mut state = GameState::new_two_player(0);
        let source = add_walker(&mut state, PlayerId(0), Zone::Battlefield);
        let mut events = Vec::new();

        resolve(
            &mut state,
            &make_source_ability(1, source, PlayerId(0)),
            &mut events,
        )
        .unwrap();
        assert_eq!(
            state.objects[&source].extra_loyalty_activations_this_turn,
            1
        );

        let mut fresh = GameState::new_two_player(0);
        let source = add_walker(&mut fresh, PlayerId(0), Zone::Battlefield);
        resolve(
            &mut fresh,
            &make_source_ability(0, source, PlayerId(0)),
            &mut events,
        )
        .unwrap();
        assert_eq!(
            fresh.objects[&source].extra_loyalty_activations_this_turn,
            0
        );
    }

    /// CR 101.3: a source that is not on the battlefield cannot receive the
    /// permission; the instruction is ignored with no event. The paired
    /// battlefield source applies.
    #[test]
    fn source_grant_without_a_battlefield_source_is_ignored() {
        let mut state = GameState::new_two_player(0);
        let gone = add_walker(&mut state, PlayerId(0), Zone::Graveyard);
        let mut events = Vec::new();
        resolve(
            &mut state,
            &make_source_ability(2, gone, PlayerId(0)),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.objects[&gone].extra_loyalty_activations_this_turn, 0);
        assert_eq!(grant_events(&events), 0);
        assert!(state.extra_loyalty_activations_this_turn.is_empty());

        let live = add_walker(&mut state, PlayerId(0), Zone::Battlefield);
        resolve(
            &mut state,
            &make_source_ability(2, live, PlayerId(0)),
            &mut events,
        )
        .unwrap();
        assert_eq!(state.objects[&live].extra_loyalty_activations_this_turn, 2);
        assert_eq!(grant_events(&events), 1);
    }

    /// CR 400.7 + CR 608.2h: an ability whose captured source incarnation is
    /// stale (the source left and returned after activation) does not bind the
    /// new object. This proves the resolver honors the stamp it is handed through
    /// `self_ref_binding`; it does not prove any production frame supplies one
    /// (the integration suite covers the activated stack entry). The paired
    /// current-incarnation ability applies.
    #[test]
    fn source_grant_with_a_stale_incarnation_does_not_bind_the_new_object() {
        let mut state = GameState::new_two_player(0);
        let source = add_walker(&mut state, PlayerId(0), Zone::Battlefield);
        let captured = state.objects[&source].incarnation;
        let mut events = Vec::new();

        let mut current = make_source_ability(2, source, PlayerId(0));
        current.source_incarnation = Some(captured);
        resolve(&mut state, &current, &mut events).unwrap();
        assert_eq!(
            state.objects[&source].extra_loyalty_activations_this_turn,
            2
        );

        // The blink: the same storage id is now a new incarnation.
        let obj = state.objects.get_mut(&source).unwrap();
        obj.extra_loyalty_activations_this_turn = 0;
        obj.bump_incarnation();
        let mut stale = make_source_ability(2, source, PlayerId(0));
        stale.source_incarnation = Some(captured);
        resolve(&mut state, &stale, &mut events).unwrap();
        assert_eq!(
            state.objects[&source].extra_loyalty_activations_this_turn, 0,
            "the stale ability must not bind the re-entered object"
        );
    }
}

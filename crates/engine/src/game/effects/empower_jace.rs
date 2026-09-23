use crate::game::effects::counters::{
    add_counter_with_replacement, append_pending_counter_post_actions,
    stash_pending_counter_completion, stash_pending_counter_post_actions,
};
use crate::game::effects::token::apply_create_token_after_replacement;
use crate::game::filter::{matches_target_filter, FilterContext};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{
    ControllerRef, Effect, EffectError, EffectKind, FilterProp, ResolvedAbility, TargetFilter,
    TypeFilter, TypedFilter,
};
use crate::types::card_type::CoreType;
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, PendingCounterPostAction, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::mana::ManaColor;
use crate::types::player::PlayerId;
use crate::types::proposed_event::{
    EtbTapState, ProposedEvent, TokenCharacteristics, TokenHostRequest, TokenSpec,
};
use std::collections::HashSet;

/// CR 701.71a: Empower Jace N.
///
/// "If you don't control a Jace planeswalker token, create a blue Jace
/// planeswalker token with 0 loyalty, '[−1]: Surveil 1,' and '[−3]: Draw a
/// card.' Choose a Jace planeswalker token you control. Put N loyalty counters
/// on it."
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let Effect::EmpowerJace { count } = &ability.effect else {
        return Ok(());
    };
    // CR 608.2h: N is determined once, before the token-creation step, so a
    // later pause cannot change it.
    let n = resolve_quantity_with_targets(state, count, ability).max(0) as u32;
    let controller = ability.controller;
    let source_id = ability.source_id;

    if !jace_token_candidates(state, controller, source_id).is_empty() {
        continue_after_creation(state, controller, source_id, n, events);
        return Ok(());
    }

    // CR 701.71a + CR 111.2 + CR 614.1: the controller creates the Jace token,
    // and token-creation replacement effects apply to that event.
    let proposed = ProposedEvent::CreateToken {
        owner: controller,
        spec: Box::new(jace_token_spec(controller, source_id)),
        copy: None,
        enter_tapped: EtbTapState::Unspecified,
        count: 1,
        applied: HashSet::new(),
    };
    let owed = || PendingCounterPostAction::ContinueEmpowerJaceAfterTokenCreation {
        controller,
        source_id,
        count: n,
    };
    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            if apply_create_token_after_replacement(state, event, events) {
                continue_after_creation(state, controller, source_id, n, events);
            } else {
                // CR 614.1c + CR 122.6: the token's entry paused on an
                // entry-counter replacement choice; choose and place only after
                // the entry settles.
                append_pending_counter_post_actions(state, vec![owed()]);
            }
        }
        // CR 701.71a: no fallback — with no Jace token to choose, nothing
        // further happens.
        ReplacementResult::Prevented => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::EmpowerJace,
                source_id,
                subject: None,
            });
        }
        ReplacementResult::NeedsChoice(player) => {
            stash_pending_counter_post_actions(
                state,
                EffectKind::EmpowerJace,
                source_id,
                vec![owed()],
            );
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
        }
    }
    Ok(())
}

/// CR 701.71a: the blue Jace planeswalker token with 0 loyalty. Its two
/// loyalty abilities come from the predefined subtype-keyed registry when the
/// token enters (`predefined_token_abilities("Jace")`).
fn jace_token_spec(controller: PlayerId, source_id: ObjectId) -> TokenSpec {
    TokenSpec {
        characteristics: TokenCharacteristics {
            display_name: "Jace".to_string(),
            power: None,
            toughness: None,
            loyalty: Some(0),
            core_types: vec![CoreType::Planeswalker],
            subtypes: vec!["Jace".to_string()],
            supertypes: vec![],
            colors: vec![ManaColor::Blue],
            keywords: vec![],
        },
        script_name: "Jace".to_string(),
        static_abilities: vec![],
        enter_with_counters: vec![],
        tapped: false,
        enters_attacking: false,
        attach_to: TokenHostRequest::NotRequested,
        sacrifice_at: None,
        source_id,
        controller,
    }
}

/// CR 701.71a: "a Jace planeswalker token you control" — tokens only
/// (CR 111.1), so a card-backed Jace planeswalker never satisfies the existence
/// test and is never a legal choice. Single predicate for the existence test,
/// the choice population and the choice handler's membership check.
pub(crate) fn jace_token_candidates(
    state: &GameState,
    controller: PlayerId,
    source_id: ObjectId,
) -> Vec<ObjectId> {
    let filter = TargetFilter::Typed(
        TypedFilter::new(TypeFilter::Planeswalker)
            .subtype("Jace".to_string())
            .controller(ControllerRef::You)
            .properties(vec![FilterProp::Token]),
    );
    let ctx = FilterContext::from_source_with_controller(source_id, controller);
    state
        .battlefield
        .iter()
        .copied()
        .filter(|&id| matches_target_filter(state, id, &filter, &ctx))
        .collect()
}

/// CR 701.71a: the choose-and-place tail, reached once any Jace token exists or
/// creation has settled. Returns `true` when the instruction finished (its
/// `EffectResolved` emitted) and `false` when resolution paused on a
/// `WaitingFor` that a later action settles.
pub(crate) fn continue_after_creation(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    let choices = jace_token_candidates(state, controller, source_id);
    match choices.as_slice() {
        [] => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::EmpowerJace,
                source_id,
                subject: None,
            });
            true
        }
        [only] => place_loyalty_counters(state, controller, source_id, *only, count, events),
        _ => {
            // CR 608.2d: the controller chooses while applying the effect.
            state.waiting_for = WaitingFor::EmpowerJaceChoice {
                player: controller,
                source_id,
                choices,
                count,
            };
            false
        }
    }
}

/// CR 701.71a + CR 122.1 + CR 122.6: put N loyalty counters on the chosen token
/// through the counter-replacement pipeline. One placement event of N counters,
/// so a "whenever you put one or more loyalty counters" observer triggers once
/// (CR 603.2c).
///
/// Returns `false` when placement paused on a replacement choice; the
/// EmpowerJace completion is stashed first, so the counter drain emits
/// `EffectResolved` once the choice settles.
pub(crate) fn place_loyalty_counters(
    state: &mut GameState,
    controller: PlayerId,
    source_id: ObjectId,
    target: ObjectId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    if !add_counter_with_replacement(
        state,
        controller,
        target,
        CounterType::Loyalty,
        count,
        events,
    ) {
        stash_pending_counter_completion(state, EffectKind::EmpowerJace, source_id);
        return false;
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::EmpowerJace,
        source_id,
        subject: None,
    });
    true
}

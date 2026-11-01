use std::collections::HashSet;

use super::life::{self, LifeChangeUnrepresentable};
use crate::game::quantity::resolve_quantity_with_targets;
use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{Effect, EffectError, EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PendingCounterAddition, PendingEffectResolved, UnrepresentablePolicy,
};
use crate::types::player::PlayerId;
use crate::types::proposed_event::{CounterPlacement, ProposedEvent};
use crate::types::resolved_commands::ResolvedPlayerEdit;

/// Adds energy through the replacement pipeline under the compatibility
/// (`IgnoreImpossible`) policy.
///
/// An engine-unrepresentable final count is not a CR 101.3 impossibility. With
/// an open ingress window the owning public action rejects and restores its
/// entry state, and outside one the logged compatibility skip applies.
pub(crate) fn add_energy_with_replacement(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> bool {
    life::ordinary_settlement(
        add_energy_core(
            state,
            actor,
            player_id,
            amount,
            UnrepresentablePolicy::IgnoreImpossible,
            events,
        ),
        true,
        "ordinary energy gain",
    )
}

/// CR 122.1 + CR 107.14: Action-ingress energy gain. Like
/// [`add_energy_with_replacement`], except that a final result the engine
/// cannot represent is returned as `Err`, so the caller rejects the action and
/// the action boundary restores its snapshot. The fresh ingress window covers
/// the whole call: a park made here is stamped for rejection on resume
/// (CR 616.1). `actor` is the player who puts the counters. `Ok(true)` means
/// the gain settled (applied or prevented) and `Ok(false)` means a choice is
/// parked.
pub(crate) fn try_add_energy_with_replacement(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> Result<bool, LifeChangeUnrepresentable> {
    life::within_rejecting_ingress_window(state, |state, policy| {
        add_energy_core(state, actor, player_id, amount, policy, events)
    })
}

/// The policy-parameterized energy add pipeline: replacement, then the shared
/// settle step (`apply_energy_addition`). `Ok(false)` means the replacement
/// pipeline parked a choice.
fn add_energy_core(
    state: &mut GameState,
    actor: PlayerId,
    player_id: PlayerId,
    amount: u32,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<bool, LifeChangeUnrepresentable> {
    if amount == 0 {
        return Ok(true);
    }

    // CR 122.1 + CR 107.14 + CR 614.17: Energy is a counter a player has, so
    // gaining energy passes through the counter-placement pipeline before the
    // dedicated player energy field is mutated.
    let proposed = ProposedEvent::AddCounter {
        placement: CounterPlacement::Energy { actor, player_id },
        count: amount,
        applied: HashSet::new(),
    };

    match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(ProposedEvent::AddCounter {
            placement: CounterPlacement::Energy { player_id, .. },
            count,
            ..
        }) => {
            apply_energy_addition(state, player_id, count, policy, events)?;
            Ok(true)
        }
        ReplacementResult::Execute(_) | ReplacementResult::Prevented => Ok(true),
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for = replacement::replacement_choice_waiting_for(player, state);
            Ok(false)
        }
    }
}

/// CR 122.1 + CR 107.14: Settle a post-replacement energy gain: the one place
/// the final edit is made and its representability decided, for the add path
/// and the CR 616.1 resume alike. Same order as `life::settle_life_gain`: a
/// recorded replacement-quantity overflow, then an already-failed ingress
/// window, then the amount must fit an `i32` delta, then the checked edit. An
/// unrepresentable result is an engine numeric limit, not a CR 101.3 /
/// CR 609.3 impossibility, and is handled per `policy`
/// (`life::settle_unrepresentable`).
pub(crate) fn apply_energy_addition(
    state: &mut GameState,
    player_id: PlayerId,
    amount: u32,
    policy: UnrepresentablePolicy,
    events: &mut Vec<GameEvent>,
) -> Result<(), LifeChangeUnrepresentable> {
    if let Some(settled) = life::settle_quantity_overflow(state, player_id, policy, "energy gain") {
        return settled.map(|_| ());
    }
    if life::ingress_window_has_failed(state) || amount == 0 {
        return Ok(());
    }
    let edit = i32::try_from(amount)
        .map_err(|_| LifeChangeUnrepresentable { player: player_id })
        .and_then(|delta| {
            life::apply_final_player_resource_edit(
                state,
                player_id,
                ResolvedPlayerEdit::Energy { delta },
            )
            .map(|()| delta)
        });
    let delta = match edit {
        Ok(delta) => delta,
        Err(unrepresentable) => {
            return life::settle_unrepresentable(state, unrepresentable, policy, "energy gain")
                .map(|_| ());
        }
    };

    // CR 122.1 + CR 107.14: Energy counters are counters placed on a player.
    events.push(GameEvent::EnergyChanged {
        player: player_id,
        delta,
    });
    Ok(())
}

/// CR 122.1: Gain energy counters. Increments the controller's energy pool.
pub fn resolve_gain(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let amount = match &ability.effect {
        Effect::GainEnergy { amount } => resolve_quantity_with_targets(state, amount, ability),
        _ => return Err(EffectError::MissingParam("amount".to_string())),
    };
    let amount = amount.max(0) as u32;

    if !add_energy_with_replacement(
        state,
        ability.controller,
        ability.controller,
        amount,
        events,
    ) {
        super::counters::stash_pending_counter_additions(
            state,
            Vec::<PendingCounterAddition>::new(),
            PendingEffectResolved::new(EffectKind::GainEnergy, ability.source_id),
        );
        return Ok(());
    }
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::GainEnergy,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones;
    use crate::types::ability::{
        ControllerRef, QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition,
        ReplacementPlayerScope, TargetFilter, TypedFilter,
    };
    use crate::types::card_type::CoreType;
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::zones::Zone;

    fn gain_energy_ability(amount: QuantityExpr) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::GainEnergy { amount },
            vec![],
            ObjectId(100),
            PlayerId(0),
        )
    }

    #[test]
    fn gain_energy_resolves_fixed_amount() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();
        let ability = gain_energy_ability(QuantityExpr::Fixed { value: 2 });

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 2);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EnergyChanged {
                player: PlayerId(0),
                delta: 2,
            }
        )));
    }

    #[test]
    fn gain_energy_resolves_dynamic_object_count() {
        let mut state = GameState::new_two_player(42);
        for idx in 0..2 {
            let id = zones::create_object(
                &mut state,
                CardId(idx),
                PlayerId(0),
                "Creature".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&id)
                .unwrap()
                .card_types
                .core_types
                .push(CoreType::Creature);
        }
        let opponent_id = zones::create_object(
            &mut state,
            CardId(99),
            PlayerId(1),
            "Opponent Creature".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&opponent_id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);

        let ability = gain_energy_ability(QuantityExpr::Ref {
            qty: QuantityRef::ObjectCount {
                filter: TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::You)),
            },
        });
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 2);
    }

    #[test]
    fn gain_energy_is_prevented_by_player_counter_prohibition() {
        let mut state = GameState::new_two_player(42);
        let source = zones::create_object(
            &mut state,
            CardId(88),
            PlayerId(1),
            "Solemnity".to_string(),
            Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Prevent);
        replacement.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        let ability = gain_energy_ability(QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::EnergyChanged { .. })),
            "prevented energy-counter additions must not emit energy-change events"
        );
    }

    #[test]
    fn gain_energy_count_is_increased_by_controller_scoped_replacement() {
        let mut state = GameState::new_two_player(42);
        let source = zones::create_object(
            &mut state,
            CardId(88),
            PlayerId(0),
            "Izzet Generatorium".to_string(),
            Zone::Battlefield,
        );
        let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
            .quantity_modification(QuantityModification::Plus { value: 1 });
        replacement.valid_player = Some(ReplacementPlayerScope::You);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![replacement].into();

        let ability = gain_energy_ability(QuantityExpr::Fixed { value: 2 });
        let mut events = Vec::new();

        resolve_gain(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].energy, 3);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::EnergyChanged {
                player: PlayerId(0),
                delta: 3,
            }
        )));
    }

    /// CR 122.1 + CR 107.14: the no-window compatibility path. A count
    /// replacement past `u32` is neither clamped nor wrapped through an `i32`
    /// delta: the gain is skipped, nothing is mutated or emitted, and the
    /// overflow record is consumed by the settle step.
    #[test]
    fn unrepresentable_replaced_energy_is_skipped_without_a_window() {
        let add_source = |state: &mut GameState, modification: QuantityModification| {
            let source = zones::create_object(
                state,
                CardId(88),
                PlayerId(0),
                "Counter Scaler".to_string(),
                Zone::Battlefield,
            );
            let mut replacement = ReplacementDefinition::new(ReplacementEvent::AddCounter)
                .quantity_modification(modification);
            replacement.valid_player = Some(ReplacementPlayerScope::AnyPlayer);
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions = vec![replacement].into();
        };

        let mut state = GameState::new_two_player(42);
        add_source(&mut state, QuantityModification::Plus { value: u32::MAX });
        let mut events = Vec::new();

        let completed =
            add_energy_with_replacement(&mut state, PlayerId(0), PlayerId(0), 1, &mut events);

        assert!(completed);
        assert_eq!(state.players[0].energy, 0);
        assert!(!events
            .iter()
            .any(|event| matches!(event, GameEvent::EnergyChanged { .. })));
        assert!(
            state.replacement_quantity_overflow.is_none(),
            "the settle step must consume the overflow record"
        );

        // Control: a representable `Plus` applies through the same path.
        let mut state = GameState::new_two_player(42);
        add_source(&mut state, QuantityModification::Plus { value: 1 });
        let mut events = Vec::new();
        add_energy_with_replacement(&mut state, PlayerId(0), PlayerId(0), 1, &mut events);
        assert_eq!(state.players[0].energy, 2);
    }
}

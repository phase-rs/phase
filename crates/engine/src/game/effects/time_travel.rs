//! CR 701.56a: Time travel — "choose any number of permanents you control with
//! one or more time counters on them and/or suspended cards you own in exile
//! with one or more time counters on them and, for each of those objects, put a
//! time counter on it or remove a time counter from it."
//!
//! Modeled like Proliferate (`WaitingFor::ProliferateChoice`), but with a
//! per-object add/remove decision. To reuse `GameAction::SelectTargets` without
//! a new action variant, the choice runs in two phases over
//! `WaitingFor::TimeTravelChoice`. `TimeTravelPhase::Remove` is offered first:
//! the player selects the objects to remove a time counter from (the common
//! case — advancing suspended cards toward casting). `TimeTravelPhase::Add`
//! then runs over the still-eligible remainder, selecting the
//! objects to add a time counter to. An object selected in the remove phase is
//! excluded from the add phase, so each object gets at most one of add/remove
//! (CR 701.56a "for each of those objects, put ... or remove ...").
//!
//! All consequences of the counter changes — the suspend last-counter free-cast
//! trigger and Vanishing's last-counter sacrifice — are handled by the existing
//! `CounterAdded`/`CounterRemoved` triggers; this resolver only drives the
//! counter primitives.

use crate::types::ability::{EffectError, EffectKind, ResolvedAbility, TargetRef};
use crate::types::counter::CounterType;
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, TimeTravelPhase, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

fn time_counters(state: &GameState, id: ObjectId) -> u32 {
    state
        .objects
        .get(&id)
        .and_then(|o| o.counters.get(&CounterType::Time).copied())
        .unwrap_or(0)
}

/// CR 701.56a: the time-travel-eligible objects for `player` — permanents they
/// control with a time counter, and suspended cards they own in exile with a
/// time counter (CR 702.62b: a suspended card is in exile with time counters).
/// Sorted by id for deterministic ordering.
pub(crate) fn eligible_objects(state: &GameState, player: PlayerId) -> Vec<TargetRef> {
    let mut eligible: Vec<ObjectId> = state
        .objects
        .iter()
        .filter(|(_, obj)| {
            obj.counters.get(&CounterType::Time).copied().unwrap_or(0) > 0
                && ((obj.zone == Zone::Battlefield && obj.controller == player)
                    || (obj.zone == Zone::Exile && obj.owner == player))
        })
        .map(|(id, _)| *id)
        .collect();
    eligible.sort_by_key(|id| id.0);
    eligible.into_iter().map(TargetRef::Object).collect()
}

/// CR 701.56a: resolve a time-travel instruction. Offers the eligible objects
/// for the remove phase; if none are eligible, time travel does nothing.
pub(crate) fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let player = ability.controller;
    let eligible = eligible_objects(state, player);
    if eligible.is_empty() {
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::TimeTravel,
            source_id: ability.source_id,
        });
        return Ok(());
    }
    state.waiting_for = WaitingFor::TimeTravelChoice {
        player,
        eligible,
        phase: TimeTravelPhase::Remove,
    };
    Ok(())
}

/// Apply one phase: put or remove a time counter on each selected object. The
/// resulting counter events drive the existing suspend/vanishing triggers
/// (free-cast on last removal, sacrifice, etc.).
pub(crate) fn apply_phase(
    state: &mut GameState,
    player: PlayerId,
    selected: &[TargetRef],
    phase: TimeTravelPhase,
    events: &mut Vec<GameEvent>,
) {
    for target in selected {
        let TargetRef::Object(id) = target else {
            continue;
        };
        // Re-validate: an object may have left its zone between phases (e.g. a
        // suspended card whose last time counter was just removed and was cast).
        match phase {
            TimeTravelPhase::Remove => {
                if time_counters(state, *id) == 0 {
                    continue;
                }
                super::counters::apply_counter_removal(state, *id, CounterType::Time, 1, events);
            }
            TimeTravelPhase::Add => {
                super::counters::apply_counter_addition(
                    state,
                    player,
                    *id,
                    CounterType::Time,
                    1,
                    events,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::Effect;
    use crate::types::identifiers::CardId;

    fn with_time(state: &mut GameState, id: ObjectId, n: u32) {
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .counters
            .insert(CounterType::Time, n);
    }

    /// CR 701.56a: eligible = controller's permanents with a time counter +
    /// controller's suspended exile cards with a time counter; not opponents',
    /// not objects without time counters.
    #[test]
    fn eligible_covers_controlled_perm_and_owned_exile_only() {
        let mut state = GameState::new_two_player(1);
        let perm = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vanisher".into(),
            Zone::Battlefield,
        );
        with_time(&mut state, perm, 2);
        let suspended = create_object(
            &mut state,
            CardId(2),
            PlayerId(0),
            "Suspended".into(),
            Zone::Exile,
        );
        with_time(&mut state, suspended, 3);
        let opp = create_object(
            &mut state,
            CardId(3),
            PlayerId(1),
            "Opp Suspended".into(),
            Zone::Exile,
        );
        with_time(&mut state, opp, 1);
        let plain = create_object(
            &mut state,
            CardId(4),
            PlayerId(0),
            "Plain".into(),
            Zone::Battlefield,
        );

        let eligible = eligible_objects(&state, PlayerId(0));
        assert!(eligible.contains(&TargetRef::Object(perm)));
        assert!(eligible.contains(&TargetRef::Object(suspended)));
        assert!(
            !eligible.contains(&TargetRef::Object(opp)),
            "an opponent's suspended card is not yours to time travel"
        );
        assert!(
            !eligible.contains(&TargetRef::Object(plain)),
            "an object without a time counter is not eligible"
        );
    }

    /// CR 701.56a: resolve offers the remove phase first; apply adjusts the counter.
    #[test]
    fn resolve_sets_remove_phase_and_apply_adjusts_counter() {
        let mut state = GameState::new_two_player(1);
        let perm = create_object(
            &mut state,
            CardId(1),
            PlayerId(0),
            "Vanisher".into(),
            Zone::Battlefield,
        );
        with_time(&mut state, perm, 2);

        let ability = ResolvedAbility::new(Effect::TimeTravel, vec![], perm, PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        match &state.waiting_for {
            WaitingFor::TimeTravelChoice {
                phase, eligible, ..
            } => {
                assert_eq!(
                    *phase,
                    TimeTravelPhase::Remove,
                    "remove phase is offered first"
                );
                assert!(eligible.contains(&TargetRef::Object(perm)));
            }
            other => panic!("expected TimeTravelChoice, got {other:?}"),
        }

        apply_phase(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(perm)],
            TimeTravelPhase::Remove,
            &mut events,
        );
        assert_eq!(time_counters(&state, perm), 1, "remove decrements 2 -> 1");
        apply_phase(
            &mut state,
            PlayerId(0),
            &[TargetRef::Object(perm)],
            TimeTravelPhase::Add,
            &mut events,
        );
        assert_eq!(time_counters(&state, perm), 2, "add increments 1 -> 2");
    }

    /// CR 701.56a: with no eligible objects, time travel does nothing (no prompt).
    #[test]
    fn no_eligible_is_noop() {
        let mut state = GameState::new_two_player(1);
        let ability = ResolvedAbility::new(Effect::TimeTravel, vec![], ObjectId(99), PlayerId(0));
        let mut events = Vec::new();
        resolve(&mut state, &ability, &mut events).unwrap();
        assert!(!matches!(
            state.waiting_for,
            WaitingFor::TimeTravelChoice { .. }
        ));
    }
}

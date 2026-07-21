use std::collections::HashSet;

use crate::game::replacement::{self, ReplacementResult};
use crate::types::ability::{EffectError, EffectKind, ResolvedAbility};
use crate::types::card_type::CoreType;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerCounterKind;
use crate::types::proposed_event::ProposedEvent;
use crate::types::zones::Zone;

/// CR 728.1: Resolve the inherent rad counter triggered ability.
/// Mill cards equal to rad counter count, then for each nonland card milled,
/// lose 1 life and remove one rad counter.
pub fn resolve(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    let player_id = ability.controller;

    // CR 728.1 intervening-if re-check: if rad counters were removed
    // between trigger creation and resolution, do nothing.
    let rad_count = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .map(|p| p.player_counter(&PlayerCounterKind::Rad))
        .unwrap_or(0);

    if rad_count == 0 {
        return Ok(());
    }

    // Snapshot library before mill to determine which cards were milled.
    let library_before: Vec<_> = state
        .players
        .iter()
        .find(|p| p.id == player_id)
        .map(|p| p.library.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default();

    // CR 728.1: Mill cards equal to rad counter count.
    // Route through the replacement pipeline for compatibility with
    // replacement effects that modify mill (e.g., "if you would mill, exile instead").
    let proposed = ProposedEvent::Mill {
        player_id,
        count: rad_count,
        destination: Zone::Graveyard,
        applied: Default::default(),
    };

    let milled_ids = match replacement::replace_event(state, proposed, events) {
        ReplacementResult::Execute(event) => {
            // Determine milled_ids from the post-replacement count so
            // replacement effects that reduce the count are respected.
            let final_count = match &event {
                ProposedEvent::Mill { count, .. } => (*count as usize).min(library_before.len()),
                _ => 0,
            };
            // CR 728.1: `milled_ids` is fixed by the post-replacement count
            // against the pre-mill library snapshot (`library_before`), so it
            // identifies the correct top-N cards regardless of where delivery
            // ultimately routes them. The life-loss loop below reads each card's
            // type from `state.objects` (zone-independent).
            let ids: Vec<_> = library_before.into_iter().take(final_count).collect();
            // CR 616.1: a per-card Moved ordering choice parks the prompt
            // (`state.waiting_for` set, `pending_replacement` holding the
            // paused card's move, tail in the active BatchDelivery frame). Bail
            // like `mill::resolve` does: continuing into the life-loss loop
            // would propose `LifeLoss` replacement events while the parked
            // choice is pending and could overwrite `pending_replacement`,
            // ghosting the paused milled card. The CR 728.1 life-loss /
            // rad-removal tail is dropped on this parked path — reachable only
            // with two simultaneous graveyard redirects active while rad
            // counters trigger; accepted and documented rather than threaded
            // through the batch resume.
            if !crate::game::effects::mill::apply_mill_after_replacement(state, event, events)? {
                return Ok(());
            }
            ids
        }
        ReplacementResult::Prevented => {
            events.push(GameEvent::EffectResolved {
                kind: EffectKind::ProcessRadCounters,
                source_id: ability.source_id,
                subject: None,
            });
            return Ok(());
        }
        ReplacementResult::NeedsChoice(player) => {
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(player, state);
            return Ok(());
        }
    };

    // CR 728.1: For each nonland card milled this way, lose 1 life and
    // remove one rad counter.
    let mut nonland_count = 0u32;
    for obj_id in &milled_ids {
        if let Some(obj) = state.objects.get(obj_id) {
            if !obj.card_types.core_types.contains(&CoreType::Land) {
                nonland_count += 1;
            }
        }
    }

    if nonland_count > 0 {
        // CR 728.1: Life loss routed through the replacement pipeline for
        // compatibility with "can't lose life" and life-loss replacement effects.
        if !crate::game::static_abilities::player_has_cant_lose_life(state, player_id) {
            let proposed = ProposedEvent::LifeLoss {
                player_id,
                amount: nonland_count,
                applied: HashSet::new(),
            };

            match replacement::replace_event(state, proposed, events) {
                ReplacementResult::Execute(event) => {
                    super::life::apply_life_loss_after_replacement(state, event, events);
                }
                ReplacementResult::Prevented => {}
                ReplacementResult::NeedsChoice(player) => {
                    state.waiting_for =
                        crate::game::replacement::replacement_choice_waiting_for(player, state);
                    // Counter removal still happens below — life loss is
                    // independent from counter removal per CR 728.1.
                }
            }
        }

        // CR 728.1: Remove one rad counter per nonland card milled.
        // This happens regardless of whether life loss was prevented or
        // deferred to a replacement choice.
        let player = state
            .players
            .iter_mut()
            .find(|p| p.id == player_id)
            .ok_or(EffectError::PlayerNotFound)?;
        let current_rad = player.player_counter(&PlayerCounterKind::Rad);
        let to_remove = nonland_count.min(current_rad);
        player.remove_player_counters(&PlayerCounterKind::Rad, to_remove);

        events.push(GameEvent::PlayerCounterChanged {
            player: player_id,
            counter_kind: PlayerCounterKind::Rad,
            delta: -(to_remove as i32),
        });
    }

    events.push(GameEvent::EffectResolved {
        kind: EffectKind::ProcessRadCounters,
        source_id: ability.source_id,
        subject: None,
    });

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{Effect, ResolvedAbility};
    use crate::types::identifiers::{CardId, ObjectId};
    use crate::types::player::PlayerId;

    fn setup_state_with_rad(rad_count: u32, library_cards: Vec<bool>) -> GameState {
        let mut state = GameState::default();
        let player_id = PlayerId(0);
        state.players[0].id = player_id;
        state.players[0].life = 20;
        state.players[0].add_player_counters(&PlayerCounterKind::Rad, rad_count);

        for (i, is_land) in library_cards.iter().enumerate() {
            let card_id = CardId(100 + i as u64);
            let obj_id =
                create_object(&mut state, card_id, player_id, String::new(), Zone::Library);
            let obj = state.objects.get_mut(&obj_id).unwrap();
            if *is_land {
                obj.card_types.core_types.push(CoreType::Land);
            } else {
                obj.card_types.core_types.push(CoreType::Creature);
            }
        }

        state
    }

    fn make_ability() -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::ProcessRadCounters,
            Vec::new(),
            ObjectId(0),
            PlayerId(0),
        )
    }

    #[test]
    fn mills_and_loses_life_for_nonland() {
        // 3 rad counters, library has [nonland, land, nonland]
        let mut state = setup_state_with_rad(3, vec![false, true, false]);
        let ability = make_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 18);
        assert_eq!(state.players[0].player_counter(&PlayerCounterKind::Rad), 1);
        assert!(state.players[0].library.is_empty());
    }

    #[test]
    fn empty_library_no_effect() {
        let mut state = setup_state_with_rad(3, vec![]);
        let ability = make_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[0].player_counter(&PlayerCounterKind::Rad), 3);
    }

    #[test]
    fn all_lands_no_life_loss() {
        let mut state = setup_state_with_rad(2, vec![true, true]);
        let ability = make_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[0].player_counter(&PlayerCounterKind::Rad), 2);
        assert!(state.players[0].library.is_empty());
    }

    #[test]
    fn library_smaller_than_rad_count() {
        // 5 rad counters but only 2 cards (1 nonland, 1 land)
        let mut state = setup_state_with_rad(5, vec![false, true]);
        let ability = make_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 19);
        assert_eq!(state.players[0].player_counter(&PlayerCounterKind::Rad), 4);
    }

    #[test]
    fn zero_rad_counters_noop() {
        let mut state = setup_state_with_rad(0, vec![false, false]);
        let ability = make_ability();
        let mut events = Vec::new();

        resolve(&mut state, &ability, &mut events).unwrap();

        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[0].library.len(), 2);
    }
}

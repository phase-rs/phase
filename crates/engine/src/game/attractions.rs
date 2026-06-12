//! CR 717 + CR 701.51 + CR 701.52: Unfinity Attraction deck, open, and visit.
//!
//! Attractions live in a supplementary deck (command zone) tracked per player via
//! `Player::attraction_deck`. Opening moves the top card to the battlefield; rolling
//! to visit is a turn-based action at the beginning of the active player's precombat
//! main phase when they control an Attraction.

use crate::types::ability::{EffectKind, ResolvedAbility};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects::roll_die;
use super::game_object::GameObject;
use crate::types::ability::EffectError;

/// CR 717.1: Default lit numbers when card data omits variant lights (1 and 6 are always lit).
pub fn default_attraction_lights() -> Vec<u8> {
    vec![1, 6]
}

pub fn is_attraction_card(obj: &GameObject) -> bool {
    obj.in_attraction_deck
        || !obj.attraction_lights.is_empty()
        || obj
            .card_types
            .subtypes
            .iter()
            .any(|s| s.eq_ignore_ascii_case("Attraction"))
}

pub fn is_attraction_permanent(obj: &GameObject) -> bool {
    obj.zone == Zone::Battlefield && is_attraction_card(obj)
}

/// CR 701.51b: Put the top card of the controller's Attraction deck onto the battlefield.
pub fn open_attractions(
    state: &mut GameState,
    player: PlayerId,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    for opened in 0..count {
        let Some(object_id) = state
            .players
            .iter_mut()
            .find(|p| p.id == player)
            .and_then(|p| p.attraction_deck.pop_front())
        else {
            // CR 609.3: If the player has fewer Attractions than requested, open
            // as many as possible and ignore the impossible remainder.
            break;
        };
        // CR 614.1c: route the Attraction's battlefield entry through the
        // zone-change pipeline so the delivery tail applies enters-with-counters
        // statics (e.g. an artifact-scoped "enters with an additional counter"
        // static) — the raw `move_to_zone` skipped that tail, so an opened
        // Attraction never received them. CR 400.7 attributes the entry to the
        // opened object itself (the pre-pipeline raw move recorded no source).
        //
        // CR 616.1: a battlefield-entry pause IS reachable here — two co-played
        // external enter-tapped `Moved` effects (the Kismet / Frozen Aether
        // class parses as ChangeZone Moved defs) both write the entry event's
        // tap field, a material same-field collision that surfaces an ordering
        // prompt. On the pause, the paused Attraction's open bookkeeping and
        // the REMAINING opens of this instruction are deferred onto a
        // `BatchCompletion::AttractionOpenRemainder` so the replacement-choice
        // resume runs them — the old bail `break` left `in_attraction_deck`
        // set, never emitted `AttractionOpened`, and dropped the remaining
        // opens.
        match super::zone_pipeline::move_object(
            state,
            super::zone_pipeline::ZoneMoveRequest::effect(object_id, Zone::Battlefield, object_id),
            events,
        ) {
            super::zone_pipeline::ZoneMoveResult::Done => {}
            super::zone_pipeline::ZoneMoveResult::NeedsChoice(_)
            | super::zone_pipeline::ZoneMoveResult::NeedsAuraAttachmentChoice => {
                super::zone_pipeline::defer_completion_on_pause(
                    state,
                    crate::types::game_state::BatchCompletion::AttractionOpenRemainder {
                        player,
                        object_id,
                        remaining: count - opened - 1,
                    },
                );
                return Ok(());
            }
        }
        finish_attraction_open(state, player, object_id, events);
    }
    Ok(())
}

/// CR 701.51b + CR 701.51c: Per-Attraction open bookkeeping, run exactly once
/// after the Attraction's battlefield entry delivers — inline on the
/// synchronous path, or from `BatchCompletion::AttractionOpenRemainder` when
/// the entry parked on a CR 616.1 replacement-ordering choice and resumed.
/// Clears the supplementary-deck membership flag and emits `AttractionOpened`
/// (the "whenever a player opens an Attraction" trigger event, which fires only
/// when the card actually entered the battlefield — CR 701.51c).
pub(crate) fn finish_attraction_open(
    state: &mut GameState,
    player: PlayerId,
    object_id: ObjectId,
    events: &mut Vec<GameEvent>,
) {
    if let Some(obj) = state.objects.get_mut(&object_id) {
        obj.in_attraction_deck = false;
    }
    // CR 701.51c: the "opens an Attraction" trigger fires only when the card
    // actually entered the battlefield — "If an effect prevents that Attraction
    // from entering the battlefield or replaces entering the battlefield with
    // another event, that ability doesn't trigger." `ZoneMoveResult::Done`
    // also covers prevented/redirected deliveries, so gate on arrival.
    if state
        .objects
        .get(&object_id)
        .is_some_and(|obj| obj.zone == Zone::Battlefield)
    {
        events.push(GameEvent::AttractionOpened {
            player_id: player,
            object_id,
        });
    }
}

/// CR 701.52a: Roll a d6 and visit each controlled Attraction whose lights include the result.
pub fn roll_to_visit_attractions(
    state: &mut GameState,
    player: PlayerId,
    events: &mut Vec<GameEvent>,
) {
    if !controls_attraction(state, player) {
        return;
    }
    let roll = roll_die::roll_die(state, player, 6, events);
    events.push(GameEvent::AttractionsRolledToVisit {
        player_id: player,
        roll,
    });
    let visited = visited_attraction_ids(state, player, roll);
    for attraction_id in visited {
        events.push(GameEvent::AttractionVisited {
            player_id: player,
            roll,
            attraction_id,
        });
    }
    super::triggers::process_triggers(state, events);
}

/// CR 703.4g + CR 717.4: Turn-based action at the beginning of the precombat main phase.
pub fn perform_roll_to_visit_turn_based_action(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let player = state.active_player;
    roll_to_visit_attractions(state, player, events);
}

fn controls_attraction(state: &GameState, player: PlayerId) -> bool {
    state.battlefield.iter().any(|id| {
        state
            .objects
            .get(id)
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            .is_some_and(|o| {
                o.controller == player && o.is_phased_in() && is_attraction_permanent(o)
            })
    })
}

fn visited_attraction_ids(state: &GameState, player: PlayerId, roll: u8) -> Vec<ObjectId> {
    state
        .battlefield
        .iter()
        .filter_map(|id| {
            let obj = state.objects.get(id)?;
            // CR 702.26b: a phased-out permanent is treated as though it does not exist.
            if obj.controller != player || !obj.is_phased_in() || !is_attraction_permanent(obj) {
                return None;
            }
            if obj.attraction_lights.contains(&roll) {
                Some(*id)
            } else {
                None
            }
        })
        .collect()
}

pub fn resolve_open(
    state: &mut GameState,
    ability: &ResolvedAbility,
    count: u32,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    open_attractions(state, ability.controller, count, events)?;
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::OpenAttractions,
        source_id: ability.source_id,
    });
    Ok(())
}

pub fn resolve_roll_to_visit(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
) -> Result<(), EffectError> {
    roll_to_visit_attractions(state, ability.controller, events);
    events.push(GameEvent::EffectResolved {
        kind: EffectKind::RollToVisitAttractions,
        source_id: ability.source_id,
    });
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::engine::apply_as_current;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, Effect, ReplacementDefinition, TargetFilter,
    };
    use crate::types::actions::GameAction;
    use crate::types::game_state::WaitingFor;
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;

    /// CR 701.51 + CR 616.1 discriminating test (fail-first): an Attraction
    /// whose battlefield entry parks on a replacement-ordering prompt (two
    /// co-played external enter-tapped `Moved` effects — the Kismet / Frozen
    /// Aether class parses as ChangeZone Moved defs and collides on the entry's
    /// tap field) must, after the prompt is answered, still receive its open
    /// bookkeeping (`in_attraction_deck` cleared, `AttractionOpened` emitted)
    /// AND the remaining opens of the same instruction must still happen. The
    /// old bail `break` skipped the bookkeeping on the paused Attraction and
    /// silently dropped every remaining open.
    #[test]
    fn paused_attraction_open_resumes_bookkeeping_and_remaining_opens() {
        let mut state = GameState::new_two_player(42);
        let player = PlayerId(0);

        // Two Attractions in the supplementary deck (command zone).
        let mut attractions = Vec::new();
        for i in 0..2u64 {
            let id = create_object(
                &mut state,
                CardId(100 + i),
                player,
                format!("Attraction {i}"),
                Zone::Command,
            );
            state.objects.get_mut(&id).unwrap().in_attraction_deck = true;
            state.players[0].attraction_deck.push_back(id);
            attractions.push(id);
        }

        // Two external enter-tapped Moved replacements (Kismet / Frozen Aether
        // class) — both write the entry event's tap field, so CR 616.1 prompts.
        for (offset, name) in [(0u64, "Kismet"), (1, "Frozen Aether")] {
            let oid = ObjectId(9000 + offset);
            let mut src = GameObject::new(
                oid,
                CardId(900 + offset),
                PlayerId(1),
                name.to_string(),
                Zone::Battlefield,
            );
            src.replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::Moved)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::SetTapState {
                        target: TargetFilter::SelfRef,
                        scope: crate::types::ability::EffectScope::Single,
                        state: crate::types::ability::TapStateChange::Tap,
                    },
                ))
                .destination_zone(Zone::Battlefield)
                .description(name.to_string())]
            .into();
            state.objects.insert(oid, src);
            state.battlefield.push_back(oid);
        }

        let mut events = Vec::new();
        open_attractions(&mut state, player, 2, &mut events).expect("open attractions");

        // CR 616.1: the first open parked on the enter-tapped collision.
        let WaitingFor::ReplacementChoice {
            player: chooser, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "expected parked ReplacementChoice for the enter-tapped collision, got {:?}",
                state.waiting_for
            );
        };
        state.priority_player = chooser;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume first open");

        // The first Attraction's open bookkeeping ran on resume.
        let first = &state.objects[&attractions[0]];
        assert_eq!(first.zone, Zone::Battlefield, "first Attraction delivered");
        assert!(
            !first.in_attraction_deck,
            "open bookkeeping must run on the resumed Attraction (old bail left the flag set)"
        );

        // The remaining open ran — and re-parked on its own entry prompt.
        let WaitingFor::ReplacementChoice {
            player: chooser2, ..
        } = state.waiting_for.clone()
        else {
            panic!(
                "remaining open must run after the pause and re-park, got {:?} (old bail dropped it)",
                state.waiting_for
            );
        };
        state.priority_player = chooser2;
        apply_as_current(&mut state, GameAction::ChooseReplacement { index: 0 })
            .expect("resume second open");

        let second = &state.objects[&attractions[1]];
        assert_eq!(
            second.zone,
            Zone::Battlefield,
            "remaining open must deliver after the pause (old bail dropped it)"
        );
        assert!(!second.in_attraction_deck);
    }
}

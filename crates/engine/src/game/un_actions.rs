//! Optional Un-card actions: the engine side of [`crate::types::un_actions`].
//!
//! Silver-border cards ask players to *do* things — say a phrase, touch their
//! face, laugh. The rules engine models those as ordinary game events:
//!
//! * A **say-it payment** (`AbilityCost::SayPhrase`) rides the CR 118.12
//!   unless-payment / optional-payment pipelines. [`player_can_say_phrases`]
//!   gates it: a player who did not opt in is never offered the choice, so the
//!   "unless" branch simply happens (CR 118.12a: the same as declining to pay).
//! * A **gesture** ([`UnTell`]) is performed from the Un-actions menu. The
//!   engine records it as `GameEvent::UnTellPerformed` and opens a Gotcha
//!   window for every opted-in opponent holding a matching Gotcha ability.
//! * Calling "Gotcha!" emits `GameEvent::GotchaSaid`, which the card's
//!   `TriggerMode::GotchaSaid` trigger consumes — the card's "When you do,
//!   return ..." clause (CR 603.12 reflexive trigger shape) puts the return on
//!   the stack through the ordinary trigger pipeline.

use crate::types::ability::{AbilityCost, Effect, TriggerDefinition};
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;
use crate::types::triggers::TriggerMode;
use crate::types::un_actions::{
    spoken_words, word_matches, GotchaCondition, GotchaOffer, UnAction, UnTell, UnTellKind,
};
use crate::types::zones::Zone;

use super::engine::EngineError;

/// Whether `player` opted in to Un-card actions — the gate on every say-it
/// prompt and every Gotcha offer.
pub fn player_can_say_phrases(state: &GameState, player: PlayerId) -> bool {
    state.un_actions.is_enabled(player)
}

/// The prompt text for an optional spoken payment ("you may say \"I'm here.\""),
/// so the "you may" window names the phrase it asks for.
pub fn say_phrase_prompt(effect: &Effect) -> Option<String> {
    match effect {
        Effect::PayCost {
            cost: AbilityCost::SayPhrase { phrase },
            ..
        } => Some(format!("Say \"{phrase}\"")),
        _ => None,
    }
}

/// Whether paying `cost` would need `player` to say a phrase they have not
/// opted in to. Such a payer is skipped when an unless-cost is surfaced, so the
/// "unless" branch happens exactly as if they had declined (CR 118.12a).
pub fn cost_requires_unavailable_speech(
    state: &GameState,
    player: PlayerId,
    cost: &AbilityCost,
) -> bool {
    match cost {
        AbilityCost::SayPhrase { .. } => !player_can_say_phrases(state, player),
        AbilityCost::Composite { costs } => costs
            .iter()
            .any(|c| cost_requires_unavailable_speech(state, player, c)),
        AbilityCost::OneOf { costs } => costs
            .iter()
            .all(|c| cost_requires_unavailable_speech(state, player, c)),
        _ => false,
    }
}

/// The Gotcha trigger definitions that currently function on `object_id`.
fn gotcha_definitions<'a>(
    state: &'a GameState,
    object_id: ObjectId,
) -> impl Iterator<Item = &'a TriggerDefinition> + 'a {
    state
        .objects
        .get(&object_id)
        .into_iter()
        .flat_map(move |obj| {
            super::functioning_abilities::active_trigger_definitions(state, obj)
                .map(|active| active.definition)
                .filter(|def| def.mode == TriggerMode::GotchaSaid)
                .filter(move |def| {
                    super::triggers::trigger_definition_functions_in_zone(def, obj.zone)
                })
        })
}

/// The player who holds an object's abilities: the controller on the
/// battlefield, the owner everywhere else.
fn holder_of(state: &GameState, object_id: ObjectId) -> Option<PlayerId> {
    let obj = state.objects.get(&object_id)?;
    Some(if obj.zone == Zone::Battlefield {
        obj.controller
    } else {
        obj.owner
    })
}

/// Graveyard cards of `player` whose name contains a word `tell` spoke.
fn graveyard_cards_named_in(state: &GameState, player: PlayerId, tell: &UnTell) -> Vec<ObjectId> {
    let UnTell::Said { text } = tell else {
        return Vec::new();
    };
    let spoken: Vec<String> = spoken_words(text).collect();
    state
        .players
        .get(player.0 as usize)
        .map(|p| p.graveyard.iter().copied().collect::<Vec<_>>())
        .unwrap_or_default()
        .into_iter()
        .filter(|id| {
            state.objects.get(id).is_some_and(|card| {
                spoken_words(&card.name)
                    .any(|name_word| spoken.iter().any(|sp| word_matches(&name_word, sp)))
            })
        })
        .collect()
}

/// The Gotcha windows `tell` opens: one per opted-in opponent of the performer
/// holding a functioning Gotcha ability whose condition `tell` satisfies.
pub fn open_gotcha_offers(
    state: &GameState,
    performer: PlayerId,
    tell: &UnTell,
) -> Vec<GotchaOffer> {
    let mut offers = Vec::new();
    for (&source_id, _) in state.objects.iter() {
        let Some(holder) = holder_of(state, source_id) else {
            continue;
        };
        if holder == performer
            || !player_can_say_phrases(state, holder)
            || !super::players::is_opponent(state, holder, performer)
        {
            continue;
        }
        for def in gotcha_definitions(state, source_id) {
            let Some(condition) = def.gotcha_condition.as_ref() else {
                continue;
            };
            match condition {
                GotchaCondition::SaysWordInGraveyardCardName => {
                    offers.extend(
                        graveyard_cards_named_in(state, holder, tell)
                            .into_iter()
                            .map(|returns| GotchaOffer {
                                id: 0,
                                player: holder,
                                source_id,
                                returns,
                            }),
                    );
                }
                other if other.matches_without_zone_context(tell) => {
                    offers.push(GotchaOffer {
                        id: 0,
                        player: holder,
                        source_id,
                        returns: source_id,
                    });
                }
                _ => {}
            }
        }
    }
    offers.sort_by_key(|o| (o.player, o.source_id.0, o.returns.0));
    offers.dedup();
    offers
}

/// Append newly opened Gotcha offers to existing offers in `state.un_actions.gotcha_offers`,
/// minting a unique identifier for each new offer and preserving offers from earlier gestures.
pub fn append_gotcha_offers(state: &mut GameState, performer: PlayerId, tell: &UnTell) {
    let new_offers = open_gotcha_offers(state, performer, tell);
    if new_offers.is_empty() {
        return;
    }
    for mut offer in new_offers {
        offer.id = state.un_actions.mint_offer_id();
        state.un_actions.gotcha_offers.push(offer);
    }
    // Sort so that newer offers (larger id) appear first for identical (player, source_id, returns),
    // allowing dedup_by to keep the most recent offer.
    state.un_actions.gotcha_offers.sort_by(|a, b| {
        (
            a.player,
            a.source_id.0,
            a.returns.0,
            std::cmp::Reverse(a.id),
        )
            .cmp(&(
                b.player,
                b.source_id.0,
                b.returns.0,
                std::cmp::Reverse(b.id),
            ))
    });
    state.un_actions.gotcha_offers.dedup_by(|b, a| {
        a.player == b.player && a.source_id == b.source_id && a.returns == b.returns
    });
    state
        .un_actions
        .gotcha_offers
        .sort_by_key(|o| (o.player, o.source_id.0, o.returns.0, o.id));
}

/// Drain any pending tells recorded outside priority and generate Gotcha offers
/// once priority is settled.
pub fn announce_pending_tells(state: &mut GameState) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return;
    }
    let pending = std::mem::take(&mut state.un_actions.pending_tells);
    for (player, tell) in pending {
        append_gotcha_offers(state, player, &tell);
    }
}

/// Recompute the Un-actions menu: the gesture kinds that some functioning Gotcha
/// ability anywhere in the game responds to. Empty (and free) unless a player
/// opted in. Called after every action, since a Gotcha card can reach a
/// graveyard at any time.
pub fn refresh_menu(state: &mut GameState) {
    let menu = if state.un_actions.enabled_players.is_empty() {
        Vec::new()
    } else {
        let mut kinds: Vec<UnTellKind> = state
            .objects
            .keys()
            .flat_map(|&id| gotcha_definitions(state, id))
            .filter_map(|def| {
                def.gotcha_condition
                    .as_ref()
                    .map(GotchaCondition::tell_kind)
            })
            .collect();
        kinds.sort();
        kinds.dedup();
        kinds
    };
    if state.un_actions.menu != menu {
        state.un_actions.menu = menu;
    }
}

/// Apply a `GameAction::UnAction`.
pub fn apply_un_action(
    state: &mut GameState,
    actor: PlayerId,
    action: UnAction,
) -> Result<ActionResult, EngineError> {
    match action {
        UnAction::SetEnabled { enabled } => {
            state.un_actions.set_enabled(actor, enabled);
            if !enabled {
                state.un_actions.gotcha_offers.retain(|o| o.player != actor);
            }
            Ok(ActionResult::applied(vec![], state.waiting_for.clone()))
        }
        UnAction::PerformTell { tell } => {
            let events = vec![GameEvent::UnTellPerformed {
                player: actor,
                tell: tell.clone(),
            }];
            if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
                append_gotcha_offers(state, actor, &tell);
            } else {
                state.un_actions.pending_tells.push((actor, tell));
            }
            Ok(ActionResult::applied(events, state.waiting_for.clone()))
        }
        UnAction::SayGotcha { source_id, returns } => {
            let Some(position) = state.un_actions.gotcha_offers.iter().position(|o| {
                o.player == actor && o.source_id == source_id && o.returns == returns
            }) else {
                return Err(EngineError::InvalidAction(
                    "No open Gotcha window for that card".to_string(),
                ));
            };
            state.un_actions.gotcha_offers.remove(position);
            let mut events = vec![GameEvent::GotchaSaid {
                player: actor,
                source_id,
                returns,
            }];
            let waiting_for = state.waiting_for.clone();
            let settled = super::engine_priority::run_post_action_pipeline_from_settled_priority(
                state,
                &mut events,
                0,
                &waiting_for,
            )?;
            state.waiting_for = settled.clone();
            Ok(ActionResult::applied(events, settled))
        }
    }
}

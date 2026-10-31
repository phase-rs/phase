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

use crate::game::game_object::ObjectIdentityBindingExt;
use crate::types::ability::{AbilityCost, Effect, TriggerDefinition};
use crate::types::events::GameEvent;
use crate::types::game_state::{ActionResult, GameState, WaitingFor};
use crate::types::identifiers::{ObjectId, ObjectIdentityBinding};
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

/// Bind `object_id` to its current incarnation and zone so a Gotcha offer goes
/// stale when that object changes zones (CR 400.7). Returns `None` if the object
/// is gone.
fn current_binding(state: &GameState, object_id: ObjectId) -> Option<ObjectIdentityBinding> {
    state
        .objects
        .get(&object_id)
        .map(ObjectIdentityBinding::from_object)
}

/// The Gotcha windows `tell` opens: one per opted-in opponent of the performer
/// holding a functioning Gotcha ability whose condition `tell` satisfies.
pub fn open_gotcha_offers(
    state: &GameState,
    performer: PlayerId,
    tell: &UnTell,
) -> Vec<GotchaOffer> {
    // CR 102.1, CR 800.4: an eliminated player has left the game; their gestures
    // cannot satisfy opponent trigger conditions.
    if !super::players::is_alive(state, performer) {
        return Vec::new();
    }
    let mut offers = Vec::new();
    for (&source_id, _) in state.objects.iter() {
        let Some(holder) = holder_of(state, source_id) else {
            continue;
        };
        // CR 102.1, CR 800.4: holder must still be alive and in the game.
        if holder == performer
            || !super::players::is_alive(state, holder)
            || !player_can_say_phrases(state, holder)
            || !super::players::is_opponent(state, holder, performer)
        {
            continue;
        }
        let Some(source) = current_binding(state, source_id) else {
            continue;
        };
        for def in gotcha_definitions(state, source_id) {
            let Some(condition) = def.gotcha_condition.as_ref() else {
                continue;
            };
            match condition {
                GotchaCondition::SaysWordInGraveyardCardName => {
                    offers.extend(
                        graveyard_cards_named_in(state, holder, tell)
                            .into_iter()
                            .filter_map(|returns| {
                                current_binding(state, returns).map(|returns| GotchaOffer {
                                    id: 0,
                                    player: holder,
                                    source,
                                    returns,
                                })
                            }),
                    );
                }
                other if other.matches_without_zone_context(tell) => {
                    offers.push(GotchaOffer {
                        id: 0,
                        player: holder,
                        source,
                        returns: source,
                    });
                }
                _ => {}
            }
        }
    }
    offers.sort_by_key(|o| {
        (
            o.player,
            o.source.reference.object_id.0,
            o.returns.reference.object_id.0,
        )
    });
    offers.dedup();
    offers
}

/// Append `new_offers` to `state.un_actions.gotcha_offers`, minting a unique
/// identifier for each and preserving offers from earlier gestures. An offer
/// identical (player, source, returned card) to an existing one replaces it, so
/// the window stays one-per-card.
fn append_gotcha_offers(state: &mut GameState, new_offers: Vec<GotchaOffer>) {
    for mut offer in new_offers {
        offer.id = state.un_actions.mint_offer_id();
        state.un_actions.gotcha_offers.retain(|o| {
            !(o.player == offer.player && o.source == offer.source && o.returns == offer.returns)
        });
        state.un_actions.gotcha_offers.push(offer);
    }
    state.un_actions.gotcha_offers.sort_by_key(|o| {
        (
            o.player,
            o.source.reference.object_id.0,
            o.returns.reference.object_id.0,
            o.id,
        )
    });
}

/// Record that `performer` just performed `tell`.
///
/// CR 603.10: objects that exist immediately after an event are checked against
/// the trigger conditions that event matched, so the eligible Gotcha sources and
/// cards are resolved by ordinary event-time matching when the tell happens. The
/// matched offers are announced at once when already at a Priority boundary, or
/// queued in `pending_offers` to open once priority settles.
///
/// A player who has not opted in to Un-card actions is treated as having
/// declined any say-it payment, so the "unless" branch simply happens
/// (CR 118.12a).
pub fn record_tell(state: &mut GameState, performer: PlayerId, tell: &UnTell) {
    if !super::players::is_alive(state, performer) {
        return;
    }
    let offers = open_gotcha_offers(state, performer, tell);
    if offers.is_empty() {
        return;
    }
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        append_gotcha_offers(state, offers);
        return;
    }
    for offer in offers {
        let already_pending = state.un_actions.pending_offers.iter().any(|o| {
            o.player == offer.player && o.source == offer.source && o.returns == offer.returns
        });
        if !already_pending {
            state.un_actions.pending_offers.push(offer);
        }
    }
}

/// Open the Gotcha offers matched while a decision was pending, once priority is
/// next settled. Re-validates each queued offer against the live board before
/// announcing it: a source or returned card that changed zones, or a player that
/// opted out, no longer gets a window.
pub fn announce_pending_tells(state: &mut GameState) {
    if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        return;
    }
    let pending = std::mem::take(&mut state.un_actions.pending_offers);
    let validated: Vec<GotchaOffer> = pending
        .into_iter()
        .filter(|offer| offer_is_still_valid(state, offer))
        .collect();
    append_gotcha_offers(state, validated);
}

/// Whether a queued Gotcha offer still matches the live game state. The offer was
/// matched when the tell was made; between then and the next priority boundary the
/// source may have changed zones, its controller may have opted out, or the returned
/// card may have moved zones — each of which invalidates the window.
///
/// CR 113.6m + CR 400.7: validates that a Gotcha offer's source and returned card
/// remain intact on the live state. The source must be the same incarnation still in
/// its expected zone with its ability still functioning there (CR 113.6: abilities
/// function only in the zones they specify; Gotcha abilities on graveyard cards
/// function from the graveyard per CR 113.6m), the recipient must remain in the game
/// (CR 102.1, CR 800.4) and opted in to Un-card actions (an engine opt-in preference
/// with no CR entry), and the returned card must be the same incarnation still in its
/// expected zone rather than a new object that changed zones (CR 400.7).
fn offer_is_still_valid(state: &GameState, offer: &GotchaOffer) -> bool {
    // CR 102.1, CR 800.4: recipient must still be in the game.
    super::players::is_alive(state, offer.player)
        // CR 400.7: a source that changed zones (even one blinked straight back) is a
        // new object with no relation to the one the tell matched.
        && offer.source.is_current(state)
        // CR 109.5: "you" on the source is its controller (or owner off the battlefield);
        // an offer is only the current holder's to accept, so a control change closes it.
        && holder_of(state, offer.source.reference.object_id) == Some(offer.player)
        // CR 113.6m: the source must still carry a functioning Gotcha ability in its
        // current zone (gotcha_definitions applies both checks).
        && gotcha_definitions(state, offer.source.reference.object_id)
            .next()
            .is_some()
        // The recipient must still be opted in to Un-card actions.
        && player_can_say_phrases(state, offer.player)
        // CR 400.7: the returned card must be the same incarnation still in its expected
        // zone — a zone change makes it a new object, invalidating the window (a stale
        // ObjectId might now name a different card).
        && offer.returns.is_current(state)
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
                state
                    .un_actions
                    .pending_offers
                    .retain(|o| o.player != actor);
            }
            Ok(ActionResult::applied(vec![], state.waiting_for.clone()))
        }
        UnAction::PerformTell { tell } => {
            // CR 102.1, CR 800.4: an eliminated player has left the game and
            // cannot take game actions or produce rules-relevant tells.
            if !super::players::is_alive(state, actor) {
                return Err(EngineError::InvalidAction(
                    "Player has left the game".to_string(),
                ));
            }
            if !state.un_actions.is_enabled(actor) {
                return Err(EngineError::InvalidAction(
                    "Player has not opted in to Un-card actions".to_string(),
                ));
            }
            let events = vec![GameEvent::UnTellPerformed {
                player: actor,
                tell: tell.clone(),
            }];
            record_tell(state, actor, &tell);
            Ok(ActionResult::applied(events, state.waiting_for.clone()))
        }
        UnAction::SayGotcha { source_id, returns } => {
            // CR 102.1, CR 800.4: an eliminated player has left the game and
            // cannot take game actions.
            if !super::players::is_alive(state, actor) {
                return Err(EngineError::InvalidAction(
                    "Player has left the game".to_string(),
                ));
            }
            let Some(position) = state.un_actions.gotcha_offers.iter().position(|o| {
                o.player == actor
                    && o.source.reference.object_id == source_id
                    && o.returns.reference.object_id == returns
            }) else {
                return Err(EngineError::InvalidAction(
                    "No open Gotcha window for that card".to_string(),
                ));
            };
            let offer = state.un_actions.gotcha_offers[position].clone();
            // CR 400.7: re-validate the source and returned card are still the same
            // incarnations in their expected zones before firing the reflexive
            // trigger — either may have moved zones while the window was open.
            if !offer_is_still_valid(state, &offer) {
                return Err(EngineError::InvalidAction(
                    "That Gotcha window has closed (its source or returned card moved or left play)"
                        .to_string(),
                ));
            }
            state.un_actions.gotcha_offers.remove(position);
            let mut events = vec![GameEvent::GotchaSaid {
                player: actor,
                source: offer.source,
                returns: offer.returns,
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

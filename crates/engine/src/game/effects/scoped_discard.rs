//! CR 101.4: per-player hand discards for one simultaneous instruction
//! ("each opponent discards a card", "each player discards a card").
//!
//! Every affected player chooses in APNAP order while every hand is intact, and
//! only then do the chosen cards leave their hands together. The picks are held
//! in the shared `PendingPlayerScopeChoice` carrier until the last choice is
//! made. CR 101.4a lets hands stay face down: a later chooser never learns
//! which card an earlier one picked, and the carrier is never projected to a
//! viewer with its picks (`visibility`).
//!
//! The pool, count and "up to" rules are the ones `discard::resolve` applies to
//! one seat (`discard::player_hand_discard_pool` is their single authority).
//! Delivery hands the collected cards to that same resolver as one specific-card
//! discard, so replacement effects (CR 614.6), Madness-style redirects and a
//! CR 616.1 ordering pause run exactly as for any other discard.

use crate::game::game_object::ObjectIncarnationRefExt;
use crate::types::ability::{
    Effect, EffectError, EffectKind, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{DiscardCause, GameState, PendingPlayerScopeChoice, WaitingFor};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::discard::{player_hand_discard_pool, PlayerHandDiscardPool};

/// CR 101.4: whether this player fan-out is a bare, chosen hand discard
/// whose per-player choices must all be made before any card is discarded.
///
/// Only the bare instruction is collected. A random discard makes no choice
/// (CR 701.9b); an "unless" discard (CR 608.2c) and any per-player rider — an
/// unless-cost, an optional or "may" grant, a repeat, a condition or its
/// "otherwise" branch, a published result — keep the generic per-seat route,
/// which owns those prompts.
pub(super) fn should_collect_player_scope_choices(ability: &ResolvedAbility) -> bool {
    matches!(
        &ability.effect,
        Effect::Discard {
            target,
            selection,
            unless_filter: None,
            ..
        } if selection.is_chosen()
            && (matches!(target, TargetFilter::Controller | TargetFilter::ScopedPlayer)
                || (ability.multi_target.is_some() && super::is_multi_target_player_filter(target)))
    ) && ability.sub_ability.is_none()
        && ability.else_ability.is_none()
        && ability.unless_pay.is_none()
        && !ability.optional
        && ability.optional_for.is_none()
        && ability.optional_player.is_none()
        && ability.condition.is_none()
        && ability.repeat_for.is_none()
        && ability.repeat_until.is_none()
        && ability.declares_return_result.is_none()
        && ability.targets.is_empty()
}

enum ScopedDiscardStep {
    /// CR 608.2d: nothing to choose (an empty pool, a zero count, no player).
    Noop,
    /// The whole eligible pool is discarded; there is no choice to make.
    Forced(Vec<ObjectId>),
    Prompt {
        player: PlayerId,
        count: usize,
        up_to: bool,
        cards: Vec<ObjectId>,
    },
}

/// What a collected answer led to.
pub(crate) enum PendingPlayerScopeDiscardOutcome {
    WaitingForNextChoice,
    /// A CR 616.1 replacement choice parked the delivery; the replacement-choice
    /// resume path finishes it and then drains the continuation.
    PausedForReplacement,
    Completed {
        events_before: usize,
        events_after: usize,
    },
}

/// The single authority on what `seat` chooses from now (CR 701.9b).
fn scoped_discard_step(
    state: &GameState,
    template: &ResolvedAbility,
    original_controller: PlayerId,
    seat: PlayerId,
) -> ScopedDiscardStep {
    let scoped = if template.multi_target.is_some() {
        // CR 109.5 + CR 608.2e: Bind the chosen member, retaining the
        // spell's controller for forced-action restrictions and caster reads.
        let mut bound = template.clone();
        bound.set_scoped_player_recursive(seat);
        bound.set_unpinned_targets(vec![TargetRef::Player(seat)]);
        bound
    } else {
        super::bind_scoped_seat(template, original_controller, seat)
    };
    let Effect::Discard {
        count,
        target,
        filter,
        ..
    } = &scoped.effect
    else {
        return ScopedDiscardStep::Noop;
    };
    // CR 701.9b + CR 608.2d: `UpTo` is the may-pick-fewer flag, not a count.
    let (inner, up_to) = count.peel_up_to();
    // CR 107.1b: a calculation that would yield a negative number uses zero.
    let wanted =
        crate::game::quantity::resolve_quantity_with_targets(state, inner, &scoped).max(0) as usize;
    let (player, pool) = match player_hand_discard_pool(state, &scoped, target, filter.as_ref()) {
        Ok(PlayerHandDiscardPool::Eligible { player, cards }) => (player, cards),
        // CR 608.2b + CR 701.9a + CR 609.3: no player, or a player no spell or
        // ability can force to discard, discards nothing.
        Ok(PlayerHandDiscardPool::NoPlayer | PlayerHandDiscardPool::Muzzled) | Err(_) => {
            return ScopedDiscardStep::Noop;
        }
    };
    let count = if up_to {
        wanted
    } else {
        wanted.min(pool.len())
    };
    if pool.is_empty() || (count == 0 && !up_to) {
        ScopedDiscardStep::Noop
    } else if !up_to && pool.len() <= count {
        ScopedDiscardStep::Forced(pool)
    } else {
        ScopedDiscardStep::Prompt {
            player,
            count,
            up_to,
            cards: pool,
        }
    }
}

fn set_scoped_discard_waiting_for(
    state: &mut GameState,
    template: &ResolvedAbility,
    player: PlayerId,
    count: usize,
    up_to: bool,
    cards: Vec<ObjectId>,
) {
    state.waiting_for = WaitingFor::DiscardChoice {
        player,
        count,
        cards,
        source_id: template.source_id,
        effect_kind: EffectKind::from(&template.effect),
        up_to,
        unless_filter: None,
        discard_frame: None,
    };
}

/// CR 400.7: pin a chosen card to the object it is now, with its chooser.
fn selections_for(
    state: &GameState,
    chooser: PlayerId,
    cards: &[ObjectId],
) -> Vec<(PlayerId, ObjectIncarnationRef)> {
    cards
        .iter()
        .filter_map(|id| super::bounce::selection_for(state, chooser, *id))
        .collect()
}

/// CR 400.7: the pick is still that object, still in its owner's hand.
fn selection_is_in_hand(state: &GameState, pin: &ObjectIncarnationRef) -> bool {
    pin.is_current(state)
        && state
            .objects
            .get(&pin.object_id)
            .is_some_and(|object| object.zone == Zone::Hand)
}

/// CR 101.4: deliver every announced discard as ONE simultaneous action through
/// the discard resolver (CR 614.6 redirects fire; a CR 616.1 pause parks the
/// remaining cards). CR 400.7: a pick that left its hand since it was chosen is
/// a new object; it is not discarded from wherever it is now.
fn deliver_scoped_discards(
    state: &mut GameState,
    template: &ResolvedAbility,
    selections: Vec<(PlayerId, ObjectIncarnationRef)>,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<super::ResolutionProgress, EffectError> {
    let cards: Vec<TargetRef> = selections
        .into_iter()
        .filter(|(_, pin)| selection_is_in_hand(state, pin))
        .map(|(_, pin)| TargetRef::Object(pin.object_id))
        .collect();
    if cards.is_empty() {
        // The marker the delivery resolver would have ended with.
        events.push(GameEvent::EffectResolved {
            kind: EffectKind::DiscardCard,
            source_id: template.source_id,
            subject: None,
        });
        return Ok(super::ResolutionProgress::Complete);
    }
    let delivery = scoped_discard_delivery_node(template, cards, DiscardCause::Effect);
    super::resolve_ability_chain_step(state, &delivery, events, depth)
}

/// CR 101.4 + CR 701.9a: the one specific-card discard node that delivers a
/// simultaneous instruction's collected hand picks (`cards`, already checked
/// to still be in hand). `cause` is the node's own (CR 118.12): `Effect` for
/// "each opponent discards a card", `Cost` for the discards chosen to pay a
/// CR 118.12a unless-cost (`simultaneous_unless`), which Library-of-Leng-class
/// replacements must not touch.
pub(super) fn scoped_discard_delivery_node(
    template: &ResolvedAbility,
    cards: Vec<TargetRef>,
    cause: DiscardCause,
) -> ResolvedAbility {
    let mut delivery = template.clone();
    delivery.effect = Effect::DiscardCard {
        count: cards.len() as u32,
        target: TargetFilter::ParentTarget,
    };
    delivery.multi_target = None;
    delivery.targets = cards;
    delivery.context.discard_cause = cause;
    delivery.sub_ability = None;
    delivery
}

/// CR 101.4: walk `remaining` in APNAP order, collecting each seat's choice
/// without discarding anything. Parks on the first seat with a real choice;
/// otherwise delivers the batch.
fn collect_scoped_discard_choices(
    state: &mut GameState,
    template: &ResolvedAbility,
    original_controller: PlayerId,
    mut remaining: Vec<PlayerId>,
    mut selections: Vec<(PlayerId, ObjectIncarnationRef)>,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<Option<super::ResolutionProgress>, EffectError> {
    while !remaining.is_empty() {
        let seat = remaining.remove(0);
        match scoped_discard_step(state, template, original_controller, seat) {
            ScopedDiscardStep::Noop => {}
            ScopedDiscardStep::Forced(cards) => {
                selections.extend(selections_for(state, seat, &cards));
            }
            ScopedDiscardStep::Prompt {
                player,
                count,
                up_to,
                cards,
            } => {
                state.pending_player_scope_choice = Some(PendingPlayerScopeChoice {
                    ability: Box::new(template.clone()),
                    remaining_players: remaining,
                    selections,
                });
                set_scoped_discard_waiting_for(state, template, player, count, up_to, cards);
                return Ok(None);
            }
        }
    }
    deliver_scoped_discards(state, template, selections, events, depth).map(Some)
}

/// Entry from the `player_scope` fan-out for a bare scoped discard.
pub(super) fn start_player_scope_choices(
    state: &mut GameState,
    template: &ResolvedAbility,
    original_controller: PlayerId,
    matching_players: &[PlayerId],
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<super::ResolutionProgress, EffectError> {
    let events_before = events.len();
    let Some(progress) = collect_scoped_discard_choices(
        state,
        template,
        original_controller,
        matching_players.to_vec(),
        Vec::new(),
        events,
        depth,
    )?
    else {
        return Ok(super::ResolutionProgress::Waiting);
    };
    if matches!(progress, super::ResolutionProgress::Complete) {
        // CR 608.2c: a clause that needed no prompt publishes its per-player
        // table, zero-filled over every player it applied to, as one result. A
        // clause that parked for a choice publishes none, as the per-seat
        // interactive route never did.
        let counts = super::previous_effect_counts_by_player_from_events(
            EffectKind::DiscardCard,
            template.source_id,
            &events[events_before..],
        )
        .map(|counts| super::fill_zero_contributors(counts, matching_players));
        super::install_previous_effect_counts_by_player(state, counts, false);
    }
    Ok(progress)
}

/// Whether the open carrier belongs to a scoped discard of `source_id`.
pub(crate) fn carrier_collects_discard(state: &GameState, source_id: ObjectId) -> bool {
    state
        .pending_player_scope_choice
        .as_ref()
        .is_some_and(|pending| {
            pending.ability.source_id == source_id
                && matches!(pending.ability.effect, Effect::Discard { .. })
        })
}

/// CR 101.4: record `chooser`'s answer and continue the APNAP walk.
pub(crate) fn advance_pending_player_scope_discard_choice(
    state: &mut GameState,
    chooser: PlayerId,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<PendingPlayerScopeDiscardOutcome, EffectError> {
    let Some(mut pending) = state.pending_player_scope_choice.take() else {
        return Ok(PendingPlayerScopeDiscardOutcome::WaitingForNextChoice);
    };
    pending
        .selections
        .extend(selections_for(state, chooser, chosen));
    let original_controller = pending.ability.controller;
    let events_before = events.len();
    // Delivery reads `waiting_for` to tell a replacement pause from completion:
    // the answered prompt is gone, so priority is what stands until one parks.
    state.waiting_for = WaitingFor::Priority { player: chooser };
    let progress = collect_scoped_discard_choices(
        state,
        &pending.ability,
        original_controller,
        pending.remaining_players,
        pending.selections,
        events,
        0,
    )?;
    Ok(match progress {
        None => PendingPlayerScopeDiscardOutcome::WaitingForNextChoice,
        Some(_) if !matches!(state.waiting_for, WaitingFor::Priority { .. }) => {
            PendingPlayerScopeDiscardOutcome::PausedForReplacement
        }
        Some(_) => PendingPlayerScopeDiscardOutcome::Completed {
            events_before,
            events_after: events.len(),
        },
    })
}

/// What a departure did to an open player-scope discard
/// (`reconcile_player_scope_discard`).
pub(crate) enum PlayerScopeDiscardReconciliation {
    /// No carrier, or its open prompt still stands for a living player, or the
    /// carrier was only pruned.
    Unchanged,
    /// The current chooser left. Their prompt was republished with the only
    /// answer it admits (no card); answering it continues the instruction.
    Settle,
    /// The carrier binds no open prompt and its acting player left; nothing can
    /// continue it, so it is dropped without completion.
    Retired,
}

/// CR 800.4a + CR 101.4: reconcile an open player-scope discard with the
/// players still in the game. A departed player makes no further choice and
/// discards nothing; a pick whose card left its hand (with its departed owner,
/// CR 800.4a) is a new object the instruction no longer finds (CR 400.7). A
/// surviving chooser's hand is unaffected by another player's departure, so
/// their open prompt stands.
pub(crate) fn reconcile_player_scope_discard(
    state: &mut GameState,
) -> PlayerScopeDiscardReconciliation {
    let Some(mut pending) = state
        .pending_player_scope_choice
        .take_if(|pending| matches!(pending.ability.effect, Effect::Discard { .. }))
    else {
        return PlayerScopeDiscardReconciliation::Unchanged;
    };
    let source_id = pending.ability.source_id;
    let open_chooser = match &state.waiting_for {
        WaitingFor::DiscardChoice {
            player,
            source_id: prompt_source,
            effect_kind: EffectKind::Discard,
            ..
        } if *prompt_source == source_id => Some(*player),
        _ => None,
    };
    pending
        .remaining_players
        .retain(|player| crate::game::players::is_alive(state, *player));
    pending.selections.retain(|(chooser, pin)| {
        crate::game::players::is_alive(state, *chooser) && selection_is_in_hand(state, pin)
    });
    let Some(chooser) = open_chooser else {
        let actor_departed = state
            .waiting_for
            .acting_player()
            .is_some_and(|player| !crate::game::players::is_alive(state, player));
        if actor_departed {
            return PlayerScopeDiscardReconciliation::Retired;
        }
        state.pending_player_scope_choice = Some(pending);
        return PlayerScopeDiscardReconciliation::Unchanged;
    };
    let template = (*pending.ability).clone();
    state.pending_player_scope_choice = Some(pending);
    if crate::game::players::is_alive(state, chooser) {
        return PlayerScopeDiscardReconciliation::Unchanged;
    }
    // CR 800.4a: a player who has left the game makes no choice.
    set_scoped_discard_waiting_for(state, &template, chooser, 0, false, Vec::new());
    PlayerScopeDiscardReconciliation::Settle
}

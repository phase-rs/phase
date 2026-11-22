//! CR 101.4: choose-all-then-act for a counted, player-scoped unless poll.
//!
//! Rottenmouth Viper: "Then for each blight counter on it, each opponent loses
//! 4 life unless that player sacrifices a nonland permanent of their choice or
//! discards a card." Every instance is one instruction taken on several players
//! (CR 608.2f), so each affected player, in APNAP order, makes every choice it
//! owes before anything happens (CR 101.4), and then every chosen permanent is
//! sacrificed, every chosen card discarded and every unpaid instance's life lost
//! together. The ruling (2024-07-26): "In a multiplayer game, each opponent in
//! turn order makes all of their choices, then all of the actions occur
//! simultaneously."
//!
//! This module is the CR 101.4 authority for counted scoped unless polls whose
//! answers must all come before any action: admission (`admits`), the per-instance
//! template, collection through the existing unless and pick prompts, delivery,
//! and reconciliation with a departure. While a poll collects it holds its
//! resolution open (`holds_resolution`): the resolving ability does not settle
//! and what it triggered waits in the deferred queue (CR 608.2c + CR 117.2e).
//! It owns only the counted for-each form, where each seat answers every
//! instance of the count (one or more). A
//! single scoped unless instruction (Hag of Ceaseless Torment, Bellowing
//! Mauler) is polled by `game::scoped_unless_poll`, which admits a chain at the
//! `player_scope` driver; this coordinator admits at the counted repeat above
//! it, so an instruction admitted here never reaches that poll. A counted
//! repeat this coordinator does not admit runs round by round, and each round
//! may open that poll.

use std::ops::Range;

use crate::game::costs::{
    single_choice_payment, single_choice_payment_branches, SingleChoicePayment,
};
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::types::ability::{
    AbilityCost, EffectError, EffectKind, RepeatForTiming, ResolvedAbility, TargetFilter, TargetRef,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    DiscardCause, GameState, PendingPlayerScopeSacrificeCompletion, PendingSimultaneousUnless,
    SimultaneousUnlessAnswer, SimultaneousUnlessCollection, SimultaneousUnlessDelivery,
    SimultaneousUnlessPhase, SimultaneousUnlessSeatAnswer, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::ResolutionProgress;

/// CR 101.4 + CR 608.2h: whether this outermost counted repeat resolves
/// choose-all-then-act. Keys on its own shape, not on the parser's
/// `SimultaneousInstances` stamp alone: the stamp covers shapes this
/// coordinator cannot represent, which fall through to the round-by-round
/// driver unchanged.
pub(super) fn admits(ability: &ResolvedAbility) -> bool {
    // A runtime iteration copy clears `repeat_for` but keeps the timing, so the
    // count itself is part of the shape.
    ability.repeat_timing == RepeatForTiming::SimultaneousInstances
        && ability.repeat_for.is_some()
        && ability.player_scope.is_some()
        && ability.unless_pay.as_ref().is_some_and(|unless| {
            unless.payer == TargetFilter::ScopedPlayer
                && single_choice_payment_branches(&unless.cost).is_some()
        })
        && !crate::game::resolution_prompt::effect_offers_choice(&ability.effect)
        && !super::awaits_resolution_object_choice(ability)
        && ability.sub_ability.is_none()
        && !ability.optional
        && ability.optional_for.is_none()
        && ability.optional_player.is_none()
        && ability.condition.is_none()
        && ability.else_ability.is_none()
        && ability.multi_target.is_none()
        && ability.distribute.is_none()
        && ability.repeat_until.is_none()
        && per_instance_template(ability).is_some()
}

/// CR 101.4 + CR 608.2c: the one-seat instruction every node of the poll is
/// built from: the fan-out's own template builder (`split_player_scope_chain`,
/// which clears `player_scope`), with the count and the unless modifier
/// removed, so no node built from it re-enters a fan-out, re-admits here or
/// re-prompts "unless". `None` when the split detaches a tail the coordinator
/// cannot carry (the shape then stays with the round-by-round driver).
fn per_instance_template(ability: &ResolvedAbility) -> Option<ResolvedAbility> {
    let scope = ability.player_scope.as_ref()?;
    let (mut template, tail) = super::split_player_scope_chain(ability, scope);
    if tail.is_some() {
        return None;
    }
    template.repeat_for = None;
    template.unless_pay = None;
    Some(template)
}

/// Where a step of the poll left it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WalkOutcome {
    /// A seat's prompt is the wait; the carrier stays open.
    Prompted,
    /// Every seat has answered every instance; the carrier is ready to deliver.
    Complete,
}

/// What a delivery left owed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeliveryOutcome {
    /// The chosen actions happened. `span` covers, in the buffer the outcome is
    /// returned into, every event of the delivery, including held events spliced
    /// back from earlier actions.
    Delivered { span: Range<usize> },
    /// CR 616.1: a step paused on a replacement choice and the carrier (in
    /// `Delivering`) holds the delivery's completed events, or the sacrifice
    /// batch paused and `pending_player_scope_sacrifice_choice.completion
    /// .deferred_events` holds them.
    Paused,
}

/// CR 101.4 + CR 608.2h: begin the poll. The number of instances is fixed once,
/// against the hydrated ability (a count that reads the trigger's object reads
/// it as it last existed). Seats answer in APNAP order, each finishing every
/// instance before the next starts. Nothing happens until every answer is in.
pub(super) fn start(
    state: &mut GameState,
    ability: &ResolvedAbility,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<ResolutionProgress, EffectError> {
    let (hydrated, count) = super::outermost_repeat_count(state, ability);
    let hydrated = hydrated.as_ref();
    let (Some(scope), Some(unless_pay), Some(template)) = (
        hydrated.player_scope.as_ref(),
        hydrated.unless_pay.clone(),
        per_instance_template(hydrated),
    ) else {
        return Err(EffectError::InvalidParam(
            "CR 101.4: a choose-all-then-act unless poll lost its admitted shape".to_string(),
        ));
    };
    let apnap = crate::game::players::apnap_order_from(
        state,
        hydrated.starting_with.clone(),
        hydrated.controller,
    );
    let mut seats = super::player_scope_members(state, scope, hydrated, apnap).into_iter();
    let (Some(first), Ok(instances)) = (seats.next(), u32::try_from(count)) else {
        return Ok(ResolutionProgress::Complete);
    };
    if instances == 0 {
        return Ok(ResolutionProgress::Complete);
    }
    state.pending_simultaneous_unless = Some(Box::new(PendingSimultaneousUnless {
        ability: Box::new(template),
        unless_pay,
        instances,
        phase: SimultaneousUnlessPhase::Collecting(SimultaneousUnlessCollection {
            current_player: first,
            next_instance: 0,
            remaining_players: seats.collect(),
            answers: Vec::new(),
            hidden_discards: Vec::new(),
        }),
        trigger_event: state.current_trigger_event.clone(),
        // CR 608.2c: bind the carrier to the resolution that wrote it.
        resolving_entry: state.resolving_stack_entry.as_ref().map(|entry| entry.id),
    }));
    match walk(state) {
        WalkOutcome::Prompted => Ok(ResolutionProgress::Waiting),
        // CR 608.2d: no seat had anything it could choose, so every instance
        // went unpaid without a prompt; the resolution delivers them now.
        WalkOutcome::Complete => {
            let carrier = state
                .pending_simultaneous_unless
                .take()
                .expect("the walk keeps the carrier it completes");
            match deliver(state, *carrier, events, depth)? {
                DeliveryOutcome::Delivered { .. } => Ok(ResolutionProgress::Complete),
                DeliveryOutcome::Paused => Ok(ResolutionProgress::Waiting),
            }
        }
    }
}

/// CR 608.2c + CR 117.2e + CR 101.4: whether an open poll holds its resolution
/// open. While it does, the resolving ability does not settle and nothing it
/// triggered is put on the stack; every route that ends the poll takes the
/// carrier first.
pub(crate) fn holds_resolution(state: &GameState) -> bool {
    state.pending_simultaneous_unless.is_some()
}

/// CR 608.2c: whether an answer by `player` to a prompt of `source_id` is an
/// answer to the open poll: the poll's current seat while it is collecting, its
/// source, and the resolution that wrote it. A foreign prompt never binds, and a
/// poll that is delivering has no collecting seat.
pub(crate) fn binds(state: &GameState, player: PlayerId, source_id: ObjectId) -> bool {
    state
        .pending_simultaneous_unless
        .as_deref()
        .is_some_and(|carrier| {
            carrier
                .collection()
                .is_some_and(|collection| collection.current_player == player)
                && carrier.ability.source_id == source_id
                && carrier.resolving_entry
                    == state.resolving_stack_entry.as_ref().map(|entry| entry.id)
        })
}

/// CR 118.12a: the current seat's answer to its unless prompt. A decline
/// records an unpaid instance. Choosing to pay a branch opens that branch's
/// pick over its live pool; nothing moves yet (CR 101.4). A branch whose pool
/// emptied since the prompt can't be paid (CR 118.3), so the instance is unpaid.
pub(crate) fn answer_branch(
    state: &mut GameState,
    pay: bool,
    cost: &AbilityCost,
) -> Result<WalkOutcome, EffectError> {
    let (carrier, collection) = open_carrier(state)?;
    if !pay {
        return Ok(record_and_walk(state, SimultaneousUnlessAnswer::Declined));
    }
    let branch = single_choice_payment(cost).ok_or_else(|| {
        EffectError::InvalidParam(
            "CR 118.12a: the chosen unless cost is not a branch of this poll".to_string(),
        )
    })?;
    let seat = collection.current_player;
    let pool = live_pool(state, carrier, collection, seat, branch);
    if pool.is_empty() {
        return Ok(record_and_walk(state, SimultaneousUnlessAnswer::Declined));
    }
    let pending_effect = Box::new(seat_instruction(carrier, seat));
    state.waiting_for = match branch {
        SingleChoicePayment::SacrificeOne { .. } => WaitingFor::WardSacrificeChoice {
            player: seat,
            permanents: pool,
            pending_effect,
            remaining: 1,
            min_total_power: None,
            // A poll pick is answered by `answer_pick`, never by the Ward
            // shortfall settle that reads this snapshot: an unpaid instance
            // is recorded on the poll, not countered.
            trigger_event: None,
        },
        SingleChoicePayment::DiscardOneChosen => WaitingFor::WardDiscardChoice {
            player: seat,
            cards: pool,
            pending_effect,
            remaining: 1,
            filter: None,
        },
    };
    Ok(WalkOutcome::Prompted)
}

/// CR 101.4 + CR 101.4a: the current seat's pick on `prompt`. The pick must be
/// exactly one member of both the prompt's list and the branch's live pool: an
/// object the seat already chose for an earlier instance is not offered again.
/// A sacrifice pick is public (CR 101.4b); a discard pick stays hidden until
/// it is discarded. Nothing moves yet.
pub(crate) fn answer_pick(
    state: &mut GameState,
    prompt: &WaitingFor,
    chosen: &[ObjectId],
) -> Result<WalkOutcome, EffectError> {
    let (carrier, collection) = open_carrier(state)?;
    let seat = collection.current_player;
    let (offered, kind) = match prompt {
        WaitingFor::WardSacrificeChoice { permanents, .. } => (permanents, PickKind::Sacrifice),
        WaitingFor::WardDiscardChoice { cards, .. } => (cards, PickKind::Discard),
        _ => {
            return Err(EffectError::InvalidParam(
                "CR 101.4: not a pick prompt of the open unless poll".to_string(),
            ))
        }
    };
    let [pick] = chosen else {
        return Err(EffectError::InvalidParam(
            "Must select exactly one card to pay this instance".to_string(),
        ));
    };
    let live = single_choice_payment_branches(&carrier.unless_pay.cost)
        .into_iter()
        .flatten()
        .filter(|(_, branch)| kind.matches(*branch))
        .any(|(_, branch)| live_pool(state, carrier, collection, seat, branch).contains(pick));
    if !offered.contains(pick) || !live {
        return Err(EffectError::InvalidParam(
            "CR 101.4: that object is not an available choice for this instance".to_string(),
        ));
    }
    let pin = state
        .objects
        .get(pick)
        .map(ObjectIncarnationRef::from_object)
        .ok_or(EffectError::ObjectNotFound(*pick))?;
    let answer = match kind {
        PickKind::Sacrifice => SimultaneousUnlessAnswer::Sacrifice { permanent: pin },
        PickKind::Discard => {
            // CR 101.4a: the card stays in hand, unrevealed, until delivery.
            if let Some(collection) = state
                .pending_simultaneous_unless
                .as_deref_mut()
                .and_then(PendingSimultaneousUnless::collection_mut)
            {
                collection.hidden_discards.push((seat, pin));
            }
            SimultaneousUnlessAnswer::Discard
        }
    };
    Ok(record_and_walk(state, answer))
}

/// Which pick prompt an answer arrived on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PickKind {
    Sacrifice,
    Discard,
}

impl PickKind {
    fn matches(self, branch: SingleChoicePayment<'_>) -> bool {
        match (self, branch) {
            (Self::Sacrifice, SingleChoicePayment::SacrificeOne { .. })
            | (Self::Discard, SingleChoicePayment::DiscardOneChosen) => true,
            (Self::Sacrifice, SingleChoicePayment::DiscardOneChosen)
            | (Self::Discard, SingleChoicePayment::SacrificeOne { .. }) => false,
        }
    }
}

/// The open poll and its collection; `Err` when no poll is collecting answers
/// (none is open, or it is already delivering).
fn open_carrier(
    state: &GameState,
) -> Result<(&PendingSimultaneousUnless, &SimultaneousUnlessCollection), EffectError> {
    state
        .pending_simultaneous_unless
        .as_deref()
        .and_then(|carrier| carrier.collection().map(|collection| (carrier, collection)))
        .ok_or_else(|| {
            EffectError::InvalidParam("CR 101.4: no unless poll is collecting answers".to_string())
        })
}

/// Record the current seat's answer for its current instance and continue the
/// walk.
fn record_and_walk(state: &mut GameState, answer: SimultaneousUnlessAnswer) -> WalkOutcome {
    if let Some(collection) = state
        .pending_simultaneous_unless
        .as_deref_mut()
        .and_then(PendingSimultaneousUnless::collection_mut)
    {
        collection.answers.push(SimultaneousUnlessSeatAnswer {
            player: collection.current_player,
            answer,
        });
        collection.next_instance += 1;
    }
    walk(state)
}

/// CR 101.4: advance the poll to the next instance that needs a prompt, in
/// APNAP seat order, raising that prompt. The poll's own trigger event is
/// installed while the prompt is built, so a prompt raised from an answer
/// carries the resolution's event like the first one did.
fn walk(state: &mut GameState) -> WalkOutcome {
    let Some(trigger_event) = state
        .pending_simultaneous_unless
        .as_deref()
        .map(|carrier| carrier.trigger_event.clone())
    else {
        return WalkOutcome::Complete;
    };
    let prior = std::mem::replace(&mut state.current_trigger_event, trigger_event);
    let outcome = walk_seats(state);
    state.current_trigger_event = prior;
    outcome
}

fn walk_seats(state: &mut GameState) -> WalkOutcome {
    loop {
        let Some(carrier) = state.pending_simultaneous_unless.as_deref_mut() else {
            return WalkOutcome::Complete;
        };
        let instances = carrier.instances;
        let Some(collection) = carrier.collection_mut() else {
            return WalkOutcome::Complete;
        };
        if collection.next_instance >= instances {
            if collection.remaining_players.is_empty() {
                return WalkOutcome::Complete;
            }
            collection.current_player = collection.remaining_players.remove(0);
            collection.next_instance = 0;
            continue;
        }
        let prompt = {
            let carrier = state
                .pending_simultaneous_unless
                .as_deref()
                .expect("checked above");
            let collection = carrier.collection().expect("checked above");
            seat_prompt(state, carrier, collection)
        };
        match prompt {
            Some(prompt) => {
                state.waiting_for = prompt;
                return WalkOutcome::Prompted;
            }
            // CR 608.2d + CR 118.3: no branch can be paid, so the only
            // option is not to pay: the instance is unpaid without a prompt.
            None => {
                let collection = state
                    .pending_simultaneous_unless
                    .as_deref_mut()
                    .and_then(PendingSimultaneousUnless::collection_mut)
                    .expect("checked above");
                collection.answers.push(SimultaneousUnlessSeatAnswer {
                    player: collection.current_player,
                    answer: SimultaneousUnlessAnswer::Declined,
                });
                collection.next_instance += 1;
            }
        }
    }
}

/// CR 118.12a: the current seat's unless prompt over the branches it can still
/// pay, or `None` when it can pay none. A branch is available when its live
/// pool is non-empty and the generic dispatch's CR 614.17b and spoken-phrase
/// filters admit it. Declining is always offered.
fn seat_prompt(
    state: &GameState,
    carrier: &PendingSimultaneousUnless,
    collection: &SimultaneousUnlessCollection,
) -> Option<WaitingFor> {
    let seat = collection.current_player;
    let bound = seat_instruction(carrier, seat);
    let mut available = available_branches(state, carrier, collection, &bound);
    let cost = match available.len() {
        0 => return None,
        1 => available.remove(0),
        _ => AbilityCost::OneOf { costs: available },
    };
    // The carrier owns seat sequencing, so the prompt names no later payer.
    Some(super::unless_payment_prompt(
        state,
        &bound,
        seat,
        &[],
        cost,
        None,
    ))
}

/// CR 118.3 + CR 118.12a: the branches of the unless cost the current seat can
/// still pay: a non-empty live pool, admitted by the generic dispatch's
/// CR 614.17b and spoken-phrase filters.
fn available_branches(
    state: &GameState,
    carrier: &PendingSimultaneousUnless,
    collection: &SimultaneousUnlessCollection,
    bound: &ResolvedAbility,
) -> Vec<AbilityCost> {
    let seat = collection.current_player;
    single_choice_payment_branches(&carrier.unless_pay.cost)
        .into_iter()
        .flatten()
        .filter(|(cost, branch)| {
            !live_pool(state, carrier, collection, seat, *branch).is_empty()
                && !crate::game::costs::resolution_cost_includes_impossible_event(
                    state, seat, cost, bound,
                )
                && !crate::game::un_actions::cost_requires_unavailable_speech(state, seat, cost)
        })
        .map(|(cost, _)| cost.clone())
        .collect()
}

/// CR 608.2 + CR 109.5: the template bound to `seat`, the acting player of one
/// instance; the printed controller stays the template's original controller.
fn seat_instruction(carrier: &PendingSimultaneousUnless, seat: PlayerId) -> ResolvedAbility {
    super::bind_scoped_seat(&carrier.ability, carrier.ability.controller, seat)
}

/// CR 101.4 + CR 400.7: what `seat` can still choose for `branch`: the branch's
/// eligibility authority minus what `seat` already chose for an earlier
/// instance of this poll (matched by incarnation).
fn live_pool(
    state: &GameState,
    carrier: &PendingSimultaneousUnless,
    collection: &SimultaneousUnlessCollection,
    seat: PlayerId,
    branch: SingleChoicePayment<'_>,
) -> Vec<ObjectId> {
    let source = carrier.ability.source_id;
    match branch {
        SingleChoicePayment::SacrificeOne { target } => {
            let pinned: Vec<&ObjectIncarnationRef> = collection
                .answers
                .iter()
                .filter(|answer| answer.player == seat)
                .filter_map(|answer| match &answer.answer {
                    SimultaneousUnlessAnswer::Sacrifice { permanent } => Some(permanent),
                    SimultaneousUnlessAnswer::Declined | SimultaneousUnlessAnswer::Discard => None,
                })
                .collect();
            crate::game::engine_payment_choices::eligible_unless_sacrifice_permanents(
                state, seat, source, target,
            )
            .into_iter()
            .filter(|id| !pinned.iter().any(|pin| pins(state, pin, *id)))
            .collect()
        }
        SingleChoicePayment::DiscardOneChosen => {
            crate::game::casting::find_eligible_discard_targets(
                state,
                seat,
                source,
                carrier.ability.context.granting_object,
                None,
            )
            .into_iter()
            .filter(|id| {
                !collection
                    .hidden_discards
                    .iter()
                    .any(|(chooser, pin)| *chooser == seat && pins(state, pin, *id))
            })
            .collect()
        }
    }
}

/// CR 400.7: `pin` names the object `id` is now.
fn pins(state: &GameState, pin: &ObjectIncarnationRef, id: ObjectId) -> bool {
    pin.object_id == id && pin.is_current(state)
}

/// CR 400.7 + CR 701.21a: a sacrifice pick is still that object, on the
/// battlefield, controlled by the player who chose it.
fn sacrifice_pin_is_live(state: &GameState, seat: PlayerId, pin: &ObjectIncarnationRef) -> bool {
    pin.is_current(state)
        && state
            .objects
            .get(&pin.object_id)
            .is_some_and(|object| object.zone == Zone::Battlefield && object.controller == seat)
}

/// CR 400.7 + CR 701.9a: a discard pick is still that card, in its chooser's
/// hand.
fn discard_pin_is_live(state: &GameState, seat: PlayerId, pin: &ObjectIncarnationRef) -> bool {
    pin.is_current(state)
        && state
            .objects
            .get(&pin.object_id)
            .is_some_and(|object| object.zone == Zone::Hand && object.owner == seat)
}

/// CR 101.4 + CR 614.1 + CR 608.2f: perform every chosen action of a completed
/// poll together. The carrier has already been taken. Discards happen first,
/// then one unpaid instance per decline, and the sacrifice batch last, each step
/// flat at constant depth. Every effect that exists when the simultaneous action
/// happens applies to all of it, including the effects of permanents sacrificed in
/// it: a replacement effect such as Rest in Peace's (CR 614.1) and a "can't" effect
/// such as Platinum Emperion's (CR 101.2). Inside the sacrifice batch, every
/// member's replacements are consulted before any member moves
/// (`perform_player_scope_sacrifices`). The events of
/// completed steps are held until the whole delivery is done (CR 603.10).
///
/// CR 118.12 + CR 118.12a: "unless" reads the choice, not the event. A pick
/// voided since it was chosen (it left the battlefield or the hand and is a new
/// object, CR 400.7) is not acted on, and its instance still counts as paid:
/// the choice met CR 118.3 when it was made.
pub(crate) fn deliver(
    state: &mut GameState,
    mut carrier: PendingSimultaneousUnless,
    events: &mut Vec<GameEvent>,
    depth: u32,
) -> Result<DeliveryOutcome, EffectError> {
    let SimultaneousUnlessPhase::Collecting(collection) = &carrier.phase else {
        return Err(EffectError::InvalidParam(
            "CR 101.4: a delivery was asked to deliver twice".to_string(),
        ));
    };
    let delivery = begin_delivery(collection);
    carrier.phase = SimultaneousUnlessPhase::Delivering(delivery);
    // Nothing of the delivery is in the buffer yet.
    let span_start = events.len();
    drive_delivery(state, carrier, events, depth, span_start)
}

/// CR 101.4 + CR 614.1: the delivery cursor of a completed collection: every
/// answer in answer order, which is APNAP order.
fn begin_delivery(collection: &SimultaneousUnlessCollection) -> SimultaneousUnlessDelivery {
    let mut unpaid = Vec::new();
    let mut sacrifices = Vec::new();
    for SimultaneousUnlessSeatAnswer { player, answer } in &collection.answers {
        match answer {
            SimultaneousUnlessAnswer::Declined => unpaid.push(*player),
            // Liveness is checked when the batch is performed.
            SimultaneousUnlessAnswer::Sacrifice { permanent } => {
                sacrifices.push((*player, *permanent));
            }
            // The card itself is in `hidden_discards`.
            SimultaneousUnlessAnswer::Discard => {}
        }
    }
    SimultaneousUnlessDelivery {
        discards: collection.hidden_discards.clone(),
        unpaid,
        sacrifices,
        held_events: Vec::new(),
    }
}

/// The single entry that runs a delivery's remaining steps: the start
/// (`deliver`, from an answer's or a departure's settle) and the resume hook
/// (`resume_parked_delivery`). It owns
/// the installation and restoration of the poll's trigger event. `span_start` is
/// the first index of `events` that belongs to this delivery.
fn drive_delivery(
    state: &mut GameState,
    carrier: PendingSimultaneousUnless,
    events: &mut Vec<GameEvent>,
    depth: u32,
    span_start: usize,
) -> Result<DeliveryOutcome, EffectError> {
    let prior = std::mem::replace(
        &mut state.current_trigger_event,
        carrier.trigger_event.clone(),
    );
    let outcome = drive_delivery_steps(state, carrier, events, depth, span_start);
    state.current_trigger_event = prior;
    outcome
}

fn drive_delivery_steps(
    state: &mut GameState,
    mut carrier: PendingSimultaneousUnless,
    events: &mut Vec<GameEvent>,
    depth: u32,
    span_start: usize,
) -> Result<DeliveryOutcome, EffectError> {
    let SimultaneousUnlessPhase::Delivering(delivery) = &mut carrier.phase else {
        return Err(EffectError::InvalidParam(
            "CR 101.4: only a delivering poll has steps to drive".to_string(),
        ));
    };
    let mut delivery = std::mem::take(delivery);
    // Mid-resolution convention: skips the depth-0 top-level reset.
    let step_depth = depth.max(1);
    let template = (*carrier.ability).clone();
    let controller = template.controller;

    // CR 701.9a + CR 118.12a + CR 400.7: discards, one card per step. The
    // discard node is a cost payment, so its discards are not an effect's (no
    // Library-of-Leng-class replacement; the forced-discard muzzle belongs to
    // the choice, #11091). A pick voided since it was chosen is skipped and its
    // instance is still paid.
    while !delivery.discards.is_empty() {
        let (seat, pin) = delivery.discards.remove(0);
        if !crate::game::players::is_alive(state, seat) || !discard_pin_is_live(state, seat, &pin) {
            continue;
        }
        let node = super::scoped_discard::scoped_discard_delivery_node(
            &template,
            vec![TargetRef::Object(pin.object_id)],
            DiscardCause::Cost,
        );
        let continuations_before = ability_continuation_frames(state);
        super::resolve_unpaid_unless_ability_chain(state, &node, events, step_depth)?;
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Ok(park(
                state,
                carrier,
                delivery,
                events,
                span_start,
                continuations_before,
            ));
        }
    }
    // CR 118.12a: unpaid instances, each its own life-loss event.
    while !delivery.unpaid.is_empty() {
        let seat = delivery.unpaid.remove(0);
        // CR 800.4a: a departed seat loses nothing.
        if !crate::game::players::is_alive(state, seat) {
            continue;
        }
        let node = super::bind_scoped_seat(&template, controller, seat);
        let continuations_before = ability_continuation_frames(state);
        super::resolve_unpaid_unless_ability_chain(state, &node, events, step_depth)?;
        if !matches!(state.waiting_for, WaitingFor::Priority { .. }) {
            return Ok(park(
                state,
                carrier,
                delivery,
                events,
                span_start,
                continuations_before,
            ));
        }
    }

    // CR 701.21a + CR 101.4: the sacrifice batch, last. Its sacrifices remove
    // the only permanents the earlier steps could have depended on. The held
    // events seed the batch's own deferred span, so they are released with it.
    let mut held = std::mem::take(&mut delivery.held_events);
    held.extend(events.drain(span_start..));
    let sacrifices = group_live_sacrifices(state, &delivery.sacrifices);
    if sacrifices.is_empty() {
        events.splice(span_start..span_start, held);
        return Ok(DeliveryOutcome::Delivered {
            span: span_start..events.len(),
        });
    }
    let completion = PendingPlayerScopeSacrificeCompletion {
        effect_kind: Some(EffectKind::Sacrifice),
        deferred_events: held,
        ..PendingPlayerScopeSacrificeCompletion::default()
    };
    match super::perform_player_scope_sacrifices(
        state,
        &template,
        sacrifices,
        Some(completion),
        events,
    )? {
        super::PlayerScopeSacrificePerformOutcome::PausedForReplacement => {
            Ok(DeliveryOutcome::Paused)
        }
        super::PlayerScopeSacrificePerformOutcome::Completed { .. } => {
            Ok(DeliveryOutcome::Delivered {
                span: span_start..events.len(),
            })
        }
    }
}

/// CR 400.7 + CR 701.21a + CR 118.12a: the chosen permanents that are still the
/// objects chosen, on the battlefield, under the player who chose them, grouped
/// by consecutive seat. A skipped pick's instance still counts as paid, and a
/// departed seat sacrifices nothing (CR 800.4a).
fn group_live_sacrifices(
    state: &GameState,
    sacrifices: &[(PlayerId, ObjectIncarnationRef)],
) -> Vec<(PlayerId, Vec<ObjectId>)> {
    let mut grouped: Vec<(PlayerId, Vec<ObjectId>)> = Vec::new();
    for (seat, pin) in sacrifices {
        if !crate::game::players::is_alive(state, *seat)
            || !sacrifice_pin_is_live(state, *seat, pin)
        {
            continue;
        }
        match grouped.last_mut() {
            Some((last, chosen)) if last == seat => chosen.push(pin.object_id),
            _ => grouped.push((*seat, vec![pin.object_id])),
        }
    }
    grouped
}

/// How many child ability continuations are parked.
fn ability_continuation_frames(state: &GameState) -> usize {
    state
        .resolution_stack
        .iter()
        .filter(|frame| {
            matches!(
                frame,
                crate::types::resolution::ResolutionFrame::AbilityContinuation(_)
            )
        })
        .count()
}

/// CR 616.1: a step paused on a replacement choice. The carrier, with the
/// remaining steps, goes back into the state, and the delivery's completed
/// events are held with it.
fn park(
    state: &mut GameState,
    mut carrier: PendingSimultaneousUnless,
    mut delivery: SimultaneousUnlessDelivery,
    events: &mut Vec<GameEvent>,
    span_start: usize,
    continuations_before: usize,
) -> DeliveryOutcome {
    // CR 603.10 + CR 101.4: completed steps are part of the one simultaneous
    // action; hold them so no pause-time collector sees them before the
    // delivery's sacrifices have happened.
    delivery.held_events.extend(events.drain(span_start..));
    carrier.phase = SimultaneousUnlessPhase::Delivering(delivery);
    state.pending_simultaneous_unless = Some(Box::new(carrier));
    // Invariants the resume hook's early return relies on (`delivery_may_resume`).
    debug_assert!(
        !matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "a parked delivery's wait is its step's prompt"
    );
    debug_assert_eq!(
        ability_continuation_frames(state),
        continuations_before,
        "a delivery step left a child ability continuation"
    );
    debug_assert!(
        state.resolution_stack.active_discard().is_none(),
        "a delivery step left a discard frame"
    );
    DeliveryOutcome::Paused
}

/// Whether a delivery is parked on a step's prompt.
pub(crate) fn delivery_is_parked(state: &GameState) -> bool {
    state
        .pending_simultaneous_unless
        .as_deref()
        .is_some_and(|carrier| carrier.delivery().is_some())
}

/// CR 101.4 + CR 608.2c: whether the parked step has fully settled, so the
/// delivery's remaining steps may run: the wait is priority and nothing the step
/// started is still in flight.
pub(crate) fn delivery_may_resume(state: &GameState) -> bool {
    delivery_is_parked(state)
        && matches!(state.waiting_for, WaitingFor::Priority { .. })
        && state.pending_replacement.is_none()
        && state.pending_discard_batch.is_none()
        && state.pending_player_scope_sacrifice_choice.is_none()
        && state.active_change_zone_frame().is_none()
        && state.active_post_replacement_drains().is_none()
        && state.resolution_stack.active_discard().is_none()
}

/// CR 101.4 + CR 614.1: run the remaining steps of a parked delivery. The
/// carrier is consumed, so a failed step can never re-run (the departure
/// settle's convention).
pub(crate) fn resume_parked_delivery(state: &mut GameState, events: &mut Vec<GameEvent>) {
    let Some(carrier) = state.pending_simultaneous_unless.take() else {
        return;
    };
    // The resume runs in the action that answered the paused step's prompt, whose
    // buffer begins with that step's own work.
    if let Err(error) = drive_delivery(state, *carrier, events, 1, 0) {
        tracing::error!(%error, "a parked simultaneous unless delivery failed to resume");
        debug_assert!(
            false,
            "a parked simultaneous unless delivery failed to resume: {error}"
        );
    }
}

/// What a departure did to an open poll (`reconcile_after_departure`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SimultaneousUnlessReconciliation {
    /// No carrier, or the open prompt still stands for a living seat (the
    /// carrier may have been pruned).
    Unchanged,
    /// CR 800.4a: the poll's controller left; the resolving ability ceased to
    /// exist and nothing was delivered.
    Retired,
    /// A living seat's prompt was (re)published and is now the wait: the
    /// departed chooser's turn passed to it, or the departure staled the open
    /// prompt of a chooser still in the game.
    Prompted(PlayerId),
    /// Collection is complete and the caller delivers it.
    Settle,
    /// CR 800.4f: departed seats' remaining delivery entries were dropped; the
    /// paused step's prompt still stands for a chooser in the game.
    DeliveryParked,
    /// CR 800.4a: the delivery's controller left; the resolving ability ceased to
    /// exist, so its remaining steps don't happen. The held events (which did
    /// happen) were appended to `events` from `released_from`.
    DeliveryRetired { released_from: usize },
}

/// CR 800.4a + CR 800.4f + CR 101.4: reconcile an open poll with the players
/// still in the game. A departed seat pays nothing and chooses nothing: its
/// remaining turn, its answers and its hidden picks are dropped. A departed
/// controller's resolving ability ceases to exist.
pub(crate) fn reconcile_after_departure(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> SimultaneousUnlessReconciliation {
    let Some(carrier) = state.pending_simultaneous_unless.as_deref() else {
        return SimultaneousUnlessReconciliation::Unchanged;
    };
    let controller = carrier.ability.controller;
    if carrier.delivery().is_some() {
        return reconcile_delivery_after_departure(state, events);
    }
    if !crate::game::players::is_alive(state, controller) {
        retire_after_controller_left(state);
        return SimultaneousUnlessReconciliation::Retired;
    }
    let mut carrier = state
        .pending_simultaneous_unless
        .take()
        .expect("checked above");
    let instances = carrier.instances;
    let alive = |player: PlayerId| crate::game::players::is_alive(state, player);
    let (chooser, chooser_alive) = {
        let collection = carrier
            .collection_mut()
            .expect("a poll that is not delivering is collecting");
        collection.remaining_players.retain(|player| alive(*player));
        collection.answers.retain(|answer| alive(answer.player));
        collection
            .hidden_discards
            .retain(|(chooser, _)| alive(*chooser));
        let chooser = collection.current_player;
        let chooser_alive = alive(chooser);
        if !chooser_alive {
            // CR 800.4f: the departed chooser answers no further instance.
            collection.next_instance = instances;
        }
        (chooser, chooser_alive)
    };
    state.pending_simultaneous_unless = Some(carrier);
    if chooser_alive {
        return refresh_live_prompt(state).unwrap_or(SimultaneousUnlessReconciliation::Unchanged);
    }
    walk_after_departure(state, chooser)
}

/// CR 800.4a + CR 800.4f + CR 101.4: reconcile a delivery parked on a step's
/// prompt with the players still in the game.
fn reconcile_delivery_after_departure(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> SimultaneousUnlessReconciliation {
    let mut carrier = state
        .pending_simultaneous_unless
        .take()
        .expect("a parked delivery");
    let controller = carrier.ability.controller;
    // CR 800.4a: a paused step whose chooser left never happens: a discard
    // step's card left the game with its owner and was not discarded
    // (CR 701.9a), and a departed seat loses no life. The departure's
    // replacement settlement (`engine_replacement::settle_replacement_after_departure`)
    // runs before this reconciliation: it abandons that step's replacement
    // choice, drains a discard step's batch and resumes this delivery itself,
    // so a delivery that reaches here is parked on a living chooser's prompt.
    if !crate::game::players::is_alive(state, controller) {
        // CR 800.4a: the resolving ability ceased to exist, so the remaining
        // steps don't happen; the events that did happen are released.
        let released_from = events.len();
        if let Some(delivery) = carrier.delivery_mut() {
            events.append(&mut delivery.held_events);
        }
        state.pending_simultaneous_unless = Some(carrier);
        retire_after_controller_left(state);
        return SimultaneousUnlessReconciliation::DeliveryRetired { released_from };
    }
    let alive = |player: PlayerId| crate::game::players::is_alive(state, player);
    if let Some(delivery) = carrier.delivery_mut() {
        delivery.discards.retain(|(seat, _)| alive(*seat));
        delivery.unpaid.retain(|seat| alive(*seat));
        delivery.sacrifices.retain(|(seat, _)| alive(*seat));
    }
    state.pending_simultaneous_unless = Some(carrier);
    SimultaneousUnlessReconciliation::DeliveryParked
}

/// Continue the walk after a departure changed the poll and report where it
/// left the poll.
fn walk_after_departure(
    state: &mut GameState,
    fallback: PlayerId,
) -> SimultaneousUnlessReconciliation {
    match walk(state) {
        WalkOutcome::Prompted => SimultaneousUnlessReconciliation::Prompted(
            state
                .pending_simultaneous_unless
                .as_deref()
                .and_then(PendingSimultaneousUnless::collection)
                .map_or(fallback, |collection| collection.current_player),
        ),
        WalkOutcome::Complete => SimultaneousUnlessReconciliation::Settle,
    }
}

/// CR 800.4a + CR 118.3 + CR 608.2d: a departure can remove objects from a
/// surviving chooser's pool while its prompt is open (a creature the chooser
/// controls but the leaver owns leaves the game), or return objects to it (a
/// leaver's control effect ends). The open prompt must offer exactly what the
/// chooser can still choose: a pick prompt whose every listed object is gone
/// could never be answered, and the poll would never release its resolution.
/// `None` when the prompt still matches the live board (or is not the poll's).
fn refresh_live_prompt(state: &mut GameState) -> Option<SimultaneousUnlessReconciliation> {
    let carrier = state.pending_simultaneous_unless.as_deref()?;
    let collection = carrier.collection()?;
    if !carrier_prompt_is_live(state, carrier) {
        return None;
    }
    let chooser = collection.current_player;
    let bound = seat_instruction(carrier, chooser);
    let stale = match &state.waiting_for {
        WaitingFor::WardSacrificeChoice { permanents, .. } => {
            let live = pick_pool(state, carrier, collection, PickKind::Sacrifice);
            (!same_members(permanents, &live)).then_some(StalePrompt::Pick(live))
        }
        WaitingFor::WardDiscardChoice { cards, .. } => {
            let live = pick_pool(state, carrier, collection, PickKind::Discard);
            (!same_members(cards, &live)).then_some(StalePrompt::Pick(live))
        }
        WaitingFor::UnlessPayment { cost, .. } => {
            let available = available_branches(state, carrier, collection, &bound);
            (!same_members(std::slice::from_ref(cost), &available)).then_some(StalePrompt::Branches)
        }
        WaitingFor::UnlessPaymentChooseCost { costs, .. } => {
            let available = available_branches(state, carrier, collection, &bound);
            (!same_members(costs, &available)).then_some(StalePrompt::Branches)
        }
        _ => None,
    }?;
    match stale {
        StalePrompt::Pick(live) if !live.is_empty() => {
            if let WaitingFor::WardSacrificeChoice {
                permanents: offered,
                ..
            }
            | WaitingFor::WardDiscardChoice { cards: offered, .. } = &mut state.waiting_for
            {
                *offered = live;
            }
            Some(SimultaneousUnlessReconciliation::Prompted(chooser))
        }
        // An emptied pick: the chooser's "pay this branch" was never recorded,
        // so the same instance is asked again over what remains: another
        // branch, or no prompt at all when none can be paid (CR 608.2d).
        // A changed branch set: the same instance is asked again over the
        // branches that can still be paid.
        StalePrompt::Pick(_) | StalePrompt::Branches => Some(walk_after_departure(state, chooser)),
    }
}

/// How a departure staled the poll's open prompt.
enum StalePrompt {
    /// The pick prompt's list differs from what the chooser can still pick.
    Pick(Vec<ObjectId>),
    /// The unless prompt offers a different set of branches than the chooser
    /// can still pay.
    Branches,
}

/// CR 101.4: everything the current seat can still pick on a pick prompt of
/// `kind`: the live pools of every branch of that kind, in order, without
/// repeats.
fn pick_pool(
    state: &GameState,
    carrier: &PendingSimultaneousUnless,
    collection: &SimultaneousUnlessCollection,
    kind: PickKind,
) -> Vec<ObjectId> {
    let seat = collection.current_player;
    let mut pool: Vec<ObjectId> = Vec::new();
    for (_, branch) in single_choice_payment_branches(&carrier.unless_pay.cost)
        .into_iter()
        .flatten()
        .filter(|(_, branch)| kind.matches(*branch))
    {
        for id in live_pool(state, carrier, collection, seat, branch) {
            if !pool.contains(&id) {
                pool.push(id);
            }
        }
    }
    pool
}

/// Whether two lists hold the same members, in any order.
fn same_members<T: PartialEq>(left: &[T], right: &[T]) -> bool {
    left.len() == right.len()
        && left.iter().all(|item| right.contains(item))
        && right.iter().all(|item| left.contains(item))
}

/// CR 800.4a: "if that player controlled any objects on the stack not
/// represented by cards, those objects cease to exist." The poll's resolving
/// ability is such an object. Frames first, as the source-bound abandonment
/// does: nothing is delivered and every pick stays where it is.
fn retire_after_controller_left(state: &mut GameState) {
    let Some(carrier) = state.pending_simultaneous_unless.take() else {
        return;
    };
    let prompt_is_live = carrier_prompt_is_live(state, &carrier);
    // When the binding no longer holds, the departure sweep already retired the
    // resolution; only the carrier is dropped.
    if carrier.resolving_entry == state.resolving_stack_entry.as_ref().map(|entry| entry.id) {
        crate::game::stack::abandon_active_resolution_carrier(
            state,
            crate::game::lifecycle::DelayedTerminalDisposition::Eliminated,
        );
    }
    if prompt_is_live {
        // CR 117.3b + CR 800.4j: once the resolving ability has ceased to exist,
        // the active player receives priority, or the next player in turn order
        // still in the game if the active player has left.
        let seat = crate::game::engine::living_priority_seat(state);
        state.waiting_for = WaitingFor::Priority { player: seat };
        state.priority_player = seat;
    }
}

/// Whether the live wait is the collecting poll's own prompt. A delivering poll
/// owns no prompt of its own: its wait is a replacement choice.
fn carrier_prompt_is_live(state: &GameState, carrier: &PendingSimultaneousUnless) -> bool {
    let Some(collection) = carrier.collection() else {
        return false;
    };
    match &state.waiting_for {
        WaitingFor::UnlessPayment {
            player,
            pending_effect,
            ..
        }
        | WaitingFor::UnlessPaymentChooseCost {
            player,
            pending_effect,
            ..
        }
        | WaitingFor::WardSacrificeChoice {
            player,
            pending_effect,
            ..
        }
        | WaitingFor::WardDiscardChoice {
            player,
            pending_effect,
            ..
        } => {
            *player == collection.current_player
                && pending_effect.source_id == carrier.ability.source_id
        }
        _ => false,
    }
}

//! CR 101.4 + CR 118.12a: the APNAP poll for a player-scoped "[instruction]
//! unless [that player] [sacrifices / discards ...]" (Hag of Ceaseless Torment,
//! Bellowing Mauler, Ob Nixilis's +1). A counted repeat of such an instruction
//! either runs round by round, each round opening this poll (Torment of
//! Hailfire's "Repeat the following process X times", whose repetitions are
//! done separately), or, when it is a for-each instance set (Rottenmouth
//! Viper's "for each blight counter on it"), is polled across all of its
//! instances by `effects::simultaneous_unless`, which admits it before the
//! `player_scope` driver that opens this poll is reached.
//!
//! CR 118.12a reads the instruction as "[that player] may [pay]. If they don't,
//! [instruction]". With several affected players, CR 101.4 has each of them
//! choose in APNAP order — each knowing the choices made before them
//! (CR 101.4b) — and only then do the actions happen, simultaneously.
//!
//! So the poll asks every seat first, through the same per-seat prompts the
//! single-payer unless path uses (`unless_payment_prompt`, then the
//! `WardSacrificeChoice` / `WardDiscardChoice` selection that
//! `prepare_unless_selection_payment` prepares), and records each answer
//! without moving anything. Once the last seat has answered it settles:
//!
//! 1. the chosen discards, as cost discards (CR 118.12 + CR 701.9a), each
//!    through the replacement pipeline, resumable after a CR 616.1 choice;
//! 2. the chosen sacrifices, as one collected batch through the shared
//!    simultaneous-sacrifice machinery
//!    (`perform_collected_player_scope_sacrifices_with_completion`);
//! 3. the instruction itself for the players who declined, by re-entering the
//!    `player_scope` driver with the scope narrowed to exactly those players
//!    and the unless cost removed. The driver therefore owns the instruction's
//!    own choices (a sacrifice "of their choice") and resolves the
//!    instructions printed after the clause exactly once (CR 608.2c), whoever
//!    paid.
//!
//! Admission is fenced to the shapes this settlement supports: a cost whose
//! payment is a choice of objects (a sacrifice, or a chosen discard, or a
//! choice among such costs) and a chain that nowhere reads whether a seat paid.
//! Every other scoped unless keeps the per-seat route.
//!
//! The aggregate token form ("for each opponent, you create a token unless
//! they sacrifice a creature", Acererak the Archlich) keeps its own poll
//! (`begin_player_scope_token_unless_sacrifice`), because its decliners owe one
//! token batch rather than one instruction each, but it settles through
//! [`settle_token_unless_sacrifice`]: the same collected sacrifice batch,
//! followed by the tokens.

use crate::game::engine::EngineError;
use crate::game::engine_payment_choices::UnlessSelectionPrep;
use crate::game::engine_resolution_choices::DepartureSettlement;
use crate::game::game_object::ObjectIncarnationRefExt;
use crate::types::ability::{
    AbilityCost, PlayerFilter, ResolvedAbility, SacrificeRequirement, TargetFilter,
};
use crate::types::events::GameEvent;
use crate::types::game_state::{
    GameState, PendingCostMoveResume, PendingPlayerScopeUnlessPunishment, PendingSingleDiscard,
    PlayerScopeUnlessAction, PlayerScopeUnlessSelection, PlayerScopeUnlessSettlement, WaitingFor,
};
use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
use crate::types::player::PlayerId;
use crate::types::zones::Zone;

use super::effects;

/// CR 118.12a: whether paying `cost` is a choice of objects the poll can record
/// now and perform at settlement. Exhaustive so a new cost shape needs a
/// deliberate decision.
fn payment_is_deferred_selection(cost: &AbilityCost) -> bool {
    match cost {
        // CR 701.21a: the payer chooses which permanents to sacrifice.
        AbilityCost::Sacrifice(sacrifice) => match sacrifice.requirement {
            SacrificeRequirement::Count { .. } | SacrificeRequirement::Aggregate { .. } => true,
            // No unless-cost producer emits "sacrifice all"; the per-seat path
            // fails it closed.
            SacrificeRequirement::All => false,
        },
        // CR 701.9a + CR 701.9b: a chosen discard is a choice; a random one is
        // not (the game picks at payment time).
        AbilityCost::Discard { selection, .. } => selection.is_chosen(),
        // CR 118.12a: a choice among costs is recorded only when every branch is.
        AbilityCost::OneOf { costs } => {
            !costs.is_empty() && costs.iter().all(payment_is_deferred_selection)
        }
        // Paid inline by the per-seat route (mana, life, energy, counters, …),
        // expanded per counter (CR 702.24a), or not an unless cost at all.
        AbilityCost::Mana { .. }
        | AbilityCost::ManaDynamic { .. }
        | AbilityCost::Tap
        | AbilityCost::Untap
        | AbilityCost::Loyalty { .. }
        | AbilityCost::SayPhrase { .. }
        | AbilityCost::PayLife { .. }
        | AbilityCost::Exile { .. }
        | AbilityCost::ExileMaterials { .. }
        | AbilityCost::CollectEvidence { .. }
        | AbilityCost::ExileWithAggregate { .. }
        | AbilityCost::TapCreatures { .. }
        | AbilityCost::RemoveCounter { .. }
        | AbilityCost::PayEnergy { .. }
        | AbilityCost::PaySpeed { .. }
        | AbilityCost::ReturnToHand { .. }
        | AbilityCost::Unattach
        | AbilityCost::UnattachFrom { .. }
        | AbilityCost::Mill { .. }
        | AbilityCost::Exert
        | AbilityCost::Blight { .. }
        | AbilityCost::Reveal { .. }
        | AbilityCost::Behold { .. }
        | AbilityCost::Composite { .. }
        | AbilityCost::Waterbend { .. }
        | AbilityCost::NinjutsuFamily { .. }
        | AbilityCost::EffectCost { .. }
        | AbilityCost::PerCounter { .. }
        | AbilityCost::KeywordCostOfCastSpell { .. }
        | AbilityCost::GetPlayerCounters { .. }
        | AbilityCost::Unimplemented { .. } => false,
    }
}

/// CR 118.12a: "If they do / don't" riders read one seat's payment, which a
/// single settlement for every seat cannot answer per seat.
fn chain_reads_payment_outcome(ability: &ResolvedAbility) -> bool {
    ability
        .condition
        .as_ref()
        .is_some_and(crate::game::engine_payment_choices::payment_outcome_condition)
        || ability
            .sub_ability
            .as_deref()
            .is_some_and(chain_reads_payment_outcome)
        || ability
            .else_ability
            .as_deref()
            .is_some_and(chain_reads_payment_outcome)
}

/// Whether the poll can own `ability`'s scoped unless instruction.
fn admits(ability: &ResolvedAbility) -> bool {
    let Some(modifier) = ability.unless_pay.as_ref() else {
        return false;
    };
    // CR 118.12a: the payer is the player the scope iterates over.
    matches!(modifier.payer, TargetFilter::ScopedPlayer)
        && payment_is_deferred_selection(&modifier.cost)
        // The settlement narrows the scope through `chosen_players`; an
        // instruction that already chose players keeps the per-seat route.
        && ability.chosen_players.is_empty()
        // The head's own gate was read before the driver; re-entering it at
        // settlement must not read it again against a changed board.
        && ability.condition.is_none()
        && ability.else_ability.is_none()
        && !ability.optional
        && !chain_reads_payment_outcome(ability)
}

/// CR 101.4 + CR 118.12a: open the poll for `ability`, whose scope covers
/// `players` in APNAP order. Returns false — leaving the per-seat route in
/// charge — when the instruction is not one the poll supports.
pub(crate) fn begin(
    state: &mut GameState,
    ability: &ResolvedAbility,
    players: &[PlayerId],
) -> bool {
    if !admits(ability) {
        return false;
    }
    let Some((&first, rest)) = players.split_first() else {
        return false;
    };
    let poll = Box::new(PendingPlayerScopeUnlessPunishment {
        ability: Box::new(ability.clone()),
        current_player: first,
        remaining_players: rest.to_vec(),
        declining_players: Vec::new(),
        payments: Vec::new(),
        current_picks: Vec::new(),
        trigger_event: state.current_trigger_event.clone(),
    });
    state.waiting_for = seat_prompt(state, &poll, first);
    state.pending_player_scope_unless_punishment = Some(poll);
    true
}

/// CR 118.12a + CR 608.2c: `seat`'s own unless prompt, built by the same
/// authority the per-seat route uses (a choice among costs first, CR 118.12a).
fn seat_prompt(
    state: &GameState,
    poll: &PendingPlayerScopeUnlessPunishment,
    seat: PlayerId,
) -> WaitingFor {
    let bound = effects::bind_scoped_seat(&poll.ability, poll.ability.controller, seat);
    let modifier = bound
        .unless_pay
        .as_ref()
        .expect("an admitted poll keeps its unless cost");
    let (cost, repeat) = effects::resolved_unless_cost(state, &bound, &modifier.cost);
    effects::unless_payment_prompt(state, &bound, seat, &[], cost, repeat)
}

/// Whether `player`'s prompt about `source_id` is the poll's live question.
pub(crate) fn owns_prompt(state: &GameState, player: PlayerId, source_id: ObjectId) -> bool {
    state
        .pending_player_scope_unless_punishment
        .as_deref()
        .is_some_and(|poll| poll.current_player == player && poll.ability.source_id == source_id)
}

/// CR 118.12a: `player` answered the poll's unless prompt with `cost` (the
/// branch they chose, if the cost was a choice among costs). Records a decline,
/// or surfaces the selection the payment asks for. Nothing moves.
pub(crate) fn answer(
    state: &mut GameState,
    player: PlayerId,
    cost: &AbilityCost,
    pay: bool,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    if !pay {
        return record_decline(state, events);
    }
    let poll = state
        .pending_player_scope_unless_punishment
        .as_deref()
        .expect("the poll is live while its prompt is answered");
    let pending_effect = effects::bind_scoped_seat(&poll.ability, poll.ability.controller, player);
    let trigger_event = poll.trigger_event.clone();
    // CR 614.17b: a player can't choose to pay a cost that includes an event
    // that can't happen.
    if crate::game::costs::resolution_cost_includes_impossible_event(
        state,
        player,
        cost,
        &pending_effect,
    ) {
        return Err(EngineError::InvalidAction(
            "CR 614.17b: a player can't choose to pay a cost that includes an event that can't happen"
                .to_string(),
        ));
    }
    // CR 118.12a: `seat_prompt` already expanded any `OneOf` via
    // `UnlessPaymentChooseCost` before re-entering here with the chosen single
    // cost. Preparation goes through the shared selection-unless authority so
    // count/aggregate/discard rules stay identical across both paths; it
    // rejects any cost that is not a choice of objects.
    match crate::game::engine_payment_choices::prepare_unless_selection_payment(
        state,
        player,
        cost,
        &pending_effect,
        trigger_event.as_ref(),
    )? {
        UnlessSelectionPrep::Prompt(wf) => {
            state.waiting_for = *wf;
            Ok(state.waiting_for.clone())
        }
        UnlessSelectionPrep::ZeroDiscard => {
            commit_payment(state, PlayerScopeUnlessAction::Discard, Vec::new(), events)
        }
        UnlessSelectionPrep::Unpayable => record_decline(state, events),
        // `payment_is_deferred_selection` keeps random discards out of the poll.
        UnlessSelectionPrep::RandomDiscard { .. } => Err(EngineError::InvalidAction(
            "random discard is not admitted by the player-scope unless poll".to_string(),
        )),
    }
}

fn record_decline(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut poll = state
        .pending_player_scope_unless_punishment
        .take()
        .expect("the poll is live while its prompt is answered");
    poll.declining_players.push(poll.current_player);
    advance(state, poll, events)
}

/// CR 101.4: one permanent picked for the current seat's sacrifice payment.
/// Asks for the next pick while the cost wants more; records the payment once
/// it is complete.
pub(crate) fn record_sacrifice_pick(
    state: &mut GameState,
    player: PlayerId,
    chosen: ObjectId,
    permanents: Vec<ObjectId>,
    remaining: u32,
    pending_effect: Box<ResolvedAbility>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    push_current_pick(state, chosen);
    if remaining > 1 {
        let eligible = permanents.into_iter().filter(|id| *id != chosen).collect();
        state.waiting_for = WaitingFor::WardSacrificeChoice {
            player,
            permanents: eligible,
            pending_effect,
            remaining: remaining - 1,
            min_total_power: None,
            trigger_event: state
                .pending_player_scope_unless_punishment
                .as_deref()
                .and_then(|poll| poll.trigger_event.clone()),
        };
        return Ok(state.waiting_for.clone());
    }
    let picks = take_current_picks(state);
    commit_payment(state, PlayerScopeUnlessAction::Sacrifice, picks, events)
}

/// CR 101.4 + CR 118.12a: the whole set of permanents chosen at once for an
/// aggregate sacrifice payment ("creatures with total power N or greater"),
/// already validated by the selection's handler.
pub(crate) fn record_sacrifice_set(
    state: &mut GameState,
    chosen: &[ObjectId],
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let picks = pins(state, chosen);
    commit_payment(state, PlayerScopeUnlessAction::Sacrifice, picks, events)
}

/// CR 101.4: one card picked for the current seat's discard payment. Asks for
/// the next pick, from the hand less what is already picked, while the cost
/// wants more ("discard two cards"); records the payment once it is complete.
pub(crate) fn record_discard_pick(
    state: &mut GameState,
    player: PlayerId,
    chosen: ObjectId,
    remaining: u32,
    filter: Option<TargetFilter>,
    pending_effect: Box<ResolvedAbility>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    push_current_pick(state, chosen);
    if remaining > 1 {
        let picked: Vec<ObjectId> = state
            .pending_player_scope_unless_punishment
            .as_deref()
            .map(|poll| poll.current_picks.iter().map(|pin| pin.object_id).collect())
            .unwrap_or_default();
        let cards = crate::game::casting::find_eligible_discard_targets(
            state,
            player,
            pending_effect.source_id,
            pending_effect.context.granting_object,
            filter.as_ref(),
        )
        .into_iter()
        .filter(|id| !picked.contains(id))
        .collect();
        state.waiting_for = WaitingFor::WardDiscardChoice {
            player,
            cards,
            pending_effect,
            remaining: remaining - 1,
            filter,
        };
        return Ok(state.waiting_for.clone());
    }
    let picks = take_current_picks(state);
    commit_payment(state, PlayerScopeUnlessAction::Discard, picks, events)
}

/// CR 400.7: pin each chosen object to the occurrence it is now.
fn pins(state: &GameState, ids: &[ObjectId]) -> Vec<ObjectIncarnationRef> {
    ids.iter()
        .filter_map(|id| state.objects.get(id))
        .map(ObjectIncarnationRef::from_object)
        .collect()
}

fn push_current_pick(state: &mut GameState, chosen: ObjectId) {
    let pin = pins(state, &[chosen]);
    if let Some(poll) = state.pending_player_scope_unless_punishment.as_deref_mut() {
        poll.current_picks.extend(pin);
    }
}

fn take_current_picks(state: &mut GameState) -> Vec<ObjectIncarnationRef> {
    state
        .pending_player_scope_unless_punishment
        .as_deref_mut()
        .map(|poll| std::mem::take(&mut poll.current_picks))
        .unwrap_or_default()
}

fn commit_payment(
    state: &mut GameState,
    action: PlayerScopeUnlessAction,
    objects: Vec<ObjectIncarnationRef>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let mut poll = state
        .pending_player_scope_unless_punishment
        .take()
        .expect("the poll is live while its payment is chosen");
    // CR 118.12a: the seat paid; the instruction does not apply to them.
    poll.payments.push(PlayerScopeUnlessSelection {
        player: poll.current_player,
        action,
        objects,
    });
    advance(state, poll, events)
}

/// CR 101.4: ask the next seat still in the game, or settle once every seat
/// has answered.
fn advance(
    state: &mut GameState,
    mut poll: Box<PendingPlayerScopeUnlessPunishment>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    poll.current_picks.clear();
    while !poll.remaining_players.is_empty() {
        let next = poll.remaining_players.remove(0);
        // CR 800.4a: a player who left the game makes no further choice.
        if !crate::game::players::is_alive(state, next) {
            continue;
        }
        poll.current_player = next;
        state.waiting_for = seat_prompt(state, &poll, next);
        state.pending_player_scope_unless_punishment = Some(poll);
        return Ok(state.waiting_for.clone());
    }
    settle(state, *poll, events)
}

/// CR 101.4: the instruction for the players who declined — the scoped
/// ability without its unless cost, its scope narrowed to exactly those
/// players, still carrying every instruction printed after it.
fn instruction_for_decliners(poll: &PendingPlayerScopeUnlessPunishment) -> Box<ResolvedAbility> {
    let mut instruction = (*poll.ability).clone();
    instruction.unless_pay = None;
    instruction.player_scope = Some(PlayerFilter::AnyOf {
        filters: (0..poll.declining_players.len())
            .map(|index| PlayerFilter::ChosenPlayer {
                index: u8::try_from(index).expect("a game seats fewer than 256 players"),
            })
            .collect(),
    });
    instruction.chosen_players = poll.declining_players.clone();
    Box::new(instruction)
}

/// CR 101.4: every seat has answered; the chosen payments and the instruction
/// for the decliners now happen.
fn settle(
    state: &mut GameState,
    poll: PendingPlayerScopeUnlessPunishment,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // The last seat's prompt belongs only to its own question.
    crate::game::engine_payment_choices::set_active_priority(state);
    let punishment = instruction_for_decliners(&poll);
    let mut discards = Vec::new();
    let mut sacrifices = Vec::new();
    for payment in poll.payments {
        match payment.action {
            PlayerScopeUnlessAction::Discard => discards.extend(
                payment
                    .objects
                    .into_iter()
                    .map(|object| (payment.player, object)),
            ),
            PlayerScopeUnlessAction::Sacrifice => {
                sacrifices.push((payment.player, payment.objects));
            }
        }
    }
    run_settlement(
        state,
        PlayerScopeUnlessSettlement {
            source_id: poll.ability.source_id,
            controller: poll.ability.controller,
            discards,
            sacrifices,
            punishment: Some(punishment),
            trigger_event: poll.trigger_event,
        },
        events,
    )
}

/// CR 101.4 + CR 118.12a + CR 111.2: settle the aggregate "for each opponent,
/// you create a token unless they sacrifice ..." poll
/// (`begin_player_scope_token_unless_sacrifice`) through the same settlement
/// as this poll: the paying seats' chosen permanents are sacrificed as one
/// batch, then the one token batch owed for the decliners (if any) is created.
pub(crate) fn settle_token_unless_sacrifice(
    state: &mut GameState,
    source_id: ObjectId,
    controller: PlayerId,
    sacrifices: Vec<(PlayerId, Vec<ObjectIncarnationRef>)>,
    tokens: Option<Box<ResolvedAbility>>,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    crate::game::engine_payment_choices::set_active_priority(state);
    let trigger_event = state.current_trigger_event.clone();
    run_settlement(
        state,
        PlayerScopeUnlessSettlement {
            source_id,
            controller,
            discards: Vec::new(),
            sacrifices,
            punishment: tokens,
            trigger_event,
        },
        events,
    )
}

/// CR 400.7: pin `chosen` to the permanent it is now, for a sacrifice recorded
/// before the poll settles.
pub(crate) fn pin(state: &GameState, chosen: ObjectId) -> Vec<ObjectIncarnationRef> {
    pins(state, &[chosen])
}

/// CR 118.12a + CR 616.1: continue a settlement whose cost discard paused on a
/// replacement choice. Reached only through the typed cost-move dispatcher.
pub(crate) fn resume_settlement(
    state: &mut GameState,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let Some(PendingCostMoveResume::PlayerScopeUnlessSettlement(settlement)) =
        state.pending_cost_move_resume.take()
    else {
        unreachable!("a player-scope unless settlement resume requires its typed continuation")
    };
    run_settlement(state, *settlement, events)
}

/// CR 603.2: the settlement runs outside the resolution step that opened the
/// poll, so the trigger's event is restored around it.
fn run_settlement(
    state: &mut GameState,
    settlement: PlayerScopeUnlessSettlement,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    let previous_trigger_event = std::mem::replace(
        &mut state.current_trigger_event,
        settlement.trigger_event.clone(),
    );
    let result = run_settlement_steps(state, settlement, events);
    state.current_trigger_event = previous_trigger_event;
    result
}

fn run_settlement_steps(
    state: &mut GameState,
    mut settlement: PlayerScopeUnlessSettlement,
    events: &mut Vec<GameEvent>,
) -> Result<WaitingFor, EngineError> {
    // CR 118.12 + CR 701.9a: each chosen discard is the payment of a cost, so
    // discard-level replacements (madness, CR 702.35a) apply and effect-caused
    // ones (Library of Leng) do not.
    while !settlement.discards.is_empty() {
        let (player, pin) = settlement.discards.remove(0);
        // CR 400.7: a pick that left its hand is a new object; nothing is discarded.
        let in_hand = pin.is_current(state)
            && state
                .objects
                .get(&pin.object_id)
                .is_some_and(|card| card.zone == Zone::Hand && card.owner == player);
        if !in_hand {
            continue;
        }
        if let effects::discard::DiscardOutcome::NeedsReplacementChoice(choice_player) =
            effects::discard::discard_as_cost_with_source(
                state,
                pin.object_id,
                player,
                Some(settlement.source_id),
                events,
            )
        {
            // CR 616.1: the paused card's discard is recorded when its move
            // resumes; the rest of the settlement waits for that answer.
            state.pending_single_discard = Some(Box::new(PendingSingleDiscard {
                paused_card: pin,
                player,
                source_id: Some(settlement.source_id),
            }));
            state.pending_cost_move_resume = Some(
                PendingCostMoveResume::PlayerScopeUnlessSettlement(Box::new(settlement)),
            );
            state.waiting_for =
                crate::game::replacement::replacement_choice_waiting_for(choice_player, state);
            return Ok(state.waiting_for.clone());
        }
    }

    // CR 101.4 + CR 701.21a: the chosen permanents are sacrificed as one
    // simultaneous batch. CR 400.7: a pick that left the battlefield, or whose
    // chooser no longer controls it, is not sacrificed.
    let selections: Vec<(PlayerId, Vec<ObjectId>)> = settlement
        .sacrifices
        .iter()
        .filter_map(|(player, picks)| {
            let ids: Vec<ObjectId> = picks
                .iter()
                .filter(|pin| {
                    pin.is_current(state)
                        && state.objects.get(&pin.object_id).is_some_and(|permanent| {
                            permanent.zone == Zone::Battlefield && permanent.controller == *player
                        })
                })
                .map(|pin| pin.object_id)
                .collect();
            (!ids.is_empty()).then_some((*player, ids))
        })
        .collect();
    if !selections.is_empty() {
        match effects::perform_collected_player_scope_sacrifices_with_completion(
            state,
            settlement.source_id,
            settlement.controller,
            selections,
            Default::default(),
            events,
        )
        .map_err(|error| EngineError::InvalidAction(error.to_string()))?
        {
            effects::PendingPlayerScopeSacrificeOutcome::Completed { .. } => {}
            // CR 616.1: the batch resumes after the replacement choice and then
            // drains the continuation, which now begins with the instruction
            // for the decliners.
            effects::PendingPlayerScopeSacrificeOutcome::PausedForReplacement => {
                if let Some(punishment) = settlement.punishment {
                    effects::prepend_to_pending_continuation(state, *punishment);
                }
                return Ok(state.waiting_for.clone());
            }
            effects::PendingPlayerScopeSacrificeOutcome::WaitingForNextChoice => {
                unreachable!("a collected sacrifice batch has no further choice to ask")
            }
        }
    }

    // CR 101.4 + CR 608.2c: the instruction applies to the decliners, and the
    // instructions printed after it follow once.
    if let Some(punishment) = settlement.punishment.as_deref() {
        effects::resolve_ability_chain(state, punishment, events, 0)
            .map_err(|error| EngineError::InvalidAction(format!("{error:?}")))?;
    }
    if matches!(state.waiting_for, WaitingFor::Priority { .. }) {
        crate::game::engine::resume_pending_continuation_if_priority(state, events)?;
    }
    Ok(state.waiting_for.clone())
}

/// Whether the live prompt is one of the poll's own per-seat questions.
fn prompt_is_live(state: &GameState, poll: &PendingPlayerScopeUnlessPunishment) -> bool {
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
        } => *player == poll.current_player && pending_effect.source_id == poll.ability.source_id,
        _ => false,
    }
}

/// CR 800.4a + CR 101.4 + CR 608.2m: reconcile an open poll with the players
/// who just left the game. A departed player makes no further choice and pays
/// nothing (CR 800.4f); an earlier answer by a player still in the game stands,
/// and the next surviving player is asked, or the poll settles.
///
/// The instruction's controller leaving does not end the poll: the ability had
/// already started to resolve, so it continues to resolve fully (CR 608.2m).
/// Its controller is reconciled like any other affected player, and what the
/// instruction would still do for them is governed by the rules for objects
/// tied to a departed player (CR 800.4b: no token is created for them).
///
/// Reports whether the departure settled the poll, so the elimination leaves a
/// departed controller's spell the settlement continued to the action
/// boundary's final-part convergence instead of retiring it.
pub(crate) fn reconcile_departures(
    state: &mut GameState,
    leaving: &std::collections::HashSet<PlayerId>,
    events: &mut Vec<GameEvent>,
) -> Result<DepartureSettlement, EngineError> {
    let Some(mut poll) = state.pending_player_scope_unless_punishment.take() else {
        return Ok(DepartureSettlement::NotSettled);
    };
    let live = prompt_is_live(state, &poll);
    poll.remaining_players
        .retain(|player| !leaving.contains(player));
    poll.declining_players
        .retain(|player| !leaving.contains(player));
    poll.payments
        .retain(|payment| !leaving.contains(&payment.player));
    if !(leaving.contains(&poll.current_player) && live) {
        state.pending_player_scope_unless_punishment = Some(poll);
        return Ok(DepartureSettlement::NotSettled);
    }
    advance(state, poll, events)?;
    Ok(if state.pending_player_scope_unless_punishment.is_some() {
        DepartureSettlement::NotSettled
    } else {
        DepartureSettlement::Settled
    })
}

/// CR 101.4a + CR 101.4b + CR 400.2: the viewer's copy of an open poll. An
/// earlier seat's chosen sacrifice is public — later choosers know it — but a
/// chosen discard names a card still in a hidden hand, and the current seat's
/// unfinished picks are not yet a choice anyone has made.
pub(crate) fn redact_for_viewer(
    poll: &mut PendingPlayerScopeUnlessPunishment,
    can_view_private: impl Fn(PlayerId) -> bool,
) {
    for payment in &mut poll.payments {
        if payment.action == PlayerScopeUnlessAction::Discard && !can_view_private(payment.player) {
            payment.objects.clear();
        }
    }
    if !can_view_private(poll.current_player) {
        poll.current_picks.clear();
    }
}

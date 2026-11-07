//! Single authority for paying life as a cost.
//!
//! All cost paths that deduct life — spell additional costs (Flashback life,
//! generic "Pay N life" additional costs), activated-ability costs (Greed,
//! Necropotence, Phyrexian Tower family), mana-ability costs, `Effect::PayCost`
//! resolution, and `UnlessCost::PayLife` — route through [`pay_life_as_cost`].
//!
//! Pre-validation sites (`can_activate_ability_now`, Defiler offer, legal-action
//! generation) consult [`can_pay_life_cost`] so the UI never offers a cost the
//! player cannot actually pay.
//!
//! # Rules
//!
//! - **CR 118.3** — A player can't pay a cost without the resources to pay it fully.
//! - **CR 118.3b** — Paying life subtracts the amount from the player's life total.
//!   Players can always pay 0 life.
//! - **CR 119.4** — "If a player pays life, the payment is subtracted from their
//!   life total; in other words, the player loses that much life." Paying life IS
//!   losing life, so the deduction routes through
//!   [`effects::life::try_apply_life_loss`] which runs the replacement pipeline.
//! - **CR 119.4b** — Players can always pay 0 life, even under a pay-life prohibition.
//! - **CR 119.8** — "A cost that involves having that player pay life can't be paid."
//!
//! # Gain-side sibling
//!
//! Costs that have OTHER players gain life (CR 118.3 + CR 119.3 — Invigorate's
//! "have an opponent gain 3 life", whose recipient the payer chooses during
//! payment per CR 115.10a, and Reverent Silence's "have each other player gain 6
//! life") live here too: [`life_gain_cost_recipients`] answers who the payment
//! reaches and [`give_life_as_cost`] performs each gain. The spell payer settles
//! them at the CR 601.2h payment, after the CR 601.2g mana-ability window.
//!
//! - **CR 119.7** — "a cost that involves having that player gain life can't be
//!   paid", so a player under a can't-gain-life lock is not a recipient.
//! - **CR 614.17b** — if an event can't happen, a player can't choose to pay a
//!   cost that includes that event. A gain that a replacement reduces to zero CAN
//!   still happen and stays payable (Invigorate's 2020-08-07 ruling).

use crate::game::effects::life::{
    apply_life_gain, try_apply_life_loss, LifeChangeError, ReplacementDeferred,
};
use crate::game::static_abilities::{
    player_cant_pay_life_as_cost, player_has_cant_gain_life, player_has_cant_lose_life,
};
use crate::types::ability::{CostRecipientSelection, TargetFilter};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
use crate::types::identifiers::ObjectId;
use crate::types::player::PlayerId;

/// Outcome of attempting to pay life as a cost.
///
/// CR 119.4b: Paying 0 life always succeeds — even under a prohibition — and
/// is represented by `Paid { amount: 0 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayLifeCostResult {
    /// The full amount was deducted (or amount was 0, always payable per CR 119.4b).
    Paid { amount: u32 },
    /// CR 118.11 + CR 614.6: The cost is paid even though a replacement
    /// modified its life-loss action, and that replacement's substitute has
    /// paused for player input. `state.waiting_for` owns the continuation.
    PaidWithDeferredSubstitution { amount: u32 },
    /// CR 118.3b + CR 119.4 + CR 616.1: The life-payment event is awaiting
    /// replacement ordering before it can be applied. No payment failure has
    /// occurred; the caller must park its typed outer action until the chosen
    /// replacement finishes.
    DeferredReplacementChoice {
        amount: u32,
        choice_player: PlayerId,
    },
    /// CR 118.3: Player's life total is below `amount` — cost can't be paid.
    /// Also returned when the post-replacement deduction is not representable
    /// (life total or lost-this-turn counter overflow).
    InsufficientLife,
    /// CR 119.8 / CR 118.3: A static prohibition makes the cost unpayable.
    Prohibited,
}

impl PayLifeCostResult {
    /// Returns `true` if the cost was successfully paid (including the zero-life case).
    pub fn is_paid(self) -> bool {
        matches!(
            self,
            PayLifeCostResult::Paid { .. } | PayLifeCostResult::PaidWithDeferredSubstitution { .. }
        )
    }

    /// Returns `true` if the cost was NOT paid (insufficient life or prohibited).
    pub fn is_unpayable(self) -> bool {
        matches!(
            self,
            PayLifeCostResult::InsufficientLife | PayLifeCostResult::Prohibited
        )
    }
}

/// CR 107.4f + CR 118.3 + CR 119.8: How many Phyrexian 2-life payments can this
/// player afford, given their current life total and pay-life prohibition status?
///
/// Threaded into [`crate::game::mana_payment::can_pay_for_spell`] so Phyrexian
/// shards without an available mana option are only treated as payable when the
/// player has the life budget to cover them.
///
/// Returns 0 under CantLoseLife (CR 119.8), direct can't-pay-life statics, or
/// when the player isn't found.
/// Otherwise floor-divides current life by 2 (CR 118.3: must have the resource
/// to pay fully).
pub fn max_phyrexian_life_payments(state: &GameState, player: PlayerId) -> u32 {
    // CR 119.8 / CR 118.3: A cost that involves paying life can't be paid while prohibited.
    if player_has_cant_lose_life(state, player) || player_cant_pay_life_as_cost(state, player) {
        return 0;
    }
    // CR 119.4a: in a team format the payable budget is bounded by the team total.
    u32::try_from((crate::game::players::team_life_total(state, player) / 2).max(0)).unwrap_or(0)
}

/// CR 118.3 + CR 119.4b + CR 119.8: Pure predicate — can this player pay `amount` life?
///
/// General cost-resource predicate. Cast/activation validation that must also
/// honor direct "can't pay life" statics uses
/// [`can_pay_life_cast_or_activation_cost`].
pub fn can_pay_life_cost(state: &GameState, player: PlayerId, amount: u32) -> bool {
    // CR 119.4b: 0 life is always payable, even under pay-life prohibitions.
    if amount == 0 {
        return true;
    }
    // CR 119.8: "a cost that involves having that player pay life can't be paid."
    if player_has_cant_lose_life(state, player) {
        return false;
    }
    // CR 119.4a: in a team format affordability is bounded by the team's
    // shared life total (off-team this is the player's own life).
    crate::game::players::team_life_total(state, player) >= amount as i32
}

/// CR 118.3b + CR 119.4 + CR 119.8: Pay `amount` life from `player` as a cost.
///
/// Routes the life deduction through [`try_apply_life_loss`] per CR 119.4
/// ("paying life IS losing life"), so the replacement pipeline and the
/// `CantLoseLife` short-circuit run consistently with every other life-loss event.
///
/// Returns a [`PayLifeCostResult`] describing the outcome. The caller is
/// responsible for translating an unpayable result into the appropriate
/// failure signal for their context (cost-payment flag, `EngineError`, etc.).
///
/// Defense in depth: the prohibition check happens here AND inside
/// `try_apply_life_loss`. This module is also called from pre-validation
/// paths which may not have reached the executor yet, so checking at the cost
/// boundary keeps the result enum accurate.
pub fn pay_life_as_cost(
    state: &mut GameState,
    player: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> PayLifeCostResult {
    // CR 119.4b: Paying 0 life always succeeds, no event emitted.
    if amount == 0 {
        return PayLifeCostResult::Paid { amount: 0 };
    }

    // CR 119.8: Lock → cost can't be paid.
    if player_has_cant_lose_life(state, player) {
        return PayLifeCostResult::Prohibited;
    }

    // CR 119.4a: Resource check — affordability is bounded by the team's
    // shared total. The DEDUCTION below lands on the individual `Player::life`
    // (CR 810.9: life loss happens to each player individually) and may go
    // negative — only the affordability gate reads the team total.
    let has_life = crate::game::players::team_life_total(state, player) >= amount as i32;
    if !has_life {
        return PayLifeCostResult::InsufficientLife;
    }

    // CR 119.4: Pay life is life loss — route through the damage/life-loss helper
    // so the replacement pipeline fires. CR 118.11: a replacement may modify
    // the action performed while paying, but the cost is still paid. Preserve
    // the typed paused-substitute outcome so callers cannot confuse a paid
    // cost whose replacement is still resolving with insufficient life.
    // Engine representability limit, not a rule: a deduction the engine cannot
    // record leaves life and counters untouched and is reported through the
    // existing unpayable variant.
    match try_apply_life_loss(state, player, amount, events) {
        Ok(_) => PayLifeCostResult::Paid { amount },
        Err(LifeChangeError::Unrepresentable(_)) => PayLifeCostResult::InsufficientLife,
        Err(LifeChangeError::Deferred(ReplacementDeferred::ReplacementChoice)) => {
            let WaitingFor::ReplacementChoice {
                player: choice_player,
                ..
            } = &state.waiting_for
            else {
                unreachable!("replacement deferral must expose its ordering player")
            };
            PayLifeCostResult::DeferredReplacementChoice {
                amount,
                choice_player: *choice_player,
            }
        }
        Err(LifeChangeError::Deferred(ReplacementDeferred::SubstitutionContinuation {
            ..
        })) => PayLifeCostResult::PaidWithDeferredSubstitution { amount },
    }
}

/// CR 118.3 + CR 119.4b + CR 601.2h + CR 602.2b: Can this player pay `amount`
/// life specifically as a spell-casting or activation cost?
pub fn can_pay_life_cast_or_activation_cost(
    state: &GameState,
    player: PlayerId,
    amount: u32,
) -> bool {
    can_pay_life_cost(state, player, amount)
        && (amount == 0 || !player_cant_pay_life_as_cost(state, player))
}

/// CR 118.3 + CR 119.4b + CR 601.2h + CR 602.2b: Pay life specifically as a
/// spell-casting or activation cost, applying direct "can't pay life" statics.
pub fn pay_life_as_cast_or_activation_cost(
    state: &mut GameState,
    player: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> PayLifeCostResult {
    if amount > 0 && player_cant_pay_life_as_cost(state, player) {
        return PayLifeCostResult::Prohibited;
    }
    pay_life_as_cost(state, player, amount, events)
}

/// Who a life-gain cost's instruction reaches when it is paid (CR 601.2h).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LifeGainCostRecipients {
    /// No legal payment exists: no choosable recipient, or (for "each other
    /// player") one of the required recipients can't gain life (CR 119.7 +
    /// CR 601.2h: partial payments are not allowed).
    Unpayable,
    /// Two or more players the payer may choose between (CR 115.10a).
    Choose(Vec<PlayerId>),
    /// Every listed player gains the life: the only choosable recipient, or each
    /// player an "each other player" cost names.
    All(Vec<PlayerId>),
}

/// CR 118.3 + CR 119.3 + CR 115.10a: the players matching a life-gain cost's
/// `recipient` filter, relative to the PAYER (team-aware, CR 102.3), in seat
/// order, before any CR 119.7 lock is considered.
///
/// The recipient is a CHOICE (or a fixed population), not a target, so it uses
/// the choice seam (`player_exists_for_choice`: alive and phased in).
fn life_gain_cost_candidates(
    state: &GameState,
    payer: PlayerId,
    source_id: ObjectId,
    recipient: &TargetFilter,
) -> Vec<PlayerId> {
    state
        .players
        .iter()
        .map(|p| p.id)
        .filter(|&p| crate::game::players::player_exists_for_choice(state, p))
        .filter(|&p| {
            crate::game::filter::player_matches_target_filter_in_state(
                state,
                recipient,
                p,
                Some(payer),
                Some(source_id),
            )
        })
        .collect()
}

/// CR 118.3 + CR 119.3 + CR 119.7 + CR 601.2h: who a life-gain cost reaches if it
/// is paid now.
///
/// CR 119.7: "a cost that involves having that player gain life can't be paid",
/// so a player under a "can't gain life" static is not a choosable recipient
/// (Invigorate's 2020-08-07 ruling), and an "each other player" cost with such a
/// player among its recipients can't be paid at all. A replacement that prevents
/// or reduces the gain (e.g. to zero) deliberately does NOT exclude anyone: the
/// gain event can still happen and is merely modified (CR 614.1a; the same
/// ruling's Sulfuric Vortex case). This intentionally differs from the
/// counter-cost gate in `costs.rs` (`mandatory_prevention_applies`); do not unify
/// the two without revisiting it.
pub fn life_gain_cost_recipients(
    state: &GameState,
    payer: PlayerId,
    source_id: ObjectId,
    recipient: &TargetFilter,
    selection: CostRecipientSelection,
) -> LifeGainCostRecipients {
    let candidates = life_gain_cost_candidates(state, payer, source_id, recipient);
    match selection {
        CostRecipientSelection::ChosenOne => {
            let eligible: Vec<PlayerId> = candidates
                .into_iter()
                .filter(|&p| !player_has_cant_gain_life(state, p))
                .collect();
            match eligible.len() {
                0 => LifeGainCostRecipients::Unpayable,
                1 => LifeGainCostRecipients::All(eligible),
                _ => LifeGainCostRecipients::Choose(eligible),
            }
        }
        CostRecipientSelection::Each => {
            if candidates
                .iter()
                .any(|&p| player_has_cant_gain_life(state, p))
            {
                LifeGainCostRecipients::Unpayable
            } else {
                LifeGainCostRecipients::All(candidates)
            }
        }
    }
}

/// CR 601.2g + CR 119.7: whether a life-gain cost that can't be paid now has
/// someone it could reach once a "can't gain life" lock is gone. Mana abilities
/// activated before the payment (CR 601.2g) can remove the lock (sacrificing
/// Archfiend of Despair to Ashnod's Altar), so a cast whose total cost includes
/// mana keeps such a cost on offer and settles it at payment.
pub fn life_gain_cost_has_unlocked_reach(
    state: &GameState,
    payer: PlayerId,
    source_id: ObjectId,
    recipient: &TargetFilter,
    selection: CostRecipientSelection,
) -> bool {
    match selection {
        CostRecipientSelection::ChosenOne => {
            !life_gain_cost_candidates(state, payer, source_id, recipient).is_empty()
        }
        // Every population is reachable: an empty one is paid vacuously.
        CostRecipientSelection::Each => true,
    }
}

/// Outcome of performing a life-gain cost for a chosen recipient.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GiveLifeCostResult {
    /// The gain event finished; `gained` is the amount actually gained after
    /// replacements (possibly 0).
    Paid { gained: u32 },
    /// The gain event paused for player input (a replacement ordering choice or a
    /// substitute's continuation); `state.waiting_for` owns the continuation.
    Deferred,
}

/// CR 118.3 + CR 119.3 + CR 614.1a: Perform a life-gain cost — `recipient` gains
/// `amount` life. The cost's instruction is the ordinary life-gain event, so CR
/// 119.7 and replacement effects apply. On `Deferred`, `state.waiting_for` owns the
/// continuation (a `ReplacementChoice` or a substitute), answered by the recipient
/// (CR 616.1: the affected player orders competing replacements).
pub fn give_life_as_cost(
    state: &mut GameState,
    recipient: PlayerId,
    amount: u32,
    events: &mut Vec<GameEvent>,
) -> GiveLifeCostResult {
    match apply_life_gain(state, recipient, amount, events) {
        Ok(gained) => GiveLifeCostResult::Paid { gained },
        Err(_) => GiveLifeCostResult::Deferred,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, ControllerRef, Effect, PlayerFilter,
        QuantityModification, ReplacementDefinition, StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
    use crate::types::replacements::ReplacementEvent;
    use crate::types::statics::{CostPaymentProhibition, ProhibitionScope, StaticMode};
    use crate::types::zones::Zone;

    fn add_cant_lose_life_permanent(state: &mut GameState, owner: PlayerId) {
        let id = create_object(
            state,
            CardId(900),
            owner,
            "Life Lock".to_string(),
            Zone::Battlefield,
        );
        state.objects.get_mut(&id).unwrap().static_definitions.push(
            StaticDefinition::new(StaticMode::CantLoseLife).affected(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
        );
    }

    fn add_cant_pay_life_permanent(state: &mut GameState, owner: PlayerId) {
        let id = create_object(
            state,
            CardId(901),
            owner,
            "Cost Lock".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .static_definitions
            .push(StaticDefinition::new(StaticMode::CantPayCost {
                who: ProhibitionScope::AllPlayers,
                cost: CostPaymentProhibition::PayLife,
            }));
    }

    #[test]
    fn paid_life_with_interactive_post_mutation_substitute_stays_paid_and_typed() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(902),
            PlayerId(0),
            "Interactive Life-Loss Modifier".to_string(),
            Zone::Battlefield,
        );
        let choice = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChooseOneOf {
                chooser: PlayerFilter::Controller,
                branches: vec![AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::GainLife {
                        amount: crate::types::ability::QuantityExpr::Fixed { value: 1 },
                        player: TargetFilter::Controller,
                    },
                )],
                kind: ChooseOneOfKind::Ordinary,
            },
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(QuantityModification::Plus { value: 0 })
            .execute(choice)]
        .into();
        let life_before = state.players[0].life;
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(
            result,
            PayLifeCostResult::PaidWithDeferredSubstitution { amount: 3 }
        );
        assert_eq!(state.players[0].life, life_before - 3);
        assert!(matches!(
            state.waiting_for,
            crate::types::game_state::WaitingFor::ChooseOneOfBranch { .. }
        ));
    }

    /// CR 118.3b + CR 119.4 + CR 616.1: Competing replacements suspend a
    /// life payment; they do not turn it into an insufficient-life failure.
    #[test]
    fn competing_life_loss_replacements_return_typed_deferred_choice() {
        let mut state = GameState::new_two_player(42);
        let source = create_object(
            &mut state,
            CardId(903),
            PlayerId(0),
            "Competing Life-Loss Modifiers".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .replacement_definitions = vec![
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::DOUBLE)
                .description("Double".to_string()),
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::Plus { value: 1 })
                .description("Plus one".to_string()),
        ]
        .into();
        let life_before = state.players[0].life;
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(
            result,
            PayLifeCostResult::DeferredReplacementChoice {
                amount: 3,
                choice_player: PlayerId(0),
            }
        );
        assert_eq!(state.players[0].life, life_before);
        assert!(matches!(
            state.waiting_for,
            WaitingFor::ReplacementChoice {
                player: PlayerId(0),
                ..
            }
        ));
        assert!(state.pending_replacement.is_some());
        assert!(events.is_empty());
    }

    /// CR 119.4b: Paying 0 life always succeeds, even under CantLoseLife.
    #[test]
    fn pay_zero_always_paid_under_lock() {
        let mut state = GameState::new_two_player(42);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 0, &mut events);

        assert_eq!(result, PayLifeCostResult::Paid { amount: 0 });
        assert_eq!(state.players[0].life, 20);
        assert!(events.is_empty(), "no life event for 0-life payment");
    }

    /// CR 118.3b: Paying N life subtracts N from life total, emits LifeChanged.
    #[test]
    fn pay_life_deducts_from_life_total() {
        let mut state = GameState::new_two_player(42);
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(result, PayLifeCostResult::Paid { amount: 3 });
        assert_eq!(state.players[0].life, 17);
        assert!(events
            .iter()
            .any(|e| matches!(e, GameEvent::LifeChanged { amount: -3, .. })));
    }

    /// CR 118.3: Insufficient life → cost can't be paid; life total unchanged.
    #[test]
    fn pay_life_insufficient_returns_insufficient() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 2;
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(result, PayLifeCostResult::InsufficientLife);
        assert_eq!(state.players[0].life, 2);
        assert!(events.is_empty());
    }

    /// A payment that is affordable but whose deduction the engine cannot
    /// record (the lost-this-turn counter would overflow) reports the existing
    /// unpayable variant and leaves life and counters untouched; one below the
    /// limit pays normally.
    #[test]
    fn pay_life_unrepresentable_deduction_returns_insufficient_unmutated() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life_lost_this_turn = u32::MAX - 1;
        let mut events = Vec::new();

        assert!(
            can_pay_life_cost(&state, PlayerId(0), 2),
            "reach guard: the payment is affordable, so only representability fails"
        );
        let result = pay_life_as_cost(&mut state, PlayerId(0), 2, &mut events);

        assert_eq!(result, PayLifeCostResult::InsufficientLife);
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[0].life_lost_this_turn, u32::MAX - 1);
        assert_eq!(state.players[0].life_gained_this_turn, 0);
        assert!(
            !events
                .iter()
                .any(|event| matches!(event, GameEvent::LifeChanged { .. })),
            "no life change may be announced for an unrepresentable payment: {events:?}"
        );
        assert_eq!(
            state.ingress_life_window, None,
            "the ingress window must be closed after the call"
        );

        state.players[0].life_lost_this_turn = u32::MAX - 2;
        let result = pay_life_as_cost(&mut state, PlayerId(0), 2, &mut events);
        assert_eq!(result, PayLifeCostResult::Paid { amount: 2 });
        assert_eq!(state.players[0].life, 18);
        assert_eq!(state.players[0].life_lost_this_turn, u32::MAX);
    }

    /// CR 119.8: CantLoseLife → cost can't be paid; life total unchanged.
    #[test]
    fn pay_life_locked_returns_prohibited() {
        let mut state = GameState::new_two_player(42);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(result, PayLifeCostResult::Prohibited);
        assert_eq!(state.players[0].life, 20);
        assert!(events.is_empty());
    }

    /// CR 118.3 + CR 119.4b: "can't pay life" is cost-scoped and rejects
    /// positive life payments without being modeled as CantLoseLife.
    #[test]
    fn pay_life_cost_prohibition_returns_prohibited() {
        let mut state = GameState::new_two_player(42);
        add_cant_pay_life_permanent(&mut state, PlayerId(0));
        let mut events = Vec::new();

        let result = pay_life_as_cast_or_activation_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(result, PayLifeCostResult::Prohibited);
        assert_eq!(state.players[0].life, 20);
        assert!(events.is_empty());
        assert!(can_pay_life_cost(&state, PlayerId(0), 1));
        assert!(can_pay_life_cast_or_activation_cost(&state, PlayerId(0), 0));
        assert!(!can_pay_life_cast_or_activation_cost(
            &state,
            PlayerId(0),
            1
        ));
    }

    /// CR 119.4b: `can_pay_life_cost` returns true for 0 even under lock.
    #[test]
    fn can_pay_zero_under_lock() {
        let mut state = GameState::new_two_player(42);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));

        assert!(can_pay_life_cost(&state, PlayerId(0), 0));
    }

    /// CR 119.8: `can_pay_life_cost` rejects any positive amount under lock.
    #[test]
    fn cant_pay_positive_under_lock() {
        let mut state = GameState::new_two_player(42);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));

        assert!(!can_pay_life_cost(&state, PlayerId(0), 1));
        assert!(!can_pay_life_cost(&state, PlayerId(0), 20));
    }

    /// CR 118.3: `can_pay_life_cost` rejects when life total < amount.
    #[test]
    fn cant_pay_more_than_life() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 5;

        assert!(can_pay_life_cost(&state, PlayerId(0), 5));
        assert!(!can_pay_life_cost(&state, PlayerId(0), 6));
    }

    /// CR 119.8: The lock affects only players matching the static's filter.
    /// An opponent can still pay life normally.
    #[test]
    fn unmatched_player_can_still_pay() {
        let mut state = GameState::new_two_player(42);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let mut events = Vec::new();

        // PlayerId(1) is not covered by PlayerId(0)'s "You"-scoped static.
        let result = pay_life_as_cost(&mut state, PlayerId(1), 3, &mut events);

        assert_eq!(result, PayLifeCostResult::Paid { amount: 3 });
        assert_eq!(state.players[1].life, 17);
    }

    /// CR 119.4a + CR 810.9a: in 2HG, `can_pay_life_cost` affordability is
    /// bounded by the TEAM total. Member at 3, teammate at 9 (team 12) → a
    /// 10-life cost is payable even though the individual has only 3. Reverting
    /// Site 3 to `p.life >= amount` flips this to unpayable. The order is
    /// preserved: 0 is payable and a lock blocks any positive amount.
    #[test]
    fn can_pay_life_cost_team_bounded_in_2hg() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].life = 3;
        state.players[1].life = 9; // team total 12

        assert!(
            can_pay_life_cost(&state, PlayerId(0), 10),
            "team total 12 affords a 10-life cost"
        );
        assert!(!can_pay_life_cost(&state, PlayerId(0), 13));
        // CR 119.4b: 0 always payable.
        assert!(can_pay_life_cost(&state, PlayerId(0), 0));
        // CR 119.8: lock blocks any positive amount.
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        assert!(!can_pay_life_cost(&state, PlayerId(0), 1));
    }

    /// Off-team degeneracy sibling for Site 3: in a 1v1, affordability is the
    /// player's own life (no team fold).
    #[test]
    fn can_pay_life_cost_off_team_individual() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life = 4;
        assert!(can_pay_life_cost(&state, PlayerId(0), 4));
        assert!(!can_pay_life_cost(&state, PlayerId(0), 5));
    }

    /// CR 119.4a + CR 810.9: `max_phyrexian_life_payments` is bounded by the
    /// team total (12 / 2 = 6), not the individual (3 / 2 = 1). The lock still
    /// short-circuits to 0.
    #[test]
    fn max_phyrexian_payments_team_bounded_in_2hg() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].life = 3;
        state.players[1].life = 9; // team 12
        assert_eq!(max_phyrexian_life_payments(&state, PlayerId(0)), 6);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        assert_eq!(max_phyrexian_life_payments(&state, PlayerId(0)), 0);
    }

    /// CR 119.4a + CR 810.9: pay-life affordability is team-bounded, and the
    /// DEDUCTION lands on the individual `Player::life` — a member may go
    /// negative while the team stays positive. Pay 6 from a {2, 6} team: payer
    /// goes to -4 (team 4), no SBA runs here (this is the cost helper only).
    #[test]
    fn pay_life_team_bounded_deduction_lands_individual_in_2hg() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].life = 2;
        state.players[1].life = 6; // team 8
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 6, &mut events);

        assert_eq!(result, PayLifeCostResult::Paid { amount: 6 });
        // Deduction is individual (CR 810.9): payer 2 - 6 = -4.
        assert_eq!(state.players[0].life, -4);
        // Team total now 2 (-4 + 6).
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(0)),
            2
        );
    }

    fn opponent_recipient() -> TargetFilter {
        TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
    }

    fn recipient_source(state: &mut GameState) -> ObjectId {
        create_object(
            state,
            CardId(910),
            PlayerId(0),
            "Recipient Cost Source".to_string(),
            Zone::Hand,
        )
    }

    fn other_players_recipient() -> TargetFilter {
        TargetFilter::PlayerMatching {
            player: Box::new(PlayerFilter::AllExcept {
                exclude: Box::new(PlayerFilter::Controller),
                relation: crate::types::ability::PlayerRelation::All,
            }),
        }
    }

    fn chosen_one(state: &GameState, payer: PlayerId, src: ObjectId) -> LifeGainCostRecipients {
        life_gain_cost_recipients(
            state,
            payer,
            src,
            &opponent_recipient(),
            CostRecipientSelection::ChosenOne,
        )
    }

    fn each_other(state: &GameState, payer: PlayerId, src: ObjectId) -> LifeGainCostRecipients {
        life_gain_cost_recipients(
            state,
            payer,
            src,
            &other_players_recipient(),
            CostRecipientSelection::Each,
        )
    }

    fn add_everyone_cant_gain_life(state: &mut GameState, owner: PlayerId) {
        let lock = create_object(
            state,
            CardId(911),
            owner,
            "Everlasting Torment".to_string(),
            Zone::Battlefield,
        );
        state
            .objects
            .get_mut(&lock)
            .unwrap()
            .static_definitions
            .push(
                StaticDefinition::new(StaticMode::CantGainLife)
                    .affected(TargetFilter::Typed(TypedFilter::default())),
            );
    }

    /// CR 115.10a + CR 102.3: recipients of "have an opponent gain life" are the
    /// payer's choosable opponents, in seat order; a lone one binds.
    #[test]
    fn life_gain_cost_recipients_are_payer_relative_choosable_opponents() {
        let mut two = GameState::new_two_player(42);
        let src = recipient_source(&mut two);
        assert_eq!(
            chosen_one(&two, PlayerId(0), src),
            LifeGainCostRecipients::All(vec![PlayerId(1)])
        );
        assert_eq!(
            chosen_one(&two, PlayerId(1), src),
            LifeGainCostRecipients::All(vec![PlayerId(0)])
        );

        let mut three = GameState::new(FormatConfig::standard(), 3, 42);
        let src = recipient_source(&mut three);
        assert_eq!(
            chosen_one(&three, PlayerId(0), src),
            LifeGainCostRecipients::Choose(vec![PlayerId(1), PlayerId(2)])
        );

        // CR 115.10a: an eliminated player can't be chosen.
        let mut events = Vec::new();
        crate::game::elimination::eliminate_player(&mut three, PlayerId(2), &mut events);
        assert_eq!(
            chosen_one(&three, PlayerId(0), src),
            LifeGainCostRecipients::All(vec![PlayerId(1)])
        );
    }

    /// CR 102.3: a teammate is not an opponent (Two-Headed Giant), but it is one
    /// of the "other players".
    #[test]
    fn life_gain_cost_recipients_scope_teammates_by_filter() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        let src = recipient_source(&mut state);
        let LifeGainCostRecipients::Choose(opponents) = chosen_one(&state, PlayerId(0), src) else {
            panic!("two opposing heads expected");
        };
        assert!(!opponents.contains(&PlayerId(0)));
        assert!(!opponents.contains(&PlayerId(1)));
        assert_eq!(opponents.len(), 2, "both opposing heads: {opponents:?}");

        assert_eq!(
            each_other(&state, PlayerId(0), src),
            LifeGainCostRecipients::All(vec![PlayerId(1), PlayerId(2), PlayerId(3)])
        );
    }

    /// CR 119.7: a player who can't gain life isn't a recipient, and an "each other
    /// player" cost with one among its recipients can't be paid. Positive controls
    /// first on the same builder; the locked opponent stays reachable for the
    /// CR 601.2g window.
    #[test]
    fn life_gain_cost_recipients_exclude_cant_gain_life() {
        let mut state = GameState::new(FormatConfig::standard(), 3, 42);
        let src = recipient_source(&mut state);
        assert_eq!(
            each_other(&state, PlayerId(0), src),
            LifeGainCostRecipients::All(vec![PlayerId(1), PlayerId(2)])
        );
        add_everyone_cant_gain_life(&mut state, PlayerId(1));
        assert_eq!(
            chosen_one(&state, PlayerId(0), src),
            LifeGainCostRecipients::Unpayable
        );
        assert_eq!(
            each_other(&state, PlayerId(0), src),
            LifeGainCostRecipients::Unpayable
        );
        assert!(life_gain_cost_has_unlocked_reach(
            &state,
            PlayerId(0),
            src,
            &opponent_recipient(),
            CostRecipientSelection::ChosenOne,
        ));
    }

    /// CR 119.3 + CR 119.7: the cost's gain is the ordinary gain event.
    #[test]
    fn give_life_as_cost_gains_through_the_life_gain_authority() {
        let mut state = GameState::new_two_player(42);
        let before = state.players[1].life;
        let mut events = Vec::new();
        assert_eq!(
            give_life_as_cost(&mut state, PlayerId(1), 3, &mut events),
            GiveLifeCostResult::Paid { gained: 3 }
        );
        assert_eq!(state.players[1].life, before + 3);
    }
}

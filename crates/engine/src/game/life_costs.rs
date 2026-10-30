//! Single authority for paying life as a cost.
//!
//! All cost paths that deduct life — spell additional costs (Flashback life,
//! generic "Pay N life" additional costs), activated-ability costs (Greed,
//! Necropotence, Phyrexian Tower family), mana-ability costs, `Effect::PayCost`
//! resolution, and `UnlessCost::PayLife` — route through [`pay_life_as_cost`].
//!
//! Pre-validation sites (`can_activate_ability_now`, Defiler offer, legal-action
//! generation) consult [`can_pay_life_cost`] so the UI never offers a cost the
//! player cannot actually pay. Prompts and probes that need the largest payable
//! amount (Phyrexian life payments, "pay X life", "pay any amount of life") read
//! [`max_payable_life`] / [`max_payable_life_cast_or_activation`], the budget
//! [`can_pay_life_cost`] tests against.
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
//! - **CR 119.4a** — In Two-Headed Giant the team's life total must cover the
//!   payment. Affordability reads the team's exact `i64` total and compares it
//!   with the whole `u32` payment, so no payment wraps into an affordable one.
//! - **CR 119.4b** — Players can always pay 0 life, even under a pay-life prohibition.
//! - **CR 119.8** — "A cost that involves having that player pay life can't be paid."

use crate::game::effects::life::{try_apply_life_loss, LifeChangeError, ReplacementDeferred};
use crate::game::static_abilities::{player_cant_pay_life_as_cost, player_has_cant_lose_life};
use crate::types::events::GameEvent;
use crate::types::game_state::{GameState, WaitingFor};
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
    /// (an overflowed replacement quantity, or a nested player-counter or
    /// energy settlement that overflows; a deduction of any `u32` amount is
    /// carried exactly, and the life total and the lost-this-turn counter
    /// saturate instead).
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
/// Otherwise floor-divides the cast/activation life budget
/// ([`max_payable_life_cast_or_activation`]) by 2 (CR 107.4f: each Phyrexian
/// payment is 2 life; CR 118.3: must have the resource to pay fully).
pub fn max_phyrexian_life_payments(state: &GameState, player: PlayerId) -> u32 {
    max_payable_life_cast_or_activation(state, player) / 2
}

/// CR 118.3 + CR 119.4 + CR 119.4a + CR 119.8: The largest amount of life
/// `player` can pay as a cost — the budget [`can_pay_life_cost`] tests every
/// payment against. It is the exact life total that pays (the team's shared
/// total in Two-Headed Giant, CR 810.9a; the player's own life otherwise),
/// clamped into `u32`: a total of 0 or less affords no positive payment, and
/// under a can't-lose-life lock only 0 is payable (CR 119.4b).
pub fn max_payable_life(state: &GameState, player: PlayerId) -> u32 {
    // CR 119.8: "a cost that involves having that player pay life can't be paid."
    if player_has_cant_lose_life(state, player) {
        return 0;
    }
    // CR 119.4 + CR 119.4a: the exact `i64` team total — a Two-Headed Giant
    // team can hold more than `i32::MAX` life between its members.
    crate::game::arithmetic::i64_to_u32_saturating(crate::game::players::team_life_sum(
        state, player,
    ))
}

/// CR 118.3 + CR 601.2h + CR 602.2b: The cast/activation twin of
/// [`max_payable_life`]: 0 while a direct "can't pay life" static applies, as
/// [`can_pay_life_cast_or_activation_cost`] then refuses any positive amount.
pub fn max_payable_life_cast_or_activation(state: &GameState, player: PlayerId) -> u32 {
    if player_cant_pay_life_as_cost(state, player) {
        return 0;
    }
    max_payable_life(state, player)
}

/// CR 118.3 + CR 119.4 + CR 119.4a + CR 119.4b + CR 119.8: Pure predicate —
/// can this player pay `amount` life?
///
/// General cost-resource predicate. Cast/activation validation that must also
/// honor direct "can't pay life" statics uses
/// [`can_pay_life_cast_or_activation_cost`].
pub fn can_pay_life_cost(state: &GameState, player: PlayerId, amount: u32) -> bool {
    // CR 119.4b: 0 life is always payable, even under pay-life prohibitions.
    // CR 118.3 + CR 119.4 + CR 119.4a: a positive payment needs a (team) life
    // total at least that large. The budget is the exact total clamped into
    // `u32`, which answers `total >= amount` exactly for every `u32` amount.
    amount == 0 || amount <= max_payable_life(state, player)
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

    // CR 118.3 + CR 119.4a: Resource check through the single affordability
    // authority, on the unreplaced payment and before any replacement sees the
    // loss — affordability is bounded by the team's exact shared total. The
    // deduction below is a life loss of the paying player (CR 810.9) applied to
    // the team's shared total; the edit authority stores it on the payer's own
    // `Player::life` first, so that field may go negative while the team total
    // is positive (CR 810.9a: the split across members is storage only).
    if !can_pay_life_cost(state, player, amount) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        AbilityDefinition, AbilityKind, ChooseOneOfKind, ControllerRef, Effect, PlayerFilter,
        QuantityExpr, QuantityModification, QuantityRef, ReplacementDefinition, StaticDefinition,
        TargetFilter, TypedFilter,
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

    /// CR 119.4: a payment whose deduction would pass the lost-this-turn
    /// counter's `u32::MAX` is paid: the counter saturates and the payment
    /// reports the full amount; one below the limit pays normally.
    #[test]
    fn pay_life_deduction_saturates_the_lost_counter() {
        let mut state = GameState::new_two_player(42);
        state.players[0].life_lost_this_turn = u32::MAX - 1;
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 2, &mut events);

        assert_eq!(result, PayLifeCostResult::Paid { amount: 2 });
        assert_eq!(state.players[0].life, 18);
        assert_eq!(state.players[0].life_lost_this_turn, u32::MAX);
        assert_eq!(state.players[0].life_gained_this_turn, 0);
        let life_changes: Vec<(PlayerId, i32)> = events
            .iter()
            .filter_map(|event| match event {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } => Some((*player_id, *amount)),
                _ => None,
            })
            .collect();
        assert_eq!(life_changes, vec![(PlayerId(0), -2)]);
        assert_eq!(
            state.ingress_life_window, None,
            "the ingress window must be closed after the call"
        );

        state.players[0].life_lost_this_turn = u32::MAX - 2;
        let result = pay_life_as_cost(&mut state, PlayerId(0), 2, &mut events);
        assert_eq!(result, PayLifeCostResult::Paid { amount: 2 });
        assert_eq!(state.players[0].life, 16);
        assert_eq!(state.players[0].life_lost_this_turn, u32::MAX);
    }

    /// A payment that is affordable but whose replaced deduction the engine
    /// cannot record reports the existing unpayable variant and leaves the
    /// state untouched. The synthetic substitute turns the loss into an energy
    /// gain of the same amount (CR 614.6), and an energy total past `u32::MAX`
    /// is unrepresentable. Reach-guard control: with room for the energy, the
    /// substitute replaces the loss and the cost is still paid (CR 118.11).
    #[test]
    fn pay_life_reports_a_nested_unrepresentable_settlement_as_insufficient_life() {
        let board = |energy: u32| {
            let mut state = GameState::new_two_player(42);
            state.players[0].energy = energy;
            let source = create_object(
                &mut state,
                CardId(905),
                PlayerId(0),
                "Energy Substitute".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions =
                vec![
                    ReplacementDefinition::new(ReplacementEvent::LoseLife).execute(
                        AbilityDefinition::new(
                            AbilityKind::Spell,
                            Effect::GainEnergy {
                                amount: QuantityExpr::Ref {
                                    qty: QuantityRef::EventContextAmount,
                                },
                            },
                        ),
                    ),
                ]
                .into();
            state
        };

        let mut control = board(0);
        let mut events = Vec::new();
        let result = pay_life_as_cost(&mut control, PlayerId(0), 2, &mut events);
        assert_eq!(result, PayLifeCostResult::Paid { amount: 2 });
        assert_eq!(control.players[0].life, 20);
        assert_eq!(control.players[0].energy, 2);

        let mut state = board(u32::MAX);
        let mut events = Vec::new();
        assert!(
            can_pay_life_cost(&state, PlayerId(0), 2),
            "reach guard: the payment is affordable, so only representability fails"
        );
        let result = pay_life_as_cost(&mut state, PlayerId(0), 2, &mut events);

        assert_eq!(result, PayLifeCostResult::InsufficientLife);
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[0].life_lost_this_turn, 0);
        assert_eq!(state.players[0].energy, u32::MAX);
        assert!(
            !events.iter().any(|event| matches!(
                event,
                GameEvent::LifeChanged { .. } | GameEvent::EnergyChanged { .. }
            )),
            "no resource change may be announced for an unrepresentable payment: {events:?}"
        );
        assert_eq!(
            state.ingress_life_window, None,
            "the ingress window must be closed after the call"
        );
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
    /// deduction (the payer's life loss, applied to the team's shared total) is
    /// stored on the payer's own `Player::life` first, so a member's field may go
    /// negative while the team stays positive (CR 810.9a: the split is storage
    /// only). Pay 6 from a {2, 6} team: payer goes to -4 (team 2), no SBA runs
    /// here (this is the cost helper only).
    #[test]
    fn pay_life_team_bounded_deduction_lands_individual_in_2hg() {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].life = 2;
        state.players[1].life = 6; // team 8
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 6, &mut events);

        assert_eq!(result, PayLifeCostResult::Paid { amount: 6 });
        // The payer's own field stores the deduction first (CR 810.9 + CR 810.9a): payer 2 - 6 = -4.
        assert_eq!(state.players[0].life, -4);
        // Team total now 2 (-4 + 6).
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(0)),
            2
        );
    }

    fn two_headed_giant_with_team_a_life(first: i32, second: i32) -> GameState {
        let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 0);
        state.players[0].life = first;
        state.players[1].life = second;
        state
    }

    /// CR 118.3 + CR 119.4 + CR 119.4a: affordability compares the exact (team)
    /// life total with the whole `u32` payment — no amount at or above 2^31
    /// wraps into an affordable negative, and a Two-Headed Giant team above
    /// `i32::MAX` affords exactly its exact total.
    #[test]
    fn life_payment_affordability_is_exact_for_every_u32_amount() {
        let mut state = GameState::new_two_player(42);
        assert!(can_pay_life_cost(&state, PlayerId(0), 20), "reach guard");
        for amount in [2_147_483_648, 3_000_000_000, u32::MAX] {
            assert!(
                !can_pay_life_cost(&state, PlayerId(0), amount),
                "life 20 cannot pay {amount}"
            );
        }

        state.players[0].life = i32::MAX;
        assert!(can_pay_life_cost(&state, PlayerId(0), i32::MAX as u32));
        assert!(!can_pay_life_cost(&state, PlayerId(0), 2_147_483_648));

        // 2HG (MAX, MAX): the team's exact total is 4,294,967,294 for either member.
        let state = two_headed_giant_with_team_a_life(i32::MAX, i32::MAX);
        assert!(can_pay_life_cost(&state, PlayerId(0), 4_294_967_294));
        assert!(can_pay_life_cost(&state, PlayerId(1), 4_294_967_294));
        assert!(!can_pay_life_cost(&state, PlayerId(0), u32::MAX));

        // 2HG (MAX, MIN): the team total is -1, so only 0 is payable (CR 119.4b).
        let state = two_headed_giant_with_team_a_life(i32::MAX, i32::MIN);
        assert!(can_pay_life_cost(&state, PlayerId(0), 0));
        assert!(!can_pay_life_cost(&state, PlayerId(0), 1));
        assert!(!can_pay_life_cost(&state, PlayerId(0), u32::MAX));
    }

    /// CR 118.3 + CR 119.4: an unaffordable payment is refused on its
    /// unreplaced amount, before any life-loss replacement sees it — CR 118.11
    /// lets a replacement modify a cost's action, not make an unaffordable cost
    /// payable. The replacements below are synthetic fixtures, not a card.
    #[test]
    fn unaffordable_wide_life_payment_is_refused_before_any_replacement() {
        let board_with = |replacements: Vec<ReplacementDefinition>| {
            let mut state = GameState::new_two_player(42);
            let source = create_object(
                &mut state,
                CardId(904),
                PlayerId(0),
                "Life-Loss Modifiers".to_string(),
                Zone::Battlefield,
            );
            state
                .objects
                .get_mut(&source)
                .unwrap()
                .replacement_definitions = replacements.into();
            state
        };

        // (a) Two competing replacements: the wide payment never parks a choice.
        let mut state = board_with(vec![
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::DOUBLE)
                .description("Double".to_string()),
            ReplacementDefinition::new(ReplacementEvent::LoseLife)
                .quantity_modification(QuantityModification::Plus { value: 1 })
                .description("Plus one".to_string()),
        ]);
        let waiting_before = state.waiting_for.clone();
        let mut events = Vec::new();
        let result = pay_life_as_cost(&mut state, PlayerId(0), 3_000_000_000, &mut events);
        assert_eq!(result, PayLifeCostResult::InsufficientLife);
        assert_eq!(state.waiting_for, waiting_before);
        assert!(state.pending_replacement.is_none());
        assert_eq!(state.players[0].life, 20);
        assert!(
            events.is_empty(),
            "no events for a refused payment: {events:?}"
        );
        assert_eq!(state.ingress_life_window, None);
        // Reach guard: an affordable payment on the same board reaches the
        // replacements and parks their ordering choice.
        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);
        assert_eq!(
            result,
            PayLifeCostResult::DeferredReplacementChoice {
                amount: 3,
                choice_player: PlayerId(0),
            }
        );

        // (b) A halving replacement cannot make a wide payment affordable.
        let mut state = board_with(vec![ReplacementDefinition::new(ReplacementEvent::LoseLife)
            .quantity_modification(QuantityModification::Half)]);
        let mut events = Vec::new();
        let result = pay_life_as_cost(&mut state, PlayerId(0), 3_000_000_000, &mut events);
        assert_eq!(result, PayLifeCostResult::InsufficientLife);
        assert_eq!(state.players[0].life, 20);
        assert_eq!(state.players[0].life_lost_this_turn, 0);
        assert!(
            events.is_empty(),
            "no events for a refused payment: {events:?}"
        );
        // Reach guard: an affordable payment on the same board is halved.
        let result = pay_life_as_cost(&mut state, PlayerId(0), 10, &mut events);
        assert_eq!(result, PayLifeCostResult::Paid { amount: 10 });
        assert_eq!(state.players[0].life, 15);
    }

    /// CR 107.4f + CR 119.4a + CR 119.8: the general and cast/activation
    /// payable-life budgets, and the Phyrexian count derived from the latter,
    /// read the exact team total; each budget is the largest amount its
    /// predicate accepts.
    #[test]
    fn payable_life_budgets_read_the_exact_team_sum() {
        let mut boards = Vec::new();

        let state = two_headed_giant_with_team_a_life(i32::MAX, i32::MAX);
        assert_eq!(max_payable_life(&state, PlayerId(0)), 4_294_967_294);
        assert_eq!(
            max_payable_life_cast_or_activation(&state, PlayerId(0)),
            4_294_967_294
        );
        assert_eq!(
            max_phyrexian_life_payments(&state, PlayerId(0)),
            2_147_483_647
        );
        boards.push(state);

        let state = two_headed_giant_with_team_a_life(i32::MAX, i32::MIN);
        assert_eq!(max_payable_life(&state, PlayerId(0)), 0);
        assert_eq!(max_payable_life_cast_or_activation(&state, PlayerId(0)), 0);
        assert_eq!(max_phyrexian_life_payments(&state, PlayerId(0)), 0);
        boards.push(state);

        let mut state = GameState::new_two_player(42);
        state.players[0].life = i32::MAX;
        assert_eq!(max_payable_life(&state, PlayerId(0)), 2_147_483_647);
        assert_eq!(
            max_payable_life_cast_or_activation(&state, PlayerId(0)),
            2_147_483_647
        );
        assert_eq!(
            max_phyrexian_life_payments(&state, PlayerId(0)),
            1_073_741_823
        );
        boards.push(state);

        // CR 118.3 + CR 601.2h: a direct "can't pay life" static zeroes only
        // the cast/activation budget (and the Phyrexian count read from it).
        let mut state = two_headed_giant_with_team_a_life(i32::MAX, i32::MAX);
        add_cant_pay_life_permanent(&mut state, PlayerId(0));
        assert_eq!(max_payable_life(&state, PlayerId(0)), 4_294_967_294);
        assert_eq!(max_payable_life_cast_or_activation(&state, PlayerId(0)), 0);
        assert_eq!(max_phyrexian_life_payments(&state, PlayerId(0)), 0);
        boards.push(state.clone());

        // CR 119.8: can't lose life zeroes the general budget too.
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        assert_eq!(max_payable_life(&state, PlayerId(0)), 0);
        boards.push(state);

        for state in &boards {
            let budget = max_payable_life(state, PlayerId(0));
            assert!(can_pay_life_cost(state, PlayerId(0), budget));
            assert!(budget == u32::MAX || !can_pay_life_cost(state, PlayerId(0), budget + 1));

            let cast_budget = max_payable_life_cast_or_activation(state, PlayerId(0));
            assert!(can_pay_life_cast_or_activation_cost(
                state,
                PlayerId(0),
                cast_budget
            ));
            assert!(
                cast_budget == u32::MAX
                    || !can_pay_life_cast_or_activation_cost(state, PlayerId(0), cast_budget + 1)
            );
        }
    }

    /// CR 119.4 + CR 119.4a + CR 810.9: paying life at the `i32` cap is exact.
    /// In Two-Headed Giant the payment lands on the payer's own life field
    /// while the team's exact total covers it; off-team a payment one past the
    /// cap is refused at the affordability gate with nothing changed.
    #[test]
    fn pay_life_at_the_cap_is_exact() {
        let mut state = two_headed_giant_with_team_a_life(i32::MAX, i32::MAX);
        let mut events = Vec::new();
        let result = pay_life_as_cost(&mut state, PlayerId(0), i32::MAX as u32, &mut events);
        assert_eq!(
            result,
            PayLifeCostResult::Paid {
                amount: i32::MAX as u32
            }
        );
        assert_eq!(state.players[0].life, 0);
        assert_eq!(state.players[1].life, i32::MAX);
        assert_eq!(
            crate::game::players::team_life_sum(&state, PlayerId(0)),
            i64::from(i32::MAX)
        );
        assert_eq!(
            crate::game::players::team_life_total(&state, PlayerId(0)),
            i32::MAX
        );

        let mut state = GameState::new_two_player(42);
        state.players[0].life = i32::MAX;
        let mut events = Vec::new();
        let result = pay_life_as_cost(&mut state, PlayerId(0), i32::MAX as u32, &mut events);
        assert_eq!(
            result,
            PayLifeCostResult::Paid {
                amount: i32::MAX as u32
            }
        );
        assert_eq!(state.players[0].life, 0);
        assert_eq!(state.players[0].life_lost_this_turn, i32::MAX as u32);
        assert!(events.iter().any(|event| matches!(
            event,
            GameEvent::LifeChanged {
                player_id: PlayerId(0),
                amount: -2_147_483_647,
                ..
            }
        )));

        let mut state = GameState::new_two_player(42);
        state.players[0].life = i32::MAX;
        let mut events = Vec::new();
        let result = pay_life_as_cost(&mut state, PlayerId(0), 2_147_483_648, &mut events);
        assert_eq!(result, PayLifeCostResult::InsufficientLife);
        assert_eq!(state.players[0].life, i32::MAX);
        assert!(
            events.is_empty(),
            "no events for a refused payment: {events:?}"
        );
    }
}

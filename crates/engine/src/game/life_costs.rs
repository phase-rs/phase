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
//!   [`effects::life::apply_damage_life_loss`] which runs the replacement pipeline.
//! - **CR 119.4b** — Players can always pay 0 life, even under CantLoseLife.
//! - **CR 119.8** — "A cost that involves having that player pay life can't be paid."

use crate::game::effects::life::{apply_damage_life_loss, ReplacementDeferred};
use crate::game::static_abilities::player_has_cant_lose_life;
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

/// Outcome of attempting to pay life as a cost.
///
/// CR 119.4b: Paying 0 life always succeeds — even under `CantLoseLife` — and
/// is represented by `Paid { amount: 0 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayLifeCostResult {
    /// The full amount was deducted (or amount was 0, always payable per CR 119.4b).
    Paid { amount: u32 },
    /// CR 118.3: Player's life total is below `amount` — cost can't be paid.
    InsufficientLife,
    /// CR 119.8: Player has `CantLoseLife` — cost can't be paid.
    LockedCantLoseLife,
}

impl PayLifeCostResult {
    /// Returns `true` if the cost was successfully paid (including the zero-life case).
    pub fn is_paid(self) -> bool {
        matches!(self, PayLifeCostResult::Paid { .. })
    }

    /// Returns `true` if the cost was NOT paid (insufficient life or locked).
    pub fn is_unpayable(self) -> bool {
        !self.is_paid()
    }
}

/// CR 118.3 + CR 119.4b + CR 119.8: Pure predicate — can this player pay `amount` life?
///
/// Used by pre-validation paths (`can_activate_ability_now`, Defiler offer filtering,
/// legal-action generation) to avoid presenting an unpayable cost to the player.
pub fn can_pay_life_cost(state: &GameState, player: PlayerId, amount: u32) -> bool {
    // CR 119.4b: 0 life is always payable, even under CantLoseLife.
    if amount == 0 {
        return true;
    }
    // CR 119.8: "a cost that involves having that player pay life can't be paid."
    if player_has_cant_lose_life(state, player) {
        return false;
    }
    // CR 118.3: life total must be at least the amount to be paid.
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| p.life >= amount as i32)
}

/// CR 118.3b + CR 119.4 + CR 119.8: Pay `amount` life from `player` as a cost.
///
/// Routes the life deduction through [`apply_damage_life_loss`] per CR 119.4
/// ("paying life IS losing life"), so the replacement pipeline and the
/// `CantLoseLife` short-circuit run consistently with every other life-loss event.
///
/// Returns a [`PayLifeCostResult`] describing the outcome. The caller is
/// responsible for translating an unpayable result into the appropriate
/// failure signal for their context (cost-payment flag, `EngineError`, etc.).
///
/// Defense in depth: the lock check happens here AND inside
/// `apply_damage_life_loss`. This module is also called from pre-validation
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
        return PayLifeCostResult::LockedCantLoseLife;
    }

    // CR 118.3: Resource check — must have enough life.
    let has_life = state
        .players
        .iter()
        .find(|p| p.id == player)
        .is_some_and(|p| p.life >= amount as i32);
    if !has_life {
        return PayLifeCostResult::InsufficientLife;
    }

    // CR 119.4: Pay life is life loss — route through the damage/life-loss helper
    // so the replacement pipeline fires. A `ReplacementDeferred` here (CR 614.7
    // multiple competing replacements) is not reachable in practice for pay-life
    // costs; if it ever occurs, treat it as unpayable rather than half-pay.
    match apply_damage_life_loss(state, player, amount, events) {
        Ok(_) => PayLifeCostResult::Paid { amount },
        Err(ReplacementDeferred) => PayLifeCostResult::LockedCantLoseLife,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::game::zones::create_object;
    use crate::types::ability::{
        ControllerRef, StaticDefinition, TargetFilter, TypedFilter,
    };
    use crate::types::identifiers::CardId;
    use crate::types::statics::StaticMode;
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

    /// CR 119.8: CantLoseLife → cost can't be paid; life total unchanged.
    #[test]
    fn pay_life_locked_returns_locked() {
        let mut state = GameState::new_two_player(42);
        add_cant_lose_life_permanent(&mut state, PlayerId(0));
        let mut events = Vec::new();

        let result = pay_life_as_cost(&mut state, PlayerId(0), 3, &mut events);

        assert_eq!(result, PayLifeCostResult::LockedCantLoseLife);
        assert_eq!(state.players[0].life, 20);
        assert!(events.is_empty());
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
}

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
//! - **CR 119.4b** — Players can always pay 0 life, even under a pay-life prohibition.
//! - **CR 119.8** — "A cost that involves having that player pay life can't be paid."

use crate::game::effects::life::{apply_damage_life_loss, ReplacementDeferred};
use crate::game::static_abilities::{player_cant_pay_life_as_cost, player_has_cant_lose_life};
use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;

/// Outcome of attempting to pay life as a cost.
///
/// CR 119.4b: Paying 0 life always succeeds — even under a prohibition — and
/// is represented by `Paid { amount: 0 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PayLifeCostResult {
    /// The full amount was deducted (or amount was 0, always payable per CR 119.4b).
    Paid { amount: u32 },
    /// CR 118.3: Player's life total is below `amount` — cost can't be paid.
    InsufficientLife,
    /// CR 119.8 / CR 118.3: A static prohibition makes the cost unpayable.
    Prohibited,
}

impl PayLifeCostResult {
    /// Returns `true` if the cost was successfully paid (including the zero-life case).
    pub fn is_paid(self) -> bool {
        matches!(self, PayLifeCostResult::Paid { .. })
    }

    /// Returns `true` if the cost was NOT paid (insufficient life or prohibited).
    pub fn is_unpayable(self) -> bool {
        !self.is_paid()
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
/// Routes the life deduction through [`apply_damage_life_loss`] per CR 119.4
/// ("paying life IS losing life"), so the replacement pipeline and the
/// `CantLoseLife` short-circuit run consistently with every other life-loss event.
///
/// Returns a [`PayLifeCostResult`] describing the outcome. The caller is
/// responsible for translating an unpayable result into the appropriate
/// failure signal for their context (cost-payment flag, `EngineError`, etc.).
///
/// Defense in depth: the prohibition check happens here AND inside
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
    // so the replacement pipeline fires. A `ReplacementDeferred` here (CR 614.7
    // multiple competing LifeLoss replacements requiring a player choice) is
    // not reachable in practice for pay-life costs — no current replacement
    // effect intercepts cost-path life loss. If that ever ships, the
    // cost-payment flow would need a new WaitingFor round-trip; for now,
    // report the cost as unpayable to avoid silently half-applying.
    match apply_damage_life_loss(state, player, amount, events) {
        Ok(_) => PayLifeCostResult::Paid { amount },
        Err(ReplacementDeferred) => {
            debug_assert!(
                false,
                "pay_life_as_cost: unexpected ReplacementDeferred during cost payment"
            );
            PayLifeCostResult::InsufficientLife
        }
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
    use crate::types::ability::{ControllerRef, StaticDefinition, TargetFilter, TypedFilter};
    use crate::types::format::FormatConfig;
    use crate::types::identifiers::CardId;
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
}

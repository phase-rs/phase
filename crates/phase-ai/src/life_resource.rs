//! Life as a spendable resource in combat decisions.
//!
//! The combat AI used to value a point of life the same at any life total, so
//! a point was worth the same at 20 life as at 4. That ignores the thing that
//! makes life a resource: it only matters in proportion to how close the
//! player is to losing it all (CR 104.3b + CR 704.5a). What a loss really
//! costs is *time*. The opposing board takes some number of turns to finish the
//! player, and a loss that shortens that clock is expensive. The same loss
//! when the clock is long is nearly free.
//!
//! Prices are in the combat math's creature-value units (`1.5 * power +
//! toughness`, so a 2/2 is 5.0), so a life cost can be weighed directly
//! against the body a block would spend.

use engine::game::players;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

/// Floor price of one life point, the cost even at a very high total.
const LIFE_POINT_BASE: f64 = 0.3;
/// Scale of the clock term: the price of the change in "fraction of the
/// opposing board's attack the remaining life can absorb".
const CLOCK_SCALE: f64 = 8.0;
/// Price of a loss that takes the player to 0 or less life. It is finite so
/// sums stay well-ordered, and large enough that no body is worth more.
pub(crate) const LETHAL_LIFE_COST: f64 = 1000.0;

/// CR 119.3 + CR 120.3a: the cost to `player` of losing `amount` life now, in
/// combat-value units. A loss that reaches 0 (CR 704.5a) costs
/// [`LETHAL_LIFE_COST`].
///
/// With `P` the opposing board's power and `L` the current life, `L / P` is
/// the number of turns the player can survive that board. The cost is a flat
/// per-point floor plus `CLOCK_SCALE * P * (1 / (L - amount) - 1 / L)`, the
/// increase in how much of the player's life one full swing takes. That term is
/// tiny at a healthy total (2 damage at 20 against 4 power is about 0.2) and
/// grows sharply as the loss eats into the last turn or two of the clock
/// (2 damage at 4 against 4 power is 8.0, more than a 2/2 body).
///
/// `stabilize_bias` is the profile's defensive bias (`AiProfile::stabilize_bias`).
/// It scales the clock term, so a more defensive profile values life higher.
pub(crate) fn life_loss_cost(
    state: &GameState,
    player: PlayerId,
    amount: i32,
    stabilize_bias: f64,
) -> f64 {
    if amount <= 0 {
        return 0.0;
    }
    let life = state.players[player.0 as usize].life;
    if amount >= life {
        return LETHAL_LIFE_COST;
    }
    // An empty opposing board still gets a minimal clock: creatures arrive.
    let power = opposing_power(state, player).max(1.0);
    let clock = power * (1.0 / f64::from(life - amount) - 1.0 / f64::from(life));
    f64::from(amount) * LIFE_POINT_BASE + CLOCK_SCALE * stabilize_bias.max(0.0) * clock
}

/// Total power of every creature the player's opponents control, tapped or
/// not: it is next turn's attack as much as this one's.
fn opposing_power(state: &GameState, player: PlayerId) -> f64 {
    let opponents = players::opponents(state, player);
    let power: i32 = state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj| {
            opponents.contains(&obj.controller)
                && obj.card_types.core_types.contains(&CoreType::Creature)
        })
        .map(|obj| obj.power.unwrap_or(0).max(0))
        .sum();
    f64::from(power)
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::zones::create_object;
    use engine::types::identifiers::CardId;
    use engine::types::zones::Zone;

    fn state_with(life: i32, opposing_powers: &[i32]) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.players[0].life = life;
        for (i, &power) in opposing_powers.iter().enumerate() {
            let id = create_object(
                &mut state,
                CardId(500 + i as u64),
                PlayerId(1),
                "Bear".to_string(),
                Zone::Battlefield,
            );
            let obj = state.objects.get_mut(&id).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            obj.power = Some(power);
            obj.toughness = Some(power);
        }
        state
    }

    #[test]
    fn two_damage_is_cheap_at_a_healthy_total_and_dear_near_death() {
        let healthy = life_loss_cost(&state_with(20, &[2, 2]), PlayerId(0), 2, 1.0);
        let low = life_loss_cost(&state_with(4, &[2, 2]), PlayerId(0), 2, 1.0);
        // A 2/2 body is worth 5.0 in combat-value units.
        assert!(healthy < 2.0, "2 damage at 20 life is minor, got {healthy}");
        assert!(low > 5.0, "2 damage at 4 life outweighs a 2/2, got {low}");
    }

    #[test]
    fn marginal_cost_rises_monotonically_as_life_falls() {
        let costs: Vec<f64> = (2..=20)
            .rev()
            .map(|life| life_loss_cost(&state_with(life, &[3]), PlayerId(0), 1, 1.0))
            .collect();
        assert!(costs.windows(2).all(|pair| pair[0] < pair[1]));
    }

    #[test]
    fn lethal_loss_is_priced_above_any_body() {
        let state = state_with(3, &[3]);
        assert_eq!(
            life_loss_cost(&state, PlayerId(0), 3, 1.0),
            LETHAL_LIFE_COST
        );
        assert_eq!(life_loss_cost(&state, PlayerId(0), 0, 1.0), 0.0);
    }

    #[test]
    fn a_bigger_opposing_board_makes_life_dearer() {
        let small = life_loss_cost(&state_with(8, &[1]), PlayerId(0), 2, 1.0);
        let big = life_loss_cost(&state_with(8, &[4, 4]), PlayerId(0), 2, 1.0);
        assert!(big > small);
    }
}

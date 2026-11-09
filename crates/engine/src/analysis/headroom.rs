//! How many repetitions of a period the engine can REPRESENT: the player-resource scalars a
//! repetition edits are stored in fixed-width integers, and a final edit whose result does not fit
//! is refused (`effects::life::LifeChangeUnrepresentable`).
//!
//! An engine limit, never a rules one, so nothing here wears a CR number as its reason: CR 119.3
//! ("that player's life total is adjusted accordingly") and CR 122.1 set no maximum. What the
//! limit governs is the loop-shortcut offer, which may state only a count its take can perform
//! (`game::engine::build_shortcut_schema`); without it an offer states a count whose take is then
//! rejected.

use std::collections::BTreeMap;

use crate::types::events::GameEvent;
use crate::types::game_state::GameState;
use crate::types::player::{Player, PlayerCounterKind, PlayerId};

/// One player-resource scalar a final player edit writes — the fields
/// `GameState::apply_resolved_player_edit` checks for overflow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum PlayerScalar {
    /// CR 119.1: the life total, an `i32`.
    Life,
    /// The per-turn life-gained tally a gain adds to, a `u32`.
    LifeGainedThisTurn,
    /// The per-turn life-lost tally a loss adds to, a `u32`.
    LifeLostThisTurn,
    /// CR 107.14 + CR 122.1: energy counters, a `u32`.
    Energy,
    /// CR 122.1: a player counter of this kind, a `u32`.
    Counter(PlayerCounterKind),
}

impl PlayerScalar {
    fn read(self, player: &Player) -> i64 {
        match self {
            PlayerScalar::Life => i64::from(player.life),
            PlayerScalar::LifeGainedThisTurn => i64::from(player.life_gained_this_turn),
            PlayerScalar::LifeLostThisTurn => i64::from(player.life_lost_this_turn),
            PlayerScalar::Energy => i64::from(player.energy),
            PlayerScalar::Counter(kind) => i64::from(player.player_counter(&kind)),
        }
    }

    /// The values the stored type holds, as the overflow check reads them. A `u32` scalar has no
    /// floor here: removing more than is there is a rules question (the cost cannot be paid),
    /// not an unrepresentable result.
    fn range(self) -> (Option<i64>, i64) {
        match self {
            PlayerScalar::Life => (Some(i64::from(i32::MIN)), i64::from(i32::MAX)),
            PlayerScalar::LifeGainedThisTurn
            | PlayerScalar::LifeLostThisTurn
            | PlayerScalar::Energy
            | PlayerScalar::Counter(_) => (None, i64::from(u32::MAX)),
        }
    }
}

/// How one repetition moves a scalar, as offsets from its value where the repetition starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Excursion {
    /// Where it ends.
    net: i64,
    /// The highest it reaches on the way.
    highest: i64,
    /// The lowest it reaches on the way.
    lowest: i64,
}

impl Excursion {
    fn step(&mut self, delta: i64) {
        self.net += delta;
        self.highest = self.highest.max(self.net);
        self.lowest = self.lowest.min(self.net);
    }

    /// The repetitions from `value` before a step leaves `range`; `None` when none ever does.
    ///
    /// Repetition `k` (from 1) starts at `value + (k - 1) * net` and spans `highest` above and
    /// `lowest` below that start, so the start moves only one way and only one end of the range
    /// can bind once the first repetition fits.
    fn repetitions(self, value: i64, (floor, ceiling): (Option<i64>, i64)) -> Option<u32> {
        let fits_above = |start: i64| start + self.highest <= ceiling;
        let fits_below = |start: i64| floor.is_none_or(|floor| start + self.lowest >= floor);
        if !fits_above(value) || !fits_below(value) {
            return Some(0);
        }
        let after_first = match self.net {
            0 => return None,
            net if net > 0 => (ceiling - value - self.highest) / net,
            // A falling scalar with no floor never leaves its range.
            net => (value + self.lowest - floor?) / -net,
        };
        Some(u32::try_from(after_first.saturating_add(1)).unwrap_or(u32::MAX))
    }
}

/// The player-resource excursions of one performed repetition, read off the events it emitted.
///
/// The events are the measure because the overflow check runs at every final edit, and a
/// priority frame can sit on either side of several of them: a repetition that gains and then
/// loses inside one resolution peaks between frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct PeriodExcursions {
    by_scalar: BTreeMap<(PlayerId, PlayerScalar), Excursion>,
}

impl PeriodExcursions {
    /// Folds one repetition's events, in the order they were emitted.
    pub(crate) fn of_events(events: &[GameEvent]) -> Self {
        let mut excursions = Self::default();
        for event in events {
            match event {
                GameEvent::LifeChanged {
                    player_id, amount, ..
                } => {
                    let amount = i64::from(*amount);
                    excursions.step(*player_id, PlayerScalar::Life, amount);
                    let tally = if amount > 0 {
                        PlayerScalar::LifeGainedThisTurn
                    } else {
                        PlayerScalar::LifeLostThisTurn
                    };
                    excursions.step(*player_id, tally, amount.abs());
                }
                GameEvent::EnergyChanged { player, delta } => {
                    excursions.step(*player, PlayerScalar::Energy, i64::from(*delta));
                }
                GameEvent::PlayerCounterChanged {
                    player,
                    counter_kind,
                    delta,
                } => {
                    excursions.step(
                        *player,
                        PlayerScalar::Counter(*counter_kind),
                        i64::from(*delta),
                    );
                }
                _ => {}
            }
        }
        excursions
    }

    fn step(&mut self, player: PlayerId, scalar: PlayerScalar, delta: i64) {
        if delta != 0 {
            self.by_scalar
                .entry((player, scalar))
                .or_default()
                .step(delta);
        }
    }

    /// The repetitions of this period that `state` can represent, the fewest any scalar allows;
    /// `None` when no scalar it moves ever leaves its stored range.
    ///
    /// Read on the board the repetitions start from. The per-turn tallies reset at a turn's
    /// start, so a period that crosses a turn is counted as if they did not; that can only
    /// understate the count.
    pub(crate) fn representable_repetitions(&self, state: &GameState) -> Option<u32> {
        self.by_scalar
            .iter()
            .filter_map(|(&(player, scalar), excursion)| {
                let value = scalar.read(state.players.iter().find(|p| p.id == player)?);
                excursion.repetitions(value, scalar.range())
            })
            .min()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P0: PlayerId = PlayerId(0);

    fn life(amount: i32) -> GameEvent {
        GameEvent::LifeChanged {
            player_id: P0,
            amount,
            new_total: Default::default(),
        }
    }

    fn state_at(life: i32) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.players[0].life = life;
        state
    }

    #[test]
    fn a_net_gain_counts_the_repetitions_up_to_the_life_limit() {
        let excursions = PeriodExcursions::of_events(&[life(2)]);
        assert_eq!(
            excursions.representable_repetitions(&state_at(i32::MAX - 1498)),
            Some(749)
        );
        assert_eq!(
            excursions.representable_repetitions(&state_at(i32::MAX - 2)),
            Some(1)
        );
        assert_eq!(
            excursions.representable_repetitions(&state_at(i32::MAX - 1)),
            Some(0)
        );
    }

    /// A repetition that gains 3 and then loses 1 peaks above where it ends, and the overflow
    /// check runs at the peak.
    #[test]
    fn the_peak_inside_a_repetition_binds_and_not_its_net() {
        let excursions = PeriodExcursions::of_events(&[life(3), life(-1)]);
        // Starts MAX-6, MAX-4, MAX-2 peak at MAX-3, MAX-1, MAX+1: two fit.
        assert_eq!(
            excursions.representable_repetitions(&state_at(i32::MAX - 6)),
            Some(2)
        );
    }

    #[test]
    fn a_net_loss_counts_the_repetitions_down_to_the_life_floor() {
        let excursions = PeriodExcursions::of_events(&[life(-5)]);
        assert_eq!(
            excursions.representable_repetitions(&state_at(i32::MIN + 12)),
            Some(2)
        );
    }

    /// A gain and an equal loss leave life where it was, but each repetition still adds to both
    /// per-turn tallies.
    #[test]
    fn a_life_neutral_repetition_is_bounded_by_the_per_turn_tallies() {
        let excursions = PeriodExcursions::of_events(&[life(4), life(-4)]);
        let mut state = state_at(20);
        assert_eq!(
            excursions.representable_repetitions(&state),
            Some(u32::MAX / 4)
        );
        state.players[0].life_gained_this_turn = u32::MAX - 8;
        assert_eq!(excursions.representable_repetitions(&state), Some(2));
    }

    #[test]
    fn energy_and_player_counters_are_bounded_at_their_stored_width() {
        let excursions = PeriodExcursions::of_events(&[
            GameEvent::EnergyChanged {
                player: P0,
                delta: 3,
            },
            GameEvent::PlayerCounterChanged {
                player: P0,
                counter_kind: PlayerCounterKind::Experience,
                delta: 1,
            },
        ]);
        let mut state = state_at(20);
        state.players[0].energy = u32::MAX - 7;
        assert_eq!(excursions.representable_repetitions(&state), Some(2));
        state.players[0].energy = 0;
        state.players[0]
            .player_counters
            .insert(PlayerCounterKind::Experience, u32::MAX - 4);
        assert_eq!(excursions.representable_repetitions(&state), Some(4));
    }

    /// Spending what a repetition then restores never leaves the stored range, and a period that
    /// moves no player resource states no limit at all.
    #[test]
    fn a_period_that_returns_its_resources_has_no_limit() {
        let restored = PeriodExcursions::of_events(&[
            GameEvent::EnergyChanged {
                player: P0,
                delta: -2,
            },
            GameEvent::EnergyChanged {
                player: P0,
                delta: 2,
            },
        ]);
        let mut state = state_at(20);
        state.players[0].energy = 2;
        assert_eq!(restored.representable_repetitions(&state), None);
        assert_eq!(
            PeriodExcursions::default().representable_repetitions(&state),
            None
        );
    }
}

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
    /// The per-turn tally of energy paid or lost that every energy decrement adds to, a `u32`.
    EnergyPaidOrLostThisTurn,
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
            PlayerScalar::EnergyPaidOrLostThisTurn => {
                i64::from(player.energy_paid_or_lost_this_turn)
            }
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
            | PlayerScalar::EnergyPaidOrLostThisTurn
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

    /// Whether a repetition starting at `start` stays on the near side of `limit`.
    fn fits(self, start: i64, limit: Limit, (floor, ceiling): (Option<i64>, i64)) -> bool {
        match limit {
            Limit::Ceiling => start.saturating_add(self.highest) <= ceiling,
            Limit::Floor => floor.is_none_or(|floor| start.saturating_add(self.lowest) >= floor),
        }
    }

    /// The `k`th repetition (from 1) of a period whose first two are `first` and `second`, as
    /// seen from `limit`. A change between them that moves toward `limit` recurs, so a period
    /// whose effect grows by a fixed amount (a gain per creature while each repetition makes a
    /// creature) keeps growing by it; a change that moves away from `limit` is taken not to
    /// recur, because it may be a one-off of the first repetition (a "first time each turn"
    /// trigger). Either way the count can only be understated.
    fn nth(first: Self, second: Self, k: i64, limit: Limit) -> Self {
        if k == 1 {
            return first;
        }
        let grown = |a: i64, b: i64| {
            let change = match limit {
                Limit::Ceiling => (b - a).max(0),
                Limit::Floor => (b - a).min(0),
            };
            b.saturating_add(change.saturating_mul(k - 2))
        };
        let net = grown(first.net, second.net);
        Self {
            net,
            highest: grown(first.highest, second.highest).max(net).max(0),
            lowest: grown(first.lowest, second.lowest).min(net).min(0),
        }
    }
}

/// Which end of a scalar's stored range a trajectory is stepped toward.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Limit {
    Ceiling,
    Floor,
}

/// The player-resource excursions of one performed repetition, read off the events it emitted.
///
/// The events are the measure because the overflow check runs at every final edit, and a
/// priority frame can sit on either side of several of them: a repetition that gains and then
/// loses inside one resolution peaks between frames.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PeriodExcursions {
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
                    let delta = i64::from(*delta);
                    excursions.step(*player, PlayerScalar::Energy, delta);
                    if delta < 0 {
                        excursions.step(*player, PlayerScalar::EnergyPaidOrLostThisTurn, -delta);
                    }
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

    /// The player-resource excursions of one performed repetition, read off the frames of a
    /// period in the loop-detection ring (newest last).
    pub(crate) fn of_frames(frames: &[&GameState]) -> Self {
        let Some(&first) = frames.first() else {
            return Self::default();
        };
        let counter_kinds: std::collections::BTreeSet<_> = frames
            .iter()
            .flat_map(|f| {
                f.players
                    .iter()
                    .flat_map(|p| p.player_counters.keys().copied())
            })
            .chain(std::iter::once(PlayerCounterKind::Poison))
            .collect();
        let mut excursions = Self::default();
        for player in &first.players {
            let scalars = [
                PlayerScalar::Life,
                PlayerScalar::LifeGainedThisTurn,
                PlayerScalar::LifeLostThisTurn,
                PlayerScalar::Energy,
                PlayerScalar::EnergyPaidOrLostThisTurn,
            ]
            .into_iter()
            .chain(counter_kinds.iter().copied().map(PlayerScalar::Counter));
            for scalar in scalars {
                let start = scalar.read(player);
                let mut highest = 0i64;
                let mut lowest = 0i64;
                for frame in frames {
                    if let Some(p) = frame.players.iter().find(|p| p.id == player.id) {
                        let offset = scalar.read(p) - start;
                        highest = highest.max(offset);
                        lowest = lowest.min(offset);
                    }
                }
                if scalar == PlayerScalar::Life {
                    if let Some(last_p) = frames
                        .last()
                        .and_then(|last| last.players.iter().find(|p| p.id == player.id))
                    {
                        let delta_gained = i64::from(
                            last_p
                                .life_gained_this_turn
                                .saturating_sub(player.life_gained_this_turn),
                        );
                        let delta_lost = i64::from(
                            last_p
                                .life_lost_this_turn
                                .saturating_sub(player.life_lost_this_turn),
                        );
                        highest = highest.max(delta_gained);
                        lowest = lowest.min(-delta_lost);
                    }
                } else if scalar == PlayerScalar::Energy {
                    // Conservative peak bound for energy: the total energy gained during the period
                    // is net_change + energy_paid_or_lost_change. This captures intra-frame peaks
                    // where energy is gained then paid within a single resolution (frame sampling
                    // only sees the net). The peak cannot exceed start + total_gained.
                    if let Some(last_p) = frames
                        .last()
                        .and_then(|last| last.players.iter().find(|p| p.id == player.id))
                    {
                        let delta_paid = i64::from(
                            last_p
                                .energy_paid_or_lost_this_turn
                                .saturating_sub(player.energy_paid_or_lost_this_turn),
                        );
                        let net = last_p.energy as i64 - player.energy as i64;
                        let total_gained = net + delta_paid;
                        if total_gained > 0 {
                            highest = highest.max(total_gained);
                        }
                    }
                }
                let net = frames
                    .last()
                    .and_then(|last| last.players.iter().find(|p| p.id == player.id))
                    .map_or(0, |last_p| scalar.read(last_p) - start);
                if net != 0 || highest != 0 || lowest != 0 {
                    excursions.by_scalar.insert(
                        (player.id, scalar),
                        Excursion {
                            net,
                            highest,
                            lowest,
                        },
                    );
                }
            }
        }
        excursions
    }

    /// Repetitions that `state` can represent when the cycle seen in `frames` repeats unchanged.
    pub(crate) fn representable_over_frames(
        frames: &[&GameState],
        state: &GameState,
        budget: u32,
    ) -> Option<u32> {
        let cycle = Self::of_frames(frames);
        Self::representable_repetitions(&[cycle.clone(), cycle], state, budget)
    }

    /// The player-resource excursions of iteration `iteration` of a declared period,
    /// constructing seat life charges from `per_cycle.declared_seat_life_charges` so
    /// declared victim relocations are accounted for in headroom analysis.
    pub(crate) fn of_declared_iteration(
        per_cycle: &crate::analysis::resource::PeriodicDelta,
        declaration: Option<&crate::analysis::decision_template::DecisionTemplate>,
        observed: Option<&crate::analysis::decision_template::DecisionTemplate>,
        points: &[crate::analysis::decision_template::DecisionPoint],
        state: &GameState,
        lead: crate::analysis::resource::AnnouncedLead<'_>,
        iteration: crate::analysis::decision_template::IterationIndex,
    ) -> Self {
        let mut excursions = Self::default();
        for player in &state.players {
            let charge = per_cycle
                .declared_seat_life_charges(
                    player.id,
                    declaration,
                    observed,
                    points,
                    state,
                    crate::analysis::resource::ChargeBound::Ceiling,
                    lead,
                )
                .nth(iteration as usize)
                .unwrap_or(crate::analysis::resource::DeclaredLifeCharge { net: 0, dip: 0 });
            if charge.net != 0 || charge.dip != 0 {
                excursions.by_scalar.insert(
                    (player.id, PlayerScalar::Life),
                    Excursion {
                        net: -charge.net,
                        highest: 0,
                        lowest: -charge.dip,
                    },
                );
                excursions.by_scalar.insert(
                    (player.id, PlayerScalar::LifeLostThisTurn),
                    Excursion {
                        net: charge.net,
                        highest: charge.dip.max(charge.net),
                        lowest: 0,
                    },
                );
            }
            if let Some(&gain) = per_cycle.delta.life.get(&player.id) {
                if gain > 0 {
                    excursions.step(player.id, PlayerScalar::Life, gain);
                    excursions.step(player.id, PlayerScalar::LifeGainedThisTurn, gain);
                }
            }
        }
        for (&player, &delta_poison) in &per_cycle.delta.poison {
            excursions.step(
                player,
                PlayerScalar::Counter(crate::types::player::PlayerCounterKind::Poison),
                delta_poison,
            );
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

    /// The repetitions, up to `budget`, of a period whose first two replayed repetitions are
    /// `cycles` that `state` can represent: the fewest any scalar allows, or `None` when every
    /// scalar fits the whole budget.
    ///
    /// Each scalar is stepped repetition by repetition from its value on `state`, the board the
    /// repetitions start from, under [`Excursion::nth`], once toward each end of its range. A
    /// period whose effect grows faster than by a fixed amount per repetition is outside what
    /// two repetitions show; its take still refuses the repetition that does not fit. The
    /// per-turn tallies reset at a turn's start, so a period that crosses a turn is counted as if
    /// they did not, which can only understate.
    pub(crate) fn representable_repetitions(
        cycles: &[Self; 2],
        state: &GameState,
        budget: u32,
    ) -> Option<u32> {
        let [first, second] = cycles;
        let scalars: std::collections::BTreeSet<_> = first
            .by_scalar
            .keys()
            .chain(second.by_scalar.keys())
            .copied()
            .collect();
        scalars
            .into_iter()
            .filter_map(|(player, scalar)| {
                let value = scalar.read(state.players.iter().find(|p| p.id == player)?);
                let [a, b] = [first, second].map(|cycle| {
                    cycle
                        .by_scalar
                        .get(&(player, scalar))
                        .copied()
                        .unwrap_or_default()
                });
                [Limit::Ceiling, Limit::Floor]
                    .into_iter()
                    .filter_map(|limit| {
                        let mut start = value;
                        (1..=budget).find_map(|k| {
                            let repetition = Excursion::nth(a, b, i64::from(k), limit);
                            if !repetition.fits(start, limit, scalar.range()) {
                                return Some(k - 1);
                            }
                            start = start.saturating_add(repetition.net);
                            None
                        })
                    })
                    .min()
            })
            .min()
    }

    /// CR 732.2a: Repetitions that `state` can represent when choices follow a schedule
    /// (e.g. piece-wise target switches across iterations), evaluating every schedule segment
    /// and the leading announcement so later pins cannot escape headroom bounds.
    pub fn representable_under_schedule(
        per_cycle: &crate::analysis::resource::PeriodicDelta,
        declaration: Option<&crate::analysis::decision_template::DecisionTemplate>,
        observed: Option<&crate::analysis::decision_template::DecisionTemplate>,
        points: &[crate::analysis::decision_template::DecisionPoint],
        state: &GameState,
        lead: crate::analysis::resource::AnnouncedLead<'_>,
        budget: u32,
    ) -> Option<u32> {
        if budget == 0 {
            return Some(0);
        }
        let mut switch_points = std::collections::BTreeSet::new();
        switch_points.insert(0);

        let collect_points =
            |template: &crate::analysis::decision_template::DecisionTemplate,
             switches: &mut std::collections::BTreeSet<u32>| {
                if let Some(pts) = template.resolution_change_points() {
                    switches.extend(pts.into_iter().filter(|&p| p < budget));
                } else {
                    for i in 0..budget {
                        switches.insert(i);
                    }
                }
            };

        if let Some(d) = declaration {
            collect_points(d, &mut switch_points);
        }
        if let Some(obs) = observed {
            if declaration != observed {
                collect_points(obs, &mut switch_points);
            }
        }
        for point in points {
            let rep_lead = lead.repetitions(&point.slot);
            if rep_lead > 0 && rep_lead < budget {
                switch_points.insert(rep_lead);
            }
        }
        for p in switch_points.clone() {
            for point in points {
                let rep_lead = lead.repetitions(&point.slot);
                if rep_lead > 0 && p.saturating_add(rep_lead) < budget {
                    switch_points.insert(p + rep_lead);
                }
            }
        }

        let mut current_values: BTreeMap<(PlayerId, PlayerScalar), i64> = BTreeMap::new();
        let counter_kinds: std::collections::BTreeSet<_> = state
            .players
            .iter()
            .flat_map(|p| p.player_counters.keys().copied())
            .chain(std::iter::once(PlayerCounterKind::Poison))
            .collect();
        let scalar_templates = [
            PlayerScalar::Life,
            PlayerScalar::LifeGainedThisTurn,
            PlayerScalar::LifeLostThisTurn,
            PlayerScalar::Energy,
            PlayerScalar::EnergyPaidOrLostThisTurn,
        ];
        for player in &state.players {
            let scalars = scalar_templates
                .iter()
                .copied()
                .chain(counter_kinds.iter().copied().map(PlayerScalar::Counter));
            for scalar in scalars {
                current_values.insert((player.id, scalar), scalar.read(player));
            }
        }

        let sorted_points: Vec<u32> = switch_points.into_iter().collect();
        for (i, &start_idx) in sorted_points.iter().enumerate() {
            let next_idx = sorted_points.get(i + 1).copied().unwrap_or(budget);
            let len = next_idx - start_idx;
            if len == 0 {
                continue;
            }
            let c0 = Self::of_declared_iteration(
                per_cycle,
                declaration,
                observed,
                points,
                state,
                lead,
                start_idx,
            );
            let c1 = if len > 1 {
                Self::of_declared_iteration(
                    per_cycle,
                    declaration,
                    observed,
                    points,
                    state,
                    lead,
                    start_idx + 1,
                )
            } else {
                c0.clone()
            };

            let scalars: std::collections::BTreeSet<_> = c0
                .by_scalar
                .keys()
                .chain(c1.by_scalar.keys())
                .copied()
                .collect();

            let mut segment_fit = len;
            for (player, scalar) in &scalars {
                let Some(&val) = current_values.get(&(*player, *scalar)) else {
                    continue;
                };
                let [a, b] = [&c0, &c1].map(|cycle| {
                    cycle
                        .by_scalar
                        .get(&(*player, *scalar))
                        .copied()
                        .unwrap_or_default()
                });
                for limit in [Limit::Ceiling, Limit::Floor] {
                    let mut start = val;
                    for k in 1..=len {
                        let rep = Excursion::nth(a, b, i64::from(k), limit);
                        if !rep.fits(start, limit, scalar.range()) {
                            segment_fit = segment_fit.min(k - 1);
                            break;
                        }
                        start = start.saturating_add(rep.net);
                    }
                }
            }

            if segment_fit < len {
                return Some(start_idx + segment_fit);
            }

            // Update current_values by net change of this segment
            for (player, scalar) in scalars {
                let [a, b] = [&c0, &c1].map(|cycle| {
                    cycle
                        .by_scalar
                        .get(&(player, scalar))
                        .copied()
                        .unwrap_or_default()
                });
                let mut net_total = 0i64;
                for k in 1..=len {
                    net_total = net_total
                        .saturating_add(Excursion::nth(a, b, i64::from(k), Limit::Ceiling).net);
                }
                if let Some(val) = current_values.get_mut(&(player, scalar)) {
                    *val = val.saturating_add(net_total);
                }
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const P0: PlayerId = PlayerId(0);
    const BUDGET: u32 = 1_000;

    fn life(amount: i32) -> GameEvent {
        GameEvent::LifeChanged {
            player_id: P0,
            amount,
            new_total: Default::default(),
        }
    }

    fn energy(delta: i32) -> GameEvent {
        GameEvent::EnergyChanged { player: P0, delta }
    }

    fn state_at(life: i32) -> GameState {
        let mut state = GameState::new_two_player(7);
        state.players[0].life = life;
        state
    }

    /// Two equal repetitions of `events`.
    fn constant(events: &[GameEvent], state: &GameState) -> Option<u32> {
        let cycle = PeriodExcursions::of_events(events);
        PeriodExcursions::representable_repetitions(&[cycle.clone(), cycle], state, BUDGET)
    }

    #[test]
    fn a_net_gain_counts_the_repetitions_up_to_the_life_limit() {
        assert_eq!(constant(&[life(2)], &state_at(i32::MAX - 1498)), Some(749));
        assert_eq!(constant(&[life(2)], &state_at(i32::MAX - 2)), Some(1));
        assert_eq!(constant(&[life(2)], &state_at(i32::MAX - 1)), Some(0));
        assert_eq!(constant(&[life(2)], &state_at(20)), None, "the budget fits");
    }

    /// A repetition that gains 3 and then loses 1 peaks above where it ends, and the overflow
    /// check runs at the peak.
    #[test]
    fn the_peak_inside_a_repetition_binds_and_not_its_net() {
        // Starts MAX-6, MAX-4, MAX-2 peak at MAX-3, MAX-1, MAX+1: two fit.
        assert_eq!(
            constant(&[life(3), life(-1)], &state_at(i32::MAX - 6)),
            Some(2)
        );
    }

    /// A gain that grows by one each repetition (2, 3, 4, ...) is stepped as it grows: from
    /// MAX-20 the totals 2, 5, 9, 14, 20 fit and the sixth (27) does not. Either repetition alone,
    /// extended as if constant, would state more.
    #[test]
    fn a_growing_gain_is_stepped_as_it_grows() {
        let cycles = [
            PeriodExcursions::of_events(&[life(2)]),
            PeriodExcursions::of_events(&[life(3)]),
        ];
        let state = state_at(i32::MAX - 20);
        assert_eq!(
            PeriodExcursions::representable_repetitions(&cycles, &state, BUDGET),
            Some(5)
        );
        assert_eq!(constant(&[life(3)], &state), Some(6));
    }

    /// A gain of 3, then 2 and 2 ever after (a one-off on the first repetition): the shrink
    /// between the replayed cycles is not carried forward. From MAX-10 the totals 3, 5, 7, 9 fit
    /// and the fifth (11) does not.
    #[test]
    fn a_shrinking_gain_is_not_carried_forward() {
        let cycles = [
            PeriodExcursions::of_events(&[life(3)]),
            PeriodExcursions::of_events(&[life(2)]),
        ];
        assert_eq!(
            PeriodExcursions::representable_repetitions(&cycles, &state_at(i32::MAX - 10), BUDGET),
            Some(4)
        );
    }

    #[test]
    fn a_net_loss_counts_the_repetitions_down_to_the_life_floor() {
        assert_eq!(constant(&[life(-5)], &state_at(i32::MIN + 12)), Some(2));
    }

    /// A gain and an equal loss leave life where it was, but each repetition still adds to both
    /// per-turn tallies.
    #[test]
    fn a_life_neutral_repetition_is_bounded_by_the_per_turn_tallies() {
        let events = [life(4), life(-4)];
        let mut state = state_at(20);
        assert_eq!(constant(&events, &state), None);
        state.players[0].life_gained_this_turn = u32::MAX - 8;
        assert_eq!(constant(&events, &state), Some(2));
        state.players[0].life_gained_this_turn = 0;
        state.players[0].life_lost_this_turn = u32::MAX - 4;
        assert_eq!(constant(&events, &state), Some(1));
    }

    #[test]
    fn energy_and_player_counters_are_bounded_at_their_stored_width() {
        let events = [
            energy(3),
            GameEvent::PlayerCounterChanged {
                player: P0,
                counter_kind: PlayerCounterKind::Experience,
                delta: 1,
            },
        ];
        let mut state = state_at(20);
        state.players[0].energy = u32::MAX - 7;
        assert_eq!(constant(&events, &state), Some(2));
        state.players[0].energy = 0;
        state.players[0]
            .player_counters
            .insert(PlayerCounterKind::Experience, u32::MAX - 4);
        assert_eq!(constant(&events, &state), Some(4));
    }

    /// Spending energy a repetition then restores leaves the energy total where it was, but
    /// each spend adds to the paid-or-lost tally; a period that moves no player resource states
    /// no limit at all.
    #[test]
    fn a_restored_energy_spend_is_bounded_only_by_its_tally() {
        let events = [energy(-2), energy(2)];
        let mut state = state_at(20);
        state.players[0].energy = 2;
        assert_eq!(constant(&events, &state), None);
        state.players[0].energy_paid_or_lost_this_turn = u32::MAX - 5;
        assert_eq!(constant(&events, &state), Some(2));
        assert_eq!(constant(&[], &state), None);
    }
}

//! Momir curve policy — when to activate a random-creature mana sink, and for
//! how much.
//!
//! Momir's Madness gives every player a game-start command-zone emblem reading
//! "{X}, Discard a card: Create a token that's a copy of a creature card with
//! mana value X chosen at random. Activate only as a sorcery and only once each
//! turn." (CR 707.2 copy semantics, CR 202.3 mana value, CR 701.9a discard.)
//! Without this policy the AI never activates it: the effect's polarity is
//! `Contextual` (`effect_classify.rs`), so no other policy has an opinion and
//! `PassPriority` wins by default.
//!
//! # The schedule
//!
//! The deck is 60 lands, so a player's land count equals their own turn count
//! and X is bounded by it. The default line is "spend the whole turn on the
//! sink", with the rung capped at 8 — mana past 8 is spent only when the pool
//! scoring below says the larger X is worth it:
//!
//! | own turn | 1 | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 9+ |
//! |---|---|---|---|---|---|---|---|---|---|
//! | on the play | — | — | 3 | 4 | 5 | 6 | 7 | 8 | 8 |
//! | on the draw | — | 2 | 3 | 4 | 5 | 6 | 7 | 8 | 8 |
//!
//! CR 103.8a: in a two-player game the player on the play skips their first
//! draw step, so they are a card behind all game. Every activation costs a
//! discard (CR 701.9a), so that player holds off one extra turn rather than
//! spending a card on a two-drop; the player on the draw, up a card, starts on
//! their second turn.
//!
//! # Moving ahead of the schedule
//!
//! The table is the floor, not the ceiling. When more mana is available than
//! the rung asks for — a mana creature, a rock, a Treasure — the policy weighs
//! every payable X at or above the rung and picks the best one:
//!
//! `score(X) = expected pool body at X − board value given up to pay X`
//!
//! - The expected body is the mean combat body of the creatures actually
//!   drawable at X, read from the loaded card database (`momir_pool`). An X
//!   with nothing drawable (CR 609.3: no token) is never picked.
//! - Mana from lands and other non-creature sources is free; the deck has
//!   nothing else to spend it on. Mana from a creature costs that creature's
//!   board value, scaled by `momir_curve_mana_creature_tap_weight` when the
//!   creature only has to TAP (CR 508.1a + CR 509.1a: attackers and blockers
//!   must be untapped, so it sits out a turn of combat), and at full value when
//!   paying with it means losing it. CR 118.3c: activating a mana ability is
//!   never mandatory, so the creature can always stay untapped.
//! - Ties go to the lower X, so creature mana is spent only for a real gain.
//!
//! So on the draw, a two-drop mana creature that can tap on the third turn
//! turns the 3 into a 4, and a large mana creature stays home to attack when
//! the next rung is not worth a turn of its combat. Past the cap of 8 the same
//! comparison decides whether a larger X is worth it; with the full card pool
//! the answer is usually yes. The exception is a mana value with no creatures
//! at all, which never gets picked.
//!
//! # Detection is structural, not by name
//!
//! The policy binds to `Effect::CreateTokenCopyFromPool` on the activated
//! ability, never to an emblem name or the format. Any future card or emblem
//! printing that effect gets the same treatment.
//!
//! # Turn accounting
//!
//! `GameState::turn_number` counts *player* turns, not rounds — it increments
//! on every turn change (`game/turns.rs`). In the two-player table Momir
//! mandates (`FormatConfig::momir` fixes `min_players == max_players == 2`),
//! a player's own turn index is therefore derived from `turn_number` and
//! whether they are `current_starting_player`.

use std::collections::HashSet;
use std::sync::Arc;

use engine::types::ability::{Comparator, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::momir_pool::{pool_profile, PoolProfile};
use super::mulligan::TurnOrder;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Highest rung of the schedule. Past this the table stops climbing: a larger X
/// is taken only when the pool scoring (see the module docs) finds the pool
/// there better than the pool at the rung.
pub const MAX_SCHEDULED_X: u32 = 8;

/// First own-turn on which the player on the play activates. CR 103.8a: they
/// skipped a draw step, so they are a card down and each activation costs a
/// discard (CR 701.9a).
pub const ON_PLAY_FIRST_TURN: u32 = 3;

/// First own-turn on which the player on the draw activates.
pub const ON_DRAW_FIRST_TURN: u32 = 2;

/// CR 202.3: Emrakul, the Aeons Torn's mana value.
///
/// Once a player can actually pay 15, the schedule abandons its cap and rolls
/// at 15 every turn until it hits Emrakul. At that mana value the eligible pool
/// is only five creatures wide, so each activation is roughly a one-in-five
/// shot at the best creature in the format — a gamble worth repeating, and one
/// that stops being worth it the moment it pays off.
pub const EMRAKUL_MANA_VALUE: u32 = 15;

/// The prize the 15-mana line is hunting. Matched by name because that is what
/// "until Emrakul is found" means — this is an identity lookup against a token
/// the draw already created, not a structural classification standing in for
/// one. Nothing else in this policy matches on a name.
pub const EMRAKUL_NAME: &str = "Emrakul, the Aeons Torn";

pub struct MomirCurvePolicy;

/// Whether `player` is on the play or on the draw.
fn turn_order(state: &GameState, player: PlayerId) -> TurnOrder {
    if state.current_starting_player == player {
        TurnOrder::OnPlay
    } else {
        TurnOrder::OnDraw
    }
}

/// The player's own turn index (their Nth turn), derived from the shared
/// `turn_number` player-turn counter on a two-player table.
///
/// `turn_number` is 1-based and increments per player turn, so the player on
/// the play owns the odd turns and the player on the draw the even ones.
fn own_turn_index(state: &GameState, player: PlayerId) -> u32 {
    match turn_order(state, player) {
        TurnOrder::OnPlay => state.turn_number.div_ceil(2),
        TurnOrder::OnDraw => state.turn_number / 2,
    }
}

/// First own-turn this player activates on.
fn first_activation_turn(state: &GameState, player: PlayerId) -> u32 {
    match turn_order(state, player) {
        TurnOrder::OnPlay => ON_PLAY_FIRST_TURN,
        TurnOrder::OnDraw => ON_DRAW_FIRST_TURN,
    }
}

/// Whether `player` already controls an Emrakul token, which ends the 15-mana
/// hunt.
///
/// A battlefield scan, so it runs LAST: every caller reaches it only after the
/// cheap turn check and the `affordable >= EMRAKUL_MANA_VALUE` test, and only
/// for a candidate already confirmed to be the pool sink.
///
/// "Found" is read as "controls one now" rather than "ever created one": if the
/// Emrakul is answered, the hunt is worth resuming, and no ever-created ledger
/// exists to consult without adding one.
fn controls_emrakul(state: &GameState, player: PlayerId) -> bool {
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .any(|object| object.controller == player && object.name == EMRAKUL_NAME)
}

/// The schedule's rung for this turn, or `None` when the player should not
/// activate at all.
///
/// `affordable` is the largest X this player could actually pay right now. It
/// is an INPUT rather than something this function computes, because the two
/// callers already hold it from different authorities: the activation decision
/// prices it with `max_x_value`, and the `{X}` prompt is handed it as the
/// prompt's own `max`.
///
/// Returns `None` — meaning "do not activate" — before the first scheduled turn
/// and whenever nothing can be paid. That second guard is load-bearing: without
/// it the AI activated on a turn whose lands were already tapped, was offered
/// `min=0 max=0`, and spent its once-per-turn activation and a card on a
/// mana-value-0 creature.
fn schedule_rung(state: &GameState, player: PlayerId, affordable: u32) -> Option<u32> {
    let own_turn = own_turn_index(state, player);
    if own_turn < first_activation_turn(state, player) || affordable == 0 {
        return None;
    }
    Some(own_turn.min(MAX_SCHEDULED_X).min(affordable))
}

/// The X to activate for right now, or `None` when the player should not
/// activate at all.
///
/// The schedule's rung is the floor. With pool data, every payable X from the
/// rung up is scored (see the module docs) and the best one wins; without it
/// (no card database installed) the rung stands, since there is nothing to
/// weigh a larger X against.
fn scheduled_x(
    state: &GameState,
    player: PlayerId,
    budget: &XBudget,
    pool: Option<&SinkPool>,
) -> Option<u32> {
    let rung = schedule_rung(state, player, budget.total)?;
    // The Emrakul hunt outranks the scoring, but only once it is genuinely
    // payable.
    if budget.total >= EMRAKUL_MANA_VALUE && !controls_emrakul(state, player) {
        return Some(EMRAKUL_MANA_VALUE);
    }
    Some(pool.map_or(rung, |pool| best_x(rung, budget, pool)))
}

/// The best-scoring payable X at or above the rung.
///
/// The rung is lowered to what free mana pays when free mana cannot reach it,
/// so reaching the rung by tapping a creature is weighed like any other
/// creature tap. Ties go to the lower X: creature mana is spent only for a
/// strict gain, and a pool no better than the rung's keeps the schedule.
fn best_x(rung: u32, budget: &XBudget, pool: &SinkPool) -> u32 {
    let lowest = rung.min(budget.free).max(1);
    (lowest..=budget.total)
        .filter_map(|x| Some((x, pool.expected_body(x)? - budget.creature_cost(x)?)))
        .fold(None::<(u32, f64)>, |best, (x, score)| match best {
            Some((_, best_score)) if best_score >= score => best,
            _ => Some((x, score)),
        })
        .map_or(rung, |(x, _)| x)
}

/// What a random-pool sink draws from: the effect's pool, and how its mana
/// value is compared against X.
struct SinkPool {
    profile: Arc<PoolProfile>,
    mv: Comparator,
}

impl SinkPool {
    fn for_sink(state: &GameState, sink: &PoolSink) -> Option<Self> {
        Some(Self {
            profile: pool_profile(state, &sink.type_filter)?,
            mv: sink.mv,
        })
    }

    /// Mean combat body of a draw at X, or `None` when nothing is drawable.
    fn expected_body(&self, x: u32) -> Option<f64> {
        self.profile.expected_body(self.mv, x)
    }
}

/// How much X the player can pay, and what paying past their free mana costs.
#[derive(Debug, Default)]
struct XBudget {
    /// Largest payable X using every mana source.
    total: u32,
    /// Largest payable X without activating any creature's mana ability.
    free: u32,
    /// Creatures whose mana raises X past `free`, cheapest board value per
    /// point of X first.
    creature_sources: Vec<CreatureManaSource>,
}

/// One creature's contribution to the X budget.
#[derive(Debug, Clone, Copy)]
struct CreatureManaSource {
    /// How much X it adds on top of the free mana.
    extra_x: u32,
    /// Board value given up by paying with it.
    cost: f64,
}

impl XBudget {
    /// Price the X budget for `cost` on `source_id`, given the `total` the
    /// caller already holds from its own affordability authority.
    ///
    /// Only runs board sweeps when creatures are present, and the per-creature
    /// sweeps only when free mana falls short of `total` — on an ordinary turn
    /// with no mana creature this is one creature scan and nothing more.
    fn price(
        state: &GameState,
        player: PlayerId,
        cost: &ManaCost,
        source_id: ObjectId,
        total: u32,
        tap_weight: f64,
    ) -> Self {
        let creatures: HashSet<ObjectId> = state
            .battlefield
            .iter()
            .copied()
            .filter(|id| {
                state.objects.get(id).is_some_and(|object| {
                    object.controller == player
                        && object.card_types.core_types.contains(&CoreType::Creature)
                })
            })
            .collect();
        if creatures.is_empty() {
            return Self::all_free(total);
        }
        let x_without = |excluded: &HashSet<ObjectId>| {
            engine::game::max_x_value_excluding(state, player, cost, Some(source_id), excluded)
                .min(total)
        };
        let free = x_without(&creatures);
        if free >= total {
            return Self::all_free(total);
        }
        let mut creature_sources: Vec<CreatureManaSource> = creatures
            .iter()
            .filter_map(|&creature| {
                let mut others = creatures.clone();
                others.remove(&creature);
                let extra_x = x_without(&others).saturating_sub(free);
                if extra_x == 0 {
                    return None;
                }
                // A creature that yields mana by tapping only sits out a turn
                // of combat; one that yields it any other way (sacrificing
                // itself, say) is given up outright.
                let given_up =
                    if engine::game::mana_sources::max_mana_yield(state, creature, player) > 0 {
                        tap_weight
                    } else {
                        1.0
                    };
                Some(CreatureManaSource {
                    extra_x,
                    cost: crate::eval::evaluate_creature_intrinsic(state, creature) * given_up,
                })
            })
            .collect();
        creature_sources.sort_by(|a, b| {
            (a.cost / f64::from(a.extra_x)).total_cmp(&(b.cost / f64::from(b.extra_x)))
        });
        Self {
            total,
            free,
            creature_sources,
        }
    }

    fn all_free(total: u32) -> Self {
        Self {
            total,
            free: total,
            creature_sources: Vec::new(),
        }
    }

    /// Board value given up to pay X, tapping the cheapest creatures first, or
    /// `None` when X is out of reach.
    ///
    /// Cheapest-first is the lower bound on the cost; the engine's auto-tapper
    /// already reaches for creatures only after every land and rock, so the
    /// free portion is always paid free.
    fn creature_cost(&self, x: u32) -> Option<f64> {
        if x > self.total {
            return None;
        }
        let mut needed = x.saturating_sub(self.free);
        let mut cost = 0.0;
        for source in &self.creature_sources {
            if needed == 0 {
                break;
            }
            cost += source.cost;
            needed = needed.saturating_sub(source.extra_x);
        }
        (needed == 0).then_some(cost)
    }
}

/// The largest X this player could pay for the ability at `ability_index` on
/// `source_id`, with the `{X}` mana leg it was priced against, or `None` when
/// the ability has no `{X}` mana leg.
///
/// `max_x_value` is a board-wide affordability sweep, so per the inner-loop
/// ordering rule it runs only after the candidate is confirmed to be the sink —
/// one object in the whole game, at most once per activation candidate.
fn affordable_x(
    state: &GameState,
    player: PlayerId,
    source_id: &ObjectId,
    ability_index: usize,
) -> Option<(u32, ManaCost)> {
    state
        .objects
        .get(source_id)
        .and_then(|object| object.abilities.get(ability_index))
        .and_then(|ability| ability.cost.as_ref())
        .and_then(engine::game::extract_x_mana_cost)
        .map(|(mana_cost, _residual)| {
            let affordable = engine::game::max_x_value(state, player, &mana_cost, Some(*source_id));
            (affordable, mana_cost)
        })
}

/// A random-pool creature sink's draw parameters.
struct PoolSink {
    type_filter: TargetFilter,
    mv: Comparator,
}

/// The first random-pool sink among `effects`, if any.
fn pool_sink_in<'a>(effects: impl IntoIterator<Item = &'a Effect>) -> Option<PoolSink> {
    effects.into_iter().find_map(|effect| match effect {
        Effect::CreateTokenCopyFromPool {
            type_filter, mv, ..
        } => Some(PoolSink {
            type_filter: type_filter.clone(),
            mv: *mv,
        }),
        _ => None,
    })
}

/// The random-pool sink the ability at `ability_index` on `source_id` is, if it
/// is one. Card-local: reads one object's ability chain, no board sweep.
fn pool_sink_activation(
    state: &GameState,
    source_id: &ObjectId,
    ability_index: usize,
) -> Option<PoolSink> {
    state
        .objects
        .get(source_id)
        .and_then(|object| object.abilities.get(ability_index))
        .and_then(|ability| pool_sink_in(crate::ability_chain::collect_chain_effects(ability)))
}

/// The random-pool sink whose X prompt the AI is answering, if it is one.
fn pending_x_pool_sink(state: &GameState) -> Option<PoolSink> {
    let WaitingFor::ChooseXValue { pending_cast, .. } = &state.waiting_for else {
        return None;
    };
    // `collect_ability_effects` walks the whole `sub_ability` chain, not just
    // the head effect.
    pool_sink_in(super::context::collect_ability_effects(
        &pending_cast.ability,
    ))
}

fn na() -> PolicyVerdict {
    PolicyVerdict::neutral(PolicyReason::new("momir_curve_na"))
}

impl TacticalPolicy for MomirCurvePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::MomirCurve
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility, DecisionKind::ChooseX]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Cheapest possible opt-out: a plain bool field read. The random-pool
        // sink is a command-zone emblem, so a format with no command zone can
        // never present one and the registry skips `verdict` entirely. The
        // precise, card-local check lives in `verdict` per the inner-loop
        // ordering rule (cheap structural gate first, never a board sweep).
        state.format_config.command_zone.then_some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let penalties = &ctx.config.policy_penalties;
        match &ctx.candidate.action {
            // CR 117.1b: activating the sink is a normal priority action; the
            // engine already enforces "only as a sorcery, only once each turn".
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => {
                let Some(sink) = pool_sink_activation(ctx.state, source_id, *ability_index) else {
                    return na();
                };
                let (affordable, x_cost) =
                    affordable_x(ctx.state, ctx.ai_player, source_id, *ability_index)
                        .unwrap_or((0, ManaCost::zero()));
                let budget = XBudget::price(
                    ctx.state,
                    ctx.ai_player,
                    &x_cost,
                    *source_id,
                    affordable,
                    penalties.momir_curve_mana_creature_tap_weight,
                );
                let pool = SinkPool::for_sink(ctx.state, &sink);
                match scheduled_x(ctx.state, ctx.ai_player, &budget, pool.as_ref()) {
                    Some(target) => PolicyVerdict::strong(
                        penalties.momir_curve_activation,
                        PolicyReason::new("momir_curve_activate")
                            .with_fact("scheduled_x", i64::from(target))
                            .with_fact("affordable_x", i64::from(affordable))
                            .with_fact("free_x", i64::from(budget.free))
                            .with_fact(
                                "own_turn",
                                i64::from(own_turn_index(ctx.state, ctx.ai_player)),
                            ),
                    ),
                    // Either the schedule has not opened yet — the discard
                    // (CR 701.9a) costs more than the small creature it would
                    // buy — or nothing is payable, which would burn the
                    // once-each-turn activation on a mana-value-0 creature.
                    None => PolicyVerdict::reject(
                        PolicyReason::new("momir_curve_not_scheduled")
                            .with_fact("affordable_x", i64::from(affordable))
                            .with_fact(
                                "own_turn",
                                i64::from(own_turn_index(ctx.state, ctx.ai_player)),
                            ),
                    ),
                }
            }
            // CR 202.3: X is the created creature's mana value, so this choice
            // IS the schedule.
            GameAction::ChooseX { value } => {
                let Some(sink) = pending_x_pool_sink(ctx.state) else {
                    return na();
                };
                let WaitingFor::ChooseXValue {
                    min,
                    max,
                    pending_cast,
                    ..
                } = &ctx.state.waiting_for
                else {
                    return na();
                };
                // The prompt's own `max` IS the total-affordability authority
                // here — the engine already priced it. Only the split between
                // free and creature mana is priced again.
                let budget = XBudget::price(
                    ctx.state,
                    ctx.ai_player,
                    &pending_cast.cost,
                    pending_cast.object_id,
                    *max,
                    penalties.momir_curve_mana_creature_tap_weight,
                );
                let pool = SinkPool::for_sink(ctx.state, &sink);
                let Some(target) = scheduled_x(ctx.state, ctx.ai_player, &budget, pool.as_ref())
                else {
                    return na();
                };
                let target = target.clamp(*min, *max);
                if *value == target {
                    return PolicyVerdict::strong(
                        penalties.momir_curve_x_on_schedule,
                        PolicyReason::new("momir_curve_x_on_schedule")
                            .with_fact("chosen_x", i64::from(*value)),
                    );
                }
                // Every other X is a veto, in BOTH directions. A schedule
                // expressed as a preference is not a schedule: the search's own
                // value function reads "bigger creature" as strictly better and
                // outbids any in-band score by more than the preference band is
                // wide. Measured on a full AI-vs-AI game, a graduated penalty
                // produced `target + 1` at every rung when mana was plentiful
                // and `target - 1` where the search preferred to hold it.
                // `target` is clamped into the prompt's own `min..=max`, so it
                // is always a legal answer and this can never veto every
                // candidate.
                PolicyVerdict::reject(
                    PolicyReason::new("momir_curve_x_off_schedule")
                        .with_fact("chosen_x", i64::from(*value))
                        .with_fact("scheduled_x", i64::from(target)),
                )
            }
            _ => na(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::game::deck_loading::momir_emblem_ability;
    use engine::types::format::FormatConfig;
    use engine::types::identifiers::ObjectId;
    use engine::types::player::PlayerId;

    const P0: PlayerId = PlayerId(0);
    const P1: PlayerId = PlayerId(1);

    /// A Momir state where `starter` took the first turn and the shared
    /// player-turn counter reads `turn_number`.
    fn momir_state(starter: PlayerId, turn_number: u32) -> GameState {
        let mut state = GameState::new(FormatConfig::momir(), 2, 42);
        state.current_starting_player = starter;
        state.turn_number = turn_number;
        state
    }

    /// The schedule P0 follows across their own turns 1..=11, as X values with
    /// `None` for "pass". `starter` decides whether P0 is on the play.
    ///
    /// `affordable` is set to the player's own turn index, which is what a real
    /// Momir board yields: the deck is 60 lands, so a player has exactly one
    /// land per turn taken and X is bounded by that.
    fn observed_schedule(starter: PlayerId) -> Vec<Option<u32>> {
        (1..=11)
            .map(|own_turn| {
                // `turn_number` is the shared per-player-turn counter: the
                // player on the play owns the odd turns, the other the even.
                let turn_number = if starter == P0 {
                    own_turn * 2 - 1
                } else {
                    own_turn * 2
                };
                let state = momir_state(starter, turn_number);
                assert_eq!(
                    own_turn_index(&state, P0),
                    own_turn,
                    "own-turn derivation must invert the turn_number mapping"
                );
                scheduled_x(&state, P0, &XBudget::all_free(own_turn), None)
            })
            .collect()
    }

    /// The requested default line, on the play: pass twice, then 3, 4, 5, 6, 7,
    /// 8, and 8 from there on.
    #[test]
    fn on_the_play_schedule_matches_the_specified_curve() {
        assert_eq!(
            observed_schedule(P0),
            vec![
                None,
                None,
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                Some(8),
                Some(8),
                Some(8),
            ]
        );
    }

    /// The requested default line, on the draw: pass once, then 2, 3, 4, 5, 6,
    /// 7, 8, and 8 from there on. CR 103.8a: the player on the draw did not skip
    /// a draw step, so they are a card up and start one turn earlier.
    #[test]
    fn on_the_draw_schedule_matches_the_specified_curve() {
        assert_eq!(
            observed_schedule(P1),
            vec![
                None,
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                Some(7),
                Some(8),
                Some(8),
                Some(8),
                Some(8),
            ]
        );
    }

    #[test]
    fn turn_order_follows_the_starting_player() {
        assert_eq!(turn_order(&momir_state(P0, 1), P0), TurnOrder::OnPlay);
        assert_eq!(turn_order(&momir_state(P1, 1), P0), TurnOrder::OnDraw);
    }

    /// The rung — the floor the pool scoring starts from, and the whole answer
    /// when there is no pool data — never climbs past the cap, however far the
    /// game runs and however much mana is payable.
    #[test]
    fn schedule_rung_never_exceeds_the_cap() {
        for own_turn in 1..=40u32 {
            let state = momir_state(P0, own_turn * 2 - 1);
            // Affordability well above the cap but below the Emrakul threshold,
            // so this asserts the ordinary cap, not the 15-mana hunt.
            let affordable = EMRAKUL_MANA_VALUE - 1;
            assert_eq!(
                scheduled_x(&state, P0, &XBudget::all_free(affordable), None),
                schedule_rung(&state, P0, affordable),
                "without pool data the rung is the answer"
            );
            if let Some(x) = schedule_rung(&state, P0, affordable) {
                assert!(
                    x <= MAX_SCHEDULED_X,
                    "own turn {own_turn} scheduled X={x} above cap {MAX_SCHEDULED_X}"
                );
            }
        }
    }

    #[test]
    fn activation_opts_out_without_a_command_zone() {
        let mut state = momir_state(P0, 5);
        state.format_config = FormatConfig::standard();
        assert!(MomirCurvePolicy
            .activation(&DeckFeatures::default(), &state, P0)
            .is_none());
    }

    #[test]
    fn activation_opts_in_with_a_command_zone() {
        let state = momir_state(P0, 5);
        assert!(state.format_config.command_zone, "precondition");
        assert!(MomirCurvePolicy
            .activation(&DeckFeatures::default(), &state, P0)
            .is_some());
    }

    /// Detection is structural: it binds to the effect, never to an emblem name
    /// or the format.
    #[test]
    fn pool_sink_detection_reads_the_effect_not_the_name() {
        let mut state = momir_state(P0, 5);
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![momir_emblem_ability()],
        );
        assert!(pool_sink_activation(&state, &emblem, 0).is_some());
        // A different ability index on the same object is not the sink.
        assert!(pool_sink_activation(&state, &emblem, 1).is_none());
        // An object with no abilities at all is not the sink.
        assert!(pool_sink_activation(&state, &ObjectId(9999), 0).is_none());
    }

    /// PRODUCTION PATH. The schedule only matters if it survives the real
    /// `PolicyRegistry`, where every other policy also scores the candidate —
    /// asserting `scheduled_x` alone would pass even if the registry then
    /// picked a different X, which is exactly what happened before the
    /// off-schedule veto (a graduated penalty was outbid by the search's own
    /// "bigger creature is better" value).
    #[test]
    fn registry_priors_elevate_the_scheduled_x_over_every_other_value() {
        use crate::context::AiContext;
        use crate::policies::context::{PriorsEnv, SearchDepth};
        use crate::policies::registry::PolicyRegistry;
        use engine::ai_support::AiDecisionContext;
        use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
        use engine::types::game_state::PendingCast;
        use engine::types::identifiers::CardId;
        use engine::types::mana::{ManaCost, ManaCostShard};

        // Own turn 6 on the play (turn_number 11), so the schedule wants X=6.
        let mut state = momir_state(P0, 11);
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![momir_emblem_ability()],
        );
        assert_eq!(own_turn_index(&state, P0), 6);

        let max = 9;
        let pending = PendingCast::new(
            emblem,
            CardId(0),
            engine::types::ability::ResolvedAbility::new(
                *momir_emblem_ability().effect.clone(),
                Vec::new(),
                emblem,
                P0,
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            },
        );
        state.waiting_for = WaitingFor::ChooseXValue {
            player: P0,
            min: 0,
            max,
            pending_cast: Box::new(pending.clone()),
            convoke_mode: None,
            x_cost_previews: vec![],
        };

        let config = crate::config::AiConfig::default();
        let ai_context = AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidates: Vec<CandidateAction> = (0..=max)
            .map(|value| CandidateAction {
                action: GameAction::ChooseX { value },
                metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Selection),
            })
            .collect();

        let env = PriorsEnv {
            state: &state,
            decision: &decision,
            ai_player: P0,
            config: &config,
            context: &ai_context,
            search_depth: SearchDepth::Lookahead,
        };
        let priors = PolicyRegistry::shared().priors(&env, &candidates);

        let best = priors
            .iter()
            .max_by(|a, b| a.prior.partial_cmp(&b.prior).expect("finite priors"))
            .expect("priors for every candidate");
        assert_eq!(
            best.candidate.action,
            GameAction::ChooseX { value: 6 },
            "the registry must top-rank the scheduled X, not the largest payable one"
        );
    }

    /// Nothing payable means the once-each-turn activation would buy a
    /// mana-value-0 creature for a card. Measured in a full AI-vs-AI game
    /// before this guard: the AI activated on a turn whose lands were already
    /// tapped, was offered `min=0 max=0`, and spent the turn on it.
    #[test]
    fn nothing_payable_means_do_not_activate() {
        let state = momir_state(P0, 11);
        assert_eq!(scheduled_x(&state, P0, &XBudget::all_free(0), None), None);
        // One mana is enough to be worth it.
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(1), None),
            Some(1)
        );
    }

    /// The schedule never asks for more than the player can actually pay.
    #[test]
    fn schedule_is_capped_by_affordability() {
        // Own turn 6 wants 6, but only 2 mana is available.
        let state = momir_state(P0, 11);
        assert_eq!(own_turn_index(&state, P0), 6);
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(2), None),
            Some(2)
        );
    }

    /// Once 15 is payable the schedule abandons the cap and rolls for Emrakul.
    #[test]
    fn fifteen_payable_hunts_emrakul_instead_of_capping_at_eight() {
        let state = momir_state(P0, 11);
        assert_eq!(own_turn_index(&state, P0), 6, "cap would otherwise say 6");
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(EMRAKUL_MANA_VALUE), None),
            Some(EMRAKUL_MANA_VALUE)
        );
        // Fourteen is not enough — the hunt only opens at a payable 15.
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(14), None),
            Some(6)
        );
    }

    /// The hunt ends when it succeeds: with an Emrakul already on the
    /// battlefield the schedule returns to its ordinary cap.
    #[test]
    fn controlling_emrakul_ends_the_hunt_and_restores_the_cap() {
        let mut state = momir_state(P0, 21);
        assert_eq!(own_turn_index(&state, P0), 11);
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(EMRAKUL_MANA_VALUE), None),
            Some(EMRAKUL_MANA_VALUE),
            "precondition: the hunt is open"
        );

        let emrakul = engine::game::zones::create_object(
            &mut state,
            engine::types::identifiers::CardId(4242),
            P0,
            EMRAKUL_NAME.to_string(),
            engine::types::zones::Zone::Battlefield,
        );
        state.objects.get_mut(&emrakul).unwrap().controller = P0;

        assert!(controls_emrakul(&state, P0));
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(EMRAKUL_MANA_VALUE), None),
            Some(MAX_SCHEDULED_X),
            "with Emrakul found, the cap applies again"
        );
    }

    /// An opponent's Emrakul does not end this player's hunt.
    #[test]
    fn an_opponents_emrakul_does_not_end_the_hunt() {
        let mut state = momir_state(P0, 21);
        let emrakul = engine::game::zones::create_object(
            &mut state,
            engine::types::identifiers::CardId(4243),
            P1,
            EMRAKUL_NAME.to_string(),
            engine::types::zones::Zone::Battlefield,
        );
        state.objects.get_mut(&emrakul).unwrap().controller = P1;

        assert!(!controls_emrakul(&state, P0));
        assert_eq!(
            scheduled_x(&state, P0, &XBudget::all_free(EMRAKUL_MANA_VALUE), None),
            Some(EMRAKUL_MANA_VALUE)
        );
    }

    /// A non-Momir emblem in the same command zone must not be mistaken for the
    /// sink — otherwise this policy would schedule unrelated activations.
    #[test]
    fn unrelated_command_zone_ability_is_not_a_pool_sink() {
        use engine::types::ability::{AbilityDefinition, AbilityKind, Effect, QuantityExpr};

        let mut state = momir_state(P0, 5);
        let draw = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
            },
        );
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![draw],
        );
        assert!(pool_sink_activation(&state, &emblem, 0).is_none());
    }

    // ── Moving ahead of the schedule ────────────────────────────────────

    use crate::policies::momir_pool::tests::{creature_face, install_db};
    use engine::types::ability::{AbilityCost, AbilityDefinition, AbilityKind};
    use engine::types::ability::{ManaContribution, ManaProduction};
    use engine::types::identifiers::CardId;
    use engine::types::mana::ManaColor;
    use engine::types::zones::Zone;

    /// Install a pool whose creatures at mana value `mv` are all `mv/mv`, for
    /// every listed mana value — so the expected body is `2.5 * mv` there and
    /// nothing is drawable anywhere else.
    fn install_curve_pool(state: &mut GameState, mana_values: impl IntoIterator<Item = u32>) {
        let faces: Vec<_> = mana_values
            .into_iter()
            .map(|mv| creature_face(&format!("Beast {mv}"), mv, Some(mv as i32), mv as i32))
            .collect();
        install_db(state, &faces);
    }

    fn momir_pool(state: &GameState) -> SinkPool {
        SinkPool::for_sink(
            state,
            &PoolSink {
                type_filter: TargetFilter::Any,
                mv: Comparator::EQ,
            },
        )
        .expect("database installed")
    }

    /// A budget of `free` mana plus creatures adding one X each at `costs`.
    fn budget(free: u32, costs: &[f64]) -> XBudget {
        XBudget {
            total: free + costs.len() as u32,
            free,
            creature_sources: costs
                .iter()
                .map(|&cost| CreatureManaSource { extra_x: 1, cost })
                .collect(),
        }
    }

    /// A 1/1 dork's combat body, as `evaluate_creature_intrinsic` prices it.
    const ONE_ONE_BODY: f64 = 2.5;
    /// A 6/6's combat body.
    const SIX_SIX_BODY: f64 = 15.0;

    /// The requested line: on the draw, a two-drop mana creature that can tap
    /// on the third own turn turns the scheduled 3 into a 4.
    #[test]
    fn a_small_mana_creature_moves_the_curve_ahead_a_rung() {
        let mut state = momir_state(P1, 6);
        assert_eq!(own_turn_index(&state, P0), 3);
        install_curve_pool(&mut state, 1..=15);
        let weight = crate::config::PolicyPenalties::default().momir_curve_mana_creature_tap_weight;
        let x = scheduled_x(
            &state,
            P0,
            &budget(3, &[ONE_ONE_BODY * weight]),
            Some(&momir_pool(&state)),
        );
        assert_eq!(x, Some(4));
    }

    /// A mana creature big enough that a turn of its combat outweighs one rung
    /// stays untapped: the schedule's own rung stands.
    #[test]
    fn a_valuable_mana_creature_is_not_tapped_for_a_small_gain() {
        let mut state = momir_state(P0, 13);
        assert_eq!(own_turn_index(&state, P0), 7);
        install_curve_pool(&mut state, 1..=15);
        let weight = crate::config::PolicyPenalties::default().momir_curve_mana_creature_tap_weight;
        let x = scheduled_x(
            &state,
            P0,
            &budget(7, &[SIX_SIX_BODY * weight]),
            Some(&momir_pool(&state)),
        );
        assert_eq!(x, Some(7));
    }

    /// Past the cap, free mana buys a larger X when the pool there is better.
    #[test]
    fn past_the_cap_free_mana_buys_a_better_pool() {
        let mut state = momir_state(P0, 19);
        assert_eq!(own_turn_index(&state, P0), 10);
        install_curve_pool(&mut state, 1..=13);
        assert_eq!(
            scheduled_x(&state, P0, &budget(10, &[]), Some(&momir_pool(&state))),
            Some(10)
        );
    }

    /// With nothing better above the cap, the AI keeps rolling 8s.
    #[test]
    fn past_the_cap_an_empty_pool_keeps_the_eights_coming() {
        let mut state = momir_state(P0, 19);
        install_curve_pool(&mut state, 1..=8);
        assert_eq!(
            scheduled_x(&state, P0, &budget(10, &[]), Some(&momir_pool(&state))),
            Some(MAX_SCHEDULED_X)
        );
    }

    /// CR 609.3: an X with nothing drawable makes no token, so it is never the
    /// pick, even when it is the largest payable.
    #[test]
    fn an_empty_mana_value_is_never_picked() {
        let mut state = momir_state(P0, 27);
        assert_eq!(own_turn_index(&state, P0), 14);
        install_curve_pool(&mut state, (1..=13).chain(15..=15));
        assert_eq!(
            scheduled_x(&state, P0, &budget(14, &[]), Some(&momir_pool(&state))),
            Some(13)
        );
    }

    /// A pool no better than the cap's keeps the schedule: ties go low.
    #[test]
    fn a_pool_no_better_than_the_cap_keeps_the_schedule() {
        let mut state = momir_state(P0, 19);
        install_db(
            &mut state,
            &[
                creature_face("Eight", 8, Some(8), 8),
                creature_face("Nine", 9, Some(8), 8),
            ],
        );
        assert_eq!(
            scheduled_x(&state, P0, &budget(9, &[]), Some(&momir_pool(&state))),
            Some(MAX_SCHEDULED_X)
        );
    }

    /// Creature mana is charged cheapest-first, and an X past every source is
    /// out of reach.
    #[test]
    fn creature_cost_taps_the_cheapest_creatures_first() {
        let mut budget = budget(3, &[5.0, 1.0]);
        budget
            .creature_sources
            .sort_by(|a, b| a.cost.total_cmp(&b.cost));
        assert_eq!(budget.creature_cost(3), Some(0.0));
        assert_eq!(budget.creature_cost(4), Some(1.0));
        assert_eq!(budget.creature_cost(5), Some(6.0));
        assert_eq!(budget.creature_cost(6), None);
    }

    fn add_forest(state: &mut GameState, player: PlayerId) -> ObjectId {
        let id = engine::game::zones::create_object(
            state,
            CardId(state.next_object_id),
            player,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.controller = player;
        obj.card_types.core_types.push(CoreType::Land);
        obj.card_types.subtypes.push("Forest".to_string());
        // CR 305.6: a Forest's intrinsic `{T}: Add {G}`. Normally granted by the
        // layer pass; written directly so the test needs no layer evaluation.
        Arc::make_mut(&mut obj.abilities).push(tap_for_green());
        id
    }

    /// A `power/toughness` creature with `{T}: Add {G}` that has been under its
    /// controller's control since their last turn began.
    fn add_mana_creature(
        state: &mut GameState,
        player: PlayerId,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = engine::game::zones::create_object(
            state,
            CardId(state.next_object_id),
            player,
            "Mana Beast".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.controller = player;
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        obj.summoning_sick = false;
        obj.entered_battlefield_turn = Some(0);
        Arc::make_mut(&mut obj.abilities).push(tap_for_green());
        id
    }

    /// `{T}: Add {G}`.
    fn tap_for_green() -> AbilityDefinition {
        let mut mana = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: vec![ManaColor::Green],
                    contribution: ManaContribution::Base,
                },
                restrictions: vec![],
                grants: vec![],
                expiry: None,
                target: None,
            },
        );
        mana.cost = Some(AbilityCost::Tap);
        mana
    }

    /// On the draw's third own turn: three Forests and a mana creature of the
    /// given size, the Momir emblem, and a full curve pool.
    fn draw_turn_three_board(dork_power: i32, dork_toughness: i32) -> (GameState, ObjectId) {
        let mut state = momir_state(P1, 6);
        install_curve_pool(&mut state, 1..=15);
        for _ in 0..3 {
            add_forest(&mut state, P0);
        }
        add_mana_creature(&mut state, P0, dork_power, dork_toughness);
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![momir_emblem_ability()],
        );
        (state, emblem)
    }

    /// The budget is priced from the real board: lands are free, the creature's
    /// mana is the part that costs.
    #[test]
    fn budget_splits_land_mana_from_creature_mana() {
        let (state, emblem) = draw_turn_three_board(1, 1);
        let (total, cost) = affordable_x(&state, P0, &emblem, 0).expect("{X} leg");
        assert_eq!(total, 4, "three Forests and a mana creature");
        let budget = XBudget::price(&state, P0, &cost, emblem, total, 0.25);
        assert_eq!(budget.free, 3);
        assert_eq!(budget.creature_sources.len(), 1);
        assert_eq!(budget.creature_sources[0].extra_x, 1);
        assert_eq!(budget.creature_sources[0].cost, ONE_ONE_BODY * 0.25);
    }

    /// A board with no creatures prices every point of X as free.
    #[test]
    fn budget_without_creatures_is_all_free() {
        let mut state = momir_state(P1, 6);
        for _ in 0..3 {
            add_forest(&mut state, P0);
        }
        let emblem = engine::game::effects::create_emblem::grant_emblem(
            &mut state,
            P0,
            Vec::new(),
            Vec::new(),
            vec![momir_emblem_ability()],
        );
        let (total, cost) = affordable_x(&state, P0, &emblem, 0).expect("{X} leg");
        let budget = XBudget::price(&state, P0, &cost, emblem, total, 0.25);
        assert_eq!((budget.total, budget.free), (3, 3));
        assert!(budget.creature_sources.is_empty());
    }

    /// Top-ranked `{X}` answer for P0 at a sink prompt on `state`, through the
    /// real `PolicyRegistry`.
    fn registry_top_x(state: &mut GameState, emblem: ObjectId, max: u32) -> u32 {
        use crate::context::AiContext;
        use crate::policies::context::{PriorsEnv, SearchDepth};
        use crate::policies::registry::PolicyRegistry;
        use engine::ai_support::AiDecisionContext;
        use engine::ai_support::{ActionMetadata, CandidateAction, TacticalClass};
        use engine::types::game_state::PendingCast;
        use engine::types::mana::ManaCostShard;

        let pending = PendingCast::new(
            emblem,
            CardId(0),
            engine::types::ability::ResolvedAbility::new(
                *momir_emblem_ability().effect.clone(),
                Vec::new(),
                emblem,
                P0,
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            },
        );
        state.waiting_for = WaitingFor::ChooseXValue {
            player: P0,
            min: 0,
            max,
            pending_cast: Box::new(pending),
            convoke_mode: None,
            x_cost_previews: vec![],
        };
        let config = crate::config::AiConfig::default();
        let ai_context = AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidates: Vec<CandidateAction> = (0..=max)
            .map(|value| CandidateAction {
                action: GameAction::ChooseX { value },
                metadata: ActionMetadata::for_actor(Some(P0), TacticalClass::Selection),
            })
            .collect();
        let env = PriorsEnv {
            state,
            decision: &decision,
            ai_player: P0,
            config: &config,
            context: &ai_context,
            search_depth: SearchDepth::Lookahead,
        };
        let priors = PolicyRegistry::shared().priors(&env, &candidates);
        let best = priors
            .iter()
            .max_by(|a, b| a.prior.partial_cmp(&b.prior).expect("finite priors"))
            .expect("priors for every candidate");
        match best.candidate.action {
            GameAction::ChooseX { value } => value,
            ref other => panic!("expected ChooseX, got {other:?}"),
        }
    }

    /// PRODUCTION PATH: a 1/1 mana creature on the draw's third turn makes the
    /// registry top-rank X=4 over the scheduled 3.
    #[test]
    fn registry_moves_ahead_with_a_small_mana_creature() {
        let (mut state, emblem) = draw_turn_three_board(1, 1);
        assert_eq!(registry_top_x(&mut state, emblem, 4), 4);
    }

    /// PRODUCTION PATH: a 6/6 mana creature is worth more attacking than the
    /// rung it would buy, so the registry keeps the scheduled 3.
    #[test]
    fn registry_keeps_a_big_mana_creature_untapped() {
        let (mut state, emblem) = draw_turn_three_board(6, 6);
        assert_eq!(registry_top_x(&mut state, emblem, 4), 3);
    }
}

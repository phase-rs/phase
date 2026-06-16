//! Monte Carlo hand-quality simulation for the playtest feature.
//!
//! `run_simulation` drives N independent `PlaytestSession` games for G turns
//! each and aggregates per-turn statistics. The result powers the
//! `SimulationPanel` in the frontend, showing average mana available, hand
//! size, lands in play, and playable-cards-count per turn over a sample of
//! games.
//!
//! All randomness is seeded from a base seed plus the game index so results
//! are deterministic for a given deck + seed pair — identical to replaying
//! the same simulation with the same parameters. This allows the UI to show
//! "run N games" with a stable result.

use serde::{Deserialize, Serialize};

use crate::types::card::CardFace;

use super::solitaire::PlaytestSession;

// ── Configuration ─────────────────────────────────────────────────────────────

/// Parameters for a Monte Carlo playtest simulation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationConfig {
    /// Number of independent games to simulate.
    pub num_games: u32,
    /// Number of turns to simulate per game (after the opening hand).
    pub num_turns: u32,
    /// Base RNG seed. Each game i uses `base_seed ^ i` so games are
    /// independent but the suite as a whole is reproducible.
    pub base_seed: u64,
    /// When true, simulate going-first (no draw on turn 1). When false,
    /// simulate going-second (one extra card drawn before turn 1 main).
    pub going_first: bool,
    /// When true, simulate an auto-keep (no mulligans). When false, apply
    /// a simple heuristic: mulligan hands with 0 or 6–7 lands.
    pub auto_keep: bool,
}

impl Default for SimulationConfig {
    fn default() -> Self {
        Self {
            num_games: 200,
            num_turns: 10,
            base_seed: 0xdead_beef,
            going_first: true,
            auto_keep: false,
        }
    }
}

// ── Per-turn aggregate ────────────────────────────────────────────────────────

/// Aggregated statistics for a single turn number across all simulated games.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnAggregate {
    /// Turn number (1-indexed).
    pub turn_number: u32,
    /// Average hand size after drawing.
    pub avg_hand_size: f64,
    /// Average lands in play.
    pub avg_lands_in_play: f64,
    /// Average total mana sources in play.
    pub avg_mana_sources: f64,
    /// Average available (untapped) mana.
    pub avg_available_mana: f64,
    /// Average number of playable non-land cards in hand.
    pub avg_playable_count: f64,
    /// Fraction of games that drew a land this turn (i.e., at least one land was
    /// in hand at the snapshot moment, not just in play — separate tracking).
    pub pct_land_in_hand: f64,
    /// Fraction of games where the library was empty at snapshot time (milled out).
    pub pct_empty_library: f64,
    /// Min available mana across all games this turn.
    pub min_available_mana: f64,
    /// Max available mana across all games this turn.
    pub max_available_mana: f64,
    /// Standard deviation of available mana.
    pub stddev_available_mana: f64,
}

// ── Opening hand stats ────────────────────────────────────────────────────────

/// Statistics about the opening hand distribution across all simulated games.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpeningHandStats {
    /// Average number of lands in the kept opening hand.
    pub avg_lands: f64,
    /// Average number of non-land cards in the kept opening hand.
    pub avg_spells: f64,
    /// Average hand size kept (after mulligans, before turn 1).
    pub avg_hand_size: f64,
    /// Distribution of kept opening hand sizes: index 0 = 7 cards, index 7 = 0 cards.
    pub hand_size_distribution: Vec<u32>,
    /// Average number of mulligans taken per game.
    pub avg_mulligans: f64,
    /// Fraction of games where 0 mulligans were taken.
    pub pct_keep_first: f64,
}

// ── Full simulation result ────────────────────────────────────────────────────

/// Result of running a `SimulationConfig` over a deck.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SimulationResult {
    /// Per-turn aggregates (length = `config.num_turns`).
    pub turns: Vec<TurnAggregate>,
    /// Opening hand statistics.
    pub opening_hand: OpeningHandStats,
    /// Actual number of games simulated (may be less than `config.num_games` if
    /// the deck was too small to deal opening hands).
    pub games_simulated: u32,
    /// Configuration used for this simulation run.
    pub config: SimulationConfig,
}

// ── Mulligan heuristic ────────────────────────────────────────────────────────

/// Simple land-count mulligan heuristic: keep if hand has 2–5 lands out of 7.
/// On subsequent mulligans the thresholds widen since the hand is smaller.
fn should_keep(session: &PlaytestSession, mulligan_count: u8) -> bool {
    let lands = session.lands_in_hand();
    let hand = session.hand.len();
    if hand == 0 {
        return true; // Always keep 0-card hands (CR 103.5)
    }
    // Scale acceptable land range with hand size.
    let (min_lands, max_lands) = match mulligan_count {
        0 => (2, 5),                      // 7-card hand: 2–5 lands
        1 => (2, 5),                      // 6-card hand (after bottoming): 2–4 lands
        2 => (1, 5),                      // 5-card hand: 1–4 lands
        _ => (1, hand.saturating_sub(1)), // Desperation keep
    };
    lands >= min_lands && lands <= max_lands
}

// ── Simulation driver ────────────────────────────────────────────────────────

/// Run a Monte Carlo simulation and return aggregated statistics.
///
/// Each game independently:
/// 1. Creates a fresh `PlaytestSession` with seed `base_seed ^ game_index`.
/// 2. Applies the mulligan heuristic (or auto-keeps when `config.auto_keep`).
/// 3. Simulates `config.num_turns` turns, playing one land per turn when
///    available (greedy land-drop — the optimizer's baseline).
/// 4. Accumulates per-turn statistics.
pub fn run_simulation(deck: &[CardFace], config: &SimulationConfig) -> SimulationResult {
    if deck.is_empty() || config.num_games == 0 || config.num_turns == 0 {
        return SimulationResult {
            turns: Vec::new(),
            opening_hand: OpeningHandStats::default(),
            games_simulated: 0,
            config: config.clone(),
        };
    }

    let deck = deck.to_vec();
    let num_turns = config.num_turns as usize;

    // Per-turn accumulators (indexed 0..num_turns).
    let mut acc_hand: Vec<f64> = vec![0.0; num_turns];
    let mut acc_lands_play: Vec<f64> = vec![0.0; num_turns];
    let mut acc_sources: Vec<f64> = vec![0.0; num_turns];
    let mut acc_mana: Vec<f64> = vec![0.0; num_turns];
    let mut acc_playable: Vec<f64> = vec![0.0; num_turns];
    let mut acc_land_in_hand: Vec<f64> = vec![0.0; num_turns];
    let mut acc_empty_lib: Vec<f64> = vec![0.0; num_turns];
    let mut acc_mana_sq: Vec<f64> = vec![0.0; num_turns]; // for stddev
    let mut acc_mana_min: Vec<f64> = vec![f64::MAX; num_turns];
    let mut acc_mana_max: Vec<f64> = vec![f64::MIN; num_turns];

    // Opening hand accumulators.
    let mut oh_lands: f64 = 0.0;
    let mut oh_hand_size: f64 = 0.0;
    let mut oh_mulligans: f64 = 0.0;
    let mut oh_keep_first: u32 = 0;
    let mut oh_hand_dist = vec![0u32; 8]; // index = 7 - hand_size

    let mut games_simulated: u32 = 0;

    for game_idx in 0..config.num_games {
        let seed = config.base_seed ^ (game_idx as u64);
        let mut session = PlaytestSession::new(deck.clone(), seed, config.going_first);

        // Mulligan phase.
        if !config.auto_keep {
            let mut mulls = 0u8;
            while session.in_mulligan && mulls < 7 {
                if should_keep(&session, mulls) {
                    break;
                }
                if session.take_mulligan().is_err() {
                    break;
                }
                mulls += 1;
                // Auto-bottom cards (put the worst cards on bottom).
                // Heuristic: bottom excess lands when land-heavy, else bottom excess spells.
                let to_bottom = session.bottoming_required as usize;
                let land_heavy = session.lands_in_hand() > 4;
                let ids_to_bottom: Vec<_> = if land_heavy {
                    session
                        .hand
                        .iter()
                        .filter(|c| PlaytestSession::is_land(&c.face))
                        .map(|c| c.id)
                        .take(to_bottom)
                        .collect()
                } else {
                    // Bottom highest-CMC spells.
                    let mut spell_slots: Vec<_> = session
                        .hand
                        .iter()
                        .filter(|c| !PlaytestSession::is_land(&c.face))
                        .collect();
                    spell_slots.sort_by(|a, b| {
                        b.face
                            .mana_value
                            .partial_cmp(&a.face.mana_value)
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    spell_slots.iter().map(|c| c.id).take(to_bottom).collect()
                };
                for id in ids_to_bottom {
                    let _ = session.bottom_card(id);
                }
            }
        }

        if session.keep_hand().is_err() {
            // Shouldn't happen for valid decks — skip this game.
            continue;
        }

        // Record opening hand stats.
        let kept_lands = session.lands_in_hand();
        let kept_size = session.hand.len();
        oh_lands += kept_lands as f64;
        oh_hand_size += kept_size as f64;
        oh_mulligans += session.mulligan_count as f64;
        if session.mulligan_count == 0 {
            oh_keep_first += 1;
        }
        let dist_idx = 7usize.saturating_sub(kept_size).min(7);
        oh_hand_dist[dist_idx] += 1;

        // Simulate turns: greedy land-drop each turn.
        for turn_idx in 0..num_turns {
            if turn_idx > 0 {
                // Advance to next turn (untap + draw).
                if session.needs_cleanup_discard() {
                    // Auto-discard highest-CMC card to reach hand size 7.
                    let to_discard = session.discard_count_needed();
                    let ids: Vec<_> = {
                        let mut slots: Vec<_> = session
                            .hand
                            .iter()
                            .filter(|c| !PlaytestSession::is_land(&c.face))
                            .collect();
                        slots.sort_by(|a, b| {
                            b.face
                                .mana_value
                                .partial_cmp(&a.face.mana_value)
                                .unwrap_or(std::cmp::Ordering::Equal)
                        });
                        slots.iter().map(|c| c.id).take(to_discard).collect()
                    };
                    for id in ids {
                        let _ = session.discard(id);
                    }
                }
                if session.advance_turn().is_err() {
                    break;
                }
            }

            // Greedy land-drop: play a land if available.
            let land_id = session
                .hand
                .iter()
                .find(|c| PlaytestSession::is_land(&c.face))
                .map(|c| c.id);
            if let Some(id) = land_id {
                let _ = session.play_land(id);
            }

            // Snapshot after land drop.
            let snap =
                session
                    .history
                    .last()
                    .cloned()
                    .unwrap_or_else(|| super::solitaire::TurnSnapshot {
                        turn_number: (turn_idx + 1) as u32,
                        hand_size: session.hand.len(),
                        lands_in_play: 0,
                        mana_sources_in_play: 0,
                        available_mana: session.available_mana(),
                        cards_drawn: 0,
                        lands_in_hand: session.lands_in_hand(),
                        playable_count: session.playable_in_hand().len(),
                    });

            // Re-read available mana after land drop (snapshot was before).
            let mana = session.available_mana();

            acc_hand[turn_idx] += snap.hand_size as f64;
            acc_lands_play[turn_idx] += snap.lands_in_play as f64;
            acc_sources[turn_idx] += snap.mana_sources_in_play as f64;
            acc_mana[turn_idx] += mana as f64;
            acc_mana_sq[turn_idx] += (mana as f64).powi(2);
            acc_playable[turn_idx] += session.playable_in_hand().len() as f64;
            acc_land_in_hand[turn_idx] += (snap.lands_in_hand > 0) as u32 as f64;
            acc_empty_lib[turn_idx] += session.library.is_empty() as u32 as f64;
            if (mana as f64) < acc_mana_min[turn_idx] {
                acc_mana_min[turn_idx] = mana as f64;
            }
            if (mana as f64) > acc_mana_max[turn_idx] {
                acc_mana_max[turn_idx] = mana as f64;
            }
        }

        games_simulated += 1;
    }

    // Aggregate.
    let n = games_simulated as f64;
    let turns = (0..num_turns)
        .map(|i| {
            let avg_mana = acc_mana[i] / n;
            let variance = (acc_mana_sq[i] / n) - avg_mana.powi(2);
            let stddev = variance.max(0.0).sqrt();
            TurnAggregate {
                turn_number: (i + 1) as u32,
                avg_hand_size: acc_hand[i] / n,
                avg_lands_in_play: acc_lands_play[i] / n,
                avg_mana_sources: acc_sources[i] / n,
                avg_available_mana: avg_mana,
                avg_playable_count: acc_playable[i] / n,
                pct_land_in_hand: acc_land_in_hand[i] / n,
                pct_empty_library: acc_empty_lib[i] / n,
                min_available_mana: if n > 0.0 && acc_mana_min[i] < f64::MAX {
                    acc_mana_min[i]
                } else {
                    0.0
                },
                max_available_mana: if n > 0.0 && acc_mana_max[i] > f64::MIN {
                    acc_mana_max[i]
                } else {
                    0.0
                },
                stddev_available_mana: stddev,
            }
        })
        .collect();

    let opening_hand = if games_simulated > 0 {
        OpeningHandStats {
            avg_lands: oh_lands / n,
            avg_spells: (oh_hand_size - oh_lands) / n,
            avg_hand_size: oh_hand_size / n,
            hand_size_distribution: oh_hand_dist,
            avg_mulligans: oh_mulligans / n,
            pct_keep_first: oh_keep_first as f64 / n,
        }
    } else {
        OpeningHandStats::default()
    };

    SimulationResult {
        turns,
        opening_hand,
        games_simulated,
        config: config.clone(),
    }
}

// ── Unit tests ────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::card::CardFace;
    use crate::types::card_type::{CardType, CoreType};

    fn land(name: &str) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Land],
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn spell(name: &str, cmc: u32) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: vec![CoreType::Instant],
                ..Default::default()
            },
            mana_value: Some(cmc as f32),
            ..Default::default()
        }
    }

    fn deck_24l_36s() -> Vec<CardFace> {
        let mut d: Vec<CardFace> = (0..24).map(|i| land(&format!("Forest {i}"))).collect();
        d.extend((0..36).map(|i| spell(&format!("Spell {i}"), 2)));
        d
    }

    #[test]
    fn simulation_produces_correct_turn_count() {
        let deck = deck_24l_36s();
        let config = SimulationConfig {
            num_games: 10,
            num_turns: 5,
            ..Default::default()
        };
        let result = run_simulation(&deck, &config);
        assert_eq!(result.turns.len(), 5);
        assert_eq!(result.games_simulated, 10);
    }

    #[test]
    fn avg_lands_in_play_increases_over_turns() {
        let deck = deck_24l_36s();
        let config = SimulationConfig {
            num_games: 100,
            num_turns: 6,
            auto_keep: true,
            ..Default::default()
        };
        let result = run_simulation(&deck, &config);
        // Each turn drops a land — avg lands in play should increase.
        for i in 1..result.turns.len() {
            assert!(
                result.turns[i].avg_lands_in_play >= result.turns[i - 1].avg_lands_in_play,
                "avg_lands_in_play decreased from turn {} to {}",
                i,
                i + 1
            );
        }
    }

    #[test]
    fn empty_deck_returns_zero_games() {
        let config = SimulationConfig::default();
        let result = run_simulation(&[], &config);
        assert_eq!(result.games_simulated, 0);
        assert!(result.turns.is_empty());
    }

    #[test]
    fn auto_keep_skips_mulligans() {
        let deck = deck_24l_36s();
        let config = SimulationConfig {
            num_games: 20,
            num_turns: 1,
            auto_keep: true,
            ..Default::default()
        };
        let result = run_simulation(&deck, &config);
        // All games should have avg_mulligans = 0.
        assert_eq!(result.opening_hand.avg_mulligans, 0.0);
        assert_eq!(result.opening_hand.pct_keep_first, 1.0);
    }
}

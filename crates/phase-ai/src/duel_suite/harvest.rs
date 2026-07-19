//! Self-play eval-feature harvesting — the observer half of the Texel retrain
//! pipeline (U4).
//!
//! [`GameHarvester`] rides the [`super::run::drive_game_observed`] observer seam,
//! snapshotting the last quiescent (empty-stack) position of each completed turn
//! from **p0's** perspective. [`FeatureRow::extract`] is the single
//! reconstruction authority: the exact unweighted feature vector the planner's
//! leaf eval (`evaluate_with_strategy`) applies its weights to, minus the two
//! serve-time carve-outs (`energy_offset`, `threat_adjustment`). Fitting weights
//! against these rows and their final-outcome labels (logistic regression) is the
//! Texel tuning method; see `scripts/train_eval_weights.py`.
//!
//! Serialization is JSONL. One [`HarvestSink`] exists per suite run: line 1 is a
//! file-scoped meta record; every subsequent line is one [`HarvestRecord`].

use std::fs::File;
use std::io::{self, BufWriter, Write};
use std::path::Path;

use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use serde::{Deserialize, Serialize};

use crate::eval::{evaluate_features, EvalWeights};
use crate::session::AiSession;

/// p0's perspective is fixed for the harvest — the win-label is p0's outcome and
/// every feature is a (p0 − opponent) differential.
const HARVEST_PERSPECTIVE: PlayerId = PlayerId(0);

/// The 9 fitted eval features plus the `energy_offset` regression control, all
/// unweighted. Field names are exactly the [`EvalWeights`] field names (no proxy
/// map): the Python trainer maps each coefficient directly onto its weight.
///
/// This is the serve reconstruction of `evaluate_with_strategy`'s linear
/// component: `weighted_total(w)` equals the planner's leaf eval minus
/// `energy_offset` (a fixed serve-time offset) and `threat_adjustment` (an
/// unfitted heuristic). The serve-reconstruction test in `planner::mod` pins that
/// identity so a future strategic term without a matching field fails loudly.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FeatureRow {
    pub life: f64,
    pub board_presence: f64,
    pub board_power: f64,
    pub board_toughness: f64,
    pub hand_size: f64,
    pub aggression: f64,
    /// `evaluate_features` nc-diff plus `card_advantage::differential` — the full
    /// value the `card_advantage` weight multiplies at serve time.
    pub card_advantage: f64,
    /// `zone_eval::zone_bonus` for p0's archetype.
    pub zone_quality: f64,
    /// `SynergyGraph::board_synergy_bonus`.
    pub synergy: f64,
    /// Fixed-coefficient control (`energy × 0.1`); excluded from `weighted_total`,
    /// discarded by the trainer.
    pub energy_offset: f64,
}

impl FeatureRow {
    /// Reconstruct the fitted feature vector from `player`'s perspective. Returns
    /// `None` for terminal positions (`evaluate_features` short-circuits with
    /// `Err`) so terminal, label-leaking snapshots are never harvested.
    ///
    /// The `zone_quality` archetype and `synergy` graph are read from `session`
    /// exactly as `build_ai_context_with_session` reads them for the live planner
    /// (`session.deck_profile[player].archetype`, `session.synergy[player]`), so
    /// the harvested vector matches the served one for the same session.
    pub(crate) fn extract(
        state: &GameState,
        session: &AiSession,
        player: PlayerId,
    ) -> Option<FeatureRow> {
        let features = evaluate_features(state, player).ok()?;
        let differential = crate::card_advantage::differential(state, player);
        let synergy = session
            .synergy
            .get(&player)
            .map_or(0.0, |graph| graph.board_synergy_bonus(state, player));
        let archetype = session
            .deck_profile
            .get(&player)
            .map(|profile| profile.archetype)
            .unwrap_or_default();
        let zone_quality = crate::zone_eval::zone_bonus(state, player, archetype);

        Some(FeatureRow {
            life: features.life,
            board_presence: features.board_presence,
            board_power: features.board_power,
            board_toughness: features.board_toughness,
            hand_size: features.hand_size,
            aggression: features.aggression,
            card_advantage: features.card_advantage_breakdown + differential,
            zone_quality,
            synergy,
            energy_offset: features.energy_offset,
        })
    }

    /// Weighted sum of all 9 fitted features, **excluding** `energy_offset`. Mirrors
    /// `evaluate_with_strategy` minus its two serve-time carve-outs.
    pub fn weighted_total(&self, w: &EvalWeights) -> f64 {
        self.life * w.life
            + self.board_presence * w.board_presence
            + self.board_power * w.board_power
            + self.board_toughness * w.board_toughness
            + self.hand_size * w.hand_size
            + self.aggression * w.aggression
            + self.card_advantage * w.card_advantage
            + self.zone_quality * w.zone_quality
            + self.synergy * w.synergy
    }
}

/// One labeled training row: a quiescent p0-perspective feature vector plus the
/// per-game identity and the final win label.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HarvestRecord {
    pub seed: u64,
    pub matchup_id: String,
    pub game_idx: usize,
    pub turn: u32,
    pub features: FeatureRow,
    pub won: bool,
}

/// File-scoped provenance written as line 1 of each JSONL shard. Per-game
/// identity (`seed`, `matchup_id`, `game_idx`) lives on each record, since one
/// seed is ill-defined at file scope for a multi-matchup shard.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct HarvestMeta {
    pub schema: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_sha: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub card_data_hash: Option<String>,
    pub difficulty: String,
}

/// Wrapper so the meta line serializes as `{"meta": {...}}` — records have no
/// `meta` key, so the trainer discriminates them by key presence.
#[derive(Serialize)]
struct MetaLine<'a> {
    meta: &'a HarvestMeta,
}

/// Per-game observer. Keeps the LAST empty-stack snapshot seen for the current
/// turn in `pending`; when `turn_number` advances, that snapshot is flushed to
/// `buffer` as the completed turn's quiescent record. A turn whose every
/// batch-boundary observation had a non-empty stack produces zero records (a gap
/// — sampling, not census).
pub(crate) struct GameHarvester {
    seed: u64,
    matchup_id: String,
    game_idx: usize,
    player: PlayerId,
    /// `(turn_number, row)` — the latest empty-stack snapshot of the current turn.
    pending: Option<(u32, FeatureRow)>,
    /// Flushed quiescent snapshots of completed turns, in turn order.
    buffer: Vec<(u32, FeatureRow)>,
}

impl GameHarvester {
    /// New harvester for one game. Perspective is fixed to p0.
    pub fn new(seed: u64, matchup_id: String, game_idx: usize) -> Self {
        Self {
            seed,
            matchup_id,
            game_idx,
            player: HARVEST_PERSPECTIVE,
            pending: None,
            buffer: Vec::new(),
        }
    }

    /// Observe a batch-boundary position. Flushes the previous turn's pending
    /// snapshot on turn advance, then (re)sets `pending` for the current turn when
    /// the stack is empty and a non-terminal feature vector is extractable.
    pub fn observe(&mut self, state: &GameState, session: &AiSession) {
        if let Some((pending_turn, _)) = &self.pending {
            if state.turn_number > *pending_turn {
                // Safe: guarded by the `is_some` match above.
                let completed = self.pending.take().expect("pending present");
                self.buffer.push(completed);
            }
        }

        if state.stack.is_empty() {
            if let Some(row) = FeatureRow::extract(state, session, self.player) {
                self.pending = Some((state.turn_number, row));
            }
        }
    }

    /// Finish the game, dropping the in-progress final turn's pending snapshot
    /// (terminal-adjacent positions are the most label-leaking). Returns empty for
    /// `winner == None` (draws, action-cap games, panicked games) — an unlabeled
    /// corpus is useless, so those games contribute nothing.
    pub fn finish(self, winner: Option<PlayerId>) -> Vec<HarvestRecord> {
        // `pending` is intentionally dropped with `self`.
        let Some(winner) = winner else {
            return Vec::new();
        };
        let won = winner == self.player;
        self.buffer
            .into_iter()
            .map(|(turn, features)| HarvestRecord {
                seed: self.seed,
                matchup_id: self.matchup_id.clone(),
                game_idx: self.game_idx,
                turn,
                features,
                won,
            })
            .collect()
    }
}

/// One sink per suite run: created once before the matchup loop, writes the
/// single file-scoped meta line at construction, then appends every game's
/// records through the same `BufWriter`. Per-matchup `File::create` is wrong — it
/// would clobber the file and emit N meta lines under the sequential branch.
pub(crate) struct HarvestSink {
    writer: BufWriter<File>,
}

impl HarvestSink {
    /// Create the shard, writing the meta record as line 1.
    pub fn create(path: &Path, meta: &HarvestMeta) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut writer = BufWriter::new(File::create(path)?);
        serde_json::to_writer(&mut writer, &MetaLine { meta }).map_err(io::Error::other)?;
        writer.write_all(b"\n")?;
        Ok(Self { writer })
    }

    /// Append a game's records, one JSON object per line.
    pub fn write_records(&mut self, records: &[HarvestRecord]) -> io::Result<()> {
        for record in records {
            serde_json::to_writer(&mut self.writer, record).map_err(io::Error::other)?;
            self.writer.write_all(b"\n")?;
        }
        Ok(())
    }

    /// Flush the buffered writer to disk.
    pub fn flush(&mut self) -> io::Result<()> {
        self.writer.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};

    fn fresh_session() -> AiSession {
        AiSession::empty()
    }

    /// A minimal non-empty stack: contents are irrelevant to `observe`, which only
    /// reads `stack.is_empty()`.
    fn push_stack(state: &mut GameState) {
        state.stack.push_back(StackEntry {
            id: ObjectId(9_999),
            source_id: ObjectId(9_999),
            controller: PlayerId(0),
            kind: StackEntryKind::Spell {
                card_id: CardId(9_999),
                ability: None,
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    /// Row "Snapshots are quiescent" + "Turn-flush semantics": a non-empty-stack
    /// batch boundary yields no snapshot; each buffered record is a distinct,
    /// strictly-increasing completed turn; the in-progress final turn is dropped.
    #[test]
    fn observe_gates_on_empty_stack_and_flushes_per_completed_turn() {
        let session = fresh_session();
        let mut harvester = GameHarvester::new(42, "unit".to_string(), 0);

        let mut state = GameState::new_two_player(42);

        // Turn 1: a NON-empty-stack boundary (reach-guard — the gate must filter
        // this) then an empty-stack boundary that sets pending.
        state.turn_number = 1;
        push_stack(&mut state);
        harvester.observe(&state, &session);
        assert!(
            harvester.pending.is_none(),
            "non-empty stack must not snapshot"
        );
        state.stack.clear();
        harvester.observe(&state, &session);
        assert_eq!(
            harvester.pending.as_ref().map(|(t, _)| *t),
            Some(1),
            "empty stack sets pending for the current turn"
        );

        // Turn 2: empty-stack boundary flushes turn 1 and sets pending for 2. A
        // later non-empty boundary in the SAME turn neither flushes nor overwrites.
        state.turn_number = 2;
        harvester.observe(&state, &session);
        assert_eq!(harvester.buffer.len(), 1, "turn advance flushes turn 1");
        push_stack(&mut state);
        harvester.observe(&state, &session);
        assert_eq!(
            harvester.pending.as_ref().map(|(t, _)| *t),
            Some(2),
            "non-empty mid-turn boundary keeps turn 2's pending"
        );
        state.stack.clear();

        // Turn 3: flush turn 2. Leaves turn 3 pending (in-progress final turn).
        state.turn_number = 3;
        harvester.observe(&state, &session);
        assert_eq!(harvester.buffer.len(), 2);

        let records = harvester.finish(Some(PlayerId(0)));
        let turns: Vec<u32> = records.iter().map(|r| r.turn).collect();
        assert_eq!(
            turns,
            vec![1, 2],
            "≤1 record per completed turn, strictly increasing, final turn dropped"
        );
        assert!(records.iter().all(|r| r.won), "p0 won ⇒ every row won:true");
    }

    /// Row "Labeling correct": winner drives `won`; a `None` winner yields no rows
    /// even though completed turns were buffered (reach-guard).
    #[test]
    fn finish_labels_by_winner_and_drops_unlabeled_games() {
        let session = fresh_session();
        let build = || {
            let mut h = GameHarvester::new(1, "m".to_string(), 3);
            let mut state = GameState::new_two_player(1);
            state.turn_number = 1;
            h.observe(&state, &session); // empty stack ⇒ pending turn 1
            state.turn_number = 2;
            h.observe(&state, &session); // flush turn 1
            h
        };

        assert!(build().finish(Some(PlayerId(0))).iter().all(|r| r.won));
        assert!(build().finish(Some(PlayerId(1))).iter().all(|r| !r.won));
        assert_eq!(
            build().finish(Some(PlayerId(1)))[0].game_idx,
            3,
            "per-game identity is carried onto the record"
        );
        assert!(
            build().finish(None).is_empty(),
            "unlabeled game (draw / cap / panic) contributes no rows"
        );
    }

    /// Terminal positions are skipped at capture time.
    #[test]
    fn observe_skips_terminal_positions() {
        let session = fresh_session();
        let mut harvester = GameHarvester::new(1, "m".to_string(), 0);
        let mut state = GameState::new_two_player(1);
        state.turn_number = 1;
        state.waiting_for = WaitingFor::GameOver {
            winner: Some(PlayerId(0)),
        };
        harvester.observe(&state, &session);
        assert!(
            harvester.pending.is_none(),
            "GameOver ⇒ evaluate_features Err ⇒ no snapshot"
        );
    }

    /// `FeatureRow` survives a JSONL round-trip and `weighted_total` sums the 9
    /// fitted features while excluding the energy control.
    #[test]
    fn feature_row_round_trips_and_excludes_energy_from_weighted_total() {
        let row = FeatureRow {
            life: 1.0,
            board_presence: 2.0,
            board_power: 3.0,
            board_toughness: 4.0,
            hand_size: 5.0,
            aggression: 6.0,
            card_advantage: 7.0,
            zone_quality: 8.0,
            synergy: 9.0,
            energy_offset: 100.0,
        };
        let json = serde_json::to_string(&row).unwrap();
        let back: FeatureRow = serde_json::from_str(&json).unwrap();
        assert_eq!(row, back);

        let w = EvalWeights {
            life: 1.0,
            aggression: 1.0,
            board_presence: 1.0,
            board_power: 1.0,
            board_toughness: 1.0,
            hand_size: 1.0,
            zone_quality: 1.0,
            card_advantage: 1.0,
            synergy: 1.0,
        };
        // 1+2+3+4+5+6+7+8+9 = 45; energy_offset (100) is excluded.
        assert!((row.weighted_total(&w) - 45.0).abs() < 1e-9);
    }
}

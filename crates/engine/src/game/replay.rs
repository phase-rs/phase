//! Deterministic action-based replay reconstruction.
//!
//! A recorded game (`ReplayLog`) carries no per-turn state — only the inputs
//! needed to reconstruct its starting state (`ReplayHeader`) and the ordered
//! sequence of actions that were submitted and accepted. Because `apply` is a
//! pure reducer over a `GameState` seeded from a fixed RNG seed, replaying the
//! same actions against the same starting state reproduces the original game
//! exactly. `ReplayPlayer` wraps that reconstruction with sparse checkpoint
//! caching so scrubbing to an arbitrary point doesn't re-simulate the whole
//! game from turn 1 every time.

use std::collections::BTreeMap;

use thiserror::Error;

use crate::database::CardDatabase;
use crate::types::game_state::GameState;
use crate::types::player::PlayerId;
use crate::types::replay::{RecordedAction, ReplayHeader, ReplayLog, REPLAY_FORMAT_VERSION};

use super::deck_loading::{load_and_hydrate_decks, resolve_deck_list};
use super::engine::{apply, start_game, start_game_with_starting_player};

/// Checkpoints are cached every `CHECKPOINT_INTERVAL` actions, bounding cache
/// size to roughly `len / CHECKPOINT_INTERVAL` snapshots while keeping any
/// `seek` to at most this many replayed actions from the nearest one.
/// `GameState::clone()` is O(log n) (the `im`-backed structural-sharing
/// containers documented in CLAUDE.md), so caching at this granularity is
/// cheap relative to re-running `apply` from scratch.
const CHECKPOINT_INTERVAL: u32 = 20;

#[derive(Debug, Error)]
pub enum ReplayError {
    #[error("replay is missing its format version")]
    MissingFormatVersion,
    #[error("unsupported replay format version {version}; this engine supports version 2")]
    UnsupportedFormatVersion { version: u32 },
    /// An action that was recorded as having succeeded failed to re-apply
    /// during reconstruction. This means the recording and the engine version
    /// replaying it have diverged (e.g. an engine change altered behavior for
    /// a state this recording depends on) — it is not a normal rules outcome.
    #[error("replay action {index} desynced reconstruction: {message}")]
    Desync { index: u32, message: String },
    /// The header carries deck data (the recorded game was not started with
    /// empty libraries) but no `CardDatabase` was supplied to resolve it.
    /// Silently skipping deck hydration in this case would reconstruct a
    /// *different* starting state (empty libraries) than the one the
    /// original game actually had — a wrong-but-quiet result is worse than
    /// failing loudly here.
    #[error(
        "replay requires a card database to resolve its recorded deck data, but none was loaded"
    )]
    MissingCardDatabase,
}

/// Reconstruct the state immediately after `start_game` (before any recorded
/// action has been applied) from a `ReplayHeader` alone. Reuses the same
/// canonical init sequence every transport (WASM, server-core) already
/// shares — see `load_and_hydrate_decks` — so reconstruction can't drift from
/// how the original game was actually started.
///
/// Errors with `ReplayError::MissingCardDatabase` when `header.deck_data` is
/// `Some` but `db` is `None` — that combination can only reconstruct a
/// wrong starting state (empty libraries instead of the recorded deck), so
/// it isn't accepted silently. `db: None` is only valid when
/// `header.deck_data` is also `None` (a format that genuinely starts with
/// empty libraries).
pub fn reconstruct_initial_state(
    header: &ReplayHeader,
    db: Option<&CardDatabase>,
) -> Result<GameState, ReplayError> {
    let mut state = GameState::new(
        header.format_config.clone(),
        header.player_count,
        header.seed,
    );
    // CR 732.2a: project the combo-detector opt-in onto `loop_detection` via the
    // single authority shared by every transport, so a replay of a detector-on
    // game reconstructs with the same runtime gate the original game had.
    state.set_match_config(header.match_config);

    // Mirror `initialize_game`: local WASM games always run with
    // `debug_mode = true`, and sandbox games (`allow_debug_actions`) pre-seed
    // `debug_permitted` for every seat before the first action. Without this,
    // a replay that contains `GrantDebugPermission` or `RevokeDebugPermission`
    // actions (which are NOT `GameAction::Debug(_)` and therefore DO get
    // recorded) applies them against an empty `debug_permitted` set instead of
    // the pre-seeded one, producing a different permission state — and any
    // subsequent `Debug(_)` actions replayed through `apply` fail outright
    // because `debug_mode` would be `false`.
    state.debug_mode = true;
    if state.format_config.allow_debug_actions {
        for i in 0..header.player_count {
            state.debug_permitted.insert(PlayerId(i));
        }
    }

    match (&header.deck_data, db) {
        (Some(deck_data), Some(db)) => {
            let payload = resolve_deck_list(db, deck_data);
            load_and_hydrate_decks(&mut state, &payload, Some(db));
            state.all_card_names = db.card_names().into();
        }
        (Some(_), None) => return Err(ReplayError::MissingCardDatabase),
        (None, _) => {}
    }

    match header.first_player {
        Some(0) => start_game_with_starting_player(&mut state, PlayerId(0)),
        Some(1) => start_game_with_starting_player(&mut state, PlayerId(1)),
        _ => start_game(&mut state),
    };
    Ok(state)
}

/// Deterministic playback over a `ReplayLog`. Holds sparse cached
/// checkpoints (see `CHECKPOINT_INTERVAL`) so repeated scrubbing doesn't
/// re-simulate the whole game on every call.
#[derive(Debug)]
pub struct ReplayPlayer {
    log: ReplayLog,
    checkpoints: BTreeMap<u32, GameState>,
    /// Holds the most recently reconstructed state when `seek` lands on a
    /// non-checkpoint-aligned index, so `seek` can return a borrow of `self`
    /// without caching every single scrubbed-through position.
    scratch: Option<GameState>,
}

impl ReplayPlayer {
    /// Build a player for `log`, eagerly reconstructing the index-0
    /// (post-`start_game`) checkpoint. `db` is forwarded to
    /// `reconstruct_initial_state`, which errors if `log.header.deck_data`
    /// is `Some` and `db` is `None` — see that function's doc comment.
    pub fn load(log: ReplayLog, db: Option<&CardDatabase>) -> Result<Self, ReplayError> {
        match log.format_version {
            Some(REPLAY_FORMAT_VERSION) => {}
            Some(version) => return Err(ReplayError::UnsupportedFormatVersion { version }),
            None => return Err(ReplayError::MissingFormatVersion),
        }
        let initial = reconstruct_initial_state(&log.header, db)?;
        let mut checkpoints = BTreeMap::new();
        checkpoints.insert(0, initial);
        Ok(Self {
            log,
            checkpoints,
            scratch: None,
        })
    }

    /// Total number of recorded actions. Valid `seek` targets are `0..=len()`.
    pub fn len(&self) -> u32 {
        self.log.actions.len() as u32
    }

    pub fn is_empty(&self) -> bool {
        self.log.actions.is_empty()
    }

    pub fn header(&self) -> &ReplayHeader {
        &self.log.header
    }

    pub fn action_at(&self, index: u32) -> Option<&RecordedAction> {
        self.log.actions.get(index as usize)
    }

    /// Reconstruct and return the state immediately after action `target`
    /// has been applied (`target == 0` is the post-`start_game` state before
    /// any action). Clamped to `len()`.
    pub fn seek(&mut self, target: u32) -> Result<&GameState, ReplayError> {
        let target = target.min(self.len());

        if self.checkpoints.contains_key(&target) {
            return Ok(self.checkpoints.get(&target).expect("just checked"));
        }

        let state = self.replay_from_nearest_checkpoint(target)?;
        if target.is_multiple_of(CHECKPOINT_INTERVAL) || target == self.len() {
            self.checkpoints.insert(target, state);
            return Ok(self.checkpoints.get(&target).expect("just inserted"));
        }

        self.scratch = Some(state);
        Ok(self.scratch.as_ref().expect("just set"))
    }

    fn replay_from_nearest_checkpoint(&self, target: u32) -> Result<GameState, ReplayError> {
        let (&start_idx, base) = self
            .checkpoints
            .range(..=target)
            .next_back()
            .expect("index 0 checkpoint is always present");
        let mut state = base.clone();
        for recorded in &self.log.actions[start_idx as usize..target as usize] {
            apply(&mut state, recorded.actor, recorded.action.clone()).map_err(|e| {
                ReplayError::Desync {
                    index: recorded.seq,
                    message: e.to_string(),
                }
            })?;
        }
        Ok(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::ability::{TriggerBaseSetInstanceRef, TriggerDefinitionOccurrenceRef};
    use crate::types::actions::GameAction;
    use crate::types::format::FormatConfig;
    use crate::types::game_state::{ProductionOverride, WaitingFor};
    use crate::types::identifiers::{ObjectId, ObjectIncarnationRef};
    use crate::types::mana::{
        ManaSourcePenalty, ManaSourceSelection, ManaType, TapsForManaSelection,
    };
    use crate::types::match_config::MatchConfig;

    fn two_player_header(seed: u64) -> ReplayHeader {
        ReplayHeader {
            format_config: FormatConfig::standard(),
            match_config: MatchConfig::default(),
            player_count: 2,
            first_player: Some(0),
            seed,
            deck_data: None,
        }
    }

    /// If the live state is sitting on a priority decision, return the
    /// `(actor, action)` pair that passes it — the simplest fully-generic
    /// action available regardless of deck contents, which is exactly why
    /// this test can run with `deck_data: None` (no CardDatabase needed).
    fn next_priority_pass(state: &GameState) -> Option<(PlayerId, GameAction)> {
        match state.waiting_for {
            WaitingFor::Priority { player } => Some((player, GameAction::PassPriority)),
            _ => None,
        }
    }

    #[test]
    fn load_rejects_missing_format_version_before_reconstruction() {
        let mut log = ReplayLog::new(two_player_header(1));
        log.format_version = None;
        let error = ReplayPlayer::load(log, None).expect_err("legacy replay must be rejected");
        assert!(matches!(error, ReplayError::MissingFormatVersion));
    }

    #[test]
    fn load_rejects_unknown_format_version_before_reconstruction() {
        let mut log = ReplayLog::new(two_player_header(1));
        log.format_version = Some(REPLAY_FORMAT_VERSION + 1);
        let error = ReplayPlayer::load(log, None).expect_err("future replay must be rejected");
        assert!(matches!(
            error,
            ReplayError::UnsupportedFormatVersion { version }
                if version == REPLAY_FORMAT_VERSION + 1
        ));
    }

    #[test]
    fn version_two_roundtrips_semantic_mana_source_selections() {
        let source = ObjectIncarnationRef::of(ObjectId(7), 3);
        let aura = ObjectIncarnationRef::of(ObjectId(9), 2);
        let action = GameAction::TapLandForMana {
            selection: ManaSourceSelection {
                source,
                ability_index: None,
                mana_type: ManaType::Green,
                atomic_combination: None,
                restrictions: Vec::new(),
                penalty: ManaSourcePenalty::None,
                taps_for_mana: vec![TapsForManaSelection {
                    source: aura,
                    occurrence: TriggerDefinitionOccurrenceRef::Printed {
                        base_set: TriggerBaseSetInstanceRef::INITIAL,
                        printed_index: 0,
                    },
                    production_override: ProductionOverride::SingleColor(ManaType::Red),
                }],
            },
        };
        let mut log = ReplayLog::new(two_player_header(5));
        log.push_action(PlayerId(0), action.clone());

        let json = serde_json::to_string(&log).expect("serialize replay v2");
        let restored: ReplayLog = serde_json::from_str(&json).expect("deserialize replay v2");

        assert_eq!(restored.format_version, Some(REPLAY_FORMAT_VERSION));
        assert_eq!(restored.actions.len(), 1);
        assert_eq!(restored.actions[0].action, action);
    }

    #[test]
    fn replay_player_reconstructs_every_recorded_index() {
        let header = two_player_header(99);
        let mut live = reconstruct_initial_state(&header, None)
            .expect("deck_data is None, so reconstruction cannot fail");

        let mut log = ReplayLog::new(header);
        let mut live_snapshots = vec![live.clone()];

        // Pass priority a handful of times — enough to walk through several
        // phases of turn 1 without reaching turn 2's draw step (which would
        // lose the game to CR 704.5b against an empty library).
        for _ in 0..8 {
            let Some((actor, action)) = next_priority_pass(&live) else {
                break;
            };
            apply(&mut live, actor, action.clone())
                .expect("passing priority while waiting on it is always legal");
            log.push_action(actor, action);
            live_snapshots.push(live.clone());
        }

        assert!(
            log.actions.len() >= 4,
            "expected several priority passes to have been recorded"
        );
        assert_eq!(log.actions.len(), live_snapshots.len() - 1);

        let mut player =
            ReplayPlayer::load(log, None).expect("deck_data is None, so load cannot fail");
        assert_eq!(player.len(), live_snapshots.len() as u32 - 1);

        for (index, expected) in live_snapshots.iter().enumerate() {
            let got = player
                .seek(index as u32)
                .unwrap_or_else(|e| panic!("seek({index}) desynced: {e}"));
            assert_eq!(
                got.turn_number, expected.turn_number,
                "turn_number at {index}"
            );
            assert_eq!(got.phase, expected.phase, "phase at {index}");
            assert_eq!(
                got.active_player, expected.active_player,
                "active_player at {index}"
            );
            assert_eq!(
                got.waiting_for, expected.waiting_for,
                "waiting_for at {index}"
            );
        }
    }

    #[test]
    fn replay_player_seeks_out_of_order_and_caches_correctly() {
        let header = two_player_header(42);
        let mut live = reconstruct_initial_state(&header, None)
            .expect("deck_data is None, so reconstruction cannot fail");
        let mut log = ReplayLog::new(header);

        for _ in 0..6 {
            let Some((actor, action)) = next_priority_pass(&live) else {
                break;
            };
            apply(&mut live, actor, action.clone()).unwrap();
            log.push_action(actor, action);
        }
        let total = log.actions.len() as u32;
        assert!(total >= 3);

        let mut player =
            ReplayPlayer::load(log, None).expect("deck_data is None, so load cannot fail");

        // Seek forward, then back, then forward again — exercises both the
        // checkpoint-cache hit path and the nearest-checkpoint replay path.
        let last = player.seek(total).unwrap().clone();
        let first = player.seek(0).unwrap().clone();
        let last_again = player.seek(total).unwrap().clone();

        assert_eq!(first.turn_number, 1);
        assert_eq!(last.waiting_for, last_again.waiting_for);
        assert_eq!(last.phase, last_again.phase);
    }

    #[test]
    fn reconstruct_initial_state_fails_loudly_without_card_database() {
        let mut header = two_player_header(13);
        header.deck_data = Some(crate::game::deck_loading::DeckList::default());

        let err = reconstruct_initial_state(&header, None)
            .expect_err("deck_data present with no CardDatabase must error, not silently reconstruct empty libraries");
        assert!(matches!(err, ReplayError::MissingCardDatabase));

        // ReplayPlayer::load propagates the same failure.
        let log = ReplayLog::new(header);
        let load_err = ReplayPlayer::load(log, None)
            .expect_err("load must surface the same MissingCardDatabase failure");
        assert!(matches!(load_err, ReplayError::MissingCardDatabase));
    }

    #[test]
    fn sandbox_game_reconstruct_pre_seeds_debug_permitted_matching_initialize_game() {
        // `initialize_game` seeds `debug_permitted` for every seat when
        // `allow_debug_actions` is true. Without the parallel seeding in
        // `reconstruct_initial_state`, a replay that contains
        // `GrantDebugPermission` or `RevokeDebugPermission` actions (which
        // are recorded, unlike `Debug(_)` ones) applies them against an
        // empty set, producing a different permission state and desyncing
        // the reconstruction from the original game.
        let sandbox_config = FormatConfig::standard().with_sandbox();
        let header = ReplayHeader {
            format_config: sandbox_config,
            match_config: MatchConfig::default(),
            player_count: 2,
            first_player: Some(0),
            seed: 7,
            deck_data: None,
        };

        let state = reconstruct_initial_state(&header, None)
            .expect("deck_data is None, so reconstruction cannot fail");

        // Both seats must be in debug_permitted, mirroring initialize_game.
        assert!(
            state.debug_mode,
            "sandbox reconstruct must have debug_mode = true, matching initialize_game"
        );
        assert!(
            state.debug_permitted.contains(&PlayerId(0)),
            "seat 0 must be in debug_permitted after sandbox reconstruction"
        );
        assert!(
            state.debug_permitted.contains(&PlayerId(1)),
            "seat 1 must be in debug_permitted after sandbox reconstruction"
        );

        // A non-sandbox game must leave debug_permitted empty.
        let non_sandbox_header = two_player_header(7);
        let non_sandbox = reconstruct_initial_state(&non_sandbox_header, None)
            .expect("non-sandbox deck_data is None");
        assert!(
            non_sandbox.debug_permitted.is_empty(),
            "non-sandbox reconstruct must leave debug_permitted empty"
        );

        // Replaying a RevokeDebugPermission action against the correctly
        // pre-seeded state must produce the same permission set as the live
        // game — if the set were empty (the pre-fix bug), remove would be a
        // no-op and the reconstructed state would diverge.
        let mut live = reconstruct_initial_state(&header, None).unwrap();
        let mut log = ReplayLog::new(header.clone());

        apply(
            &mut live,
            PlayerId(0),
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(1),
            },
        )
        .expect("host revoking P1 permission must be accepted in a sandbox game");
        log.push_action(
            PlayerId(0),
            GameAction::RevokeDebugPermission {
                player_id: PlayerId(1),
            },
        );
        assert!(
            live.debug_permitted.contains(&PlayerId(0)),
            "host must still be in debug_permitted after revoking P1"
        );
        assert!(
            !live.debug_permitted.contains(&PlayerId(1)),
            "P1 must be removed from debug_permitted after revoke"
        );

        // Reconstruct to the same point via replay.
        let mut player = ReplayPlayer::load(log, None).unwrap();
        let replayed = player.seek(1).unwrap();
        assert_eq!(
            replayed.debug_permitted, live.debug_permitted,
            "replayed debug_permitted must match live game after RevokeDebugPermission"
        );
    }

    #[test]
    fn replay_player_seek_clamps_to_length_and_handles_empty_log() {
        let header = two_player_header(7);
        let log = ReplayLog::new(header);
        let mut player =
            ReplayPlayer::load(log, None).expect("deck_data is None, so load cannot fail");

        assert_eq!(player.len(), 0);
        assert!(player.is_empty());

        // Seeking past the end of an empty log clamps to 0, not an error.
        let state = player
            .seek(50)
            .expect("empty log still has the initial state");
        assert_eq!(state.turn_number, 1);
    }
}

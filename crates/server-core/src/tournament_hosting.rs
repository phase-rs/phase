//! Server-authoritative tournament match hosting.
//!
//! Mirrors the draft match-hosting template ([`crate::draft_session`]) for
//! tournaments: it spawns a real [`crate::session::GameSession`] for each ready
//! pairing, records the `(tournament, pairing) -> game_code` binding, and — on
//! the game's server-verified terminal — the caller hands the result back to the
//! tournament core via [`lobby_broker::tournament::TournamentManager::report_result_system`]
//! ([`Self::tournament_for_game_code`] is that handoff's entry point).
//!
//! The tournament CORE (`lobby_broker::tournament`) stays free of `GameState`
//! and `game_code` by design; this server layer owns both. It asks the core
//! [`TournamentManager::hostable_pairings`] for readiness and
//! [`TournamentManager::begin_hosting`] for the durable host marker, and never
//! re-derives either — the core is the single authority for both.

use std::collections::HashMap;
use std::sync::Arc;

use engine::database::{CardDatabase, ComboTable};
use engine::types::format::FormatConfig;
use engine::types::match_config::{MatchConfig, MatchType};
use engine::types::player::PlayerId;
use lobby_broker::tournament::{HostablePairing, PairingId, TournamentManager};
use lobby_broker::BrokerEnv;
use seat_reducer::types::DeckChoice;
use tracing::warn;

use crate::deck_resolve::{self, deck_data_from_payload};
use crate::session::SessionManager;

/// Binds hosted tournament pairings to the game sessions serving them. The
/// tournament core is deliberately `game_code`-free (see `tournament.rs`
/// `begin_hosting`: "Freeing the live game is the server layer's concern (its
/// own `active_matches`)"); this map IS that concern.
#[derive(Debug, Default)]
pub struct TournamentHostingManager {
    /// `(tournament_code, pairing_id) -> spawned game_code`.
    active_matches: HashMap<(String, PairingId), String>,
}

/// A spawned hosted tournament match.
#[derive(Debug, Clone)]
pub struct TournamentMatchSpawn {
    pub code: String,
    pub pairing_id: PairingId,
    pub game_code: String,
    /// One entry per seat, in pairing `players` order — seat `i` is
    /// `pairing.players[i]` and game `PlayerId(i)`, the mapping the game-over
    /// handoff inverts.
    pub seats: Vec<TournamentMatchSeat>,
}

#[derive(Debug, Clone)]
pub struct TournamentMatchSeat {
    pub player_key: String,
    pub game_player: PlayerId,
}

impl TournamentHostingManager {
    pub fn new() -> Self {
        Self::default()
    }

    /// The game_code serving `(code, pairing_id)`, if one is live.
    pub fn game_code_for(&self, code: &str, pairing_id: PairingId) -> Option<&str> {
        self.active_matches
            .get(&(code.to_string(), pairing_id))
            .map(String::as_str)
    }

    /// Reverse lookup: the `(tournament, pairing)` a spawned game serves — the
    /// game-over handoff's entry point (mirrors `draft_for_game_code`).
    pub fn tournament_for_game_code(&self, game_code: &str) -> Option<(String, PairingId)> {
        self.active_matches
            .iter()
            .find(|(_, gc)| gc.as_str() == game_code)
            .map(|((code, pairing_id), _)| (code.clone(), *pairing_id))
    }

    /// Forget a finished match's binding — called once its result has been
    /// handed back to the core, so the freed pairing can never re-resolve to a
    /// torn-down game. The core's `hosted` provenance is durable and untouched;
    /// only this server-layer binding is dropped.
    pub fn forget(&mut self, code: &str, pairing_id: PairingId) -> Option<String> {
        self.active_matches.remove(&(code.to_string(), pairing_id))
    }

    /// Spawn a game for every pairing the core reports hostable and this manager
    /// is not already serving. For each pairing: resolve both seats' submitted
    /// decks, create → join → start the game, mark the pairing hosted in the
    /// core, and record the binding.
    ///
    /// A pairing whose decks fail to resolve, or whose game fails to spawn, is
    /// skipped (logged) and left un-hosted, so a later tick retries it and the
    /// core never marks a pairing hosted without a live game behind it. Marking
    /// hosted happens only *after* the game starts, so a partial spawn cannot
    /// strand a pairing in `ReportGate::Hosted` with no game to report it.
    pub fn spawn_ready_matches(
        &mut self,
        code: &str,
        tournaments: &mut TournamentManager,
        game_mgr: &mut SessionManager,
        db: &Arc<CardDatabase>,
        combos: &ComboTable,
        env: &impl BrokerEnv,
    ) -> Vec<TournamentMatchSpawn> {
        let mut spawns = Vec::new();
        // `hostable_pairings` returns an owned Vec, so the immutable borrow of
        // `tournaments` ends here and the loop body is free to borrow it mutably
        // (`begin_hosting`).
        for hostable in tournaments.hostable_pairings(code) {
            let key = (code.to_string(), hostable.pairing_id);
            if self.active_matches.contains_key(&key) {
                continue;
            }
            match spawn_one(code, &hostable, tournaments, game_mgr, db, combos, env) {
                Ok(spawn) => {
                    self.active_matches.insert(key, spawn.game_code.clone());
                    spawns.push(spawn);
                }
                Err(error) => {
                    warn!(
                        tournament = %code,
                        pairing = hostable.pairing_id,
                        %error,
                        "tournament match spawn skipped for pairing"
                    );
                }
            }
        }
        spawns
    }
}

/// Create → join → start the game for one hostable pairing, then mark it hosted.
/// Mirrors the per-pairing body of `draft_session::spawn_match_games_for_round`.
fn spawn_one(
    code: &str,
    hostable: &HostablePairing,
    tournaments: &mut TournamentManager,
    game_mgr: &mut SessionManager,
    db: &Arc<CardDatabase>,
    combos: &ComboTable,
    env: &impl BrokerEnv,
) -> Result<TournamentMatchSpawn, String> {
    // Hosted v1 is Swiss + head-to-head + Bo3 (the core's `is_hosted_eligible`
    // guarantees the two-seat shape), played on a standard constructed table.
    let match_config = MatchConfig {
        match_type: MatchType::Bo3,
        ..Default::default()
    };
    let format_config = FormatConfig::standard();

    let decks = hostable
        .seats
        .iter()
        .map(|seat| deck_resolve::resolve_deck(db, combos, &seat.deck))
        .collect::<Result<Vec<_>, String>>()?;

    // A submitted deck arrives as raw `DeckData`; each seat's provenance is
    // recovered from its resolved payload against the same database (mirrors the
    // draft spawn).
    let choice0 = DeckChoice::DeckList(Box::new(deck_data_from_payload(db, &decks[0])));
    let choice1 = DeckChoice::DeckList(Box::new(deck_data_from_payload(db, &decks[1])));

    let (game_code, _token0) = game_mgr.create_game_n_players(
        decks[0].clone(),
        Some(choice0),
        hostable.seats[0].display_name.clone(),
        None,
        2,
        match_config,
        Some(format_config),
    )?;

    // The manager created this game moments ago and no handle has been handed
    // out, so the synchronous exclusive accessor is the right one — `server-core`
    // stays free of `async` (mirrors the draft spawn).
    let token1 = {
        let session = game_mgr
            .session_exclusive(&game_code)
            .ok_or_else(|| format!("spawned game missing: {game_code}"))?;
        let (token, _) = session.join_with_reservation(
            decks[1].clone(),
            Some(choice1),
            hostable.seats[1].display_name.clone(),
            None,
        )?;
        session
            .start_game(db)
            .map_err(|e| format!("start_game failed for {game_code}: {e:?}"))?;
        token
    };
    game_mgr.index_token(token1, &game_code);

    // Mark hosted only now the game is live: the core re-validates eligibility
    // and the report gate, and bumps the durable host generation. If this fails
    // the caller skips the pairing and does NOT record the binding, so the
    // just-spawned game is orphaned rather than left masquerading as the hosted
    // table — acceptable because `begin_hosting` only fails on a state the
    // readiness gate already excluded (a terminal tournament, a bye/forfeit).
    tournaments.begin_hosting(code, hostable.pairing_id, env)?;

    Ok(TournamentMatchSpawn {
        code: code.to_string(),
        pairing_id: hostable.pairing_id,
        game_code,
        seats: hostable
            .seats
            .iter()
            .enumerate()
            .map(|(i, seat)| TournamentMatchSeat {
                player_key: seat.player_key.clone(),
                game_player: PlayerId(i as u8),
            })
            .collect(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    use engine::starter_decks::DeckData;
    use lobby_broker::tournament::{
        BracketShape, CreateTournamentRequest, MatchArity, ScoringPolicy,
    };

    /// Deterministic broker env for the hosting tests.
    struct FakeEnv {
        counter: Cell<u64>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                counter: Cell::new(0),
            }
        }

        fn next(&self) -> u64 {
            let n = self.counter.get();
            self.counter.set(n + 1);
            n
        }
    }

    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            1_000
        }
        fn new_token(&self) -> String {
            format!("tok{}", self.next())
        }
        fn new_game_code(&self) -> String {
            format!("G{}", self.next())
        }
    }

    fn deck_of(name: &str) -> DeckData {
        DeckData {
            main_deck: vec![name.to_string(); 40],
            ..Default::default()
        }
    }

    /// A 2-player Swiss+Bo3 tournament with round 1 paired and both decks in — a
    /// single hostable pairing.
    fn ready_tournament(env: &FakeEnv) -> TournamentManager {
        let arity = MatchArity::new(2).expect("valid arity");
        let mut mgr = TournamentManager::new();
        mgr.create_tournament(
            "T",
            CreateTournamentRequest {
                name: "Test Event".to_string(),
                arity,
                scoring: ScoringPolicy::default_for_arity(arity),
                bracket: BracketShape::Swiss,
                total_rounds: None,
                plus_rounds: None,
                format: None,
                match_type: None,
            },
            env,
        )
        .expect("create_tournament");
        mgr.join_tournament("T", "p00", "Player 0", env)
            .expect("join 0");
        mgr.join_tournament("T", "p01", "Player 1", env)
            .expect("join 1");
        mgr.generate_pairings("T", env).expect("round 1");
        mgr.submit_deck("T", "p00", deck_of("Island"), env)
            .expect("deck 0");
        mgr.submit_deck("T", "p01", deck_of("Forest"), env)
            .expect("deck 1");
        mgr
    }

    /// An empty card database cannot resolve the submitted decks, so the ready
    /// pairing is skipped — and, crucially, NOT marked hosted, so a later tick
    /// (with a real DB) can still spawn it. This is the "never strand a pairing
    /// in `ReportGate::Hosted` without a live game" invariant.
    #[test]
    fn spawn_skips_and_does_not_host_when_decks_fail_to_resolve() {
        let env = FakeEnv::new();
        let mut tournaments = ready_tournament(&env);
        let pairing_id = tournaments.get("T").expect("t").pairings[0].id;
        assert!(
            !tournaments.hostable_pairings("T").is_empty(),
            "the pairing is ready to host"
        );

        let mut hosting = TournamentHostingManager::new();
        let mut game_mgr = SessionManager::new();
        let db = Arc::new(CardDatabase::default());
        let combos = ComboTable::default();
        let spawns =
            hosting.spawn_ready_matches("T", &mut tournaments, &mut game_mgr, &db, &combos, &env);

        assert!(spawns.is_empty(), "unresolved decks spawn no game");
        assert_eq!(game_mgr.game_count(), 0);
        assert_eq!(
            tournaments.hosting_generation("T", pairing_id),
            None,
            "a pairing whose game never spawned must not be left marked hosted"
        );
    }

    /// The `(tournament, pairing) <-> game_code` binding supports both lookups
    /// and is dropped by `forget`.
    #[test]
    fn map_reverse_lookup_and_forget() {
        let mut hosting = TournamentHostingManager::new();
        hosting
            .active_matches
            .insert(("T".to_string(), 7), "GAME1".to_string());

        assert_eq!(hosting.game_code_for("T", 7), Some("GAME1"));
        assert_eq!(
            hosting.tournament_for_game_code("GAME1"),
            Some(("T".to_string(), 7))
        );
        assert_eq!(hosting.tournament_for_game_code("absent"), None);

        assert_eq!(hosting.forget("T", 7), Some("GAME1".to_string()));
        assert_eq!(hosting.game_code_for("T", 7), None);
        assert_eq!(hosting.tournament_for_game_code("GAME1"), None);
    }
}

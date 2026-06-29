//! Swiss-pairing tournament organizer — pure registry alongside [`LobbyManager`].
//!
//! Organizes multi-round P2P Swiss events: registration, round lifecycle, drops,
//! pairings with rematch avoidance, and standings using the standard Magic
//! tiebreaker order (match points → OMW% → GW% → OGW%, each floored at 1/3).

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::env::BrokerEnv;
use crate::protocol::{
    PairingView, TournamentStanding, TournamentStatus, TournamentSummary, TournamentView,
};

/// Capacity cap for registered tournaments in broker memory.
pub const MAX_TOURNAMENT_ENTRIES: usize = 100;
/// Max players per tournament.
pub const MAX_TOURNAMENT_PLAYERS: usize = 128;
/// Default Swiss round count when the organizer does not specify one.
pub const DEFAULT_SWISS_ROUNDS: u8 = 3;
/// Minimum players required to start a Swiss event.
pub const MIN_TOURNAMENT_PLAYERS: usize = 4;

/// Tiebreaker floor used by official Magic tournaments (33%).
const TIE_BREAKER_FLOOR: f64 = 1.0 / 3.0;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentPlayer {
    pub player_key: String,
    pub display_name: String,
    pub dropped: bool,
    pub had_bye: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MatchResult {
    pub winner_player_key: Option<String>,
    pub player_a_wins: u8,
    pub player_b_wins: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentPairing {
    pub match_id: String,
    pub round: u8,
    pub table: u8,
    pub player_a: String,
    pub player_b: Option<String>,
    pub result: Option<MatchResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TournamentMeta {
    pub code: String,
    pub name: String,
    pub organizer_name: String,
    pub created_at: u64,
    pub status: TournamentStatus,
    pub total_rounds: u8,
    pub current_round: u8,
    pub players: Vec<TournamentPlayer>,
    pub pairings: Vec<TournamentPairing>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
struct PlayerRecord {
    match_wins: u32,
    match_losses: u32,
    match_draws: u32,
    game_wins: u32,
    game_losses: u32,
    opponents: Vec<String>,
}

/// Pure tournament registry — no I/O, deterministic given `BrokerEnv` injection.
#[derive(Debug, Default, Serialize, Deserialize)]
pub struct TournamentManager {
    tournaments: HashMap<String, TournamentMeta>,
}

impl TournamentManager {
    pub fn new() -> Self {
        Self {
            tournaments: HashMap::new(),
        }
    }

    pub fn len(&self) -> usize {
        self.tournaments.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tournaments.is_empty()
    }

    pub fn has_tournament(&self, code: &str) -> bool {
        self.tournaments.contains_key(code)
    }

    pub fn get(&self, code: &str) -> Option<&TournamentMeta> {
        self.tournaments.get(code)
    }

    pub fn public_summaries(&self) -> Vec<TournamentSummary> {
        let mut summaries: Vec<TournamentSummary> = self
            .tournaments
            .values()
            .filter(|t| t.status != TournamentStatus::Completed)
            .map(|t| TournamentSummary {
                tournament_code: t.code.clone(),
                name: t.name.clone(),
                organizer_name: t.organizer_name.clone(),
                created_at: t.created_at,
                status: t.status,
                player_count: active_player_count(t),
                total_rounds: t.total_rounds,
                current_round: t.current_round,
            })
            .collect();
        summaries.sort_by_key(|s| std::cmp::Reverse(s.created_at));
        summaries
    }

    pub fn register_tournament(
        &mut self,
        code: &str,
        name: String,
        organizer_name: String,
        total_rounds: u8,
        env: &impl BrokerEnv,
    ) {
        let rounds = total_rounds.max(1);
        self.tournaments.insert(
            code.to_string(),
            TournamentMeta {
                code: code.to_string(),
                name,
                organizer_name,
                created_at: env.now_ms(),
                status: TournamentStatus::Registration,
                total_rounds: rounds,
                current_round: 0,
                players: Vec::new(),
                pairings: Vec::new(),
            },
        );
    }

    pub fn unregister_tournament(&mut self, code: &str) -> bool {
        self.tournaments.remove(code).is_some()
    }

    pub fn join_tournament(
        &mut self,
        code: &str,
        display_name: String,
        player_key: String,
    ) -> Result<(), String> {
        let tournament = self
            .tournaments
            .get_mut(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        if tournament.status != TournamentStatus::Registration {
            return Err("Tournament registration is closed".to_string());
        }
        if tournament.players.len() >= MAX_TOURNAMENT_PLAYERS {
            return Err("Tournament is full".to_string());
        }
        if tournament
            .players
            .iter()
            .any(|p| p.display_name.eq_ignore_ascii_case(&display_name))
        {
            return Err("Display name already taken in this tournament".to_string());
        }
        tournament.players.push(TournamentPlayer {
            player_key,
            display_name,
            dropped: false,
            had_bye: false,
        });
        Ok(())
    }

    pub fn drop_player(&mut self, code: &str, player_key: &str) -> Result<(), String> {
        let tournament = self
            .tournaments
            .get_mut(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        let player = tournament
            .players
            .iter_mut()
            .find(|p| p.player_key == player_key)
            .ok_or_else(|| "You are not registered in this tournament".to_string())?;
        if player.dropped {
            return Err("Already dropped from tournament".to_string());
        }
        player.dropped = true;
        Ok(())
    }

    pub fn start_round(&mut self, code: &str, env: &impl BrokerEnv) -> Result<(), String> {
        let tournament = self
            .tournaments
            .get_mut(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        if tournament.status == TournamentStatus::Completed {
            return Err("Tournament is already completed".to_string());
        }
        let active = active_players(tournament);
        if tournament.status == TournamentStatus::Registration {
            if active.len() < MIN_TOURNAMENT_PLAYERS {
                return Err(format!(
                    "Need at least {MIN_TOURNAMENT_PLAYERS} players to start"
                ));
            }
            tournament.status = TournamentStatus::InProgress;
            tournament.current_round = 1;
        } else {
            if !round_complete(tournament) {
                return Err("Current round is not complete".to_string());
            }
            if tournament.current_round >= tournament.total_rounds {
                tournament.status = TournamentStatus::Completed;
                return Ok(());
            }
            tournament.current_round += 1;
        }

        let round = tournament.current_round;
        let records = compute_records(tournament);
        let prior_pairs = prior_opponent_pairs(tournament);
        let new_pairings =
            generate_swiss_pairings(tournament, round, &records, &prior_pairs, env.now_ms());
        tournament.pairings.extend(new_pairings);
        Ok(())
    }

    pub fn report_result(
        &mut self,
        code: &str,
        match_id: &str,
        result: MatchResult,
    ) -> Result<(), String> {
        let tournament = self
            .tournaments
            .get_mut(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        if tournament.status != TournamentStatus::InProgress {
            return Err("Tournament is not in progress".to_string());
        }
        let pairing = tournament
            .pairings
            .iter_mut()
            .find(|p| p.match_id == match_id && p.round == tournament.current_round)
            .ok_or_else(|| format!("Pairing not found: {match_id}"))?;
        if pairing.player_b.is_none() {
            return Err("Cannot report result for a bye".to_string());
        }
        validate_match_result(pairing, &result)?;
        pairing.result = Some(result);
        Ok(())
    }

    pub fn end_tournament(&mut self, code: &str) -> Result<(), String> {
        let tournament = self
            .tournaments
            .get_mut(code)
            .ok_or_else(|| format!("Tournament not found: {code}"))?;
        tournament.status = TournamentStatus::Completed;
        Ok(())
    }

    pub fn check_expired(&mut self, timeout_secs: u64, env: &impl BrokerEnv) -> Vec<String> {
        let now = env.now_ms();
        let cutoff = now.saturating_sub(timeout_secs.saturating_mul(1000));
        let expired: Vec<String> = self
            .tournaments
            .iter()
            .filter(|(_, t)| t.created_at < cutoff)
            .map(|(code, _)| code.clone())
            .collect();
        for code in &expired {
            self.tournaments.remove(code);
        }
        expired
    }

    pub fn to_view(&self, code: &str) -> Option<TournamentView> {
        self.tournaments.get(code).map(build_view)
    }
}

fn active_player_count(tournament: &TournamentMeta) -> u32 {
    tournament.players.iter().filter(|p| !p.dropped).count() as u32
}

fn active_players(tournament: &TournamentMeta) -> Vec<&TournamentPlayer> {
    tournament.players.iter().filter(|p| !p.dropped).collect()
}

fn round_complete(tournament: &TournamentMeta) -> bool {
    let round = tournament.current_round;
    if round == 0 {
        return false;
    }
    tournament
        .pairings
        .iter()
        .filter(|p| p.round == round)
        .all(|p| p.player_b.is_none() || p.result.is_some())
}

fn validate_match_result(pairing: &TournamentPairing, result: &MatchResult) -> Result<(), String> {
    let player_b = pairing
        .player_b
        .as_ref()
        .ok_or_else(|| "Invalid pairing".to_string())?;
    let valid_keys = [&pairing.player_a, player_b];
    if let Some(winner) = &result.winner_player_key {
        if !valid_keys.iter().any(|k| *k == winner) {
            return Err("Winner must be one of the paired players".to_string());
        }
    }
    if result.player_a_wins == result.player_b_wins && result.winner_player_key.is_some() {
        return Err("Draw results must not specify a winner".to_string());
    }
    Ok(())
}

fn prior_opponent_pairs(tournament: &TournamentMeta) -> HashSet<(String, String)> {
    tournament
        .pairings
        .iter()
        .filter_map(|p| p.player_b.as_ref().map(|b| (p.player_a.clone(), b.clone())))
        .flat_map(|(a, b)| [(a.clone(), b.clone()), (b, a)])
        .collect()
}

fn compute_records(tournament: &TournamentMeta) -> HashMap<String, PlayerRecord> {
    let mut records: HashMap<String, PlayerRecord> = tournament
        .players
        .iter()
        .map(|p| (p.player_key.clone(), PlayerRecord::default()))
        .collect();

    for pairing in &tournament.pairings {
        let Some(player_b) = pairing.player_b.as_ref() else {
            if let Some(rec) = records.get_mut(&pairing.player_a) {
                rec.match_wins += 1;
                rec.game_wins += 1;
            }
            continue;
        };
        let Some(result) = pairing.result.as_ref() else {
            continue;
        };
        let a = &pairing.player_a;
        let b = player_b;
        if let Some(rec_a) = records.get_mut(a) {
            rec_a.game_wins += u32::from(result.player_a_wins);
            rec_a.game_losses += u32::from(result.player_b_wins);
            rec_a.opponents.push(b.clone());
        }
        if let Some(rec_b) = records.get_mut(b) {
            rec_b.game_wins += u32::from(result.player_b_wins);
            rec_b.game_losses += u32::from(result.player_a_wins);
            rec_b.opponents.push(a.clone());
        }
        match &result.winner_player_key {
            Some(winner) if winner == a => {
                records.get_mut(a).unwrap().match_wins += 1;
                records.get_mut(b).unwrap().match_losses += 1;
            }
            Some(winner) if winner == b => {
                records.get_mut(b).unwrap().match_wins += 1;
                records.get_mut(a).unwrap().match_losses += 1;
            }
            Some(_) => {}
            None => {
                records.get_mut(a).unwrap().match_draws += 1;
                records.get_mut(b).unwrap().match_draws += 1;
            }
        }
    }
    records
}

fn match_points(record: &PlayerRecord) -> u32 {
    record.match_wins * 3 + record.match_draws
}

fn game_win_percentage(record: &PlayerRecord) -> f64 {
    let total = record.game_wins + record.game_losses;
    if total == 0 {
        return TIE_BREAKER_FLOOR;
    }
    (record.game_wins as f64 / total as f64).max(TIE_BREAKER_FLOOR)
}

fn opponent_match_win_percentage(
    record: &PlayerRecord,
    records: &HashMap<String, PlayerRecord>,
) -> f64 {
    if record.opponents.is_empty() {
        return TIE_BREAKER_FLOOR;
    }
    let sum: f64 = record
        .opponents
        .iter()
        .filter_map(|opp| records.get(opp))
        .map(|opp_rec| {
            let played = opp_rec.match_wins + opp_rec.match_losses + opp_rec.match_draws;
            if played == 0 {
                return TIE_BREAKER_FLOOR;
            }
            (opp_rec.match_wins as f64 * 3.0 + opp_rec.match_draws as f64) / (played as f64 * 3.0)
        })
        .map(|pct| pct.max(TIE_BREAKER_FLOOR))
        .sum();
    (sum / record.opponents.len() as f64).max(TIE_BREAKER_FLOOR)
}

fn opponent_game_win_percentage(
    record: &PlayerRecord,
    records: &HashMap<String, PlayerRecord>,
) -> f64 {
    if record.opponents.is_empty() {
        return TIE_BREAKER_FLOOR;
    }
    let sum: f64 = record
        .opponents
        .iter()
        .filter_map(|opp| records.get(opp))
        .map(|opp_rec| game_win_percentage(opp_rec))
        .sum();
    (sum / record.opponents.len() as f64).max(TIE_BREAKER_FLOOR)
}

fn build_standings(tournament: &TournamentMeta) -> Vec<TournamentStanding> {
    let records = compute_records(tournament);
    let mut standings: Vec<TournamentStanding> = tournament
        .players
        .iter()
        .filter(|p| !p.dropped || records.get(&p.player_key).is_some_and(|r| r.match_wins > 0))
        .map(|p| {
            let record = records.get(&p.player_key).cloned().unwrap_or_default();
            TournamentStanding {
                player_key: p.player_key.clone(),
                display_name: p.display_name.clone(),
                dropped: p.dropped,
                match_points: match_points(&record),
                match_wins: record.match_wins,
                match_losses: record.match_losses,
                match_draws: record.match_draws,
                game_wins: record.game_wins,
                game_losses: record.game_losses,
                omw_percentage: opponent_match_win_percentage(&record, &records),
                gw_percentage: game_win_percentage(&record),
                ogw_percentage: opponent_game_win_percentage(&record, &records),
            }
        })
        .collect();

    standings.sort_by(|a, b| {
        b.match_points
            .cmp(&a.match_points)
            .then_with(|| {
                b.omw_percentage
                    .partial_cmp(&a.omw_percentage)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.gw_percentage
                    .partial_cmp(&a.gw_percentage)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| {
                b.ogw_percentage
                    .partial_cmp(&a.ogw_percentage)
                    .unwrap_or(std::cmp::Ordering::Equal)
            })
            .then_with(|| a.display_name.cmp(&b.display_name))
    });
    standings
}

fn build_view(tournament: &TournamentMeta) -> TournamentView {
    let player_names: HashMap<String, String> = tournament
        .players
        .iter()
        .map(|p| (p.player_key.clone(), p.display_name.clone()))
        .collect();

    let pairings: Vec<PairingView> = tournament
        .pairings
        .iter()
        .filter(|p| p.round == tournament.current_round)
        .map(|p| {
            let name_a = player_names
                .get(&p.player_a)
                .cloned()
                .unwrap_or_else(|| "Unknown".to_string());
            let name_b = p
                .player_b
                .as_ref()
                .and_then(|k| player_names.get(k).cloned());
            PairingView {
                match_id: p.match_id.clone(),
                round: p.round,
                table: p.table,
                player_a_key: p.player_a.clone(),
                player_a_name: name_a,
                player_b_key: p.player_b.clone(),
                player_b_name: name_b,
                reported: p.result.is_some(),
                winner_player_key: p.result.as_ref().and_then(|r| r.winner_player_key.clone()),
            }
        })
        .collect();

    TournamentView {
        tournament_code: tournament.code.clone(),
        name: tournament.name.clone(),
        organizer_name: tournament.organizer_name.clone(),
        created_at: tournament.created_at,
        status: tournament.status,
        total_rounds: tournament.total_rounds,
        current_round: tournament.current_round,
        player_count: active_player_count(tournament),
        standings: build_standings(tournament),
        pairings,
    }
}

/// Swiss pairing: score-group pairing with backtracking rematch avoidance,
/// float-down for odd groups, bye preferring players without a prior bye.
fn generate_swiss_pairings(
    tournament: &mut TournamentMeta,
    round: u8,
    records: &HashMap<String, PlayerRecord>,
    prior_pairs: &HashSet<(String, String)>,
    seed: u64,
) -> Vec<TournamentPairing> {
    let mut active: Vec<String> = tournament
        .players
        .iter()
        .filter(|p| !p.dropped)
        .map(|p| p.player_key.clone())
        .collect();

    active.sort_by(|a, b| {
        let pts_a = records.get(a).map(match_points).unwrap_or(0);
        let pts_b = records.get(b).map(match_points).unwrap_or(0);
        pts_b.cmp(&pts_a)
    });

    let mut brackets: Vec<Vec<String>> = Vec::new();
    let mut current_pts: Option<u32> = None;
    for key in active {
        let pts = records.get(&key).map(match_points).unwrap_or(0);
        if current_pts != Some(pts) {
            brackets.push(Vec::new());
            current_pts = Some(pts);
        }
        brackets.last_mut().unwrap().push(key);
    }

    let mut rng = SimpleRng::new(seed ^ u64::from(round));
    for bracket in &mut brackets {
        rng.shuffle(bracket);
    }

    let mut paired: Vec<(String, String)> = Vec::new();
    let mut carry: Option<String> = None;

    for bracket in brackets {
        let mut pool = bracket;
        if let Some(c) = carry.take() {
            pool.insert(0, c);
        }

        let (bracket_pairs, floated) =
            pair_bracket_with_backtracking(&pool, prior_pairs, &mut paired);
        paired.extend(bracket_pairs);
        carry = floated;
    }

    let mut bye_player: Option<String> = None;
    if let Some(unpaired) = carry {
        bye_player = Some(select_bye_player(tournament, &unpaired, records, &mut rng));
    }

    if let Some(ref bye_key) = bye_player {
        if let Some(player) = tournament
            .players
            .iter_mut()
            .find(|p| p.player_key == *bye_key)
        {
            player.had_bye = true;
        }
    }

    let mut pairings: Vec<TournamentPairing> = paired
        .iter()
        .enumerate()
        .map(|(table, (a, b))| TournamentPairing {
            match_id: format!("{round}-t{table}"),
            round,
            table: table as u8,
            player_a: a.clone(),
            player_b: Some(b.clone()),
            result: None,
        })
        .collect();

    if let Some(bye_key) = bye_player {
        pairings.push(TournamentPairing {
            match_id: format!("{round}-bye"),
            round,
            table: pairings.len() as u8,
            player_a: bye_key,
            player_b: None,
            result: Some(MatchResult {
                winner_player_key: None,
                player_a_wins: 1,
                player_b_wins: 0,
            }),
        });
    }

    pairings
}

fn pair_bracket_with_backtracking(
    pool: &[String],
    prior_pairs: &HashSet<(String, String)>,
    already_paired: &[(String, String)],
) -> (Vec<(String, String)>, Option<String>) {
    if pool.is_empty() {
        return (Vec::new(), None);
    }
    if pool.len() == 1 {
        return (Vec::new(), Some(pool[0].clone()));
    }

    let used: HashSet<String> = already_paired
        .iter()
        .flat_map(|(a, b)| [a.clone(), b.clone()])
        .collect();

    let available: Vec<String> = pool
        .iter()
        .filter(|k| !used.contains(*k))
        .cloned()
        .collect();

    if let Some(solution) = backtrack_pair(&available, prior_pairs, &mut Vec::new()) {
        let floated = if solution.len() * 2 < available.len() {
            available
                .iter()
                .find(|k| !solution.iter().any(|(a, b)| a == *k || b == *k))
                .cloned()
        } else {
            None
        };
        return (solution, floated);
    }

    // Fallback: greedy pairing allowing rematches if backtracking fails.
    let mut remaining = available;
    let mut pairs = Vec::new();
    let mut floated = None;
    while remaining.len() >= 2 {
        let first = remaining.remove(0);
        let partner_idx = remaining
            .iter()
            .position(|p| !prior_pairs.contains(&(first.clone(), p.clone())))
            .unwrap_or(0);
        let partner = remaining.remove(partner_idx);
        pairs.push((first, partner));
    }
    if remaining.len() == 1 {
        floated = Some(remaining.remove(0));
    }
    (pairs, floated)
}

fn backtrack_pair(
    players: &[String],
    prior_pairs: &HashSet<(String, String)>,
    acc: &mut Vec<(String, String)>,
) -> Option<Vec<(String, String)>> {
    if players.is_empty() {
        return Some(acc.clone());
    }
    if players.len() == 1 {
        return None;
    }
    let first = &players[0];
    for (i, partner) in players.iter().enumerate().skip(1) {
        if prior_pairs.contains(&(first.clone(), partner.clone())) {
            continue;
        }
        let mut rest: Vec<String> = players
            .iter()
            .enumerate()
            .filter(|(idx, _)| *idx != 0 && *idx != i)
            .map(|(_, k)| k.clone())
            .collect();
        acc.push((first.clone(), partner.clone()));
        if let Some(solution) = backtrack_pair(&rest, prior_pairs, acc) {
            return Some(solution);
        }
        acc.pop();
        let _ = &mut rest;
    }
    None
}

/// Deterministic PRNG for bracket shuffles — no `rand` crate in this WASM-safe core.
struct SimpleRng {
    state: u64,
}

impl SimpleRng {
    fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_mul(6364136223846793005).wrapping_add(1);
        self.state
    }

    fn shuffle(&mut self, slice: &mut [String]) {
        for i in (1..slice.len()).rev() {
            let j = (self.next_u64() as usize) % (i + 1);
            slice.swap(i, j);
        }
    }
}

fn select_bye_player(
    tournament: &TournamentMeta,
    floated: &str,
    records: &HashMap<String, PlayerRecord>,
    _rng: &mut SimpleRng,
) -> String {
    let active_without_bye: Vec<String> = tournament
        .players
        .iter()
        .filter(|p| !p.dropped && !p.had_bye)
        .map(|p| p.player_key.clone())
        .collect();

    if active_without_bye.contains(&floated.to_string()) {
        return floated.to_string();
    }

    let mut candidates: Vec<String> = active_without_bye;
    if candidates.is_empty() {
        return floated.to_string();
    }
    candidates.sort_by(|a, b| {
        let pts_a = records.get(a).map(match_points).unwrap_or(0);
        let pts_b = records.get(b).map(match_points).unwrap_or(0);
        pts_a.cmp(&pts_b)
    });
    candidates
        .first()
        .cloned()
        .unwrap_or_else(|| floated.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::env::BrokerEnv;

    struct FakeEnv {
        now: u64,
        code_seq: std::cell::Cell<u32>,
        token_seq: std::cell::Cell<u32>,
    }

    impl FakeEnv {
        fn new() -> Self {
            Self {
                now: 1_000_000,
                code_seq: std::cell::Cell::new(1),
                token_seq: std::cell::Cell::new(1),
            }
        }
    }

    impl BrokerEnv for FakeEnv {
        fn now_ms(&self) -> u64 {
            self.now
        }
        fn new_token(&self) -> String {
            let n = self.token_seq.get();
            self.token_seq.set(n + 1);
            format!("tok-{n}")
        }
        fn new_game_code(&self) -> String {
            let n = self.code_seq.get();
            self.code_seq.set(n + 1);
            format!("T{n:04}")
        }
    }

    fn register_players(mgr: &mut TournamentManager, code: &str, count: usize) {
        for i in 0..count {
            let key = format!("p{i}");
            mgr.join_tournament(code, format!("Player {i}"), key)
                .expect("join");
        }
    }

    #[test]
    fn registration_and_join() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Friday Swiss".into(), "TO".into(), 3, &env);
        mgr.join_tournament("T0001", "Alice".into(), "alice".into())
            .unwrap();
        let view = mgr.to_view("T0001").unwrap();
        assert_eq!(view.player_count, 1);
        assert_eq!(view.status, TournamentStatus::Registration);
    }

    #[test]
    fn swiss_pairings_cover_all_active_players_round_1() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Event".into(), "TO".into(), 3, &env);
        register_players(&mut mgr, "T0001", 8);
        mgr.start_round("T0001", &env).unwrap();
        let view = mgr.to_view("T0001").unwrap();
        assert_eq!(view.current_round, 1);
        assert_eq!(view.pairings.len(), 4);
        let mut paired: HashSet<String> = HashSet::new();
        for p in &view.pairings {
            paired.insert(p.player_a_key.clone());
            if let Some(b) = &p.player_b_key {
                paired.insert(b.clone());
            }
        }
        assert_eq!(paired.len(), 8);
    }

    #[test]
    fn standings_sort_by_match_points_then_tiebreakers() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Event".into(), "TO".into(), 1, &env);
        for (i, name) in ["A", "B", "C", "D"].iter().enumerate() {
            mgr.join_tournament("T0001", (*name).into(), format!("p{i}"))
                .unwrap();
        }
        mgr.start_round("T0001", &env).unwrap();
        let view = mgr.to_view("T0001").unwrap();
        let m1 = view.pairings[0].match_id.clone();
        mgr.report_result(
            "T0001",
            &m1,
            MatchResult {
                winner_player_key: Some(view.pairings[0].player_a_key.clone()),
                player_a_wins: 2,
                player_b_wins: 0,
            },
        )
        .unwrap();
        let standings = mgr.to_view("T0001").unwrap().standings;
        assert!(!standings.is_empty());
        assert!(standings[0].match_points >= standings.last().unwrap().match_points);
    }

    #[test]
    fn drop_player_prevents_future_pairing() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Event".into(), "TO".into(), 2, &env);
        register_players(&mut mgr, "T0001", 5);
        mgr.drop_player("T0001", "p4").unwrap();
        mgr.start_round("T0001", &env).unwrap();
        let view = mgr.to_view("T0001").unwrap();
        let paired: HashSet<_> = view
            .pairings
            .iter()
            .flat_map(|p| std::iter::once(p.player_a_key.clone()).chain(p.player_b_key.clone()))
            .collect();
        assert!(!paired.contains("p4"));
    }

    #[test]
    fn odd_player_count_assigns_bye() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Event".into(), "TO".into(), 1, &env);
        register_players(&mut mgr, "T0001", 5);
        mgr.start_round("T0001", &env).unwrap();
        let view = mgr.to_view("T0001").unwrap();
        let byes = view
            .pairings
            .iter()
            .filter(|p| p.player_b_key.is_none())
            .count();
        assert_eq!(byes, 1);
    }

    #[test]
    fn rematch_avoided_when_possible_in_round_2() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Event".into(), "TO".into(), 2, &env);
        register_players(&mut mgr, "T0001", 8);
        mgr.start_round("T0001", &env).unwrap();
        for p in mgr.get("T0001").unwrap().pairings.clone() {
            if let Some(b) = p.player_b {
                mgr.report_result(
                    "T0001",
                    &p.match_id,
                    MatchResult {
                        winner_player_key: Some(p.player_a.clone()),
                        player_a_wins: 2,
                        player_b_wins: 0,
                    },
                )
                .unwrap();
                let _ = b;
            }
        }
        mgr.start_round("T0001", &env).unwrap();
        let prior: HashSet<(String, String)> = mgr
            .get("T0001")
            .unwrap()
            .pairings
            .iter()
            .filter(|p| p.round == 1)
            .filter_map(|p| p.player_b.as_ref().map(|b| (p.player_a.clone(), b.clone())))
            .flat_map(|(a, b)| [(a.clone(), b.clone()), (b, a)])
            .collect();
        let round2 = mgr
            .to_view("T0001")
            .unwrap()
            .pairings
            .into_iter()
            .filter(|p| p.round == 2)
            .collect::<Vec<_>>();
        for p in round2 {
            if let Some(b) = &p.player_b_key {
                let a = &p.player_a_key;
                assert!(
                    !prior.contains(&(a.clone(), b.clone())),
                    "rematch: {a} vs {b}"
                );
            }
        }
    }

    #[test]
    fn public_summaries_omit_completed() {
        let env = FakeEnv::new();
        let mut mgr = TournamentManager::new();
        mgr.register_tournament("T0001", "Open".into(), "TO".into(), 1, &env);
        mgr.end_tournament("T0001").unwrap();
        assert!(mgr.public_summaries().is_empty());
    }
}

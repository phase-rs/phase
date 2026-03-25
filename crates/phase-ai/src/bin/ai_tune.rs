use std::collections::{HashMap, HashSet};
use std::path::PathBuf;

use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{create_config, AiConfig, AiDifficulty, AiProfile, Platform};
use phase_ai::eval::{EvalWeightSet, EvalWeights};

use engine::database::CardDatabase;
use engine::game::deck_loading::{resolve_deck_list, DeckList, DeckPayload, PlayerDeckList};
use engine::game::engine::start_game_skip_mulligan;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

// Parameter vector layout (12 parameters):
//
// Indices 0-8: EvalWeights (9 params, optimized as late-game weights)
//   0=life, 1=aggression, 2=board_presence, 3=board_power,
//   4=board_toughness, 5=hand_size, 6=zone_quality, 7=card_advantage,
//   8=synergy
//
// Indices 9-11: AiProfile (3 params)
//   9=risk_tolerance, 10=interaction_patience, 11=stabilize_bias
//
// CMA-ES optimizes the late-game weights directly. Early and mid phases
// are derived by applying the ratios from the 17Lands-trained weight set,
// since the relative importance shift across phases is well-established
// from 90M+ training samples and doesn't need evolutionary optimization.
const PARAM_COUNT: usize = 12;

/// Maximum turns before declaring a draw (prevents infinite games).
const MAX_TURNS: u32 = 100;

/// Convert a parameter vector into an AiConfig.
/// Uses Medium difficulty search settings (depth 2, 24 nodes).
/// Optimizes late-game weights directly; early/mid derived from 17Lands ratios.
/// All weight values are clamped to [0.01, 10.0] to prevent degenerate configs.
fn params_to_config(params: &[f64]) -> AiConfig {
    let clamp = |v: f64| v.clamp(0.01, 10.0);

    let late = EvalWeights {
        life: clamp(params[0]),
        aggression: clamp(params[1]),
        board_presence: clamp(params[2]),
        board_power: clamp(params[3]),
        board_toughness: clamp(params[4]),
        hand_size: clamp(params[5]),
        zone_quality: clamp(params[6]),
        card_advantage: clamp(params[7]),
        synergy: clamp(params[8]),
    };

    // Derive early/mid phases from 17Lands-learned ratios applied to CMA-ES late weights.
    // Ratios are learned_early/learned_late and learned_mid/learned_late per field.
    let learned = EvalWeightSet::learned();
    let early = scale_from_ratios(&late, &learned.early, &learned.late);
    let mid = scale_from_ratios(&late, &learned.mid, &learned.late);

    let profile = AiProfile {
        risk_tolerance: params[9].clamp(0.01, 2.0),
        interaction_patience: params[10].clamp(0.01, 2.0),
        stabilize_bias: params[11].clamp(0.01, 3.0),
    };

    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.weights = EvalWeightSet { early, mid, late };
    config.profile = profile;
    config
}

/// Scale a base weight set by the ratio between a target phase and a reference phase.
/// For each field: result = base * (target / reference), clamped to [0.01, 10.0].
fn scale_from_ratios(
    base: &EvalWeights,
    target: &EvalWeights,
    reference: &EvalWeights,
) -> EvalWeights {
    let ratio = |t: f64, r: f64| {
        if r > 0.001 {
            (t / r).clamp(0.1, 10.0)
        } else {
            1.0
        }
    };
    EvalWeights {
        life: (base.life * ratio(target.life, reference.life)).clamp(0.01, 10.0),
        aggression: (base.aggression * ratio(target.aggression, reference.aggression))
            .clamp(0.01, 10.0),
        board_presence: (base.board_presence
            * ratio(target.board_presence, reference.board_presence))
        .clamp(0.01, 10.0),
        board_power: (base.board_power * ratio(target.board_power, reference.board_power))
            .clamp(0.01, 10.0),
        board_toughness: (base.board_toughness
            * ratio(target.board_toughness, reference.board_toughness))
        .clamp(0.01, 10.0),
        hand_size: (base.hand_size * ratio(target.hand_size, reference.hand_size))
            .clamp(0.01, 10.0),
        zone_quality: (base.zone_quality * ratio(target.zone_quality, reference.zone_quality))
            .clamp(0.01, 10.0),
        card_advantage: (base.card_advantage
            * ratio(target.card_advantage, reference.card_advantage))
        .clamp(0.01, 10.0),
        synergy: (base.synergy * ratio(target.synergy, reference.synergy)).clamp(0.01, 10.0),
    }
}

/// Extract the initial parameter vector from current learned defaults.
fn initial_params() -> Vec<f64> {
    let w = EvalWeightSet::learned().late;
    let p = AiProfile::default();
    vec![
        w.life,
        w.aggression,
        w.board_presence,
        w.board_power,
        w.board_toughness,
        w.hand_size,
        w.zone_quality,
        w.card_advantage,
        w.synergy,
        p.risk_tolerance,
        p.interaction_patience,
        p.stabilize_bias,
    ]
}

/// CMA-ES (Covariance Matrix Adaptation Evolution Strategy) optimizer.
///
/// Implements the standard CMA-ES algorithm for derivative-free optimization
/// of continuous parameters. Maintains a multivariate normal distribution
/// that adapts its mean, step size, and covariance matrix based on fitness.
struct CmaEs {
    dim: usize,
    mean: Vec<f64>,
    sigma: f64,
    cov: Vec<Vec<f64>>,
    lambda: usize,
    mu: usize,
    weights_recomb: Vec<f64>,
    mu_eff: f64,
    c_sigma: f64,
    d_sigma: f64,
    c_c: f64,
    c_1: f64,
    c_mu_param: f64,
    p_sigma: Vec<f64>,
    p_c: Vec<f64>,
    generation: usize,
}

impl CmaEs {
    fn new(dim: usize, initial_mean: Vec<f64>, sigma: f64, lambda: usize) -> Self {
        assert_eq!(initial_mean.len(), dim);
        let mu = lambda / 2;

        // Log-scaled recombination weights
        let raw_weights: Vec<f64> = (0..mu)
            .map(|i| ((mu as f64 + 0.5).ln() - ((i + 1) as f64).ln()).max(0.0))
            .collect();
        let sum_w: f64 = raw_weights.iter().sum();
        let weights_recomb: Vec<f64> = raw_weights.iter().map(|w| w / sum_w).collect();

        let mu_eff: f64 = 1.0 / weights_recomb.iter().map(|w| w * w).sum::<f64>();

        // Adaptation parameters
        let c_sigma = (mu_eff + 2.0) / (dim as f64 + mu_eff + 5.0);
        let d_sigma =
            1.0 + 2.0 * (((mu_eff - 1.0) / (dim as f64 + 1.0)).sqrt() - 1.0).max(0.0) + c_sigma;
        let c_c = (4.0 + mu_eff / dim as f64) / (dim as f64 + 4.0 + 2.0 * mu_eff / dim as f64);
        let c_1 = 2.0 / ((dim as f64 + 1.3).powi(2) + mu_eff);
        let c_mu_param = (2.0 * (mu_eff - 2.0 + 1.0 / mu_eff)
            / ((dim as f64 + 2.0).powi(2) + mu_eff))
            .min(1.0 - c_1);

        // Identity covariance matrix
        let mut cov = vec![vec![0.0; dim]; dim];
        for (i, row) in cov.iter_mut().enumerate() {
            row[i] = 1.0;
        }

        CmaEs {
            dim,
            mean: initial_mean,
            sigma,
            cov,
            lambda,
            mu,
            weights_recomb,
            mu_eff,
            c_sigma,
            d_sigma,
            c_c,
            c_1,
            c_mu_param,
            p_sigma: vec![0.0; dim],
            p_c: vec![0.0; dim],
            generation: 0,
        }
    }

    /// Sample `lambda` candidate solutions from N(mean, sigma^2 * C).
    /// Uses Cholesky decomposition of the covariance matrix.
    fn sample(&self, rng: &mut impl rand::Rng) -> Vec<Vec<f64>> {
        let chol = cholesky(&self.cov);

        (0..self.lambda)
            .map(|_| {
                let z: Vec<f64> = (0..self.dim).map(|_| sample_normal(rng)).collect();
                // x = mean + sigma * L * z
                let mut x = self.mean.clone();
                for i in 0..self.dim {
                    let mut lz = 0.0;
                    for j in 0..=i {
                        lz += chol[i][j] * z[j];
                    }
                    x[i] += self.sigma * lz;
                }
                x
            })
            .collect()
    }

    /// Update the distribution after evaluating the population.
    /// `evaluated` is a slice of (candidate, fitness) pairs where higher fitness is better.
    fn step(&mut self, evaluated: &mut [(Vec<f64>, f64)]) {
        // Sort by fitness descending (higher is better)
        evaluated.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        let old_mean = self.mean.clone();

        // Compute new mean as weighted average of top mu individuals
        self.mean = vec![0.0; self.dim];
        for (i, (candidate, _)) in evaluated.iter().take(self.mu).enumerate() {
            for (mean_j, cand_j) in self.mean.iter_mut().zip(candidate.iter()) {
                *mean_j += self.weights_recomb[i] * cand_j;
            }
        }

        // Compute mean displacement
        let diff: Vec<f64> = self
            .mean
            .iter()
            .zip(&old_mean)
            .map(|(m, o)| (m - o) / self.sigma)
            .collect();

        // Inverse square root of C for isotropic path
        let inv_sqrt_c = invsqrt_cov(&self.cov);

        // Update evolution path for sigma (isotropic)
        let c_sigma_complement = (1.0 - self.c_sigma).sqrt();
        let c_sigma_scale = (self.c_sigma * (2.0 - self.c_sigma) * self.mu_eff).sqrt();
        let inv_c_diff: Vec<f64> = (0..self.dim)
            .map(|i| {
                (0..self.dim)
                    .map(|j| inv_sqrt_c[i][j] * diff[j])
                    .sum::<f64>()
            })
            .collect();

        for (ps, icd) in self.p_sigma.iter_mut().zip(inv_c_diff.iter()) {
            *ps = c_sigma_complement * *ps + c_sigma_scale * icd;
        }

        // Expected length of N(0,I) vector
        let chi_n = (self.dim as f64).sqrt()
            * (1.0 - 1.0 / (4.0 * self.dim as f64) + 1.0 / (21.0 * (self.dim as f64).powi(2)));

        let p_sigma_norm: f64 = self.p_sigma.iter().map(|v| v * v).sum::<f64>().sqrt();

        // Heaviside function for p_c update
        let h_sigma = if p_sigma_norm
            / (1.0 - (1.0 - self.c_sigma).powi(2 * (self.generation as i32 + 1))).sqrt()
            < (1.4 + 2.0 / (self.dim as f64 + 1.0)) * chi_n
        {
            1.0
        } else {
            0.0
        };

        // Update evolution path for covariance
        let c_c_complement = (1.0 - self.c_c).sqrt();
        let c_c_scale = h_sigma * (self.c_c * (2.0 - self.c_c) * self.mu_eff).sqrt();
        for (pc, d) in self.p_c.iter_mut().zip(diff.iter()) {
            *pc = c_c_complement * *pc + c_c_scale * d;
        }

        // Update covariance matrix
        let delta_h = (1.0 - h_sigma) * self.c_c * (2.0 - self.c_c);
        let c_old_scale = 1.0 + self.c_1 * delta_h - self.c_1 - self.c_mu_param;

        for i in 0..self.dim {
            for j in 0..=i {
                // Rank-one update
                let rank_one = self.c_1 * self.p_c[i] * self.p_c[j];

                // Rank-mu update
                let mut rank_mu = 0.0;
                for (k, (candidate, _)) in evaluated.iter().take(self.mu).enumerate() {
                    let yi = (candidate[i] - old_mean[i]) / self.sigma;
                    let yj = (candidate[j] - old_mean[j]) / self.sigma;
                    rank_mu += self.weights_recomb[k] * yi * yj;
                }

                self.cov[i][j] =
                    c_old_scale.max(0.0) * self.cov[i][j] + rank_one + self.c_mu_param * rank_mu;
                self.cov[j][i] = self.cov[i][j];
            }
        }

        // Update step size
        self.sigma *= ((self.c_sigma / self.d_sigma) * (p_sigma_norm / chi_n - 1.0)).exp();

        self.generation += 1;
    }

    fn best_mean(&self) -> &[f64] {
        &self.mean
    }

    fn current_sigma(&self) -> f64 {
        self.sigma
    }
}

/// Sample from a standard normal distribution using the Box-Muller transform.
fn sample_normal(rng: &mut impl rand::Rng) -> f64 {
    let u1: f64 = rng.random::<f64>().max(f64::MIN_POSITIVE);
    let u2: f64 = rng.random::<f64>();
    (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos()
}

/// Cholesky decomposition of a symmetric positive-definite matrix.
/// Returns lower triangular matrix L such that A = L * L^T.
fn cholesky(a: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let n = a.len();
    let mut l = vec![vec![0.0; n]; n];
    for i in 0..n {
        for j in 0..=i {
            let sum: f64 = l[i][..j]
                .iter()
                .zip(l[j][..j].iter())
                .map(|(a, b)| a * b)
                .sum();
            if i == j {
                // Add small epsilon for numerical stability
                l[i][j] = (a[i][i] - sum).max(1e-12).sqrt();
            } else {
                l[i][j] = (a[i][j] - sum) / l[j][j].max(1e-12);
            }
        }
    }
    l
}

/// Compute the inverse square root of a covariance matrix via eigendecomposition.
/// For small dimensions (<=12), this is adequate.
fn invsqrt_cov(cov: &[Vec<f64>]) -> Vec<Vec<f64>> {
    let n = cov.len();
    // For the CMA-ES with small dimensions, approximate with Cholesky inverse
    let l = cholesky(cov);
    // Invert lower triangular L
    let mut l_inv = vec![vec![0.0; n]; n];
    for i in 0..n {
        l_inv[i][i] = 1.0 / l[i][i].max(1e-12);
        for j in (0..i).rev() {
            let mut sum = 0.0;
            for k in (j + 1)..=i {
                sum += l[i][k] * l_inv[k][j];
            }
            l_inv[i][j] = -sum / l[i][i].max(1e-12);
        }
    }
    // C^{-1/2} ≈ L^{-T} (the inverse sqrt approximation via Cholesky)
    // Actually C^{-1} = L^{-T} L^{-1}, and C^{-1/2} = L^{-1}
    // Since C = L L^T, C^{1/2} = L, C^{-1/2} = L^{-1}
    l_inv
}

/// A deck matchup: two named deck lists to pit against each other.
struct Matchup {
    name: &'static str,
    deck_a: PlayerDeckList,
    deck_b: PlayerDeckList,
    /// Expected winner based on MTG archetype triangle (aggro < midrange < control < aggro).
    /// None = mirror/even matchup, Some(0) = deck_a favored, Some(1) = deck_b favored.
    expected_winner: Option<u8>,
}

/// Build the 3 deck matchups for fitness evaluation.
fn build_matchups() -> Vec<Matchup> {
    let red_aggro = PlayerDeckList {
        main_deck: [
            vec!["Mountain".to_string(); 24],
            vec!["Lightning Bolt".to_string(); 4],
            vec!["Shock".to_string(); 4],
            vec!["Monastery Swiftspear".to_string(); 4],
            vec!["Goblin Guide".to_string(); 4],
            vec!["Zurgo Bellstriker".to_string(); 4],
            vec!["Bomat Courier".to_string(); 4],
            vec!["Fanatical Firebrand".to_string(); 4],
            vec!["Ghitu Lavarunner".to_string(); 4],
            vec!["Viashino Pyromancer".to_string(); 4],
        ]
        .concat(),
        sideboard: vec![],
        commander: vec![],
    };

    let green_midrange = PlayerDeckList {
        main_deck: [
            vec!["Forest".to_string(); 24],
            vec!["Llanowar Elves".to_string(); 4],
            vec!["Elvish Mystic".to_string(); 4],
            vec!["Grizzly Bears".to_string(); 4],
            vec!["Leatherback Baloth".to_string(); 4],
            vec!["Gigantosaurus".to_string(); 4],
            vec!["Garruk's Companion".to_string(); 4],
            vec!["Kalonian Tusker".to_string(); 4],
            vec!["Rampant Growth".to_string(); 4],
            vec!["Giant Growth".to_string(); 4],
            vec!["Blossoming Defense".to_string(); 4],
        ]
        .concat(),
        sideboard: vec![],
        commander: vec![],
    };

    let white_weenie = PlayerDeckList {
        main_deck: [
            vec!["Plains".to_string(); 22],
            vec!["Savannah Lions".to_string(); 4],
            vec!["Elite Vanguard".to_string(); 4],
            vec!["Soldier of the Pantheon".to_string(); 4],
            vec!["Imposing Sovereign".to_string(); 4],
            vec!["Precinct Captain".to_string(); 4],
            vec!["Raise the Alarm".to_string(); 4],
            vec!["Honor of the Pure".to_string(); 4],
            vec!["Brave the Elements".to_string(); 4],
            vec!["Condemn".to_string(); 4],
            vec!["Banishing Light".to_string(); 2],
        ]
        .concat(),
        sideboard: vec![],
        commander: vec![],
    };

    let blue_control = PlayerDeckList {
        main_deck: [
            vec!["Island".to_string(); 26],
            vec!["Counterspell".to_string(); 4],
            vec!["Mana Leak".to_string(); 4],
            vec!["Opt".to_string(); 4],
            vec!["Preordain".to_string(); 4],
            vec!["Delver of Secrets".to_string(); 4],
            vec!["Augur of Bolas".to_string(); 4],
            vec!["Man-o'-War".to_string(); 4],
            vec!["Essence Scatter".to_string(); 4],
            vec!["Unsummon".to_string(); 2],
        ]
        .concat(),
        sideboard: vec![],
        commander: vec![],
    };

    vec![
        Matchup {
            name: "Red Aggro vs Green Midrange",
            deck_a: red_aggro.clone(),
            deck_b: green_midrange.clone(),
            expected_winner: Some(1), // midrange > aggro
        },
        Matchup {
            name: "White Weenie vs Red Aggro",
            deck_a: white_weenie,
            deck_b: red_aggro,
            expected_winner: None, // aggro mirror, roughly even
        },
        Matchup {
            name: "Green Midrange vs Blue Control",
            deck_a: green_midrange,
            deck_b: blue_control,
            expected_winner: Some(1), // control > midrange
        },
    ]
}

/// Run a single game with separate AI configs for each player.
/// Returns the winner (if any) and the turn count.
fn run_game(
    payload: &DeckPayload,
    seed: u64,
    config_p0: &AiConfig,
    config_p1: &AiConfig,
) -> (Option<PlayerId>, u32) {
    let mut state = GameState::new_two_player(seed);
    engine::game::deck_loading::load_deck_into_state(&mut state, payload);

    // Start game, skip mulligan for speed
    let _ = start_game_skip_mulligan(&mut state);

    let ai_players: HashSet<PlayerId> = [PlayerId(0), PlayerId(1)].into_iter().collect();
    let mut ai_configs: HashMap<PlayerId, AiConfig> = HashMap::new();
    ai_configs.insert(PlayerId(0), config_p0.clone());
    ai_configs.insert(PlayerId(1), config_p1.clone());

    let mut turns = 0u32;
    loop {
        if let WaitingFor::GameOver { winner } = &state.waiting_for {
            return (*winner, turns);
        }
        if turns >= MAX_TURNS {
            return (None, turns);
        }

        let results = run_ai_actions(&mut state, &ai_players, &ai_configs);
        if results.is_empty() {
            // No actions could be taken — game is stuck
            return (None, turns);
        }

        // Track turns by monitoring turn changes
        if let Some(actor) = state.waiting_for.acting_player() {
            if actor == PlayerId(0) {
                turns += 1;
            }
        }
    }
}

/// Evaluate fitness of a parameter vector by playing games across matchups.
/// Returns the average win rate of the candidate config vs the baseline.
fn evaluate_fitness(
    params: &[f64],
    matchups: &[(DeckPayload, &str)],
    games_per_matchup: usize,
    base_seed: u64,
    baseline: &AiConfig,
) -> f64 {
    let candidate = params_to_config(params);
    let mut total_wins = 0usize;
    let mut total_games = 0usize;

    for (matchup_idx, (payload, _name)) in matchups.iter().enumerate() {
        for game_idx in 0..games_per_matchup {
            let seed = base_seed + (matchup_idx * games_per_matchup + game_idx) as u64;

            // Alternate sides: even games candidate=P0, odd games candidate=P1
            let (config_p0, config_p1) = if game_idx % 2 == 0 {
                (&candidate, baseline)
            } else {
                (baseline, &candidate)
            };

            let (winner, _turns) = run_game(payload, seed, config_p0, config_p1);

            let candidate_won = match winner {
                Some(PlayerId(0)) => game_idx % 2 == 0,
                Some(PlayerId(1)) => game_idx % 2 == 1,
                _ => false,
            };

            if candidate_won {
                total_wins += 1;
            }
            total_games += 1;
        }
    }

    total_wins as f64 / total_games.max(1) as f64
}

fn print_usage() {
    eprintln!("Usage: ai-tune <data-root> [OPTIONS]");
    eprintln!();
    eprintln!("Options:");
    eprintln!("  --generations N   CMA-ES generations (default: 100)");
    eprintln!("  --population N    Population size (default: 50)");
    eprintln!("  --games N         Games per matchup per fitness eval (default: 20)");
    eprintln!("  --seed S          RNG seed (default: time-based)");
    eprintln!("  --output PATH     Output JSON path (default: <data-root>/learned-weights.json)");
    eprintln!("  --validate        Run baseline vs learned comparison (no CMA-ES)");
}

fn main() {
    let args: Vec<String> = std::env::args().collect();

    if args.len() < 2 || args[1] == "--help" || args[1] == "-h" {
        print_usage();
        std::process::exit(if args.len() < 2 { 1 } else { 0 });
    }

    let data_root = PathBuf::from(&args[1]);

    // Parse CLI options
    let mut generations = 100usize;
    let mut population = 50usize;
    let mut games = 20usize;
    let mut seed: Option<u64> = None;
    let mut output: Option<PathBuf> = None;
    let mut validate = false;

    let mut i = 2;
    while i < args.len() {
        match args[i].as_str() {
            "--generations" => {
                i += 1;
                generations = args[i].parse().expect("invalid --generations");
            }
            "--population" => {
                i += 1;
                population = args[i].parse().expect("invalid --population");
            }
            "--games" => {
                i += 1;
                games = args[i].parse().expect("invalid --games");
            }
            "--seed" => {
                i += 1;
                seed = Some(args[i].parse().expect("invalid --seed"));
            }
            "--output" => {
                i += 1;
                output = Some(PathBuf::from(&args[i]));
            }
            "--validate" => {
                validate = true;
            }
            other => {
                eprintln!("Unknown option: {other}");
                print_usage();
                std::process::exit(1);
            }
        }
        i += 1;
    }

    let output_path = output.unwrap_or_else(|| data_root.join("learned-weights.json"));

    // Load card database
    let card_data_path = data_root.join("card-data.json");
    let alt_path = PathBuf::from("client/public/card-data.json");
    let db_path = if card_data_path.exists() {
        card_data_path
    } else if alt_path.exists() {
        alt_path
    } else {
        eprintln!(
            "Error: card-data.json not found at {:?} or {:?}",
            card_data_path, alt_path
        );
        std::process::exit(1);
    };

    let db = CardDatabase::from_export(&db_path).unwrap_or_else(|e| {
        eprintln!("Error loading card database: {e}");
        std::process::exit(1);
    });

    // Build matchups
    let matchup_defs = build_matchups();
    let resolved: Vec<(DeckPayload, &Matchup)> = matchup_defs
        .iter()
        .map(|m| {
            let deck_list = DeckList {
                player: m.deck_a.clone(),
                opponent: m.deck_b.clone(),
                ai_decks: vec![],
            };
            (resolve_deck_list(&db, &deck_list), m)
        })
        .collect();

    let base_seed = seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });

    if validate {
        run_validate(&resolved, games, base_seed, &output_path);
    } else {
        let fitness_matchups: Vec<(DeckPayload, &str)> =
            resolved.iter().map(|(p, m)| (p.clone(), m.name)).collect();
        run_cmaes(
            &fitness_matchups,
            generations,
            population,
            games,
            base_seed,
            &output_path,
        );
    }
}

/// Validates learned weights by measuring matchup correctness.
///
/// For each matchup, runs self-play (both players use the same weights) and checks
/// whether the expected-favored deck wins more. Based on the MTG archetype triangle:
/// aggro < midrange < control < aggro.
///
/// A weight set that produces correct matchup polarities is better than one that
/// doesn't — regardless of raw win rate against a baseline.
fn run_validate(
    matchups: &[(DeckPayload, &Matchup)],
    games: usize,
    base_seed: u64,
    output_path: &std::path::Path,
) {
    let games = if games == 20 { 500 } else { games }; // Default to 500 for validate
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;

    eprintln!("=== Matchup Correctness Validation ===");
    eprintln!("Games per matchup: {games}");
    eprintln!(
        "Metric: does the expected-favored deck win more? (aggro < midrange < control < aggro)"
    );

    let baseline_config = create_config(AiDifficulty::Medium, Platform::Native);
    // "Learned" uses same defaults — in the future, load from a tuned weights file
    let learned_config = create_config(AiDifficulty::Medium, Platform::Native);

    let mut matchup_results = Vec::new();
    let mut baseline_correct = 0;
    let mut learned_correct = 0;
    let mut polarized_count = 0;

    for (payload, matchup) in matchups {
        let name = matchup.name;
        let expected = matchup.expected_winner;
        eprintln!("\nMatchup: {name}");
        if let Some(e) = expected {
            eprintln!(
                "  Expected winner: deck_{} ({})",
                if e == 0 { "A" } else { "B" },
                if e == 0 {
                    name.split(" vs ").next().unwrap()
                } else {
                    name.split(" vs ").nth(1).unwrap()
                }
            );
        } else {
            eprintln!("  Expected: even matchup");
        }

        // Self-play with baseline weights (both players use default weights)
        let mut baseline_p0_wins = 0usize;
        for g in 0..games {
            let seed = base_seed + g as u64;
            let (winner, _) = run_game(payload, seed, &baseline_config, &baseline_config);
            if winner == Some(PlayerId(0)) {
                baseline_p0_wins += 1;
            }
        }
        let baseline_p0_wr = baseline_p0_wins as f64 / games as f64;

        // Self-play with learned weights (both players use learned weights)
        let mut learned_p0_wins = 0usize;
        for g in 0..games {
            let seed = base_seed + 10000 + g as u64;
            let (winner, _) = run_game(payload, seed, &learned_config, &learned_config);
            if winner == Some(PlayerId(0)) {
                learned_p0_wins += 1;
            }
        }
        let learned_p0_wr = learned_p0_wins as f64 / games as f64;

        // Check matchup polarity correctness
        let baseline_polarity_correct = match expected {
            Some(0) => baseline_p0_wr > 0.5, // deck_a should win
            Some(1) => baseline_p0_wr < 0.5, // deck_b should win
            _ => true,                       // even matchup, any result is "correct"
        };
        let learned_polarity_correct = match expected {
            Some(0) => learned_p0_wr > 0.5,
            Some(1) => learned_p0_wr < 0.5,
            _ => true,
        };

        // Measure how close to expected polarity (further from 50% in correct direction = better)
        let polarity_strength = |p0_wr: f64, expected: Option<u8>| -> f64 {
            match expected {
                Some(0) => p0_wr - 0.5,    // positive = correct (deck_a wins more)
                Some(1) => 0.5 - p0_wr,    // positive = correct (deck_b wins more)
                _ => -(0.5 - p0_wr).abs(), // closer to 50% = better for even matchups
            }
        };

        let baseline_strength = polarity_strength(baseline_p0_wr, expected);
        let learned_strength = polarity_strength(learned_p0_wr, expected);

        if expected.is_some() {
            polarized_count += 1;
            if baseline_polarity_correct {
                baseline_correct += 1;
            }
            if learned_polarity_correct {
                learned_correct += 1;
            }
        }

        eprintln!(
            "  Baseline self-play: deck_A wins {:.1}%{}",
            baseline_p0_wr * 100.0,
            if baseline_polarity_correct {
                " ✓"
            } else {
                " ✗ WRONG POLARITY"
            }
        );
        eprintln!(
            "  Learned self-play:  deck_A wins {:.1}%{}",
            learned_p0_wr * 100.0,
            if learned_polarity_correct {
                " ✓"
            } else {
                " ✗ WRONG POLARITY"
            }
        );
        eprintln!(
            "  Polarity strength:  baseline={:+.3} learned={:+.3}{}",
            baseline_strength,
            learned_strength,
            if learned_strength > baseline_strength {
                " (improved)"
            } else {
                ""
            }
        );

        matchup_results.push(serde_json::json!({
            "name": name,
            "games": games,
            "expected_winner": match expected { Some(0) => "deck_a", Some(1) => "deck_b", _ => "even" },
            "baseline_p0_wr": r3(baseline_p0_wr),
            "learned_p0_wr": r3(learned_p0_wr),
            "baseline_polarity_correct": baseline_polarity_correct,
            "learned_polarity_correct": learned_polarity_correct,
            "baseline_polarity_strength": r3(baseline_strength),
            "learned_polarity_strength": r3(learned_strength),
        }));
    }

    let improvement = learned_correct >= baseline_correct
        && matchup_results
            .iter()
            .filter(|r| {
                r["learned_polarity_strength"].as_f64().unwrap()
                    > r["baseline_polarity_strength"].as_f64().unwrap()
            })
            .count()
            >= matchup_results.len() / 2;

    eprintln!("\n=== Summary ===");
    eprintln!(
        "Polarized matchups correct: baseline={}/{} learned={}/{}",
        baseline_correct, polarized_count, learned_correct, polarized_count
    );
    eprintln!("Matchup correctness improved: {improvement}");

    let result = serde_json::json!({
        "mode": "validate",
        "metric": "matchup_correctness",
        "matchups": matchup_results,
        "baseline_correct": baseline_correct,
        "learned_correct": learned_correct,
        "polarized_matchups": polarized_count,
        "improvement_detected": improvement,
    });

    let json = serde_json::to_string_pretty(&result).unwrap();
    std::fs::write(output_path, &json).unwrap();
    eprintln!("\nResults written to {}", output_path.display());
    println!("{json}");
}

fn run_cmaes(
    matchups: &[(DeckPayload, &str)],
    generations: usize,
    population: usize,
    games: usize,
    base_seed: u64,
    output_path: &std::path::Path,
) {
    eprintln!("=== CMA-ES AI Weight Tuning ===");
    eprintln!(
        "Parameters: {PARAM_COUNT}, Generations: {generations}, Population: {population}, Games/eval: {games}"
    );

    let baseline = create_config(AiDifficulty::Medium, Platform::Native);
    let initial = initial_params();

    let mut cma = CmaEs::new(PARAM_COUNT, initial, 0.3, population);
    let mut rng = if base_seed != 0 {
        <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(base_seed)
    } else {
        <rand::rngs::StdRng as rand::SeedableRng>::from_os_rng()
    };

    let mut best_fitness = 0.0f64;

    for gen in 0..generations {
        let candidates = cma.sample(&mut rng);

        // Evaluate population (parallel if rayon is available)
        let gen_seed = base_seed.wrapping_add((gen as u64) * 10000);

        #[cfg(feature = "tune")]
        let fitnesses: Vec<f64> = {
            use rayon::prelude::*;
            candidates
                .par_iter()
                .enumerate()
                .map(|(i, params)| {
                    evaluate_fitness(
                        params,
                        matchups,
                        games,
                        gen_seed.wrapping_add(i as u64 * 1000),
                        &baseline,
                    )
                })
                .collect()
        };

        #[cfg(not(feature = "tune"))]
        let fitnesses: Vec<f64> = candidates
            .iter()
            .enumerate()
            .map(|(i, params)| {
                evaluate_fitness(
                    params,
                    matchups,
                    games,
                    gen_seed.wrapping_add(i as u64 * 1000),
                    &baseline,
                )
            })
            .collect();

        let mut evaluated: Vec<(Vec<f64>, f64)> = candidates
            .into_iter()
            .zip(fitnesses.iter().copied())
            .collect();

        let gen_best = fitnesses.iter().copied().fold(f64::NEG_INFINITY, f64::max);
        let gen_mean = fitnesses.iter().sum::<f64>() / fitnesses.len() as f64;

        if gen_best > best_fitness {
            best_fitness = gen_best;
        }

        eprintln!(
            "Gen {}/{}: best={:.3} mean={:.3} sigma={:.4}",
            gen + 1,
            generations,
            gen_best,
            gen_mean,
            cma.current_sigma()
        );

        cma.step(&mut evaluated);
    }

    // Use the CMA-ES mean as the final result (more stable than best individual)
    let final_params = cma.best_mean();
    let final_config = params_to_config(final_params);

    let w = &final_config.weights.late;
    let p = &final_config.profile;

    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    let result = serde_json::json!({
        "source": "cma-es-self-play",
        "generations": generations,
        "population": population,
        "games_per_eval": games,
        "best_fitness": r3(best_fitness),
        "weights": {
            "life": r3(w.life),
            "aggression": r3(w.aggression),
            "board_presence": r3(w.board_presence),
            "board_power": r3(w.board_power),
            "board_toughness": r3(w.board_toughness),
            "hand_size": r3(w.hand_size),
            "zone_quality": r3(w.zone_quality),
            "card_advantage": r3(w.card_advantage),
            "synergy": r3(w.synergy),
        },
        "profile": {
            "risk_tolerance": (p.risk_tolerance * 1000.0).round() / 1000.0,
            "interaction_patience": (p.interaction_patience * 1000.0).round() / 1000.0,
            "stabilize_bias": (p.stabilize_bias * 1000.0).round() / 1000.0,
        },
        "future_parameters": {
            "note": "Archetype multipliers and keyword bonuses require adjust_weights()/evaluate_creature() refactoring to accept parameterized values",
            "keyword_bonuses": {
                "flying_mult": 1.0, "trample_mult": 0.5, "deathtouch_flat": 3.0,
                "lifelink_mult": 0.5, "hexproof_flat": 2.0, "indestructible_flat": 4.0,
                "first_strike_mult": 0.8, "vigilance_flat": 1.0, "menace_mult": 0.5,
                "tapped_penalty": 1.5,
            },
        },
    });

    let json = serde_json::to_string_pretty(&result).unwrap();
    std::fs::write(output_path, &json).unwrap();
    eprintln!("\nOptimized weights written to {}", output_path.display());
    println!("{json}");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cma_es_mean_moves_toward_better_fitness() {
        // Create CMA-ES with mean at origin, dim=3
        let mut cma = CmaEs::new(3, vec![0.0, 0.0, 0.0], 1.0, 10);

        // Provide synthetic fitnesses where candidates near [1, 1, 1] score highest
        let mut evaluated: Vec<(Vec<f64>, f64)> = (0..10)
            .map(|i| {
                let x = vec![i as f64 * 0.2, i as f64 * 0.2, i as f64 * 0.2];
                // Fitness = negative distance from [1, 1, 1]
                let dist: f64 = x.iter().map(|v| (v - 1.0).powi(2)).sum::<f64>().sqrt();
                (x, 1.0 / (1.0 + dist))
            })
            .collect();

        cma.step(&mut evaluated);

        // After one step, mean should have moved toward [1, 1, 1]
        let mean = cma.best_mean();
        assert!(
            mean[0] > 0.0 && mean[1] > 0.0 && mean[2] > 0.0,
            "Mean should move toward positive direction: {:?}",
            mean
        );
    }

    #[test]
    fn params_to_config_clamps_values() {
        let params = vec![
            -5.0, 100.0, 0.0, 1.0, 2.0, 3.0, 0.5, 1.5, 0.8, -1.0, 5.0, 0.005,
        ];
        let config = params_to_config(&params);

        assert!(config.weights.late.life >= 0.01);
        assert!(config.weights.late.aggression <= 10.0);
        assert!(config.profile.risk_tolerance >= 0.01);
        assert!(config.profile.interaction_patience <= 2.0);
    }

    #[test]
    fn initial_params_has_correct_length() {
        let params = initial_params();
        assert_eq!(params.len(), PARAM_COUNT);
    }

    #[test]
    fn cholesky_identity() {
        let identity = vec![vec![1.0, 0.0], vec![0.0, 1.0]];
        let l = cholesky(&identity);
        assert!((l[0][0] - 1.0).abs() < 1e-10);
        assert!((l[1][1] - 1.0).abs() < 1e-10);
        assert!((l[1][0]).abs() < 1e-10);
    }
}

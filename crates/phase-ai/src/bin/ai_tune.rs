use std::collections::{HashMap, HashSet};
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::path::PathBuf;
use std::process::Command;

use phase_ai::auto_play::run_ai_actions;
use phase_ai::config::{
    create_config, AiConfig, AiDifficulty, AiProfile, Platform, PolicyPenalties,
    ACTIVE_POLICY_PENALTY_FIELDS,
};
use phase_ai::deck_profile::ArchetypeMultipliers;
use phase_ai::duel_suite::compare::sign_test_mid_p_upper_tail;
use phase_ai::duel_suite::{all_matchups, resolve_deck_ref, MatchupSpec};
use phase_ai::eval::{EvalWeightSet, EvalWeights, KeywordBonuses};

use engine::database::CardDatabase;
use engine::game::deck_loading::{resolve_deck_list, DeckList, DeckPayload, PlayerDeckList};
use engine::game::engine::start_game_skip_mulligan;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

const CMA_TUNED_KIND: &str = "cma_tuned_weights";
const FITNESS_MATCHUP_IDS: &[&str] = &["red-vs-green", "white-vs-red", "red-vs-blue"];
const HOLDOUT_MATCHUP_IDS: &[&str] = &["black-vs-blue", "azorius-vs-prowess", "delver-vs-green"];
const EVAL_PARAMETER_NAMES: &[&str] = &[
    "late.life",
    "late.aggression",
    "late.board_presence",
    "late.board_power",
    "late.board_toughness",
    "late.hand_size",
    "late.zone_quality",
    "late.card_advantage",
    "late.synergy",
    "profile.risk_tolerance",
    "profile.interaction_patience",
    "profile.stabilize_bias",
];
const KEYWORD_PARAMETER_NAMES: &[&str] = &[
    "keyword.flying_mult",
    "keyword.trample_mult",
    "keyword.deathtouch_flat",
    "keyword.lifelink_mult",
    "keyword.hexproof_flat",
    "keyword.indestructible_flat",
    "keyword.first_strike_mult",
    "keyword.vigilance_flat",
    "keyword.menace_mult",
    "keyword.tapped_penalty",
];
const ARCHETYPE_NAMES: &[&str] = &["aggro", "midrange", "control", "combo", "ramp"];
const ARCHETYPE_WEIGHT_NAMES: &[&str] = &[
    "life",
    "aggression",
    "board_presence",
    "board_power",
    "board_toughness",
    "hand_size",
    "zone_quality",
    "card_advantage",
    "synergy",
];

/// Maximum turns before declaring a draw (prevents infinite games).
const MAX_TURNS: u32 = 100;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TuneGroup {
    Eval,
    Penalties,
    Keywords,
    Archetype,
}

impl TuneGroup {
    fn from_label(label: &str) -> Option<Self> {
        match label {
            "eval" => Some(Self::Eval),
            "penalties" => Some(Self::Penalties),
            "keywords" => Some(Self::Keywords),
            "archetype" => Some(Self::Archetype),
            _ => None,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Eval => "eval",
            Self::Penalties => "penalties",
            Self::Keywords => "keywords",
            Self::Archetype => "archetype",
        }
    }

    fn parameter_names(self) -> Vec<String> {
        match self {
            Self::Eval => EVAL_PARAMETER_NAMES
                .iter()
                .map(|name| name.to_string())
                .collect(),
            Self::Penalties => ACTIVE_POLICY_PENALTY_FIELDS
                .iter()
                .map(|name| format!("policy_penalties.{name}"))
                .collect(),
            Self::Keywords => KEYWORD_PARAMETER_NAMES
                .iter()
                .map(|name| name.to_string())
                .collect(),
            Self::Archetype => ARCHETYPE_NAMES
                .iter()
                .flat_map(|archetype| {
                    ARCHETYPE_WEIGHT_NAMES
                        .iter()
                        .map(move |weight| format!("archetype.{archetype}.{weight}"))
                })
                .collect(),
        }
    }
}

/// Convert the legacy eval/profile parameter vector into an AiConfig.
/// Uses Medium difficulty search settings (depth 2, 24 nodes).
/// Optimizes late-game weights directly; early/mid derived from 17Lands ratios.
/// All weight values are clamped to [0.01, 10.0] to prevent degenerate configs.
#[cfg(test)]
fn params_to_config(params: &[f64]) -> AiConfig {
    params_to_config_for(TuneGroup::Eval, params)
}

fn params_to_config_for(group: TuneGroup, params: &[f64]) -> AiConfig {
    match group {
        TuneGroup::Eval => eval_params_to_config(params),
        TuneGroup::Penalties => {
            let mut config = create_config(AiDifficulty::Medium, Platform::Native);
            config.policy_penalties = policy_penalties_from_params(params);
            config
        }
        TuneGroup::Keywords => {
            let mut config = create_config(AiDifficulty::Medium, Platform::Native);
            config.keyword_bonuses = keyword_bonuses_from_params(params);
            config
        }
        TuneGroup::Archetype => {
            let mut config = create_config(AiDifficulty::Medium, Platform::Native);
            config.archetype_multipliers = archetype_multipliers_from_params(params);
            config
        }
    }
}

fn eval_params_to_config(params: &[f64]) -> AiConfig {
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

fn numeric_fields_to_params<T: serde::Serialize>(value: &T, fields: &[&str]) -> Vec<f64> {
    let value = serde_json::to_value(value).expect("tuning config serializes");
    let object = value
        .as_object()
        .expect("tuning config serializes as object");
    fields
        .iter()
        .map(|field| {
            object
                .get(*field)
                .and_then(|v| v.as_f64())
                .unwrap_or_else(|| panic!("missing numeric tuning field {field}"))
        })
        .collect()
}

fn deserialize_numeric_fields<T>(fields: &[&str], params: &[f64], min: f64, max: f64) -> T
where
    T: serde::Serialize + serde::de::DeserializeOwned + Default,
{
    assert_eq!(fields.len(), params.len());
    let mut value = serde_json::to_value(T::default()).expect("tuning config serializes");
    let object = value
        .as_object_mut()
        .expect("tuning config serializes as object");
    for (field, param) in fields.iter().zip(params.iter()) {
        object.insert(
            (*field).to_string(),
            serde_json::Number::from_f64(param.clamp(min, max))
                .map(serde_json::Value::Number)
                .expect("finite tuning parameter"),
        );
    }
    serde_json::from_value(value).expect("tuning config deserializes")
}

fn policy_penalties_from_params(params: &[f64]) -> PolicyPenalties {
    deserialize_numeric_fields(ACTIVE_POLICY_PENALTY_FIELDS, params, -15.0, 15.0)
}

fn keyword_bonuses_from_params(params: &[f64]) -> KeywordBonuses {
    let fields: Vec<&str> = KEYWORD_PARAMETER_NAMES
        .iter()
        .map(|name| name.strip_prefix("keyword.").unwrap_or(name))
        .collect();
    deserialize_numeric_fields(&fields, params, -10.0, 15.0)
}

fn archetype_multipliers_from_params(params: &[f64]) -> ArchetypeMultipliers {
    assert_eq!(
        params.len(),
        ARCHETYPE_NAMES.len() * ARCHETYPE_WEIGHT_NAMES.len()
    );
    let mut multipliers = ArchetypeMultipliers::default();
    for (archetype_idx, chunk) in params.chunks(ARCHETYPE_WEIGHT_NAMES.len()).enumerate() {
        let values: [f64; 9] = chunk
            .iter()
            .map(|v| v.clamp(0.01, 5.0))
            .collect::<Vec<_>>()
            .try_into()
            .expect("archetype group uses 9 weights per archetype");
        match ARCHETYPE_NAMES[archetype_idx] {
            "aggro" => multipliers.aggro = values,
            "midrange" => multipliers.midrange = values,
            "control" => multipliers.control = values,
            "combo" => multipliers.combo = values,
            "ramp" => multipliers.ramp = values,
            _ => unreachable!("unknown archetype tuning group"),
        }
    }
    multipliers
}

fn config_from_late_weights_and_profile(late: EvalWeights, profile: AiProfile) -> AiConfig {
    let learned = EvalWeightSet::learned();
    let early = scale_from_ratios(&late, &learned.early, &learned.late);
    let mid = scale_from_ratios(&late, &learned.mid, &learned.late);

    let mut config = create_config(AiDifficulty::Medium, Platform::Native);
    config.weights = EvalWeightSet { early, mid, late };
    config.profile = profile;
    config
}

fn load_cma_tuned_config(path: &std::path::Path) -> Result<AiConfig, String> {
    let text = std::fs::read_to_string(path)
        .map_err(|err| format!("failed to read tuned artifact {}: {err}", path.display()))?;
    let value: serde_json::Value = serde_json::from_str(&text)
        .map_err(|err| format!("failed to parse tuned artifact {}: {err}", path.display()))?;
    let kind = value
        .get("kind")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "tuned artifact missing kind".to_string())?;
    if kind != CMA_TUNED_KIND {
        return Err(format!(
            "expected artifact kind {CMA_TUNED_KIND}, found {kind}"
        ));
    }
    let group = value
        .get("group")
        .and_then(|v| v.as_str())
        .and_then(TuneGroup::from_label)
        .unwrap_or(TuneGroup::Eval);
    let mut config = create_config(AiDifficulty::Medium, Platform::Native);

    if group == TuneGroup::Penalties {
        let penalties = value
            .get("policy_penalties")
            .ok_or_else(|| "tuned penalties artifact missing policy_penalties".to_string())?;
        config.policy_penalties = serde_json::from_value(penalties.clone())
            .map_err(|err| format!("invalid policy_penalties section: {err}"))?;
        return Ok(config);
    }

    if group == TuneGroup::Keywords {
        let bonuses = value
            .get("keyword_bonuses")
            .ok_or_else(|| "tuned keyword artifact missing keyword_bonuses".to_string())?;
        config.keyword_bonuses = serde_json::from_value(bonuses.clone())
            .map_err(|err| format!("invalid keyword_bonuses section: {err}"))?;
        return Ok(config);
    }

    if group == TuneGroup::Archetype {
        let multipliers = value
            .get("archetype_multipliers")
            .ok_or_else(|| "tuned archetype artifact missing archetype_multipliers".to_string())?;
        config.archetype_multipliers = serde_json::from_value(multipliers.clone())
            .map_err(|err| format!("invalid archetype_multipliers section: {err}"))?;
        return Ok(config);
    }

    let weights = value
        .get("weights")
        .ok_or_else(|| "tuned artifact missing weights".to_string())?;
    let profile = value
        .get("profile")
        .ok_or_else(|| "tuned artifact missing profile".to_string())?;

    let field = |object: &serde_json::Value, name: &str| -> Result<f64, String> {
        object
            .get(name)
            .and_then(|v| v.as_f64())
            .ok_or_else(|| format!("tuned artifact missing numeric field {name}"))
    };

    let late = EvalWeights {
        life: field(weights, "life")?,
        aggression: field(weights, "aggression")?,
        board_presence: field(weights, "board_presence")?,
        board_power: field(weights, "board_power")?,
        board_toughness: field(weights, "board_toughness")?,
        hand_size: field(weights, "hand_size")?,
        zone_quality: field(weights, "zone_quality")?,
        card_advantage: field(weights, "card_advantage")?,
        synergy: field(weights, "synergy")?,
    };
    let profile = AiProfile {
        risk_tolerance: field(profile, "risk_tolerance")?,
        interaction_patience: field(profile, "interaction_patience")?,
        stabilize_bias: field(profile, "stabilize_bias")?,
    };

    Ok(config_from_late_weights_and_profile(late, profile))
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

/// Extract the initial parameter vector from current defaults.
#[cfg(test)]
fn initial_params() -> Vec<f64> {
    initial_params_for(TuneGroup::Eval)
}

fn initial_params_for(group: TuneGroup) -> Vec<f64> {
    match group {
        TuneGroup::Eval => eval_initial_params(),
        TuneGroup::Penalties => {
            numeric_fields_to_params(&PolicyPenalties::default(), ACTIVE_POLICY_PENALTY_FIELDS)
        }
        TuneGroup::Keywords => {
            let fields: Vec<&str> = KEYWORD_PARAMETER_NAMES
                .iter()
                .map(|name| name.strip_prefix("keyword.").unwrap_or(name))
                .collect();
            numeric_fields_to_params(&KeywordBonuses::default(), &fields)
        }
        TuneGroup::Archetype => {
            let multipliers = ArchetypeMultipliers::default();
            [
                multipliers.aggro,
                multipliers.midrange,
                multipliers.control,
                multipliers.combo,
                multipliers.ramp,
            ]
            .into_iter()
            .flatten()
            .collect()
        }
    }
}

fn eval_initial_params() -> Vec<f64> {
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

fn build_tuning_matchups(
    db: &CardDatabase,
    ids: &[&str],
) -> Result<Vec<(DeckPayload, &'static MatchupSpec)>, String> {
    ids.iter()
        .map(|id| {
            let spec = all_matchups()
                .iter()
                .find(|spec| spec.id == *id)
                .ok_or_else(|| format!("unknown tuning matchup {id}"))?;
            Ok((build_matchup_payload(db, spec)?, spec))
        })
        .collect()
}

fn build_matchup_payload(db: &CardDatabase, spec: &MatchupSpec) -> Result<DeckPayload, String> {
    let p0 = resolve_deck_ref(&spec.p0).map_err(|err| format!("{} p0: {err}", spec.id))?;
    let p1 = resolve_deck_ref(&spec.p1).map_err(|err| format!("{} p1: {err}", spec.id))?;
    let deck_list = DeckList {
        player: PlayerDeckList {
            main_deck: p0,
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
        },
        opponent: PlayerDeckList {
            main_deck: p1,
            sideboard: Vec::new(),
            commander: Vec::new(),
            ..Default::default()
        },
        ..Default::default()
    };
    Ok(resolve_deck_list(db, &deck_list))
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
    ai_configs.insert(PlayerId(0), config_p0.clone().into_measurement(seed));
    ai_configs.insert(
        PlayerId(1),
        config_p1.clone().into_measurement(seed.wrapping_add(1)),
    );

    let mut ai_rng = <rand::rngs::StdRng as rand::SeedableRng>::seed_from_u64(seed);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);
    loop {
        if let WaitingFor::GameOver { winner } = &state.waiting_for {
            return (*winner, state.turn_number);
        }
        if state.turn_number >= MAX_TURNS {
            return (None, state.turn_number);
        }

        let results = match std::panic::catch_unwind(AssertUnwindSafe(|| {
            run_ai_actions(
                &mut state,
                &ai_players,
                &ai_configs,
                &mut ai_rng,
                &ai_session,
            )
        })) {
            Ok(results) => results,
            Err(_) => return (None, state.turn_number),
        };
        if results.is_empty() {
            // No actions could be taken — game is stuck
            return (None, state.turn_number);
        }
    }
}

/// Evaluate fitness of a parameter vector by playing games across matchups.
/// Returns the average win rate of the candidate config vs the baseline.
fn evaluate_fitness(
    group: TuneGroup,
    params: &[f64],
    matchups: &[(DeckPayload, &str)],
    games_per_matchup: usize,
    base_seed: u64,
) -> f64 {
    let candidate = params_to_config_for(group, params);
    let opponent_pool = [AiDifficulty::Easy, AiDifficulty::Medium, AiDifficulty::Hard];
    let mut total_wins = 0usize;
    let mut total_games = 0usize;

    for (matchup_idx, (payload, _name)) in matchups.iter().enumerate() {
        for (opponent_idx, opponent_difficulty) in opponent_pool.iter().enumerate() {
            let opponent = create_config(*opponent_difficulty, Platform::Native);
            for game_idx in 0..games_per_matchup {
                let seed = base_seed
                    .wrapping_add(matchup_idx as u64 * 100_000)
                    .wrapping_add(opponent_idx as u64 * 10_000)
                    .wrapping_add(game_idx as u64);

                let paired = [
                    (true, run_game(payload, seed, &candidate, &opponent)),
                    (false, run_game(payload, seed, &opponent, &candidate)),
                ];
                for (candidate_is_p0, (winner, _turns)) in paired {
                    let Some(_) = winner else {
                        continue;
                    };
                    if candidate_won(winner, candidate_is_p0) {
                        total_wins += 1;
                    }
                    total_games += 1;
                }
            }
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
    eprintln!("  --output PATH     Output JSON path (default: <data-root>/cma-tuned-weights.json)");
    eprintln!(
        "  --group NAME      Parameter group: eval|penalties|keywords|archetype (default: eval)"
    );
    eprintln!("  --validate        Validate the tuned artifact at --output");
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
    let mut group = TuneGroup::Eval;

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
            "--group" => {
                i += 1;
                group = TuneGroup::from_label(&args[i]).unwrap_or_else(|| {
                    eprintln!("invalid --group '{}'", args[i]);
                    print_usage();
                    std::process::exit(1);
                });
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

    let output_path = output.unwrap_or_else(|| data_root.join("cma-tuned-weights.json"));

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

    let base_seed = seed.unwrap_or_else(|| {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    });

    if validate {
        let holdout = build_holdout_matchups(&db).unwrap_or_else(|err| {
            eprintln!("Error building holdout matchups: {err}");
            std::process::exit(1);
        });
        run_validate(&holdout, games, base_seed, &output_path);
    } else {
        let resolved = build_tuning_matchups(&db, FITNESS_MATCHUP_IDS).unwrap_or_else(|err| {
            eprintln!("Error building fitness matchups: {err}");
            std::process::exit(1);
        });
        let fitness_matchups: Vec<(DeckPayload, &str)> =
            resolved.iter().map(|(p, m)| (p.clone(), m.id)).collect();
        run_cmaes(
            group,
            &fitness_matchups,
            generations,
            population,
            games,
            base_seed,
            &output_path,
        );
    }
}

fn build_holdout_matchups(
    db: &CardDatabase,
) -> Result<Vec<(DeckPayload, &'static MatchupSpec)>, String> {
    build_tuning_matchups(db, HOLDOUT_MATCHUP_IDS)
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
    matchups: &[(DeckPayload, &'static MatchupSpec)],
    games: usize,
    base_seed: u64,
    tuned_path: &std::path::Path,
) {
    let games = if games == 20 { 100 } else { games }; // Default to 100 for validate
    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;

    eprintln!("=== Paired Holdout Validation ===");
    eprintln!("Games per matchup: {games}");
    eprintln!("Holdouts: {}", HOLDOUT_MATCHUP_IDS.join(", "));
    eprintln!("Opponent pool: Easy, Medium, Hard");

    let baseline_config = create_config(AiDifficulty::Medium, Platform::Native);
    let learned_config = load_cma_tuned_config(tuned_path).unwrap_or_else(|err| {
        eprintln!("Error loading tuned artifact: {err}");
        std::process::exit(1);
    });

    let opponent_pool = [AiDifficulty::Easy, AiDifficulty::Medium, AiDifficulty::Hard];
    let mut rows = Vec::new();
    let mut total_flipped_w_to_l = 0usize;
    let mut total_flipped_l_to_w = 0usize;
    let mut total_unchanged = 0usize;

    for (matchup_idx, (payload, matchup)) in matchups.iter().enumerate() {
        eprintln!("\nHoldout: {}", matchup.id);
        for (opponent_idx, opponent_difficulty) in opponent_pool.iter().enumerate() {
            let opponent_config = create_config(*opponent_difficulty, Platform::Native);
            let mut baseline_wins = 0usize;
            let mut learned_wins = 0usize;
            let mut flipped_w_to_l = 0usize;
            let mut flipped_l_to_w = 0usize;
            let mut unchanged = 0usize;

            for game_idx in 0..games {
                let seed = base_seed
                    .wrapping_add(matchup_idx as u64 * 100_000)
                    .wrapping_add(opponent_idx as u64 * 10_000)
                    .wrapping_add(game_idx as u64);
                let candidate_is_p0 = game_idx % 2 == 0;
                let (baseline_p0, baseline_p1) = if candidate_is_p0 {
                    (&baseline_config, &opponent_config)
                } else {
                    (&opponent_config, &baseline_config)
                };
                let (learned_p0, learned_p1) = if candidate_is_p0 {
                    (&learned_config, &opponent_config)
                } else {
                    (&opponent_config, &learned_config)
                };

                let (baseline_winner, _) = run_game(payload, seed, baseline_p0, baseline_p1);
                let (learned_winner, _) = run_game(payload, seed, learned_p0, learned_p1);
                let baseline_won = candidate_won(baseline_winner, candidate_is_p0);
                let learned_won = candidate_won(learned_winner, candidate_is_p0);

                if baseline_won {
                    baseline_wins += 1;
                }
                if learned_won {
                    learned_wins += 1;
                }

                match (baseline_won, learned_won) {
                    (true, false) => flipped_w_to_l += 1,
                    (false, true) => flipped_l_to_w += 1,
                    _ => unchanged += 1,
                }
            }

            total_flipped_w_to_l += flipped_w_to_l;
            total_flipped_l_to_w += flipped_l_to_w;
            total_unchanged += unchanged;
            let flips = flipped_w_to_l + flipped_l_to_w;
            let sign_test_p = (flips > 0)
                .then(|| sign_test_mid_p_upper_tail(flips, flipped_w_to_l.max(flipped_l_to_w)));
            let status = validation_status(flipped_w_to_l, flipped_l_to_w, sign_test_p);

            eprintln!(
                "  {:?}: baseline={:.1}% learned={:.1}% W→L={} L→W={} p={} {status}",
                opponent_difficulty,
                baseline_wins as f64 / games as f64 * 100.0,
                learned_wins as f64 / games as f64 * 100.0,
                flipped_w_to_l,
                flipped_l_to_w,
                sign_test_p
                    .map(|p| format!("{p:.4}"))
                    .unwrap_or_else(|| "—".to_string()),
            );

            rows.push(serde_json::json!({
                "matchup_id": matchup.id,
                "p0_label": matchup.p0_label,
                "p1_label": matchup.p1_label,
                "opponent_difficulty": format!("{opponent_difficulty:?}"),
                "games": games,
                "baseline_candidate_win_rate": r3(baseline_wins as f64 / games as f64),
                "learned_candidate_win_rate": r3(learned_wins as f64 / games as f64),
                "flipped_w_to_l": flipped_w_to_l,
                "flipped_l_to_w": flipped_l_to_w,
                "unchanged": unchanged,
                "sign_test_p": sign_test_p.map(r3),
                "status": status,
            }));
        }
    }

    let total_flips = total_flipped_w_to_l + total_flipped_l_to_w;
    let aggregate_p = (total_flips > 0).then(|| {
        sign_test_mid_p_upper_tail(total_flips, total_flipped_w_to_l.max(total_flipped_l_to_w))
    });
    let aggregate_status =
        validation_status(total_flipped_w_to_l, total_flipped_l_to_w, aggregate_p);

    eprintln!("\n=== Summary ===");
    eprintln!(
        "Aggregate flips: W→L={} L→W={} unchanged={} p={} {aggregate_status}",
        total_flipped_w_to_l,
        total_flipped_l_to_w,
        total_unchanged,
        aggregate_p
            .map(|p| format!("{p:.4}"))
            .unwrap_or_else(|| "—".to_string()),
    );

    let result = serde_json::json!({
        "mode": "validate",
        "metric": "paired_holdout_vs_opponent_pool",
        "artifact": tuned_path,
        "holdout_matchups": HOLDOUT_MATCHUP_IDS,
        "opponent_pool": opponent_pool.iter().map(|d| format!("{d:?}")).collect::<Vec<_>>(),
        "games_per_cell": games,
        "base_seed": base_seed,
        "rows": rows,
        "aggregate": {
            "flipped_w_to_l": total_flipped_w_to_l,
            "flipped_l_to_w": total_flipped_l_to_w,
            "unchanged": total_unchanged,
            "sign_test_p": aggregate_p.map(r3),
            "status": aggregate_status,
        },
        "improvement_detected": aggregate_status == "IMPROVED",
        "regression_detected": aggregate_status == "REGRESSED",
    });

    let json = serde_json::to_string_pretty(&result).unwrap();
    let output_path = validation_output_path(tuned_path);
    std::fs::write(&output_path, &json).unwrap();
    eprintln!("\nResults written to {}", output_path.display());
    println!("{json}");
}

fn candidate_won(winner: Option<PlayerId>, candidate_is_p0: bool) -> bool {
    matches!(
        (winner, candidate_is_p0),
        (Some(PlayerId(0)), true) | (Some(PlayerId(1)), false)
    )
}

fn validation_status(w_to_l: usize, l_to_w: usize, sign_test_p: Option<f64>) -> &'static str {
    if w_to_l > l_to_w && sign_test_p.is_some_and(|p| p < 0.05) {
        "REGRESSED"
    } else if l_to_w > w_to_l && sign_test_p.is_some_and(|p| p < 0.05) {
        "IMPROVED"
    } else if w_to_l != l_to_w {
        "SHIFTED"
    } else {
        "UNCHANGED"
    }
}

fn validation_output_path(tuned_path: &std::path::Path) -> PathBuf {
    let stem = tuned_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cma-tuned-weights");
    tuned_path.with_file_name(format!("{stem}-validation.json"))
}

fn manifest_output_path(tuned_path: &std::path::Path) -> PathBuf {
    let stem = tuned_path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("cma-tuned-weights");
    tuned_path.with_file_name(format!("{stem}-manifest.json"))
}

fn command_output(program: &str, args: &[&str]) -> Option<String> {
    let output = Command::new(program).args(args).output().ok()?;
    output
        .status
        .success()
        .then(|| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn config_hash(config: &AiConfig) -> String {
    let mut hasher = DefaultHasher::new();
    format!("{config:?}").hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

fn run_cmaes(
    group: TuneGroup,
    matchups: &[(DeckPayload, &str)],
    generations: usize,
    population: usize,
    games: usize,
    base_seed: u64,
    output_path: &std::path::Path,
) {
    eprintln!("=== CMA-ES AI Weight Tuning ===");
    let parameter_names = group.parameter_names();
    eprintln!(
        "Group: {}, Parameters: {}, Generations: {generations}, Population: {population}, Games/eval/cell: {games}",
        group.label(),
        parameter_names.len()
    );

    let baseline = create_config(AiDifficulty::Medium, Platform::Native);
    let initial = initial_params_for(group);

    let mut cma = CmaEs::new(parameter_names.len(), initial, 0.3, population);
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
                        group,
                        params,
                        matchups,
                        games,
                        gen_seed.wrapping_add(i as u64 * 1000),
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
                    group,
                    params,
                    matchups,
                    games,
                    gen_seed.wrapping_add(i as u64 * 1000),
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
    let final_config = params_to_config_for(group, final_params);

    let w = &final_config.weights.late;
    let p = &final_config.profile;

    let r3 = |v: f64| (v * 1000.0).round() / 1000.0;
    let parameters: serde_json::Map<String, serde_json::Value> = parameter_names
        .iter()
        .zip(final_params.iter())
        .map(|(name, value)| (name.clone(), serde_json::json!(r3(*value))))
        .collect();
    let result = serde_json::json!({
        "kind": CMA_TUNED_KIND,
        "source": "cma-es-self-play",
        "group": group.label(),
        "generations": generations,
        "population": population,
        "games_per_eval": games,
        "best_fitness": r3(best_fitness),
        "parameters": parameters,
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
        "policy_penalties": final_config.policy_penalties,
        "keyword_bonuses": final_config.keyword_bonuses,
        "archetype_multipliers": final_config.archetype_multipliers,
    });

    let json = serde_json::to_string_pretty(&result).unwrap();
    std::fs::write(output_path, &json).unwrap();
    let manifest = serde_json::json!({
        "kind": "cma_tuning_manifest",
        "artifact_kind": CMA_TUNED_KIND,
        "group": group.label(),
        "artifact_path": output_path,
        "git_sha": command_output("git", &["rev-parse", "--short=12", "HEAD"]),
        "seed": base_seed,
        "parameter_names": parameter_names,
        "fitness_decks": matchups.iter().map(|(_, name)| *name).collect::<Vec<_>>(),
        "holdout_decks": HOLDOUT_MATCHUP_IDS,
        "opponent_pool": ["Easy", "Medium", "Hard"],
        "draws_excluded_from_fitness": true,
        "paired_mirrored_seeds": true,
        "games_per_eval": games,
        "generations": generations,
        "population": population,
        "baseline_config_hash": config_hash(&baseline),
    });
    let manifest_json = serde_json::to_string_pretty(&manifest).unwrap();
    let manifest_path = manifest_output_path(output_path);
    std::fs::write(&manifest_path, manifest_json).unwrap();
    eprintln!("\nOptimized weights written to {}", output_path.display());
    eprintln!("Tune manifest written to {}", manifest_path.display());
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
        assert_eq!(initial_params().len(), EVAL_PARAMETER_NAMES.len());
        assert_eq!(
            initial_params_for(TuneGroup::Penalties).len(),
            ACTIVE_POLICY_PENALTY_FIELDS.len()
        );
        assert_eq!(
            initial_params_for(TuneGroup::Keywords).len(),
            KEYWORD_PARAMETER_NAMES.len()
        );
        assert_eq!(
            initial_params_for(TuneGroup::Archetype).len(),
            ARCHETYPE_NAMES.len() * ARCHETYPE_WEIGHT_NAMES.len()
        );
    }

    #[test]
    fn grouped_params_round_trip_to_config_sections() {
        let penalties = initial_params_for(TuneGroup::Penalties);
        let config = params_to_config_for(TuneGroup::Penalties, &penalties);
        assert_eq!(
            config.policy_penalties.combo_progress_this_turn_bonus,
            PolicyPenalties::default().combo_progress_this_turn_bonus
        );

        let keywords = initial_params_for(TuneGroup::Keywords);
        let config = params_to_config_for(TuneGroup::Keywords, &keywords);
        assert_eq!(
            config.keyword_bonuses.flying_mult,
            KeywordBonuses::default().flying_mult
        );
    }

    #[test]
    fn load_cma_tuned_config_rejects_wrong_kind() {
        let path = std::env::temp_dir().join("phase-ai-wrong-kind.json");
        std::fs::write(
            &path,
            r#"{
              "kind": "17lands_phase_weights",
              "weights": {},
              "profile": {}
            }"#,
        )
        .unwrap();

        let error = load_cma_tuned_config(&path).unwrap_err();

        assert!(error.contains("expected artifact kind"));
        let _ = std::fs::remove_file(path);
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

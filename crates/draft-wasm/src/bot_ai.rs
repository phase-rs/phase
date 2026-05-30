use std::collections::HashMap;

use draft_core::types::DraftCardInstance;
use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use phase_ai::draft_eval;
use rand::Rng;

/// Select a card index from the pack for a bot to pick.
///
/// Strategy scales with difficulty per D-02:
/// - VeryEasy: pure random
/// - Easy: rarity-weighted
/// - Medium / Hard: `phase_ai::draft_eval` card quality + rarity + color discipline + curve
/// - VeryHard: same, with stricter color discipline (an off-color penalty)
///
/// (Medium falls back to the lighter color + rarity + curve heuristic when no
/// CardDatabase is loaded, via [`pick_by_evaluation`].)
///
/// Returns the index into the `pack` slice.
pub fn bot_pick(
    pack: &[DraftCardInstance],
    difficulty: AiDifficulty,
    prior_picks: &[DraftCardInstance],
    card_db: Option<&CardDatabase>,
    rng: &mut impl Rng,
) -> usize {
    if pack.is_empty() {
        return 0;
    }

    match difficulty {
        AiDifficulty::VeryEasy => rng.random_range(0..pack.len()),
        AiDifficulty::Easy => pick_by_rarity(pack),
        AiDifficulty::Medium | AiDifficulty::Hard => {
            pick_by_evaluation(pack, prior_picks, card_db, false)
        }
        AiDifficulty::VeryHard | AiDifficulty::CEDH => {
            pick_by_evaluation(pack, prior_picks, card_db, true)
        }
    }
}

/// Pick the highest-rarity card. Ties broken by first occurrence.
fn pick_by_rarity(pack: &[DraftCardInstance]) -> usize {
    pack.iter()
        .enumerate()
        .max_by_key(|(_, c)| rarity_score(&c.rarity))
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Lighter heuristic: score = rarity * 2 + color_bonus + curve_bonus, using the
/// enriched DraftCardInstance fields (colors, cmc) directly. Used as the no-DB
/// fallback inside [`pick_by_evaluation`].
fn pick_by_color_and_rarity(
    pack: &[DraftCardInstance],
    prior_picks: &[DraftCardInstance],
) -> usize {
    let preferred_colors = color_preference(prior_picks);

    pack.iter()
        .enumerate()
        .max_by_key(|(_, card)| {
            let rarity = rarity_score(&card.rarity) as i16 * 2;
            let color_bonus = if card.colors.is_empty() {
                // Colorless cards are always on-color
                1i16
            } else if card.colors.iter().any(|c| preferred_colors.contains(c)) {
                3
            } else if preferred_colors.is_empty() {
                // No preference yet (early picks) — no bonus/penalty
                0
            } else {
                -1
            };
            let curve = curve_bonus(card.cmc, prior_picks.len() as u8);
            rarity + color_bonus + curve as i16
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Medium/Hard/VeryHard strategy: `phase_ai::draft_eval` card quality plus a rarity
/// prior, color discipline, and a curve bonus. `strict` (VeryHard) raises the
/// on-color bonus and adds an off-color penalty. Falls back to
/// [`pick_by_color_and_rarity`] if no CardDatabase is loaded.
fn pick_by_evaluation(
    pack: &[DraftCardInstance],
    prior_picks: &[DraftCardInstance],
    card_db: Option<&CardDatabase>,
    strict: bool,
) -> usize {
    let card_db = match card_db {
        Some(db) => db,
        None => return pick_by_color_and_rarity(pack, prior_picks),
    };

    let preferred_colors = color_preference(prior_picks);
    let pick_number = prior_picks.len() as u8;

    // Color bonus multiplier: stricter for VeryHard
    let on_color_bonus: f64 = if strict { 6.0 } else { 4.0 };
    let off_color_penalty: f64 = if strict { -2.0 } else { 0.0 };

    pack.iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            let score_a = eval_score(
                a,
                card_db,
                &preferred_colors,
                pick_number,
                on_color_bonus,
                off_color_penalty,
            );
            let score_b = eval_score(
                b,
                card_db,
                &preferred_colors,
                pick_number,
                on_color_bonus,
                off_color_penalty,
            );
            score_a
                .partial_cmp(&score_b)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(i, _)| i)
        .unwrap_or(0)
}

/// Pick-context score for a card: intrinsic card quality (`phase_ai::draft_eval`)
/// plus a rarity prior, color discipline relative to prior picks, and a curve bonus.
fn eval_score(
    card: &DraftCardInstance,
    card_db: &CardDatabase,
    preferred_colors: &[String],
    pick_number: u8,
    on_color_bonus: f64,
    off_color_penalty: f64,
) -> f64 {
    let base = card_quality(card, Some(card_db));

    let color_bonus = if card.colors.is_empty() {
        1.0 // Colorless — always fine
    } else if preferred_colors.is_empty() {
        0.0 // No preference yet
    } else if card.colors.iter().any(|c| preferred_colors.contains(c)) {
        on_color_bonus
    } else {
        off_color_penalty
    };

    let curve = curve_bonus(card.cmc, pick_number) as f64;

    base + color_bonus + curve
}

/// Intrinsic card quality: the engine-data evaluator ([`draft_eval::evaluate_draft_card`])
/// plus a small rarity prior. Falls back to just the rarity prior when no
/// CardDatabase is loaded or the card face isn't found.
fn card_quality(card: &DraftCardInstance, card_db: Option<&CardDatabase>) -> f64 {
    let quality = card_db
        .and_then(|db| db.get_face_by_name(&card.name))
        .map(draft_eval::evaluate_draft_card_default)
        .unwrap_or(0.0);
    quality + draft_eval::rarity_prior(&card.rarity)
}

fn rarity_score(rarity: &str) -> u8 {
    match rarity {
        "mythic" => 4,
        "rare" => 3,
        "uncommon" => 2,
        "common" => 1,
        _ => 0,
    }
}

/// Extract the 1-2 most common colors from prior picks.
/// Returns empty vec if no clear preference (early draft).
fn color_preference(prior_picks: &[DraftCardInstance]) -> Vec<String> {
    if prior_picks.len() < 3 {
        return Vec::new();
    }

    let mut counts: HashMap<&str, u32> = HashMap::new();
    for card in prior_picks {
        for color in &card.colors {
            *counts.entry(color.as_str()).or_insert(0) += 1;
        }
    }

    if counts.is_empty() {
        return Vec::new();
    }

    let mut sorted: Vec<(&&str, &u32)> = counts.iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(a.1));

    // Take top 2 colors
    sorted
        .iter()
        .take(2)
        .map(|(color, _)| color.to_string())
        .collect()
}

/// Mana curve position bonus. Prefer CMC 2-4 creatures, especially early in draft.
fn curve_bonus(cmc: u8, pick_number: u8) -> i8 {
    let early = pick_number < 15; // First pack roughly

    match cmc {
        2 => {
            if early {
                2
            } else {
                1
            }
        }
        3 => {
            if early {
                2
            } else {
                1
            }
        }
        4 => 1,
        5 => 0,
        1 => 0,
        0 => 0, // lands, weird cards
        _ => {
            // CMC 6+: slight penalty, less so late
            if early {
                -1
            } else {
                0
            }
        }
    }
}

//! The bot decision layer: what a bot seat does with what the ENGINE PUBLISHED
//! to it.
//!
//! Two bots live here, for the two distributions that seat one. The
//! pick-and-pass bot ([`bot_pick`] / [`bot_picks`]) chooses a card out of a
//! pack it is handed. The shared-stack bot ([`winston_decision`]) chooses
//! take-or-decline at the cursor of a Winston turn, scoring with
//! `phase_ai::winston_eval`.
//!
//! # The no-cheating property, and exactly what enforces each half
//!
//! > The bot's decision is a function of the projection and nothing else: two
//! > worlds whose projections for the bot's seat are identical must produce the
//! > same decision, however they differ underneath.
//!
//! There are three channels by which the decision could come to depend on the
//! hidden main stack or on a pile the bot has not inspected, and each has ONE
//! named device. No device is claimed to cover a channel it does not:
//!
//! 1. **The argument list.** [`winston_decision`] takes a `&DraftPlayerView`,
//!    and `phase_ai::winston_eval` takes plain card facts plus a slice of pile
//!    HEIGHTS -- there is no field in either that could carry a card the bot may
//!    not see. Enforced for the `phase-ai` half by the type system (the hidden
//!    board is inexpressible there) and for this half by
//!    `the_bot_ignores_the_world_it_is_not_handed`, which varies the world while
//!    holding the argument fixed.
//! 2. **The raw thread-local.** `DRAFT_SESSION` lives in the private sibling
//!    module `crate::session_cell`, so neither `crate::DRAFT_SESSION` (no such
//!    path: `error[E0425]`) nor `crate::session_cell::DRAFT_SESSION`
//!    (`error[E0603]`) resolves from here. Enforced by the COMPILER.
//! 3. **The `pub(crate)` accessors.** `session_cell::with_draft` and its
//!    siblings ARE nameable from this module, and Rust has no visibility that
//!    says "the crate root but not a sibling". Enforced by a TEST, not the
//!    compiler: `the_bot_ignores_the_world_it_is_not_handed` installs a third,
//!    independently started session between calls and requires the decision to
//!    be unchanged. Stated plainly rather than dressed up -- if that test is
//!    ever weakened, this channel is open.
//!
//! There is no RNG anywhere in the shared-stack decision, deliberately: "same
//! projection => same decision" is meant literally, which is also why every
//! tally it folds is ordered (`ColorPassTally`'s `BTreeMap`,
//! `draft_eval::dominant_colors`'s total order).
//!
//! # What the bot deliberately does NOT model yet
//!
//! - **Cross-turn memory of pile contents across a take-and-refill.**
//!   [`opponent_read`]'s fold recovers what another seat passed on piles that
//!   have not been taken since; a human also remembers what a pile held before
//!   it was taken and refilled, and the projection DID legitimately show them
//!   those cards, so modelling it would not be cheating. It is deferred because
//!   it needs bot-local persistent state keyed by seat, with its own storage
//!   lifecycle in a wasm thread-local and its own invalidation rule -- a second
//!   feature, not a missing line. Named here so the next author is not scared
//!   off it by the no-cheating property.
//! - **The "card type" half of "watch what your opponent passes".**
//!   `DraftCardInstance` publishes `type_line` and `cmc` beside `colors`, so a
//!   type tally is recoverable from the IDENTICAL input with no new
//!   information. Deferred on effort -- it needs its own weight and its own
//!   two-sided test -- not on an information limit. The design is
//!   `phase_ai::winston_eval::ColorPassTally` with a type key.

use draft_core::types::{DraftCardInstance, SharedStackPileDecision};
use draft_core::view::{DraftPlayerView, SharedStackPileView, SharedStackView};
use engine::database::CardDatabase;
use phase_ai::config::AiDifficulty;
use phase_ai::draft_eval;
use phase_ai::winston_eval::{
    draft_progress, valuate_turn, CardContext, ColorPassTally, DraftCardFacts, OpponentRead,
    WinstonStrategy, WinstonTurn,
};
use rand::Rng;

/// Cards a Winston bot must hold before its own pool implies a colour
/// preference at all, matching [`color_preference`]'s floor for the
/// pick-and-pass bot: one authority for "which colours is this pile of cards
/// in", one convention for when it has seen enough to answer.
const MIN_POOL_COLOR_SAMPLE: usize = 3;

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

/// Select `count` distinct card indices for a bot's pick step, or every index
/// when the pack holds fewer than `count`.
///
/// CR 903.13b: a Commander Draft seat drafts two cards per step, so a bot in
/// such a pod must return two indices or the round never completes. Composed
/// from [`bot_pick`] applied to the shrinking remainder; *which* cards a bot
/// takes for a multi-card step is deliberately untuned (out of scope).
///
/// Returns indices into the original `pack` slice, in selection order.
pub fn bot_picks(
    pack: &[DraftCardInstance],
    count: usize,
    difficulty: AiDifficulty,
    prior_picks: &[DraftCardInstance],
    card_db: Option<&CardDatabase>,
    rng: &mut impl Rng,
) -> Vec<usize> {
    // Candidates carry their original index so the caller can map back to
    // `instance_id`s before mutating anything. Held as two parallel vectors
    // rather than a `Vec<(usize, _)>` so that `bot_pick` can borrow the cards as
    // the contiguous slice it takes, without rebuilding one per iteration:
    // `swap_remove(position)` applies the same permutation to both, so they stay
    // aligned, and the whole walk costs one clone of the pack instead of
    // `count + 1`.
    let mut candidates: Vec<DraftCardInstance> = pack.to_vec();
    let mut original_indices: Vec<usize> = (0..pack.len()).collect();
    let mut picked = Vec::with_capacity(count.min(candidates.len()));

    for _ in 0..count.min(pack.len()) {
        let position = bot_pick(&candidates, difficulty, prior_picks, card_db, rng);
        candidates.swap_remove(position);
        picked.push(original_indices.swap_remove(position));
    }

    picked
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
/// Returns empty vec if no clear preference (fewer than 3 prior picks).
///
/// Delegates to [`draft_eval::dominant_colors`], the single authority for
/// "which colors is this pile of cards in" — shared with the Winston valuation
/// layer's late color ramp, so the two never drift apart.
///
/// **This changes `bot_pick`'s behavior for pools with tied colors**, and that is
/// the point of the lift. The body that used to live here sorted `HashMap`
/// entries on count alone; Rust randomizes `HashMap` iteration order per map
/// instance, so tied colors came out in an arbitrary order and identical inputs
/// produced different picks. MEASURED: a four-way tie produced 12 distinct
/// answers over 2000 draws, and a W=3/U=2/B=2 pool was a coin flip between
/// `["W", "U"]` and `["W", "B"]`. `dominant_colors`'s total order pins it to
/// `["W", "B"]`. Behavior on strictly ordered pools is unchanged —
/// `dominant_colors_delegation_preserves_bot_pick_on_a_strictly_ordered_pool`.
fn color_preference(prior_picks: &[DraftCardInstance]) -> Vec<String> {
    let colors: Vec<&[String]> = prior_picks
        .iter()
        .map(|card| card.colors.as_slice())
        .collect();
    draft_eval::dominant_colors(&colors, 3)
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

// ── Shared-stack (Winston) bot ──────────────────────────────────────────────

/// One published card, as the valuation layer wants it.
///
/// The only place a `DraftCardInstance` becomes a `DraftCardFacts`: the face
/// comes from the PUBLIC card database through this bot's own argument, and
/// every other field rides the card instance the projection published.
fn card_facts<'a>(
    card: &'a DraftCardInstance,
    card_db: Option<&'a CardDatabase>,
) -> DraftCardFacts<'a> {
    DraftCardFacts {
        face: card_db.and_then(|db| db.get_face_by_name(&card.name)),
        rarity: &card.rarity,
        cmc: card.cmc,
        colors: &card.colors,
    }
}

/// Whether the ENGINE published this decision as legal on this pile.
///
/// `shared_stack::refusal_for` is the single legality authority and this bot is
/// a CHOOSING layer above it, never a second authority: nothing here looks at
/// `total`, `main_stack_remaining` or the pile count to decide what is allowed.
/// A decision the published vector does not mention at all is treated as
/// illegal, which is the safe direction -- the reducer would refuse it.
fn published_legal(pile: &SharedStackPileView, decision: SharedStackPileDecision) -> bool {
    pile.legality
        .iter()
        .any(|entry| entry.decision == decision && entry.refusal.is_none())
}

/// Laplace-smoothed share, `(hits + 1) / (trials + 2)`, so one observation
/// cannot produce a 0.0 or 1.0 rate the rest of the arithmetic would then
/// treat as certainty.
fn laplace_rate(hits: usize, trials: usize) -> f64 {
    (hits + 1) as f64 / (trials + 2) as f64
}

/// What the other seats have told the bot about themselves, folded out of the
/// PUBLISHED decision history and the bot's own PUBLISHED `revealed` prefixes.
///
/// Two reads, one input. The pile-size appetite (a seat that snaps up small
/// piles is bomb-hunting; a seat that takes big ones makes declining expensive)
/// comes from `stack.history` alone. The colour read -- "watch what your
/// opponent passes; a colour they keep passing is a colour that is OPEN" --
/// joins `stack.history` against `stack.piles[..].revealed`.
///
/// # The reconstruction rule for the colour read
///
/// For each history record `r = (seat != own, pile p, Decline, pile_size s)` at
/// index `i`:
///
/// 1. **Invalidation.** Skip `r` if any record after `i` is `(pile p, Take)`.
///    A take empties pile `p` and refills it with one fresh card, so the
///    prefix's identity is destroyed. Declines only append, so they never
///    invalidate.
/// 2. **Visibility.** Read `view.shared_stack.piles[p].revealed`, addressed by
///    `index` and never by vector position. Take
///    `revealed[.. min(s, revealed.len())]`. `revealed` is non-empty only for
///    piles at or below the cursor and only for the active viewer -- which is
///    exactly when the bot is deciding.
/// 3. **Binding.** **Snapshotted per call**, recomputed from the view each
///    decision. Nothing is cached across turns; cross-turn bot-local storage
///    remains the deferral this module's doc names.
///
/// It invents nothing: MEASURED across ten seeded walks against ground truth
/// taken from the reducer's own state, the rule reconstructed 18-41 distinct
/// passed cards with **zero** false positives, and dropping clause 1 is exactly
/// what would start inventing them.
///
/// Two consequences worth stating rather than discovering. A pile another seat
/// declined twice contributes its overlapping prefixes twice -- deliberate,
/// because they did pass those cards twice, and `ColorPassTally` is a tally of
/// passing events rather than of distinct cards. And a record whose `pile_size`
/// exceeds the published prefix TRUNCATES rather than panicking: the history is
/// bounded and the view is rebuilt every decision, so the two can legitimately
/// disagree at the edges.
pub fn opponent_read(stack: &SharedStackView, own_seat: u8, split: usize) -> OpponentRead {
    let mut small_takes = 0usize;
    let mut small_total = 0usize;
    let mut large_takes = 0usize;
    let mut large_total = 0usize;
    let mut samples = 0usize;
    let mut passed_colors = ColorPassTally::default();

    for (index, record) in stack.history.iter().enumerate() {
        // The bot reads OTHER seats. Folding its own decisions in would make it
        // model itself and call the result an opponent.
        if record.seat == own_seat {
            continue;
        }
        samples += 1;

        let took = matches!(record.decision, SharedStackPileDecision::Take);
        if record.pile_size >= split {
            large_total += 1;
            large_takes += usize::from(took);
        } else {
            small_total += 1;
            small_takes += usize::from(took);
        }

        if took {
            continue;
        }
        // Clause 1: a later take on that pile destroys the prefix's identity.
        let disturbed = stack.history[index + 1..].iter().any(|later| {
            later.pile == record.pile && matches!(later.decision, SharedStackPileDecision::Take)
        });
        if disturbed {
            continue;
        }
        // Clause 2: addressed by `index`, never by vector position, and
        // truncated to what is actually published.
        let Some(pile) = stack.piles.iter().find(|pile| pile.index == record.pile) else {
            continue;
        };
        let seen = record.pile_size.min(pile.revealed.len());
        for card in &pile.revealed[..seen] {
            passed_colors.observe(&card.colors);
        }
    }

    OpponentRead {
        small_pile_take_rate: laplace_rate(small_takes, small_total),
        large_pile_take_rate: laplace_rate(large_takes, large_total),
        passed_colors,
        samples,
    }
}

/// Decide a shared-stack turn: take the cursor pile, or put it back.
///
/// Returns `(pile, decision)` for the CURSOR pile, or `None` when there is no
/// live pile turn in this projection or the engine published no legal decision
/// at all. The `u8` is the cursor's own published `index`, never a position in
/// the pile vector and never a pile this function picked out by score: the
/// cursor belongs to the engine, and a bot that scored all three piles and
/// returned the best one would be refused `PileNotActive` every time. That is
/// why the decision axis is take-or-decline AT THE CURSOR and the returned
/// index is an optimistic-concurrency check rather than a selection.
///
/// # Inputs, and only these
///
/// `view` is the bot seat's own projection. Everything the decision reads comes
/// out of it: the cursor pile's `revealed`, every pile's `total` and `index`,
/// `main_stack_remaining` through `total_cards`, `active_seat`, `active_pile`,
/// the published `legality` vector, `history`, and the bot's own `pool`. The
/// main stack's ORDER and the contents of any pile the bot has not inspected
/// are not in the projection at all, and this function reads nothing else --
/// see the module doc for which device closes which channel.
///
/// `card_db` is the PUBLIC card database. Without one, every card degrades to
/// its rarity prior (principles 1 and 5 go quiet) and the decision is still
/// legal and still produced -- pinned by
/// `winston_decision_degrades_without_a_card_database`.
///
/// # Legality is asked, never derived
///
/// The bot picks among decisions whose PUBLISHED `refusal` is `None`. It cannot
/// return a decision the reducer then refuses, because it never forms an
/// opinion about legality of its own: `shared_stack::refusal_for` decides, this
/// function chooses.
pub fn winston_decision(
    view: &DraftPlayerView,
    difficulty: AiDifficulty,
    card_db: Option<&CardDatabase>,
) -> Option<(u8, SharedStackPileDecision)> {
    let stack = view.shared_stack.as_ref()?;
    // BY `index`, never by vector position -- the discipline the pick timer's
    // `autoDecideSharedStackTurn` already states on the client side.
    let cursor = stack
        .piles
        .iter()
        .find(|pile| pile.index == stack.active_pile)?;

    let weights = match WinstonStrategy::for_difficulty(difficulty) {
        // Exactly the move `shared_stack::forced_decision` folds and the pick
        // timer submits: the first always-legal decision, no evaluation at all.
        // `SharedStackPileDecision::ALL` is folded rather than the two variants
        // written out, so this cannot go narrow if the axis ever grows.
        WinstonStrategy::FirstLegal => {
            return SharedStackPileDecision::ALL
                .into_iter()
                .find(|decision| published_legal(cursor, *decision))
                .map(|decision| (cursor.index, decision));
        }
        WinstonStrategy::Scored(weights) => weights,
    };

    // The seat whose turn it is. The driver hands this function
    // `filter_for_player(session, active_seat)`, so "the bot" and "the active
    // seat" are the same seat by construction; reading it off the projection
    // rather than taking it as an argument keeps the two from disagreeing.
    let own_seat = stack.active_seat;
    let read = opponent_read(stack, own_seat, weights.opponent_size_split);

    let cursor_pile: Vec<DraftCardFacts> = cursor
        .revealed
        .iter()
        .map(|card| card_facts(card, card_db))
        .collect();
    let pool: Vec<DraftCardFacts> = view
        .pool
        .iter()
        .map(|card| card_facts(card, card_db))
        .collect();

    let pool_colors: Vec<&[String]> = view
        .pool
        .iter()
        .map(|card| card.colors.as_slice())
        .collect();
    let preferred_colors = draft_eval::dominant_colors(&pool_colors, MIN_POOL_COLOR_SAMPLE);

    // Heights only, in pile order, INCLUSIVE of the last pile: declining onto
    // the final pile and taking it is the commonest way a Winston turn ends
    // with a large pile, and a continuation that stopped one pile short would
    // systematically under-value it. Sorted by the engine's `index` rather than
    // trusting the vector's order.
    let mut later: Vec<&SharedStackPileView> = stack
        .piles
        .iter()
        .filter(|pile| pile.index > cursor.index)
        .collect();
    later.sort_by_key(|pile| pile.index);
    let later_pile_sizes: Vec<usize> = later.iter().map(|pile| pile.total).collect();

    // ONLY from the published verdict, never re-derived from
    // `main_stack_remaining`. The projection publishes a legality verdict for
    // the CURSOR pile alone (every other pile answers `PileNotActive`), so the
    // forced draw is priced exactly when the cursor IS the final pile and the
    // engine published that decline as legal.
    //
    // APPROXIMATION, with its direction: from an earlier cursor the bot cannot
    // know whether the forced draw will still be legal several declines from
    // now without re-deriving legality from the card counts, so it omits that
    // stopping point and UNDER-values the continuation there. Never the other
    // way, which is the direction that would matter: it can only make the bot
    // take a pile it might have declined, never propose an illegal decline.
    let forced_draw_legal =
        later_pile_sizes.is_empty() && published_legal(cursor, SharedStackPileDecision::Decline);

    let turn = WinstonTurn {
        cursor_pile: &cursor_pile,
        later_pile_sizes: &later_pile_sizes,
        forced_draw_legal,
        pool: &pool,
        read: &read,
        context: CardContext {
            preferred_colors: &preferred_colors,
            // `total_cards` is the UNDRAFTED count, so this ratio rises
            // monotonically over the draft. `f64` division lives in
            // `draft_progress`; nothing here quantises it.
            progress: draft_progress(view.pool.len(), stack.total_cards, view.seats.len()),
            passed: &read.passed_colors,
        },
    };

    // Preference first, published legality second, and NEVER the other way: the
    // bot proposes, the authority disposes. Ties inside `prefers_taking` go to
    // `Take`, matching `SharedStackPileDecision::ALL`'s declaration order and
    // the order `forced_decision` folds.
    let preferred = if valuate_turn(&turn, &weights).prefers_taking() {
        [
            SharedStackPileDecision::Take,
            SharedStackPileDecision::Decline,
        ]
    } else {
        [
            SharedStackPileDecision::Decline,
            SharedStackPileDecision::Take,
        ]
    };
    preferred
        .into_iter()
        .find(|decision| published_legal(cursor, *decision))
        .map(|decision| (cursor.index, decision))
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand::SeedableRng;
    use rand_chacha::ChaCha20Rng;

    fn pack(size: usize) -> Vec<DraftCardInstance> {
        (0..size)
            .map(|i| DraftCardInstance {
                instance_id: format!("card-{i}"),
                name: format!("Card {i}"),
                set_code: "TST".to_string(),
                collector_number: format!("{i}"),
                rarity: "common".to_string(),
                colors: Vec::new(),
                cmc: 0,
                type_line: String::new(),
                draft_effect: None,
            })
            .collect()
    }

    fn colored(name: &str, colors: &[&str]) -> DraftCardInstance {
        DraftCardInstance {
            instance_id: format!("{name}-id"),
            name: name.to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: "common".to_string(),
            colors: colors.iter().map(|c| c.to_string()).collect(),
            cmc: 2,
            type_line: "Creature".to_string(),
            draft_effect: None,
        }
    }

    /// The `dominant_colors` lift is behavior-preserving where the old body was
    /// deterministic — i.e. on a pool with a STRICT color ordering. The tie case
    /// deliberately changes (see `color_preference`'s doc and
    /// `draft_eval::dominant_colors_is_total_on_ties`), so it is pinned there and
    /// not here.
    #[test]
    fn dominant_colors_delegation_preserves_bot_pick_on_a_strictly_ordered_pool() {
        // W=2, U=1: a strict ordering with no tie to break.
        let prior = vec![
            colored("Prior W1", &["W"]),
            colored("Prior W2", &["W"]),
            colored("Prior U1", &["U"]),
        ];
        assert_eq!(
            color_preference(&prior),
            vec!["W".to_string(), "U".to_string()],
            "a strictly ordered pool keeps its pre-lift preference"
        );

        // On-color first, off-color second: `max_by_key` returns the LAST maximum,
        // so an empty preference (which is what a changed sample floor produces)
        // flips the pick to index 1.
        let pack = vec![colored("On Color", &["W"]), colored("Off Color", &["G"])];
        let mut rng = ChaCha20Rng::seed_from_u64(1);
        for _ in 0..200 {
            assert_eq!(
                bot_pick(&pack, AiDifficulty::Medium, &prior, None, &mut rng),
                0,
                "the on-color card must win, every time"
            );
        }

        // Below the sample floor there is no preference at all, and the tie
        // resolves the other way — the paired negative that proves the preference
        // is what drove the pick above.
        let short = vec![colored("Prior W1", &["W"]), colored("Prior W2", &["W"])];
        assert!(color_preference(&short).is_empty());
        assert_eq!(
            bot_pick(&pack, AiDifficulty::Medium, &short, None, &mut rng),
            1
        );
    }

    // ── Shared-stack (Winston) fixtures ─────────────────────────────────────
    //
    // Every fixture below is built from ONE five-card vocabulary with ONE
    // card-database, so a pile's score is a property of the cards in it rather
    // than of a fixture written for the test that needed it.

    use draft_core::pack_source::PackSource;
    use draft_core::session;
    use draft_core::types::{
        DeckAddableCards, DraftAction, DraftConfig, DraftKind, DraftPack, DraftSeat, DraftSession,
        DraftSource, DraftStatus, PodPolicy, SharedStackDecisionRecord, SharedStackRefusal,
        SpectatorVisibility, TournamentFormat,
    };
    use draft_core::view::{filter_for_player, SharedStackDecisionView};
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, TargetFilter, TriggerDefinition,
    };
    use engine::types::card::CardFace;
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    /// The six `AiDifficulty` rungs, in declaration order.
    ///
    /// Hand-written, with [`rung_index`] as the wildcard-free `match` that makes
    /// a seventh rung an `E0004` right beside it -- the `DraftKind::ALL` idiom.
    /// Every no-cheating test folds this array rather than naming `Medium`, so
    /// none of them can go narrow when the ladder grows.
    const ALL_RUNGS: [AiDifficulty; 6] = [
        AiDifficulty::VeryEasy,
        AiDifficulty::Easy,
        AiDifficulty::Medium,
        AiDifficulty::Hard,
        AiDifficulty::VeryHard,
        AiDifficulty::CEDH,
    ];

    fn rung_index(difficulty: AiDifficulty) -> usize {
        match difficulty {
            AiDifficulty::VeryEasy => 0,
            AiDifficulty::Easy => 1,
            AiDifficulty::Medium => 2,
            AiDifficulty::Hard => 3,
            AiDifficulty::VeryHard => 4,
            AiDifficulty::CEDH => 5,
        }
    }

    #[test]
    fn all_rungs_lists_every_difficulty_once() {
        for (position, rung) in ALL_RUNGS.into_iter().enumerate() {
            assert_eq!(rung_index(rung), position, "{rung:?}");
        }
    }

    const DUAL_LAND: &str = "Test Dual Land";
    const BEAR: &str = "Test Bear";
    const FILLER: &str = "Test Filler";
    const REMOVAL: &str = "Test Removal";
    const BOMB: &str = "Test Bomb";
    const VOCABULARY: [&str; 5] = [DUAL_LAND, BEAR, FILLER, REMOVAL, BOMB];

    fn named_face(name: &str, core: Vec<CoreType>) -> CardFace {
        CardFace {
            name: name.to_string(),
            card_type: CardType {
                core_types: core,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn destroy_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        )
    }

    /// The five faces, mirroring `phase_ai::winston_eval`'s own fixtures so the
    /// two layers agree on what a dual land / a removal spell / a body is.
    fn fixture_faces() -> Vec<CardFace> {
        let mut dual = named_face(DUAL_LAND, vec![CoreType::Land]);
        dual.card_type.subtypes = vec!["Plains".to_string(), "Island".to_string()];

        let bear = CardFace {
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            mana_cost: ManaCost::generic(2),
            ..named_face(BEAR, vec![CoreType::Creature])
        };
        let filler = CardFace {
            power: Some(PtValue::Fixed(1)),
            toughness: Some(PtValue::Fixed(1)),
            mana_cost: ManaCost::generic(4),
            ..named_face(FILLER, vec![CoreType::Creature])
        };
        let removal = CardFace {
            mana_cost: ManaCost::generic(2),
            abilities: vec![destroy_ability()],
            ..named_face(REMOVAL, vec![CoreType::Instant])
        };
        let mut bomb = CardFace {
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            mana_cost: ManaCost::generic(4),
            ..named_face(BOMB, vec![CoreType::Creature])
        };
        bomb.triggers = vec![TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(destroy_ability())];

        vec![dual, bear, filler, removal, bomb]
    }

    /// The public card database the bot reads through its own argument list.
    fn fixture_card_db() -> CardDatabase {
        let entries: serde_json::Map<String, serde_json::Value> = fixture_faces()
            .into_iter()
            .map(|face| {
                (
                    face.name.to_lowercase(),
                    serde_json::to_value(&face).expect("a face serializes"),
                )
            })
            .collect();
        CardDatabase::from_json_str(&serde_json::Value::Object(entries).to_string())
            .expect("the fixture export loads")
    }

    /// One published card instance. The printing's axes (rarity, colours, mana
    /// value) live here; the face lives in the database.
    fn fixture_instance(name: &str, instance_id: String) -> DraftCardInstance {
        let (colors, cmc, rarity): (&[&str], u8, &str) = match name {
            DUAL_LAND => (&[], 0, "uncommon"),
            BEAR => (&["W"], 2, "common"),
            FILLER => (&["G"], 4, "common"),
            REMOVAL => (&["W"], 2, "uncommon"),
            BOMB => (&["U"], 4, "rare"),
            other => panic!("unknown fixture card {other}"),
        };
        DraftCardInstance {
            instance_id,
            name: name.to_string(),
            set_code: "TST".to_string(),
            collector_number: "1".to_string(),
            rarity: rarity.to_string(),
            colors: colors.iter().map(|c| (*c).to_string()).collect(),
            cmc,
            type_line: String::new(),
            draft_effect: None,
        }
    }

    fn fixture_card(name: &str, id: &str) -> DraftCardInstance {
        fixture_instance(name, id.to_string())
    }

    /// A pack source over the fixture vocabulary, so a started session's piles
    /// hold cards the fixture database actually knows.
    struct FixtureWinstonSource {
        cards_per_pack: u8,
    }

    impl PackSource for FixtureWinstonSource {
        fn generate_pack(
            &self,
            _rng: &mut dyn rand::RngCore,
            seat: u8,
            pack_number: u8,
        ) -> DraftPack {
            DraftPack(
                (0..self.cards_per_pack)
                    .map(|i| {
                        let name = VOCABULARY[usize::from(i) % VOCABULARY.len()];
                        fixture_instance(name, format!("TST-{seat}-{pack_number}-{i}"))
                    })
                    .collect(),
            )
        }
    }

    const CARDS_PER_PACK: u8 = 15;

    fn winston_session(pod_size: u8, rng_seed: u64, bot_seats: &[u8]) -> DraftSession {
        let config = DraftConfig {
            source: DraftSource::single_set("TST".to_string()),
            set_code: "TST".to_string(),
            kind: DraftKind::Winston,
            pod_size,
            cards_per_pack: CARDS_PER_PACK,
            pack_count: 3,
            min_deck_size: 40,
            addable_cards: DeckAddableCards::standard_basics(),
            rng_seed,
            tournament_format: TournamentFormat::Swiss,
            pod_policy: PodPolicy::Competitive,
            spectator_visibility: SpectatorVisibility::default(),
        };
        let seats: Vec<DraftSeat> = (0..pod_size)
            .map(|i| {
                if bot_seats.contains(&i) {
                    DraftSeat::Bot {
                        name: format!("Bot {i}"),
                    }
                } else {
                    DraftSeat::Human {
                        player_id: PlayerId(i),
                        display_name: format!("Player {i}"),
                    }
                }
            })
            .collect();
        DraftSession::new(config, seats, "WIN-BOT".to_string())
    }

    fn started_winston(pod_size: u8, rng_seed: u64, bot_seats: &[u8]) -> DraftSession {
        let mut session = winston_session(pod_size, rng_seed, bot_seats);
        session::apply(
            &mut session,
            DraftAction::StartDraft,
            Some(&FixtureWinstonSource {
                cards_per_pack: CARDS_PER_PACK,
            }),
        )
        .expect("a Winston pod starts");
        session
    }

    fn stack_of(session: &DraftSession) -> &draft_core::types::SharedStackState {
        session.shared_stack.as_ref().expect("a live pile turn")
    }

    fn apply_decision(
        session: &mut DraftSession,
        decision: SharedStackPileDecision,
    ) -> Result<(), String> {
        let (seat, pile) = {
            let state = stack_of(session);
            (state.active_seat, state.cursor)
        };
        session::apply(
            session,
            DraftAction::SharedStackDecision {
                seat,
                pile,
                decision,
            },
            None,
        )
        .map(|_| ())
        .map_err(|e| e.to_string())
    }

    /// One whole turn of declines, which is what grows the piles past their
    /// one-card opening shape -- the state the hidden-information fixtures need.
    fn declined_one_turn(pod_size: u8, rng_seed: u64) -> DraftSession {
        let mut session = started_winston(pod_size, rng_seed, &[]);
        for _ in 0..3 {
            apply_decision(&mut session, SharedStackPileDecision::Decline)
                .expect("a decline at the start of a full stack is legal");
        }
        session
    }

    /// The same pod with its SECRETS permuted and every published count left
    /// alone: the main stack reversed, and the contents of every pile strictly
    /// above the cursor rotated between those piles (lengths preserved).
    fn permuted_secrets(session: &DraftSession) -> DraftSession {
        let mut clone = session.clone();
        let state = clone.shared_stack.as_mut().expect("a live pile turn");
        state.main_stack.reverse();
        let cursor = usize::from(state.cursor);
        let above: Vec<usize> = (cursor + 1..state.piles.len()).collect();
        assert!(
            above.len() >= 2,
            "the permutation needs at least two piles above the cursor"
        );
        let first = state.piles[above[0]].clone();
        for window in above.windows(2) {
            state.piles[window[0]] = state.piles[window[1]].clone();
        }
        let last = *above.last().expect("non-empty");
        state.piles[last] = first;
        clone
    }

    fn pile_signature(session: &DraftSession) -> Vec<Vec<String>> {
        stack_of(session)
            .piles
            .iter()
            .map(|pile| pile.iter().map(|c| c.instance_id.clone()).collect())
            .collect()
    }

    fn stack_top(session: &DraftSession) -> String {
        stack_of(session)
            .main_stack
            .last()
            .expect("a non-empty main stack")
            .instance_id
            .clone()
    }

    fn legality(
        take: Option<SharedStackRefusal>,
        decline: Option<SharedStackRefusal>,
    ) -> Vec<SharedStackDecisionView> {
        vec![
            SharedStackDecisionView {
                decision: SharedStackPileDecision::Take,
                refusal: take,
            },
            SharedStackDecisionView {
                decision: SharedStackPileDecision::Decline,
                refusal: decline,
            },
        ]
    }

    /// Every non-cursor pile answers `PileNotActive`, exactly as
    /// `shared_stack::refusal_for` does for one.
    fn idle_pile(index: u8, total: usize) -> SharedStackPileView {
        SharedStackPileView {
            index,
            total,
            revealed: Vec::new(),
            legality: legality(
                Some(SharedStackRefusal::PileNotActive),
                Some(SharedStackRefusal::PileNotActive),
            ),
        }
    }

    /// A hand-built projection: the cursor pile with its revealed prefix and its
    /// published verdicts, plus two later piles of a stated height.
    fn stack_view(
        revealed: Vec<DraftCardInstance>,
        verdicts: Vec<SharedStackDecisionView>,
        later_totals: [usize; 2],
        history: Vec<SharedStackDecisionRecord>,
    ) -> SharedStackView {
        let cursor = SharedStackPileView {
            index: 0,
            total: revealed.len(),
            revealed,
            legality: verdicts,
        };
        let total_cards = cursor.total + later_totals.iter().sum::<usize>();
        SharedStackView {
            main_stack_remaining: 40,
            total_cards: total_cards + 40,
            active_seat: 0,
            active_pile: 0,
            piles: vec![
                cursor,
                idle_pile(1, later_totals[0]),
                idle_pile(2, later_totals[1]),
            ],
            decisions: 0,
            history,
            // The bot never reads it: a forced draw is a fact about a card
            // already in a pool, and the bot decides from the table.
            forced_draw: None,
        }
    }

    /// A real projection with a hand-built shared stack swapped in, so the ~40
    /// other view fields are the engine's own rather than invented here.
    fn view_with(stack: SharedStackView) -> DraftPlayerView {
        let session = started_winston(2, 5, &[]);
        let mut view = filter_for_player(&session, stack_of(&session).active_seat);
        view.pool = Vec::new();
        view.shared_stack = Some(stack);
        view
    }

    fn both_legal() -> Vec<SharedStackDecisionView> {
        legality(None, None)
    }

    /// The same projection with its `piles` VECTOR reversed and every pile's
    /// `index` left alone.
    ///
    /// The engine publishes the two in agreement, so this shape is not
    /// reachable from `shared_stack_view` -- which is exactly why it is built by
    /// hand here. It is the only fixture that can tell "addressed by `index`"
    /// apart from "addressed by vector position", and both the cursor lookup and
    /// the colour read's clause 2 are specified as the former.
    fn reordered(mut stack: SharedStackView) -> SharedStackView {
        stack.piles.reverse();
        stack
    }

    // ── T-CHEAT-1a ──────────────────────────────────────────────────────────

    /// A FIXTURE-VALIDITY property, not a bot property: the projection really
    /// does hide what the next test varies. Without this, T-CHEAT-1b could pass
    /// because the two worlds were never actually different.
    #[test]
    fn the_projection_hides_what_the_bot_must_not_see() {
        let a = declined_one_turn(2, 13);
        let b = permuted_secrets(&a);
        let seat = stack_of(&a).active_seat;

        // The hidden halves REALLY differ.
        assert_ne!(
            a.shared_stack, b.shared_stack,
            "the permutation must change the hidden state"
        );
        assert_ne!(
            stack_top(&a),
            stack_top(&b),
            "the main stack's top must differ"
        );
        let (sig_a, sig_b) = (pile_signature(&a), pile_signature(&b));
        let cursor = usize::from(stack_of(&a).cursor);
        for index in cursor + 1..sig_a.len() {
            assert_ne!(
                sig_a[index], sig_b[index],
                "pile {index} above the cursor must really have been permuted"
            );
            assert_eq!(
                sig_a[index].len(),
                sig_b[index].len(),
                "the permutation must preserve every published height"
            );
        }

        // And the projection is IDENTICAL. `SharedStackView` derives
        // `PartialEq`, so this half compares directly; `DraftPlayerView` derives
        // only `Debug, Clone, Serialize, Deserialize`, so the whole-view leg
        // goes through `serde_json`.
        let view_a = filter_for_player(&a, seat);
        let view_b = filter_for_player(&b, seat);
        assert_eq!(view_a.shared_stack, view_b.shared_stack);
        assert_eq!(
            serde_json::to_value(&view_a).unwrap(),
            serde_json::to_value(&view_b).unwrap()
        );
    }

    // ── T-CHEAT-1b ──────────────────────────────────────────────────────────

    /// **The no-cheating test.** The view is built ONCE and the WORLD is varied
    /// underneath it: three different sessions are installed in the thread-local
    /// `bot_ai` can still name (`session_cell`'s `pub(crate)` accessors -- Rust
    /// has no visibility that would close that channel, so this test does), and
    /// the decision must not move.
    ///
    /// Session C is independently started from a different seed, so it differs
    /// from BOTH the fixture and its permutation in the main stack's top and in
    /// every pile -- a mutation that reads the installed session cannot survive
    /// by coincidence.
    ///
    /// **These three seeds are load-bearing and were CHOSEN, not picked.** The
    /// mutation this test exists to catch perturbs a score while the assertion
    /// is a discrete `Option<(u8, SharedStackPileDecision)>`, so whether a
    /// main-stack-derived term flips take-vs-decline is a property of the
    /// fixture. MEASURED: with seeds 11 / permuted(11) / 4242 the maximal
    /// channel-3 mutation ("if the hidden top card is the bomb, force
    /// `Decline`") stayed GREEN, because none of those three worlds has the
    /// bomb on top. Seeds 13 / permuted(13) / 5 put a bomb on exactly ONE of the
    /// three tops (`Test Filler` / `Test Bomb` / `Test Bear`) against an
    /// unmutated answer of `Take`, so the mutation reddens. Re-seeding this
    /// fixture without re-running that mutation would silently retire the test.
    #[test]
    fn the_bot_ignores_the_world_it_is_not_handed() {
        let a = declined_one_turn(2, 13);
        let b = permuted_secrets(&a);
        let c = declined_one_turn(2, 5);
        let db = fixture_card_db();

        // The worlds really are three, not two: pairwise distinct tops and
        // pairwise distinct piles.
        let tops = [stack_top(&a), stack_top(&b), stack_top(&c)];
        assert_ne!(tops[0], tops[1]);
        assert_ne!(tops[0], tops[2]);
        assert_ne!(tops[1], tops[2]);
        let signatures = [pile_signature(&a), pile_signature(&b), pile_signature(&c)];
        assert_ne!(signatures[0], signatures[1]);
        assert_ne!(signatures[0], signatures[2]);
        assert_ne!(signatures[1], signatures[2]);

        let view = filter_for_player(&a, stack_of(&a).active_seat);

        for rung in ALL_RUNGS {
            let mut decisions = Vec::new();
            let mut installed_tops = Vec::new();
            for world in [&a, &b, &c] {
                crate::session_cell::install(world.clone());
                // POSITIVE CONTROL that the seeding took: a test that silently
                // failed to install would pass vacuously.
                installed_tops.push(crate::session_cell::with_installed(|session| {
                    session
                        .shared_stack
                        .as_ref()
                        .and_then(|state| state.main_stack.last())
                        .map(|card| card.instance_id.clone())
                        .expect("the installed session has a main stack")
                }));
                decisions.push(winston_decision(&view, rung, Some(&db)));
            }
            crate::session_cell::clear();

            assert_eq!(
                installed_tops.len(),
                3,
                "three worlds must have been installed"
            );
            assert_ne!(installed_tops[0], installed_tops[1]);
            assert_ne!(installed_tops[0], installed_tops[2]);
            assert_ne!(installed_tops[1], installed_tops[2]);

            assert!(
                decisions[0].is_some(),
                "{rung:?} must produce a decision at all"
            );
            assert_eq!(
                decisions[0], decisions[1],
                "{rung:?} moved with a permuted world"
            );
            assert_eq!(
                decisions[0], decisions[2],
                "{rung:?} moved with a third, independent world"
            );
        }
    }

    // ── T-CHEAT-2 ───────────────────────────────────────────────────────────

    /// The mandatory paired positive: the bot DOES move when a field it is
    /// allowed to read moves. Without it, T-CHEAT-1b is satisfied by a constant.
    ///
    /// The two views differ in the cursor pile's published `revealed` CONTENTS
    /// and in nothing else -- same height, same legality, same later piles --
    /// which is stricter than differing in size as well.
    #[test]
    fn bot_does_move_when_a_published_field_moves() {
        let db = fixture_card_db();
        let premium = view_with(stack_view(
            vec![fixture_card(REMOVAL, "premium")],
            both_legal(),
            [1, 1],
            Vec::new(),
        ));
        let dross = view_with(stack_view(
            vec![fixture_card(FILLER, "dross")],
            both_legal(),
            [1, 1],
            Vec::new(),
        ));

        let on_premium = winston_decision(&premium, AiDifficulty::Medium, Some(&db));
        let on_dross = winston_decision(&dross, AiDifficulty::Medium, Some(&db));
        assert_eq!(
            on_premium,
            Some((0, SharedStackPileDecision::Take)),
            "a cheap removal spell is worth taking"
        );
        assert_eq!(
            on_dross,
            Some((0, SharedStackPileDecision::Decline)),
            "one sub-replacement common is not"
        );
        assert_ne!(on_premium, on_dross);
    }

    // ── Legality ────────────────────────────────────────────────────────────

    /// The published vector is the authority, and the bot obeys it even when it
    /// would rather do the other thing. Both fixtures are built so the SCORE
    /// prefers the refused decision -- a bot that re-derived legality, or that
    /// checked its preference after choosing, fails both legs.
    #[test]
    fn winston_decision_obeys_the_published_legality_vector() {
        let db = fixture_card_db();

        // A pile the bot would love to take, published as untakeable.
        let take_refused = view_with(stack_view(
            vec![
                fixture_card(REMOVAL, "a"),
                fixture_card(BOMB, "b"),
                fixture_card(REMOVAL, "c"),
            ],
            legality(Some(SharedStackRefusal::PileEmpty), None),
            [1, 1],
            Vec::new(),
        ));
        // A pile the bot would rather decline (two nine-card piles ahead of it),
        // published as undeclinable.
        let decline_refused = view_with(stack_view(
            vec![fixture_card(FILLER, "d")],
            legality(None, Some(SharedStackRefusal::NoGuaranteedCard)),
            [9, 9],
            Vec::new(),
        ));

        for rung in ALL_RUNGS {
            assert_eq!(
                winston_decision(&take_refused, rung, Some(&db)),
                Some((0, SharedStackPileDecision::Decline)),
                "{rung:?} must decline when the take is refused"
            );
            assert_eq!(
                winston_decision(&decline_refused, rung, Some(&db)),
                Some((0, SharedStackPileDecision::Take)),
                "{rung:?} must take when the decline is refused"
            );
        }

        // The preference really did point the other way, so each leg above is a
        // refusal being obeyed rather than a coincidence.
        let mut both = take_refused.clone();
        both.shared_stack.as_mut().unwrap().piles[0].legality = both_legal();
        assert_eq!(
            winston_decision(&both, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Take))
        );
        let mut both = decline_refused.clone();
        both.shared_stack.as_mut().unwrap().piles[0].legality = both_legal();
        assert_eq!(
            winston_decision(&both, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Decline))
        );

        // The cursor pile is found by `index`, never by vector position: with
        // the piles vector reversed, position 0 is pile 2 (which publishes
        // `PileNotActive` for both decisions) while the cursor is still pile 0.
        let reversed = view_with(reordered(stack_view(
            vec![fixture_card(REMOVAL, "f")],
            both_legal(),
            [1, 1],
            Vec::new(),
        )));
        for rung in ALL_RUNGS {
            assert_eq!(
                winston_decision(&reversed, rung, Some(&db)),
                Some((0, SharedStackPileDecision::Take)),
                "{rung:?} must address the cursor pile by index"
            );
        }

        // No legal decision at all is `None`, never an illegal guess.
        let stuck = view_with(stack_view(
            vec![fixture_card(FILLER, "e")],
            legality(
                Some(SharedStackRefusal::PileEmpty),
                Some(SharedStackRefusal::NoGuaranteedCard),
            ),
            [0, 0],
            Vec::new(),
        ));
        for rung in ALL_RUNGS {
            assert_eq!(winston_decision(&stuck, rung, Some(&db)), None, "{rung:?}");
        }
    }

    /// Drive a whole seeded draft through the bot and the REAL reducer: every
    /// decision it returns applies without an `Err`.
    ///
    /// Reach-guards, because a take-only walk would pass this vacuously: both
    /// decisions must occur, and the endgame (`main_stack_remaining` down to
    /// 2/1/0, where the decline adjudication changes) must be reached.
    #[test]
    fn winston_decision_never_returns_a_refused_decision() {
        let db = fixture_card_db();
        let mut session = started_winston(2, 7, &[]);
        let mut takes = 0usize;
        let mut declines = 0usize;
        let mut min_remaining = usize::MAX;
        let mut applied = 0usize;

        while session.status == DraftStatus::Drafting {
            let seat = stack_of(&session).active_seat;
            let view = filter_for_player(&session, seat);
            min_remaining = min_remaining.min(
                view.shared_stack
                    .as_ref()
                    .expect("a live pile turn")
                    .main_stack_remaining,
            );
            let (pile, decision) = winston_decision(&view, AiDifficulty::Medium, Some(&db))
                .expect("the active seat always has a legal decision while drafting");
            match decision {
                SharedStackPileDecision::Take => takes += 1,
                SharedStackPileDecision::Decline => declines += 1,
            }
            session::apply(
                &mut session,
                DraftAction::SharedStackDecision {
                    seat,
                    pile,
                    decision,
                },
                None,
            )
            .unwrap_or_else(|e| panic!("the reducer refused the bot's decision: {e}"));
            applied += 1;
            assert!(applied < 1000, "the walk must terminate");
        }

        assert_eq!(session.status, DraftStatus::Deckbuilding);
        assert!(takes > 0, "a walk that never takes proves nothing");
        assert!(declines > 0, "a walk that never declines proves nothing");
        assert!(
            min_remaining <= 2,
            "the endgame was never reached (min remaining {min_remaining})"
        );
    }

    /// Without a card database the bot still decides, still legally, and is
    /// still MOVED by the terms that survive the degradation.
    ///
    /// A correction to the plan's wording, measured here rather than assumed:
    /// with no face, a card's whole value is `rarity_prior` (<= 1.5) plus the
    /// colour ramp (<= `color_commitment_max`, 1.5) plus the pass bonus, which
    /// cannot reach `replacement_level` (3.0) in any ordinary position -- so
    /// `playables` is identically zero and it is the DENIAL term that carries
    /// the decision. That is what the paired positive below pins.
    #[test]
    fn winston_decision_degrades_without_a_card_database() {
        let db = fixture_card_db();

        // The fixing and interaction terms are provably INERT without a face:
        // a dual land and a filler creature decide the same way. With a
        // database they decide differently -- the paired positive that makes
        // this an inertness claim rather than a constant.
        let fixing = view_with(stack_view(
            vec![fixture_card(DUAL_LAND, "land")],
            both_legal(),
            [1, 1],
            Vec::new(),
        ));
        let vanilla = view_with(stack_view(
            vec![fixture_card(FILLER, "filler")],
            both_legal(),
            [1, 1],
            Vec::new(),
        ));
        assert_eq!(
            winston_decision(&fixing, AiDifficulty::Medium, None),
            winston_decision(&vanilla, AiDifficulty::Medium, None),
            "with no face there is no fixing premium to tell these apart"
        );
        assert_ne!(
            winston_decision(&fixing, AiDifficulty::Medium, Some(&db)),
            winston_decision(&vanilla, AiDifficulty::Medium, Some(&db)),
            "with a database the fixing premium must tell them apart"
        );

        // The denial term still moves it: three cards are worth taking where
        // one is not, with no database on either side.
        let one = view_with(stack_view(
            vec![fixture_card(FILLER, "one")],
            both_legal(),
            [1, 1],
            Vec::new(),
        ));
        let three = view_with(stack_view(
            vec![
                fixture_card(FILLER, "a"),
                fixture_card(FILLER, "b"),
                fixture_card(FILLER, "c"),
            ],
            both_legal(),
            [1, 1],
            Vec::new(),
        ));
        assert_eq!(
            winston_decision(&one, AiDifficulty::Medium, None),
            Some((0, SharedStackPileDecision::Decline))
        );
        assert_eq!(
            winston_decision(&three, AiDifficulty::Medium, None),
            Some((0, SharedStackPileDecision::Take)),
            "denial must still be able to carry a decision with no database"
        );

        // And a whole walk with no database stays legal and stays live.
        let mut session = started_winston(2, 13, &[]);
        let mut takes = 0usize;
        let mut declines = 0usize;
        let mut applied = 0usize;
        while session.status == DraftStatus::Drafting {
            let seat = stack_of(&session).active_seat;
            let view = filter_for_player(&session, seat);
            let (pile, decision) = winston_decision(&view, AiDifficulty::Medium, None)
                .expect("a decision is produced without a card database");
            match decision {
                SharedStackPileDecision::Take => takes += 1,
                SharedStackPileDecision::Decline => declines += 1,
            }
            session::apply(
                &mut session,
                DraftAction::SharedStackDecision {
                    seat,
                    pile,
                    decision,
                },
                None,
            )
            .unwrap_or_else(|e| panic!("the reducer refused a database-less decision: {e}"));
            applied += 1;
            assert!(applied < 1000, "the walk must terminate");
        }
        assert_eq!(session.status, DraftStatus::Deckbuilding);
        assert!(takes > 0 && declines > 0, "both decisions must still occur");
    }

    // ── P4a, the `bot_ai` leg ───────────────────────────────────────────────

    fn record(
        seat: u8,
        pile: u8,
        decision: SharedStackPileDecision,
        pile_size: usize,
    ) -> SharedStackDecisionRecord {
        SharedStackDecisionRecord {
            seat,
            pile,
            decision,
            pile_size,
        }
    }

    /// The pile-size read folds OTHER seats only, and smooths a single
    /// observation off certainty.
    #[test]
    fn opponent_read_folds_only_other_seats() {
        let split = 3;
        // One observation: seat 1 took a five-card pile.
        let stack = stack_view(
            Vec::new(),
            both_legal(),
            [1, 1],
            vec![record(1, 0, SharedStackPileDecision::Take, 5)],
        );
        let read = opponent_read(&stack, 0, split);
        assert_eq!(read.samples, 1);
        // Laplace: 2/3, NOT 1.0 -- one observation is not certainty.
        assert!(
            (read.large_pile_take_rate - 2.0 / 3.0).abs() < 1e-9,
            "large rate was {}",
            read.large_pile_take_rate
        );
        // No small-pile observation at all: the prior, not a zero.
        assert!((read.small_pile_take_rate - 0.5).abs() < 1e-9);

        // The bot's OWN decisions change nothing, however many there are.
        let mut with_own = stack.clone();
        for _ in 0..4 {
            with_own
                .history
                .push(record(0, 0, SharedStackPileDecision::Take, 5));
        }
        let own = opponent_read(&with_own, 0, split);
        assert_eq!(own, read, "the bot must not fold itself into its own read");

        // The two buckets are genuinely separate: an opponent that takes big
        // piles and declines small ones reads differently on each axis.
        let mut greedy = stack_view(Vec::new(), both_legal(), [1, 1], Vec::new());
        for _ in 0..5 {
            greedy
                .history
                .push(record(1, 0, SharedStackPileDecision::Take, 6));
            greedy
                .history
                .push(record(1, 0, SharedStackPileDecision::Decline, 1));
        }
        let greedy_read = opponent_read(&greedy, 0, split);
        assert_eq!(greedy_read.samples, 10);
        assert!(
            greedy_read.large_pile_take_rate > greedy_read.small_pile_take_rate,
            "large {} small {}",
            greedy_read.large_pile_take_rate,
            greedy_read.small_pile_take_rate
        );
        assert!(greedy_read.large_pile_take_rate > 0.8);
        assert!(greedy_read.small_pile_take_rate < 0.2);
        // The split is a parameter, not a constant: read at a higher split, the
        // six-card piles are still large but the same history re-buckets.
        let wide = opponent_read(&greedy, 0, 7);
        assert!(wide.large_pile_take_rate < greedy_read.large_pile_take_rate);
    }

    // ── P4c-fold ────────────────────────────────────────────────────────────

    /// The colour read's HOSTILE fixture: the reconstruction rule must never
    /// invent a card.
    ///
    /// A pile that was taken after the opponent declined it has been emptied and
    /// refilled, so the published prefix is no longer the cards they looked at.
    /// Dropping that clause is exactly the mutation that would produce false
    /// positives (MEASURED at zero with it).
    #[test]
    fn opponent_pass_tally_skips_disturbed_prefixes() {
        let revealed = vec![
            fixture_card(BEAR, "w1"),
            fixture_card(REMOVAL, "w2"),
            fixture_card(BOMB, "u1"),
            fixture_card(FILLER, "g1"),
        ];

        // NEGATIVE: a decline, then somebody takes that pile. Nothing survives.
        let disturbed = stack_view(
            revealed.clone(),
            both_legal(),
            [1, 1],
            vec![
                record(1, 0, SharedStackPileDecision::Decline, 4),
                record(0, 0, SharedStackPileDecision::Take, 4),
            ],
        );
        assert_eq!(
            opponent_read(&disturbed, 0, 3).passed_colors.cards(),
            0,
            "a taken pile's prefix is not what the opponent looked at"
        );

        // PAIRED POSITIVE: the same fixture without the take contributes
        // exactly the four-card prefix, by colour.
        let intact = stack_view(
            revealed.clone(),
            both_legal(),
            [1, 1],
            vec![record(1, 0, SharedStackPileDecision::Decline, 4)],
        );
        let tally = opponent_read(&intact, 0, 3).passed_colors;
        assert_eq!(tally.cards(), 4);
        // Two white, one blue, one green, Laplace-smoothed over five colours.
        assert!((tally.pass_share("W") - 3.0 / 9.0).abs() < 1e-9);
        assert!((tally.pass_share("U") - 2.0 / 9.0).abs() < 1e-9);
        assert!((tally.pass_share("G") - 2.0 / 9.0).abs() < 1e-9);
        assert!((tally.pass_share("B") - 1.0 / 9.0).abs() < 1e-9);

        // A LATER decline on the same pile does not invalidate: declines only
        // append, so the prefix survives.
        let mut redeclined = intact.clone();
        redeclined
            .history
            .push(record(1, 0, SharedStackPileDecision::Decline, 4));
        assert_eq!(
            opponent_read(&redeclined, 0, 3).passed_colors.cards(),
            8,
            "two declines on an undisturbed pile are two passing events"
        );

        // OWN SEAT: the bot's own declines are not an opponent read.
        let mine = stack_view(
            revealed.clone(),
            both_legal(),
            [1, 1],
            vec![record(0, 0, SharedStackPileDecision::Decline, 4)],
        );
        assert_eq!(opponent_read(&mine, 0, 3).passed_colors.cards(), 0);

        // TRUNCATION: a `pile_size` beyond the published prefix truncates
        // rather than panicking.
        let overlong = stack_view(
            revealed.clone(),
            both_legal(),
            [1, 1],
            vec![record(1, 0, SharedStackPileDecision::Decline, 99)],
        );
        assert_eq!(opponent_read(&overlong, 0, 3).passed_colors.cards(), 4);

        // ADDRESSED BY `index`, never by vector position: a record naming pile 2
        // reads pile 2's own prefix, and pile 2 publishes none.
        let elsewhere = stack_view(
            revealed.clone(),
            both_legal(),
            [1, 1],
            vec![record(1, 2, SharedStackPileDecision::Decline, 4)],
        );
        assert_eq!(opponent_read(&elsewhere, 0, 3).passed_colors.cards(), 0);

        // The same claim from the other side, on the one fixture that can tell
        // the two spellings apart: with the piles vector reversed, a record
        // naming pile 0 must still read pile 0's prefix (four cards) and not
        // whatever sits at position 0 (pile 2, which publishes none).
        let reversed = reordered(stack_view(
            revealed,
            both_legal(),
            [1, 1],
            vec![record(1, 0, SharedStackPileDecision::Decline, 4)],
        ));
        assert_eq!(opponent_read(&reversed, 0, 3).passed_colors.cards(), 4);
    }

    /// CR 903.13b: a bot in a two-card pod must return two usable indices.
    ///
    /// A direct unit test of a pre-wired helper: `count > 1` is not reachable
    /// from any production path in this phase, because the wasm bot loop is
    /// `Quick`-gated and `Quick` has `cards_per_pick == 1`. Saying so is more
    /// useful than implying production coverage.
    #[test]
    fn bot_picks_returns_n_distinct_indices() {
        let cards = pack(14);
        let mut rng = ChaCha20Rng::seed_from_u64(42);

        let picked = bot_picks(&cards, 2, AiDifficulty::Medium, &[], None, &mut rng);
        assert_eq!(picked.len(), 2);
        assert_ne!(picked[0], picked[1], "a bot cannot draft one card twice");
        assert!(picked.iter().all(|index| *index < cards.len()));

        // Clamped: a count larger than the pack yields every index exactly once.
        let small = pack(1);
        let clamped = bot_picks(&small, 2, AiDifficulty::Medium, &[], None, &mut rng);
        assert_eq!(clamped, vec![0]);

        // Degenerate counts.
        assert!(bot_picks(&cards, 0, AiDifficulty::Medium, &[], None, &mut rng).is_empty());
        assert!(bot_picks(&[], 2, AiDifficulty::Medium, &[], None, &mut rng).is_empty());

        // The single-card case must still agree with `bot_pick` itself, since
        // that is the path every existing kind takes.
        let mut rng_a = ChaCha20Rng::seed_from_u64(7);
        let mut rng_b = ChaCha20Rng::seed_from_u64(7);
        assert_eq!(
            bot_picks(&cards, 1, AiDifficulty::Medium, &[], None, &mut rng_a),
            vec![bot_pick(
                &cards,
                AiDifficulty::Medium,
                &[],
                None,
                &mut rng_b
            )]
        );
    }

    // ── The published inputs, joined to the decision ─────────────────────────
    //
    // `phase_ai::winston_eval` pins every weight and `opponent_read` pins the
    // fold, but a test on each END of a wire is not a test of the wire. The six
    // tests below each hold every other published field fixed and move ONE, and
    // each one's fixture is chosen so the returned `SharedStackPileDecision`
    // FLIPS -- which is what makes discarding that input at the `WinstonTurn`
    // construction a red test rather than a silent disconnection.
    //
    // Every band below was MEASURED, not estimated: the per-card numbers under
    // `WinstonWeights::baseline()` are dual land 4.9 / bear 3.5 / filler 1.625 /
    // removal 7.4 / bomb 8.75, against a `replacement_level` of 3.0, so the
    // surpluses are 1.9 / 0.5 / 0.0 / 4.4 / 5.75.

    /// The published card counts restated, so a fixture that needs a specific
    /// `draft_progress` says so in one place instead of leaving
    /// `stack_view`'s 40-card default to imply it.
    fn with_totals(
        mut stack: SharedStackView,
        main_stack_remaining: usize,
        total_cards: usize,
    ) -> SharedStackView {
        stack.main_stack_remaining = main_stack_remaining;
        stack.total_cards = total_cards;
        stack
    }

    fn with_pool(mut view: DraftPlayerView, pool: Vec<DraftCardInstance>) -> DraftPlayerView {
        view.pool = pool;
        view
    }

    /// The same printing in a different colour. Colour is a property of the
    /// INSTANCE (`DraftCardInstance.colors`), not of the face, so this varies
    /// the one published axis the colour terms read while holding card quality,
    /// rarity and mana value exactly fixed.
    fn recolored(mut card: DraftCardInstance, colors: &[&str]) -> DraftCardInstance {
        card.colors = colors.iter().map(|c| (*c).to_string()).collect();
        card
    }

    fn pile_of(name: &str, count: usize) -> Vec<DraftCardInstance> {
        (0..count)
            .map(|i| fixture_card(name, &format!("{name}-{i}")))
            .collect()
    }

    /// A projection whose cursor is the LAST pile, which is the only shape in
    /// which `later_pile_sizes` is empty and the forced draw off the main stack
    /// is a stopping point at all.
    fn final_pile_view(
        revealed: Vec<DraftCardInstance>,
        verdicts: Vec<SharedStackDecisionView>,
        earlier_totals: [usize; 2],
    ) -> SharedStackView {
        let cursor = SharedStackPileView {
            index: 2,
            total: revealed.len(),
            revealed,
            legality: verdicts,
        };
        let total_cards = cursor.total + earlier_totals.iter().sum::<usize>() + 40;
        SharedStackView {
            main_stack_remaining: 40,
            total_cards,
            active_seat: 0,
            active_pile: 2,
            piles: vec![
                idle_pile(0, earlier_totals[0]),
                idle_pile(1, earlier_totals[1]),
                cursor,
            ],
            decisions: 0,
            history: Vec::new(),
            forced_draw: None,
        }
    }

    /// **Principle 4, first half, joined.** `OpponentRead`'s take rates reach the
    /// decision through `handoff_cost`, and nothing but this test would notice if
    /// they stopped.
    ///
    /// Both legs publish the SAME twenty observations by the same seat on the
    /// same pile size, so `samples`, the bucket and every other term are equal;
    /// only the DECISION in those records differs. A seat that keeps taking big
    /// piles makes each decline expensive (cost 0.739), a seat that keeps
    /// declining them makes it cheap (0.511), and the two-decline stopping point
    /// doubles the difference into 3.189 versus 3.644 against a `take_now` of
    /// 3.5.
    ///
    /// The passive leg's records name pile 1, whose published `revealed` is
    /// empty, so neither leg's colour tally sees anything -- this test moves the
    /// rates and only the rates.
    ///
    /// MUTATION (applied, observed red, reverted): `read: &OpponentRead::default()`
    /// at the `WinstonTurn` construction takes `samples` to 0, `handoff_cost`
    /// falls back to the flat 0.5 penalty, and the greedy leg's `Take` becomes
    /// `Decline`.
    #[test]
    fn winston_decision_prices_the_opponents_pile_appetite() {
        let db = fixture_card_db();
        // Five fillers (surplus 0.0) and one bear (0.5): playables 0.5 plus
        // denial 3.0 is a `take_now` of 3.5, and the six-card sample is exactly
        // `MIN_SURPLUS_SAMPLE`, so `expected` is the measured 0.0833 rather than
        // the prior.
        let cursor = || {
            let mut cards = pile_of(FILLER, 5);
            cards.push(fixture_card(BEAR, "bear"));
            cards
        };
        let history = |decision| {
            (0..20)
                .map(|_| record(1, 1, decision, 6))
                .collect::<Vec<_>>()
        };

        let greedy = view_with(stack_view(
            cursor(),
            both_legal(),
            [1, 8],
            history(SharedStackPileDecision::Take),
        ));
        let passive = view_with(stack_view(
            cursor(),
            both_legal(),
            [1, 8],
            history(SharedStackPileDecision::Decline),
        ));

        // FIXTURE VALIDITY: the two histories really do read differently, and
        // neither of them says anything about colour.
        let greedy_read = opponent_read(
            greedy.shared_stack.as_ref().unwrap(),
            0,
            phase_ai::winston_eval::WinstonWeights::baseline().opponent_size_split,
        );
        let passive_read = opponent_read(
            passive.shared_stack.as_ref().unwrap(),
            0,
            phase_ai::winston_eval::WinstonWeights::baseline().opponent_size_split,
        );
        assert_eq!(greedy_read.samples, passive_read.samples);
        assert!(greedy_read.large_pile_take_rate > 0.9);
        assert!(passive_read.large_pile_take_rate < 0.1);
        assert_eq!(greedy_read.passed_colors.cards(), 0);
        assert_eq!(passive_read.passed_colors.cards(), 0);

        assert_eq!(
            winston_decision(&greedy, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Take)),
            "handing a fat pile to a seat that takes fat piles is expensive, so take now"
        );
        assert_eq!(
            winston_decision(&passive, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Decline)),
            "against a seat that keeps declining, the same decline is cheap enough to make"
        );
    }

    /// **Principle 4, second half, joined.** The `ColorPassTally` folded out of
    /// the published history reaches the decision through
    /// `CardContext::passed`, and this is the test that would notice if it
    /// stopped.
    ///
    /// Both legs publish TWO declines by seat 1 of a four-card pile, so
    /// `samples`, both take rates and `handoff_cost` are identical. They differ
    /// in the pile those declines NAME: pile 0 is the cursor, whose four white
    /// cards are published to this viewer, so eight white observations land in
    /// the tally (share 9/13 against a uniform 0.2); pile 1 publishes no
    /// `revealed` at all, so the same two declines teach nothing.
    ///
    /// MUTATION (applied, observed red, reverted):
    /// `passed: &ColorPassTally::default()` at the `CardContext` construction
    /// turns the open-colour leg's `Take` into `Decline`. Note, MEASURED, that
    /// `read: &OpponentRead::default()` alone does NOT redden this one and
    /// reddens the pile-appetite test instead: the tally reaches the decision
    /// through `CardContext::passed`, which is a second wire off the same fold,
    /// so the two halves of principle 4 need the two tests they have.
    #[test]
    fn winston_decision_prices_the_colors_the_opponent_passes() {
        let db = fixture_card_db();
        let cursor = || pile_of(BEAR, 4);

        let open = view_with(stack_view(
            cursor(),
            both_legal(),
            [1, 4],
            vec![
                record(1, 0, SharedStackPileDecision::Decline, 4),
                record(1, 0, SharedStackPileDecision::Decline, 4),
            ],
        ));
        let unread = view_with(stack_view(
            cursor(),
            both_legal(),
            [1, 4],
            vec![
                record(1, 1, SharedStackPileDecision::Decline, 4),
                record(1, 1, SharedStackPileDecision::Decline, 4),
            ],
        ));

        // FIXTURE VALIDITY: the pile-size half is held FIXED across the two
        // legs, and only the colour tally moves -- and it clears
        // `MIN_PASS_SAMPLE`, without which the term is gated to exactly 0.0 and
        // this test would be pinning nothing.
        let split = phase_ai::winston_eval::WinstonWeights::baseline().opponent_size_split;
        let open_read = opponent_read(open.shared_stack.as_ref().unwrap(), 0, split);
        let unread_read = opponent_read(unread.shared_stack.as_ref().unwrap(), 0, split);
        assert_eq!(open_read.samples, unread_read.samples);
        assert_eq!(
            open_read.large_pile_take_rate,
            unread_read.large_pile_take_rate
        );
        assert_eq!(
            open_read.small_pile_take_rate,
            unread_read.small_pile_take_rate
        );
        assert!(open_read.passed_colors.cards() >= phase_ai::winston_eval::MIN_PASS_SAMPLE);
        assert_eq!(unread_read.passed_colors.cards(), 0);

        assert_eq!(
            winston_decision(&open, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Take)),
            "white is the colour they keep passing, so this white pile is worth more"
        );
        assert_eq!(
            winston_decision(&unread, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Decline)),
            "the identical pile with no colour read is not worth taking"
        );
    }

    /// **The bot's own pool is its estimate of an average card**, and that
    /// estimate is what prices every pile it cannot see into.
    ///
    /// Both legs hold the cursor pile, the pile heights, the history and the
    /// pool SIZE fixed; only the pool's contents differ. Six bombs put the
    /// measured mean surplus at 5.2, so an unseen pile is worth more than the
    /// dual land on the table; six fillers put it at 0.27, so it is worth far
    /// less.
    ///
    /// The cursor card is deliberately COLOURLESS and `draft_progress` is
    /// asserted below `color_commitment_start`, so neither the colour ramp nor
    /// the pass bonus can be what moved this -- the two pools differ in colour
    /// as well as in quality, and this is what makes that difference inert.
    ///
    /// MUTATION (applied, observed red, reverted): `pool: &[]` drops the sample
    /// below `MIN_SURPLUS_SAMPLE`, `expected` falls back to
    /// `prior_surplus_per_card`, and the bomb-pool leg's `Decline` becomes
    /// `Take`.
    #[test]
    fn winston_decision_prices_unseen_piles_off_its_own_pool() {
        let db = fixture_card_db();
        let stack = || {
            stack_view(
                vec![fixture_card(DUAL_LAND, "cursor")],
                both_legal(),
                [1, 1],
                Vec::new(),
            )
        };
        let rich = with_pool(view_with(stack()), pile_of(BOMB, 6));
        let poor = with_pool(view_with(stack()), pile_of(FILLER, 6));

        // FIXTURE VALIDITY: the colour ramp is provably inert at this progress,
        // so the two pools' different colours cannot be the mover.
        let progress = draft_progress(
            rich.pool.len(),
            rich.shared_stack.as_ref().unwrap().total_cards,
            rich.seats.len(),
        );
        assert!(
            progress < phase_ai::winston_eval::WinstonWeights::baseline().color_commitment_start,
            "progress {progress} must sit below the colour ramp's start"
        );
        assert_eq!(rich.pool.len(), poor.pool.len());

        assert_eq!(
            winston_decision(&rich, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Decline)),
            "a bot whose own pool says cards are worth 5.2 expects better than a dual land"
        );
        assert_eq!(
            winston_decision(&poor, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Take)),
            "a bot whose pool says cards are worth 0.27 takes the dual land"
        );
    }

    /// **Principle 2, joined.** The colour ramp is keyed on `draft_progress`,
    /// and `draft_progress` is computed from published counts.
    ///
    /// The two views are identical in every field except the published card
    /// counts: one is early (progress 0.375, ramp exactly 0.0) and one is late
    /// (progress 0.833, ramp 1.0). The pool is thirty white cards whose surplus
    /// stays FLOORED AT ZERO on both legs -- `card_surplus` clamps at
    /// `replacement_level`, and a 1.625-value filler plus a 1.0 ramp is still
    /// 2.625 -- so the ramp moves `take_now` without dragging the continuation's
    /// `expected` up with it. That is what makes the flip attributable to the
    /// ramp rather than to the sample.
    ///
    /// MUTATION (applied, observed red, reverted): `progress: 0.0` at the
    /// `CardContext` construction makes the late leg score exactly like the
    /// early one, and its `Take` becomes `Decline`.
    #[test]
    fn winston_decision_commits_to_a_color_only_late_in_the_draft() {
        let db = fixture_card_db();
        let pool: Vec<DraftCardInstance> = pile_of(FILLER, 30)
            .into_iter()
            .map(|card| recolored(card, &["W"]))
            .collect();
        let stack = |main_stack_remaining, total_cards| {
            with_totals(
                stack_view(
                    vec![fixture_card(BEAR, "cursor")],
                    both_legal(),
                    [4, 4],
                    Vec::new(),
                ),
                main_stack_remaining,
                total_cards,
            )
        };
        let early = with_pool(view_with(stack(91, 100)), pool.clone());
        let late = with_pool(view_with(stack(3, 12)), pool.clone());

        // FIXTURE VALIDITY: the two legs really do straddle the ramp's start,
        // and they differ in nothing else the decision reads.
        let weights = phase_ai::winston_eval::WinstonWeights::baseline();
        let early_progress = draft_progress(pool.len(), 100, early.seats.len());
        let late_progress = draft_progress(pool.len(), 12, late.seats.len());
        assert!(
            early_progress < weights.color_commitment_start,
            "early progress {early_progress}"
        );
        assert!(
            late_progress > weights.color_commitment_start,
            "late progress {late_progress}"
        );
        assert_eq!(
            draft_eval::dominant_colors(
                &pool.iter().map(|c| c.colors.as_slice()).collect::<Vec<_>>(),
                MIN_POOL_COLOR_SAMPLE
            ),
            vec!["W".to_string()],
        );

        assert_eq!(
            winston_decision(&early, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Decline)),
            "early, an on-colour bear is just a bear and the piles ahead are bigger"
        );
        assert_eq!(
            winston_decision(&late, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Take)),
            "late, the same bear in the bot's own colour is worth taking"
        );
    }

    /// **The bot's colours come off its own published pool**, and a card is
    /// on-colour or not by the colours the projection prints on it.
    ///
    /// Both legs are the LATE fixture above -- same pool, same counts, same
    /// progress -- and the cursor card is the same printing at the same rarity
    /// and mana value. Only its published `colors` differ.
    ///
    /// MUTATION (applied, observed red, reverted): `preferred_colors: &[]` at
    /// the `CardContext` construction makes no card on-colour, and the white
    /// leg's `Take` becomes `Decline`.
    #[test]
    fn winston_decision_reads_its_colors_off_its_own_pool() {
        let db = fixture_card_db();
        let pool: Vec<DraftCardInstance> = pile_of(FILLER, 30)
            .into_iter()
            .map(|card| recolored(card, &["W"]))
            .collect();
        let stack = |cursor: DraftCardInstance| {
            with_totals(
                stack_view(vec![cursor], both_legal(), [4, 4], Vec::new()),
                3,
                12,
            )
        };
        let on_color = with_pool(
            view_with(stack(recolored(fixture_card(BEAR, "cursor"), &["W"]))),
            pool.clone(),
        );
        let off_color = with_pool(
            view_with(stack(recolored(fixture_card(BEAR, "cursor"), &["G"]))),
            pool.clone(),
        );

        // FIXTURE VALIDITY: one published axis differs, and it is the colour.
        assert_eq!(
            on_color.shared_stack.as_ref().unwrap().piles[0].revealed[0].name,
            off_color.shared_stack.as_ref().unwrap().piles[0].revealed[0].name
        );
        assert_ne!(
            on_color.shared_stack.as_ref().unwrap().piles[0].revealed[0].colors,
            off_color.shared_stack.as_ref().unwrap().piles[0].revealed[0].colors
        );

        assert_eq!(
            winston_decision(&on_color, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Take)),
        );
        assert_eq!(
            winston_decision(&off_color, AiDifficulty::Medium, Some(&db)),
            Some((0, SharedStackPileDecision::Decline)),
            "the same card out of the bot's colours is not worth the same"
        );
    }

    /// **The forced draw is a stopping point only when the projection publishes
    /// that decline as legal**, and pricing it is what makes a thin last pile
    /// declinable at all.
    ///
    /// This is the one fixture shape in which `later_pile_sizes` is empty: the
    /// cursor is the LAST pile, so the only continuation left is declining onto
    /// the main stack. With the decline published legal the continuation is
    /// worth 1.0 against a `take_now` of 0.5, so the bot declines; the paired
    /// positive shows the comparison is live rather than a constant, and the
    /// paired negative shows the published refusal is obeyed.
    ///
    /// MUTATION (applied, observed red, reverted): `forced_draw_legal: false` at
    /// the `WinstonTurn` construction empties the stopping-point iterator,
    /// `continuation_value` returns `NEG_INFINITY`, and the thin-pile leg's
    /// `Decline` becomes `Take`.
    #[test]
    fn winston_decision_prices_the_forced_draw_when_it_is_published_legal() {
        let db = fixture_card_db();

        let thin = view_with(final_pile_view(
            vec![fixture_card(FILLER, "thin")],
            both_legal(),
            [1, 1],
        ));
        // FIXTURE VALIDITY: the cursor really is the last pile, so there is no
        // later pile for the continuation to price instead.
        let stack = thin.shared_stack.as_ref().unwrap();
        assert!(stack
            .piles
            .iter()
            .all(|pile| pile.index <= stack.active_pile));

        assert_eq!(
            winston_decision(&thin, AiDifficulty::Medium, Some(&db)),
            Some((2, SharedStackPileDecision::Decline)),
            "one sub-replacement common is worth less than a card off the main stack"
        );

        // PAIRED POSITIVE: the comparison is live -- a pile worth more than the
        // forced draw is taken even though the forced draw is equally legal.
        let fat = view_with(final_pile_view(
            vec![fixture_card(REMOVAL, "fat")],
            both_legal(),
            [1, 1],
        ));
        assert_eq!(
            winston_decision(&fat, AiDifficulty::Medium, Some(&db)),
            Some((2, SharedStackPileDecision::Take)),
        );

        // PAIRED NEGATIVE: the same thin pile with the forced draw published as
        // REFUSED. There is no continuation to price and no legal decline to
        // return.
        let refused = view_with(final_pile_view(
            vec![fixture_card(FILLER, "thin")],
            legality(None, Some(SharedStackRefusal::NoGuaranteedCard)),
            [1, 1],
        ));
        assert_eq!(
            winston_decision(&refused, AiDifficulty::Medium, Some(&db)),
            Some((2, SharedStackPileDecision::Take)),
        );
    }
}

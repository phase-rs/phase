//! Winston Draft valuation: what a *pile* is worth, and whether taking it beats
//! the best continuation of this turn.
//!
//! This module is the **scoring half** of a Winston Draft bot. It makes no
//! decisions and knows nothing about sessions, seats, actions or legality — it
//! takes plain card facts ([`DraftCardFacts`]) and pile heights and returns
//! numbers. The decision layer that consumes it lives in `draft-wasm::bot_ai`,
//! which is the only place `SharedStackPileDecision` and the draft projection
//! are both in scope.
//!
//! # No Comprehensive Rules annotation, deliberately
//!
//! Winston Draft has **no CR section** — grep-verified: `docs/MagicCompRules.txt`
//! has zero case-insensitive hits for "winston", and the neighbouring limited
//! procedures it *does* define are `905.` (Conspiracy Draft) and `903.13`
//! (Commander Draft). More to the point, nothing here implements a game rule:
//! this file is draft *strategy*, which is not a rules matter at any level. The
//! procedural authority for the format (WotC "Casual Formats") is already cited
//! at the reducer in `draft-core::shared_stack`.
//!
//! # The five principles, one named term each
//!
//! Each principle is a separately tunable weight on [`WinstonWeights`], not a
//! fold into one opaque number, so each can be tested — and retuned — alone:
//!
//! 1. **Prioritize mana fixing.** [`WinstonWeights::fixing_premium`], folded into
//!    [`DraftWeights::fixing_land`] by [`WinstonWeights::card_weights`] so the
//!    *detector* stays `draft_eval::produced_color_count(face) >= 2` and only the
//!    price changes. Winston pools are colour soup; fixing is worth more here
//!    than in a pick-and-pass draft.
//! 2. **Avoid narrow archetypes.** [`WinstonWeights::color_commitment_start`] /
//!    [`WinstonWeights::color_commitment_max`] — the colour bonus is *exactly*
//!    zero until the draft is well along, then ramps to a maximum far below the
//!    pick-and-pass bot's 4.0/6.0. Take raw power early, pick colours at
//!    deckbuilding.
//! 3. **Pile math.** [`pile_value`] returns a decomposed [`PileValuation`]:
//!    `playables` is the floored sum of per-card surplus over
//!    [`WinstonWeights::replacement_level`] (a medium pile of playables beats one
//!    premium card, seven unplayables do not), and `denial` is
//!    [`WinstonWeights::denial_per_card`] × the pile's height (a card in your
//!    pool is a card not in theirs).
//! 4. **Information gathering.** [`OpponentRead`] carries both halves: the
//!    pile-size appetite that prices [`handoff_cost`] (a player who snaps up
//!    small piles is bomb-hunting, and a player who takes big ones makes
//!    declining expensive), and the [`ColorPassTally`] of what other seats have
//!    looked at and put back, priced by
//!    [`WinstonWeights::opponent_pass_color_weight`].
//! 5. **Interaction is key.** [`WinstonWeights::interaction_premium`] folds into
//!    [`DraftWeights::removal`] / [`DraftWeights::counter`], and
//!    [`WinstonWeights::cheap_interaction_bonus`] adds the *cheapness* axis that
//!    `DraftWeights` has no room for, gated by
//!    [`WinstonWeights::cheap_interaction_max_mv`].
//!
//! # Known limitations, with their reasons
//!
//! - **Later piles are priced by height alone.** [`unseen_pile_value`] treats a
//!   pile of `m` cards as `m` average cards, because a pile above the cursor
//!   publishes its height and nothing else. Deliberate, and the only place the
//!   bot guesses.
//! - **[`handoff_cost`] is charged uniformly across a multi-decline
//!   continuation.** The *k*-th decline actually fattens the *k*-th pile, not the
//!   cursor pile. Charging the cursor's rate throughout overstates the cost when
//!   later piles are smaller and understates it when they are larger. The exact
//!   version needs no new input — it is a per-step `take_rate_for` — and is named
//!   here so the approximation is a decision rather than a bug.
//! - **The "card type" half of principle 4 is not implemented.** A type tally is
//!   recoverable from the identical published input (`type_line` rides the same
//!   card instances the colour tally folds); it is scoped out because it needs
//!   its own weight and its own two-sided test, not because the information is
//!   missing. The design is [`ColorPassTally`] with a type key.

use std::cmp::Ordering;
use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use engine::types::card::CardFace;

use crate::cast_facts::EffectProfile;
use crate::config::AiDifficulty;
use crate::draft_eval::{evaluate_draft_card, rarity_prior, DraftWeights};

/// Cards the bot must have seen before it trusts its own surplus estimate.
/// Below this, [`SurplusSample::mean_or`] falls back to
/// [`WinstonWeights::prior_surplus_per_card`].
pub const MIN_SURPLUS_SAMPLE: usize = 6;

/// Cards the opponent must have passed before the colour read is worth anything.
/// Below this, [`opponent_pass_bonus`] is exactly `0.0` — an honest confidence
/// gate rather than a fabricated signal.
pub const MIN_PASS_SAMPLE: usize = 8;

/// The neutral point of [`ColorPassTally::pass_share`]: the share a colour would
/// hold in a perfectly uniform tally of mono-coloured cards, which is what the
/// Laplace prior smooths towards.
///
/// APPROXIMATION, with its error direction: gold cards contribute to two shares,
/// so the shares of a real tally sum above 1 and a perfectly neutral tally hands
/// every *coloured* card a small positive bonus while a colourless card sits at
/// exactly `0.0`. The bias is second-order and systematically
/// colour-over-artifact. If it ever matters, normalise by the sum of the shares
/// rather than by the colour count.
pub const UNIFORM_SHARE: f64 = 0.2;

/// The five colours, the denominator of [`ColorPassTally`]'s Laplace prior.
const COLOR_COUNT: usize = 5;

// ── Inputs ──────────────────────────────────────────────────────────────────

/// Everything the valuation layer knows about one card.
///
/// Deliberately *not* a draft-core type: this crate does not depend on
/// `draft-core`, and the fields here are exactly the ones a Winston projection
/// publishes for a card the bot is allowed to see. There is no field that could
/// carry a card the bot must not see — the no-cheating boundary is the type,
/// not a review note.
#[derive(Debug, Clone, Copy)]
pub struct DraftCardFacts<'a> {
    /// The parsed face, when a [`CardDatabase`](engine::database::CardDatabase)
    /// is loaded and knows the name. `None` degrades the card to its rarity
    /// prior — principles 1 and 5 go quiet, which is what
    /// `winston_decision_degrades_without_a_card_database` pins.
    pub face: Option<&'a CardFace>,
    /// The printing's rarity, for [`rarity_prior`]. A property of the printing,
    /// not of the face, which is why it rides alongside.
    pub rarity: &'a str,
    /// Mana value, for the cheap-interaction gate. Published per instance.
    pub cmc: u8,
    /// The card's colours, for the colour ramp and the opponent read.
    pub colors: &'a [String],
}

/// The draft-wide context a single card's value depends on, beyond the card.
///
/// Separate from [`WinstonTurn`] so card- and pile-level scoring can be tested
/// without inventing a whole turn.
#[derive(Debug, Clone, Copy)]
pub struct CardContext<'a> {
    /// The bot's own colours — `draft_eval::dominant_colors` over its pool.
    /// Empty means "no read yet", and then no card is on-colour.
    pub preferred_colors: &'a [String],
    /// How far through the draft this seat is, `0.0` → `1.0`. See
    /// [`draft_progress`], which is the only place the ratio is computed.
    pub progress: f64,
    /// What other seats have looked at and put back.
    pub passed: &'a ColorPassTally,
}

/// One Winston turn as the bot sees it: the cursor pile it may take, the heights
/// of the piles it could decline onto, and the reads that price both.
#[derive(Debug, Clone, Copy)]
pub struct WinstonTurn<'a> {
    /// The cursor pile, fully revealed to the seat whose turn it is.
    pub cursor_pile: &'a [DraftCardFacts<'a>],
    /// The heights — and *only* the heights — of every pile after the cursor, in
    /// pile order.
    ///
    /// The caller must include the **last** pile: declining onto the final pile
    /// and taking it is the commonest way a Winston turn ends with a large pile,
    /// and a continuation that stops one pile short would systematically
    /// under-value it.
    pub later_pile_sizes: &'a [usize],
    /// Whether declining the *last* pile — the forced draw off the main stack —
    /// is published as legal. Read from the projection's legality vector, never
    /// re-derived from a card count.
    pub forced_draw_legal: bool,
    /// Every card the bot has already drafted. Feeds [`surplus_sample`], which
    /// is how the bot prices a pile it cannot see into.
    pub pool: &'a [DraftCardFacts<'a>],
    /// What other seats have done, so far, with piles of each size.
    pub read: &'a OpponentRead,
    /// See [`CardContext`].
    pub context: CardContext<'a>,
}

// ── Reads ───────────────────────────────────────────────────────────────────

/// Cards other seats inspected and put back, tallied by colour.
///
/// `BTreeMap`, never `HashMap`: a decision must not depend on map iteration
/// order. (MEASURED on the pre-existing count-only `color_preference` fold: 12
/// distinct answers over 2000 draws on a four-way tie. See
/// [`crate::draft_eval::dominant_colors`].)
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColorPassTally {
    counts: BTreeMap<String, usize>,
    cards: usize,
}

impl ColorPassTally {
    /// Record one passed card by its colours. A colourless card still counts
    /// towards [`Self::cards`] — it is evidence about the *rate* of passing even
    /// though it names no colour.
    pub fn observe(&mut self, colors: &[String]) {
        self.cards += 1;
        for color in colors {
            *self.counts.entry(color.clone()).or_insert(0) += 1;
        }
    }

    /// Laplace-smoothed share of passed cards carrying `color`:
    /// `(count + 1) / (cards + 5)`.
    ///
    /// The `+1 / +5` prior is what makes [`UNIFORM_SHARE`] exactly the neutral
    /// point: an empty tally returns `0.2` for every colour, so a one-card
    /// sample cannot produce a `0.0` or `1.0` share.
    pub fn pass_share(&self, color: &str) -> f64 {
        let count = self.counts.get(color).copied().unwrap_or(0);
        (count + 1) as f64 / (self.cards + COLOR_COUNT) as f64
    }

    /// How many passed cards this tally has seen. The gate
    /// [`opponent_pass_bonus`] applies [`MIN_PASS_SAMPLE`] to.
    pub fn cards(&self) -> usize {
        self.cards
    }
}

/// What the bot has read off the other seats' published decisions.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OpponentRead {
    /// Share of piles below [`WinstonWeights::opponent_size_split`] that other
    /// seats **took**. A high rate is the bomb-hunting tell: they keep snapping
    /// up small piles, so the pile they hand back is worth less to them than to
    /// you.
    pub small_pile_take_rate: f64,
    /// Share of piles at or above the split that they took. This is the rate
    /// that prices a *decline*, because declining is what makes a pile big.
    pub large_pile_take_rate: f64,
    /// Cards other seats looked at and put back, tallied by colour.
    pub passed_colors: ColorPassTally,
    /// Decision observations behind the two rates. `0` means no read at all, and
    /// [`handoff_cost`] then falls back to the flat penalty rather than trusting
    /// a rate computed from nothing.
    pub samples: usize,
}

impl OpponentRead {
    /// The take rate that applies to a pile of `pile_size` cards.
    pub fn take_rate_for(&self, pile_size: usize, split: usize) -> f64 {
        if pile_size >= split {
            self.large_pile_take_rate
        } else {
            self.small_pile_take_rate
        }
    }
}

/// A running mean of per-card surplus over the cards the bot has legitimately
/// seen. A typed accumulator rather than a `bool` for "do we have enough yet".
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct SurplusSample {
    /// Summed [`card_surplus`] over the sampled cards.
    pub total: f64,
    /// How many cards were sampled.
    pub count: usize,
}

impl SurplusSample {
    /// Mean surplus per card, or `prior` while the sample is below
    /// [`MIN_SURPLUS_SAMPLE`].
    pub fn mean_or(&self, prior: f64) -> f64 {
        if self.count < MIN_SURPLUS_SAMPLE {
            prior
        } else {
            self.total / self.count as f64
        }
    }
}

// ── Outputs ─────────────────────────────────────────────────────────────────

/// A pile's worth, kept **decomposed** so a test can assert which half moved.
///
/// Principle 3 has two independent halves — "a medium pile of playables often
/// beats one premium card" and "larger piles starve the opponent" — and an
/// aggregate hides which one a change touched. Concretely: for pile A of one
/// card at surplus 5.0 and pile B of seven cards at surplus 1.0, replacing the
/// sum with `max(card_surplus)` gives `A = 5 + d` and `B = 1 + 7d`, so the
/// aggregate assertion `B > A` stays green for every `denial_per_card > 2/3`.
/// The `playables` assertion is red for *every* value.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct PileValuation {
    /// Principle 3's "medium pile of playables": the floored sum of per-card
    /// surplus. Sub-replacement cards contribute exactly zero.
    pub playables: f64,
    /// Principle 3's "starve them": flat value per card in the pile, regardless
    /// of whether the bot would ever play it.
    pub denial: f64,
}

impl PileValuation {
    /// The two halves, summed — what the decision actually compares.
    pub fn total(&self) -> f64 {
        self.playables + self.denial
    }
}

/// Take-now versus the best continuation of this turn, kept as two numbers for
/// the same reason [`PileValuation`] is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TurnValuation {
    /// [`pile_value`] of the cursor pile, totalled. Fully revealed — no guessing.
    pub take_now: f64,
    /// The best value reachable by declining, net of what each decline costs.
    /// [`f64::NEG_INFINITY`] when no continuation exists at all (the cursor is
    /// the last pile and the forced draw is not legal), which is the honest
    /// value of an empty maximum and makes taking the only option.
    pub continuation: f64,
}

impl TurnValuation {
    /// Whether the bot should take the cursor pile. Ties go to `Take`, matching
    /// `SharedStackPileDecision::ALL`'s declaration order and the order
    /// `shared_stack::forced_decision` folds.
    pub fn prefers_taking(&self) -> bool {
        self.take_now >= self.continuation
    }
}

// ── Weights and the difficulty ladder ───────────────────────────────────────

/// Every tunable in the Winston valuation, one named field per principle term.
///
/// None of these values is measured — the tests pin *relations* between terms
/// (fixing beats a body, seven playables beat one bomb, cheap interaction beats
/// expensive), never magnitudes, so retuning is a tuning change rather than a
/// regression.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WinstonWeights {
    /// "Generally playable". A card below this contributes **zero** to
    /// [`PileValuation::playables`].
    ///
    /// This single constant sets where principle 3's crossover sits, and it is
    /// also what makes principle 3 live at all: MEASURED, a bot with
    /// `replacement_level = 0` takes every pile, the cursor-pile height collapses
    /// to a median of 1, and the medium-pile regime the principle describes never
    /// occurs. `replacement_level` is what *creates* piles.
    pub replacement_level: f64,
    /// Principle 3's denial half: flat value for every card in a pile you take,
    /// including sub-replacement ones — a card in your pool is a card not in
    /// theirs whether or not you play it.
    pub denial_per_card: f64,
    /// Principle 1. Added to [`DraftWeights::fixing_land`] by
    /// [`WinstonWeights::card_weights`].
    pub fixing_premium: f64,
    /// Principle 5's first half. Added to [`DraftWeights::removal`] and
    /// [`DraftWeights::counter`].
    pub interaction_premium: f64,
    /// Principle 5's second half: the cheapness axis `DraftWeights` has no room
    /// for, since it carries no mana-value term for spells.
    pub cheap_interaction_bonus: f64,
    /// The mana value at or below which interaction counts as cheap.
    pub cheap_interaction_max_mv: u8,
    /// Principle 2: the [`draft_progress`] point below which the colour bonus is
    /// **exactly zero**. Not "a small number" — a small bias is still a
    /// commitment.
    pub color_commitment_start: f64,
    /// Principle 2: the colour bonus at the very end of the draft. Kept well
    /// below the pick-and-pass bot's 4.0/6.0, because a Winston pool is colour
    /// soup and is about half the cards in the draft.
    pub color_commitment_max: f64,
    /// Principle 4: how hard the opponent's take rate scales [`handoff_cost`].
    pub opponent_greed_weight: f64,
    /// The pile height at which [`OpponentRead::take_rate_for`] switches from the
    /// small-pile rate to the large-pile rate.
    pub opponent_size_split: usize,
    /// Principle 4's colour read: the price of a colour the opponent passes more
    /// than its share.
    ///
    /// The sign encodes a genuinely ambiguous reading of the requirement. Positive
    /// means *a colour they keep passing is OPEN* — it will keep flowing to you,
    /// so it is safer to invest in. Negative would mean *they pass it because it
    /// is weak*. Flipping the sign is a one-constant change that reddens exactly
    /// one test, which is why it is a weight rather than a hard-coded direction.
    pub opponent_pass_color_weight: f64,
    /// The base cost of a decline: it appends a card to the cursor pile and hands
    /// the fattened pile to the next seat. Declining is never free.
    pub handoff_penalty: f64,
    /// The per-card surplus assumed for an unseen pile before the bot's own
    /// sample reaches [`MIN_SURPLUS_SAMPLE`].
    pub prior_surplus_per_card: f64,
}

impl WinstonWeights {
    /// All five principles live, conservatively tuned. This is what a pod gets
    /// BY DEFAULT: a pod that names no difficulty runs its bots at
    /// `AiDifficulty::Medium`, so a principle placed above `Medium` would be
    /// dead for every such pod. The higher rungs are genuinely reachable — a
    /// pod CAN name a difficulty and `create_multiplayer_draft` writes it on
    /// every pool arm — they are simply not what the common pod gets, which is
    /// why the full set lives here rather than at `sharp()`.
    pub fn baseline() -> Self {
        Self {
            // A vanilla 2/2-for-2 scores 3.5 under `DraftWeights::default`, so
            // 3.0 makes that card marginally playable and anything weaker filler.
            replacement_level: 3.0,
            denial_per_card: 0.5,
            fixing_premium: 2.5,
            interaction_premium: 1.5,
            cheap_interaction_bonus: 1.5,
            cheap_interaction_max_mv: 3,
            // Half-way through the draft: colours are decided at deckbuilding,
            // and before this point the bonus is exactly 0.0.
            color_commitment_start: 0.5,
            color_commitment_max: 1.5,
            opponent_greed_weight: 0.5,
            opponent_size_split: 3,
            opponent_pass_color_weight: 1.0,
            handoff_penalty: 0.5,
            prior_surplus_per_card: 1.0,
        }
    }

    /// Principle 3's denial half and nothing else — "the biggest pile wins".
    ///
    /// An infinite `replacement_level` floors every card's surplus at zero, so
    /// `playables` is identically 0.0 and only pile *height* moves the score.
    /// This is a **parameterization of the scored strategy**, not a third
    /// [`WinstonStrategy`] variant.
    ///
    /// Caution for a future consumer: `f64::INFINITY` does not survive a JSON
    /// round trip (`serde_json` writes it as `null` and refuses to read it back
    /// as an `f64`). Nothing serializes a [`WinstonWeights`] today — the derive
    /// mirrors [`DraftWeights`]'s — so if one is ever persisted or sent over a
    /// wire, this profile needs a sentinel rather than an infinity.
    pub fn denial_only() -> Self {
        Self {
            replacement_level: f64::INFINITY,
            denial_per_card: 0.5,
            fixing_premium: 0.0,
            interaction_premium: 0.0,
            cheap_interaction_bonus: 0.0,
            cheap_interaction_max_mv: 0,
            color_commitment_start: 1.0,
            color_commitment_max: 0.0,
            opponent_greed_weight: 0.0,
            opponent_size_split: 3,
            opponent_pass_color_weight: 0.0,
            handoff_penalty: 0.5,
            prior_surplus_per_card: 0.0,
        }
    }

    /// [`Self::baseline`] with the opponent read and interaction tuned harder.
    pub fn sharp() -> Self {
        Self {
            interaction_premium: 2.5,
            opponent_greed_weight: 1.0,
            opponent_pass_color_weight: 1.5,
            ..Self::baseline()
        }
    }

    /// [`Self::sharp`] plus the earliest colour commitment and the highest
    /// cheap-interaction bonus.
    pub fn sharpest() -> Self {
        Self {
            color_commitment_start: 0.3,
            cheap_interaction_bonus: 2.5,
            ..Self::sharp()
        }
    }

    /// The shared card-quality weights with this profile's premiums folded in.
    ///
    /// Composition, not a fork: the *detectors* stay `draft_eval`'s
    /// (`produced_color_count >= 2` for fixing, `EffectProfile` for removal and
    /// counterspells) and only the prices change, so every card in the database
    /// is covered by construction.
    ///
    /// [`DraftWeights::mass_removal`] is deliberately **not** premiumed: a board
    /// wipe is already the highest-weighted effect there (5.0) and it is a bomb
    /// rather than the cheap answer principle 5 asks for. The cheapness axis is
    /// applied separately, in [`card_value`].
    ///
    /// Note, since it is load-bearing and pre-existing:
    /// `cast_facts::is_direct_removal` classifies `Effect::Counter` as removal as
    /// well as a counterspell, so a counterspell collects `interaction_premium`
    /// through both folds. That is `draft_eval`'s shared detector, shared with the
    /// pick-and-pass bot, and is left alone here rather than forked — the
    /// resulting double weighting is consistent with principle 5 ranking stack
    /// interaction alongside removal. Pinned by
    /// `cheap_interaction_outranks_an_equal_bodied_creature`.
    pub fn card_weights(&self) -> DraftWeights {
        let base = DraftWeights::default();
        DraftWeights {
            fixing_land: base.fixing_land + self.fixing_premium,
            removal: base.removal + self.interaction_premium,
            counter: base.counter + self.interaction_premium,
            ..base
        }
    }
}

/// How a bot seat decides. **Two** variants, because there are exactly two
/// *shapes*: evaluate, or do not.
///
/// Every scoring rung on the difficulty ladder is the same `Scored` variant with
/// different [`WinstonWeights`] — "the biggest pile wins" is
/// `Scored(denial_only())`, not a third sibling. Five rungs, one variant.
#[derive(Debug, Clone, PartialEq)]
pub enum WinstonStrategy {
    /// No evaluation at all: take the first legal decision the projection
    /// publishes. This is exactly the move `shared_stack::forced_decision` folds
    /// and the pick-timer's `autoDecideSharedStackTurn` already submits — a
    /// legal, maximally passive opponent, reusing the existing notion of "weak"
    /// rather than inventing a second one.
    FirstLegal,
    /// Score the turn with these weights. Boxed: [`WinstonWeights`] is far larger
    /// than the unit variant beside it.
    Scored(Box<WinstonWeights>),
}

impl WinstonStrategy {
    /// The difficulty ladder. Exhaustive over all six `AiDifficulty` rungs with
    /// no wildcard, so adding a rung is a compile error here rather than a silent
    /// downgrade.
    ///
    /// Every rung reads exactly the same published projection — the ladder is
    /// *which terms are live and how hard they are tuned*, never "reads more of
    /// the hidden state".
    pub fn for_difficulty(difficulty: AiDifficulty) -> Self {
        match difficulty {
            AiDifficulty::VeryEasy => Self::FirstLegal,
            AiDifficulty::Easy => Self::Scored(Box::new(WinstonWeights::denial_only())),
            AiDifficulty::Medium => Self::Scored(Box::new(WinstonWeights::baseline())),
            AiDifficulty::Hard => Self::Scored(Box::new(WinstonWeights::sharp())),
            // `CEDH` shares `VeryHard`'s weights but is written as its own arm,
            // not a `_`, so the exhaustiveness above is real.
            AiDifficulty::VeryHard => Self::Scored(Box::new(WinstonWeights::sharpest())),
            AiDifficulty::CEDH => Self::Scored(Box::new(WinstonWeights::sharpest())),
        }
    }
}

// ── Card-level valuation ────────────────────────────────────────────────────

/// How far through the draft a seat is, `0.0` at the first pick approaching
/// `1.0` at the last.
///
/// `total_cards` is the count of cards still **undrafted**, so the ratio rises
/// monotonically. **`f64` division throughout** — an integer division here
/// truncates `total_cards / seat_count` and quantises the whole ramp.
pub fn draft_progress(pool_size: usize, total_cards: usize, seat_count: usize) -> f64 {
    let this_seats_share = total_cards as f64 / seat_count.max(1) as f64;
    let denominator = pool_size as f64 + this_seats_share;
    if denominator <= 0.0 {
        return 0.0;
    }
    pool_size as f64 / denominator
}

/// Principle 2's late colour ramp: **exactly zero** until the draft is
/// `color_commitment_start` of the way through, then linear to
/// `color_commitment_max` at the end.
///
/// Off-colour cards are never *penalised*, at any point — a penalty is a
/// commitment, and principle 2 says not to commit. A colourless card is not
/// on-colour and scores `0.0`; it is equally castable in every deck, so it needs
/// no nudge.
pub fn color_bonus(card: &DraftCardFacts, w: &WinstonWeights, ctx: &CardContext) -> f64 {
    if ctx.progress < w.color_commitment_start {
        return 0.0;
    }
    let on_color = card
        .colors
        .iter()
        .any(|color| ctx.preferred_colors.contains(color));
    if !on_color {
        return 0.0;
    }
    let span = 1.0 - w.color_commitment_start;
    if span <= 0.0 {
        return w.color_commitment_max;
    }
    w.color_commitment_max * ((ctx.progress - w.color_commitment_start) / span)
}

/// Principle 4's colour read: what a card gains (or loses) for being in a colour
/// the opponent passes more (or less) than its share.
///
/// Two-sided by construction — `pass_share - UNIFORM_SHARE` is negative for an
/// under-passed colour — so the term penalises as well as rewards. A one-sided
/// term is the shape that passes a test vacuously.
///
/// Gated on sample count rather than on draft progress: this is a *supply*
/// signal, not a commitment, so principle 2's ramp deliberately does not apply.
/// A colourless card scores exactly `0.0` from the empty maximum, which is
/// intended rather than incidental.
pub fn opponent_pass_bonus(
    card: &DraftCardFacts,
    w: &WinstonWeights,
    tally: &ColorPassTally,
) -> f64 {
    if tally.cards() < MIN_PASS_SAMPLE {
        return 0.0;
    }
    card.colors
        .iter()
        .map(|color| tally.pass_share(color) - UNIFORM_SHARE)
        .max_by(|a, b| a.partial_cmp(b).unwrap_or(Ordering::Equal))
        .map_or(0.0, |best| w.opponent_pass_color_weight * best)
}

/// What one card is worth to this bot, right now.
///
/// Composes `draft_eval`'s card quality (which deliberately excludes rarity, so
/// the [`rarity_prior`] added here is not a double count) with the three
/// context-dependent terms: cheap interaction, the colour ramp, and the opponent
/// read.
pub fn card_value(card: &DraftCardFacts, w: &WinstonWeights, ctx: &CardContext) -> f64 {
    card_value_with(card, w, &w.card_weights(), ctx)
}

/// [`card_value`] with the folded [`DraftWeights`] hoisted out, so a pile does
/// not rebuild them per card.
fn card_value_with(
    card: &DraftCardFacts,
    w: &WinstonWeights,
    card_weights: &DraftWeights,
    ctx: &CardContext,
) -> f64 {
    let quality = card
        .face
        .map_or(0.0, |face| evaluate_draft_card(face, card_weights));

    // Principle 5's cheapness axis. `EffectProfile::from_face` runs a second time
    // here (`evaluate_draft_card` builds one internally); that is one extra pass
    // over an already-parsed face, on a path bounded by the cursor pile's height.
    // The alternative is exporting `evaluate_draft_card`'s internals, which would
    // couple this layer to that function's implementation rather than its
    // interface.
    let cheap_interaction = match card.face {
        Some(face) if card.cmc <= w.cheap_interaction_max_mv => {
            let profile = EffectProfile::from_face(face);
            if profile.has_direct_removal_text || profile.has_counter_spell {
                w.cheap_interaction_bonus
            } else {
                0.0
            }
        }
        _ => 0.0,
    };

    quality
        + rarity_prior(card.rarity)
        + cheap_interaction
        + color_bonus(card, w, ctx)
        + opponent_pass_bonus(card, w, ctx.passed)
}

/// A card's value **over replacement**, floored at zero.
///
/// The floor is what stops "seven unplayable cards beat a bomb" while still
/// letting "seven playable cards beat a bomb".
pub fn card_surplus(card: &DraftCardFacts, w: &WinstonWeights, ctx: &CardContext) -> f64 {
    card_surplus_with(card, w, &w.card_weights(), ctx)
}

/// [`card_surplus`] with the folded [`DraftWeights`] hoisted out.
fn card_surplus_with(
    card: &DraftCardFacts,
    w: &WinstonWeights,
    card_weights: &DraftWeights,
    ctx: &CardContext,
) -> f64 {
    (card_value_with(card, w, card_weights, ctx) - w.replacement_level).max(0.0)
}

// ── Pile-level valuation ────────────────────────────────────────────────────

/// Principle 3, decomposed: what a pile of known cards is worth.
///
/// `playables` is a **sum**, not a max and not an average — explicitly *not*
/// diminishing returns, which would fight principle 3's "larger piles are
/// better". `denial` is flat per card.
pub fn pile_value(
    cards: &[DraftCardFacts],
    w: &WinstonWeights,
    ctx: &CardContext,
) -> PileValuation {
    let card_weights = w.card_weights();
    PileValuation {
        playables: cards
            .iter()
            .map(|card| card_surplus_with(card, w, &card_weights, ctx))
            .sum(),
        denial: w.denial_per_card * cards.len() as f64,
    }
}

/// What a pile the bot cannot see into is worth: `m` cards of average value.
///
/// The bot knows a later pile's height and nothing else — the projection leaks
/// no contents for a pile above the cursor — so it prices it as `m` average
/// cards. The only guess in the design, and it is confined to this function.
pub fn unseen_pile_value(size: usize, expected_surplus: f64, w: &WinstonWeights) -> f64 {
    size as f64 * (expected_surplus + w.denial_per_card)
}

/// The bot's own estimate of what an average card is worth, from every card it
/// has legitimately seen: its pool, plus the cursor pile revealed to it this
/// turn. Both are published fields of its own projection.
pub fn surplus_sample(turn: &WinstonTurn, w: &WinstonWeights) -> SurplusSample {
    let card_weights = w.card_weights();
    let mut sample = SurplusSample::default();
    for card in turn.pool.iter().chain(turn.cursor_pile.iter()) {
        sample.total += card_surplus_with(card, w, &card_weights, &turn.context);
        sample.count += 1;
    }
    sample
}

/// What one decline costs: it appends a card to the cursor pile and hands the
/// fattened pile to the next seat.
///
/// Principle 4's pile-size read is the multiplier — an opponent who takes big
/// piles makes handing them a big pile expensive. With no observations at all
/// (`samples == 0`) the rates are meaningless, so the cost falls back to the flat
/// penalty rather than trusting a number computed from nothing.
pub fn handoff_cost(cursor_pile_size: usize, read: &OpponentRead, w: &WinstonWeights) -> f64 {
    if read.samples == 0 {
        return w.handoff_penalty;
    }
    let rate = read.take_rate_for(cursor_pile_size + 1, w.opponent_size_split);
    w.handoff_penalty * (1.0 + w.opponent_greed_weight * rate)
}

/// The best value reachable by declining, net of what each decline costs.
///
/// The stopping points are every later pile **inclusive of the last one** —
/// declining onto the final pile and taking it is the commonest way a Winston
/// turn ends with a large pile — plus the forced draw off the main stack when the
/// projection publishes that decline as legal.
///
/// Unrolled, never recursive: there are at most `pile_count` stopping points
/// (three in a standard Winston pod), so this is a bounded maximum over a short
/// iterator with no search. Returns [`f64::NEG_INFINITY`] when no continuation
/// exists.
///
/// The opponent does not act between the bot's own declines, so there is no
/// within-turn survival discount; the opponent enters the arithmetic only through
/// [`handoff_cost`], which is the correct and only place.
pub fn continuation_value(turn: &WinstonTurn, w: &WinstonWeights) -> f64 {
    let expected = surplus_sample(turn, w).mean_or(w.prior_surplus_per_card);
    let cost = handoff_cost(turn.cursor_pile.len(), turn.read, w);

    let stops = turn
        .later_pile_sizes
        .iter()
        .enumerate()
        .map(|(offset, &size)| {
            let declines = offset + 1;
            unseen_pile_value(size, expected, w) - declines as f64 * cost
        })
        .chain(turn.forced_draw_legal.then(|| {
            // The forced draw is one card off the main stack, after declining
            // every pile including the last.
            let declines = turn.later_pile_sizes.len() + 1;
            (expected + w.denial_per_card) - declines as f64 * cost
        }));

    stops.fold(f64::NEG_INFINITY, f64::max)
}

/// Take the cursor pile now, or decline and keep going? Both numbers, decomposed.
pub fn valuate_turn(turn: &WinstonTurn, w: &WinstonWeights) -> TurnValuation {
    TurnValuation {
        take_now: pile_value(turn.cursor_pile, w, &turn.context).total(),
        continuation: continuation_value(turn, w),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::draft_eval::dominant_colors;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, Effect, PtValue, TargetFilter, TriggerDefinition,
    };
    use engine::types::card_type::{CardType, CoreType};
    use engine::types::mana::ManaCost;
    use engine::types::triggers::TriggerMode;
    use engine::types::zones::Zone;

    // ── Fixtures ────────────────────────────────────────────────────────────
    // Built from `draft_eval`'s own test fixtures so the two layers agree on
    // what a dual land / a removal spell / a vanilla body is.

    fn face(core: Vec<CoreType>) -> CardFace {
        CardFace {
            card_type: CardType {
                core_types: core,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn vanilla_creature(power: i32, toughness: i32, mv: u32) -> CardFace {
        CardFace {
            power: Some(PtValue::Fixed(power)),
            toughness: Some(PtValue::Fixed(toughness)),
            mana_cost: ManaCost::generic(mv),
            ..face(vec![CoreType::Creature])
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

    fn removal_spell(mv: u32) -> CardFace {
        CardFace {
            mana_cost: ManaCost::generic(mv),
            abilities: vec![destroy_ability()],
            ..face(vec![CoreType::Instant])
        }
    }

    fn counterspell(mv: u32) -> CardFace {
        CardFace {
            mana_cost: ManaCost::generic(mv),
            abilities: vec![AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Counter {
                    target: TargetFilter::Any,
                    source_rider: None,
                    countered_spell_zone: None,
                },
            )],
            ..face(vec![CoreType::Instant])
        }
    }

    /// The Ravenous Chupacabra shape: a body plus ETB removal. Comfortably the
    /// best card in any of these fixtures.
    fn bomb() -> CardFace {
        let mut c = vanilla_creature(2, 2, 4);
        c.triggers = vec![TriggerDefinition::new(TriggerMode::ChangesZone)
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(destroy_ability())];
        c
    }

    /// `Land — Plains Island`: the exact dual fixture `draft_eval`'s own tests use.
    fn dual_land() -> CardFace {
        let mut f = face(vec![CoreType::Land]);
        f.card_type.subtypes = vec!["Plains".to_string(), "Island".to_string()];
        f
    }

    fn mono_land() -> CardFace {
        let mut f = face(vec![CoreType::Land]);
        f.card_type.subtypes = vec!["Forest".to_string()];
        f
    }

    fn colors(list: &[&str]) -> Vec<String> {
        list.iter().map(|c| c.to_string()).collect()
    }

    fn card<'a>(face: &'a CardFace, cmc: u8, colors: &'a [String]) -> DraftCardFacts<'a> {
        DraftCardFacts {
            face: Some(face),
            rarity: "common",
            cmc,
            colors,
        }
    }

    /// No colour read, no opponent read, no draft progress — so every test that
    /// is not about those terms sees them contribute exactly zero.
    fn neutral_context<'a>(
        empty_colors: &'a [String],
        empty_tally: &'a ColorPassTally,
    ) -> CardContext<'a> {
        CardContext {
            preferred_colors: empty_colors,
            progress: 0.0,
            passed: empty_tally,
        }
    }

    // ── P1: prioritize mana fixing ──────────────────────────────────────────

    /// Principle 1. Both directions in one test: with the premium the dual land
    /// pile wins, with the premium at zero the creature pile does — so this is
    /// red whether the premium is deleted or merely folded into the wrong
    /// `DraftWeights` field.
    #[test]
    fn fixing_premium_moves_a_dual_land_pile_above_a_creature_pile() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);

        let dual = dual_land();
        let bear = vanilla_creature(2, 2, 2);
        let dual_pile = [card(&dual, 0, &none)];
        let creature_pile = [card(&bear, 2, &none)];

        let with_premium = WinstonWeights::baseline();
        assert!(
            with_premium.fixing_premium > 0.0,
            "the premium must be live, or both legs below are 0.0 == 0.0"
        );
        let without_premium = WinstonWeights {
            fixing_premium: 0.0,
            ..WinstonWeights::baseline()
        };

        let dual_with = pile_value(&dual_pile, &with_premium, &ctx);
        let creature_with = pile_value(&creature_pile, &with_premium, &ctx);
        assert!(
            dual_with.playables > creature_with.playables,
            "fixing must outrank a filler body in a colour-soup pool: {dual_with:?} vs {creature_with:?}"
        );
        assert!(dual_with.total() > creature_with.total());

        let dual_without = pile_value(&dual_pile, &without_premium, &ctx);
        let creature_without = pile_value(&creature_pile, &without_premium, &ctx);
        assert!(
            creature_without.playables > dual_without.playables,
            "without the premium the base weights put the body first: {creature_without:?} vs {dual_without:?}"
        );

        // Hostile fixture: the premium prices FIXING, not lands. A mono-colour
        // nonbasic produces one colour, so it stays at zero either way.
        let mono = mono_land();
        let mono_pile = [card(&mono, 0, &none)];
        assert_eq!(pile_value(&mono_pile, &with_premium, &ctx).playables, 0.0);
        assert_eq!(
            pile_value(&mono_pile, &without_premium, &ctx).playables,
            0.0
        );
    }

    // ── P2: avoid narrow archetypes ─────────────────────────────────────────

    /// Principle 2. Early in the draft the colour term is **exactly** zero — not
    /// small, zero — and it only ramps once the draft is mostly done.
    #[test]
    fn colour_discipline_is_exactly_zero_before_the_ramp_opens() {
        let w = WinstonWeights::baseline();

        // Anti-vacuity (both mandatory): without these the whole ramp can be dead
        // at zero and every assertion below reads 0.0 == 0.0.
        assert!(w.color_commitment_max > 0.0);
        assert!(w.color_commitment_start > 0.0 && w.color_commitment_start < 1.0);

        // Reach-guard: there IS a colour preference, so the early zero is a
        // suppressed bonus rather than an absent one.
        let pool = [colors(&["W"]), colors(&["W"]), colors(&["W"])];
        let borrowed: Vec<&[String]> = pool.iter().map(|c| c.as_slice()).collect();
        let preferred = dominant_colors(&borrowed, 3);
        assert_eq!(preferred, colors(&["W"]));

        let tally = ColorPassTally::default();
        let early = CardContext {
            preferred_colors: &preferred,
            progress: 0.1,
            passed: &tally,
        };
        let late = CardContext {
            preferred_colors: &preferred,
            progress: 0.9,
            passed: &tally,
        };

        let white = colors(&["W"]);
        let green = colors(&["G"]);
        let filler = vanilla_creature(2, 2, 2);
        let premium = vanilla_creature(3, 3, 2);
        let on_color = card(&filler, 2, &white);
        let off_color = card(&premium, 2, &green);

        // Early: the on-colour filler gets no edge at all over the off-colour
        // premium — the difference in the colour term is exactly 0.0.
        assert_eq!(color_bonus(&on_color, &w, &early), 0.0);
        assert_eq!(color_bonus(&off_color, &w, &early), 0.0);
        assert_eq!(
            color_bonus(&on_color, &w, &early) - color_bonus(&off_color, &w, &early),
            0.0
        );
        // ... and the raw-power card is still the better pick early on, which is
        // what principle 2 asks for.
        assert!(card_value(&off_color, &w, &early) > card_value(&on_color, &w, &early));

        // Late: exactly the linear ramp, and nothing for the off-colour card.
        let expected = w.color_commitment_max * (0.9 - w.color_commitment_start)
            / (1.0 - w.color_commitment_start);
        let late_gain = color_bonus(&on_color, &w, &late);
        assert!(
            (late_gain - expected).abs() < 1e-9,
            "late ramp {late_gain} != {expected}"
        );
        assert!(late_gain > 0.0, "the ramp must actually pay something");
        assert_eq!(color_bonus(&off_color, &w, &late), 0.0);

        // The ramp is wired into `card_value`, not just into the helper.
        let wired = card_value(&on_color, &w, &late) - card_value(&on_color, &w, &early);
        assert!((wired - late_gain).abs() < 1e-9);
        assert_eq!(
            card_value(&off_color, &w, &late),
            card_value(&off_color, &w, &early),
            "an off-colour card is never PENALISED — a penalty is a commitment"
        );
    }

    // ── P3 / P3b: pile math ─────────────────────────────────────────────────

    /// Principle 3's first half. Asserted on the **decomposed** `playables`
    /// term: the aggregate stays green under the `max()` mutation for every
    /// `denial_per_card > 2/3`, so an aggregate-only assertion would not
    /// discriminate.
    #[test]
    fn a_medium_pile_of_playables_beats_one_premium_card() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let w = WinstonWeights::baseline();

        let premium_face = bomb();
        let filler_face = vanilla_creature(3, 3, 2);
        let chaff_face = vanilla_creature(0, 1, 3);

        let premium = card(&premium_face, 4, &none);
        let filler = card(&filler_face, 2, &none);
        let chaff = card(&chaff_face, 3, &none);

        // Reach-guards: the premium card really is the best single card, and the
        // filler really is above replacement. Without both, the `max()` mutation
        // would not change the answer and this test would not discriminate.
        assert!(card_surplus(&premium, &w, &ctx) > card_surplus(&filler, &w, &ctx));
        assert!(card_surplus(&filler, &w, &ctx) > 0.0);
        assert_eq!(card_surplus(&chaff, &w, &ctx), 0.0);

        let pile_a = [premium];
        let pile_b = [filler; 7];

        let a = pile_value(&pile_a, &w, &ctx);
        let b = pile_value(&pile_b, &w, &ctx);
        assert!(
            b.playables > a.playables,
            "seven playables must out-sum one bomb: {b:?} vs {a:?}"
        );
        assert!(b.total() > a.total());

        // Sibling leg: seven cards BELOW replacement are worth nothing as
        // playables. This is what stops "seven lands beat a bomb" — it is the
        // `max(0, ..)` floor, and it is the whole reason `replacement_level`
        // exists.
        let pile_c = [chaff; 7];
        let c = pile_value(&pile_c, &w, &ctx);
        assert_eq!(c.playables, 0.0);
        assert!(c.denial > 0.0);
        assert!(a.total() > c.total());
    }

    /// Principle 3's second half, in two legs. Leg (a) pins denial while the
    /// playables term is pinned at zero; leg (b) is the positive reach-guard that
    /// the playables term is live at all.
    #[test]
    fn denial_rewards_the_larger_pile_at_equal_quality() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let w = WinstonWeights::baseline();
        assert!(w.denial_per_card > 0.0);

        // (a) sub-replacement: identical per-card quality, at or below replacement.
        let chaff_face = vanilla_creature(0, 1, 3);
        let chaff = card(&chaff_face, 3, &none);
        assert_eq!(card_surplus(&chaff, &w, &ctx), 0.0);

        let small = pile_value(&[chaff; 2], &w, &ctx);
        let large = pile_value(&[chaff; 8], &w, &ctx);
        // Absolute first, then exact. The exact leg is expressed in terms of
        // `denial_per_card` and so is vacuously true when that constant is zero —
        // MEASURED: with only the exact leg and the guard above removed, the
        // `denial_per_card = 0.0` mutation stayed GREEN. This line is what
        // reddens it on the behaviour rather than on the guard.
        assert!(
            large.denial > small.denial,
            "a bigger pile must starve the opponent more: {large:?} vs {small:?}"
        );
        assert!((large.denial - small.denial - 6.0 * w.denial_per_card).abs() < 1e-9);
        assert_eq!(large.playables - small.playables, 0.0);
        assert!(
            large.total() > small.total(),
            "and sub-replacement cards alone must still make the bigger pile better"
        );

        // (b) above replacement: the same size gap, and now BOTH terms move.
        let good_face = vanilla_creature(3, 3, 2);
        let good = card(&good_face, 2, &none);
        let surplus = card_surplus(&good, &w, &ctx);
        assert!(
            surplus > 0.0,
            "leg (b) is vacuous unless the card is playable"
        );

        let small = pile_value(&[good; 2], &w, &ctx);
        let large = pile_value(&[good; 8], &w, &ctx);
        assert!(large.denial > small.denial);
        assert!(large.playables > small.playables);
        assert!((large.denial - small.denial - 6.0 * w.denial_per_card).abs() < 1e-9);
        assert!((large.playables - small.playables - 6.0 * surplus).abs() < 1e-9);
    }

    // ── P4a: information gathering, pile-size appetite ──────────────────────

    /// Principle 4's first half. An opponent who takes big piles makes handing
    /// them a bigger pile expensive, which flips a marginal turn from "decline
    /// and keep looking" to "take it now".
    #[test]
    fn an_opponent_that_takes_big_piles_makes_declining_expensive() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let w = WinstonWeights::baseline();

        // The two reads differ ONLY in the large-pile rate. If `handoff_cost`
        // read `small_pile_take_rate` instead, the two costs would be identical
        // and the decomposed assertion below would redden.
        let passive = OpponentRead {
            small_pile_take_rate: 0.0,
            large_pile_take_rate: 0.1,
            passed_colors: ColorPassTally::default(),
            samples: 10,
        };
        let greedy = OpponentRead {
            large_pile_take_rate: 0.9,
            ..passive.clone()
        };

        let chaff_face = vanilla_creature(0, 1, 3);
        let ok_face = vanilla_creature(5, 4, 5);
        let chaff = card(&chaff_face, 3, &none);
        let ok = card(&ok_face, 5, &none);
        // Cursor pile of 3 → the handoff is priced for a pile of 4, which is at
        // or above `opponent_size_split`, so the LARGE rate is the one in play.
        let cursor = [chaff, chaff, ok];
        assert!(cursor.len() + 1 >= w.opponent_size_split);

        // A closure returning the VALUATION rather than the borrowing
        // `WinstonTurn` keeps the fixture identical between the two reads while
        // staying inside one lifetime.
        let valuate = |read: &OpponentRead| {
            valuate_turn(
                &WinstonTurn {
                    cursor_pile: &cursor,
                    later_pile_sizes: &[1, 2],
                    forced_draw_legal: false,
                    pool: &[],
                    read,
                    context: ctx,
                },
                &w,
            )
        };

        // Decomposed: the read moves `handoff_cost` itself.
        let cheap = handoff_cost(cursor.len(), &passive, &w);
        let dear = handoff_cost(cursor.len(), &greedy, &w);
        assert!(
            dear > cheap,
            "a greedy opponent must make declining cost more: {dear} vs {cheap}"
        );

        // Aggregate: and that difference is enough to flip the decision.
        let against_passive = valuate(&passive);
        let against_greedy = valuate(&greedy);
        assert_eq!(
            against_passive.take_now, against_greedy.take_now,
            "only the continuation side may move"
        );
        assert!(
            !against_passive.prefers_taking(),
            "against a passive opponent this turn is worth continuing: {against_passive:?}"
        );
        assert!(
            against_greedy.prefers_taking(),
            "against a greedy one it is not: {against_greedy:?}"
        );

        // Hostile fixture: with no observations at all there is no rate to
        // trust, so the cost falls back to the flat penalty rather than to a
        // number computed from nothing.
        let no_read = OpponentRead::default();
        assert_eq!(no_read.samples, 0);
        assert_eq!(handoff_cost(cursor.len(), &no_read, &w), w.handoff_penalty);
    }

    // ── P4c: information gathering, the colour read ─────────────────────────

    fn tally_of(cards: &[(&str, usize)]) -> ColorPassTally {
        let mut tally = ColorPassTally::default();
        for (color, count) in cards {
            for _ in 0..*count {
                tally.observe(&colors(&[color]));
            }
        }
        tally
    }

    /// Principle 4's second half, both directions on both cards, so a one-sided
    /// term cannot pass. The reading taken is "a colour they keep passing is
    /// OPEN, so it is safer to invest in"; the opposite reading is the sign of
    /// `opponent_pass_color_weight`, which is why flipping it reddens exactly
    /// this test.
    #[test]
    fn a_colour_the_opponent_keeps_passing_gains_value() {
        let w = WinstonWeights::baseline();
        assert!(w.opponent_pass_color_weight > 0.0);

        let white_heavy = tally_of(&[("W", 10), ("U", 2)]);
        let blue_heavy = tally_of(&[("U", 10), ("W", 2)]);
        assert!(white_heavy.cards() >= MIN_PASS_SAMPLE);

        let white = colors(&["W"]);
        let blue = colors(&["U"]);
        let body = vanilla_creature(2, 2, 2);
        let artifact = face(vec![CoreType::Artifact]);
        let no_colors = Vec::new();

        let white_card = card(&body, 2, &white);
        let blue_card = card(&body, 2, &blue);
        let colorless_card = card(&artifact, 2, &no_colors);

        // Four directional assertions: each card gains under the tally that
        // over-passes its colour and LOSES under the one that under-passes it.
        assert!(opponent_pass_bonus(&white_card, &w, &white_heavy) > 0.0);
        assert!(opponent_pass_bonus(&white_card, &w, &blue_heavy) < 0.0);
        assert!(opponent_pass_bonus(&blue_card, &w, &blue_heavy) > 0.0);
        assert!(opponent_pass_bonus(&blue_card, &w, &white_heavy) < 0.0);

        // Wired into `card_value`, on the same two mirrored tallies.
        let value_under = |c: &DraftCardFacts, t: &ColorPassTally| {
            card_value(
                c,
                &w,
                &CardContext {
                    preferred_colors: &no_colors,
                    progress: 0.0,
                    passed: t,
                },
            )
        };
        assert!(value_under(&white_card, &white_heavy) > value_under(&white_card, &blue_heavy));
        assert!(value_under(&blue_card, &blue_heavy) > value_under(&blue_card, &white_heavy));

        // Third leg: below the sample floor there is no read, and the term is
        // EXACTLY zero rather than a small guess.
        let thin = tally_of(&[("W", MIN_PASS_SAMPLE - 1)]);
        assert!(thin.cards() < MIN_PASS_SAMPLE);
        assert_eq!(opponent_pass_bonus(&white_card, &w, &thin), 0.0);
        assert_eq!(opponent_pass_bonus(&blue_card, &w, &thin), 0.0);

        // Fourth leg: a colourless card names no colour, so it gets exactly
        // zero from every tally — the empty maximum, deliberately.
        assert_eq!(opponent_pass_bonus(&colorless_card, &w, &white_heavy), 0.0);
        assert_eq!(opponent_pass_bonus(&colorless_card, &w, &blue_heavy), 0.0);
        assert_eq!(opponent_pass_bonus(&colorless_card, &w, &thin), 0.0);
    }

    // ── P5: interaction is key ──────────────────────────────────────────────

    /// Principle 5. The ORDERING, not a threshold: `cheap > expensive >
    /// creature` pins the cheapness axis and the interaction axis separately.
    #[test]
    fn cheap_interaction_outranks_an_equal_bodied_creature() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let w = WinstonWeights::baseline();

        let cheap_face = removal_spell(2);
        let expensive_face = removal_spell(6);
        let body_face = vanilla_creature(3, 3, 2);
        assert!(2 <= w.cheap_interaction_max_mv && w.cheap_interaction_max_mv < 6);

        let cheap = card(&cheap_face, 2, &none);
        let expensive = card(&expensive_face, 6, &none);
        let body = card(&body_face, 2, &none);

        let cheap_value = card_value(&cheap, &w, &ctx);
        let expensive_value = card_value(&expensive, &w, &ctx);
        let body_value = card_value(&body, &w, &ctx);
        assert!(
            cheap_value > expensive_value,
            "cheap removal stabilises; expensive removal is a luxury: {cheap_value} vs {expensive_value}"
        );
        assert!(
            expensive_value > body_value,
            "interaction outranks a body of comparable base quality: {expensive_value} vs {body_value}"
        );

        // Without the interaction premium, the body wins — so the premium is
        // load-bearing rather than decorative.
        let flat = WinstonWeights {
            interaction_premium: 0.0,
            cheap_interaction_bonus: 0.0,
            ..WinstonWeights::baseline()
        };
        assert!(card_value(&body, &flat, &ctx) > card_value(&expensive, &flat, &ctx));
        assert_eq!(
            card_value(&cheap, &flat, &ctx),
            card_value(&expensive, &flat, &ctx)
        );

        // Stack interaction is the other half of principle 5, and it takes the
        // same premium — both the `counter` fold and the cheapness bonus.
        let counter_face = counterspell(2);
        let counter = card(&counter_face, 2, &none);
        let premiumed = card_value(&counter, &w, &ctx) - card_value(&counter, &flat, &ctx);
        // `cast_facts::is_direct_removal` already counts `Effect::Counter` as
        // removal (pre-existing shared-detector behaviour, not introduced here),
        // so a counterspell collects the interaction premium through BOTH the
        // `removal` and `counter` folds. Pinned at its real value rather than
        // papered over: if that detector ever changes, this reddens and the
        // premium can be re-derived deliberately.
        let expected = 2.0 * w.interaction_premium + w.cheap_interaction_bonus;
        assert!(
            (premiumed - expected).abs() < 1e-9,
            "a cheap counterspell must collect the interaction premium and the cheapness bonus, got {premiumed} (expected {expected})"
        );
    }

    // ── Structural ──────────────────────────────────────────────────────────

    /// The continuation must price the LAST pile and the forced draw. A
    /// Rust-exclusive range over the later piles would silently delete the final
    /// pile, and the bot would systematically under-value it.
    #[test]
    fn the_continuation_prices_every_stopping_point() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let w = WinstonWeights::baseline();
        let read = OpponentRead::default();

        let continuation_of = |sizes: &[usize], forced: bool| {
            continuation_value(
                &WinstonTurn {
                    cursor_pile: &[],
                    later_pile_sizes: sizes,
                    forced_draw_legal: forced,
                    pool: &[],
                    read: &read,
                    context: ctx,
                },
                &w,
            )
        };

        // A fat LAST pile must be visible to the continuation.
        let short = continuation_of(&[1], false);
        let fat_last = continuation_of(&[1, 9], false);
        assert!(
            fat_last > short,
            "the final pile is a stopping point: {fat_last} vs {short}"
        );

        // The forced draw is a stopping point too, but only when the projection
        // publishes that decline as legal.
        let without_draw = continuation_of(&[], false);
        assert_eq!(
            without_draw,
            f64::NEG_INFINITY,
            "no piles and no legal forced draw means there is no continuation at all"
        );
        let with_draw = continuation_of(&[], true);
        assert!(with_draw.is_finite() && with_draw > without_draw);

        // ... and with no continuation, taking is the only option.
        let valuation = valuate_turn(
            &WinstonTurn {
                cursor_pile: &[],
                later_pile_sizes: &[],
                forced_draw_legal: false,
                pool: &[],
                read: &read,
                context: ctx,
            },
            &w,
        );
        assert!(valuation.prefers_taking());
    }

    /// `draft_progress` divides in `f64`. Integer division of
    /// `total_cards / seat_count` truncates and quantises the whole colour ramp.
    #[test]
    fn draft_progress_is_a_float_ratio() {
        let float_answer = draft_progress(1, 89, 2);
        let truncated = 1.0 / (1.0 + (89usize / 2) as f64);
        assert!((float_answer - 1.0 / 45.5).abs() < 1e-12);
        assert!(
            (float_answer - truncated).abs() > 1e-9,
            "integer division would have been indistinguishable here"
        );

        // Runs 0 → 1 monotonically as the pool fills and the remainder drains.
        assert_eq!(draft_progress(0, 90, 2), 0.0);
        assert!(draft_progress(10, 70, 2) < draft_progress(30, 30, 2));
        assert_eq!(draft_progress(45, 0, 2), 1.0);
        // Degenerate inputs must not divide by zero.
        assert_eq!(draft_progress(0, 0, 0), 0.0);
    }

    /// The ladder is exhaustive over all six rungs, and `Easy` is the denial
    /// half of principle 3 alone — a parameterization, not a third strategy
    /// variant.
    #[test]
    fn the_difficulty_ladder_parameterizes_one_scored_strategy() {
        use AiDifficulty::*;

        assert_eq!(
            WinstonStrategy::for_difficulty(VeryEasy),
            WinstonStrategy::FirstLegal
        );
        for (rung, expected) in [
            (Easy, WinstonWeights::denial_only()),
            (Medium, WinstonWeights::baseline()),
            (Hard, WinstonWeights::sharp()),
            (VeryHard, WinstonWeights::sharpest()),
            (CEDH, WinstonWeights::sharpest()),
        ] {
            assert_eq!(
                WinstonStrategy::for_difficulty(rung),
                WinstonStrategy::Scored(Box::new(expected)),
                "{rung:?}"
            );
        }

        // `denial_only` really is denial-only: every card is below an infinite
        // replacement level, so only pile height moves the score — and nothing
        // produces a NaN.
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let easy = WinstonWeights::denial_only();
        let bomb_face = bomb();
        let bomb_card = card(&bomb_face, 4, &none);
        let one_bomb = pile_value(&[bomb_card], &easy, &ctx);
        let three_bombs = pile_value(&[bomb_card; 3], &easy, &ctx);
        assert_eq!(one_bomb.playables, 0.0);
        assert!(one_bomb.total().is_finite());
        assert!(three_bombs.total() > one_bomb.total());

        // Medium — what a pod gets when it names no difficulty — has every
        // principle live. A pod that DOES name one reaches the harder rungs;
        // this is the floor, not the ceiling.
        let medium = WinstonWeights::baseline();
        assert!(medium.fixing_premium > 0.0);
        assert!(medium.color_commitment_max > 0.0);
        assert!(medium.denial_per_card > 0.0);
        assert!(medium.opponent_greed_weight > 0.0);
        assert!(medium.opponent_pass_color_weight > 0.0);
        assert!(medium.interaction_premium > 0.0);
    }

    /// The bot's estimate of an unseen pile comes from cards it has actually
    /// seen, and falls back to the prior until it has enough of them.
    #[test]
    fn the_surplus_sample_falls_back_to_the_prior_while_it_is_thin() {
        let none = Vec::new();
        let tally = ColorPassTally::default();
        let ctx = neutral_context(&none, &tally);
        let w = WinstonWeights::baseline();
        let read = OpponentRead::default();

        let good_face = vanilla_creature(3, 3, 2);
        let good = card(&good_face, 2, &none);
        let pool = [good; MIN_SURPLUS_SAMPLE];

        let thin = WinstonTurn {
            cursor_pile: &[],
            later_pile_sizes: &[],
            forced_draw_legal: false,
            pool: &pool[..1],
            read: &read,
            context: ctx,
        };
        assert_eq!(surplus_sample(&thin, &w).count, 1);
        assert_eq!(
            surplus_sample(&thin, &w).mean_or(w.prior_surplus_per_card),
            w.prior_surplus_per_card
        );

        let full = WinstonTurn {
            pool: &pool,
            ..thin
        };
        let sample = surplus_sample(&full, &w);
        assert_eq!(sample.count, MIN_SURPLUS_SAMPLE);
        let mean = sample.mean_or(w.prior_surplus_per_card);
        assert!((mean - card_surplus(&good, &w, &ctx)).abs() < 1e-9);

        // The cursor pile counts too: it is revealed to this seat, so it is
        // legitimately seen.
        let with_cursor = WinstonTurn {
            cursor_pile: &pool[..2],
            ..full
        };
        assert_eq!(
            surplus_sample(&with_cursor, &w).count,
            MIN_SURPLUS_SAMPLE + 2
        );
    }
}

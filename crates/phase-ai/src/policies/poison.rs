//! `PoisonClockPolicy` — makes the CR 104.3d poison clock visible to an AI
//! whose evaluation is otherwise life-total-centric.
//!
//! ## The defect this closes
//!
//! CR 104.3d: "If a player has ten or more poison counters, that player loses
//! the game the next time a player would receive priority." That is a win
//! condition entirely independent of life total, tracked in a dedicated engine
//! field (`Player.poison_counters`). Nothing in the AI scored progress along
//! it, so an infect/toxic deck's whole plan registered as doing nothing: a
//! proliferate that takes an opponent from 9 to 10 poison scored the same as
//! one that took them from 0 to 1.
//!
//! ## Rules-correctness note that drives the branch structure
//!
//! CR 701.34a defines proliferate over "permanents and/or players that **have
//! a counter**". Proliferating when no opponent is poisoned adds nothing on
//! this axis — so that branch scores zero rather than a nudge. Getting this
//! backwards would push the AI to durdle with proliferate before the clock has
//! started.
//!
//! ## Where each branch is scored
//!
//! The clock advances through three distinct decisions, and each is scored at
//! the seam where it is actually decided:
//!
//! | Decision | Seam | Rule |
//! |---|---|---|
//! | direct poison / proliferate | `CastSpell` · `ActivateAbility` | CR 122.1f · CR 701.34a |
//! | a modal card's poison mode | `SelectModes` | CR 601.2b |
//! | attacking with a poison source | `DeclareAttackers` | CR 702.90b · 702.164c · 702.70a |
//!
//! CR 601.2b makes mode selection a step of *announcing* a spell, so a
//! `CastSpell` candidate has no chosen mode yet and must not be credited with
//! a mode's poison. The unconditional-vs-modal split is expressed once, in
//! [`crate::ability_chain::AbilityScope`], and shared with deck-time detection.
//!
//! ## Performance
//!
//! `verdict()` runs per candidate per search node. Every predicate here is
//! card-local (`obj.abilities` / `obj.keywords`) or a
//! `players::poison_loss_measure` call per live opponent; the combat branch is
//! linear in the candidate's own attack list. Only a candidate whose estimate
//! reaches lethal runs `preview_player_counter_addition`, one state clone per
//! placement; sub-lethal candidates stay card-local.

use engine::game::combat::AttackTarget;
use engine::game::effects::player_counter::{
    preview_player_counter_addition, PlayerCounterAdditionPreview,
};
use engine::game::players::{self, PoisonLossMeasure, PoisonLossScope};
use engine::types::ability::AbilityDefinition;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::{PlayerCounterKind, PlayerId};

use crate::ability_chain::AbilityScope;
use crate::features::poison::{
    opponent_poison_recipients_parts, poison_yield_parts, proliferates_parts, PoisonRecipients,
    POISON_CLOCK_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::{selected_mode_abilities, PolicyContext};
use super::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy, CRITICAL_MAX, STRONG_MAX,
};

pub struct PoisonClockPolicy;

/// One poison clock a candidate advances (CR 810.10: teammates share one), the counters credited
/// to it, the `(recipient, count)` placements the engine resolves (per attacker for combat), and
/// the highest band its claim may reach.
#[derive(Debug, Clone, PartialEq)]
struct PoisonPool {
    measure: PoisonLossMeasure,
    added: u32,
    placements: Vec<(PlayerId, u32)>,
    ceiling: f64,
}

/// What the candidate action contributes to the poison clock. Typed rather
/// than a pair of bools so the branch set stays exhaustive.
#[derive(Debug, Clone, PartialEq)]
enum PoisonContribution {
    /// Adds poison counters to opponents (CR 122.1f); the recipients decide how
    /// many land on one shared pool (CR 810.10).
    DirectPoison(PoisonRecipients),
    /// Proliferates (CR 701.34a) — only advances the clock on an already
    /// poisoned opponent.
    Proliferate,
    /// CR 508.1: an attack declaration whose poison sources are pointed at a
    /// live opponent: the closest pool the declared attackers advance if the
    /// damage connects.
    CombatDamage(PoisonPool),
    /// Nothing to do with the poison clock.
    None,
}

impl PoisonClockPolicy {
    /// Re-classify the LIVE candidate structurally. Deck-time classification is
    /// deliberately not trusted here — the object on the battlefield may have
    /// been modified since the deck was analyzed.
    fn contribution(&self, ctx: &PolicyContext<'_>) -> PoisonContribution {
        match &ctx.candidate.action {
            GameAction::CastSpell { object_id, .. } => match ctx.state.objects.get(object_id) {
                // CR 601.2b + CR 700.2: a modal spell's poison lives in one of
                // its printed modes, and mode selection is a later step of
                // announcing the spell. Crediting the cast would score a branch
                // the AI may never take — `SelectModes` is where it is decided.
                Some(obj) if obj.modal.is_some() => PoisonContribution::None,
                Some(obj) => classify_abilities(obj.abilities.iter()),
                None => PoisonContribution::None,
            },
            GameAction::ActivateAbility {
                source_id,
                ability_index,
                ..
            } => ctx
                .state
                .objects
                .get(source_id)
                .and_then(|obj| obj.abilities.get(*ability_index))
                .map_or(PoisonContribution::None, |ability| {
                    classify_abilities(std::slice::from_ref(ability).iter())
                }),
            // CR 601.2b: the mode IS chosen here — classify exactly the branch
            // the candidate selects, so a poison mode and a non-poison mode of
            // the same card score differently.
            GameAction::SelectModes { indices } => {
                let selected =
                    selected_mode_abilities(ctx.state, &ctx.decision.waiting_for, indices);
                classify_abilities(selected.iter().copied())
            }
            GameAction::DeclareAttackers { attacks, .. } => combat_contribution(ctx, attacks),
            _ => PoisonContribution::None,
        }
    }
}

/// Classify an already-scoped ability set. Direct poison outranks proliferate
/// because it advances the clock without needing an existing counter.
///
/// The walk is always [`AbilityScope::Unconditional`]: every caller has already
/// resolved which branch this candidate commits to, so a still-unchosen mode
/// must not leak in.
fn classify_abilities<'a, I>(abilities: I) -> PoisonContribution
where
    I: IntoIterator<Item = &'a AbilityDefinition> + Clone,
{
    if let Some(recipients) =
        opponent_poison_recipients_parts(abilities.clone(), AbilityScope::Unconditional)
    {
        PoisonContribution::DirectPoison(recipients)
    } else if proliferates_parts(abilities, AbilityScope::Unconditional) {
        PoisonContribution::Proliferate
    } else {
        PoisonContribution::None
    }
}

/// CR 508.1: score an attack declaration by the poison it converts.
///
/// CR 702.90b / CR 702.164c / CR 702.70a all key on combat damage dealt **to a
/// player**, so an attack aimed at a planeswalker or a battle adds nothing on
/// this axis. Poison is summed per poison clock — attackers on one seat, or on
/// teammates who share poison (CR 810.10), share one clock — and the seat
/// closest to its poison-loss threshold is the one scored.
fn combat_contribution(
    ctx: &PolicyContext<'_>,
    attacks: &[(ObjectId, AttackTarget)],
) -> PoisonContribution {
    let gains = attacks.iter().filter_map(|(attacker_id, target)| {
        let AttackTarget::Player(defender) = target else {
            return None;
        };
        let attacker = ctx.state.objects.get(attacker_id)?;
        let yielded = poison_yield_parts(
            &attacker.card_types.core_types,
            &attacker.keywords,
            attacker.power.unwrap_or(0),
        );
        (yielded > 0).then_some((*defender, yielded))
    });
    closest_delivered(
        ctx.state,
        ctx.ai_player,
        clock_pools(ctx.state, ctx.ai_player, gains, STRONG_MAX),
    )
    .map_or(PoisonContribution::None, PoisonContribution::CombatDamage)
}

/// CR 810.10: sum per-seat poison gains into the pool each seat's measure reads.
fn clock_pools(
    state: &GameState,
    ai_player: PlayerId,
    gains: impl IntoIterator<Item = (PlayerId, u32)>,
    ceiling: f64,
) -> Vec<PoisonPool> {
    // Keyed by the seat that opened the pool.
    let mut pools: Vec<(PlayerId, PoisonPool)> = Vec::new();
    for (seat, gained) in gains {
        let Some(measure) = live_opponent_poison(state, ai_player, seat) else {
            continue;
        };
        let shares_clock = |other: &PlayerId| {
            *other == seat
                || (measure.scope == PoisonLossScope::Team
                    && players::teammates(state, seat).contains(other))
        };
        match pools.iter_mut().find(|(other, _)| shares_clock(other)) {
            Some((_, pool)) => {
                pool.added = pool.added.saturating_add(gained);
                pool.placements.push((seat, gained));
            }
            None => pools.push((
                seat,
                PoisonPool {
                    measure,
                    added: gained,
                    placements: vec![(seat, gained)],
                    ceiling,
                },
            )),
        }
    }
    pools.into_iter().map(|(_, pool)| pool).collect()
}

/// One single-counter pool per live opponent seat whose measure passes `keep`: a choose-one
/// recipient, or proliferate (CR 701.34b: a shared-poison team gets one counter).
fn seat_pools(
    state: &GameState,
    ai_player: PlayerId,
    keep: impl Fn(&PoisonLossMeasure) -> bool,
    ceiling: f64,
) -> Vec<PoisonPool> {
    opponent_poison_seats(state, ai_player)
        .filter(|(_, measure)| keep(measure))
        .map(|(seat, measure)| PoisonPool {
            measure,
            added: 1,
            placements: vec![(seat, 1)],
            ceiling,
        })
        .collect()
}

/// CR 614.1 + CR 614.17: the poison counters one placement actually gives, read through the
/// engine's replacement pipeline; `None` when a replacement choice (CR 616.1) or event rewrite
/// leaves the result unestablished.
pub(crate) fn previewed_poison(
    state: &GameState,
    actor: PlayerId,
    recipient: PlayerId,
    count: u32,
) -> Option<u32> {
    match preview_player_counter_addition(state, actor, recipient, PlayerCounterKind::Poison, count)
    {
        PlayerCounterAdditionPreview::Applied { count }
        | PlayerCounterAdditionPreview::Transformed { count } => Some(count),
        PlayerCounterAdditionPreview::Prevented => Some(0),
        PlayerCounterAdditionPreview::ChoiceRequired { .. }
        | PlayerCounterAdditionPreview::Unsupported => None,
    }
}

/// CR 101.2: a lethal estimate is replaced by what the engine would deliver, since a can't
/// effect (Solemnity) or a replacement decides how many counters land.
fn confirm_lethal(state: &GameState, actor: PlayerId, mut pool: PoisonPool) -> PoisonPool {
    // The preview clones the state, so only an estimate that would claim lethal pays for one.
    if !reaches_lethal(pool.measure, pool.added) {
        return pool;
    }
    let delivered = pool
        .placements
        .iter()
        .try_fold(0u32, |total, &(recipient, count)| {
            Some(total.saturating_add(previewed_poison(state, actor, recipient, count)?))
        });
    match delivered {
        Some(delivered) => pool.added = delivered,
        None => pool.ceiling = pool.ceiling.min(STRONG_MAX),
    }
    pool
}

/// The confirmed pool with the fewest counters left to lethal.
fn closest_delivered(
    state: &GameState,
    actor: PlayerId,
    pools: Vec<PoisonPool>,
) -> Option<PoisonPool> {
    pools
        .into_iter()
        .map(|pool| confirm_lethal(state, actor, pool))
        .filter(|pool| pool.added > 0)
        .min_by_key(|pool| headroom(pool.measure).saturating_sub(pool.added))
}

/// Counters still needed before this measure is lethal.
fn headroom(measure: PoisonLossMeasure) -> u32 {
    measure.threshold.saturating_sub(measure.count)
}

/// CR 102.3 + CR 704.5c / CR 704.6b: each live opponent seat of `ai_player` with its
/// poison-loss measure.
pub(crate) fn opponent_poison_seats(
    state: &GameState,
    ai_player: PlayerId,
) -> impl Iterator<Item = (PlayerId, PoisonLossMeasure)> + '_ {
    players::opponents(state, ai_player)
        .into_iter()
        .filter_map(move |seat| live_opponent_poison(state, ai_player, seat).map(|m| (seat, m)))
}

/// CR 102.3 + CR 800.4: this seat's poison-loss measure, or `None` when it is
/// not a live opponent of `ai_player` or no poison-loss rule applies to it.
pub(crate) fn live_opponent_poison(
    state: &GameState,
    ai_player: PlayerId,
    seat: PlayerId,
) -> Option<PoisonLossMeasure> {
    if !(players::is_opponent(state, ai_player, seat) && players::is_alive(state, seat)) {
        return None;
    }
    let player = state.players.iter().find(|player| player.id == seat)?;
    players::poison_loss_measure(state, player)
}

/// CR 704.5c / CR 704.6b: would `added` more counters reach the measure's threshold?
pub(crate) fn reaches_lethal(measure: PoisonLossMeasure, added: u32) -> bool {
    PoisonLossMeasure {
        count: measure.count.saturating_add(added),
        ..measure
    }
    .is_lethal()
}

impl TacticalPolicy for PoisonClockPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::PoisonClock
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        // `SelectModes` routes through `ActivateAbility` (decision_kind.rs maps
        // both `ModeChoice` and `AbilityModeChoice` to that bucket).
        &[
            DecisionKind::CastSpell,
            DecisionKind::ActivateAbility,
            DecisionKind::DeclareAttackers,
        ]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.poison.commitment < POISON_CLOCK_FLOOR {
            None
        } else {
            Some(features.poison.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let scalar = ctx.config.policy_penalties.poison_clock_pressure;
        match self.contribution(ctx) {
            PoisonContribution::None => {
                PolicyVerdict::neutral(PolicyReason::new("poison_clock_na"))
            }
            // One counter per recipient is the floor: `GivePlayerCounter.count`
            // need not be static. A resolving spell's lethal case reaches
            // `CRITICAL_MAX` only once `confirm_lethal` has read the counters
            // through the engine.
            PoisonContribution::DirectPoison(recipients) => {
                let pools = match recipients {
                    PoisonRecipients::One => {
                        seat_pools(ctx.state, ctx.ai_player, |_| true, CRITICAL_MAX)
                    }
                    // CR 810.10: each opponent gets its counter individually; teammates' counters share one pool.
                    PoisonRecipients::EachOpponent => clock_pools(
                        ctx.state,
                        ctx.ai_player,
                        players::opponents(ctx.state, ctx.ai_player)
                            .into_iter()
                            .map(|seat| (seat, 1)),
                        CRITICAL_MAX,
                    ),
                };
                match closest_delivered(ctx.state, ctx.ai_player, pools) {
                    Some(pool) => score_clock(scalar, pool.measure, pool.added, pool.ceiling),
                    None => {
                        PolicyVerdict::neutral(PolicyReason::new("poison_clock_no_poison_loss"))
                    }
                }
            }
            PoisonContribution::Proliferate => {
                // CR 701.34b: one proliferate gives a shared-poison team a single counter.
                // CR 701.34a: proliferate chooses among permanents and players
                // that ALREADY have a counter. With no poisoned opponent it
                // advances nothing, so it earns nothing on this axis.
                let pools = seat_pools(
                    ctx.state,
                    ctx.ai_player,
                    |measure| measure.count > 0,
                    CRITICAL_MAX,
                );
                match closest_delivered(ctx.state, ctx.ai_player, pools) {
                    Some(pool) => score_clock(scalar, pool.measure, pool.added, pool.ceiling),
                    None => PolicyVerdict::neutral(PolicyReason::new(
                        "poison_clock_no_counters_to_proliferate",
                    )),
                }
            }
            // CR 509.1a: declared combat damage is not guaranteed — the attack
            // can be blocked or prevented — so even a would-be-lethal poison
            // swing is held below the critical band (`STRONG_MAX` ceiling) that
            // a guaranteed direct poison earns. A committed attacker is still a
            // strong play, just not a booked win.
            PoisonContribution::CombatDamage(pool) => {
                score_clock(scalar, pool.measure, pool.added, pool.ceiling)
            }
        }
    }
}

/// Floor on the progress multiplier so the first counters still read as a real
/// play rather than a rounding error — a clock at 1/10 is worth more than
/// one tenth of a clock at 10/10, because it is the only way to reach ten.
const MIN_CLOCK_PROGRESS: f64 = 0.25;

/// Shared scoring for every branch: reaching the measure's threshold is the top
/// of the scale, otherwise scaled by how far the clock has run — the last
/// counters are worth more than the first.
///
/// `ceiling` is the highest band this branch may reach: `CRITICAL_MAX` for a
/// counter confirmed through the engine (a resolving direct-poison spell), `STRONG_MAX` for a
/// merely-declared one (a combat swing that can still be blocked). Both
/// magnitudes are state- and config-dependent, so they route through
/// `PolicyVerdict::score`, which selects the band from the clamped value. The
/// sub-lethal case is always held under `STRONG_MAX`, so an advancing-but-not-
/// lethal clock never outranks a booked win.
fn score_clock(scalar: f64, measure: PoisonLossMeasure, added: u32, ceiling: f64) -> PolicyVerdict {
    let facts = |reason: PolicyReason| {
        reason
            .with_fact("opponent_poison", i64::from(measure.count))
            .with_fact("poison_added", i64::from(added))
            .with_fact("poison_threshold", i64::from(measure.threshold))
    };

    if reaches_lethal(measure, added) {
        return PolicyVerdict::score(
            scalar.min(ceiling),
            facts(PolicyReason::new("poison_clock_lethal")),
        );
    }

    let projected = measure.count.saturating_add(added);
    let progress = f64::from(projected) / f64::from(measure.threshold);
    PolicyVerdict::score(
        scalar.min(STRONG_MAX) * progress.max(MIN_CLOCK_PROGRESS),
        facts(PolicyReason::new("poison_clock_pressure")),
    )
}

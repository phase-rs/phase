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
//! linear in the candidate's own attack list. Only a pool whose estimate
//! reaches lethal, or a direct poison whose printed count is dynamic, pays for
//! one state clone, on which the engine casts the candidate and resolves the
//! stack, or deals the unblocked combat damage (and the attackers' poison
//! triggers it fires); sub-lethal candidates stay card-local.

use engine::ai_support::{current_target_selection_targets, legal_actions};
use engine::game::combat::AttackTarget;
use engine::game::combat_damage::preview_unblocked_combat_damage;
use engine::game::effects::player_counter::{
    preview_player_counter_addition, PlayerCounterAdditionPreview,
};
use engine::game::engine::apply_as_current_for_simulation;
use engine::game::players::{self, PoisonLossMeasure, PoisonLossScope};
use engine::game::sba::check_state_based_actions;
use engine::types::ability::{AbilityDefinition, Effect, QuantityExpr, ResolvedAbility, TargetRef};
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, GameState, StackEntryKind, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::{PlayerCounterKind, PlayerId};

use crate::ability_chain::{collect_scoped_nodes, AbilityScope};
use crate::features::poison::{
    opponent_poison_recipients_parts, poison_yield_parts, proliferates_parts, PoisonRecipients,
    POISON_CLOCK_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::{pending_mode_source, selected_mode_abilities, PolicyContext};
use super::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy, CRITICAL_MAX, STRONG_MAX,
};

pub struct PoisonClockPolicy;

/// One poison clock a candidate advances (CR 810.10: teammates share one), the seat that opened
/// it, the counters credited to it, and whether those counters may claim lethal.
#[derive(Debug, Clone, PartialEq)]
struct PoisonPool {
    seat: PlayerId,
    measure: PoisonLossMeasure,
    added: u32,
    delivery: Delivery,
}

/// Whether a pool's credited counters count toward lethal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Delivery {
    /// The engine read the delivery, or the estimate is short of lethal and needs no read.
    Counted,
    /// The delivery was not read, or it did not end with the seat out and the AI in, so its
    /// estimate stops one counter short of lethal.
    Unread,
}

impl PoisonPool {
    /// The counters that count toward lethal: an unread delivery stops one short of it.
    fn credited(&self) -> u32 {
        match self.delivery {
            Delivery::Counted => self.added,
            Delivery::Unread => self.added.min(headroom(self.measure).saturating_sub(1)),
        }
    }
}

/// What the candidate action contributes to the poison clock. Typed rather
/// than a pair of bools so the branch set stays exhaustive.
#[derive(Debug, Clone, PartialEq)]
enum PoisonContribution<'a> {
    /// Adds poison counters to opponents (CR 122.1f); the recipients decide how
    /// many land on one shared pool (CR 810.10). A lethal claim is confirmed by
    /// casting the candidate on a copy and resolving the stack.
    DirectPoison {
        recipients: PoisonRecipients,
        abilities: Vec<&'a AbilityDefinition>,
    },
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
    fn contribution<'a>(&self, ctx: &PolicyContext<'a>) -> PoisonContribution<'a> {
        match &ctx.candidate.action {
            GameAction::DeclareAttackers { attacks, .. } => combat_contribution(ctx, attacks),
            _ => candidate_abilities(ctx).map_or(PoisonContribution::None, classify_abilities),
        }
    }
}

/// The ability set a cast, activation, or mode selection commits to.
fn candidate_abilities<'a>(ctx: &PolicyContext<'a>) -> Option<Vec<&'a AbilityDefinition>> {
    let state = ctx.state;
    match &ctx.candidate.action {
        GameAction::CastSpell { object_id, .. } => {
            let obj = state.objects.get(object_id)?;
            // CR 601.2b + CR 700.2: a modal spell's poison lives in one of
            // its printed modes, and mode selection is a later step of
            // announcing the spell. Crediting the cast would score a branch
            // the AI may never take — `SelectModes` is where it is decided.
            obj.modal.is_none().then(|| obj.abilities.iter().collect())
        }
        GameAction::ActivateAbility {
            source_id,
            ability_index,
            ..
        } => {
            let ability = state
                .objects
                .get(source_id)?
                .abilities
                .get(*ability_index)?;
            Some(vec![ability])
        }
        // CR 601.2b: the mode IS chosen here — classify exactly the branch
        // the candidate selects, so a poison mode and a non-poison mode of
        // the same card score differently.
        GameAction::SelectModes { indices } => Some(selected_mode_abilities(
            state,
            &ctx.decision.waiting_for,
            indices,
        )),
        _ => None,
    }
}

/// Classify an already-scoped ability set. Direct poison outranks proliferate
/// because it advances the clock without needing an existing counter.
///
/// The walk is always [`AbilityScope::Unconditional`]: every caller has already
/// resolved which branch this candidate commits to, so a still-unchosen mode
/// must not leak in.
fn classify_abilities(abilities: Vec<&AbilityDefinition>) -> PoisonContribution<'_> {
    if let Some(recipients) =
        opponent_poison_recipients_parts(abilities.iter().copied(), AbilityScope::Unconditional)
    {
        PoisonContribution::DirectPoison {
            recipients,
            abilities,
        }
    } else if proliferates_parts(abilities.iter().copied(), AbilityScope::Unconditional) {
        PoisonContribution::Proliferate
    } else {
        PoisonContribution::None
    }
}

/// The poison counters each recipient gets as printed; `None` when any count is dynamic.
fn printed_poison(abilities: &[&AbilityDefinition]) -> Option<u32> {
    abilities
        .iter()
        .flat_map(|ability| collect_scoped_nodes(ability, AbilityScope::Unconditional))
        .filter_map(|node| match &*node.effect {
            Effect::GivePlayerCounter {
                counter_kind: PlayerCounterKind::Poison,
                count,
                ..
            } => Some(count),
            _ => None,
        })
        .try_fold(0u32, |total, count| match count {
            QuantityExpr::Fixed { value } => {
                Some(total.saturating_add(u32::try_from(*value).unwrap_or(0)))
            }
            _ => None,
        })
}

/// An answer that never moves its prompt forward would otherwise loop forever.
const CANDIDATE_DELIVERY_MAX_STEPS: usize = 64;

/// CR 601.2i + CR 603.3 + CR 117.4: the board after the candidate, cast or activated on a copy
/// with its mana paid automatically, resolves and every player passes until the stack is back to
/// its depth. None when the cast asks the AI anything but its targets, when no choice of targets
/// brings it to priority alone on top of the stack, or when another player owns a choice as it
/// resolves.
fn candidate_delivery(
    state: &GameState,
    ai_player: PlayerId,
    action: &GameAction,
    seat: PlayerId,
) -> Option<GameState> {
    let chosen = TargetRef::Player(seat);
    let mut board = state.clone();
    // CR 601.2a: a spell whose modes are being chosen is already on the stack, so the depth is
    // the stack below it.
    let depth = pending_mode_source(&board.waiting_for)
        .and_then(|spell| board.stack.iter().position(|entry| entry.id == spell))
        .unwrap_or(board.stack.len());
    let mut action = action.clone();
    if let GameAction::CastSpell { payment_mode, .. }
    | GameAction::ActivateAbility { payment_mode, .. } = &mut action
    {
        *payment_mode = CastPaymentMode::Auto;
    }
    apply_as_current_for_simulation(&mut board, action).ok()?;
    let mut steps = CANDIDATE_DELIVERY_MAX_STEPS;
    let mut board = settled_cast(board, ai_player, depth, &chosen, &mut steps)?;
    while let Some(left) = steps.checked_sub(1) {
        steps = left;
        let answer = match &board.waiting_for {
            WaitingFor::GameOver { .. } => return Some(board),
            WaitingFor::Priority { .. } if board.stack.len() <= depth => return Some(board),
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            waiting_for if waiting_for.acting_player() != Some(ai_player) => return None,
            WaitingFor::ProliferateChoice { eligible, .. } if eligible.contains(&chosen) => {
                GameAction::SelectTargets {
                    targets: vec![chosen.clone()],
                }
            }
            _ => legal_actions(&board).into_iter().next()?,
        };
        apply_as_current_for_simulation(&mut board, answer).ok()?;
    }
    None
}

/// CR 601.2c + CR 601.2i + CR 603.3: the cast stepped to priority with the candidate alone above
/// `depth`, trying each of the AI's legal targets in turn (`chosen` first) on its own copy; None
/// when no choice gets there.
fn settled_cast(
    board: GameState,
    ai_player: PlayerId,
    depth: usize,
    chosen: &TargetRef,
    steps: &mut usize,
) -> Option<GameState> {
    match &board.waiting_for {
        WaitingFor::GameOver { .. } => Some(board),
        // CR 603.3: a trigger the cast caused sits above the candidate and resolves first.
        WaitingFor::Priority { .. } => {
            let alone = board.stack.len() == depth + 1
                && board.stack.last().is_some_and(|top| {
                    !matches!(top.kind, StackEntryKind::TriggeredAbility { .. })
                });
            alone.then_some(board)
        }
        WaitingFor::TargetSelection { .. }
            if board.waiting_for.acting_player() == Some(ai_player) =>
        {
            let (seat, rest): (Vec<_>, Vec<_>) = current_target_selection_targets(&board)?
                .iter()
                .cloned()
                .partition(|target| target == chosen);
            seat.into_iter().chain(rest).find_map(|target| {
                *steps = steps.checked_sub(1)?;
                let mut attempt = board.clone();
                let answer = GameAction::ChooseTarget {
                    target: Some(target),
                };
                apply_as_current_for_simulation(&mut attempt, answer).ok()?;
                settled_cast(attempt, ai_player, depth, chosen, steps)
            })
        }
        _ => None,
    }
}

/// CR 702.70a: a trigger whose instructions give poison counters (Poisonous).
fn gives_poison(ability: &ResolvedAbility) -> bool {
    matches!(
        ability.effect,
        Effect::GivePlayerCounter {
            counter_kind: PlayerCounterKind::Poison,
            ..
        }
    ) || ability.sub_ability.as_deref().is_some_and(gives_poison)
}

/// CR 508.1: score an attack declaration by the poison it converts.
///
/// CR 702.90b / CR 702.164c / CR 702.70a all key on combat damage dealt **to a
/// player**, so an attack aimed at a planeswalker or a battle adds nothing on
/// this axis. Poison is summed per poison clock — attackers on one seat, or on
/// teammates who share poison (CR 810.10), share one clock — and the seat
/// closest to its poison-loss threshold is the one scored.
fn combat_contribution<'a>(
    ctx: &PolicyContext<'a>,
    attacks: &[(ObjectId, AttackTarget)],
) -> PoisonContribution<'a> {
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
    let at_players: Vec<(ObjectId, PlayerId)> = attacks
        .iter()
        .filter_map(|(attacker, target)| match target {
            AttackTarget::Player(player) => Some((*attacker, *player)),
            _ => None,
        })
        .collect();
    closest_delivered(
        ctx.ai_player,
        clock_pools(ctx.state, ctx.ai_player, gains),
        |_| preview_unblocked_combat_damage(ctx.state, &at_players, gives_poison),
        false,
    )
    .map_or(PoisonContribution::None, PoisonContribution::CombatDamage)
}

/// CR 810.10: sum per-seat poison gains into the pool each seat's measure reads.
fn clock_pools(
    state: &GameState,
    ai_player: PlayerId,
    gains: impl IntoIterator<Item = (PlayerId, u32)>,
) -> Vec<PoisonPool> {
    let mut pools: Vec<PoisonPool> = Vec::new();
    for (seat, gained) in gains {
        let Some(measure) = live_opponent_poison(state, ai_player, seat) else {
            continue;
        };
        let shares_clock = |other: &PlayerId| {
            *other == seat
                || (measure.scope == PoisonLossScope::Team
                    && players::teammates(state, seat).contains(other))
        };
        match pools.iter_mut().find(|pool| shares_clock(&pool.seat)) {
            Some(pool) => pool.added = pool.added.saturating_add(gained),
            None => pools.push(PoisonPool {
                seat,
                measure,
                added: gained,
                delivery: Delivery::Counted,
            }),
        }
    }
    pools
}

/// One `added`-counter pool per live opponent seat whose measure passes `keep`: a choose-one
/// recipient, or proliferate (CR 701.34b: a shared-poison team gets one counter).
fn seat_pools(
    state: &GameState,
    ai_player: PlayerId,
    added: u32,
    keep: impl Fn(&PoisonLossMeasure) -> bool,
) -> Vec<PoisonPool> {
    opponent_poison_seats(state, ai_player)
        .filter(|(_, measure)| keep(measure))
        .map(|(seat, measure)| PoisonPool {
            seat,
            measure,
            added,
            delivery: Delivery::Counted,
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

/// CR 101.2 + CR 614.1: the engine's resolution decides what lands, since a can't effect
/// (Solemnity) or a replacement (Vorinclex) can change every counter, so a lethal estimate is
/// replaced by the pool's poison on the board `deliver` returns for its seat. `must_resolve`
/// reads it even below lethal, for a count with no printed estimate. A delivery the engine
/// could not read, or that does not end with the seat out and the AI in, is `Delivery::Unread`.
fn confirm_lethal(
    ai_player: PlayerId,
    mut pool: PoisonPool,
    deliver: impl Fn(PlayerId) -> Option<GameState>,
    must_resolve: bool,
) -> PoisonPool {
    // The delivery clones the state, so only an estimate that would claim lethal pays for one.
    if !must_resolve && !reaches_lethal(pool.measure, pool.added) {
        return pool;
    }
    let Some(mut after) = deliver(pool.seat) else {
        pool.delivery = Delivery::Unread;
        return pool;
    };
    check_state_based_actions(&mut after, &mut Vec::new());
    // CR 704.3 + CR 104.4a: lethal is the seat out of the game and the AI still in it, since every
    // remaining player losing at once is a draw.
    if !players::is_alive(&after, pool.seat) {
        if players::is_alive(&after, ai_player) {
            pool.added = pool.added.max(headroom(pool.measure));
        } else {
            pool.delivery = Delivery::Unread;
        }
        return pool;
    }
    match live_opponent_poison(&after, ai_player, pool.seat) {
        Some(measure) => {
            pool.added = measure.count.saturating_sub(pool.measure.count);
            // A seat still in the game at its threshold has not lost.
            if reaches_lethal(pool.measure, pool.added) {
                pool.delivery = Delivery::Unread;
            }
        }
        None => pool.delivery = Delivery::Unread,
    }
    pool
}

/// The confirmed pool with the fewest counters left to lethal.
fn closest_delivered(
    ai_player: PlayerId,
    pools: Vec<PoisonPool>,
    deliver: impl Fn(PlayerId) -> Option<GameState>,
    must_resolve: bool,
) -> Option<PoisonPool> {
    pools
        .into_iter()
        .map(|pool| confirm_lethal(ai_player, pool, &deliver, must_resolve))
        .filter(|pool| pool.added > 0)
        .min_by_key(|pool| headroom(pool.measure).saturating_sub(pool.credited()))
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
            // The printed fixed count is the estimate, and a dynamic count is always resolved
            // (CR 107.1b: it can be zero). A lethal case reaches `CRITICAL_MAX` only once
            // `confirm_lethal` has read the counters through the engine.
            PoisonContribution::DirectPoison {
                recipients,
                abilities,
            } => {
                let printed = printed_poison(&abilities);
                let estimate = printed.unwrap_or(1);
                let pools = match recipients {
                    PoisonRecipients::One => {
                        seat_pools(ctx.state, ctx.ai_player, estimate, |_| true)
                    }
                    // CR 810.10: each opponent gets its counter individually; teammates' counters share one pool.
                    PoisonRecipients::EachOpponent => clock_pools(
                        ctx.state,
                        ctx.ai_player,
                        players::opponents(ctx.state, ctx.ai_player)
                            .into_iter()
                            .map(|seat| (seat, estimate)),
                    ),
                };
                let deliver = |seat| {
                    candidate_delivery(ctx.state, ctx.ai_player, &ctx.candidate.action, seat)
                };
                match closest_delivered(ctx.ai_player, pools, deliver, printed.is_none()) {
                    Some(pool) => score_clock(scalar, &pool, CRITICAL_MAX),
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
                let pools = seat_pools(ctx.state, ctx.ai_player, 1, |measure| measure.count > 0);
                let deliver = |seat| {
                    candidate_delivery(ctx.state, ctx.ai_player, &ctx.candidate.action, seat)
                };
                match closest_delivered(ctx.ai_player, pools, deliver, false) {
                    Some(pool) => score_clock(scalar, &pool, CRITICAL_MAX),
                    None => PolicyVerdict::neutral(PolicyReason::new(
                        "poison_clock_no_counters_to_proliferate",
                    )),
                }
            }
            // CR 509.1a: declared combat damage is not guaranteed — the attack
            // can be blocked or prevented — so even a would-be-lethal poison
            // swing is held below the critical band (`STRONG_MAX` ceiling) that
            // a direct poison confirmed through the engine earns. A committed attacker is still a
            // strong play, just not a booked win.
            PoisonContribution::CombatDamage(pool) => score_clock(scalar, &pool, STRONG_MAX),
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
fn score_clock(scalar: f64, pool: &PoisonPool, ceiling: f64) -> PolicyVerdict {
    let PoisonPool { measure, added, .. } = *pool;
    let facts = |reason: PolicyReason| {
        reason
            .with_fact("opponent_poison", i64::from(measure.count))
            .with_fact("poison_added", i64::from(added))
            .with_fact("poison_threshold", i64::from(measure.threshold))
    };

    let credited = pool.credited();
    if reaches_lethal(measure, credited) {
        return PolicyVerdict::score(
            scalar.min(ceiling),
            facts(PolicyReason::new("poison_clock_lethal")),
        );
    }

    let projected = measure.count.saturating_add(credited);
    let progress = f64::from(projected) / f64::from(measure.threshold);
    PolicyVerdict::score(
        scalar.min(STRONG_MAX) * progress.max(MIN_CLOCK_PROGRESS),
        facts(PolicyReason::new("poison_clock_pressure")),
    )
}

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
//! card-local (`obj.abilities` / `obj.keywords`) or a plain `u32` field read on
//! `Player.poison_counters`; the combat branch is linear in the candidate's own
//! attack list. No board-wide sweep, no affordability call, no
//! `find_legal_targets` — nothing this policy touches is on the documented
//! inner-loop landmine list.

use engine::game::ability_utils::modal_spell_mode_ability_refs;
use engine::game::combat::AttackTarget;
use engine::types::ability::AbilityDefinition;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use crate::ability_chain::AbilityScope;
use crate::features::poison::{
    gives_opponents_poison_parts, poison_yield_parts, proliferates_parts, LETHAL_POISON,
    POISON_CLOCK_FLOOR,
};
use crate::features::DeckFeatures;

use super::context::PolicyContext;
use super::registry::{
    DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy, STRONG_MAX,
};

pub struct PoisonClockPolicy;

/// What the candidate action contributes to the poison clock. Typed rather
/// than a pair of bools so the branch set stays exhaustive.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PoisonContribution {
    /// Adds poison counters to a player who can be an opponent (CR 122.1f).
    DirectPoison,
    /// Proliferates (CR 701.34a) — only advances the clock on an already
    /// poisoned opponent.
    Proliferate,
    /// CR 508.1: an attack declaration whose poison sources are pointed at a
    /// live opponent. `current` is that defender's poison total now, `added`
    /// what the declared attackers convert if the damage connects.
    CombatDamage { current: u32, added: u32 },
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
                let modes = pending_mode_abilities(ctx.state, &ctx.decision.waiting_for);
                let selected: Vec<&AbilityDefinition> = indices
                    .iter()
                    .filter_map(|index| modes.get(*index).copied())
                    .collect();
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
    if gives_opponents_poison_parts(abilities.clone(), AbilityScope::Unconditional) {
        PoisonContribution::DirectPoison
    } else if proliferates_parts(abilities, AbilityScope::Unconditional) {
        PoisonContribution::Proliferate
    } else {
        PoisonContribution::None
    }
}

/// CR 700.2: the modes a pending `SelectModes` decision is choosing among.
/// A modal SPELL carries them as the spell-kind abilities of the object being
/// cast (`modal_spell_mode_ability_refs`, the engine's authority, which
/// `handle_select_modes` indexes with the same `indices`); a modal activated or
/// triggered ABILITY carries them on the waiting payload.
fn pending_mode_abilities<'a>(
    state: &'a GameState,
    waiting_for: &'a WaitingFor,
) -> Vec<&'a AbilityDefinition> {
    match waiting_for {
        WaitingFor::ModeChoice { pending_cast, .. } => state
            .objects
            .get(&pending_cast.object_id)
            .map(|obj| modal_spell_mode_ability_refs(obj).collect())
            .unwrap_or_default(),
        WaitingFor::AbilityModeChoice { mode_abilities, .. } => mode_abilities.iter().collect(),
        _ => Vec::new(),
    }
}

/// CR 508.1: score an attack declaration by the poison it converts.
///
/// CR 702.90b / CR 702.164c / CR 702.70a all key on combat damage dealt **to a
/// player**, so an attack aimed at a planeswalker or a battle adds nothing on
/// this axis (CR 506.4c). Poison is summed per defending player — several
/// infect creatures attacking the same seat share one clock — and the seat
/// closest to CR 104.3d's ten is the one scored.
fn combat_contribution(
    ctx: &PolicyContext<'_>,
    attacks: &[(ObjectId, AttackTarget)],
) -> PoisonContribution {
    // (defending seat, its poison now, poison this declaration would add).
    let mut per_defender: Vec<(PlayerId, u32, u32)> = Vec::new();
    for (attacker_id, target) in attacks {
        let AttackTarget::Player(defender) = target else {
            continue;
        };
        let Some(current) = live_opponent_poison(ctx.state, ctx.ai_player, *defender) else {
            continue;
        };
        let Some(attacker) = ctx.state.objects.get(attacker_id) else {
            continue;
        };
        let yielded = poison_yield_parts(
            &attacker.card_types.core_types,
            &attacker.keywords,
            attacker.power.unwrap_or(0),
        );
        if yielded == 0 {
            continue;
        }
        match per_defender.iter_mut().find(|(seat, ..)| seat == defender) {
            Some((_, _, added)) => *added = added.saturating_add(yielded),
            None => per_defender.push((*defender, current, yielded)),
        }
    }

    per_defender
        .into_iter()
        .max_by_key(|(_, current, added)| current.saturating_add(*added))
        .map_or(PoisonContribution::None, |(_, current, added)| {
            PoisonContribution::CombatDamage { current, added }
        })
}

/// CR 104.3d: the highest poison total among the AI's LIVE opponents.
///
/// CR 800.4: a multiplayer game continues after a player leaves, and the
/// eliminated seat stays in `GameState.players` — so a dead player's counters
/// must not be read as pressure the AI is still applying.
pub(crate) fn most_poisoned_opponent(state: &GameState, ai_player: PlayerId) -> u32 {
    state
        .players
        .iter()
        .filter(|player| player.id != ai_player && !player.is_eliminated)
        .map(|player| player.poison_counters)
        .max()
        .unwrap_or(0)
}

/// CR 104.3d + CR 800.4: this seat's poison total, or `None` when it is not a
/// live opponent of `ai_player`.
pub(crate) fn live_opponent_poison(
    state: &GameState,
    ai_player: PlayerId,
    seat: PlayerId,
) -> Option<u32> {
    state
        .players
        .iter()
        .find(|player| player.id == seat && player.id != ai_player && !player.is_eliminated)
        .map(|player| player.poison_counters)
}

/// CR 104.3d: would `added` more poison counters put this player at ten or
/// more, losing them the game the next time a player would receive priority?
pub(crate) fn reaches_lethal(current_poison: u32, added: u32) -> bool {
    current_poison.saturating_add(added) >= LETHAL_POISON
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
            // A single counter is the conservative floor: `GivePlayerCounter.count`
            // is a `QuantityExpr` that need not be statically known.
            PoisonContribution::DirectPoison => {
                score_clock(scalar, most_poisoned_opponent(ctx.state, ctx.ai_player), 1)
            }
            PoisonContribution::Proliferate => {
                let highest = most_poisoned_opponent(ctx.state, ctx.ai_player);
                // CR 701.34a: proliferate chooses among permanents and players
                // that ALREADY have a counter. With no poisoned opponent it
                // advances nothing, so it earns nothing on this axis.
                if highest == 0 {
                    return PolicyVerdict::neutral(PolicyReason::new(
                        "poison_clock_no_counters_to_proliferate",
                    ));
                }
                score_clock(scalar, highest, 1)
            }
            PoisonContribution::CombatDamage { current, added } => {
                score_clock(scalar, current, added)
            }
        }
    }
}

/// Floor on the progress multiplier so the first counters still read as a real
/// play rather than a rounding error — a clock at 1/10 is worth more than
/// one tenth of a clock at 10/10, because it is the only way to reach ten.
const MIN_CLOCK_PROGRESS: f64 = 0.25;

/// Shared scoring for every branch: game-ending when the action reaches CR
/// 104.3d's ten, otherwise scaled by how far the clock has run — the last
/// counters are worth more than the first.
///
/// Both magnitudes are state- and config-dependent, so they route through
/// `PolicyVerdict::score`, which selects the band. The sub-lethal ceiling is
/// held at `STRONG_MAX` so only an action that actually reaches ten can land in
/// the critical band; an advancing-but-not-lethal clock must never outrank a
/// genuine win.
fn score_clock(scalar: f64, current: u32, added: u32) -> PolicyVerdict {
    let facts = |reason: PolicyReason| {
        reason
            .with_fact("opponent_poison", i64::from(current))
            .with_fact("poison_added", i64::from(added))
    };

    if reaches_lethal(current, added) {
        return PolicyVerdict::score(scalar, facts(PolicyReason::new("poison_clock_lethal")));
    }

    let projected = current.saturating_add(added);
    let progress = f64::from(projected) / f64::from(LETHAL_POISON);
    PolicyVerdict::score(
        scalar.min(STRONG_MAX) * progress.max(MIN_CLOCK_PROGRESS),
        facts(PolicyReason::new("poison_clock_pressure")),
    )
}

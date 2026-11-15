//! Removal-lethality assessment — makes a direct-damage removal spell prefer a
//! target its damage can actually KILL over the biggest body on the board.
//!
//! ## The defect this closes (#6582)
//!
//! [`EvasionRemovalPriorityPolicy`](super::evasion_removal_priority) ranks
//! removal targets by threat value, so it points a fixed-damage burn spell at
//! the biggest creature — a 7/7 — even when the spell deals 3 and cannot
//! destroy it, wasting the card. The AI modelled no lethality at all, so
//! "kills it" and "tickles it" scored the same and the biggest threat always
//! won.
//!
//! ## Model the engine's damage RESULTS, not a damage integer
//!
//! Whether damage kills depends on the damage SOURCE, not only on the amount
//! (CR 120.3), so collapsing a spell's damage into one number gets three whole
//! classes of removal wrong:
//!
//! * CR 120.3d + CR 702.80a: a source with wither/infect marks no damage at
//!   all — it puts that many -1/-1 counters on the creature, which lower its
//!   toughness (CR 122.1a). Reaching 0 toughness is CR 704.5f (put into the
//!   graveyard), which is *not* a destruction, so indestructible (CR 702.12b)
//!   does not save the creature.
//! * CR 702.2b + CR 704.5h: a source with deathtouch makes *any* marked damage
//!   lethal, however large the body.
//! * CR 120.3: for `DamageSource::Target` the first object target IS the source
//!   and is excluded from the recipients, so the object being scored may not be
//!   a recipient at all.
//!
//! The pending spell's damage is therefore reduced to a typed [`DamageOutcome`]
//! (marked damage + -1/-1 counters + deathtouch) resolved per damage source,
//! and only then judged against the target in [`outcome_is_lethal`], which
//! mirrors the state-based-action precedence in `engine::game::sba`. Where the
//! source is not knowable while a target is still being chosen, the term
//! reports [`PendingDamage::Unresolved`] and the policy stays neutral rather
//! than scoring a guess.
//!
//! ## Building block, not a card fix
//!
//! These are pure functions over the pending spell's own damage effects and the
//! target's runtime state — a reusable primitive any removal-targeting policy
//! can consult, covering every direct-damage removal spell rather than one
//! card. The term is inert (`0.0`) whenever the pending effect deals no
//! modelled damage to the target, so `-X/-X`, destroy, and exile removal are
//! untouched.

use engine::game::filter::{matches_target_filter, FilterContext};
use engine::game::game_object::GameObject;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::players::is_opponent;
use engine::game::quantity::{resolve_quantity, resolve_quantity_with_targets_slice};
use engine::game::targeting::find_legal_targets;
use engine::types::ability::{
    ControllerRef, DamageSource, Effect, QuantityExpr, TargetFilter, TargetRef, TypeFilter,
    TypedFilter,
};
use engine::types::card_type::CoreType;
use engine::types::game_state::{DistributionUnit, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};

use super::context::PolicyContext;
use super::effect_classify::{
    effect_polarity, effect_targets_object, extract_target_filter, targets_creatures_only,
    EffectPolarity,
};

/// Reward for a target the removal spell actually kills — a clean kill is worth
/// more than the marginal threat-value ranking that lured the AI to an
/// un-killable body. Sized to clear `removal_target_quality_score`'s `2.0` cap
/// so a lethal small target outranks a survivable large one.
pub(crate) const LETHAL_BONUS: f64 = 2.5;
/// Per-point-of-survived-toughness penalty for a damage spell that leaves the
/// creature alive — the classic "3 damage on a 7/7" waste. Scaling by the body
/// it failed to kill punishes the biggest whiffs hardest.
pub(crate) const WASTE_PENALTY_MULT: f64 = 0.45;
/// Cap on the waste penalty so a single non-lethal target can dampen but not
/// completely dominate the overall target ranking.
pub(crate) const WASTE_PENALTY_MAX: f64 = 3.0;

/// CR 120.3: the object whose characteristics govern one damage effect's
/// results. Deathtouch (CR 702.2b) and wither/infect (CR 120.3d) are read from
/// the SOURCE, never from the spell that created the damage, so the source must
/// be resolved before an amount can be turned into an outcome.
enum EffectDamageSource {
    /// A concrete object, resolvable while targets are still being chosen.
    Object(ObjectId),
    /// The source depends on information this policy does not have yet:
    ///
    /// * [`DamageSource::Target`] — the first object target *is* the source and
    ///   is excluded from the recipient slice
    ///   (`effects::deal_damage::resolve_effect_recipients`), so the object
    ///   being scored may be the source rather than a recipient.
    /// * [`DamageSource::EachTarget`] — every leading target is an independent
    ///   source with its own keywords and its own re-resolved amount.
    /// * [`DamageSource::TriggeringSource`] — bound to the triggering event's
    ///   object; the engine's `targeting::extract_source_from_event` authority
    ///   is crate-private, and re-deriving that mapping in the AI layer would
    ///   duplicate engine logic.
    Unresolved,
}

/// CR 120.3: resolve which object deals one `DealDamage` effect's damage.
fn effect_damage_source(
    ctx: &PolicyContext<'_>,
    damage_source: Option<&DamageSource>,
) -> EffectDamageSource {
    match damage_source {
        // CR 120.3: default — the spell or ability's own source deals the damage.
        None => ctx
            .source_object()
            .map_or(EffectDamageSource::Unresolved, |object| {
                EffectDamageSource::Object(object.id)
            }),
        // CR 120.3: for `DamageSource::Target` the first object target IS the
        // source. During interactive target selection the engine binds already-
        // declared targets in `TargetSelectionProgress.selected_slots` (NOT
        // `ability.targets`, which stays empty until
        // `assign_selected_slots_in_chain` welds the final selection after the
        // last slot commits — CR 601.2c / CR 608.2c). Once a later slot's
        // selection is being made the source is already bound there and is
        // knowable, so lethality against it can be modelled.
        Some(DamageSource::Target) => match bound_target_source_id(ctx) {
            Some(source_id) => EffectDamageSource::Object(source_id),
            // Before the source's own slot is declared (`selected_slots` empty)
            // there is no bound source to model — stay neutral rather than
            // guessing (first-slot / empty-selection case).
            None => EffectDamageSource::Unresolved,
        },
        // CR 120.1 (EachTarget) / triggering-event source: the source is not
        // resolvable from interactive target selection — see the enum doc.
        Some(DamageSource::EachTarget | DamageSource::TriggeringSource) => {
            EffectDamageSource::Unresolved
        }
    }
}

/// CR 120.3: the first already-declared OBJECT target of a `DamageSource::Target`
/// effect, as bound in `TargetSelectionProgress.selected_slots` while targets
/// are still being chosen (CR 601.2c). Before that slot is declared the source
/// is not yet bound, so the caller stays `Unresolved` (neutral), not a guess.
fn bound_target_source_id(ctx: &PolicyContext<'_>) -> Option<ObjectId> {
    match &ctx.decision.waiting_for {
        WaitingFor::TargetSelection { selection, .. }
        | WaitingFor::TriggerTargetSelection { selection, .. } => {
            selection.selected_slots.iter().find_map(|slot| match slot {
                Some(TargetRef::Object(id)) => Some(*id),
                _ => None,
            })
        }
        _ => None,
    }
}

/// CR 601.2c: the declared-object target slice for the current interactive
/// selection — the engine buffers already-chosen targets in
/// `TargetSelectionProgress.selected_slots`. Used to resolve a
/// `DamageSource::Target` amount that references the first object target
/// (`QuantityRef::Power { scope: Target }`), so "X, where X is its power"
/// reads the already-bound source's power. A `Some(TargetRef::Object(id))`
/// slot is unwrapped into the slice; `None`/`Player` slots are skipped.
fn bound_target_slice(ctx: &PolicyContext<'_>) -> Vec<TargetRef> {
    match &ctx.decision.waiting_for {
        WaitingFor::TargetSelection { selection, .. }
        | WaitingFor::TriggerTargetSelection { selection, .. } => {
            selection.selected_slots.iter().flatten().cloned().collect()
        }
        _ => Vec::new(),
    }
}

/// CR 120.3: how one modelled batch of damage lands on a single creature. Kept
/// as a typed per-source outcome so the results stay distinguishable through
/// aggregation instead of collapsing into a single "damage" integer that
/// silently loses wither/infect and deathtouch.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct DamageOutcome {
    /// CR 120.3e: damage from sources with neither wither nor infect, marked on
    /// the creature.
    pub(crate) marked: u32,
    /// CR 120.3d + CR 702.80a: damage from a wither/infect source, dealt as
    /// -1/-1 counters instead of being marked.
    pub(crate) minus_counters: u32,
    /// CR 702.2b: at least one source contributing this damage has deathtouch.
    pub(crate) deathtouch: bool,
}

/// What the pending spell or ability does to one candidate object, resolved
/// against live game state (so `X` and dynamic amounts are concrete).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PendingDamage {
    /// No damage effect on the pending spell reaches this object — the signal
    /// the caller uses to stay inert for non-damage removal.
    None,
    /// A damage effect reaches (or may reach) this object, but its source — and
    /// therefore its result — is not modellable during target selection. The
    /// caller stays neutral instead of scoring a guess.
    Unresolved,
    /// Fully modelled damage results.
    Dealt(DamageOutcome),
}

/// CR 120.3: resolve one `DealDamage` effect's amount against live game state.
///
/// CR 120.3 + CR 208.1 + CR 601.2c: for a `DamageSource::Target` effect whose
/// amount is "X, where X is its power", the amount is the FIRST object target's
/// power — the same bound source object the caller resolved.
/// `resolve_quantity_with_targets_slice` resolves `QuantityRef::Power { scope:
/// Target }` against the first entry of the passed slice, which is the declared
/// source already bound in `selection.selected_slots[0]`. All other sources
/// resolve the amount against the source object (CR 120.3 default) or a fixed
/// value.
///
/// The single authority for "how many points does this effect deal", shared by
/// the per-target [`pending_damage_to_object`] and the divided-damage pool in
/// [`divided_damage_facts`] so the two can never disagree about an amount.
fn resolve_damage_amount(
    ctx: &PolicyContext<'_>,
    amount: &QuantityExpr,
    damage_source: Option<&DamageSource>,
    source_id: ObjectId,
) -> u32 {
    let resolved = if matches!(damage_source, Some(DamageSource::Target)) {
        resolve_quantity_with_targets_slice(
            ctx.state,
            amount,
            ctx.ai_player,
            source_id,
            &bound_target_slice(ctx),
        )
    } else {
        resolve_quantity(ctx.state, amount, ctx.ai_player, source_id)
    };
    u32::try_from(resolved.max(0)).unwrap_or(u32::MAX)
}

/// Reduce every damage effect on the pending spell that reaches `target` into a
/// single typed [`PendingDamage`].
///
/// CR 120.3d / CR 120.3e: each effect's amount is routed to -1/-1 counters or
/// to marked damage according to ITS OWN source's wither/infect, so a spell
/// mixing sources aggregates correctly.
pub(crate) fn pending_damage_to_object(
    ctx: &PolicyContext<'_>,
    target_id: ObjectId,
    target: &GameObject,
) -> PendingDamage {
    // CR 120.3d: only a creature converts wither/infect damage into -1/-1
    // counters; other permanents take the damage by their own rules.
    let is_creature = target.card_types.core_types.contains(&CoreType::Creature);
    let mut outcome = DamageOutcome::default();
    let mut found = false;

    for effect in ctx.effects() {
        match effect {
            Effect::DealDamage {
                amount,
                damage_source,
                ..
            } => {
                if !effect_targets_object(ctx, effect, target_id) {
                    continue;
                }
                // CR 120.3: resolve the damage source.
                let EffectDamageSource::Object(source_id) =
                    effect_damage_source(ctx, damage_source.as_ref())
                else {
                    return PendingDamage::Unresolved;
                };
                found = true;
                let dealt = resolve_damage_amount(ctx, amount, damage_source.as_ref(), source_id);
                // CR 120.3d + CR 702.80a + CR 702.90c: wither/infect damage to a
                // creature is dealt as -1/-1 counters and is never marked.
                if is_creature
                    && (object_has_effective_keyword_kind(
                        ctx.state,
                        source_id,
                        KeywordKind::Wither,
                    ) || object_has_effective_keyword_kind(
                        ctx.state,
                        source_id,
                        KeywordKind::Infect,
                    ))
                {
                    outcome.minus_counters = outcome.minus_counters.saturating_add(dealt);
                } else {
                    // CR 120.3e: otherwise the damage is marked on the creature.
                    outcome.marked = outcome.marked.saturating_add(dealt);
                }
                // CR 702.2b: the deathtouch flag comes from the source that
                // actually dealt damage, mirroring `dealt_deathtouch_damage`.
                outcome.deathtouch |= dealt > 0
                    && object_has_effective_keyword_kind(
                        ctx.state,
                        source_id,
                        KeywordKind::Deathtouch,
                    );
            }
            // CR 120.1: multi-source batches and mass damage put damage on this
            // object from sources this policy does not model per-source. Bail
            // rather than under-count and mis-report a lethal spell as a whiff.
            Effect::EachDealsDamageEqualToPower { .. }
            | Effect::EachSourceDealsDamage { .. }
            | Effect::DamageAll { .. }
            | Effect::ApplyPostReplacementDamage { .. } => return PendingDamage::Unresolved,
            _ => {}
        }
    }

    if found {
        PendingDamage::Dealt(outcome)
    } else {
        PendingDamage::None
    }
}

/// Does `outcome` kill `target` at the next state-based action check?
///
/// Ordered to match the precedence in `engine::game::sba`:
///
/// 1. CR 704.5f: -1/-1 counters (CR 120.3d) lower toughness (CR 122.1a); at 0
///    or less the creature is put into its owner's graveyard. That is not a
///    destruction, so CR 702.12b indestructible does NOT prevent it.
/// 2. CR 702.12b: otherwise an indestructible creature ignores both
///    lethal-damage state-based actions.
/// 3. CR 704.5h + CR 702.2b: any marked damage from a deathtouch source is
///    lethal to a creature with toughness greater than 0.
/// 4. CR 704.5g: marked damage reaching the counter-reduced toughness.
///
/// A creature already at 0 or less toughness is dying to its own CR 704.5f
/// state-based action, not to this spell, so it does not count as killed here.
pub(crate) fn outcome_is_lethal(target: &GameObject, outcome: &DamageOutcome) -> bool {
    if target.toughness.unwrap_or(0) <= 0 {
        return false;
    }
    let reduced = reduced_toughness(target, outcome);
    // CR 704.5f: 0 toughness kills through indestructible.
    if reduced <= 0 {
        return true;
    }
    // CR 702.12b: indestructible ignores the lethal-damage state-based actions.
    if target.has_keyword(&Keyword::Indestructible) {
        return false;
    }
    let marked = target.damage_marked.saturating_add(outcome.marked);
    // CR 704.5h + CR 702.2b: deathtouch damage is lethal regardless of amount.
    if outcome.deathtouch && marked > 0 {
        return true;
    }
    // CR 704.5g: lethal marked damage, measured against the reduced toughness.
    u32::try_from(reduced).is_ok_and(|threshold| marked >= threshold)
}

/// CR 122.1a: `target`'s toughness once this spell's -1/-1 counters (CR 120.3d)
/// are on it. The single authority for the counter-reduced toughness — it is
/// both the CR 704.5g lethal-damage threshold and, clamped at 0, the size of the
/// body a non-lethal spell failed to kill.
fn reduced_toughness(target: &GameObject, outcome: &DamageOutcome) -> i32 {
    let counters = i32::try_from(outcome.minus_counters).unwrap_or(i32::MAX);
    target.toughness.unwrap_or(0).saturating_sub(counters)
}

/// Lethality contribution for pointing a damage removal spell at `target`.
///
/// * Kills it (CR 704.5f / CR 704.5g / CR 704.5h) → `+LETHAL_BONUS`.
/// * Survives (high toughness, or indestructible per CR 702.12b) → a penalty
///   scaled by the body it failed to kill, so a 3-damage spell on a 7/7 ranks
///   well below a smaller target the same spell destroys.
/// * No modelled damage reaches the target, or the damage source is not
///   resolvable during target selection → `0.0`, leaving that targeting
///   decision exactly as it was.
pub(crate) fn lethality_bonus(
    ctx: &PolicyContext<'_>,
    target_id: ObjectId,
    target: &GameObject,
) -> f64 {
    let PendingDamage::Dealt(outcome) = pending_damage_to_object(ctx, target_id, target) else {
        return 0.0;
    };
    if outcome.marked == 0 && outcome.minus_counters == 0 {
        return 0.0;
    }
    if outcome_is_lethal(target, &outcome) {
        return LETHAL_BONUS;
    }
    let survived = reduced_toughness(target, &outcome).max(0);
    -(f64::from(survived) * WASTE_PENALTY_MULT).min(WASTE_PENALTY_MAX)
}

/// CR 601.2d: does the pending spell or ability DIVIDE damage among the targets
/// being chosen?
///
/// Read from the ENGINE's authority for the division — `PendingCast.distribute`
/// for a spell being cast and `PendingTrigger.distribute` for a triggered
/// ability — rather than from a copy carried on the resolved ability, so the
/// answer is the same one the engine consults before it opens
/// `WaitingFor::DistributeAmong`.
///
/// `WaitingFor::MultiTargetSelection` is deliberately OUT of scope: it presents
/// whole target SUBSETS as candidates instead of one step-wise `ChooseTarget`
/// per slot, so there is no already-declared prefix to price a remaining budget
/// against. Only the step-wise `TargetSelection` / `TriggerTargetSelection`
/// arms are covered.
fn pending_ability_distributes_damage(ctx: &PolicyContext<'_>) -> bool {
    let unit = match &ctx.decision.waiting_for {
        WaitingFor::TargetSelection { pending_cast, .. } => pending_cast.distribute.as_ref(),
        WaitingFor::TriggerTargetSelection { .. } => ctx
            .state
            .pending_trigger
            .as_ref()
            .and_then(|trigger| trigger.distribute.as_ref()),
        _ => None,
    };
    matches!(unit, Some(DistributionUnit::Damage))
}

/// CR 120.3 + CR 601.2d: everything a divided-damage pool's lethality maths
/// needs from the damage SOURCE, hoisted ONCE per candidate so the per-share
/// scans below never repeat a keyword lookup.
struct DividedDamageFacts {
    /// CR 601.2d: the WHOLE pool being divided, not a per-target share.
    total: u32,
    /// CR 120.3d + CR 702.80a + CR 702.90c: the source has wither or infect, so
    /// its damage to a creature becomes -1/-1 counters instead of marked damage.
    wither_or_infect: bool,
    /// CR 702.2b: the source has deathtouch, so any marked damage is lethal.
    deathtouch: bool,
}

/// CR 601.2d: the divided-damage pool and its source facts for the effect that
/// reaches `target_id` (or, for `None`, the ability's divided-damage effect
/// itself — a player candidate has no object to match). `None` when no modelled
/// damage effect applies or the source is unresolvable (CR 120.3) —
/// the caller then stays neutral rather than scoring a guess.
fn divided_damage_facts(
    ctx: &PolicyContext<'_>,
    target_id: Option<ObjectId>,
) -> Option<DividedDamageFacts> {
    ctx.effects().into_iter().find_map(|effect| {
        let Effect::DealDamage {
            amount,
            damage_source,
            ..
        } = effect
        else {
            return None;
        };
        // A player candidate has no object to match against the filter; the
        // pool and its source facts are properties of the effect either way.
        if target_id.is_some_and(|id| !effect_targets_object(ctx, effect, id)) {
            return None;
        }
        let EffectDamageSource::Object(source_id) =
            effect_damage_source(ctx, damage_source.as_ref())
        else {
            return None;
        };
        Some(DividedDamageFacts {
            total: resolve_damage_amount(ctx, amount, damage_source.as_ref(), source_id),
            wither_or_infect: object_has_effective_keyword_kind(
                ctx.state,
                source_id,
                KeywordKind::Wither,
            ) || object_has_effective_keyword_kind(
                ctx.state,
                source_id,
                KeywordKind::Infect,
            ),
            deathtouch: object_has_effective_keyword_kind(
                ctx.state,
                source_id,
                KeywordKind::Deathtouch,
            ),
        })
    })
}

/// CR 120.3d / CR 120.3e: route `dealt` points of a divided pool onto one
/// recipient according to the hoisted source facts.
fn divided_outcome(facts: &DividedDamageFacts, target: &GameObject, dealt: u32) -> DamageOutcome {
    // CR 120.3d: only a creature converts wither/infect damage into -1/-1
    // counters; other permanents take the damage by their own rules.
    let is_creature = target.card_types.core_types.contains(&CoreType::Creature);
    DamageOutcome {
        marked: if is_creature && facts.wither_or_infect {
            0
        } else {
            dealt
        },
        minus_counters: if is_creature && facts.wither_or_infect {
            dealt
        } else {
            0
        },
        // CR 702.2b: the deathtouch flag comes from the source that actually
        // dealt damage, so a zero share carries none.
        deathtouch: facts.deathtouch && dealt > 0,
    }
}

/// CR 601.2d + CR 704.5f/g/h: the smallest share of a divided pool that kills
/// `target`, or `None` when no share within `budget` does.
///
/// [`outcome_is_lethal`] is MONOTONE in the share for a fixed source — more
/// marked damage only crosses the CR 704.5g threshold, more -1/-1 counters only
/// lower the CR 704.5f toughness — and a share equal to the creature's
/// toughness is already lethal under either routing whenever any share is. The
/// scan is therefore bounded by the toughness, never by the pool: a 100-point
/// Fireball still costs O(toughness), not O(100).
fn divided_lethal_requirement(
    facts: &DividedDamageFacts,
    target: &GameObject,
    budget: u32,
) -> Option<u32> {
    let toughness = u32::try_from(target.toughness.unwrap_or(0)).unwrap_or(0);
    (1..=budget.min(toughness))
        .find(|&dealt| outcome_is_lethal(target, &divided_outcome(facts, target, dealt)))
}

/// CR 601.2d: how much of the pool the ALREADY-DECLARED targets have committed.
///
/// Each chosen target must receive at least one of what is divided, and a
/// chosen creature target is only worth choosing for the share that actually
/// kills it — so its reserve is its lethal requirement, falling back to the
/// CR 601.2d minimum of one when no share in the pool can kill it
/// (indestructible per CR 702.12b, or already dying). Player and non-creature
/// targets reserve that same minimum.
///
/// Reads the same `TargetSelectionProgress.selected_slots` slice
/// [`bound_target_slice`] does (CR 601.2c) — bounded by the ability's target
/// maximum, so this is a handful of iterations, never a board scan.
fn declared_targets_reserve(
    ctx: &PolicyContext<'_>,
    declared: &[TargetRef],
    facts: &DividedDamageFacts,
) -> u32 {
    declared
        .iter()
        .map(|target| match target {
            TargetRef::Object(id) => ctx
                .state
                .objects
                .get(id)
                .filter(|object| object.card_types.core_types.contains(&CoreType::Creature))
                .and_then(|object| divided_lethal_requirement(facts, object, facts.total))
                .unwrap_or(1),
            _ => 1,
        })
        .fold(0u32, u32::saturating_add)
}

/// CR 601.2d: what a divided-damage pool can still do to one more target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum DividedDamage {
    /// Some share of the REMAINING pool kills the target.
    Lethal,
    /// No share of the remaining pool kills it — including the exhausted case
    /// where nothing is left to assign beyond the CR 601.2d minimum.
    CannotKill,
}

/// CR 601.2d: the pool left for one MORE target after the already-declared
/// targets have taken their lethal reserves.
///
/// `None` when the pending ability does not divide damage, when the damage
/// source is not resolvable during target selection (CR 120.3), or when NO
/// target has been declared yet — with nothing reserved there is no "extra"
/// target and nothing for one to eat into.
///
/// `Some(0)` is the exhausted pool: every declared target already holds exactly
/// the share that kills it, so any further target — a player or planeswalker
/// just as much as a creature — must be given its mandatory one point
/// (CR 601.2d) out of a reserve that was killing something.
pub(crate) fn divided_damage_remaining_budget(ctx: &PolicyContext<'_>) -> Option<u32> {
    if !pending_ability_distributes_damage(ctx) {
        return None;
    }
    let declared = bound_target_slice(ctx);
    if declared.is_empty() {
        return None;
    }
    let facts = divided_damage_facts(ctx, None)?;
    Some(
        facts
            .total
            .saturating_sub(declared_targets_reserve(ctx, &declared, &facts)),
    )
}

/// Divided-damage building block: can the pool still kill `target` once the
/// already-declared targets have taken their lethal reserves?
///
/// `None` when the pending ability does not divide damage (every non-divided
/// damage spell is untouched) or the damage source is not resolvable during
/// target selection (CR 120.3).
///
/// This is what stops the "add every creature until the slot maximum" pathology
/// on the Bogardan Hellkite class: the per-candidate lethality term prices each
/// `ChooseTarget` step as if it received the whole pool, so every creature with
/// toughness ≤ the pool looks killable, and CR 601.2d then forces a
/// one-point-each split that kills nothing. Covers every "divided as you
/// choose" damage source, not one card.
pub(crate) fn divided_damage_verdict(
    ctx: &PolicyContext<'_>,
    target_id: ObjectId,
    target: &GameObject,
) -> Option<DividedDamage> {
    if !pending_ability_distributes_damage(ctx) {
        return None;
    }
    let facts = divided_damage_facts(ctx, Some(target_id))?;
    let declared = bound_target_slice(ctx);
    let remaining = facts
        .total
        .saturating_sub(declared_targets_reserve(ctx, &declared, &facts));
    Some(
        if divided_lethal_requirement(&facts, target, remaining).is_some() {
            DividedDamage::Lethal
        } else {
            DividedDamage::CannotKill
        },
    )
}

/// Cast-commit lethality guard: does the pending spell's targeted creature
/// damage have ANY legal target it can actually kill?
///
/// The cast-commit dual of [`lethality_bonus`] (which ranks targets during
/// selection). Prevents cases where a burn spell whose damage is provably
/// non-lethal against every legal target gets cast and pointed at the biggest
/// body, wasting the card. This gate tells the cast-commit whiff check
/// ([`super::anti_self_harm::score_pre_cast`]) whether committing
/// is ever worthwhile against the board.
///
/// **Conservative no-veto contract** — returns `true` (do not veto) whenever it
/// cannot *prove* a total whiff:
/// * no source object / no usable filter (cannot reason);
/// * ANY `Harmful` or `Contextual` non-`DealDamage` effect that has at least one
///   legal target/population under an OPPONENT's control (e.g. a mixed "deal
///   damage + destroy" spell — the Destroy half is an independent, useful
///   removal line, CR 701.8a; "deal damage + gain control" — stealing a
///   planeswalker or artifact is an independent control-changing line,
///   CR 613.1b Layer 2; or a mass wipe, CR 701.8). The population is resolved
///   with the COMPLETE typed filter — including `TypedFilter.controller`
///   (CR 108.4 / CR 109.5) — so an own-controller-constrained line ("gain
///   control of target creature you control") credits nothing, so the spell is
///   never a total *damage* whiff). A wipe's population is evaluated
///   resolver-mirroring (CR 115.10a: it is NON-targeted, so hexproof/protected
///   creatures still count and `TargetFilter::None` means the resolver's
///   default all-creatures population, destroy.rs `resolve_all`);
/// * ANY `DealDamage` amount references `X` (CR 107.3a — the caster chooses the
///   value at announcement, so it is unknowable at cast-commit), including a
///   `DealDamage` whose target filter is not creature-only;
/// * any legal target yields [`PendingDamage::Unresolved`] (damage source not
///   knowable at cast-commit, CR 120.3) or [`PendingDamage::None`] (non-damage
///   removal like Destroy/Exile — never a damage whiff);
/// * there are zero legal object targets (empty set — never conclude a veto
///   from a vacuous "all survived").
///
/// It returns `false` (veto) **only** when at least one legal object target was
/// fully modelled as [`PendingDamage::Dealt`] and **every** modelled target is
/// provably non-lethal per [`outcome_is_lethal`] (CR 704.5f / 704.5g / 704.5h).
pub(crate) fn can_kill_any_legal_target(ctx: &PolicyContext<'_>) -> bool {
    // CR 120.3: to resolve a damage source the caster's source object must be
    // known. Without it, no filter controller and no resolvable source — fail
    // open rather than veto on something we cannot reason about.
    let Some(source) = ctx.source_object() else {
        return true;
    };
    let effects = ctx.effects();

    // FAIL OPEN when a `Harmful`/`Contextual` non-`DealDamage` effect has at
    // least one legal target/population under an OPPONENT's control — e.g. a
    // mixed "deal 1 damage to target creature; destroy target creature" spell,
    // a control-changing line like "deal 1 damage; gain control of target
    // permanent" (`GainControl`, CR 613.1b Layer 2), or a mass wipe
    // (`DestroyAll`, CR 701.8). The decision applies the COMPLETE typed filter
    // — including `TypedFilter.controller` (CR 108.4 / CR 109.5) — through
    // the engine's `find_legal_targets`: an own-controller-constrained line
    // ("gain control of target creature you control") credits nothing, and a
    // wipe's real population is resolved, not guessed from filter shape.
    // Wipes take a resolver-mirroring population path instead of target
    // legality: `DestroyAll` is NON-targeted (CR 115.10a), so hexproof/protected
    // creatures count toward the population and `TargetFilter::None` means the
    // resolver's default all-creatures population (destroy.rs `resolve_all`).
    if effects.iter().any(|effect| {
        matches!(
            effect_polarity(effect),
            EffectPolarity::Harmful | EffectPolarity::Contextual
        ) && !matches!(effect, Effect::DealDamage { .. })
            && effect_has_legal_opposing_line(ctx, effect)
    }) {
        return true;
    }

    // FAIL OPEN when ANY `DealDamage` effect on the spell references a
    // variable-X — including one whose target filter is not creature-only.
    // CR 107.3a: X is chosen by the caster at announcement and cannot be
    // known at the commit decision. Therefore, no damage-only X spell is
    // ever a provable total whiff. Scan every `DealDamage`.
    if effects
        .iter()
        .any(|effect| matches!(effect, Effect::DealDamage { amount, .. } if amount.contains_x()))
    {
        return true;
    }

    let mut modelled_any_target = false;

    for effect in effects.iter().copied().filter(|effect| {
        matches!(effect_polarity(effect), EffectPolarity::Harmful) && targets_creatures_only(effect)
    }) {
        // No usable target filter (or a filter this policy can't analyse) — fail
        // open, mirroring `harmful_effect_has_opponent_creature_target`.
        let Some(filter) = extract_target_filter(effect) else {
            return true;
        };
        for target in find_legal_targets(ctx.state, filter, ctx.ai_player, source.id) {
            let TargetRef::Object(object_id) = target else {
                continue;
            };
            // A harmful removal spell is only useful against an OPPONENT's
            // creature — a target the caster controls would be self-targeting
            // (anti-self-harm, handled separately). Mirror the gating
            // `has_targetable_opponent_creature` via `players::is_opponent`
            // (CR 102.2 / CR 102.3: team-aware — a teammate is not an opponent).
            let Some(object) = ctx.state.objects.get(&object_id).filter(|object| {
                is_opponent(ctx.state, ctx.ai_player, object.controller)
                    && object.card_types.core_types.contains(&CoreType::Creature)
            }) else {
                continue;
            };
            match pending_damage_to_object(ctx, object_id, object) {
                // CR 120.3: source not resolvable at cast-commit, or this is
                // non-damage removal — inconclusive, no veto.
                PendingDamage::Unresolved | PendingDamage::None => return true,
                PendingDamage::Dealt(outcome) => {
                    modelled_any_target = true;
                    // CR 704.5f/g/h: even one legal target this spell can kill
                    // means the cast is not a total whiff.
                    if outcome_is_lethal(object, &outcome) {
                        return true;
                    }
                }
            }
        }
    }

    // Veto only when we fully modelled at least one legal target and every
    // modelled target survived — i.e. `model_any_target && !any_escape`. The
    // empty-set case (`!modelled_any_target`) fails open by contract.
    !modelled_any_target
}

/// Cast-commit seam query: does ANY inherently-mass non-`DealDamage` effect
/// on the pending spell (currently `DestroyAll`, CR 701.8) have a non-empty
/// opposing population under the resolver's semantics — NON-targeted (CR
/// 115.10a), team-aware (`is_opponent`, CR 102.2/102.3), indestructible
/// skipped? Independent of target legality: consulted by
/// `anti_self_harm::score_pre_cast` BEFORE the `has_targetable_opponent_creature`
/// gate so a useful wipe line rescues a mixed spell whose only opposing
/// creatures are hexproof/protected (un-targetable, but wiped).
/// Returns `true` for an UNKNOWN population too (an unbound player-relative
/// wipe, e.g. a companion `TargetOpponent` controller scope — CR 109.4 /
/// CR 115.1): only a provably-empty population (`Some(false)`) reads false.
/// This threads the fail-open to BOTH `anti_self_harm::score_pre_cast`'s
/// rescue and `tactical_gate::is_redundant_creature_only_removal`'s
/// suppression, so an unresolvable-at-commit wipe can never apply a whiff
/// penalty or hard-reject the cast.
pub(crate) fn has_opposing_mass_population(ctx: &PolicyContext<'_>) -> bool {
    let Some(source) = ctx.source_object() else {
        return false;
    };
    ctx.effects().iter().any(|effect| {
        matches!(
            effect,
            Effect::DestroyAll { target, .. }
                if mass_effect_has_opposing_population(ctx, source, target) != Some(false)
        )
    })
}

/// Does this non-`DealDamage` effect currently have a legal target or
/// population under an OPPONENT's control? Applies the full typed filter —
/// including `TypedFilter.controller` (CR 108.4 / CR 109.5) — via the
/// engine's `find_legal_targets`; never a filter-shape proxy. Effects with no
/// extractable filter (or no source object) resolve to false (no line).
///
/// [`Effect::DestroyAll`] bypasses the targeting path entirely: it is
/// NON-targeted, so the engine resolver (`destroy::resolve_all`) matches a
/// battlefield POPULATION with no hexproof/shroud/protection exemptions and a
/// default all-creatures population for `TargetFilter::None` (CR 115.10a).
/// `find_legal_targets` would wrongly gate that population on target legality
/// and read `None` as an empty set, so wipes take the resolver-mirroring
/// [`mass_effect_has_opposing_population`] path instead.
fn effect_has_legal_opposing_line(ctx: &PolicyContext<'_>, effect: &Effect) -> bool {
    let Some(source) = ctx.source_object() else {
        return false;
    };
    // CR 115.10a: inherently-mass effects (`DestroyAll`) are NON-targeted —
    // the resolver matches a battlefield POPULATION (engine destroy.rs
    // `resolve_all`) with no target-legality exemptions and a default
    // population when the filter is `None`. Evaluate those resolver-mirroring;
    // `find_legal_targets` would wrongly apply hexproof/shroud/protection and
    // read `None` as an empty set.
    if let Effect::DestroyAll { target, .. } = effect {
        // `None` (unbound player-relative controller, e.g. a companion
        // `TargetOpponent` wipe) is UNKNOWN — FAIL OPEN as useful, only a
        // provably-empty population (`Some(false)`) is not worth a line.
        return mass_effect_has_opposing_population(ctx, source, target) != Some(false);
    }
    let Some(filter) = extract_target_filter(effect) else {
        return false;
    };
    find_legal_targets(ctx.state, filter, ctx.ai_player, source.id)
        .into_iter()
        .any(|target| {
            matches!(
                target,
                TargetRef::Object(id)
                    if ctx
                        .state
                        .objects
                        .get(&id)
                        .is_some_and(|o| is_opponent(ctx.state, ctx.ai_player, o.controller))
            )
        })
}

/// Resolver-mirroring population evaluation for a non-targeted mass effect
/// (CR 115.10a): iterate the battlefield and match the effect's population
/// exactly as `engine::game::effects::destroy::resolve_all` does —
/// indestructible objects are skipped (CR 702.12b: they can't be destroyed)
/// and `TargetFilter::None` means the resolver's default population (all
/// creatures). Unlike `find_legal_targets`, NO hexproof / shroud / protection
/// targets-exemption applies: those gate targeting only (CR 115.10a) and
/// never a wipe's population.
///
/// Tri-state result, conservative by construction:
/// * `Some(true)` — an opposing population exists (the wipe is useful);
/// * `Some(false)` — the opposing population is provably empty;
/// * `None` — UNKNOWN: the population filter carries a player-RELATIVE
///   controller scope (`TargetPlayer` / `TargetOpponent`, `ScopedPlayer`,
///   `ParentTarget*`, `Chosen*`, `TriggeringPlayer`, ...) whose companion
///   player target is not bound at cast-commit. The engine reads the
///   companion from `ability.targets` (filter.rs
///   `ControllerRef::TargetPlayer|TargetOpponent` arm) and FAILS CLOSED
///   without it, while `destroy::resolve_all` resolves it later via
///   `FilterContext::from_ability` AFTER the companion player is announced
///   (CR 601.2c / CR 603.3d). We cannot know the population now, so consumers
///   must FAIL OPEN on `None` (treat `!= Some(false)` as useful).
fn mass_effect_has_opposing_population(
    ctx: &PolicyContext<'_>,
    source: &GameObject,
    target: &TargetFilter,
) -> Option<bool> {
    // Mirror destroy.rs `resolve_all`'s `None` -> default creature population.
    let default_population = TargetFilter::Typed(TypedFilter::new(TypeFilter::Creature));
    let effective = if matches!(target, TargetFilter::None) {
        &default_population
    } else {
        target
    };
    // CR 109.4 + CR 115.1: a player-RELATIVE population filter — `TargetPlayer` /
    // `TargetOpponent` companion wipes ("destroy all creatures target player
    // controls"), or any other context-bound controller scope — is NOT resolvable at
    // cast-commit: the engine reads the companion from `ability.targets`
    // (filter.rs `ControllerRef::TargetPlayer|TargetOpponent` arm) and FAILS CLOSED
    // without it, while `destroy::resolve_all` resolves it later via
    // `FilterContext::from_ability` AFTER the companion player is announced
    // (CR 601.2c / CR 603.3d). We cannot know the population now → `None`
    // (UNKNOWN = explicit fail-open); consumers treat `!= Some(false)` as useful.
    if filter_has_unbound_player_controller(effective) {
        return None;
    }
    let filter_ctx = FilterContext::from_source_with_controller(source.id, source.controller);
    Some(ctx.state.battlefield.iter().any(|&id| {
        let Some(obj) = ctx.state.objects.get(&id) else {
            return false;
        };
        is_opponent(ctx.state, ctx.ai_player, obj.controller)
            && !obj.has_keyword(&Keyword::Indestructible)
            && matches_target_filter(ctx.state, id, effective, &filter_ctx)
    }))
}

/// Does the filter's controller scope resolve from the casting source alone at
/// cast-commit? Only `ControllerRef::You` / `ControllerRef::Opponent` are
/// statically derivable from the pending spell object. Every other scope —
/// `TargetPlayer`/`TargetOpponent` companion wipes, `ScopedPlayer`,
/// `ParentTarget*`, `Chosen*`, `TriggeringPlayer` — depends on an announced
/// target or resolution context (CR 109.4 / CR 115.1 / CR 608.2c) that does not
/// exist yet, so we can never provably empty the population now. The remaining
/// source/global-state-readable variants (`ActivePlayer`, resolved from
/// `state.active_player` in `filter.rs`; `EnchantedPlayer`, from
/// `source.attached_to`; `SourceChosenPlayer`, from `source.chosen_attributes`)
/// would resolve at cast-commit in isolation, but a pending spell object carries
/// no `attached_to` / `chosen_attributes`, and coupling their resolution here
/// would beg the very scope question — so for the purpose of this guard they are
/// classified UNBOUND BY CONSERVATIVE DESIGN (an over-approximation: a wipe of
/// such a scope is treated as possibly-non-empty unless its scope is statically
/// derivable, so a conservative wipe is never vetoed at cast-commit).
///
/// Boundary: the unbound check covers only the controller-scope positions the
/// parser emits for wipe populations — `TypedFilter.controller`, including
/// nested `Or`/`And`/`Not` filters. It does NOT recurse into `FilterProp`
/// payloads that themselves embed `ControllerRef`s (`Owned { controller }`,
/// `Attacking { defender }`, `ProtectorMatches { controller }`,
/// `HasAttachment { controller }`, `HasAnyAttachmentOf { controller }`,
/// `MostPrevalentCreatureTypeIn { scope }`, or the nested `TargetFilter` inside
/// `CanEnchant`). No parser-emittable wipe population uses these today, so the
/// gap is latent; if one ever appeared it would be conservatively non-fail-open
/// and would need revisiting. Conservative by construction: any future
/// `ControllerRef` variant falls to `true` (unknown → fail open).
fn filter_has_unbound_player_controller(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed
            .controller
            .as_ref()
            .is_some_and(|ctrl| !matches!(ctrl, ControllerRef::You | ControllerRef::Opponent)),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(filter_has_unbound_player_controller)
        }
        TargetFilter::Not { filter: inner } => filter_has_unbound_player_controller(inner),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::policies::evasion_removal_priority::EvasionRemovalPriorityPolicy;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::triggers::PendingTrigger;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, EffectKind, QuantityExpr, QuantityRef,
        ResolvedAbility, TargetFilter, TypeFilter, TypedFilter,
    };
    use engine::types::actions::GameAction;
    use engine::types::game_state::{
        CastPaymentMode, GameState, TargetEffectDetail, TargetSelectionProgress,
        TargetSelectionSlot, WaitingFor,
    };
    // `verdict` is a `TacticalPolicy` method — the divided-damage Reject is
    // asserted through the production trait, not a bare inherent call.
    use crate::policies::registry::{PolicyVerdict, TacticalPolicy};
    use engine::types::identifiers::CardId;
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.turn_number = 2;
        state
    }

    fn add_creature(
        state: &mut GameState,
        owner: PlayerId,
        name: &str,
        power: i32,
        toughness: i32,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Creature);
        obj.power = Some(power);
        obj.toughness = Some(toughness);
        id
    }

    /// Add a bare non-creature permanent (artifact) to the battlefield — a
    /// legal, useful "gain control of target permanent" target that is
    /// invisible to creature-only filters.
    fn add_artifact(state: &mut GameState, owner: PlayerId, name: &str) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.next_object_id),
            owner,
            name.to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        id
    }

    /// A damage spell that deals the given `amount` to "target creature", then
    /// runs `f` with a `PolicyContext` for casting it. Constructed inline so the
    /// borrowed temporaries (`decision`/`candidate`/`context`) live for the
    /// duration of `f`.
    fn with_damage_spell<R>(
        state: &mut GameState,
        amount: QuantityExpr,
        f: impl FnOnce(&PolicyContext<'_>) -> R,
    ) -> R {
        let spell_id = create_object(
            state,
            CardId(90_000),
            PlayerId(0),
            "Predictable Burn".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DealDamage {
                amount,
                target: TargetFilter::Typed(TypedFilter::creature()),
                damage_source: None,
                excess: None,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_000),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        f(&ctx)
    }

    /// Slash-of-Light-shaped whiff: 1 damage, an opponent 3/3 that survives.
    /// Nothing to kill → `can_kill_any_legal_target` must return false (veto).
    #[test]
    fn can_kill_vetoes_when_every_legal_target_survives() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "My Bear", 2, 1);
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        // 1 damage, undamaged 3/3 survives (CR 704.5g) → veto.
        with_damage_spell(&mut state, QuantityExpr::Fixed { value: 1 }, |ctx| {
            assert!(
                !can_kill_any_legal_target(ctx),
                "1 damage with no lethal legal opponent target must veto the whiff"
            );
        });
    }

    /// Positive reach-guard: burn that kills a legal opponent target must NOT
    /// veto. Model 4 damage against a 3/3 (lethal via CR 704.5g).
    #[test]
    fn can_kill_does_not_veto_when_a_legal_target_is_lethal() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        with_damage_spell(&mut state, QuantityExpr::Fixed { value: 4 }, |ctx| {
            assert!(
                can_kill_any_legal_target(ctx),
                "4 damage killing the 3/3 must NOT veto the cast"
            );
        });
    }

    /// Multi-authority hostile fixture: opponent has a 2/2 and a 3/3, burn
    /// deals 2. The 3/3 survives but the 2/2 is a legal lethal target → the
    /// cast is NOT a total whiff, so no veto. Partial-whiff target choice is
    /// deferred to the target-selection `lethality_bonus`.
    #[test]
    fn can_kill_does_not_veto_when_any_single_target_is_lethal() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Small Bear", 2, 2);
        add_creature(&mut state, PlayerId(1), "Big Bear", 3, 3);

        with_damage_spell(&mut state, QuantityExpr::Fixed { value: 2 }, |ctx| {
            assert!(
                can_kill_any_legal_target(ctx),
                "2 damage that can kill the opponent 2/2 must NOT veto (partial whiff)"
            );
        });
    }

    /// Variable-X damage is chosen by the caster at announcement — never veto.
    #[test]
    fn can_kill_never_vetoes_variable_x_damage() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let amount = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        with_damage_spell(&mut state, amount, |ctx| {
            assert!(can_kill_any_legal_target(ctx));
        });
    }

    /// Self-controlled targets are not "useful" removal targets. The
    /// empty/self-only target set is covered by the sibling branch in
    /// `anti_self_harm::score_pre_cast` (no targetable opponent creature),
    /// which fires before this gate is consulted — so this gate's contract
    /// fails open on it (never veto from a vacuous "all survived").
    #[test]
    fn can_kill_fails_open_on_self_only_targets() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "My Bear", 2, 1);

        with_damage_spell(&mut state, QuantityExpr::Fixed { value: 1 }, |ctx| {
            // Empty opponent-target set → fail open (no veto); the whiff is
            // handled by the sibling no-opponent-target branch instead.
            assert!(can_kill_any_legal_target(ctx));
        });
    }

    /// Non-damage removal (e.g. Destroy) is never a damage whiff — never veto.
    #[test]
    fn can_kill_never_vetoes_non_damage_removal() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        // Build a `Destroy` spell — no DealDamage in the effect set, so
        // `pending_damage_to_object` returns `PendingDamage::None` (never a
        // damage whiff). Override the ability to a Destroy.
        let spell_id = create_object(
            &mut state,
            CardId(90_001),
            PlayerId(0),
            "Murder".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Destroy {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_001),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(can_kill_any_legal_target(&ctx));
    }

    /// Mixed removal spell: "deal 1 damage to target creature; destroy target
    /// creature". The 1-damage half is a whiff on the 3/3, but the Destroy half
    /// is independently useful (CR 701.8a). The cast-commit gate must FAIL OPEN
    /// (not veto), otherwise it reports a false *damage* whiff for a spell that
    /// still has a genuine removal line.
    ///
    /// `pending_damage_to_object` aggregates the spell's `DealDamage` halves
    /// only. Without the non-`DealDamage` fail-open guard, the Destroy half
    /// reads as a surviving 1-damage target and the whole spell is wrongly vetoed.
    #[test]
    fn can_kill_fails_open_on_mixed_damage_and_destroy() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let spell_id = create_object(
            &mut state,
            CardId(90_002),
            PlayerId(0),
            "Charred Murder".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_002),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            can_kill_any_legal_target(&ctx),
            "mixed deal-1 + destroy must fail open: the Destroy half is a useful \
             removal line, so the spell is not a total damage whiff even though \
             1 damage alone cannot kill the 3/3"
        );
    }

    /// Mixed removal spell with a wipe line: "deal 1 damage to target creature;
    /// destroy all creatures". The 1-damage half is a whiff on the 3/3, but the
    /// `DestroyAll` half (CR 701.8) is an independent, useful mass-removal
    /// line. `Effect::DestroyAll` is dispatched DIRECTLY (it bypasses the
    /// target-only `extract_target_filter`) and the cast-commit gate resolves
    /// it against the real opposing population (the 3/3) through the
    /// resolver-mirroring mass path
    /// (`mass_effect_has_opposing_population` — battlefield population matched
    /// with `matches_target_filter`, CR 115.10a; DestroyAll is non-targeted),
    /// not via `find_legal_targets`, which gates target legality.
    #[test]
    fn can_kill_fails_open_on_mixed_damage_and_destroy_all() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let spell_id = create_object(
            &mut state,
            CardId(90_006),
            PlayerId(0),
            "Charred Cataclysm".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DestroyAll {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    cant_regenerate: false,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_006),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            can_kill_any_legal_target(&ctx),
            "mixed deal-1 + destroy-all must fail open: `DestroyAll` (CR 701.8) \
             resolves a real opposing population (the 3/3) through the \
             resolver-mirroring mass population path \
             (`mass_effect_has_opposing_population`, CR 115.10a), so the spell \
             is not a total whiff even though 1 damage alone cannot kill the 3/3"
        );
    }

    /// A mixed damage + wipe spell whose `DestroyAll` population carries a
    /// player-RELATIVE controller scope (`ControllerRef::TargetOpponent`, a
    /// companion "destroy all creatures target opponent controls" wipe) with
    /// the companion player target NOT bound at cast-commit. The engine
    /// resolves that scope by reading the first `TargetRef::Player` from
    /// `ability.targets` and FAILS CLOSED without it (CR 109.4 / CR 115.1),
    /// while `destroy::resolve_all` resolves it later via
    /// `FilterContext::from_ability` after the companion is announced
    /// (CR 601.2c). The population is therefore UNKNOWABLE at cast-commit:
    /// `mass_effect_has_opposing_population` must report `None` (unknown),
    /// the seam must fail open (`has_opposing_mass_population == true`), and
    /// the mixed spell must not be vetoed. Pre-fix the population read as
    /// empty → the 1-damage half vetoed the whole cast.
    #[test]
    fn mass_population_unknown_for_unbound_player_controller() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let wipe_filter =
            TargetFilter::Typed(TypedFilter::creature().controller(ControllerRef::TargetOpponent));

        let spell_id = create_object(
            &mut state,
            CardId(90_020),
            PlayerId(0),
            "Player-targeted Cataclysm".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DestroyAll {
                    target: wipe_filter.clone(),
                    cant_regenerate: false,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_020),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let source = ctx.source_object().unwrap();
        // (a) The helper's own discriminating seam: unbound player-relative
        // controller → UNKNOWN (`None`), not a provable empty (`Some(false)`).
        assert_eq!(
            mass_effect_has_opposing_population(&ctx, source, &wipe_filter),
            None,
            "an unbound player-relative controller scope (TargetOpponent) must read UNKNOWN"
        );
        // (b) The seam threads the fail-open: an UNKNOWN population is useful.
        assert!(
            has_opposing_mass_population(&ctx),
            "an unknown (unbound player-relative) mass population must fail open through the seam"
        );
        // (c) The mixed spell is not vetoed (the unknown wipe rescues the
        // non-lethal damage half).
        assert!(
            can_kill_any_legal_target(&ctx),
            "mixed deal-1 + player-relative wipe must not be vetoed when the \
             population is unknown at cast-commit"
        );
    }

    /// Mixed removal spell with a DEFAULT-population wipe line: "deal 1 damage
    /// to target creature; destroy all permanents" where the `DestroyAll` half
    /// declares `TargetFilter::None`. The engine resolver (`destroy.rs`
    /// `resolve_all`) treats `None` as its DEFAULT population — all creatures —
    /// so the 3/3 is a wipe target even though the spell declares no filter
    /// (CR 701.8). Pre-fix, the gate fed the raw `None` through
    /// `find_legal_targets` (the extraction-as-target error), which reads an
    /// empty set: the wipe half was not credited and the 1-damage half vetoed
    /// the whole cast. Post-fix the dispatch is direct — `Effect::DestroyAll`
    /// bypasses the target-only `extract_target_filter` and is resolved
    /// resolver-mirroring, so the gate must fail open via the
    /// mass-population path (CR 115.10a).
    #[test]
    fn can_kill_fails_open_on_default_population_destroy_all() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let spell_id = create_object(
            &mut state,
            CardId(90_007),
            PlayerId(0),
            "Charred Judgement".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            // `None` is the serde default for `DestroyAll.target`, but construct
            // it explicitly: the resolver's `None` -> all-creatures default
            // population is the whole point of this test.
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DestroyAll {
                    target: TargetFilter::None,
                    cant_regenerate: false,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_007),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            can_kill_any_legal_target(&ctx),
            "mixed deal-1 + default-population destroy-all must fail open: the \
             resolver's default population (`None` -> all creatures, destroy.rs \
             `resolve_all`) makes the 3/3 a wipe target (CR 701.8) even though \
             the spell declares no filter, so the spell is not a total damage \
             whiff even though 1 damage alone cannot kill the 3/3"
        );
    }

    /// Wipe population counts HEXPROOF opponent creatures: "destroy all
    /// creatures" against a board whose only opposing creature is hexproof.
    /// Hexproof gates TARGETING only (CR 115.10a) — an affected object is not a
    /// target — so it never protects anything from a non-targeted wipe's
    /// population, exactly as the resolver matches it (`destroy.rs`
    /// `resolve_all`). Pre-fix, `find_legal_targets` excluded the hexproof
    /// creature on target legality, so the wipe half credited nothing. The
    /// helper-level assert pins the resolver-semantics seam directly; the
    /// can_kill-level empty-set clause would fail open even pre-fix, so it is a
    /// secondary guard.
    #[test]
    fn mass_population_counts_protected_opponent_creatures() {
        let mut state = make_state();
        let hexproof_bear = add_creature(&mut state, PlayerId(1), "Hexproof Bear", 3, 3);
        state
            .objects
            .get_mut(&hexproof_bear)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let spell_id = create_object(
            &mut state,
            CardId(90_008),
            PlayerId(0),
            "Hexproof-Proof Wipe".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::DestroyAll {
                target: TargetFilter::Typed(TypedFilter::creature()),
                cant_regenerate: false,
            },
        )]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_008),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };

        let filter = TargetFilter::Typed(TypedFilter::creature());
        // THE discriminating seam: pre-fix, `find_legal_targets` returned false
        // for this hexproof-only board (target legality), so the wipe half
        // credited nothing even though the resolver destroys the hexproof bear.
        assert!(
            mass_effect_has_opposing_population(&ctx, ctx.source_object().unwrap(), &filter)
                == Some(true),
            "the wipe's resolver-mirroring population must count hexproof \
             opponent creatures: hexproof gates targeting only (CR 115.10a), \
             and the resolver (destroy.rs `resolve_all`) matches the population \
             with no target-legality exemptions"
        );
        assert!(
            can_kill_any_legal_target(&ctx),
            "destroy-all vs a hexproof-only opposing board must fail open: the \
             wipe is non-targeted (CR 115.10a), so hexproof does not protect \
             the 3/3 from the mass-removal line"
        );
    }

    /// Own-controller-constrained control line: "deal 1 damage to target
    /// creature; gain control of target creature YOU control". `GainControl` is
    /// `Contextual`, but its `TypedFilter.controller` is `ControllerRef::You`
    /// (CR 108.4 / CR 109.5), so `find_legal_targets` names only the caster's
    /// own creatures — there is no legal OPPOSING population for the control
    /// line. Only the whiff 1-damage half remains, so the gate vetoes.
    #[test]
    fn can_kill_vetoes_when_control_line_is_own_controller_constrained() {
        let mut state = make_state();
        // The AI's own bear keeps the You-constrained population NON-empty —
        // the veto must come from the controller axis, not the empty set.
        add_creature(&mut state, PlayerId(0), "My Bear", 2, 1);
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Creature],
                controller: Some(ControllerRef::You),
                ..Default::default()
            }),
            |ctx| {
                assert!(
                    !can_kill_any_legal_target(ctx),
                    "an own-controller-constrained control line (CR 108.4/109.5) must \
                     NOT be credited as opposing removal: the You filter names only the \
                     caster's own bear, so only the whiff 1-damage half remains and \
                     the gate vetoes the total damage whiff"
                );
            },
        );
    }

    /// Mixed spell: "deal 1 damage to target creature; gain control of target
    /// creature". The 1-damage half is a whiff on the 3/3, but `GainControl`
    /// (CR 613.1b, Layer 2) is an independent, useful control line. `GainControl`
    /// is classified `EffectPolarity::Contextual`, so the fail-open guard must
    /// cover Contextual non-`DealDamage` effects — not just `Harmful` ones.
    /// Without that extension this spell is wrongly vetoed as a total *damage*
    /// whiff.
    #[test]
    fn can_kill_fails_open_on_mixed_damage_and_gain_control_creature() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let spell_id = create_object(
            &mut state,
            CardId(90_003),
            PlayerId(0),
            "Charmed Lightning".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainControl {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_003),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            can_kill_any_legal_target(&ctx),
            "mixed deal-1 + gain control of a creature must fail open: the \
             control half (CR 613.1b, Layer 2) is a useful line even though \
             1 damage alone cannot kill the 3/3"
        );
    }

    /// Same mixed-shape spell, but the control half targets ANY permanent
    /// ("gain control of target permanent" — planeswalkers, artifacts, lands,
    /// enchantments, CR 613.1b) instead of only creatures. The fail-open must
    /// not require a creature filter: `permanent()` here targets the
    /// opponent's artifact, a legal and useful control line invisible to
    /// creature-only filters.
    #[test]
    fn can_kill_fails_open_on_mixed_damage_and_gain_control_of_permanent() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);
        add_artifact(&mut state, PlayerId(1), "Opponent Rock");

        let spell_id = create_object(
            &mut state,
            CardId(90_004),
            PlayerId(0),
            "Charmed Heist".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainControl {
                    target: TargetFilter::Typed(TypedFilter::permanent()),
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_004),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            can_kill_any_legal_target(&ctx),
            "mixed deal-1 + gain control of a permanent must fail open: the \
             permanent-stealing half (CR 613.1b, Layer 2) is useful against the \
             opponent's artifact even though 1 damage alone cannot kill the 3/3"
        );
    }

    /// Mixed spell with a parameterized `GainControl` filter: "deal 1 damage to
    /// target creature" (a whiff on a 3/3) plus "gain control of [filter]".
    /// The `GainControl` half is `EffectPolarity::Contextual`, so the fail-open
    /// guard must recognize whichever target-filter shape `filter` names; a
    /// filter the guard cannot analyse wrongly vetoes the control line
    /// (CR 613.1b, Layer 2).
    fn with_mixed_gain_control_spell<R>(
        state: &mut GameState,
        control_filter: TargetFilter,
        f: impl FnOnce(&PolicyContext<'_>) -> R,
    ) -> R {
        let spell_id = create_object(
            state,
            CardId(90_005),
            PlayerId(0),
            "Charmed Heist Shapes".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::GainControl {
                    target: control_filter,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_005),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        f(&ctx)
    }

    /// The control half targets "artifact or creature" via a NESTED
    /// TypeFilter-level `AnyOf(AnyOf(artifact, creature))` disjunction — the
    /// opponent's artifact is a legal control target (CR 613.1b, Layer 2). The
    /// pre-commit `AnyOf` arm only matched a single level of plain
    /// permanent-type inners, so this nested disjunction fell through to the
    /// catch-all and vetoed the mixed spell; the recursive
    /// helper must descend into nested `AnyOf`. A sibling
    /// `AnyOf(Non(Land), Non(Creature))` case pins the same recursion over
    /// negated inners.
    #[test]
    fn can_kill_fails_open_on_anyof_gain_control_target() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);
        add_artifact(&mut state, PlayerId(1), "Opponent Rock");

        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::AnyOf(vec![TypeFilter::AnyOf(vec![
                    TypeFilter::Artifact,
                    TypeFilter::Creature,
                ])])],
                ..Default::default()
            }),
            |ctx| {
                assert!(
                    can_kill_any_legal_target(ctx),
                    "nested AnyOf(AnyOf(artifact, creature)) control half must fail \
                     open: the outer disjunction wraps a disjunction naming the \
                     opponent's artifact (CR 613.1b, Layer 2), a useful control line \
                     the recursive matcher must descend to find"
                );
            },
        );

        // Negated inners: "nonland, noncreature" is `AnyOf(Non(Land), Non(Creature))`
        // — the opponent's artifact satisfies the Non(Land) alternative, so the
        // recursive helper must descend through the disjunction into the negation.
        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::AnyOf(vec![
                    TypeFilter::Non(Box::new(TypeFilter::Land)),
                    TypeFilter::Non(Box::new(TypeFilter::Creature)),
                ])],
                ..Default::default()
            }),
            |ctx| {
                assert!(
                    can_kill_any_legal_target(ctx),
                    "AnyOf(Non(Land), Non(Creature)) control half must fail open: \
                     the Non(Land) alternative matches the opponent's artifact \
                     (CR 613.1b, Layer 2), a useful control line the recursive \
                     matcher must descend to find"
                );
            },
        );
    }

    /// The control half targets "nonland" via `TypeFilter::Non` — the opponent's
    /// artifact matches ("nonland, noncreature permanent" filters are the
    /// canonical Non shape), a legal control target (CR 613.1b, Layer 2). The
    /// guard must treat `Non(Land)` as able to match a permanent; the catch-all
    /// vetoed every Non shape, including "noncreature permanent" control lines.
    #[test]
    fn can_kill_fails_open_on_non_land_gain_control_target() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);
        add_artifact(&mut state, PlayerId(1), "Opponent Rock");

        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Typed(TypedFilter {
                type_filters: vec![TypeFilter::Non(Box::new(TypeFilter::Land))],
                ..Default::default()
            }),
            |ctx| {
                assert!(
                    can_kill_any_legal_target(ctx),
                    "Non(Land) control half must fail open: the negation matches \
                     the opponent's artifact (CR 613.1b, Layer 2), so the mixed \
                     spell is not a total whiff"
                );
            },
        );
    }

    /// The control half targets "a permanent, or a player" via a TargetFilter
    /// level `Or` — the opponent's artifact satisfies the permanent alternative
    /// (CR 613.1b, Layer 2). The guard must walk `TargetFilter::Or` branches;
    /// the `let Some(TargetFilter::Typed(..))` destructure returned `false`
    /// for any non-Typed shape and vetoed the mixed spell.
    #[test]
    fn can_kill_fails_open_on_or_filter_gain_control_target() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);
        add_artifact(&mut state, PlayerId(1), "Opponent Rock");

        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Or {
                filters: vec![
                    TargetFilter::Typed(TypedFilter::permanent()),
                    TargetFilter::Player,
                ],
            },
            |ctx| {
                assert!(
                    can_kill_any_legal_target(ctx),
                    "Or(permanent, player) control half must fail open: the \
                     permanent branch is a useful control line against the \
                     opponent's artifact (CR 613.1b, Layer 2)"
                );
            },
        );
    }

    /// Variable-X damage found anywhere in the spell — including a `DealDamage`
    /// whose target filter is NOT creature-only — must fail open. The X-scan
    /// covers every `DealDamage`, not just the creature-only ones. Here, a
    /// creature-only fixed-damage spell carries a sibling `DealDamage` to "any
    /// target" with X. The caster could choose X at announcement (CR 107.3a) to
    /// make the spell lethal, so even though the creature-only half is a provable
    /// whiff on the 3/3, the spell as a whole must not be vetoed.
    #[test]
    fn can_kill_fails_open_when_non_creature_deal_x_present() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(1), "Opponent Bear", 3, 3);

        let x_amount = QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        };
        let spell_id = create_object(
            &mut state,
            CardId(90_003),
            PlayerId(0),
            "X-Blast".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: x_amount,
                    // ANY target (player/planeswalker/creature) — NOT creature-only.
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_003),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            can_kill_any_legal_target(&ctx),
            "a sibling non-creature-only deal-X means X is castable-lethal; the \
             spell must fail open despite the creature-only half being a whiff"
        );
    }

    /// Two-Headed Giant teammate regression: a teammate's creature is a LEGAL
    /// target (in `find_legal_targets`) and sits in any battlefield population,
    /// but is NOT an opponent under the team-aware relation (CR 102.2 /
    /// CR 102.3 + 2HG topology: P0/P1 are teammates, P2/P3 are P0's opponents).
    /// Pre-fix, the removal-line predicates used `controller != ai_player`,
    /// which wrongly credited a teammate-only creature/population as an
    /// opposing removal line. `players::is_opponent` is the authority.
    #[test]
    fn opposing_lines_are_team_aware_in_two_headed_giant() {
        let mut state = GameState::new(
            engine::types::format::FormatConfig::two_headed_giant(),
            4,
            42,
        );
        // P1 is P0's teammate in 2HG (topology: team_id = player.0 / team_size).
        add_creature(&mut state, PlayerId(1), "Teammate Bear", 2, 2);

        // Teammate only — a legal target, but no opposing removal line.
        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Typed(TypedFilter::creature()),
            |ctx| {
                let gc = Effect::GainControl {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                };
                assert!(
                    !effect_has_legal_opposing_line(ctx, &gc),
                    "a teammate's creature must NOT be an opposing removal line: it is a \
                     legal target but not an opponent (CR 102.2/102.3 + 2HG topology)"
                );
                assert!(
                    mass_effect_has_opposing_population(
                        ctx,
                        ctx.source_object().unwrap(),
                        &TargetFilter::Typed(TypedFilter::creature())
                    ) == Some(false),
                    "a teammate's creature must NOT be an opposing wipe population \
                     (CR 102.2/102.3 + 2HG topology)"
                );
            },
        );

        // P2 IS P0's opponent in 2HG — the same shapes must now credit it.
        add_creature(&mut state, PlayerId(2), "Enemy Bear", 3, 3);
        with_mixed_gain_control_spell(
            &mut state,
            TargetFilter::Typed(TypedFilter::creature()),
            |ctx| {
                let gc = Effect::GainControl {
                    target: TargetFilter::Typed(TypedFilter::creature()),
                };
                assert!(
                    effect_has_legal_opposing_line(ctx, &gc),
                    "an enemy (P2) creature must be an opposing removal line \
                     (CR 102.2/102.3 + 2HG topology)"
                );
                assert!(
                    mass_effect_has_opposing_population(
                        ctx,
                        ctx.source_object().unwrap(),
                        &TargetFilter::Typed(TypedFilter::creature())
                    ) == Some(true),
                    "an enemy (P2) creature must be an opposing wipe population \
                     (CR 102.2/102.3 + 2HG topology)"
                );
            },
        );
    }

    /// Direct seam test: `has_opposing_mass_population` — the cast-commit seam
    /// consulted by `anti_self_harm::score_pre_cast` BEFORE the target-legality
    /// gate — must report TRUE for a mixed damage+wipe spell whose only opposing
    /// creature is HEXPROOF. The wipe is NON-targeted (CR 115.10a), so the
    /// hexproof 3/3 is in its resolver population even though it has no legal
    /// target (hexproof gates targeting only, CR 702.11b). This is the
    /// population truth that rescues the mixed spell from the no-target
    /// penalty in `anti_self_harm::score_pre_cast`.
    #[test]
    fn seam_has_opposing_mass_population_counts_hexproof_opponent() {
        let mut state = make_state();
        add_creature(&mut state, PlayerId(0), "My Bear", 2, 1);
        let hexproof_bear = add_creature(&mut state, PlayerId(1), "Hexproof Bear", 3, 3);
        state
            .objects
            .get_mut(&hexproof_bear)
            .unwrap()
            .keywords
            .push(Keyword::Hexproof);

        let spell_id = create_object(
            &mut state,
            CardId(90_009),
            PlayerId(0),
            "Wipe Plus Damage".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&spell_id).unwrap();
        obj.abilities = Arc::new(vec![
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Typed(TypedFilter::creature()),
                    damage_source: None,
                    excess: None,
                },
            ),
            AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::DestroyAll {
                    target: TargetFilter::None,
                    cant_regenerate: false,
                },
            ),
        ]);

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id: CardId(90_009),
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Spell),
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        assert!(
            has_opposing_mass_population(&ctx),
            "the mixed wipe's resolver-mirroring population must include the hexproof 3/3 \
             (CR 115.10a: the wipe is NON-targeted, so hexproof gates targeting only, \
             CR 702.11b)"
        );
    }

    // ---------------------------------------------------------------------
    // CR 601.2d — divided damage ("deals N damage divided as you choose")
    // ---------------------------------------------------------------------

    /// A Bogardan-Hellkite-shaped divided-damage ETB trigger mid-target-
    /// selection, with `already_selected` declared in
    /// `TargetSelectionProgress.selected_slots` (CR 601.2c) and a `ChooseTarget`
    /// candidate naming `candidate_target`.
    ///
    /// `trigger_unit` is what `PendingTrigger.distribute` carries — the ENGINE's
    /// authority for the division. The `ResolvedAbility` deliberately leaves its
    /// own `distribute` at the `ResolvedAbility::new` default (`None`) so the
    /// fixture also pins WHICH field the term reads.
    ///
    /// Shape verified against `data/card-data.json`, "bogardan hellkite":
    /// `mode: ChangesZone`, `execute: { DealDamage { Fixed 5, Any },
    /// multi_target { min: 0, max: 5 }, distribute: Damage }`.
    fn with_divided_damage_trigger<R>(
        state: &mut GameState,
        trigger_unit: Option<DistributionUnit>,
        already_selected: &[ObjectId],
        candidate_target: Option<TargetRef>,
        f: impl FnOnce(&PolicyContext<'_>) -> R,
    ) -> R {
        let source = create_object(
            state,
            CardId(90_100),
            PlayerId(0),
            "Bogardan Hellkite".to_string(),
            Zone::Battlefield,
        );
        state.pending_trigger = Some(Box::new(PendingTrigger {
            source_id: source,
            controller: PlayerId(0),
            condition: None,
            ability: Box::new(ResolvedAbility::new(
                Effect::DealDamage {
                    amount: QuantityExpr::Fixed { value: 5 },
                    target: TargetFilter::Any,
                    damage_source: None,
                    excess: None,
                },
                Vec::new(),
                source,
                PlayerId(0),
            )),
            timestamp: 1,
            target_constraints: Vec::new(),
            distribute: trigger_unit,
            trigger_event: None,
            modal: None,
            mode_abilities: Vec::new(),
            description: None,
            may_trigger_origin: None,
            subject_match_count: None,
            die_result: None,
            provenance: None,
        }));

        let config = AiConfig::default();
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TriggerTargetSelection {
                player: PlayerId(0),
                trigger_controller: Some(PlayerId(0)),
                trigger_event: None,
                trigger_events: Vec::new(),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: Vec::new(),
                    optional: true,
                    chooser: None,
                    effect_kind: EffectKind::DealDamage,
                    effect_detail: TargetEffectDetail::None,
                }],
                mode_labels: Vec::new(),
                target_constraints: Vec::new(),
                selection: TargetSelectionProgress {
                    current_slot: already_selected.len(),
                    selected_slots: already_selected
                        .iter()
                        .map(|id| Some(TargetRef::Object(*id)))
                        .collect(),
                    current_legal_targets: Vec::new(),
                },
                source_id: Some(source),
                description: None,
            },
            candidates: Vec::new(),
        };
        let context = crate::context::AiContext::empty(&config.weights);
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: candidate_target,
            },
            metadata: ActionMetadata::for_actor(Some(PlayerId(0)), TacticalClass::Target),
        };
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        f(&ctx)
    }

    /// CR 601.2d, first step: nothing is declared yet, so the whole 5-point pool
    /// is available and the 4-toughness body is killable.
    #[test]
    fn divided_damage_first_target_gets_full_budget() {
        let mut state = make_state();
        let bear = add_creature(&mut state, PlayerId(1), "Four Tough", 1, 4);

        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[],
            Some(TargetRef::Object(bear)),
            |ctx| {
                let target = ctx.state.objects.get(&bear).unwrap();
                assert_eq!(
                    divided_damage_verdict(ctx, bear, target),
                    Some(DividedDamage::Lethal),
                    "the first chosen target of a 5-damage division can take the whole pool"
                );
            },
        );
    }

    /// CR 601.2d, later steps: the already-declared 4-toughness target reserves
    /// the 4 points that kill it, so only 1 point remains. A 3-toughness body
    /// can no longer be killed; a 1-toughness body still can.
    ///
    /// REVERT-FAILING: without the remaining-budget model every candidate is
    /// priced against the full 5, so the 3-toughness body reads lethal and the
    /// AI keeps adding targets until the slot maximum — at which point CR 601.2d
    /// forces one point each and nothing dies.
    #[test]
    fn divided_damage_budget_shrinks_after_selected_targets() {
        let mut state = make_state();
        let big = add_creature(&mut state, PlayerId(1), "Four Tough", 1, 4);
        let medium = add_creature(&mut state, PlayerId(1), "Three Tough", 1, 3);
        let small = add_creature(&mut state, PlayerId(1), "One Tough", 1, 1);

        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[big],
            Some(TargetRef::Object(medium)),
            |ctx| {
                let target = ctx.state.objects.get(&medium).unwrap();
                assert_eq!(
                    divided_damage_verdict(ctx, medium, target),
                    Some(DividedDamage::CannotKill),
                    "with 4 of 5 points reserved by the declared target, 1 point cannot \
                     kill a 3-toughness creature (CR 704.5g)"
                );
            },
        );
        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[big],
            Some(TargetRef::Object(small)),
            |ctx| {
                let target = ctx.state.objects.get(&small).unwrap();
                assert_eq!(
                    divided_damage_verdict(ctx, small, target),
                    Some(DividedDamage::Lethal),
                    "the last remaining point still kills a 1-toughness creature"
                );
            },
        );
    }

    /// CR 601.2d, exhausted pool: the declared 4- and 1-toughness targets reserve
    /// all 5 points, so any further creature target is a categorical Reject —
    /// adding it would force a point away from a body that dies. The finishing
    /// `ChooseTarget { target: None }` keeps a finite score, so it wins.
    ///
    /// REVERT-FAILING: pre-fix both candidates score, and the extra target's
    /// full-pool "lethal" bonus beats finishing.
    #[test]
    fn divided_damage_exhausted_budget_rejects_further_targets() {
        let mut state = make_state();
        let big = add_creature(&mut state, PlayerId(1), "Four Tough", 1, 4);
        let small = add_creature(&mut state, PlayerId(1), "One Tough", 1, 1);
        let extra = add_creature(&mut state, PlayerId(1), "Two Tough", 1, 2);

        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[big, small],
            Some(TargetRef::Object(extra)),
            |ctx| {
                assert_eq!(divided_damage_remaining_budget(ctx), Some(0));
                assert!(
                    matches!(
                        EvasionRemovalPriorityPolicy.verdict(ctx),
                        PolicyVerdict::Reject { .. }
                    ),
                    "a target the exhausted pool cannot kill must be rejected categorically"
                );
            },
        );
        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[big, small],
            None,
            |ctx| {
                assert!(
                    matches!(
                        EvasionRemovalPriorityPolicy.verdict(ctx),
                        PolicyVerdict::Score { .. }
                    ),
                    "finishing target selection must stay a finite score, so it outranks \
                     the rejected extra target"
                );
            },
        );
    }

    /// CR 601.2d, exhausted pool, NON-creature recipient: with the declared
    /// 4- and 1-toughness targets holding all 5 points, even a face-damage
    /// target is rejected — its mandatory one point has to come out of a
    /// reserve that was killing a creature. The player exemption ("chip damage
    /// to the face is legitimate") only holds while a point is actually spare.
    #[test]
    fn divided_damage_exhausted_budget_rejects_a_player_target_too() {
        let mut state = make_state();
        let big = add_creature(&mut state, PlayerId(1), "Four Tough", 1, 4);
        let small = add_creature(&mut state, PlayerId(1), "One Tough", 1, 1);
        let face = Some(TargetRef::Player(PlayerId(1)));

        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[big, small],
            face.clone(),
            |ctx| {
                assert_eq!(divided_damage_remaining_budget(ctx), Some(0));
                assert!(
                    matches!(
                        EvasionRemovalPriorityPolicy.verdict(ctx),
                        PolicyVerdict::Reject { .. }
                    ),
                    "an exhausted pool must reject a PLAYER target too — its \
                     CR 601.2d minimum comes out of a lethal reserve"
                );
            },
        );

        // One point still spare: face damage is a legitimate use of it again.
        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[big],
            face,
            |ctx| {
                assert_eq!(divided_damage_remaining_budget(ctx), Some(1));
                assert!(
                    matches!(
                        EvasionRemovalPriorityPolicy.verdict(ctx),
                        PolicyVerdict::Score { .. }
                    ),
                    "with a point to spare, chip damage to the face keeps a finite score"
                );
            },
        );
    }

    /// A pending ability that does NOT divide its damage is untouched: the term
    /// reports `None` and every non-divided damage spell keeps today's
    /// full-amount lethality model.
    #[test]
    fn non_divided_damage_verdict_is_none() {
        let mut state = make_state();
        let bear = add_creature(&mut state, PlayerId(1), "Four Tough", 1, 4);

        with_divided_damage_trigger(
            &mut state,
            None,
            &[],
            Some(TargetRef::Object(bear)),
            |ctx| {
                let target = ctx.state.objects.get(&bear).unwrap();
                assert_eq!(
                    divided_damage_verdict(ctx, bear, target),
                    None,
                    "without a division there is no budget to model"
                );
            },
        );
    }

    /// R5 pin: the division is read from the ENGINE's authority —
    /// `PendingTrigger.distribute` — not from the `ResolvedAbility` copy. The
    /// fixture's `ResolvedAbility::new` leaves `distribute` at `None`, so a term
    /// reading the ability copy would report `None` here.
    #[test]
    fn divided_damage_reads_distribute_from_the_pending_trigger() {
        let mut state = make_state();
        let bear = add_creature(&mut state, PlayerId(1), "Four Tough", 1, 4);

        with_divided_damage_trigger(
            &mut state,
            Some(DistributionUnit::Damage),
            &[],
            Some(TargetRef::Object(bear)),
            |ctx| {
                assert!(
                    ctx.state
                        .pending_trigger
                        .as_ref()
                        .is_some_and(|trigger| trigger.ability.distribute.is_none()),
                    "fixture premise: the ResolvedAbility copy carries no division"
                );
                let target = ctx.state.objects.get(&bear).unwrap();
                assert!(
                    divided_damage_verdict(ctx, bear, target).is_some(),
                    "the division must be read from PendingTrigger.distribute"
                );
            },
        );
    }
}

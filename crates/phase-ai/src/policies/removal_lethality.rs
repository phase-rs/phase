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

use engine::game::game_object::GameObject;
use engine::game::keywords::object_has_effective_keyword_kind;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::{DamageSource, Effect};
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::{Keyword, KeywordKind};

use super::context::PolicyContext;
use super::effect_classify::effect_targets_object;

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
        Some(DamageSource::Target | DamageSource::EachTarget | DamageSource::TriggeringSource) => {
            EffectDamageSource::Unresolved
        }
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
                let EffectDamageSource::Object(source_id) =
                    effect_damage_source(ctx, damage_source.as_ref())
                else {
                    return PendingDamage::Unresolved;
                };
                found = true;
                let dealt = u32::try_from(
                    resolve_quantity(ctx.state, amount, ctx.ai_player, source_id).max(0),
                )
                .unwrap_or(u32::MAX);
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

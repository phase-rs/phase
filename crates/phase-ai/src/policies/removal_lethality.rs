//! Removal-lethality assessment — makes a direct-damage removal spell prefer a
//! target its damage can actually KILL over the biggest body on the board.
//!
//! ## The defect this closes (#6582)
//!
//! [`EvasionRemovalPriorityPolicy`](super::evasion_removal_priority) ranks
//! removal targets by threat value, so it points a fixed-damage burn spell at
//! the biggest creature — a 7/7 — even when the spell deals 3 and cannot
//! destroy it, wasting the card. CR 120.6 / CR 704.5g: a creature is destroyed
//! only once the damage marked on it reaches its toughness; CR 702.12b: an
//! indestructible creature is never destroyed by lethal damage. The AI modelled
//! neither, so "kills it" and "tickles it" scored the same and the biggest
//! threat always won.
//!
//! ## Building block, not a card fix
//!
//! These are pure functions over the pending spell's own `DealDamage` effects
//! and the target's runtime toughness / marked damage / indestructibility — a
//! reusable primitive any removal-targeting policy can consult, covering every
//! direct-damage removal spell rather than one card. The term is inert (`0.0`)
//! whenever the pending effect deals no damage to the target, so `-X/-X`,
//! destroy, and exile removal are untouched.

use engine::game::game_object::GameObject;
use engine::game::quantity::resolve_quantity;
use engine::types::ability::Effect;
use engine::types::identifiers::ObjectId;
use engine::types::keywords::Keyword;

use super::context::PolicyContext;
use super::effect_classify::effect_targets_object;

/// Reward for a target the removal spell actually destroys — a clean kill is
/// worth more than the marginal threat-value ranking that lured the AI to an
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

/// Total damage the pending spell/ability will deal to `target_id`, resolved
/// against live game state (so `X` and dynamic amounts are concrete). `None`
/// when the pending effect deals no damage to that object at all — the signal
/// the caller uses to stay inert for non-damage removal.
///
/// CR 120.3e: damage from a source without wither/infect is marked on the
/// creature; multiple `DealDamage` effects on one spell stack additively.
pub(crate) fn pending_damage_to_object(
    ctx: &PolicyContext<'_>,
    target_id: ObjectId,
) -> Option<u32> {
    let source_id = ctx.source_object()?.id;
    let mut total: i64 = 0;
    let mut found = false;
    for effect in ctx.effects() {
        if let Effect::DealDamage { amount, .. } = effect {
            if effect_targets_object(ctx, effect, target_id) {
                found = true;
                total +=
                    i64::from(resolve_quantity(ctx.state, amount, ctx.ai_player, source_id).max(0));
            }
        }
    }
    found.then_some(total.clamp(0, i64::from(u32::MAX)) as u32)
}

/// CR 120.6 + CR 704.5g: `damage` is lethal to `target` when the target has
/// toughness greater than 0 and the damage added to what is already marked
/// reaches that toughness — UNLESS the creature is indestructible (CR 702.12b),
/// which ignores the lethal-damage state-based action. A creature already at 0
/// toughness is dying to its own SBA, not to this spell, so it is not "killed"
/// by the damage here.
pub(crate) fn damage_is_lethal(target: &GameObject, damage: u32) -> bool {
    if target.has_keyword(&Keyword::Indestructible) {
        return false;
    }
    let toughness = target.toughness.unwrap_or(0);
    if toughness <= 0 {
        return false;
    }
    target.damage_marked.saturating_add(damage) >= toughness as u32
}

/// Lethality contribution for pointing a damage removal spell at `target`.
///
/// * Lethal (CR 704.5g destroy) → `+LETHAL_BONUS`: a clean kill.
/// * Survives (ordinary high toughness OR indestructible, CR 702.12b) → a
///   penalty scaled by the body it failed to kill, so a 3-damage spell on a 7/7
///   ranks well below a smaller target the same spell destroys.
/// * Pending spell deals no damage to the target → `0.0`, leaving non-damage
///   removal targeting (`-X/-X`, destroy, exile) exactly as it was.
pub(crate) fn lethality_bonus(
    ctx: &PolicyContext<'_>,
    target_id: ObjectId,
    target: &GameObject,
) -> f64 {
    let Some(damage) = pending_damage_to_object(ctx, target_id) else {
        return 0.0;
    };
    if damage == 0 {
        return 0.0;
    }
    if damage_is_lethal(target, damage) {
        return LETHAL_BONUS;
    }
    // Survives: the removal is wasted. Penalty grows with the toughness of the
    // creature that lived through it (indestructible bodies count their full
    // toughness), capped so one whiff can't swamp the ranking.
    let survived_toughness = f64::from(target.toughness.unwrap_or(0).max(0));
    -(survived_toughness * WASTE_PENALTY_MULT).min(WASTE_PENALTY_MAX)
}

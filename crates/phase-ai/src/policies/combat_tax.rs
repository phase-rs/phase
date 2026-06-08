//! `CombatTaxPaymentPolicy` — decide whether to pay the aggregate combat tax
//! imposed by UnlessPay combat restrictions (Ghostly Prison, Propaganda, Sphere
//! of Safety, Windborn Muse).
//!
//! Fires only on `GameAction::PayCombatTax` during a `WaitingFor::CombatTaxPayment`
//! pause. The decision is binary (`accept` vs decline); scoring biases the AI
//! toward paying when expected damage exceeds the tax cost by a meaningful
//! margin, scaled down by deck archetype (aggro decks push harder).
//!
//! CR 508.1d + CR 508.1h + CR 509.1c + CR 509.1d: the taxed player may choose
//! either branch; declining drops the taxed creatures from the declaration.

use engine::types::actions::GameAction;
use engine::types::game_state::{CombatTaxContext, GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// When declining the tax would remove this fraction or more of the declared
/// attackers, treat the decision as "would collapse the attack" and bias toward
/// declining unless damage potential is very high. 0.75 = if 3 of 4 attackers
/// are taxed, declining is structurally similar to declining combat.
const ATTACK_COLLAPSE_FRACTION: f64 = 0.75;

/// Base bonus applied when expected damage through exceeds the tax total.
const DAMAGE_EXCEEDS_TAX_BONUS: f64 = 0.35;

/// Base penalty applied when the tax total exceeds expected damage through.
const TAX_EXCEEDS_DAMAGE_PENALTY: f64 = -0.45;

/// Penalty for paying when the tax would consume every mana source we have —
/// leaves us unable to interact on the opponent's turn.
const TAP_OUT_PENALTY: f64 = -0.2;

/// Aggro archetypes pay the tax more aggressively — multiplier on damage-exceeds-tax.
const AGGRO_AMP: f64 = 1.4;

/// Control archetypes conserve mana for interaction — multiplier penalty on acceptance.
const CONTROL_DAMP: f64 = 0.6;

/// Reduced control dampening for blocking decisions to prevent aggressive tax decline
/// that causes blockers to disappear (issue #1541).
const BLOCKING_CONTROL_DAMP: f64 = 0.8;

/// Bonus for paying block tax to preserve valuable blockers.
const BLOCKER_VALUE_BONUS: f64 = 0.25;

pub struct CombatTaxPaymentPolicy;

impl TacticalPolicy for CombatTaxPaymentPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::CombatTaxPayment
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        // CR 508.1d + CR 509.1c: the CombatTaxPayment state maps to Attackers
        // or Blockers by context (see decision_kind::classify).
        &[
            DecisionKind::DeclareAttackers,
            DecisionKind::DeclareBlockers,
        ]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Fires reactively whenever a combat-tax decision lands on an AI seat;
        // archetype weighting lives inside verdict() so the multiplier in the
        // registry is the identity.
        // activation-constant: reactive combat-tax policy.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let accept = match ctx.candidate.action {
            GameAction::PayCombatTax { accept } => accept,
            _ => {
                return PolicyVerdict::Score {
                    delta: 0.0,
                    reason: PolicyReason::new("combat_tax_na"),
                };
            }
        };

        let Some(snap) = extract_tax_state(&ctx.state.waiting_for) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("combat_tax_na"),
            };
        };
        let TaxSnapshot {
            context,
            total_mana_value: total_cost_mv,
            per_creature,
        } = snap;

        // Damage potential: sum of powers of the taxed creatures.
        let expected_damage: i32 = per_creature
            .iter()
            .map(|(id, _)| {
                ctx.state
                    .objects
                    .get(id)
                    .and_then(|obj| obj.power)
                    .unwrap_or(0)
            })
            .sum();
        let tax = total_cost_mv as i32;

        // Total declared attackers/blockers — used to detect "declining would
        // collapse the declaration".
        let total_declared = total_declared_count(&ctx.state.waiting_for);
        let taxed = per_creature.len();
        let collapse_fraction = if total_declared > 0 {
            taxed as f64 / total_declared as f64
        } else {
            0.0
        };

        // Archetype modifier — aggro amplifies, control dampens. Pulls the AI
        // seat's features from the per-session cache (empty DeckFeatures if the
        // seat wasn't registered).
        let default_features = DeckFeatures::default();
        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .unwrap_or(&default_features);
        let archetype_mod = archetype_multiplier(features, context.clone());

        let base_delta = if expected_damage > tax {
            DAMAGE_EXCEEDS_TAX_BONUS * archetype_mod
        } else if expected_damage < tax {
            TAX_EXCEEDS_DAMAGE_PENALTY / archetype_mod.max(0.01)
        } else {
            0.0
        };

        // Mana availability penalty — if we'd tap out and lose interaction.
        let available = count_untapped_mana_sources(ctx.state, ctx.ai_player);
        let tap_out_penalty = if available > 0 && available.saturating_sub(total_cost_mv) == 0 {
            TAP_OUT_PENALTY
        } else {
            0.0
        };

        // If declining would collapse the attack (> fraction taxed), the policy
        // treats that as "might as well pay" and adds a modest additional bonus.
        let collapse_bonus =
            if collapse_fraction >= ATTACK_COLLAPSE_FRACTION && accept && expected_damage > 0 {
                0.15
            } else {
                0.0
            };

        // Blocker value bonus: when blocking, add bonus for paying tax to preserve
        // valuable blockers. This addresses issue #1541 where blockers disappear
        // due to aggressive tax decline.
        let blocker_value_bonus = if matches!(context, CombatTaxContext::Blocking) && accept {
            BLOCKER_VALUE_BONUS
        } else {
            0.0
        };

        let delta = if accept {
            base_delta + tap_out_penalty + collapse_bonus + blocker_value_bonus
        } else {
            // Decline: sign-flipped base_delta (declining is the opposite decision).
            -base_delta
        };

        let kind = if accept {
            "combat_tax_accept"
        } else {
            "combat_tax_decline"
        };
        PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new(kind)
                .with_fact("tax_mv", tax as i64)
                .with_fact("expected_damage", expected_damage as i64)
                .with_fact("taxed", taxed as i64)
                .with_fact("declared", total_declared as i64),
        }
    }
}

/// CombatTaxPayment summary extracted from the `WaitingFor` for scoring.
struct TaxSnapshot {
    context: CombatTaxContext,
    total_mana_value: u32,
    per_creature: Vec<(ObjectId, engine::types::mana::ManaCost)>,
}

/// Extract the CombatTaxPayment waiting state's context, total mana value, and
/// per-creature tax breakdown.
fn extract_tax_state(waiting_for: &WaitingFor) -> Option<TaxSnapshot> {
    if let WaitingFor::CombatTaxPayment {
        context,
        total_cost,
        per_creature,
        ..
    } = waiting_for
    {
        Some(TaxSnapshot {
            context: context.clone(),
            total_mana_value: total_cost.mana_value(),
            per_creature: per_creature.clone(),
        })
    } else {
        None
    }
}

/// Size of the parent declaration (attackers or blockers) from the state — used
/// to compute what fraction of the declaration is taxed.
fn total_declared_count(waiting_for: &WaitingFor) -> usize {
    match waiting_for {
        WaitingFor::CombatTaxPayment { pending, .. } => match pending {
            engine::types::game_state::CombatTaxPending::Attack { attacks, .. } => attacks.len(),
            engine::types::game_state::CombatTaxPending::Block { assignments } => assignments.len(),
        },
        _ => 0,
    }
}

/// Count untapped mana sources (lands + mana rocks) the AI controls. Mirrors
/// the helper used by `HoldManaUpForInteractionPolicy`.
fn count_untapped_mana_sources(state: &GameState, player: PlayerId) -> u32 {
    state
        .battlefield
        .iter()
        .filter(|&&id| {
            let Some(obj) = state.objects.get(&id) else {
                return false;
            };
            if obj.controller != player || obj.tapped {
                return false;
            }
            // Heuristic: lands + artifacts with a mana ability produce mana.
            obj.card_types
                .core_types
                .iter()
                .any(|t| matches!(t, engine::types::card_type::CoreType::Land))
                || obj.abilities.iter().any(|a| {
                    matches!(a.kind, engine::types::ability::AbilityKind::Activated)
                        && matches!(*a.effect, engine::types::ability::Effect::Mana { .. })
                })
        })
        .count() as u32
}

/// CR 109.5 + deck archetype: aggro decks push harder on paying the attack tax
/// (so their attack doesn't collapse); control decks conserve mana for
/// interaction (so they decline the block tax more often).
fn archetype_multiplier(features: &DeckFeatures, context: CombatTaxContext) -> f64 {
    let aggro = features.aggro_pressure.commitment.clamp(0.0, 1.0) as f64;
    let control = features.control.commitment.clamp(0.0, 1.0) as f64;

    match context {
        // Attack side: aggro wants the attack to continue → amplify accept bias.
        CombatTaxContext::Attacking => {
            1.0 + (AGGRO_AMP - 1.0) * aggro - (1.0 - CONTROL_DAMP) * control
        }
        // Block side: reduced control dampening to prevent aggressive tax decline
        // that causes blockers to disappear (issue #1541). Control decks still
        // conserve mana, but the penalty is less severe to preserve valuable blockers.
        CombatTaxContext::Blocking => 1.0 - (1.0 - BLOCKING_CONTROL_DAMP) * control,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::DeckFeatures;
    use engine::types::game_state::{CombatTaxPending, WaitingFor};
    use engine::types::identifiers::ObjectId;
    use engine::types::mana::ManaCost;
    use engine::types::player::PlayerId;

    fn features_aggro() -> DeckFeatures {
        let mut f = DeckFeatures::default();
        f.aggro_pressure.commitment = 0.9;
        f
    }

    fn features_control() -> DeckFeatures {
        let mut f = DeckFeatures::default();
        f.control.commitment = 0.9;
        f
    }

    #[test]
    fn aggro_amplifies_accept_when_damage_exceeds_tax() {
        let aggro = features_aggro();
        let control = features_control();
        let amp_aggro = archetype_multiplier(&aggro, CombatTaxContext::Attacking);
        let amp_control = archetype_multiplier(&control, CombatTaxContext::Attacking);
        assert!(
            amp_aggro > amp_control,
            "aggro amplifier {amp_aggro} should exceed control {amp_control}"
        );
    }

    #[test]
    fn total_declared_count_matches_pending_attack() {
        let waiting = WaitingFor::CombatTaxPayment {
            player: PlayerId(0),
            context: CombatTaxContext::Attacking,
            total_cost: ManaCost::generic(4),
            per_creature: vec![(ObjectId(1), ManaCost::generic(2))],
            pending: CombatTaxPending::Attack {
                attacks: vec![
                    (
                        ObjectId(1),
                        engine::game::combat::AttackTarget::Player(PlayerId(1)),
                    ),
                    (
                        ObjectId(2),
                        engine::game::combat::AttackTarget::Player(PlayerId(1)),
                    ),
                ],
                bands: vec![],
            },
        };
        assert_eq!(total_declared_count(&waiting), 2);
    }

    #[test]
    fn extract_tax_state_returns_none_for_non_combat_tax_state() {
        let waiting = WaitingFor::Priority {
            player: PlayerId(0),
        };
        assert!(extract_tax_state(&waiting).is_none());
    }
}

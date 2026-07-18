//! Generic archetype-payoff tactical policy.
//!
//! The eight `*_payoff` sibling policies (lifegain / mill / energy /
//! enchantments / equipment / blink / reanimator / artifact-synergy) shared an
//! identical spine: they fire only on `DecisionKind::CastSpell`, gate activation
//! on a deck-commitment count, and score the live cast object by first-match
//! classification into ordered tiers. This module parameterizes that spine over
//! a `&'static PayoffSpec` table so a new archetype payoff is one `static
//! PayoffSpec` plus one registry line rather than a whole new file.
//!
//! Each policy keeps its own stable `PolicyId` (telemetry / attribution
//! identity) and its exact per-tier bonuses, reasons, count-gates, and (for
//! mill / energy) state-dependent scaling — the classification predicates are
//! reused verbatim from the feature detectors so the deck-time and cast-time
//! views never drift.

use engine::game::game_object::GameObject;
use engine::game::players;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::config::PolicyPenalties;
use crate::features::DeckFeatures;

// ── mill urgency tiers (moved verbatim from the former mill_payoff.rs) ────────

/// Library size below which mill urgency escalates to ×3.0. At this count the
/// opponent is one moderate mill spell away from an empty library.
pub(crate) const LIBRARY_THRESHOLD_URGENT: usize = 5;

/// Library size below which mill urgency escalates to ×2.0. Fewer than 15
/// cards puts the opponent within two-spell range.
pub(crate) const LIBRARY_THRESHOLD_ELEVATED: usize = 15;

pub(crate) const URGENCY_SCALE_HIGH: f64 = 3.0;
pub(crate) const URGENCY_SCALE_MID: f64 = 2.0;
pub(crate) const URGENCY_SCALE_NORMAL: f64 = 1.0;

// ── energy momentum tiers (moved verbatim from the former energy_payoff.rs) ───

/// Energy reserve at or above which the momentum scale reaches ×3.0. Five
/// banked counters means the deck can threaten a major sink next turn.
pub(crate) const RESERVE_THRESHOLD_HIGH: usize = 5;

/// Energy reserve at or above which the momentum scale reaches ×2.0. Two
/// counters means the engine is producing surplus beyond the first spend.
pub(crate) const RESERVE_THRESHOLD_MID: usize = 2;

pub(crate) const MOMENTUM_SCALE_HIGH: f64 = 3.0;
pub(crate) const MOMENTUM_SCALE_MID: f64 = 2.0;
pub(crate) const MOMENTUM_SCALE_NORMAL: f64 = 1.0;

/// State-dependent scaling applied on top of a tier's flat bonus. `mult`
/// multiplies the base bonus; `facts` are appended to the verdict reason in
/// order. Only mill and energy use this — the other six tiers are flat.
pub(crate) struct PayoffScale {
    pub mult: f64,
    pub facts: Vec<(&'static str, i64)>,
}

/// One ordered classification tier. The first tier whose `matches` predicate
/// holds for the live cast object wins; its `bonus` (a `PolicyPenalties` field
/// accessor) sets the base delta, optionally scaled by `scale`.
pub(crate) struct PayoffTier {
    pub matches: fn(&GameObject) -> bool,
    pub reason: &'static str,
    pub bonus: fn(&PolicyPenalties) -> f64,
    /// `None` → flat bonus, no facts. `Some` → state-dependent multiplier +
    /// observability facts (mill / energy only).
    pub scale: Option<fn(&PolicyContext<'_>, &GameObject) -> PayoffScale>,
}

/// The static description of one archetype-payoff policy.
pub(crate) struct PayoffSpec {
    pub id: PolicyId,
    /// Reason kind for a non-`CastSpell` candidate or a missing object.
    pub na_reason: &'static str,
    /// Reason kind when the object matched no tier.
    pub inert_reason: &'static str,
    pub activation: fn(&DeckFeatures) -> Option<f32>,
    pub tiers: &'static [PayoffTier],
}

/// Generic archetype-payoff policy driven by a `&'static PayoffSpec`.
pub(crate) struct PayoffPolicy {
    spec: &'static PayoffSpec,
}

impl PayoffPolicy {
    pub(crate) const fn new(spec: &'static PayoffSpec) -> Self {
        Self { spec }
    }
}

impl TacticalPolicy for PayoffPolicy {
    fn id(&self) -> PolicyId {
        self.spec.id
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::CastSpell]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        (self.spec.activation)(features)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new(self.spec.na_reason));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new(self.spec.na_reason));
        };

        for tier in self.spec.tiers {
            if (tier.matches)(object) {
                let base = (tier.bonus)(ctx.penalties());
                let mut reason = PolicyReason::new(tier.reason);
                let delta = match tier.scale {
                    Some(scale) => {
                        let s = scale(ctx, object);
                        for (key, value) in s.facts {
                            reason = reason.with_fact(key, value);
                        }
                        base * s.mult
                    }
                    None => base,
                };
                return PolicyVerdict::score(delta, reason);
            }
        }

        PolicyVerdict::neutral(PolicyReason::new(self.spec.inert_reason))
    }
}

// ═══ lifegain ════════════════════════════════════════════════════════════════

fn lifegain_activation(features: &DeckFeatures) -> Option<f32> {
    let lifegain = &features.lifegain;
    if lifegain.payoff_count == 0
        || lifegain.commitment < crate::features::lifegain::COMMITMENT_FLOOR
    {
        None
    } else {
        Some(lifegain.commitment)
    }
}

// CR 702.15a / CR 119.3: a lifegain source feeds "whenever you gain life" payoffs.
fn matches_lifegain_source(object: &GameObject) -> bool {
    let effects: Vec<_> = object
        .abilities
        .iter()
        .flat_map(collect_chain_effects)
        .collect();
    let trigger_borne_source = object
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .any(crate::features::lifegain::is_lifegain_source_trigger);
    crate::features::lifegain::is_lifegain_source_parts(&object.keywords, &effects)
        || trigger_borne_source
}

fn lifegain_source_bonus(p: &PolicyPenalties) -> f64 {
    p.lifegain_source_bonus
}

pub(crate) static LIFEGAIN_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::LifegainPayoff,
    na_reason: "lifegain_payoff_na",
    inert_reason: "lifegain_payoff_inert",
    activation: lifegain_activation,
    tiers: &[PayoffTier {
        matches: matches_lifegain_source,
        reason: "lifegain_source_for_payoff",
        bonus: lifegain_source_bonus,
        scale: None,
    }],
};

// ═══ enchantments ════════════════════════════════════════════════════════════

fn enchantments_activation(features: &DeckFeatures) -> Option<f32> {
    let enchantments = &features.enchantments;
    if enchantments.payoff_count == 0
        || enchantments.commitment < crate::features::enchantments::COMMITMENT_FLOOR
    {
        None
    } else {
        Some(enchantments.commitment)
    }
}

// CR 301.1: an enchantment cast fuels enchantress / constellation payoffs.
fn matches_enchantment(object: &GameObject) -> bool {
    object
        .card_types
        .core_types
        .contains(&CoreType::Enchantment)
}

fn enchantment_cast_bonus(p: &PolicyPenalties) -> f64 {
    p.enchantment_cast_bonus
}

pub(crate) static ENCHANTMENTS_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::EnchantmentsPayoff,
    na_reason: "enchantments_payoff_na",
    inert_reason: "enchantments_payoff_inert",
    activation: enchantments_activation,
    tiers: &[PayoffTier {
        matches: matches_enchantment,
        reason: "enchantment_cast_for_payoff",
        bonus: enchantment_cast_bonus,
        scale: None,
    }],
};

// ═══ artifact synergy ════════════════════════════════════════════════════════

fn artifacts_activation(features: &DeckFeatures) -> Option<f32> {
    if features.artifacts.commitment < crate::features::artifacts::COMMITMENT_FLOOR {
        None
    } else {
        Some(features.artifacts.commitment)
    }
}

// CR 702.41a / CR 702.126a: affinity / improvise spells are the cost payoff.
fn matches_artifact_cost_payoff(object: &GameObject) -> bool {
    crate::features::artifacts::is_artifact_cost_payoff_parts(&object.keywords)
}

// CR 301.1: deploying an artifact grows the count the payoffs scale on.
fn matches_artifact_core(object: &GameObject) -> bool {
    object.card_types.core_types.contains(&CoreType::Artifact)
}

fn artifact_cost_payoff_bonus(p: &PolicyPenalties) -> f64 {
    p.artifact_cost_payoff_bonus
}

fn deploy_artifact_bonus(p: &PolicyPenalties) -> f64 {
    p.deploy_artifact_bonus
}

pub(crate) static ARTIFACT_SYNERGY: PayoffSpec = PayoffSpec {
    id: PolicyId::ArtifactSynergyTactical,
    na_reason: "artifact_synergy_na",
    inert_reason: "artifact_synergy_inert",
    activation: artifacts_activation,
    tiers: &[
        PayoffTier {
            matches: matches_artifact_cost_payoff,
            reason: "artifact_cost_payoff",
            bonus: artifact_cost_payoff_bonus,
            scale: None,
        },
        PayoffTier {
            matches: matches_artifact_core,
            reason: "deploy_artifact_for_synergy",
            bonus: deploy_artifact_bonus,
            scale: None,
        },
    ],
};

// ═══ equipment ═══════════════════════════════════════════════════════════════

fn equipment_activation(features: &DeckFeatures) -> Option<f32> {
    let equipment = &features.equipment;
    if equipment.equipment_count == 0
        || equipment.payoff_count == 0
        || equipment.commitment < crate::features::equipment::COMMITMENT_FLOOR
    {
        None
    } else {
        Some(equipment.commitment)
    }
}

// CR 301.5: deploying an Equipment grows the voltron package.
fn matches_equipment_subtype(object: &GameObject) -> bool {
    crate::features::equipment::subtypes_contain_equipment(&object.card_types.subtypes)
}

// CR 701.23 / CR 702.6 / CR 601.2: an equipment payoff — tutor / auto-attacher /
// equip-cost grant / equipment-cast trigger.
fn matches_equipment_payoff(object: &GameObject) -> bool {
    object
        .static_definitions
        .iter_unchecked()
        .any(crate::features::equipment::static_grants_equip)
        || object
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| &entry.definition)
            .any(crate::features::equipment::trigger_references_equipment)
        || object
            .abilities
            .iter()
            .flat_map(collect_chain_effects)
            .chain(
                object
                    .trigger_definitions
                    .iter_unchecked()
                    .map(|entry| &entry.definition)
                    .filter_map(|trigger| trigger.execute.as_deref())
                    .flat_map(collect_chain_effects),
            )
            .any(crate::features::equipment::effect_is_equipment_support)
}

fn deploy_equipment_bonus(p: &PolicyPenalties) -> f64 {
    p.deploy_equipment_bonus
}

fn equipment_payoff_cast_bonus(p: &PolicyPenalties) -> f64 {
    p.equipment_payoff_cast_bonus
}

pub(crate) static EQUIPMENT_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::EquipmentPayoff,
    na_reason: "equipment_payoff_na",
    inert_reason: "equipment_payoff_inert",
    activation: equipment_activation,
    tiers: &[
        PayoffTier {
            matches: matches_equipment_subtype,
            reason: "deploy_equipment_for_payoff",
            bonus: deploy_equipment_bonus,
            scale: None,
        },
        PayoffTier {
            matches: matches_equipment_payoff,
            reason: "equipment_payoff_cast",
            bonus: equipment_payoff_cast_bonus,
            scale: None,
        },
    ],
};

// ═══ blink ═══════════════════════════════════════════════════════════════════

fn blink_activation(features: &DeckFeatures) -> Option<f32> {
    let blink = &features.blink;
    if blink.flicker_count == 0
        || blink.etb_payoff_count == 0
        || blink.commitment < crate::features::blink::COMMITMENT_FLOOR
    {
        None
    } else {
        Some(blink.commitment)
    }
}

// CR 603.7: deploying the flicker engine.
fn matches_blink_flicker(object: &GameObject) -> bool {
    object.abilities.iter().any(|ability| {
        crate::features::blink::effects_include_flicker(&collect_chain_effects(ability))
    }) || object
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .any(|trigger| {
            trigger.execute.as_deref().is_some_and(|execute| {
                crate::features::blink::effects_include_flicker(&collect_chain_effects(execute))
            })
        })
}

// CR 603.6a: a value-ETB creature is a re-triggerable payoff.
fn matches_blink_etb_payoff(object: &GameObject) -> bool {
    object.card_types.core_types.contains(&CoreType::Creature)
        && object
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| &entry.definition)
            .any(crate::features::blink::trigger_is_value_etb)
}

fn deploy_flicker_engine_bonus(p: &PolicyPenalties) -> f64 {
    p.deploy_flicker_engine_bonus
}

fn etb_payoff_cast_bonus(p: &PolicyPenalties) -> f64 {
    p.etb_payoff_cast_bonus
}

pub(crate) static BLINK_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::BlinkPayoff,
    na_reason: "blink_payoff_na",
    inert_reason: "blink_payoff_inert",
    activation: blink_activation,
    tiers: &[
        PayoffTier {
            matches: matches_blink_flicker,
            reason: "deploy_flicker_engine",
            bonus: deploy_flicker_engine_bonus,
            scale: None,
        },
        PayoffTier {
            matches: matches_blink_etb_payoff,
            reason: "etb_payoff_cast",
            bonus: etb_payoff_cast_bonus,
            scale: None,
        },
    ],
};

// ═══ reanimator ══════════════════════════════════════════════════════════════

fn reanimator_activation(features: &DeckFeatures) -> Option<f32> {
    let reanimator = &features.reanimator;
    if reanimator.reanimation_count == 0
        || reanimator.target_count == 0
        || reanimator.commitment < crate::features::reanimator::COMMITMENT_FLOOR
    {
        None
    } else {
        Some(reanimator.commitment)
    }
}

/// Collect the object's own ability-chain effects together with its
/// trigger-executed chain effects — the same combined slice both reanimator
/// tiers classify against.
fn reanimator_effects(object: &GameObject) -> Vec<&Effect> {
    object
        .abilities
        .iter()
        .flat_map(collect_chain_effects)
        .chain(
            object
                .trigger_definitions
                .iter_unchecked()
                .map(|entry| &entry.definition)
                .filter_map(|trigger| trigger.execute.as_deref())
                .flat_map(collect_chain_effects),
        )
        .collect()
}

// CR 404.1 + CR 110.1: the reanimation itself.
fn matches_reanimation(object: &GameObject) -> bool {
    crate::features::reanimator::effects_include_reanimation(&reanimator_effects(object))
}

// CR 701.17a / CR 701.9a: a graveyard enabler (self-mill / discard outlet).
fn matches_reanimator_enabler(object: &GameObject) -> bool {
    crate::features::reanimator::effects_include_self_graveyard_fill(&reanimator_effects(object))
        || object
            .abilities
            .iter()
            .any(crate::features::reanimator::ability_is_discard_outlet)
}

fn reanimation_cast_bonus(p: &PolicyPenalties) -> f64 {
    p.reanimation_cast_bonus
}

fn graveyard_enabler_bonus(p: &PolicyPenalties) -> f64 {
    p.graveyard_enabler_bonus
}

pub(crate) static REANIMATOR_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::ReanimatorPayoff,
    na_reason: "reanimator_payoff_na",
    inert_reason: "reanimator_payoff_inert",
    activation: reanimator_activation,
    tiers: &[
        PayoffTier {
            matches: matches_reanimation,
            reason: "reanimation_cast_for_payoff",
            bonus: reanimation_cast_bonus,
            scale: None,
        },
        PayoffTier {
            matches: matches_reanimator_enabler,
            reason: "graveyard_enabler_for_reanimation",
            bonus: graveyard_enabler_bonus,
            scale: None,
        },
    ],
};

// ═══ mill ════════════════════════════════════════════════════════════════════

fn mill_activation(features: &DeckFeatures) -> Option<f32> {
    if features.mill.commitment < crate::features::mill::COMMITMENT_FLOOR {
        None
    } else {
        Some(features.mill.commitment)
    }
}

// Re-classify each ability/trigger chain in isolation so two unrelated
// abilities cannot combine into a false-positive opponent-mill detection.
fn matches_opponent_mill(object: &GameObject) -> bool {
    object.abilities.iter().any(|ability| {
        collect_chain_effects(ability)
            .iter()
            .copied()
            .any(crate::features::mill::effect_is_opponent_mill)
    }) || object
        .trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .any(|trigger| {
            trigger.execute.as_deref().is_some_and(|execute| {
                collect_chain_effects(execute)
                    .iter()
                    .copied()
                    .any(crate::features::mill::effect_is_opponent_mill)
            })
        })
}

fn mill_cast_bonus(p: &PolicyPenalties) -> f64 {
    p.mill_cast_bonus
}

// CR 104.3c: scale by how close the lowest-library opponent is to decking.
fn mill_scale(ctx: &PolicyContext<'_>, _object: &GameObject) -> PayoffScale {
    let min_library = players::opponents(ctx.state, ctx.ai_player)
        .iter()
        .map(|&opp_id| ctx.state.players[opp_id.0 as usize].library.len())
        .min()
        .unwrap_or(60);

    let urgency_scale = if min_library < LIBRARY_THRESHOLD_URGENT {
        URGENCY_SCALE_HIGH
    } else if min_library < LIBRARY_THRESHOLD_ELEVATED {
        URGENCY_SCALE_MID
    } else {
        URGENCY_SCALE_NORMAL
    };

    PayoffScale {
        mult: urgency_scale,
        facts: vec![
            ("library_remaining", min_library as i64),
            ("urgency_x10", (urgency_scale * 10.0) as i64),
        ],
    }
}

pub(crate) static MILL_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::MillPayoff,
    na_reason: "mill_payoff_na",
    inert_reason: "mill_payoff_inert",
    activation: mill_activation,
    tiers: &[PayoffTier {
        matches: matches_opponent_mill,
        reason: "mill_cast",
        bonus: mill_cast_bonus,
        scale: Some(mill_scale),
    }],
};

// ═══ energy ══════════════════════════════════════════════════════════════════

fn energy_activation(features: &DeckFeatures) -> Option<f32> {
    if features.energy.commitment < crate::features::energy::COMMITMENT_FLOOR {
        None
    } else {
        Some(features.energy.commitment)
    }
}

fn energy_is_producer(object: &GameObject) -> bool {
    object
        .abilities
        .iter()
        .any(crate::features::energy::chain_includes_energy_gain)
        || object
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| &entry.definition)
            .any(|trigger| {
                trigger
                    .execute
                    .as_deref()
                    .is_some_and(crate::features::energy::chain_includes_energy_gain)
            })
}

fn energy_is_sink(object: &GameObject) -> bool {
    object
        .abilities
        .iter()
        .any(crate::features::energy::ability_tree_pays_energy)
        || object
            .trigger_definitions
            .iter_unchecked()
            .map(|entry| &entry.definition)
            .any(|trigger| {
                trigger
                    .execute
                    .as_deref()
                    .is_some_and(crate::features::energy::ability_tree_pays_energy)
            })
}

fn matches_energy_relevant(object: &GameObject) -> bool {
    energy_is_producer(object) || energy_is_sink(object)
}

fn energy_cast_bonus(p: &PolicyPenalties) -> f64 {
    p.energy_cast_bonus
}

// CR 122.1: banked energy reserve is the engine's momentum.
fn energy_scale(ctx: &PolicyContext<'_>, object: &GameObject) -> PayoffScale {
    let is_producer = energy_is_producer(object);
    let is_sink = energy_is_sink(object);
    let reserve = ctx.state.players[ctx.ai_player.0 as usize].energy as usize;

    let momentum_scale = if reserve >= RESERVE_THRESHOLD_HIGH {
        MOMENTUM_SCALE_HIGH
    } else if reserve >= RESERVE_THRESHOLD_MID {
        MOMENTUM_SCALE_MID
    } else {
        MOMENTUM_SCALE_NORMAL
    };

    PayoffScale {
        mult: momentum_scale,
        facts: vec![
            ("energy_reserve", reserve as i64),
            ("urgency_x10", (momentum_scale * 10.0) as i64),
            ("is_producer", i64::from(is_producer)),
            ("is_sink", i64::from(is_sink)),
        ],
    }
}

pub(crate) static ENERGY_PAYOFF: PayoffSpec = PayoffSpec {
    id: PolicyId::EnergyPayoff,
    na_reason: "energy_payoff_na",
    inert_reason: "energy_payoff_inert",
    activation: energy_activation,
    tiers: &[PayoffTier {
        matches: matches_energy_relevant,
        reason: "energy_cast",
        bonus: energy_cast_bonus,
        scale: Some(energy_scale),
    }],
};

//! Planeswalker loyalty-ability tactical policy.
//!
//! Two observed blunders (Discord #ai-suggestions):
//!   - The AI plays a planeswalker and does NOT use a loyalty ability that turn.
//!   - The AI fires a value-negative minus that sacrifices the planeswalker for
//!     a single-target combat trick (Quintorius: "-4: target creature gains
//!     double strike") instead of the baseline plus that generates its value.
//!
//! Design — blunder-only penalty. The ONLY thing penalized is the specific
//! blunder: sacrificing/crippling the planeswalker (left at <= 1 loyalty) for a
//! single-target combat trick. Every other loyalty activation — plus abilities,
//! removal, emblems/ultimates, tokens, draw, mass effects, steal, reanimation —
//! earns a modest "use your planeswalker" bonus. This deliberately avoids
//! classifying effect *value* (a "low-value vs high-value" split misclassifies
//! the 76 `CreateEmblem` ultimates as low-value and would suppress them); by
//! penalizing only the combat-trick-sacrifice shape, ultimates and removal are
//! structurally never penalized.
//!
//! Effect-value ranking is NOT this policy's job — `EffectTimingPolicy` and
//! `effect_classify` already reward removal/damage. This policy only encourages
//! using the planeswalker and blocks the one clear self-sacrifice blunder.
//!
//! CR 606.1: Some activated abilities are loyalty abilities (an activated
//! ability with a loyalty symbol in its cost), subject to special rules. The
//! engine gates legality (sorcery speed, once per planeswalker per turn); this
//! policy only chooses among the legal candidates the engine offers.

use engine::types::ability::{
    AbilityCost, ContinuousModification, Effect, StaticDefinition, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::effect_classify::{effect_polarity, EffectPolarity};
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Bonus for activating a loyalty ability — "use your planeswalker." Applied to
/// plus abilities and every non-blunder minus. Beats passing.
const USE_BONUS: f64 = 1.5;

/// Penalty for sacrificing/crippling the planeswalker on a single-target combat
/// trick. Net well below `USE_BONUS`, so a plus on the same planeswalker wins.
const SACRIFICE_PENALTY: f64 = 5.0;

/// A minus is a sacrifice blunder when it leaves the planeswalker at or below
/// this loyalty (dead, or one ping from it).
const SACRIFICE_FLOOR: i32 = 1;

pub struct PlaneswalkerLoyaltyPolicy;

impl TacticalPolicy for PlaneswalkerLoyaltyPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::PlaneswalkerLoyalty
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck; the verdict's planeswalker+loyalty guard
        // self-gates (non-loyalty / non-PW activations return _na).
        // activation-constant: planeswalker loyalty-ability choice, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::Score {
            delta: 0.0,
            reason: PolicyReason::new("planeswalker_loyalty_na"),
        };

        let (source_id, ability_index) = match &ctx.candidate.action {
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            } => (*source_id, *ability_index),
            _ => return na(),
        };

        let Some(obj) = ctx.state.objects.get(&source_id) else {
            return na();
        };
        if !obj.card_types.core_types.contains(&CoreType::Planeswalker) {
            return na();
        }
        let Some(ability) = obj.abilities.get(ability_index) else {
            return na();
        };
        // CR 606.1: only loyalty-cost abilities are scored here.
        let amount = match &ability.cost {
            Some(AbilityCost::Loyalty { amount }) => *amount,
            _ => return na(),
        };

        let score = |delta: f64, kind: &'static str| PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new(kind),
        };

        // Plus / zero: always good — build loyalty and get value.
        if amount >= 0 {
            return score(USE_BONUS, "planeswalker_plus_ability");
        }

        // Minus: penalize ONLY the self-sacrifice-for-a-combat-trick blunder.
        let loyalty = obj.loyalty.unwrap_or(0) as i32;
        if is_combat_trick(&ability.effect) && loyalty + amount <= SACRIFICE_FLOOR {
            return score(
                -SACRIFICE_PENALTY,
                "planeswalker_minus_sacrifices_for_trick",
            );
        }
        // All other minuses (removal, emblem/ultimate, tokens, draw, mass,
        // steal, reanimate, or an affordable trick): use your planeswalker.
        score(USE_BONUS, "planeswalker_minus_ability")
    }
}

/// A single-target *beneficial* P/T-or-keyword buff — the marginal "combat
/// trick" class. Both arms exclude negative-pump removal (e.g. "target
/// opponent's creature gets -3/-3", which is `Effect::Pump` with negative P/T)
/// so removal minuses are never treated as tricks.
fn is_combat_trick(effect: &Effect) -> bool {
    match effect {
        // Beneficial ⇒ non-negative P/T (effect_polarity's sign rule), so
        // negative/shrink pumps are excluded.
        Effect::Pump { .. } => matches!(effect_polarity(effect), EffectPolarity::Beneficial),
        // A grant scoped to the single targeted creature (ParentTarget), whose
        // every modification is a beneficial buff. A team anthem has
        // `affected: Typed{controller: You}` and falls through.
        Effect::GenericEffect {
            static_abilities, ..
        } => {
            !static_abilities.is_empty()
                && static_abilities.iter().all(static_is_single_target_buff)
        }
        _ => false,
    }
}

fn static_is_single_target_buff(sd: &StaticDefinition) -> bool {
    matches!(sd.affected, Some(TargetFilter::ParentTarget))
        && !sd.modifications.is_empty()
        && sd.modifications.iter().all(is_beneficial_buff_mod)
}

fn is_beneficial_buff_mod(m: &ContinuousModification) -> bool {
    match m {
        ContinuousModification::AddKeyword { .. } => true,
        ContinuousModification::AddPower { value }
        | ContinuousModification::AddToughness { value } => *value >= 0,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, PtValue, QuantityExpr, TypedFilter,
    };
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::keywords::Keyword;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);

    /// Single-target double-strike grant (Quintorius shape): GenericEffect with
    /// `affected: ParentTarget`, AddKeyword mod.
    fn single_target_double_strike() -> Effect {
        grant_effect(Some(TargetFilter::ParentTarget), Keyword::DoubleStrike)
    }

    fn grant_effect(affected: Option<TargetFilter>, keyword: Keyword) -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition {
                mode: StaticMode::Continuous,
                affected,
                modifications: vec![ContinuousModification::AddKeyword { keyword }],
                condition: None,
                per_player_condition: None,
                affected_zone: None,
                effect_zone: None,
                active_zones: Vec::new(),
                characteristic_defining: false,
                description: None,
                attack_defended: None,
                source_controller: None,
                bypass_beneficiary: None,
            }],
            target: None,
            duration: None,
        }
    }

    /// Battlefield planeswalker controlled by the AI with `loyalty` and a single
    /// activated loyalty ability (`Loyalty{amount}`, effect `effect`) at index 0.
    fn pw_with_loyalty_ability(
        state: &mut GameState,
        loyalty: u32,
        amount: i32,
        effect: Effect,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Walker".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Planeswalker);
        obj.loyalty = Some(loyalty);
        let mut ability = AbilityDefinition::new(AbilityKind::Activated, effect);
        ability.cost = Some(AbilityCost::Loyalty { amount });
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn loyalty_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index: 0,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        PlaneswalkerLoyaltyPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, expect_kind: &str, expect_delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, expect_kind, "reason kind");
                assert_eq!(delta, expect_delta, "delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn plus_ability_rewarded() {
        let mut state = GameState::new_two_player(42);
        let id = pw_with_loyalty_ability(
            &mut state,
            3,
            1,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_plus_ability",
            USE_BONUS,
        );
    }

    #[test]
    fn removal_minus_rewarded() {
        let mut state = GameState::new_two_player(42);
        let id = pw_with_loyalty_ability(
            &mut state,
            5,
            -3,
            Effect::Destroy {
                target: TargetFilter::Any,
                cant_regenerate: false,
            },
        );
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_minus_ability",
            USE_BONUS,
        );
    }

    /// BLOCKER-1 regression: a deep self-sacrificing ultimate (CreateEmblem,
    /// which the effect_profile flag set misses) must NOT be penalized.
    #[test]
    fn emblem_ultimate_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let id = pw_with_loyalty_ability(
            &mut state,
            7,
            -7,
            Effect::CreateEmblem {
                statics: Vec::new(),
                triggers: Vec::new(),
            },
        );
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_minus_ability",
            USE_BONUS,
        );
    }

    /// #4e discriminator: a single-target combat trick that sacrifices the
    /// planeswalker (loyalty 4, -4 → 0) is penalized.
    #[test]
    fn combat_trick_sacrificing_pw_penalized() {
        let mut state = GameState::new_two_player(42);
        let id = pw_with_loyalty_ability(&mut state, 4, -4, single_target_double_strike());
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_minus_sacrifices_for_trick",
            -SACRIFICE_PENALTY,
        );
    }

    /// An affordable combat trick (loyalty 6, -2 → 4, above SACRIFICE_FLOOR) is
    /// NOT a blunder — just use-credit.
    #[test]
    fn combat_trick_pw_survives_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let id = pw_with_loyalty_ability(&mut state, 6, -2, single_target_double_strike());
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_minus_ability",
            USE_BONUS,
        );
    }

    /// A board-wide buff (`affected: Typed{controller: You}`) is not a
    /// single-target trick even when it sacrifices the planeswalker.
    #[test]
    fn team_anthem_minus_not_a_trick() {
        let mut state = GameState::new_two_player(42);
        let anthem = grant_effect(
            Some(TargetFilter::Typed(
                TypedFilter::default().controller(ControllerRef::You),
            )),
            Keyword::Trample,
        );
        let id = pw_with_loyalty_ability(&mut state, 3, -3, anthem);
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_minus_ability",
            USE_BONUS,
        );
    }

    /// BLOCKER-2 regression: a negative-pump removal minus ("-3/-3 to an
    /// opponent's creature") must NOT be flagged as a combat trick.
    #[test]
    fn removal_minus_negative_pump_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let neg_pump = Effect::Pump {
            power: PtValue::Fixed(-3),
            toughness: PtValue::Fixed(-3),
            target: TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent)),
        };
        let id = pw_with_loyalty_ability(&mut state, 3, -3, neg_pump);
        assert_score(
            loyalty_verdict(&state, id),
            "planeswalker_minus_ability",
            USE_BONUS,
        );
    }

    #[test]
    fn non_planeswalker_activation_na() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(2),
            AI,
            "Rock".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        Arc::make_mut(&mut obj.abilities).push(ability);
        assert_score(loyalty_verdict(&state, id), "planeswalker_loyalty_na", 0.0);
    }

    #[test]
    fn non_loyalty_planeswalker_ability_na() {
        let mut state = GameState::new_two_player(42);
        // A planeswalker with a mana-cost (non-loyalty) activated ability.
        let id = pw_with_loyalty_ability(&mut state, 4, 1, Effect::Proliferate);
        // Overwrite the cost with a non-loyalty cost.
        let obj = state.objects.get_mut(&id).unwrap();
        Arc::make_mut(&mut obj.abilities)[0].cost = Some(AbilityCost::Tap);
        assert_score(loyalty_verdict(&state, id), "planeswalker_loyalty_na", 0.0);
    }
}

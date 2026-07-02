//! Tactical X-value policy — prefer non-zero X for X-cost spells and abilities.
//!
//! Issue #710: The AI was casting X-cost spells (Fireball, Banefire, Hydroid
//! Krasis, Comet Storm, etc.) with X = 0 because no policy scored
//! `WaitingFor::ChooseXValue` candidates outside of copy spells. The fallback
//! at `search::fallback_action` and the projection layer both picked the first
//! legal value (typically X = 0), so the AI cast the spell for no effect.
//!
//! This policy generalizes the X choice: for *any* X-cost spell or activated
//! ability whose effect tree references the chosen X (via
//! `QuantityRef::Variable { name: "X" }`), prefer the maximum legal value.
//! Helix Pinnacle's `{X}: Put X tower counters on ~` (PutCounter with X count)
//! is in this class — issue #3877. Candidate generation must still enumerate
//! X=0 (CR 107.4); this policy outscores it rather than removing the action.
//! The engine has already capped `max` to what the player can legally pay
//! (CR 107.1c + CR 601.2f, via `engine::game::casting_costs::max_x_value`),
//! so picking max is always affordable. Copy spells (`CopyValuePolicy`) score
//! 100 + delta to keep their existing target-aware preference and override this
//! generic preference.
//!
//! Build for the class, not the card: the only signal needed is whether the
//! spell's effect references X. Damage-X (Fireball/Banefire), draw-X
//! (Stroke of Genius), token-X (Hangarback Walker on ETB), P/T-X (Hydroid
//! Krasis), and counters-X (Reinforce X, Helix Pinnacle) all share this shape
//! and all want a non-zero X when affordable.

use engine::types::ability::{
    AbilityDefinition, ContinuousModification, Effect, QuantityRef, ResolvedAbility,
    StaticDefinition,
};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::statics::{HandSizeModification, StaticMode};

use crate::features::DeckFeatures;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};

pub struct XValuePolicy;

impl XValuePolicy {
    pub fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        let (max, ability, object_id, candidate_x) =
            match (&ctx.decision.waiting_for, &ctx.candidate.action) {
                (
                    WaitingFor::ChooseXValue {
                        max, pending_cast, ..
                    },
                    GameAction::ChooseX { value },
                ) => (*max, &pending_cast.ability, pending_cast.object_id, *value),
                _ => return 0.0,
            };

        if max == 0 {
            return 0.0;
        }
        if !ability_references_x(ability) && !spell_object_references_x(ctx.state, object_id) {
            return 0.0;
        }

        // Linear ramp: 0 at X=0, ~1.0 at X=max. Keeps the contribution well
        // below CopyValuePolicy's +100 anchor so copy spells still pick their
        // target-aware preference, while non-copy X spells finally get a
        // non-zero candidate elevated above the X=0 baseline.
        candidate_x as f64 / max as f64
    }
}

impl TacticalPolicy for XValuePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::XValue
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ChooseX]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        PolicyVerdict::Score {
            delta: self.score(ctx),
            reason: PolicyReason::new("x_value_score"),
        }
    }
}

/// True when any effect in the resolving ability tree references the chosen X
/// via `QuantityRef::Variable { name: "X" }` (directly or wrapped in
/// `Offset`/`Multiply`/`DivideRounded`/`UpTo`/`Power`/`Sum`/`Difference`).
/// Also walks `repeat_for` so cards whose X drives only an iteration count
/// (Disorder in the Court class) are recognised.
fn ability_references_x(ability: &ResolvedAbility) -> bool {
    if effect_references_x(&ability.effect) {
        return true;
    }
    if let Some(expr) = &ability.repeat_for {
        if expr.contains_x() {
            return true;
        }
    }
    if let Some(sub) = &ability.sub_ability {
        if ability_references_x(sub) {
            return true;
        }
    }
    if let Some(else_branch) = &ability.else_ability {
        if ability_references_x(else_branch) {
            return true;
        }
    }
    false
}

/// True when the spell-object-on-stack's printed triggers or replacement
/// effects reference X. Covers X-cost creature spells whose X is consumed by
/// cast triggers or ETB replacements rather than the resolving spell effect
/// itself — Hydroid Krasis ("when you cast this spell, you gain X life and
/// draw X cards" + "enters as an X/X"), Genesis Hydra ("when you cast … look
/// at the top X cards"), Hooded Hydra / Hangarback Walker (ETB-with-X-counter
/// replacement on the creature itself). Without this, the AI would still pick
/// X=0 for the entire Hydra / X-counter-ETB class because their X reference
/// is structurally outside the resolving spell ability.
fn spell_object_references_x(state: &GameState, object_id: ObjectId) -> bool {
    let Some(obj) = state.objects.get(&object_id) else {
        return false;
    };
    // Spell-cast triggers / dies / etc. on the stack object.
    for trigger in obj.trigger_definitions.iter_unchecked() {
        if let Some(exec) = &trigger.execute {
            if ability_definition_references_x(exec) {
                return true;
            }
        }
    }
    // ETB-X-counter and similar self-replacements stamped on the spell
    // object (consumed when the permanent enters the battlefield).
    for replacement in obj.replacement_definitions.iter_unchecked() {
        if let Some(exec) = &replacement.execute {
            if ability_definition_references_x(exec) {
                return true;
            }
        }
    }
    // The printed abilities themselves may reference X via repeat_for /
    // sub_ability chains (rare but cheap to scan).
    for ability in obj.abilities.iter() {
        if ability_definition_references_x(ability) {
            return true;
        }
    }
    // X-cost creatures can carry their payoff as static definitions instead
    // of spell/trigger/replacement effects, e.g. dynamic P/T or granted
    // ability/static payloads. Scan both live and printed baselines: live
    // definitions may be layer-filtered, while base definitions are the
    // authoritative printed shape for the stack object.
    for static_def in obj.static_definitions.iter_unchecked() {
        if static_definition_references_x(static_def) {
            return true;
        }
    }
    for static_def in obj.base_static_definitions.iter() {
        if static_definition_references_x(static_def) {
            return true;
        }
    }
    false
}

fn ability_definition_references_x(ability: &AbilityDefinition) -> bool {
    if effect_references_x(&ability.effect) {
        return true;
    }
    if let Some(expr) = &ability.repeat_for {
        if expr.contains_x() {
            return true;
        }
    }
    if let Some(sub) = &ability.sub_ability {
        if ability_definition_references_x(sub) {
            return true;
        }
    }
    if let Some(else_branch) = &ability.else_ability {
        if ability_definition_references_x(else_branch) {
            return true;
        }
    }
    false
}

fn static_definition_references_x(static_def: &StaticDefinition) -> bool {
    static_mode_references_x(&static_def.mode)
        || static_def
            .modifications
            .iter()
            .any(continuous_modification_references_x)
}

fn static_mode_references_x(mode: &StaticMode) -> bool {
    match mode {
        StaticMode::MaximumHandSize {
            modification: HandSizeModification::EqualTo(expr),
        } => expr.contains_x(),
        StaticMode::ModifyCost {
            dynamic_count: Some(qty),
            ..
        }
        | StaticMode::ReduceAbilityCost {
            dynamic_count: Some(qty),
            ..
        } => quantity_ref_references_x(qty),
        _ => false,
    }
}

fn continuous_modification_references_x(modification: &ContinuousModification) -> bool {
    match modification {
        ContinuousModification::CopyValues { values, .. } => {
            values.abilities.iter().any(ability_definition_references_x)
                || values
                    .trigger_definitions
                    .iter()
                    .any(trigger_definition_references_x)
                || values
                    .replacement_definitions
                    .iter()
                    .any(replacement_definition_references_x)
                || values
                    .static_definitions
                    .iter()
                    .any(static_definition_references_x)
        }
        ContinuousModification::GrantAbility { definition } => {
            ability_definition_references_x(definition)
        }
        ContinuousModification::GrantTrigger { trigger } => {
            trigger_definition_references_x(trigger)
        }
        ContinuousModification::GrantStaticAbility { definition } => {
            static_definition_references_x(definition)
        }
        ContinuousModification::SetDynamicPower { value }
        | ContinuousModification::SetDynamicToughness { value }
        | ContinuousModification::SetPowerDynamic { value }
        | ContinuousModification::SetToughnessDynamic { value }
        | ContinuousModification::AddDynamicPower { value }
        | ContinuousModification::AddDynamicToughness { value }
        | ContinuousModification::AddDynamicKeyword { value, .. }
        | ContinuousModification::AddCounterOnEnter { count: value, .. } => value.contains_x(),
        ContinuousModification::SetName { .. }
        | ContinuousModification::AddPower { .. }
        | ContinuousModification::AddToughness { .. }
        | ContinuousModification::SetPower { .. }
        | ContinuousModification::SetToughness { .. }
        | ContinuousModification::AddKeyword { .. }
        | ContinuousModification::RemoveKeyword { .. }
        | ContinuousModification::GrantAllActivatedAbilitiesOf { .. }
        | ContinuousModification::GrantAllTriggeredAbilitiesOf { .. }
        | ContinuousModification::RemoveAllAbilities
        | ContinuousModification::AddType { .. }
        | ContinuousModification::RemoveType { .. }
        | ContinuousModification::AddSubtype { .. }
        | ContinuousModification::RemoveSubtype { .. }
        | ContinuousModification::SetCardTypes { .. }
        | ContinuousModification::RemoveAllSubtypes { .. }
        | ContinuousModification::AddAllCreatureTypes
        | ContinuousModification::AddAllBasicLandTypes
        | ContinuousModification::AddAllLandTypes
        | ContinuousModification::AddChosenSubtype { .. }
        | ContinuousModification::AddChosenColor
        | ContinuousModification::RemoveChosenKeyword
        | ContinuousModification::AddChosenKeyword
        | ContinuousModification::SetColor { .. }
        | ContinuousModification::AddColor { .. }
        | ContinuousModification::AddStaticMode { .. }
        | ContinuousModification::SwitchPowerToughness
        | ContinuousModification::AssignDamageFromToughness
        | ContinuousModification::AssignDamageAsThoughUnblocked
        | ContinuousModification::AssignNoCombatDamage
        | ContinuousModification::ChangeController
        | ContinuousModification::SetBasicLandType { .. }
        | ContinuousModification::SetChosenBasicLandType
        | ContinuousModification::RetainPrintedTriggerFromSource { .. }
        | ContinuousModification::RetainPrintedAbilityFromSource { .. }
        | ContinuousModification::AddSupertype { .. }
        | ContinuousModification::RemoveSupertype { .. }
        | ContinuousModification::SetStartingLoyalty { .. }
        | ContinuousModification::RemoveManaCost => false,
    }
}

fn trigger_definition_references_x(trigger: &engine::types::ability::TriggerDefinition) -> bool {
    trigger
        .execute
        .as_ref()
        .is_some_and(|exec| ability_definition_references_x(exec))
}

fn replacement_definition_references_x(
    replacement: &engine::types::ability::ReplacementDefinition,
) -> bool {
    replacement
        .execute
        .as_ref()
        .is_some_and(|exec| ability_definition_references_x(exec))
}

fn quantity_ref_references_x(qty: &QuantityRef) -> bool {
    matches!(qty, QuantityRef::Variable { name } if name == "X")
}

/// Walk every `QuantityExpr` reachable from `effect` and return true if any
/// resolves through `QuantityRef::Variable { name: "X" }`. Delegates the
/// per-expression test to `QuantityExpr::contains_x`, the engine's single
/// authority, so the AI scores X exactly as the engine evaluates it.
fn effect_references_x(effect: &Effect) -> bool {
    match effect {
        Effect::DealDamage { amount, .. }
        | Effect::DamageAll { amount, .. }
        | Effect::DamageEachPlayer { amount, .. }
        | Effect::GainLife { amount, .. }
        | Effect::LoseLife { amount, .. } => amount.contains_x(),
        Effect::Draw { count, .. }
        | Effect::Mill { count, .. }
        | Effect::Discard { count, .. }
        | Effect::Scry { count, .. }
        | Effect::Surveil { count, .. }
        | Effect::Sacrifice { count, .. }
        | Effect::Dig { count, .. }
        | Effect::ExileTop { count, .. }
        | Effect::PutAtLibraryPosition { count, .. }
        | Effect::PutCounter { count, .. }
        | Effect::PutCounterAll { count, .. }
        | Effect::CopyTokenOf { count, .. }
        | Effect::SearchLibrary { count, .. } => count.contains_x(),
        Effect::Token {
            count,
            enter_with_counters,
            ..
        } => count.contains_x() || enter_with_counters.iter().any(|(_, qty)| qty.contains_x()),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::scenario::{GameScenario, P0};
    use engine::types::ability::{
        Effect as AbilityEffect, QuantityExpr, QuantityRef, ResolvedAbility, TargetFilter,
    };
    use engine::types::game_state::{CastPaymentMode, PendingCast};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::{ManaCost, ManaCostShard, ManaType, ManaUnit};
    use engine::types::phase::Phase;
    use engine::types::CounterType;
    use rand::rngs::SmallRng;
    use rand::SeedableRng;

    fn make_state() -> GameState {
        let mut state = GameState::new_two_player(42);
        state.active_player = PlayerId(0);
        state.priority_player = PlayerId(0);
        state
    }

    fn fireball_pending_cast() -> PendingCast {
        PendingCast::new(
            ObjectId(42),
            CardId(42),
            ResolvedAbility::new(
                AbilityEffect::DealDamage {
                    amount: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::Any,
                    damage_source: None,
                },
                Vec::new(),
                ObjectId(42),
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Red],
                generic: 0,
            },
        )
    }

    fn helix_pinnacle_pending_activation() -> PendingCast {
        PendingCast::new(
            ObjectId(77),
            CardId(77),
            ResolvedAbility::new(
                AbilityEffect::PutCounter {
                    counter_type: CounterType::Generic("tower".to_string()),
                    count: QuantityExpr::Ref {
                        qty: QuantityRef::Variable {
                            name: "X".to_string(),
                        },
                    },
                    target: TargetFilter::SelfRef,
                },
                Vec::new(),
                ObjectId(77),
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            },
        )
    }

    fn x_quantity() -> QuantityExpr {
        QuantityExpr::Ref {
            qty: QuantityRef::Variable {
                name: "X".to_string(),
            },
        }
    }

    fn colorless_mana(count: usize) -> Vec<ManaUnit> {
        (0..count)
            .map(|_| ManaUnit {
                color: ManaType::Colorless,
                source_id: ObjectId(0),
                pip_id: engine::types::mana::ManaPipId(0),
                supertype: None,
                source_could_produce_two_or_more_colors: false,
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
            })
            .collect()
    }

    fn static_x_definition() -> StaticDefinition {
        StaticDefinition::continuous().modifications(vec![
            ContinuousModification::SetPowerDynamic {
                value: x_quantity(),
            },
            ContinuousModification::SetToughnessDynamic {
                value: x_quantity(),
            },
        ])
    }

    fn static_x_state() -> GameState {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.with_mana_pool(P0, colorless_mana(4));
        let spell = scenario
            .add_creature_to_hand(P0, "Static X Creature", 0, 0)
            .with_mana_cost(ManaCost::Cost {
                shards: vec![ManaCostShard::X],
                generic: 0,
            })
            .with_static_definition(static_x_definition())
            .id();
        let mut runner = scenario.build();
        let card_id = runner.state().objects[&spell].card_id;
        runner
            .act(GameAction::CastSpell {
                object_id: spell,
                card_id,
                targets: Vec::new(),
                payment_mode: CastPaymentMode::Auto,
            })
            .expect("X-cost creature cast should reach ChooseXValue");
        assert!(
            matches!(runner.state().waiting_for, WaitingFor::ChooseXValue { .. }),
            "cast must stop at ChooseXValue, got {:?}",
            runner.state().waiting_for
        );
        runner.state().clone()
    }

    fn make_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a crate::config::AiConfig,
        ai_context: &'a crate::context::AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(0),
            config,
            context: ai_context,
            cast_facts: None,
        }
    }

    #[test]
    fn helix_pinnacle_put_counter_x_prefers_max_over_zero() {
        let state = make_state();
        let config = crate::config::AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                min: 0,
                max: 3,
                pending_cast: Box::new(helix_pinnacle_pending_activation()),
                convoke_mode: None,
                x_cost_previews: vec![],
            },
            candidates: Vec::new(),
        };

        let cand_zero = CandidateAction {
            action: GameAction::ChooseX { value: 0 },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };
        let cand_three = CandidateAction {
            action: GameAction::ChooseX { value: 3 },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };

        let score_zero = XValuePolicy.score(&make_ctx(
            &state,
            &decision,
            &cand_zero,
            &config,
            &ai_context,
        ));
        let score_three = XValuePolicy.score(&make_ctx(
            &state,
            &decision,
            &cand_three,
            &config,
            &ai_context,
        ));

        assert!(
            score_three > score_zero,
            "Helix Pinnacle X=3 must outscore X=0 (got {score_three} vs {score_zero})"
        );
    }

    #[test]
    fn helix_pinnacle_registry_keeps_x_zero_but_outscores_it() {
        use crate::policies::registry::PolicyRegistry;

        let state = make_state();
        let config = crate::config::AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                min: 0,
                max: 3,
                pending_cast: Box::new(helix_pinnacle_pending_activation()),
                convoke_mode: None,
                x_cost_previews: vec![],
            },
            candidates: Vec::new(),
        };

        let candidates: Vec<CandidateAction> = (0..=3)
            .map(|value| CandidateAction {
                action: GameAction::ChooseX { value },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            })
            .collect();

        let priors = PolicyRegistry::shared().priors(
            &state,
            &decision,
            &candidates,
            PlayerId(0),
            &config,
            &ai_context,
        );

        let prior_zero = priors
            .iter()
            .find(|p| matches!(p.candidate.action, GameAction::ChooseX { value: 0 }))
            .map(|p| p.prior)
            .expect("X=0 candidate must remain legal (issue #3877)");
        let prior_max = priors
            .iter()
            .find(|p| matches!(p.candidate.action, GameAction::ChooseX { value: 3 }))
            .map(|p| p.prior)
            .expect("X=3 candidate present");

        assert!(
            prior_max > prior_zero,
            "Registry priors must elevate max X over X=0 for Helix Pinnacle \
             (got prior_max={prior_max}, prior_zero={prior_zero})"
        );
    }

    #[test]
    fn fireball_choose_x_prefers_max_over_zero() {
        let state = make_state();
        let config = crate::config::AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                min: 0,
                max: 4,
                pending_cast: Box::new(fireball_pending_cast()),
                convoke_mode: None,
                x_cost_previews: vec![],
            },
            candidates: Vec::new(),
        };

        let cand_zero = CandidateAction {
            action: GameAction::ChooseX { value: 0 },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };
        let cand_four = CandidateAction {
            action: GameAction::ChooseX { value: 4 },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };

        let score_zero = XValuePolicy.score(&make_ctx(
            &state,
            &decision,
            &cand_zero,
            &config,
            &ai_context,
        ));
        let score_four = XValuePolicy.score(&make_ctx(
            &state,
            &decision,
            &cand_four,
            &config,
            &ai_context,
        ));

        assert!(
            score_four > score_zero,
            "Fireball X=4 must outscore X=0 (got {score_four} vs {score_zero})"
        );
        assert!(
            score_zero <= 0.0,
            "X=0 must not get a positive bonus from XValuePolicy (got {score_zero})"
        );
    }

    #[test]
    fn registry_priors_elevate_nonzero_x_for_damage_spell() {
        // End-to-end: the full PolicyRegistry (not just XValuePolicy) must
        // produce a higher prior for X > 0 than X = 0 on a damage-X spell.
        // This is the discriminating test: it fails on upstream/main, where no
        // policy scores non-copy X spells and every value gets the uniform
        // floor prior.
        use crate::policies::registry::PolicyRegistry;

        let state = make_state();
        let config = crate::config::AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                min: 0,
                max: 5,
                pending_cast: Box::new(fireball_pending_cast()),
                convoke_mode: None,
                x_cost_previews: vec![],
            },
            candidates: Vec::new(),
        };

        let candidates: Vec<CandidateAction> = (0..=5)
            .map(|value| CandidateAction {
                action: GameAction::ChooseX { value },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            })
            .collect();

        let priors = PolicyRegistry::shared().priors(
            &state,
            &decision,
            &candidates,
            PlayerId(0),
            &config,
            &ai_context,
        );

        let prior_zero = priors
            .iter()
            .find(|p| matches!(p.candidate.action, GameAction::ChooseX { value: 0 }))
            .map(|p| p.prior)
            .expect("X=0 candidate present");
        let prior_max = priors
            .iter()
            .find(|p| matches!(p.candidate.action, GameAction::ChooseX { value: 5 }))
            .map(|p| p.prior)
            .expect("X=5 candidate present");

        assert!(
            prior_max > prior_zero,
            "Registry priors must elevate X=5 over X=0 for Fireball-shape spell \
             (got prior_max={prior_max}, prior_zero={prior_zero})"
        );
    }

    #[test]
    fn registry_priors_elevate_nonzero_x_for_static_definition_spell() {
        use crate::policies::registry::PolicyRegistry;

        let state = static_x_state();
        let config = crate::config::AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);
        let decision = AiDecisionContext {
            waiting_for: state.waiting_for.clone(),
            candidates: Vec::new(),
        };
        let candidates: Vec<CandidateAction> = (0..=4)
            .map(|value| CandidateAction {
                action: GameAction::ChooseX { value },
                metadata: ActionMetadata {
                    actor: Some(PlayerId(0)),
                    tactical_class: TacticalClass::Selection,
                },
            })
            .collect();

        let priors = PolicyRegistry::shared().priors(
            &state,
            &decision,
            &candidates,
            PlayerId(0),
            &config,
            &ai_context,
        );

        let prior_zero = priors
            .iter()
            .find(|p| matches!(p.candidate.action, GameAction::ChooseX { value: 0 }))
            .map(|p| p.prior)
            .expect("X=0 candidate present");
        let prior_max = priors
            .iter()
            .find(|p| matches!(p.candidate.action, GameAction::ChooseX { value: 4 }))
            .map(|p| p.prior)
            .expect("X=4 candidate present");

        assert!(
            prior_max > prior_zero,
            "Registry priors must elevate X=4 over X=0 for static-definition X spell \
             (got prior_max={prior_max}, prior_zero={prior_zero})"
        );
    }

    #[test]
    fn choose_action_picks_nonzero_x_for_static_definition_spell() {
        let state = static_x_state();
        let config = crate::config::AiConfig::default();
        let mut rng = SmallRng::seed_from_u64(710);

        let action = crate::choose_action(&state, PlayerId(0), &config, &mut rng);

        match action {
            Some(GameAction::ChooseX { value }) => {
                assert!(value > 0, "AI must not choose X=0 for an X creature")
            }
            other => panic!("expected ChooseX action, got {other:?}"),
        }
    }

    #[test]
    fn non_x_referencing_effect_does_not_score() {
        // Sanity: a spell whose effect does NOT reference X (only its cost
        // does) should not trigger this policy. Edge case for spells whose
        // effect is fully fixed and X is purely a tax. None ship today but
        // the policy must not over-claim.
        let state = make_state();
        let config = crate::config::AiConfig::default();
        let ai_context = crate::context::AiContext::empty(&config.weights);

        let pending_cast = PendingCast::new(
            ObjectId(99),
            CardId(99),
            ResolvedAbility::new(
                AbilityEffect::Draw {
                    count: QuantityExpr::Fixed { value: 1 },
                    target: TargetFilter::Controller,
                },
                Vec::new(),
                ObjectId(99),
                PlayerId(0),
            ),
            ManaCost::Cost {
                shards: vec![ManaCostShard::X, ManaCostShard::Blue],
                generic: 0,
            },
        );

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::ChooseXValue {
                player: PlayerId(0),
                min: 0,
                max: 3,
                pending_cast: Box::new(pending_cast),
                convoke_mode: None,
                x_cost_previews: vec![],
            },
            candidates: Vec::new(),
        };

        let cand_three = CandidateAction {
            action: GameAction::ChooseX { value: 3 },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Selection,
            },
        };

        let score = XValuePolicy.score(&make_ctx(
            &state,
            &decision,
            &cand_three,
            &config,
            &ai_context,
        ));
        assert_eq!(
            score, 0.0,
            "Effect that doesn't reference X must not contribute (got {score})"
        );
    }

    #[test]
    fn ability_references_x_walks_else_branch() {
        let mut ability = ResolvedAbility::new(
            AbilityEffect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: TargetFilter::Controller,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        );
        ability.else_ability = Some(Box::new(ResolvedAbility::new(
            AbilityEffect::DealDamage {
                amount: QuantityExpr::Ref {
                    qty: QuantityRef::Variable {
                        name: "X".to_string(),
                    },
                },
                target: TargetFilter::Any,
                damage_source: None,
            },
            Vec::new(),
            ObjectId(7),
            PlayerId(0),
        )));

        assert!(
            ability_references_x(&ability),
            "XValuePolicy must see X references in else_ability branches"
        );
    }
}

use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::game::game_object::GameObject;
use engine::types::ability::{AbilityDefinition, Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::cast_facts::{
    cast_facts_for_action, effect_profile_for_action, CastFacts, EffectProfile,
};
use crate::config::{AiConfig, PolicyPenalties};
use crate::eval::{strategic_intent, StrategicIntent};

pub struct PolicyContext<'a> {
    pub state: &'a GameState,
    pub decision: &'a AiDecisionContext,
    pub candidate: &'a CandidateAction,
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
    pub context: &'a crate::context::AiContext,
    pub cast_facts: Option<CastFacts<'a>>,
}

impl<'a> PolicyContext<'a> {
    pub fn strategic_intent(&self) -> StrategicIntent {
        strategic_intent(self.state, self.ai_player)
    }

    pub fn penalties(&self) -> &PolicyPenalties {
        &self.config.policy_penalties
    }

    /// True when the top-level wall-clock deadline has already elapsed.
    /// Policies doing non-essential expensive work (opponent-turn
    /// projections, deep synergy sweeps) should short-circuit via this
    /// rather than threading the raw `Deadline` everywhere.
    pub fn deadline_expired(&self) -> bool {
        self.context.deadline.expired()
    }

    /// True when an uncached multi-turn projection is affordable given the
    /// remaining wall-clock budget. The threshold is
    /// `SearchConfig::projection_min_budget_ms` (tunable per difficulty);
    /// policies that project should gate their work behind this helper so
    /// the tightest-budget path (Medium, 1500ms) doesn't pay the ~1.5s
    /// simulation cost and blow its own budget.
    pub fn can_afford_projection(&self) -> bool {
        if self.context.deadline.expired() {
            return false;
        }
        let floor = self.config.search.projection_min_budget_ms;
        if floor == 0 {
            return true;
        }
        self.context
            .deadline
            .remaining()
            .is_none_or(|r| r.as_millis() >= floor)
    }

    pub fn source_object(&self) -> Option<&'a GameObject> {
        match &self.candidate.action {
            GameAction::CastSpell { card_id, .. } => self
                .state
                .objects
                .values()
                .find(|object| object.card_id == *card_id),
            GameAction::ActivateAbility { source_id, .. } => self.state.objects.get(source_id),
            // During target selection, the source is in the pending cast or trigger.
            GameAction::ChooseTarget { .. } | GameAction::SelectTargets { .. } => {
                match &self.decision.waiting_for {
                    WaitingFor::TargetSelection { pending_cast, .. } => {
                        self.state.objects.get(&pending_cast.object_id)
                    }
                    WaitingFor::MultiTargetSelection {
                        pending_ability, ..
                    } => self.state.objects.get(&pending_ability.source_id),
                    WaitingFor::TriggerTargetSelection { source_id, .. } => {
                        source_id.as_ref().and_then(|id| self.state.objects.get(id))
                    }
                    _ => None,
                }
            }
            _ => None,
        }
    }

    pub fn effects(&self) -> Vec<&'a Effect> {
        // If we're casting/activating, get effects from the source object
        match &self.candidate.action {
            GameAction::CastSpell { .. } => {
                return self
                    .source_object()
                    .into_iter()
                    .flat_map(|object| object.abilities.iter().flat_map(collect_definition_effects))
                    .collect();
            }
            GameAction::ActivateAbility {
                ability_index,
                source_id,
            } => {
                return self
                    .state
                    .objects
                    .get(source_id)
                    .and_then(|object| object.abilities.get(*ability_index))
                    .map(collect_definition_effects)
                    .unwrap_or_default();
            }
            _ => {}
        }

        // During target selection, extract effects from the pending cast/ability/trigger
        match &self.decision.waiting_for {
            WaitingFor::TargetSelection { pending_cast, .. } => {
                collect_ability_effects(&pending_cast.ability)
            }
            WaitingFor::MultiTargetSelection {
                pending_ability, ..
            } => collect_ability_effects(pending_ability),
            WaitingFor::TriggerTargetSelection { .. } => self
                .state
                .pending_trigger
                .as_ref()
                .map(|t| collect_ability_effects(&t.ability))
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }

    pub fn cast_facts(&self) -> Option<CastFacts<'a>> {
        self.cast_facts
            .clone()
            .or_else(|| match &self.candidate.action {
                GameAction::CastSpell { .. } => {
                    cast_facts_for_action(self.state, &self.candidate.action, self.ai_player)
                }
                _ => None,
            })
    }

    /// Effect-level profile for both spells and activated abilities.
    /// For spells, delegates to CastFacts (includes ETB/replacement effects).
    /// For activated abilities, scans the specific ability's effect chain.
    pub fn effect_profile(&self) -> Option<EffectProfile> {
        if let Some(facts) = &self.cast_facts {
            return Some(facts.profile.clone());
        }
        effect_profile_for_action(self.state, &self.candidate.action, self.ai_player)
    }
}

/// Walk a ResolvedAbility's sub_ability chain, collecting all effects.
pub(crate) fn collect_ability_effects(ability: &ResolvedAbility) -> Vec<&Effect> {
    let mut effects = vec![&ability.effect];
    let mut current = &ability.sub_ability;
    while let Some(sub) = current {
        effects.push(&sub.effect);
        current = &sub.sub_ability;
    }
    effects
}

fn collect_definition_effects(ability: &AbilityDefinition) -> Vec<&Effect> {
    let mut effects = vec![&*ability.effect];
    let mut current = &ability.sub_ability;
    while let Some(sub) = current {
        effects.push(&*sub.effect);
        current = &sub.sub_ability;
    }
    effects
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, PtValue, QuantityExpr, TargetFilter,
    };
    use engine::types::game_state::{PendingCast, TargetSelectionSlot};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::zones::Zone;

    #[test]
    fn effects_returns_pending_cast_during_target_selection() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(3),
                toughness: PtValue::Fixed(3),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget {
                target: Some(engine::types::ability::TargetRef::Object(ObjectId(2))),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let effects = ctx.effects();
        assert_eq!(effects.len(), 1);
        assert!(matches!(effects[0], Effect::Pump { .. }));
    }

    #[test]
    fn effects_walks_sub_ability_chain() {
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();

        let sub = ResolvedAbility::new(
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        )
        .sub_ability(sub);

        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let effects = ctx.effects();
        assert_eq!(
            effects.len(),
            2,
            "Should collect both main and sub-ability effects"
        );
        assert!(matches!(effects[0], Effect::Pump { .. }));
        assert!(matches!(effects[1], Effect::Draw { .. }));
    }

    #[test]
    fn cast_spell_effects_walk_sub_ability_chain() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let card_id = CardId(1);
        let mut ability = AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
        );
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        )));
        let spell_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&spell_id).unwrap().abilities = Arc::new(vec![ability]);

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id: spell_id,
                card_id,
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        let effects = ctx.effects();
        assert_eq!(effects.len(), 2);
        assert!(matches!(effects[0], Effect::Pump { .. }));
        assert!(matches!(effects[1], Effect::Draw { .. }));
    }

    #[test]
    fn cast_facts_returns_spell_cast_facts_without_changing_effects() {
        let mut state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let object_id = create_object(
            &mut state,
            CardId(9),
            PlayerId(0),
            "Test Creature".to_string(),
            Zone::Hand,
        );
        let object = state.objects.get_mut(&object_id).unwrap();
        object
            .card_types
            .core_types
            .push(engine::types::card_type::CoreType::Creature);
        Arc::make_mut(&mut object.abilities).push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
                target: engine::types::ability::TargetFilter::Controller,
                up_to: false,
            },
        ));
        object.trigger_definitions.push(
            engine::types::ability::TriggerDefinition::new(
                engine::types::triggers::TriggerMode::ChangesZone,
            )
            .valid_card(TargetFilter::SelfRef)
            .destination(Zone::Battlefield)
            .execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Destroy {
                    target: TargetFilter::Any,
                    cant_regenerate: false,
                },
            )),
        );

        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority {
                player: PlayerId(0),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::CastSpell {
                object_id,
                card_id: CardId(9),
                targets: Vec::new(),
            },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Spell,
            },
        };
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: PlayerId(0),
            config: &config,
            context: &crate::context::AiContext::empty(&config.weights),
            cast_facts: None,
        };

        assert_eq!(ctx.effects().len(), 1);
        let facts = ctx.cast_facts().expect("cast facts");
        assert_eq!(facts.immediate_etb_triggers.len(), 1);
        assert!(facts.has_direct_removal_text());
    }

    fn deadline_test_ctx<'a>(
        state: &'a GameState,
        decision: &'a AiDecisionContext,
        candidate: &'a CandidateAction,
        config: &'a AiConfig,
        context: &'a crate::context::AiContext,
    ) -> PolicyContext<'a> {
        PolicyContext {
            state,
            decision,
            candidate,
            ai_player: PlayerId(0),
            config,
            context,
            cast_facts: None,
        }
    }

    #[test]
    fn deadline_expired_gates_projection() {
        // When the wall-clock deadline is already blown, projection-gated
        // policies must short-circuit — `can_afford_projection` returns false
        // so callers (velocity_score etc.) skip `get_or_project` and don't
        // blow past the user-visible turn-time budget on an uncached sim.
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let mut ai_ctx = crate::context::AiContext::empty(&config.weights);
        ai_ctx.deadline = engine::util::Deadline::after(0);
        std::thread::sleep(std::time::Duration::from_millis(2));

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = deadline_test_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        assert!(ctx.deadline_expired(), "deadline should have expired");
        assert!(
            !ctx.can_afford_projection(),
            "expired deadline must disallow projection"
        );
    }

    #[test]
    fn fresh_deadline_allows_projection() {
        // Mirror of `deadline_expired_gates_projection`: with a healthy
        // remaining budget, `can_afford_projection` must return true so the
        // velocity signal still runs in the common case.
        let state = GameState::new_two_player(42);
        let config = AiConfig::default();
        let mut ai_ctx = crate::context::AiContext::empty(&config.weights);
        // 5s remaining — well above the default 500ms floor.
        ai_ctx.deadline = engine::util::Deadline::after(5_000);

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = deadline_test_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        assert!(!ctx.deadline_expired());
        assert!(ctx.can_afford_projection());
    }

    #[test]
    fn zero_projection_floor_always_allows() {
        // Escape hatch: setting `projection_min_budget_ms = 0` forces the
        // policy to always attempt projection (used by difficulties with
        // ample budget, or by deterministic regression harnesses).
        let state = GameState::new_two_player(42);
        let mut config = AiConfig::default();
        config.search.projection_min_budget_ms = 0;

        let mut ai_ctx = crate::context::AiContext::empty(&config.weights);
        // Tight but not expired — 1ms remaining.
        ai_ctx.deadline = engine::util::Deadline::after(1);

        let ability = ResolvedAbility::new(
            Effect::Pump {
                power: PtValue::Fixed(2),
                toughness: PtValue::Fixed(2),
                target: TargetFilter::Any,
            },
            Vec::new(),
            ObjectId(1),
            PlayerId(0),
        );
        let pending_cast = PendingCast::new(ObjectId(1), CardId(1), ability, ManaCost::zero());
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::TargetSelection {
                player: PlayerId(0),
                pending_cast: Box::new(pending_cast),
                target_slots: vec![TargetSelectionSlot {
                    legal_targets: vec![],
                    optional: false,
                }],
                selection: Default::default(),
            },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ChooseTarget { target: None },
            metadata: ActionMetadata {
                actor: Some(PlayerId(0)),
                tactical_class: TacticalClass::Target,
            },
        };
        let ctx = deadline_test_ctx(&state, &decision, &candidate, &config, &ai_ctx);

        // With floor=0, even 1ms remaining allows projection. Only an
        // already-expired deadline blocks.
        assert!(ctx.can_afford_projection());
    }
}

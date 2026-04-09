use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::game::game_object::GameObject;
use engine::types::ability::{AbilityDefinition, Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::cast_facts::{cast_facts_for_action, CastFacts};
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
            },
        )));
        let spell_id = create_object(
            &mut state,
            card_id,
            PlayerId(0),
            "Test Spell".to_string(),
            Zone::Hand,
        );
        state.objects.get_mut(&spell_id).unwrap().abilities = vec![ability];

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
        object.abilities.push(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Draw {
                count: QuantityExpr::Fixed { value: 1 },
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
        assert!(facts.has_direct_removal_text);
    }
}

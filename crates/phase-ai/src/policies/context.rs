use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::game::game_object::GameObject;
use engine::types::ability::{Effect, ResolvedAbility};
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::player::PlayerId;

use crate::config::AiConfig;
use crate::eval::{strategic_intent, StrategicIntent};

pub struct PolicyContext<'a> {
    pub state: &'a GameState,
    pub decision: &'a AiDecisionContext,
    pub candidate: &'a CandidateAction,
    pub ai_player: PlayerId,
    pub config: &'a AiConfig,
}

impl<'a> PolicyContext<'a> {
    pub fn strategic_intent(&self) -> StrategicIntent {
        strategic_intent(self.state, self.ai_player)
    }

    pub fn source_object(&self) -> Option<&'a GameObject> {
        match &self.candidate.action {
            GameAction::CastSpell { card_id, .. } => self
                .state
                .objects
                .values()
                .find(|object| object.card_id == *card_id),
            GameAction::ActivateAbility { source_id, .. } => self.state.objects.get(source_id),
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
                    .flat_map(|object| object.abilities.iter().map(|ability| &ability.effect))
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
                    .map(|ability| vec![&ability.effect])
                    .unwrap_or_default();
            }
            _ => {}
        }

        // During target selection, extract effects from the pending cast/ability
        match &self.decision.waiting_for {
            WaitingFor::TargetSelection { pending_cast, .. } => {
                collect_ability_effects(&pending_cast.ability)
            }
            WaitingFor::MultiTargetSelection {
                pending_ability, ..
            } => collect_ability_effects(pending_ability),
            _ => Vec::new(),
        }
    }
}

/// Walk a ResolvedAbility's sub_ability chain, collecting all effects.
fn collect_ability_effects(ability: &ResolvedAbility) -> Vec<&Effect> {
    let mut effects = vec![&ability.effect];
    let mut current = &ability.sub_ability;
    while let Some(sub) = current {
        effects.push(&sub.effect);
        current = &sub.sub_ability;
    }
    effects
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, TacticalClass};
    use engine::types::ability::{PtValue, QuantityExpr, TargetFilter};
    use engine::types::game_state::{PendingCast, TargetSelectionSlot};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;

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
        let pending_cast = PendingCast {
            object_id: ObjectId(1),
            card_id: CardId(1),
            ability,
            cost: ManaCost::zero(),
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
        };
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

        let pending_cast = PendingCast {
            object_id: ObjectId(1),
            card_id: CardId(1),
            ability,
            cost: ManaCost::zero(),
            activation_cost: None,
            activation_ability_index: None,
            target_constraints: Vec::new(),
        };
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
}

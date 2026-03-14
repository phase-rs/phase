use engine::ai_support::{AiDecisionContext, CandidateAction};
use engine::game::game_object::GameObject;
use engine::types::ability::Effect;
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

pub struct PolicyContext<'a> {
    pub state: &'a GameState,
    pub decision: &'a AiDecisionContext,
    pub candidate: &'a CandidateAction,
    pub ai_player: PlayerId,
}

impl<'a> PolicyContext<'a> {
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
        match &self.candidate.action {
            GameAction::CastSpell { .. } => self
                .source_object()
                .into_iter()
                .flat_map(|object| object.abilities.iter().map(|ability| &ability.effect))
                .collect(),
            GameAction::ActivateAbility {
                ability_index,
                source_id,
            } => self
                .state
                .objects
                .get(source_id)
                .and_then(|object| object.abilities.get(*ability_index))
                .map(|ability| vec![&ability.effect])
                .unwrap_or_default(),
            _ => Vec::new(),
        }
    }
}

use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::identifiers::ObjectId;

use crate::eval::{evaluate_creature, threat_level};

use super::context::PolicyContext;
use super::registry::TacticalPolicy;

pub struct AntiSelfHarmPolicy;

impl TacticalPolicy for AntiSelfHarmPolicy {
    fn score(&self, ctx: &PolicyContext<'_>) -> f64 {
        match &ctx.candidate.action {
            GameAction::ChooseTarget { target } => target
                .as_ref()
                .map_or(-0.25, |target| score_target_ref(ctx, target)),
            GameAction::SelectTargets { targets } => targets
                .iter()
                .map(|target| score_target_ref(ctx, target))
                .sum(),
            _ => 0.0,
        }
    }
}

fn score_target_ref(ctx: &PolicyContext<'_>, target: &TargetRef) -> f64 {
    match target {
        TargetRef::Player(player_id) => {
            if *player_id == ctx.ai_player {
                -100.0
            } else {
                4.0 + threat_level(ctx.state, ctx.ai_player, *player_id) * 8.0
            }
        }
        TargetRef::Object(object_id) => score_target_object(ctx, *object_id),
    }
}

fn score_target_object(ctx: &PolicyContext<'_>, object_id: ObjectId) -> f64 {
    let Some(object) = ctx.state.objects.get(&object_id) else {
        return -10.0;
    };

    let controller_delta = if object.controller == ctx.ai_player {
        -1.0
    } else {
        1.0
    };
    let mut score = controller_delta * 2.0;

    if object.card_types.core_types.contains(&CoreType::Creature) {
        score += controller_delta * evaluate_creature(ctx.state, object_id);
    }

    score
}

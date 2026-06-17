//! Artifacts-matter tactical policy.
//!
//! For decks committed to the artifacts-matter axis, nudges deploying artifacts
//! (which grow the board affinity/improvise/metalcraft payoffs feed on) and
//! prefers casting affinity-for-artifacts / improvise spells (the cost payoff
//! itself). Opts out below `COMMITMENT_FLOOR`, so non-artifact decks are
//! unaffected.
//!
//! CR 702.41a: Affinity for artifacts — costs {1} less per artifact you control.
//! CR 702.126a: Improvise — tap untapped artifacts to pay generic mana.

use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::artifacts::{is_artifact_cost_payoff_parts, COMMITMENT_FLOOR};
use crate::features::DeckFeatures;

pub struct ArtifactSynergyPolicy;

impl TacticalPolicy for ArtifactSynergyPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::ArtifactSynergyTactical
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
        if features.artifacts.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(features.artifacts.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::CastSpell { object_id, .. } = &ctx.candidate.action else {
            return PolicyVerdict::neutral(PolicyReason::new("artifact_synergy_na"));
        };
        let Some(object) = ctx.state.objects.get(object_id) else {
            return PolicyVerdict::neutral(PolicyReason::new("artifact_synergy_na"));
        };

        // Casting an affinity-for-artifacts or improvise spell is the synergy
        // payoff itself — it gets cheaper / easier the wider the artifact board
        // already is (CR 702.41a / CR 702.126a). Prefer it. Classification is
        // shared with the deck-time feature detector via the single-authority
        // `is_artifact_cost_payoff_parts` predicate so the two never drift.
        if is_artifact_cost_payoff_parts(&object.keywords) {
            return PolicyVerdict::score(
                ctx.penalties().artifact_cost_payoff_bonus,
                PolicyReason::new("artifact_cost_payoff"),
            );
        }

        // Deploying an artifact grows the count the payoffs scale on — a mild
        // nudge to prioritize artifacts over otherwise-equal plays. CR 301.1.
        if object.card_types.core_types.contains(&CoreType::Artifact) {
            return PolicyVerdict::score(
                ctx.penalties().deploy_artifact_bonus,
                PolicyReason::new("deploy_artifact_for_synergy"),
            );
        }

        PolicyVerdict::neutral(PolicyReason::new("artifact_synergy_inert"))
    }
}

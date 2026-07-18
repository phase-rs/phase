//! Land-play sequencing tactical policy.
//!
//! Report (#ai-suggestions "AI Cast & Bounce lands"): the AI plays a Karoo
//! bounce-land (Simic Growth Chamber) first when it should play a different
//! land first, losing tempo (the bounce-land returns one of your lands to hand
//! on ETB). `PlayLand` is declared in `BoardDevelopmentPolicy.decision_kinds()`
//! but its `score()` only handles `CastSpell`/`PassPriority`, so land choice is
//! otherwise unscored. This policy fills that gap for the bounce-land case.
//!
//! Scope (#4a): when the land being played is a self-bouncing Ravnica/MOM
//! bounce-land AND a non-bouncing land is also in hand, deprioritize the
//! bounce-land so the non-bounce land is played first. Deferred (#4b, a
//! separate `SelectTarget` decision): when the bounce-land's ETB returns "a
//! land you control," the AI should return the least-useful land, never the
//! just-played bounce-land — that needs its own target-selection policy.
//!
//! Detection is structural (no card names): an ETB trigger that bounces a land
//! you control. This matches the whole Ravnica/MOM `Effect::Bounce` cycle. The
//! Mercadian "Karoo"/Coral Atoll cycle sacrifices itself (`Effect::Sacrifice`)
//! and has no sequencing downside — it is deliberately NOT matched.

use engine::game::game_object::GameObject;
use engine::types::ability::{ControllerRef, Effect, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::triggers::TriggerMode;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::DeckFeatures;

/// Penalty for playing a self-bouncing land while a non-bouncing land is also
/// in hand. The non-bounce land's `PlayLand` scores `0.0` here, so it wins the
/// argmax and is played first; the bounce-land is only deferred within the turn.
const BOUNCE_DEPRIORITIZE: f64 = 1.5;

pub struct LandSequencingPolicy;

impl TacticalPolicy for LandSequencingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LandSequencing
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::PlayLand]
    }

    fn activation(
        &self,
        _features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        // Applies to every deck; the verdict's bounce-land guard self-gates.
        // activation-constant: land-play sequencing, universal.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let score = |delta: f64, kind: &'static str| PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new(kind),
        };

        let GameAction::PlayLand { object_id, .. } = &ctx.candidate.action else {
            return score(0.0, "land_sequencing_na");
        };
        let object_id = *object_id;

        let Some(played) = ctx.state.objects.get(&object_id) else {
            return score(0.0, "land_sequencing_na");
        };
        if !is_self_bounce_land(played) {
            return score(0.0, "land_sequencing_na");
        }

        // Is there another, non-bouncing land in hand to play first?
        let hand = &ctx.state.players[ctx.ai_player.0 as usize].hand;
        let has_non_bounce_alternative = hand.iter().any(|&id| {
            id != object_id
                && ctx.state.objects.get(&id).is_some_and(|o| {
                    o.card_types.core_types.contains(&CoreType::Land) && !is_self_bounce_land(o)
                })
        });

        if has_non_bounce_alternative {
            score(-BOUNCE_DEPRIORITIZE, "land_sequencing_play_other_first")
        } else {
            // Bounce-land is the only land to play — let it through.
            score(0.0, "land_sequencing_no_alternative")
        }
    }
}

/// True when `obj` has an ETB trigger that returns a land YOU control to hand
/// (the Ravnica/MOM bounce-land / "Karoo" cycle). Structural, not name-matched.
fn is_self_bounce_land(obj: &GameObject) -> bool {
    obj.trigger_definitions
        .iter_unchecked()
        .map(|entry| &entry.definition)
        .any(|t| {
            t.mode == TriggerMode::ChangesZone
                && t.destination == Some(Zone::Battlefield)
                && matches!(t.valid_card, Some(TargetFilter::SelfRef))
                && t.execute
                    .as_deref()
                    .is_some_and(|exec| collect_chain_effects(exec).iter().any(bounces_own_land))
        })
}

fn bounces_own_land(effect: &&Effect) -> bool {
    matches!(effect, Effect::Bounce { target, .. } if target_is_own_land(target))
}

fn target_is_own_land(filter: &TargetFilter) -> bool {
    matches!(
        filter,
        TargetFilter::Typed(t)
            if t.controller == Some(ControllerRef::You)
                && t.type_filters.iter().any(type_filter_is_land)
    )
}

fn type_filter_is_land(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Land => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_land),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, BounceSelection, TargetFilter, TriggerDefinition,
        TypedFilter,
    };
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::zones::Zone;

    use crate::config::AiConfig;
    use crate::context::AiContext;

    const AI: PlayerId = PlayerId(0);

    fn own_land_bounce_effect() -> Effect {
        Effect::Bounce {
            target: TargetFilter::Typed(
                TypedFilter::default()
                    .with_type(TypeFilter::Land)
                    .controller(ControllerRef::You),
            ),
            destination: None,
            selection: BounceSelection::default(),
        }
    }

    /// A bounce-land in hand with the SGC-shape ETB self-bounce trigger.
    fn bounce_land(state: &mut GameState) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Growth Chamber".to_string(),
            Zone::Hand,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    own_land_bounce_effect(),
                )),
        );
        id
    }

    fn plain_land(state: &mut GameState, name: &str) -> ObjectId {
        let id = create_object(state, CardId(2), AI, name.to_string(), Zone::Hand);
        state
            .objects
            .get_mut(&id)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Land);
        id
    }

    fn play_verdict(state: &GameState, object_id: ObjectId) -> PolicyVerdict {
        let candidate = CandidateAction {
            action: GameAction::PlayLand {
                object_id,
                card_id: CardId(0),
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Land,
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
        LandSequencingPolicy.verdict(&ctx)
    }

    fn assert_score(verdict: PolicyVerdict, kind: &str, delta: f64) {
        match verdict {
            PolicyVerdict::Score { delta: d, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(d, delta, "delta");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected reject"),
        }
    }

    #[test]
    fn bounce_land_deprioritized_when_alternative() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [karoo, basic].into_iter().collect();
        assert_score(
            play_verdict(&state, karoo),
            "land_sequencing_play_other_first",
            -BOUNCE_DEPRIORITIZE,
        );
    }

    #[test]
    fn bounce_land_alone_not_penalized() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        state.players[0].hand = [karoo].into_iter().collect();
        assert_score(
            play_verdict(&state, karoo),
            "land_sequencing_no_alternative",
            0.0,
        );
    }

    #[test]
    fn non_bounce_land_na() {
        let mut state = GameState::new_two_player(42);
        let karoo = bounce_land(&mut state);
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [karoo, basic].into_iter().collect();
        assert_score(play_verdict(&state, basic), "land_sequencing_na", 0.0);
    }

    /// A land with a non-bounce ETB (e.g. a scry/tap land) must NOT be detected
    /// as a bounce-land — guards the structural matcher against false positives.
    #[test]
    fn non_karoo_etb_land_na() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(&mut state, CardId(3), AI, "Temple".to_string(), Zone::Hand);
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        obj.trigger_definitions.push(
            TriggerDefinition::new(TriggerMode::ChangesZone)
                .valid_card(TargetFilter::SelfRef)
                .destination(Zone::Battlefield)
                .execute(AbilityDefinition::new(
                    AbilityKind::Spell,
                    Effect::Proliferate,
                )),
        );
        let basic = plain_land(&mut state, "Forest");
        state.players[0].hand = [id, basic].into_iter().collect();
        assert_score(play_verdict(&state, id), "land_sequencing_na", 0.0);
    }
}

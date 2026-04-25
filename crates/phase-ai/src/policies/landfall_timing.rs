//! Landfall timing policy.
//!
//! Scores land-sacrificing non-mana activations (fetchlands) based on whether
//! a landfall payoff is currently on the AI player's battlefield.
//!
//! CR 305.2: Players normally play one land per turn; continuous effects may
//! raise that limit. CR 305.4: Effects that put lands onto the battlefield do
//! not count as land drops. CR 701.21: Sacrifice — the controller moves a
//! permanent they control from the battlefield into its owner's graveyard (the
//! fetchland's payment step). CR 701.23: Search is a keyword action — the
//! fetch's effect searches the library for a land. CR 603: triggered abilities
//! (the payoffs fetches feed) fire when their event occurs on the stack.

use engine::game::game_object::GameObject;
use engine::types::ability::{AbilityDefinition, CostCategory, Effect, TargetFilter, TypeFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::ability_chain::collect_chain_effects;
use crate::features::DeckFeatures;

/// Penalty applied when the AI would crack a fetchland with no payoff in play.
const DELTA_NO_PAYOFF: f64 = -3.0;
/// Bonus applied when the AI has a payoff in play and is cracking a fetch.
const DELTA_WITH_PAYOFF: f64 = 2.5;
/// `activation()` returns `None` for decks with commitment below this floor,
/// so the policy opts out entirely for non-landfall decks.
const COMMITMENT_FLOOR: f32 = 0.1;

pub struct LandfallTimingPolicy;

impl TacticalPolicy for LandfallTimingPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LandfallTiming
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[
            DecisionKind::ActivateAbility,
            DecisionKind::ActivateManaAbility,
            DecisionKind::PlayLand,
        ]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        _state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        if features.landfall.commitment < COMMITMENT_FLOOR {
            None
        } else {
            Some(features.landfall.commitment)
        }
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        // Only fetch-shaped activations are in scope. Everything else is a no-op.
        // CR 701.21 + CR 305.4 + CR 701.23: fetchlands pay a sacrifice cost
        // (CR 701.21), their effect searches the library (CR 701.23), and puts
        // a land onto the battlefield (CR 305.4) — they do not play a land
        // (CR 305.2).
        if !is_fetch_shaped_activation(ctx) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("landfall_timing_na"),
            };
        }

        let features = ctx
            .context
            .session
            .features
            .get(&ctx.ai_player)
            .cloned()
            .unwrap_or_default();

        // CR 603: payoffs are triggered abilities; the relevant state is what
        // is currently on the battlefield under the AI's control.
        let payoffs_on_board =
            count_payoffs_on_board(ctx.state, ctx.ai_player, &features.landfall.payoff_names);

        if payoffs_on_board > 0 {
            PolicyVerdict::Score {
                delta: DELTA_WITH_PAYOFF,
                reason: PolicyReason::new("landfall_payoff_present")
                    .with_fact("payoff_count_on_board", payoffs_on_board as i64),
            }
        } else {
            PolicyVerdict::Score {
                delta: DELTA_NO_PAYOFF,
                reason: PolicyReason::new("landfall_no_payoff_on_board").with_fact(
                    "payoff_count_in_deck",
                    features.landfall.payoff_count as i64,
                ),
            }
        }
    }
}

/// CR 701.21 + CR 305.4 + CR 701.23: a fetch-shaped activation has a
/// sacrifice cost (CR 701.21) and an effect chain that searches the library
/// (CR 701.23) and moves a land onto the battlefield (CR 305.4). Never
/// destructures `AbilityCost::Sacrifice` — consumes the `CostCategory`
/// taxonomy instead (single-authority invariant).
fn is_fetch_shaped_activation(ctx: &PolicyContext<'_>) -> bool {
    let GameAction::ActivateAbility {
        source_id,
        ability_index,
    } = &ctx.candidate.action
    else {
        return false;
    };
    let Some(object) = ctx.state.objects.get(source_id) else {
        return false;
    };
    let Some(ability) = object.abilities.get(*ability_index) else {
        return false;
    };
    if !ability
        .cost_categories()
        .contains(&CostCategory::SacrificesPermanent)
    {
        return false;
    }
    ability_searches_library_for_land(ability) && !ability_is_pure_mana(ability)
}

/// CR 701.23 + CR 305.4: the effect chain searches for a land and puts one
/// onto the battlefield. Searching for a non-land card (tutor) or effects
/// that just shuffle are not fetch-shaped.
fn ability_searches_library_for_land(ability: &AbilityDefinition) -> bool {
    let effects = collect_chain_effects(ability);
    let searches_land = effects.iter().any(|e| {
        matches!(
            e,
            Effect::SearchLibrary { filter, .. } if target_filter_references_land(filter)
        )
    });
    let puts_onto_battlefield = effects.iter().any(|e| {
        matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                ..
            }
        )
    });
    searches_land && puts_onto_battlefield
}

/// Exclude pure mana abilities (e.g., "T, sacrifice this: add {G}"). A fetch
/// has no mana production in its effect chain.
fn ability_is_pure_mana(ability: &AbilityDefinition) -> bool {
    let effects = collect_chain_effects(ability);
    effects.iter().all(|e| matches!(e, Effect::Mana { .. }))
}

fn target_filter_references_land(filter: &TargetFilter) -> bool {
    match filter {
        TargetFilter::Typed(typed) => typed.type_filters.iter().any(type_filter_is_land),
        TargetFilter::Or { filters } | TargetFilter::And { filters } => {
            filters.iter().any(target_filter_references_land)
        }
        _ => false,
    }
}

fn type_filter_is_land(tf: &TypeFilter) -> bool {
    match tf {
        TypeFilter::Land => true,
        TypeFilter::AnyOf(inner) => inner.iter().any(type_filter_is_land),
        _ => false,
    }
}

fn count_payoffs_on_board(state: &GameState, player: PlayerId, payoff_names: &[String]) -> usize {
    if payoff_names.is_empty() {
        return 0;
    }
    state
        .battlefield
        .iter()
        .filter_map(|id| state.objects.get(id))
        .filter(|obj: &&GameObject| obj.controller == player && obj.zone == Zone::Battlefield)
        .filter(|obj| payoff_names.iter().any(|name| name == &obj.name))
        .count()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::features::{DeckFeatures, LandfallFeature};
    use crate::session::AiSession;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, Effect, ManaContribution, ManaProduction,
        QuantityExpr, TargetFilter, TypedFilter,
    };
    use engine::types::game_state::{GameState, WaitingFor};
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::player::PlayerId;
    use engine::types::zones::Zone;
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    fn make_fetch_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                up_to: false,
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice {
                    target: TargetFilter::SelfRef,
                    count: 1,
                },
            ],
        });
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                under_your_control: true,
                enter_tapped: false,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
            },
        )));
        ability
    }

    fn make_mana_ability() -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Fixed {
                    colors: Vec::new(),
                    contribution: ManaContribution::Base,
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
            },
        );
        ability.cost = Some(AbilityCost::Tap);
        ability
    }

    fn context_with_features(features: DeckFeatures) -> (AiContext, AiConfig) {
        let config = AiConfig::default();
        let mut session = AiSession::empty();
        session.features.insert(AI, features);
        let mut context = AiContext::empty(&config.weights);
        context.session = Arc::new(session);
        context.player = AI;
        (context, config)
    }

    fn activate_candidate(source_id: ObjectId, ability_index: usize) -> CandidateAction {
        CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
            metadata: ActionMetadata {
                actor: Some(AI),
                tactical_class: TacticalClass::Ability,
            },
        }
    }

    fn decision() -> AiDecisionContext {
        AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        }
    }

    fn landfall_features(commitment: f32, with_payoff: bool) -> DeckFeatures {
        let mut features = DeckFeatures::default();
        let payoff_names = if with_payoff {
            vec!["Landfall Payoff".to_string()]
        } else {
            Vec::new()
        };
        features.landfall = LandfallFeature {
            payoff_count: 2,
            enabler_count: 2,
            commitment,
            payoff_names,
        };
        features
    }

    fn add_fetch_to_battlefield(state: &mut GameState, name: &str, card_id: CardId) -> ObjectId {
        let id = create_object(state, card_id, AI, name.to_string(), Zone::Battlefield);
        Arc::make_mut(&mut state.objects.get_mut(&id).unwrap().abilities)
            .push(make_fetch_ability());
        id
    }

    #[test]
    fn penalizes_fetch_crack_without_payoff_on_board() {
        let mut state = GameState::new_two_player(42);
        let fetch = add_fetch_to_battlefield(&mut state, "Fetchland", CardId(1));
        let candidate = activate_candidate(fetch, 0);
        let decision = decision();
        let (context, config) = context_with_features(landfall_features(0.9, true));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = LandfallTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "landfall_no_payoff_on_board");
                assert!(delta < 0.0, "expected negative delta, got {delta}");
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn bonuses_fetch_crack_when_payoff_on_board() {
        let mut state = GameState::new_two_player(42);
        let fetch = add_fetch_to_battlefield(&mut state, "Fetchland", CardId(1));
        // Payoff object must be on the AI's battlefield with matching name.
        let _payoff = create_object(
            &mut state,
            CardId(2),
            AI,
            "Landfall Payoff".to_string(),
            Zone::Battlefield,
        );
        let candidate = activate_candidate(fetch, 0);
        let decision = decision();
        let (context, config) = context_with_features(landfall_features(0.9, true));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = LandfallTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "landfall_payoff_present");
                assert!(delta > 0.0, "expected positive delta, got {delta}");
                assert!(reason
                    .facts
                    .iter()
                    .any(|(k, _)| *k == "payoff_count_on_board"));
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn non_fetch_ability_yields_no_op_verdict() {
        let mut state = GameState::new_two_player(42);
        let land = create_object(
            &mut state,
            CardId(1),
            AI,
            "Forest".to_string(),
            Zone::Battlefield,
        );
        Arc::make_mut(&mut state.objects.get_mut(&land).unwrap().abilities)
            .push(make_mana_ability());
        let candidate = activate_candidate(land, 0);
        let decision = decision();
        let (context, config) = context_with_features(landfall_features(0.9, true));
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
        };

        let verdict = LandfallTimingPolicy.verdict(&ctx);
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "landfall_timing_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("unexpected Reject"),
        }
    }

    #[test]
    fn low_commitment_opts_out_via_activation() {
        let features = landfall_features(0.0, false);
        let state = GameState::new_two_player(42);
        assert!(LandfallTimingPolicy
            .activation(&features, &state, AI)
            .is_none());
    }

    #[test]
    fn high_commitment_activation_scales_with_commitment() {
        let features = landfall_features(0.75, true);
        let state = GameState::new_two_player(42);
        let act = LandfallTimingPolicy.activation(&features, &state, AI);
        assert!(matches!(act, Some(v) if (v - 0.75).abs() < 1e-5));
    }
}

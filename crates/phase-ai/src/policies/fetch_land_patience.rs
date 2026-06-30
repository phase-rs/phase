//! Fetch-land patience tactical policy.
//!
//! Report (Discord #ai-suggestions): the AI cracks Evolving Wilds the instant
//! it resolves, with no patience. Evolving Wilds ("{T}, Sacrifice this land:
//! Search your library for a basic land card, put it onto the battlefield
//! tapped, then shuffle.") and its class (Terramorphic Expanse, Terminal
//! Moraine, …) sacrifice themselves to fetch a land that enters **tapped**.
//!
//! Because the fetched land enters tapped (CR 305.4 — it is *put* onto the
//! battlefield, not played), cracking the fetch yields **zero mana this turn**.
//! The only payoff is fixing/thinning for a *later* turn. So there is never a
//! same-turn reason to crack early: the patient line is to hold the fetch until
//! the AI's own end step, by which point it has seen the whole turn and knows
//! which basic it actually wants. Cracking earlier gives up that information for
//! no tempo gain.
//!
//! This policy rejects cracking a tapped self-sacrifice land-fetch outside the
//! AI's own end step, and gives a small nudge to crack it *at* the end step (the
//! source produces no mana on its own, so leaving it uncracked strands a dead
//! land). It deliberately ignores untapped true fetchlands (Wooded Foothills,
//! Flooded Strand): those produce mana the turn they are cracked, so early
//! cracking is correct and must not be gated.

use engine::types::ability::{AbilityCost, Effect, TargetFilter};
use engine::types::actions::GameAction;
use engine::types::game_state::GameState;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::{EtbTapState, Zone};

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::mana_ramp::target_filter_references_land;
use crate::features::DeckFeatures;

/// Small positive nudge to crack the fetch at the AI's own end step. The source
/// produces no mana itself, so converting it to a (tapped) basic is strictly
/// card-neutral and readies a real mana source for next turn — leaving it
/// uncracked strands a dead land. Nudge-band: enough to beat `PassPriority`,
/// never enough to override a genuinely better line.
const END_STEP_CRACK_NUDGE: f64 = 0.3;

pub struct FetchLandPatiencePolicy;

impl TacticalPolicy for FetchLandPatiencePolicy {
    fn id(&self) -> PolicyId {
        PolicyId::FetchLandPatience
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
        // Applies to every deck — Evolving Wilds shows up anywhere. The verdict's
        // classifier self-gates to the tapped fetch-sacrifice land class.
        // activation-constant: classifier-gated fetch-land patience policy.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let na = || PolicyVerdict::neutral(PolicyReason::new("fetch_patience_na"));

        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return na();
        };

        let Some(def) = ctx
            .state
            .objects
            .get(source_id)
            .and_then(|obj| obj.abilities.get(*ability_index))
        else {
            return na();
        };

        // The Evolving Wilds class: a self-sacrifice cost whose effect chain
        // fetches a land that enters the battlefield tapped.
        if !cost_sacrifices_self(def.cost.as_ref())
            || !effects_are_tapped_land_fetch(&ctx.effects())
        {
            return na();
        }

        // CR 513 / CR 514: the AI's own end step is the patient window. The land
        // enters tapped, so cracking now vs. at end step is identical for this
        // turn's mana — but end step preserves information until the last moment.
        let own_end_step =
            ctx.state.phase == Phase::End && ctx.state.active_player == ctx.ai_player;
        if own_end_step {
            return PolicyVerdict::nudge(
                END_STEP_CRACK_NUDGE,
                PolicyReason::new("fetch_patience_end_step"),
            );
        }

        // Any earlier window: hold the fetch. Hard-`Reject` rather than penalise —
        // the end-step nudge above guarantees it still cracks eventually, so the
        // land is never stranded.
        PolicyVerdict::reject(PolicyReason::new("fetch_patience_hold"))
    }
}

/// True if `cost` sacrifices the source permanent itself (CR 701.21) — the
/// signature of a one-shot fetch land. Recurses into composite costs so it
/// matches "{T}, Sacrifice ~" and "{1}, {T}, Sacrifice ~" alike.
fn cost_sacrifices_self(cost: Option<&AbilityCost>) -> bool {
    fn check(c: &AbilityCost) -> bool {
        match c {
            AbilityCost::Sacrifice(sac) => matches!(sac.target, TargetFilter::SelfRef),
            AbilityCost::Composite { costs } => costs.iter().any(check),
            _ => false,
        }
    }
    cost.is_some_and(check)
}

/// True if the effect chain searches the library for a land and puts a land
/// onto the battlefield **tapped** (the Evolving Wilds signature, as opposed to
/// untapped true fetchlands which produce mana the turn they are cracked).
///
/// The two predicates are checked independently across the chain rather than
/// proving the *searched* card is the one entering tapped — the fetch-land class
/// always co-locates them (search land → put that land in tapped → shuffle), and
/// the self-sacrifice gate in `cost_sacrifices_self` already isolates the class.
/// If a future composite ability searches a land *and* separately drops some
/// other tapped permanent, tighten this to the `ChangeZone` whose `target`
/// references land.
fn effects_are_tapped_land_fetch(effects: &[&Effect]) -> bool {
    let searches_land = effects.iter().copied().any(|e| {
        matches!(e, Effect::SearchLibrary { filter, .. } if target_filter_references_land(filter))
    });
    let puts_tapped_land = effects.iter().copied().any(|e| {
        matches!(
            e,
            Effect::ChangeZone {
                destination: Zone::Battlefield,
                enter_tapped: EtbTapState::Tapped,
                ..
            }
        )
    });
    searches_land && puts_tapped_land
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityDefinition, AbilityKind, ControllerRef, QuantityExpr, SacrificeCost, TypedFilter,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use std::sync::Arc;

    const AI: PlayerId = PlayerId(0);

    /// Build an Evolving Wilds-shaped activated ability: `{T}, Sacrifice ~`
    /// searching for a land that enters the battlefield with the given tap state.
    fn fetch_land_ability(enter_tapped: EtbTapState) -> AbilityDefinition {
        let mut ability = AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::SearchLibrary {
                filter: TargetFilter::Typed(TypedFilter::land()),
                count: QuantityExpr::Fixed { value: 1 },
                reveal: false,
                target_player: None,
                selection_constraint: engine::types::ability::SearchSelectionConstraint::None,
                split: None,
                source_zones: vec![Zone::Library],
            },
        );
        ability.cost = Some(AbilityCost::Composite {
            costs: vec![
                AbilityCost::Tap,
                AbilityCost::Sacrifice(SacrificeCost::count(TargetFilter::SelfRef, 1)),
            ],
        });
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::ChangeZone {
                origin: Some(Zone::Library),
                destination: Zone::Battlefield,
                target: TargetFilter::Typed(TypedFilter::land()),
                owner_library: false,
                enter_transformed: false,
                enters_under: Some(ControllerRef::You),
                enter_tapped,
                enters_attacking: false,
                up_to: false,
                enter_with_counters: vec![],
                conditional_enter_with_counters: vec![],
                face_down_profile: None,
            },
        )));
        ability
    }

    fn ai_land_with_ability(state: &mut GameState, ability: AbilityDefinition) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Evolving Wilds".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types
            .core_types
            .push(engine::types::card_type::CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn verdict_for(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
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
        };
        FetchLandPatiencePolicy.verdict(&ctx)
    }

    /// Cracking Evolving Wilds during the AI's own precombat main (the reported
    /// "instant pop") is rejected — the fetched land enters tapped, so there is
    /// no same-turn payoff to justify giving up information now.
    #[test]
    fn evolving_wilds_main_phase_rejected() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Tapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, "fetch_patience_hold"),
            PolicyVerdict::Score { .. } => panic!("expected reject during main phase"),
        }
    }

    /// At the AI's own end step the fetch is nudged to crack — leaving the
    /// no-mana source uncracked strands a dead land.
    #[test]
    fn evolving_wilds_own_end_step_nudged() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::End;
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Tapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_end_step");
                assert!(delta > 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("expected nudge at end step"),
        }
    }

    /// An untapped true fetchland (Wooded Foothills shape) is NOT gated — it
    /// produces mana the turn it is cracked, so early cracking is correct.
    #[test]
    fn untapped_fetchland_unaffected() {
        let mut state = GameState::new_two_player(42);
        state.active_player = AI;
        state.phase = Phase::PreCombatMain;
        let id = ai_land_with_ability(&mut state, fetch_land_ability(EtbTapState::Untapped));
        match verdict_for(&state, id) {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, "fetch_patience_na");
                assert_eq!(delta, 0.0);
            }
            PolicyVerdict::Reject { .. } => panic!("untapped fetch must not be gated"),
        }
    }
}

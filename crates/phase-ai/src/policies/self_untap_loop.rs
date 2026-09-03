//! Self-funded untap-loop veto (Basalt Monolith / Grim Monolith class).
//!
//! Report (Discord threads 1542264507844395078, 1543494524654059560,
//! 1544378844516065441 — "interaction spam"): the AI activates Basalt
//! Monolith's `{3}: Untap Basalt Monolith` on an *already untapped* Monolith,
//! four or five priority passes in a row. Grim Monolith is the same shape at
//! `{4}`.
//!
//! Mechanism. The card parses as two activated abilities: `{T}: Add {C}{C}{C}`
//! (a mana ability) and `{3}: Untap this` (a normal activated ability). Only
//! the second is ever a priority candidate — CR 605.3b, a mana ability doesn't
//! use the stack, so the tap half is not emitted as an `ActivateAbility`
//! decision. When the source is untapped at announcement the untap can add
//! nothing: it is already untapped. Payment then comes from the engine's
//! auto-tap, which may legally tap the source itself for its own untap
//! (CR 605.3a — a mana ability may be activated while paying a cost). Tap for
//! {C}{C}{C}, spend {3}, untap: the board is byte-identical and the mana pool
//! empties (CR 106.4). Net zero, repeated every pass.
//!
//! Why a `Reject` and not a graded penalty. `redundancy_avoidance`'s
//! `untap_redundancy` arm already scores this shape −3.0 and the loop persists:
//! under the softmax a graduated penalty is a *rate*, and only `Reject` (mapped
//! to `NEG_INFINITY` by `PolicyRegistry::score`) is a bound. Both fire on the
//! untapped shape; `Reject` dominates. That policy's arms model "this effect is
//! a no-op on its targets" and return summed deltas — the right home for a
//! categorical veto is a policy of its own.
//!
//! The sole veto is "source untapped at announcement". A *tapped* in-class
//! source is explicitly NOT vetoed — paying idle land mana at an opponent's end
//! step (mana that CR 106.4 would empty anyway) to have the rock available next
//! turn is the correct line for a rock that does not untap normally. Nor is
//! there any need to ask whether a tapped source could only fund its own untap:
//! the engine already answers that. The only battlefield `ActivateAbility`
//! emission path (`candidates.rs` → `can_activate_ability_now_with_restriction_gates`
//! → `can_pay_ability_cost_now`) requires the cost to be payable, and a tapped
//! source cannot tap itself for mana — so any tapped in-class candidate is
//! payable from elsewhere by construction. An affordability re-check here would
//! be a tautology costing one state clone per candidate; do not re-add one.
//!
//! Because the veto is categorical, the stand-downs are load-bearing. They are
//! written against payoff *shapes*, not a card list: an activation-cost
//! reduction static (the untap stops being net-zero), a tap / untap /
//! activation trigger on our own board (the loop becomes a payoff engine —
//! Forsaken Monument, Kinnan, Rings of Brighthearth), and a source whose
//! untapping restores a real use (a creature, or another non-mana ability with
//! a tap cost). Known gap: an own-board mana-doubling `ProduceMana` replacement
//! (Nyxbloom Ancient, Mana Reflection) also makes the loop net-positive and is
//! not yet a stand-down; the tapped-state continuation of that line stays
//! neutral, so only starting it from untapped is foreclosed.
//!
//! Perf: untap-self activations are rare candidates. Predicates run card-local
//! first (root-effect match, then mana-source and cost checks on the candidate's
//! own object), then the O(1) static-presence index, and only then a single
//! battlefield trigger walk — reached only once the class is confirmed. No
//! affordability sweep, no `find_legal_targets`, no state clone.

use engine::game::functioning_abilities::{battlefield_active_triggers, static_kind_present};
use engine::game::game_object;
use engine::game::mana_abilities::is_mana_ability;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, CostCategory, Effect, EffectScope, TapStateChange, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::player::PlayerId;
use engine::types::statics::StaticModeKind;
use engine::types::triggers::TriggerMode;

use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

pub struct SelfUntapLoopPolicy;

impl TacticalPolicy for SelfUntapLoopPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::SelfUntapLoop
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
        // Every deck can run a self-untapping mana rock; `verdict` short-circuits
        // on a card-local root-effect match before doing any work.
        // activation-constant: untap-self loop check, deck-independent.
        Some(1.0)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let neutral = |kind: &'static str| PolicyVerdict::neutral(PolicyReason::new(kind));

        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return neutral("self_untap_loop_na");
        };

        let Some(source) = ctx.state.objects.get(source_id) else {
            return neutral("self_untap_loop_na");
        };
        let Some(def) = source.abilities.get(*ability_index) else {
            return neutral("self_untap_loop_na");
        };

        // 1. Card-local root-effect match, before anything that allocates.
        if !untaps_only_itself(def) {
            return neutral("self_untap_loop_na");
        }

        // 2. The class is "a mana source that untaps itself".
        if !source.abilities.iter().any(is_mana_ability) {
            return neutral("self_untap_loop_na");
        }

        // 3. A plain mana cost with real mana value — Basalt/Grim's shape. A free
        //    or non-mana-priced untap is a different class and is left alone.
        let Some(AbilityCost::Mana { cost }) = def.cost.as_ref() else {
            return neutral("self_untap_loop_na");
        };
        if cost.mana_value() == 0 {
            return neutral("self_untap_loop_na");
        }

        // 4a. An activation-cost reduction static in play (Power Artifact,
        //     Zirda, Training Grounds) makes the untap cheaper than the mana the
        //     source produces, so the loop is no longer net-zero. The presence
        //     index is a superset test — it says a `ReduceAbilityCost` static
        //     exists somewhere, not that it applies here — which is exactly the
        //     safe direction for a stand-down: at worst we decline to veto. It
        //     covers battlefield/command-zone `static_definitions` only, so a
        //     transient granted reduction is missed; accepted.
        if static_kind_present(ctx.state, StaticModeKind::ReduceAbilityCost) {
            return neutral("self_untap_loop_cost_reducer_present");
        }

        // 4b. Untapping restores something other than the mana ability.
        if untap_has_use(source, *ability_index) {
            return neutral("self_untap_loop_untap_has_use");
        }

        // 4c. A tap/untap/activation payoff on our own board turns the loop into
        //     an engine. The only board walk, and only once 1–4b hold.
        if has_tap_payoff_trigger(ctx.state, ctx.ai_player) {
            return neutral("self_untap_loop_trigger_payoff");
        }

        // 5. CR 106.4: the mana spent untapping an already-untapped source buys
        //    nothing and empties at end of step. Categorical veto.
        if !source.tapped {
            return PolicyVerdict::reject(PolicyReason::new("self_untap_loop_untapped"));
        }

        neutral("self_untap_loop_tapped")
    }
}

/// True iff `def`'s entire payoff is "untap this permanent" — a single-scope
/// `SetTapState`/`Untap` on `SelfRef`, with no chained or alternative branch.
///
/// Both `sub_ability` and `else_ability` are checked directly rather than via a
/// chain walk: an untap that is merely the *first* link of a larger ability
/// (or the "if" half of an if/otherwise) is not the net-zero class.
fn untaps_only_itself(def: &AbilityDefinition) -> bool {
    def.sub_ability.is_none()
        && def.else_ability.is_none()
        && matches!(
            &*def.effect,
            Effect::SetTapState {
                target: TargetFilter::SelfRef,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            }
        )
}

/// True iff untapping `source` restores a use other than its mana ability: the
/// source is a creature (untapping has combat value), or it carries another
/// activated ability that taps itself and is not a mana ability.
fn untap_has_use(source: &game_object::GameObject, untap_index: usize) -> bool {
    if source.card_types.core_types.contains(&CoreType::Creature) {
        return true;
    }
    source.abilities.iter().enumerate().any(|(index, ability)| {
        index != untap_index
            && ability.cost_categories().contains(&CostCategory::TapsSelf)
            && !is_mana_ability(ability)
    })
}

/// True iff the AI controls a battlefield permanent whose trigger pays off
/// tapping, untapping, or activating — Forsaken Monument and Kinnan
/// (`TapsForMana`), Rings of Brighthearth (`AbilityActivated`), and the
/// tap/untap watchers. Any of these makes the loop a real engine.
fn has_tap_payoff_trigger(state: &GameState, ai_player: PlayerId) -> bool {
    battlefield_active_triggers(state).any(|(obj, active)| {
        obj.controller == ai_player
            && matches!(
                active.definition.mode,
                TriggerMode::Taps
                    | TriggerMode::TapsForMana
                    | TriggerMode::TapAll
                    | TriggerMode::Untaps
                    | TriggerMode::UntapAll
                    | TriggerMode::AbilityActivated
            )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::layers::evaluate_layers;
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityKind, ManaProduction, QuantityExpr, TypeFilter, TypedFilter,
    };
    use engine::types::game_state::WaitingFor;
    use engine::types::identifiers::{CardId, ObjectId};
    use engine::types::mana::ManaCost;
    use engine::types::statics::{ActivationExemption, CostModifyMode, StaticMode};
    use engine::types::zones::Zone;
    use engine::types::{StaticDefinition, TriggerDefinition};
    use std::sync::Arc;

    use crate::config::AiConfig;
    use crate::context::AiContext;
    use crate::policies::registry::PolicyRegistry;

    const AI: PlayerId = PlayerId(0);
    const UNTAP_INDEX: usize = 1;

    /// `{T}: Add {C}{C}{C}` — Basalt Monolith's mana half.
    fn colorless_mana_ability() -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::Mana {
                produced: ManaProduction::Colorless {
                    count: QuantityExpr::Fixed { value: 3 },
                },
                restrictions: Vec::new(),
                grants: Vec::new(),
                expiry: None,
                target: None,
            },
        )
        .cost(AbilityCost::Tap)
    }

    /// `{3}: Untap <target>` — Basalt Monolith's untap half.
    fn untap_ability(target: TargetFilter) -> AbilityDefinition {
        AbilityDefinition::new(
            AbilityKind::Activated,
            Effect::SetTapState {
                target,
                scope: EffectScope::Single,
                state: TapStateChange::Untap,
            },
        )
        .cost(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: Vec::new(),
                generic: 3,
            },
        })
    }

    /// Basalt Monolith: an artifact with the mana ability at index 0 and the
    /// untap-self ability at `UNTAP_INDEX`.
    fn monolith(state: &mut GameState, tapped: bool) -> ObjectId {
        let id = create_object(
            state,
            CardId(1),
            AI,
            "Basalt Monolith".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        obj.tapped = tapped;
        let abilities = Arc::make_mut(&mut obj.abilities);
        abilities.push(colorless_mana_ability());
        abilities.push(untap_ability(TargetFilter::SelfRef));
        id
    }

    /// A battlefield permanent controlled by `controller` carrying one trigger
    /// with `mode`. The controller is fixed at creation, not patched afterwards —
    /// `evaluate_layers` re-derives `controller` from the object's base state.
    fn permanent_with_trigger(
        state: &mut GameState,
        controller: PlayerId,
        mode: TriggerMode,
    ) -> ObjectId {
        let id = create_object(
            state,
            CardId(2),
            controller,
            "Payoff".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Enchantment);
        obj.trigger_definitions
            .push(TriggerDefinition::new(mode).execute(AbilityDefinition::new(
                AbilityKind::Spell,
                Effect::Proliferate,
            )));
        id
    }

    /// Runs the layer pass — `GameState::new` seeds `static_mode_presence` to
    /// "all present", so without this every `static_kind_present` query is true
    /// and the 4a stand-down would fire on a bare Monolith (mirrors
    /// `policies/tests/vehicle_deployment.rs::cant_tap_creature`).
    fn activate_verdict(
        state: &mut GameState,
        source_id: ObjectId,
        ability_index: usize,
    ) -> PolicyVerdict {
        evaluate_layers(state);
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
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
        SelfUntapLoopPolicy.verdict(&ctx)
    }

    fn assert_neutral(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Score { delta, reason } => {
                assert_eq!(reason.kind, kind, "reason kind");
                assert_eq!(delta, 0.0, "delta");
            }
            PolicyVerdict::Reject { reason } => panic!("unexpected reject: {}", reason.kind),
        }
    }

    fn assert_rejected(verdict: PolicyVerdict, kind: &str) {
        match verdict {
            PolicyVerdict::Reject { reason } => assert_eq!(reason.kind, kind, "reason kind"),
            PolicyVerdict::Score { reason, .. } => panic!("expected reject, got {}", reason.kind),
        }
    }

    /// The reported bug: untapping an already-untapped Monolith.
    #[test]
    fn untapped_monolith_untap_is_rejected() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        assert_rejected(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_untapped",
        );
    }

    /// A tapped Monolith is never vetoed — even with no other mana source on the
    /// board. Affordability is the engine's job: `can_pay_ability_cost_now`
    /// already gates candidate emission, and a tapped source cannot tap itself
    /// for mana, so a tapped candidate is payable from elsewhere by construction.
    #[test]
    fn tapped_monolith_is_never_rejected() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, true);
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_tapped",
        );
    }

    /// Power Artifact / Zirda shape: an activation-cost reduction in play makes
    /// the untap cheaper than the mana produced, so the loop is not net-zero.
    #[test]
    fn cost_reducer_static_stands_down() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        let reducer = create_object(
            &mut state,
            CardId(3),
            AI,
            "Zirda, the Dawnwaker".to_string(),
            Zone::Battlefield,
        );
        {
            let obj = state.objects.get_mut(&reducer).unwrap();
            obj.card_types.core_types.push(CoreType::Creature);
            let def = StaticDefinition::new(StaticMode::ReduceAbilityCost {
                mode: CostModifyMode::Reduce,
                keyword: "activated".to_string(),
                amount: 2,
                minimum_mana: None,
                dynamic_count: None,
                exemption: ActivationExemption::ManaAbilities,
                activator: None,
            })
            .affected(TargetFilter::SelfRef);
            // Both lists, then the layer pass — `static_definitions` is rebuilt
            // from `base_static_definitions` by `evaluate_layers`.
            obj.static_definitions.push(def.clone());
            Arc::make_mut(&mut obj.base_static_definitions).push(def);
        }
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_cost_reducer_present",
        );
    }

    /// Forsaken Monument / Kinnan shape.
    #[test]
    fn taps_for_mana_trigger_stands_down() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        permanent_with_trigger(&mut state, AI, TriggerMode::TapsForMana);
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_trigger_payoff",
        );
    }

    /// Rings of Brighthearth shape.
    #[test]
    fn ability_activated_trigger_stands_down() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        permanent_with_trigger(&mut state, AI, TriggerMode::AbilityActivated);
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_trigger_payoff",
        );
    }

    /// An opponent's tap payoff is not ours — the stand-down must not fire.
    #[test]
    fn opponent_trigger_does_not_stand_down() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        permanent_with_trigger(&mut state, PlayerId(1), TriggerMode::TapsForMana);
        assert_rejected(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_untapped",
        );
    }

    /// A creature that untaps itself has combat value in the untap.
    #[test]
    fn creature_source_stands_down() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        state
            .objects
            .get_mut(&source)
            .unwrap()
            .card_types
            .core_types
            .push(CoreType::Creature);
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_untap_has_use",
        );
    }

    /// A second, non-mana tap ability means untapping restores a real use.
    #[test]
    fn non_mana_tap_ability_stands_down() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        let obj = state.objects.get_mut(&source).unwrap();
        Arc::make_mut(&mut obj.abilities).push(
            AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate)
                .cost(AbilityCost::Tap),
        );
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_untap_has_use",
        );
    }

    /// A permanent that untaps itself but produces no mana is out of class.
    #[test]
    fn non_mana_source_untap_not_in_class() {
        let mut state = GameState::new_two_player(42);
        let id = create_object(
            &mut state,
            CardId(4),
            AI,
            "Untapper".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Artifact);
        Arc::make_mut(&mut obj.abilities).push(untap_ability(TargetFilter::SelfRef));
        assert_neutral(activate_verdict(&mut state, id, 0), "self_untap_loop_na");
    }

    /// Untapping *another* artifact (Voltaic Key shape) is out of class.
    #[test]
    fn untap_other_permanent_not_in_class() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        let obj = state.objects.get_mut(&source).unwrap();
        Arc::make_mut(&mut obj.abilities)[UNTAP_INDEX] = untap_ability(TargetFilter::Typed(
            TypedFilter::default().with_type(TypeFilter::Artifact),
        ));
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_na",
        );
    }

    /// An untap that is only the first link of a larger ability is out of class.
    #[test]
    fn untap_with_sub_ability_not_in_class() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        let obj = state.objects.get_mut(&source).unwrap();
        Arc::make_mut(&mut obj.abilities)[UNTAP_INDEX].sub_ability = Some(Box::new(
            AbilityDefinition::new(AbilityKind::Activated, Effect::Proliferate),
        ));
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_na",
        );
    }

    /// A free untap is a different class — nothing is wasted.
    #[test]
    fn free_untap_not_in_class() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        let obj = state.objects.get_mut(&source).unwrap();
        Arc::make_mut(&mut obj.abilities)[UNTAP_INDEX].cost = None;
        assert_neutral(
            activate_verdict(&mut state, source, UNTAP_INDEX),
            "self_untap_loop_na",
        );
    }

    /// The mana half is never a priority candidate (CR 605.3b), but if it is
    /// scored it must not be mistaken for the untap.
    #[test]
    fn mana_ability_index_not_in_class() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        assert_neutral(
            activate_verdict(&mut state, source, 0),
            "self_untap_loop_na",
        );
    }

    #[test]
    fn non_activate_decision_na() {
        let mut state = GameState::new_two_player(42);
        let source = monolith(&mut state, false);
        evaluate_layers(&mut state);
        let candidate = CandidateAction {
            action: GameAction::PassPriority,
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Pass),
        };
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let config = AiConfig::default();
        let context = AiContext::empty(&config.weights);
        let ctx = PolicyContext {
            state: &state,
            decision: &decision,
            candidate: &candidate,
            ai_player: AI,
            config: &config,
            context: &context,
            cast_facts: None,
            search_depth: crate::policies::context::SearchDepth::Root,
        };
        let _ = source;
        assert_neutral(SelfUntapLoopPolicy.verdict(&ctx), "self_untap_loop_na");
    }

    #[test]
    fn registry_registers_self_untap_loop() {
        assert!(PolicyRegistry::default().has_policy(PolicyId::SelfUntapLoop));
    }
}

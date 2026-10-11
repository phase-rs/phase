//! Land Animation Timing Policy
//!
//! Evaluates when to animate man-lands like Lumbering Falls.

use engine::game::game_object;
use engine::types::ability::{Effect, ManaProduction};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;

use super::activation::turn_only;
use super::context::PolicyContext;
use super::registry::{DecisionKind, PolicyId, PolicyReason, PolicyVerdict, TacticalPolicy};
use crate::features::DeckFeatures;

/// Penalty applied by the `is_only_source_of_color` arm.
const MANA_NEEDED_PENALTY: f64 = -2.0;

/// Penalty for animating a land that ends up tapped (no combat value). Sits at
/// the bottom of the critical band (`-CRITICAL_MAX`) — the strongest finite
/// discouragement the score contract allows. Issue #5473: this was a raw -100.0
/// sentinel that bypassed the band helpers and tripped the registry's
/// critical-band assert once scaled by `activation` (turn_only, up to 1.3x).
///
/// Note: pinned at the critical ceiling, this branch is inert to `activation()`
/// tuning — `-CRITICAL_MAX × any activation` re-bands back to `-CRITICAL_MAX`.
const TAPPED_LAND_PENALTY: f64 = -super::registry::CRITICAL_MAX;

/// Bonus for animating when sufficient alternative mana sources exist.
const SUFFICIENT_MANA_BONUS: f64 = 0.3;

pub struct LandAnimationPolicy;

impl TacticalPolicy for LandAnimationPolicy {
    fn id(&self) -> PolicyId {
        PolicyId::LandAnimation
    }

    fn decision_kinds(&self) -> &'static [DecisionKind] {
        &[DecisionKind::ActivateAbility]
    }

    fn activation(
        &self,
        features: &DeckFeatures,
        state: &GameState,
        _player: PlayerId,
    ) -> Option<f32> {
        turn_only(features, state)
    }

    fn verdict(&self, ctx: &PolicyContext<'_>) -> PolicyVerdict {
        let GameAction::ActivateAbility {
            source_id,
            ability_index,
        } = &ctx.candidate.action
        else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_na"),
            };
        };

        // Get the ability definition
        let Some(obj) = ctx.state.objects.get(source_id) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_na"),
            };
        };

        // Check if this is a land
        if !obj.card_types.core_types.contains(&CoreType::Land) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_not_land"),
            };
        }

        let Some(ability_def) = obj.abilities.get(*ability_index) else {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_na"),
            };
        };

        if !crate::manland::animates_source(ability_def) {
            return PolicyVerdict::Score {
                delta: 0.0,
                reason: PolicyReason::new("land_animation_not_animation"),
            };
        }

        // Categorical veto rather than a penalty: a finite delta is a rate and
        // only `Reject` bounds repetition (registry.rs::PolicyRegistry::score
        // maps it to NEG_INFINITY and, unlike `Score`, it is not scaled by
        // `activation`).
        //
        // This must precede the tapped check: `land_animation_tapped` returns
        // first for a tapped source, and an already-animated man-land can be
        // tapped, which would make this veto unreachable on those boards.
        if crate::manland::animation_payoff_in_force(ctx.state, *source_id)
            && crate::manland::repeat_only_resets_characteristics(ability_def)
            && !crate::manland::repeat_changes_source(
                ctx.state,
                ctx.ai_player,
                *source_id,
                ability_def,
            )
        {
            return PolicyVerdict::reject(PolicyReason::new(
                "land_animation_redundant_already_creature",
            ));
        }

        let mut delta = 0.0;

        // CR 508.1a / CR 509.1a: an animated land that ends up tapped can
        // neither attack nor block this turn, so animating it has no combat
        // value. This is the Shambling Vent failure mode — the AI taps the
        // manland for mana to help pay its own {1}{W}{B} animation cost and
        // turns it into a useless tapped creature. Strongly disprefer any
        // activation that leaves the source tapped.
        if crate::manland::activation_leaves_source_tapped(
            ctx.state,
            ctx.ai_player,
            *source_id,
            *ability_index,
            ability_def,
        ) {
            // Route the critical penalty through the band helper (CR-equivalent
            // score contract) rather than a raw Score literal so the delta stays
            // clamped to the critical band before `activation` scaling.
            return PolicyVerdict::critical(
                TAPPED_LAND_PENALTY,
                PolicyReason::new("land_animation_tapped"),
            );
        }

        // Check if this is the only source of a critical color
        let is_critical_color_source = is_only_source_of_color(ctx, *source_id);
        if is_critical_color_source {
            delta += MANA_NEEDED_PENALTY;
        }

        // Bonus if sufficient alternative mana sources exist
        let sufficient_mana = has_sufficient_mana_sources(ctx, *source_id);
        if sufficient_mana {
            delta += SUFFICIENT_MANA_BONUS;
        }

        PolicyVerdict::Score {
            delta,
            reason: PolicyReason::new("land_animation_score"),
        }
    }
}

/// Check if this land is the only source of a critical color for the AI.
fn is_only_source_of_color(ctx: &PolicyContext<'_>, land_id: ObjectId) -> bool {
    let Some(land) = ctx.state.objects.get(&land_id) else {
        return false;
    };

    // Get colors this land can produce
    let land_colors = colors_produced_by_land(land);

    // For each color, check if this is the only source
    for color in land_colors {
        let other_sources = ctx
            .state
            .battlefield
            .iter()
            .filter(|&&id| {
                id != land_id && {
                    let Some(obj) = ctx.state.objects.get(&id) else {
                        return false;
                    };
                    obj.controller == ctx.ai_player
                        && obj.card_types.core_types.contains(&CoreType::Land)
                        && !obj.tapped
                        && colors_produced_by_land(obj).contains(&color)
                }
            })
            .count();

        if other_sources == 0 {
            return true;
        }
    }

    false
}

/// Get the colors a land can produce.
fn colors_produced_by_land(land: &game_object::GameObject) -> Vec<engine::types::mana::ManaColor> {
    let mut colors = Vec::new();
    for ability in land.abilities.iter() {
        if let Effect::Mana { produced, .. } = &*ability.effect {
            match produced {
                ManaProduction::Fixed {
                    colors: produced_colors,
                    ..
                } => {
                    colors.extend(produced_colors.clone());
                }
                ManaProduction::Mixed {
                    colors: produced_colors,
                    ..
                } => {
                    colors.extend(produced_colors.clone());
                }
                ManaProduction::AnyOneColor { color_options, .. } => {
                    colors.extend(color_options.clone());
                }
                ManaProduction::AnyCombination { color_options, .. } => {
                    colors.extend(color_options.clone());
                }
                ManaProduction::ChosenColor {
                    fixed_alternative, ..
                } => {
                    if let Some(c) = land.chosen_color() {
                        colors.push(c);
                    }
                    if let Some(c) = fixed_alternative {
                        colors.push(*c);
                    }
                }
                // CR 202.2c: Omnath, Locus of All — colors come from a target
                // object resolved at trigger time, not statically predictable
                // for land-animation color preview. Contribute nothing.
                ManaProduction::AnyCombinationOfObjectColors { .. } => {}
                _ => {}
            }
        }
    }
    colors
}

/// Check if the AI has sufficient alternative mana sources.
fn has_sufficient_mana_sources(ctx: &PolicyContext<'_>, exclude_land: ObjectId) -> bool {
    let land_count = ctx
        .state
        .battlefield
        .iter()
        .filter(|&&id| {
            id != exclude_land && {
                let Some(obj) = ctx.state.objects.get(&id) else {
                    return false;
                };
                obj.controller == ctx.ai_player
                    && obj.card_types.core_types.contains(&CoreType::Land)
            }
        })
        .count();

    land_count >= 3 // Heuristic: need at least 3 other lands
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use super::*;
    use crate::config::AiConfig;
    use crate::context::AiContext;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::effects::counter;
    use engine::game::engine::apply_as_current_for_simulation;
    use engine::game::layers::flush_layers;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification,
        CopyRetargetPermission, PtValue, QuantityExpr, StaticDefinition, TargetFilter,
    };
    use engine::types::ability::{Duration, ResolvedAbility, TargetRef};
    use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use engine::types::phase::Phase;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

    use crate::policies::effect_classify::{effect_polarity, EffectPolarity};

    const AI: PlayerId = PlayerId(0);

    fn mana_effect(colors: Vec<ManaColor>) -> Effect {
        Effect::Mana {
            produced: ManaProduction::Fixed {
                colors,
                contribution: Default::default(),
            },
            restrictions: Vec::new(),
            grants: Vec::new(),
            expiry: None,
            target: None,
        }
    }

    fn animate_effect() -> Effect {
        Effect::Animate {
            power: Some(PtValue::Fixed(2)),
            toughness: Some(PtValue::Fixed(2)),
            types: vec!["Creature".to_string()],
            remove_types: Vec::new(),
            target: TargetFilter::SelfRef,
            keywords: Vec::new(),
        }
    }

    fn generic_creature_type_effect() -> Effect {
        Effect::GenericEffect {
            static_abilities: vec![StaticDefinition::new(StaticMode::Continuous).modifications(
                vec![ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                }],
            )],
            duration: None,
            target: Some(TargetFilter::SelfRef),
            end_cost: None,
        }
    }

    fn land_with_ability(state: &mut GameState, ability: AbilityDefinition) -> ObjectId {
        let id = create_object(
            state,
            CardId(state.objects.len() as u64 + 1),
            AI,
            "Test Land".to_string(),
            Zone::Battlefield,
        );
        let obj = state.objects.get_mut(&id).unwrap();
        obj.card_types.core_types.push(CoreType::Land);
        Arc::make_mut(&mut obj.abilities).push(ability);
        id
    }

    fn policy_verdict(state: &GameState, source_id: ObjectId) -> PolicyVerdict {
        verdict_at(state, source_id, 0)
    }

    fn assert_score(verdict: PolicyVerdict, expected_reason: &str) {
        let PolicyVerdict::Score { delta, reason } = verdict else {
            panic!("expected score verdict");
        };
        assert_eq!(delta, 0.0);
        assert_eq!(reason.kind, expected_reason);
    }

    fn reason_kind(verdict: &PolicyVerdict) -> &str {
        let PolicyVerdict::Score { reason, .. } = verdict else {
            panic!("expected score verdict");
        };
        reason.kind
    }

    /// {1}{W}{B} animation ability (mana value 3), mirroring Shambling Vent.
    fn animate_ability_with_mana_cost() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, animate_effect()).cost(AbilityCost::Mana {
            cost: ManaCost::Cost {
                shards: vec![ManaCostShard::White, ManaCostShard::Black],
                generic: 1,
            },
        })
    }

    /// {1}{W}{B} animation via the `GenericEffect` + `AddType Creature` shape
    /// that real man-lands (Shambling Vent) actually use, not the synthetic
    /// `Effect::Animate`. Locks the production code path through the guard.
    fn generic_animate_ability_with_mana_cost() -> AbilityDefinition {
        AbilityDefinition::new(AbilityKind::Activated, generic_creature_type_effect()).cost(
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::White, ManaCostShard::Black],
                    generic: 1,
                },
            },
        )
    }

    /// An untapped basic-style land: `{T}: Add {color}`.
    fn mana_land(state: &mut GameState, color: ManaColor) -> ObjectId {
        land_with_ability(
            state,
            AbilityDefinition::new(AbilityKind::Activated, mana_effect(vec![color]))
                .cost(AbilityCost::Tap),
        )
    }

    #[test]
    fn animation_forcing_self_tap_for_mana_is_penalized() {
        // Manland is untapped but the AI has no other mana: paying {1}{W}{B}
        // would tap the manland itself, animating it into a tapped creature.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
        let PolicyVerdict::Score { delta, .. } = verdict else {
            panic!("expected score verdict");
        };
        assert_eq!(delta, TAPPED_LAND_PENALTY);
    }

    #[test]
    fn animation_with_sufficient_other_mana_is_not_tapped_penalized() {
        // W + B + a third source cover {1}{W}{B} without tapping the manland,
        // so it can animate and still attack/block this turn.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Black);
        mana_land(&mut state, ManaColor::White);

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_score");
    }

    #[test]
    fn animation_color_starved_off_source_is_penalized() {
        // Three other untapped sources cover the *total* of {1}{W}{B}, but none
        // produces black — only the manland could, so the engine would tap it
        // for {B} and leave a tapped creature. A count-only check would miss
        // this; the color-aware engine solver catches it.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Green);

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
    }

    #[test]
    fn generic_effect_manland_self_tap_is_penalized() {
        // Real Shambling Vent shape (GenericEffect AddType Creature). With no
        // other mana, paying {1}{W}{B} forces tapping the manland → penalized.
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, generic_animate_ability_with_mana_cost());

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
    }

    #[test]
    fn already_tapped_manland_animation_is_penalized() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Black);
        mana_land(&mut state, ManaColor::White);
        state.objects.get_mut(&source_id).unwrap().tapped = true;

        let verdict = policy_verdict(&state, source_id);
        assert_eq!(reason_kind(&verdict), "land_animation_tapped");
    }

    #[test]
    fn mana_ability_on_land_is_not_animation() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(
            &mut state,
            AbilityDefinition::new(AbilityKind::Activated, mana_effect(vec![ManaColor::Green])),
        );

        assert_score(
            policy_verdict(&state, source_id),
            "land_animation_not_animation",
        );
    }

    #[test]
    fn ability_animates_land_walks_sub_ability_chain() {
        let mut ability =
            AbilityDefinition::new(AbilityKind::Activated, mana_effect(vec![ManaColor::Green]));
        ability.sub_ability = Some(Box::new(AbilityDefinition::new(
            AbilityKind::Activated,
            animate_effect(),
        )));

        assert!(crate::manland::animates_source(&ability));
    }

    #[test]
    fn ability_animates_land_detects_generic_creature_type_grant() {
        let ability =
            AbilityDefinition::new(AbilityKind::Activated, generic_creature_type_effect());

        assert!(crate::manland::animates_source(&ability));
    }

    #[test]
    fn colors_produced_by_land_handles_any_one_color() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(
            &mut state,
            AbilityDefinition::new(
                AbilityKind::Activated,
                Effect::Mana {
                    produced: ManaProduction::AnyOneColor {
                        count: QuantityExpr::Fixed { value: 1 },
                        color_options: vec![ManaColor::White, ManaColor::Blue],
                        contribution: Default::default(),
                    },
                    restrictions: Vec::new(),
                    grants: Vec::new(),
                    expiry: None,
                    target: None,
                },
            ),
        );

        let colors = colors_produced_by_land(state.objects.get(&source_id).unwrap());
        assert_eq!(colors, vec![ManaColor::White, ManaColor::Blue]);
    }

    const MUTAVAULT: &str = "{T}: Add {C}.\n{1}: This land becomes a 2/2 creature with all creature types until end of turn. It's still a land.";
    const TREETOP_VILLAGE: &str = "This land enters tapped.\n{T}: Add {G}.\n{1}{G}: This land becomes a 3/3 green Ape creature with trample until end of turn. It's still a land. (It can deal excess combat damage to the player or planeswalker it's attacking.)";
    const CRAWLING_BARRENS: &str = "{T}: Add {C}.\n{4}: Put two +1/+1 counters on this land. Then you may have it become a 0/0 Elemental creature until end of turn. It's still a land.";
    const BLINKMOTH_NEXUS: &str = "{T}: Add {C}.\n{1}: This land becomes a 1/1 Blinkmoth artifact creature with flying until end of turn. It's still a land.\n{1}, {T}: Target Blinkmoth creature gets +1/+1 until end of turn.";
    const STALKING_STONES: &str = "{T}: Add {C}.\n{6}: This land becomes a 3/3 Elemental artifact creature that's still a land. (This effect lasts indefinitely.)";
    const RAGING_RAVINE: &str = "This land enters tapped.\n{T}: Add {R} or {G}.\n{2}{R}{G}: Until end of turn, this land becomes a 3/3 red and green Elemental creature with \"Whenever this creature attacks, put a +1/+1 counter on it.\" It's still a land.";

    fn verdict_at(state: &GameState, source_id: ObjectId, ability_index: usize) -> PolicyVerdict {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
            metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
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
        LandAnimationPolicy.verdict(&ctx)
    }

    fn is_redundant_reject(verdict: &PolicyVerdict) -> bool {
        matches!(verdict, PolicyVerdict::Reject { reason } if reason.kind == "land_animation_redundant_already_creature")
    }

    fn oracle_board(
        name: &str,
        oracle: &str,
        lands: &[(ManaColor, usize)],
    ) -> (GameRunner, ObjectId, usize) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let id = scenario.add_land_from_oracle(P0, name, oracle).id();
        for (color, n) in lands {
            for _ in 0..*n {
                scenario.add_basic_land(P0, *color);
            }
        }
        scenario.add_basic_land(P1, ManaColor::Blue);
        let mut runner = scenario.build();
        {
            let s = runner.state_mut();
            s.turn_number = 3;
            s.active_player = P0;
            s.phase = Phase::PreCombatMain;
            s.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let index = runner.state().objects[&id]
            .abilities
            .iter()
            .position(crate::manland::animates_source)
            .expect("the Oracle text parses to a self-animating ability");
        (runner, id, index)
    }

    fn announce(runner: &mut GameRunner, source_id: ObjectId, ability_index: usize) {
        apply_as_current_for_simulation(
            runner.state_mut(),
            GameAction::ActivateAbility {
                source_id,
                ability_index,
            },
        )
        .expect("the engine accepts the activation");
    }

    fn resolve_accepting_optional(runner: &mut GameRunner) {
        for _ in 0..8 {
            runner.advance_until_stack_empty();
            if matches!(
                runner.state().waiting_for,
                WaitingFor::OptionalEffectChoice { .. }
            ) {
                runner
                    .act(GameAction::DecideOptionalEffect { accept: true })
                    .expect("accept the optional animation");
                continue;
            }
            break;
        }
        let s = runner.state_mut();
        s.phase = Phase::PreCombatMain;
        s.active_player = P0;
        s.waiting_for = WaitingFor::Priority { player: P0 };
    }

    fn is_creature(state: &GameState, id: ObjectId) -> bool {
        state.objects[&id]
            .card_types
            .core_types
            .contains(&CoreType::Creature)
    }

    #[test]
    fn already_animated_land_reanimation_is_rejected() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        assert_eq!(
            reason_kind(&verdict_at(runner.state(), village, index)),
            "land_animation_score",
            "control: the first animation is scored"
        );
        announce(&mut runner, village, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            is_creature(runner.state(), village),
            "reach guard: the animation resolved"
        );
        assert!(
            runner.state().stack.is_empty(),
            "reach guard: only the type half can answer"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn pending_animation_on_the_stack_rejects_a_second_activation() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        announce(&mut runner, village, index);
        assert_eq!(
            runner.state().stack.len(),
            1,
            "reach guard: the animation is pending"
        );
        assert!(
            !is_creature(runner.state(), village),
            "reach guard: the type half cannot answer"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn unanimated_manland_is_not_rejected() {
        let mut state = GameState::new_two_player(42);
        let source_id = land_with_ability(&mut state, animate_ability_with_mana_cost());
        mana_land(&mut state, ManaColor::White);
        mana_land(&mut state, ManaColor::Black);
        mana_land(&mut state, ManaColor::White);
        assert!(!is_creature(&state, source_id));
        assert!(state.stack.is_empty());
        assert_eq!(
            reason_kind(&policy_verdict(&state, source_id)),
            "land_animation_score"
        );
    }

    #[test]
    fn countered_animation_does_not_block_a_legal_reanimation() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        announce(&mut runner, village, index);
        assert!(
            is_redundant_reject(&verdict_at(runner.state(), village, index)),
            "control: pending rejects"
        );
        let mut events = Vec::new();
        counter::resolve_all(
            runner.state_mut(),
            &ResolvedAbility::new(
                Effect::CounterAll {
                    target: TargetFilter::StackAbility {
                        controller: None,
                        tag: None,
                        kind: None,
                    },
                },
                Vec::new(),
                ObjectId(999),
                AI,
            ),
            &mut events,
        )
        .expect("counter resolves");
        assert!(
            runner.state().stack.is_empty(),
            "reach guard: the counter removed the entry"
        );
        assert!(
            !is_creature(runner.state(), village),
            "reach guard: the animation never resolved"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn counter_placing_reactivation_is_not_rejected() {
        let (mut runner, barrens, index) = oracle_board(
            "Crawling Barrens",
            CRAWLING_BARRENS,
            &[(ManaColor::Green, 12)],
        );
        announce(&mut runner, barrens, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            is_creature(runner.state(), barrens),
            "reach guard: the payoff is in force"
        );
        assert!(crate::manland::animation_payoff_in_force(
            runner.state(),
            barrens
        ));
        let verdict = verdict_at(runner.state(), barrens, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn trigger_granting_reactivation_is_not_rejected() {
        let (mut runner, ravine, index) = oracle_board(
            "Raging Ravine",
            RAGING_RAVINE,
            &[(ManaColor::Red, 6), (ManaColor::Green, 6)],
        );
        announce(&mut runner, ravine, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            is_creature(runner.state(), ravine),
            "reach guard: the payoff is in force"
        );
        let verdict = verdict_at(runner.state(), ravine, index);
        assert_eq!(
            reason_kind(&verdict),
            "land_animation_score",
            "got {verdict:?}"
        );
    }

    #[test]
    fn tapped_animated_manland_reanimation_is_rejected_not_tapped_scored() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        announce(&mut runner, village, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            is_creature(runner.state(), village),
            "reach guard: the animation resolved"
        );
        runner.state_mut().objects.get_mut(&village).unwrap().tapped = true;
        let state = runner.state();
        let ability = state.objects[&village].abilities[index].clone();
        assert!(
            crate::manland::activation_leaves_source_tapped(state, AI, village, index, &ability),
            "reach guard: the tapped branch is live on this board"
        );
        let verdict = verdict_at(state, village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn mutavault_reanimation_after_real_resolution_is_rejected() {
        let (mut runner, vault, index) =
            oracle_board("Mutavault", MUTAVAULT, &[(ManaColor::Green, 5)]);
        announce(&mut runner, vault, index);
        resolve_accepting_optional(&mut runner);
        let verdict = verdict_at(runner.state(), vault, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    /// An opponent's until-end-of-turn effect on `target`, timestamped after
    /// everything already applied to it.
    fn later_opponent_effect(
        runner: &mut GameRunner,
        target: ObjectId,
        modifications: Vec<ContinuousModification>,
    ) {
        let state = runner.state_mut();
        let opponent_land = state
            .objects
            .values()
            .find(|object| object.controller == P1)
            .map(|object| object.id)
            .expect("oracle_board gives the opponent a land");
        state.add_transient_continuous_effect(
            opponent_land,
            P1,
            Duration::UntilEndOfTurn,
            TargetFilter::SpecificObject { id: target },
            modifications,
            None,
        );
        flush_layers(state);
    }

    fn vetoed_without_board_check(state: &GameState, id: ObjectId, index: usize) -> bool {
        crate::manland::animation_payoff_in_force(state, id)
            && crate::manland::repeat_only_resets_characteristics(
                &state.objects[&id].abilities[index],
            )
    }

    #[test]
    fn mutavault_reactivation_after_a_later_pt_setting_effect_is_not_rejected() {
        let (mut runner, vault, index) =
            oracle_board("Mutavault", MUTAVAULT, &[(ManaColor::Green, 5)]);
        announce(&mut runner, vault, index);
        resolve_accepting_optional(&mut runner);
        later_opponent_effect(
            &mut runner,
            vault,
            vec![
                ContinuousModification::SetPower { value: 1 },
                ContinuousModification::SetToughness { value: 1 },
            ],
        );
        let state = runner.state();
        assert_eq!(
            (state.objects[&vault].power, state.objects[&vault].toughness),
            (Some(1), Some(1)),
            "reach guard: the later effect overrides the animation's 2/2"
        );
        assert!(
            vetoed_without_board_check(state, vault, index),
            "reach guard: only the board check can exempt this"
        );
        let verdict = verdict_at(state, vault, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn blinkmoth_reactivation_after_losing_flying_is_not_rejected() {
        let (mut runner, nexus, index) =
            oracle_board("Blinkmoth Nexus", BLINKMOTH_NEXUS, &[(ManaColor::Green, 5)]);
        announce(&mut runner, nexus, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            runner.state().objects[&nexus]
                .keywords
                .contains(&Keyword::Flying),
            "control: the animation grants flying"
        );
        later_opponent_effect(
            &mut runner,
            nexus,
            vec![ContinuousModification::RemoveKeyword {
                keyword: Keyword::Flying,
            }],
        );
        let state = runner.state();
        assert!(
            !state.objects[&nexus].keywords.contains(&Keyword::Flying),
            "reach guard: the later effect removed flying"
        );
        assert!(
            vetoed_without_board_check(state, nexus, index),
            "reach guard: only the board check can exempt this"
        );
        let verdict = verdict_at(state, nexus, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn externally_animated_land_whose_own_animation_differs_is_not_rejected() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        later_opponent_effect(
            &mut runner,
            village,
            vec![
                ContinuousModification::AddType {
                    core_type: CoreType::Creature,
                },
                ContinuousModification::SetPower { value: 1 },
                ContinuousModification::SetToughness { value: 1 },
            ],
        );
        let state = runner.state();
        assert!(
            is_creature(state, village),
            "reach guard: the external effect animated it"
        );
        assert!(
            vetoed_without_board_check(state, village, index),
            "reach guard: only the board check can exempt this"
        );
        let verdict = verdict_at(state, village, index);
        assert_eq!(
            reason_kind(&verdict),
            "land_animation_score",
            "got {verdict:?}"
        );
    }

    #[test]
    fn leaf_chain_reactivation_after_a_later_pt_setting_effect_is_not_rejected() {
        let (mut runner, stones, index) =
            oracle_board("Stalking Stones", STALKING_STONES, &[(ManaColor::Green, 8)]);
        assert!(
            runner.state().objects[&stones].abilities[index]
                .sub_ability
                .is_none(),
            "reach guard: a leaf chain, which leaves the layers unflushed"
        );
        announce(&mut runner, stones, index);
        resolve_accepting_optional(&mut runner);
        later_opponent_effect(
            &mut runner,
            stones,
            vec![
                ContinuousModification::SetPower { value: 1 },
                ContinuousModification::SetToughness { value: 1 },
            ],
        );
        let state = runner.state();
        assert_eq!(
            (
                state.objects[&stones].power,
                state.objects[&stones].toughness
            ),
            (Some(1), Some(1)),
            "reach guard: the later effect overrides the animation's 3/3"
        );
        assert!(
            vetoed_without_board_check(state, stones, index),
            "reach guard: only the board check can exempt this"
        );
        let verdict = verdict_at(state, stones, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    const STIFLE: &str =
        "Counter target activated or triggered ability. (Mana abilities can't be targeted.)";
    const LIGHTNING_BOLT: &str = "Lightning Bolt deals 3 damage to any target.";
    const BEAST_WITHIN: &str =
        "Destroy target permanent. Its controller creates a 3/3 green Beast creature token.";

    /// Treetop Village for the AI plus an instant in `responder_controller`'s
    /// hand, with the AI holding priority in its own precombat main phase.
    fn responder_board(
        responder_card: &str,
        responder_oracle: &str,
        responder_controller: PlayerId,
    ) -> (GameRunner, ObjectId, usize, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let responder = scenario
            .add_spell_to_hand_from_oracle(
                responder_controller,
                responder_card,
                true,
                responder_oracle,
            )
            .id();
        scenario.add_basic_land(responder_controller, ManaColor::Blue);
        let mut runner = scenario.build();
        {
            let s = runner.state_mut();
            s.turn_number = 3;
            s.active_player = P0;
            s.phase = Phase::PreCombatMain;
            s.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let index = runner.state().objects[&village]
            .abilities
            .iter()
            .position(crate::manland::animates_source)
            .expect("the Oracle text parses to a self-animating ability");
        (runner, village, index, responder)
    }

    /// CR 117.4: one pass is not all players passing in succession, so the
    /// activation stays on the stack while the opponent takes priority.
    /// Returns the pending entry's id.
    fn pass_to_opponent(runner: &mut GameRunner) -> ObjectId {
        let pending = runner.state().stack.back().expect("pending entry").id;
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("the AI passes priority");
        pending
    }

    fn entry_targets(state: &GameState, entry_id: ObjectId) -> Vec<TargetRef> {
        state
            .stack
            .iter()
            .find(|e| e.id == entry_id)
            .and_then(|e| e.ability())
            .map(|a| a.targets.clone())
            .unwrap_or_default()
    }

    #[test]
    fn counter_targeting_a_pending_animation_allows_a_response_activation() {
        let (mut runner, village, index, stifle) = responder_board("Stifle", STIFLE, P1);
        announce(&mut runner, village, index);
        assert!(
            is_redundant_reject(&verdict_at(runner.state(), village, index)),
            "control: with nothing answering it, the pending activation vetoes"
        );
        let pending = pass_to_opponent(&mut runner);
        {
            let commit = runner.cast(stifle).target_object(pending).commit();
            assert_eq!(
                commit.state().stack.len(),
                2,
                "reach guard: both on the stack"
            );
        }
        let top = runner.state().stack.back().unwrap().id;
        assert!(
            entry_targets(runner.state(), top).contains(&TargetRef::Object(pending)),
            "reach guard: the counter targets the pending activation"
        );
        assert!(
            !is_creature(runner.state(), village),
            "reach guard: only the stack half can answer"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn an_unrelated_object_above_a_pending_animation_still_vetoes() {
        let (mut runner, village, index, bolt) =
            responder_board("Lightning Bolt", LIGHTNING_BOLT, P1);
        announce(&mut runner, village, index);
        let pending = pass_to_opponent(&mut runner);
        {
            let commit = runner.cast(bolt).target_player(P0).commit();
            assert_eq!(
                commit.state().stack.len(),
                2,
                "reach guard: both on the stack"
            );
        }
        let top = runner.state().stack.back().unwrap().id;
        assert_ne!(
            top, pending,
            "reach guard: the bolt sits above the activation"
        );
        assert!(
            !entry_targets(runner.state(), top).contains(&TargetRef::Object(pending)),
            "reach guard: the bolt does not target the pending activation"
        );
        assert!(
            !is_creature(runner.state(), village),
            "reach guard: only the stack half can answer"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn removal_aimed_at_the_land_not_at_the_pending_activation_still_vetoes() {
        let (mut runner, village, index, beast_within) =
            responder_board("Beast Within", BEAST_WITHIN, P1);
        announce(&mut runner, village, index);
        let pending = pass_to_opponent(&mut runner);
        {
            let commit = runner.cast(beast_within).target_object(village).commit();
            assert_eq!(
                commit.state().stack.len(),
                2,
                "reach guard: both on the stack"
            );
        }
        let top = runner.state().stack.back().unwrap().id;
        assert!(
            entry_targets(runner.state(), top).contains(&TargetRef::Object(village)),
            "reach guard: the removal targets the land permanent"
        );
        assert!(
            !entry_targets(runner.state(), top).contains(&TargetRef::Object(pending)),
            "reach guard: it does not target the pending activation"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    /// CR 117.3c: activating keeps priority with the same player, so P0 can
    /// immediately follow the activation with its own Stifle — no
    /// `pass_to_opponent` step is needed to reach this board. Reuses
    /// `responder_board` with the responder controlled by P0 instead of P1.
    #[test]
    fn same_controller_counter_targeting_a_pending_animation_also_allows_a_response_activation() {
        let (mut runner, village, index, stifle) = responder_board("Stifle", STIFLE, P0);
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        {
            let commit = runner.cast(stifle).target_object(pending).commit();
            assert_eq!(
                commit.state().stack.len(),
                2,
                "reach guard: both on the stack"
            );
        }
        let top = runner.state().stack.back().unwrap().id;
        assert_eq!(
            runner.state().stack.back().unwrap().controller,
            P0,
            "reach guard: the counter's controller is P0, the same as the pending entry's"
        );
        assert!(
            entry_targets(runner.state(), top).contains(&TargetRef::Object(pending)),
            "reach guard: the counter targets the pending activation"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn a_second_unanswered_pending_animation_still_vetoes_a_third_activation() {
        // After Stifle answers the first pending animation and both
        // players pass back to P0, the AI is free to activate again — but the
        // resulting second pending animation is itself unanswered, so a third
        // activation must stay vetoed.
        let (mut runner, village, index, stifle) = responder_board("Stifle", STIFLE, P1);
        announce(&mut runner, village, index);
        let pending = pass_to_opponent(&mut runner);
        {
            let commit = runner.cast(stifle).target_object(pending).commit();
            assert_eq!(
                commit.state().stack.len(),
                2,
                "reach guard: both on the stack"
            );
        }
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("P1 passes priority back to P0 after casting Stifle");
        let second_verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(second_verdict, PolicyVerdict::Score { .. }),
            "second activation: got {second_verdict:?}"
        );
        announce(&mut runner, village, index);
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .filter(|entry| matches!(
                    &entry.kind,
                    StackEntryKind::ActivatedAbility { source_id, .. } if *source_id == village
                ))
                .count(),
            2,
            "reach guard: both pending copies of the animation are on the stack"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            is_redundant_reject(&verdict),
            "third activation: got {verdict:?}"
        );
    }

    /// CR 707.10: a copy effect's polarity is `Contextual`
    /// (`effect_classify::effect_polarity`), not `Harmful` — confirmed below —
    /// so it must not count as an answer to a pending animation the way
    /// Stifle's `Effect::Counter` does.
    fn copy_spell_effect() -> Effect {
        Effect::CopySpell {
            target: TargetFilter::Any,
            retarget: CopyRetargetPermission::KeepOriginalTargets,
            copier: None,
            additional_modifications: Vec::new(),
            starting_loyalty_from_casualty_sacrifice: false,
        }
    }

    #[test]
    fn copy_effect_targeting_a_pending_animation_does_not_lift_the_veto() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().expect("pending entry").id;
        assert!(
            is_redundant_reject(&verdict_at(runner.state(), village, index)),
            "control: with nothing answering it, the pending activation vetoes"
        );
        assert_eq!(
            effect_polarity(&copy_spell_effect()),
            EffectPolarity::Contextual,
            "reach guard: CopySpell must not classify as Harmful for this test to discriminate"
        );

        let copy_entry_id = ObjectId(runner.state().next_object_id);
        {
            let state = runner.state_mut();
            let ability = ResolvedAbility::new(
                copy_spell_effect(),
                vec![TargetRef::Object(pending)],
                ObjectId(9998),
                P1,
            );
            state.stack.push_back(StackEntry {
                id: copy_entry_id,
                source_id: ObjectId(9998),
                controller: P1,
                kind: StackEntryKind::ActivatedAbility {
                    source_id: ObjectId(9998),
                    ability: Box::new(ability),
                },
            });
            state.next_object_id += 1;
        }
        assert!(
            entry_targets(runner.state(), copy_entry_id).contains(&TargetRef::Object(pending)),
            "reach guard: the copy entry targets the pending activation"
        );

        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }
}

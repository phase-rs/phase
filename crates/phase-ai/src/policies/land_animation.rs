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
            ..
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
        //
        // Root only (`SearchDepth`): the check activates and settles the
        // repeat on copies of the board.
        if ctx.at_root()
            && crate::manland::repeat_gains_nothing(ctx.state, &ctx.candidate.action, ability_def)
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
    use crate::policies::context::SearchDepth;
    use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
    use engine::game::effects::counter;
    use engine::game::engine::apply_as_current_for_simulation;
    use engine::game::layers::flush_layers;
    use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
    use engine::game::zones::create_object;
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ContinuousModification, PtValue, QuantityExpr,
        StaticDefinition, TargetFilter,
    };
    use engine::types::ability::{Duration, ResolvedAbility, TargetRef};
    use engine::types::counter::CounterType;
    use engine::types::game_state::{StackEntry, StackEntryKind, WaitingFor};
    use engine::types::identifiers::CardId;
    use engine::types::keywords::Keyword;
    use engine::types::mana::{ManaColor, ManaCost, ManaCostShard};
    use engine::types::phase::Phase;
    use engine::types::statics::StaticMode;
    use engine::types::zones::Zone;

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
        verdict_at_depth(state, source_id, ability_index, SearchDepth::Root)
    }

    fn verdict_at_depth(
        state: &GameState,
        source_id: ObjectId,
        ability_index: usize,
        search_depth: SearchDepth,
    ) -> PolicyVerdict {
        let decision = AiDecisionContext {
            waiting_for: WaitingFor::Priority { player: AI },
            candidates: Vec::new(),
        };
        let candidate = CandidateAction {
            action: GameAction::ActivateAbility {
                source_id,
                ability_index,
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
            search_depth,
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
                payment_mode: engine::types::game_state::CastPaymentMode::Auto,
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
            "reach guard: nothing is pending"
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
            "reach guard: the land is not a creature yet"
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
        // Hand-built `CounterAll` standing in for Glen Elendra's Answer.
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
        let verdict = verdict_at(runner.state(), barrens, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// Synthetic: Crawling Barrens, the nearest printed land, with its optional
    /// animation made mandatory.
    const MANDATORY_CRAWLING_BARRENS: &str = "{T}: Add {C}.\n{4}: Put two +1/+1 counters on this land. It becomes a 0/0 Elemental creature until end of turn. It's still a land.";

    #[test]
    fn a_reactivation_that_also_places_counters_is_not_rejected() {
        let (mut runner, barrens, index) = oracle_board(
            "Crawling Barrens",
            MANDATORY_CRAWLING_BARRENS,
            &[(ManaColor::Green, 12)],
        );
        announce(&mut runner, barrens, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            is_creature(runner.state(), barrens),
            "reach guard: the animation resolved"
        );
        let counters = |state: &GameState| {
            state.objects[&barrens]
                .counters
                .get(&CounterType::Plus1Plus1)
                .copied()
        };
        assert_eq!(
            (
                counters(&played_out(runner.state(), None)),
                counters(&played_out(runner.state(), Some(activate(barrens, index)))),
            ),
            (Some(2), Some(4)),
            "reach guard: the repeat is worth two counters"
        );
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
    /// everything already applied to it. The effects are injected, not cast;
    /// printed effects of these shapes include Diminish (base power and
    /// toughness), Canopy Claws (loses flying) and Kamahl, Fist of Krosa (a land becomes a 1/1 creature).
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
        is_creature(state, id)
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
        let (mut runner, stones, index) = oracle_board(
            "Stalking Stones",
            STALKING_STONES,
            &[(ManaColor::Green, 12)],
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
        pass_until(&mut runner, p0_has_priority);
        assert!(
            !is_creature(runner.state(), village),
            "reach guard: the land is not a creature yet"
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
        pass_until(&mut runner, p0_has_priority);
        assert!(
            !is_creature(runner.state(), village),
            "reach guard: the land is not a creature yet"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    /// CR 700.4: on the line with the repeat, the land is a creature when
    /// Beast Within destroys it, so only that line has a creature die.
    #[test]
    fn removal_aimed_at_the_land_above_the_pending_activation_leaves_the_repeat_eligible() {
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
        pass_until(&mut runner, p0_has_priority);
        let died_a_creature = |state: &GameState| {
            state.zone_changes_this_turn.iter().any(|record| {
                record.object_id == village && record.core_types.contains(&CoreType::Creature)
            })
        };
        assert_eq!(
            (
                died_a_creature(&played_out(runner.state(), None)),
                died_a_creature(&played_out(runner.state(), Some(activate(village, index)))),
            ),
            (false, true),
            "reach guard: the land dies a creature only after a repeat"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
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
        // After Stifle answers the first pending animation, the AI is free to
        // activate again — but the resulting second pending animation is
        // itself unanswered, so a third activation must stay vetoed.
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

    const RETURN_THE_FAVOR: &str = "Spree (Choose one or more additional costs.)\n+ {1} \u{2014} Copy target instant spell, sorcery spell, activated ability, or triggered ability. You may choose new targets for the copy.\n+ {1} \u{2014} Change the target of target spell or ability with a single target.";

    #[test]
    fn copy_effect_targeting_a_pending_animation_does_not_lift_the_veto() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let copier = scenario
            .add_spell_to_hand_from_oracle(P1, "Return the Favor", true, RETURN_THE_FAVOR)
            .id();
        for _ in 0..3 {
            scenario.add_basic_land(P1, ManaColor::Red);
        }
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = runner.state().objects[&village]
            .abilities
            .iter()
            .position(crate::manland::animates_source)
            .expect("the Oracle text parses to a self-animating ability");
        announce(&mut runner, village, index);
        assert!(
            is_redundant_reject(&verdict_at(runner.state(), village, index)),
            "control: with nothing answering it, the pending activation vetoes"
        );

        let pending = pass_to_opponent(&mut runner);
        runner
            .cast(copier)
            .modes(&[0])
            .target_object(pending)
            .commit();
        pass_until(&mut runner, p0_has_priority);
        let top = runner.state().stack.back().expect("the copy spell");
        assert!(
            runner.state().stack.len() == 2
                && top.controller == P1
                && matches!(top.kind, StackEntryKind::Spell { .. })
                && entry_targets(runner.state(), top.id).contains(&TargetRef::Object(pending)),
            "reach guard: P1's Return the Favor is on the stack above, targeting, the pending animation"
        );

        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    const DEFLECTING_SWAT: &str = "If you control a commander, you may cast this spell without paying its mana cost.\nYou may choose new targets for target spell or ability.";
    const QUICKEN: &str = "The next sorcery spell you cast this turn can be cast as though it had flash. (It can be cast any time you could cast an instant.)\nDraw a card.";
    const BRAMBLECRUSH: &str = "Destroy target noncreature permanent.";
    const GLEN_ELENDRAS_ANSWER: &str = "This spell can't be countered.\nCounter all spells your opponents control and all abilities your opponents control. Create a 1/1 blue and black Faerie creature token with flying for each spell and ability countered this way.";
    /// Synthetic: no printed land has two self-animating abilities; the nearest printed card is Chimeric Sphere (an artifact, outside this policy).
    const TWO_BODIES: &str = "{1}: This land becomes a 2/2 creature until end of turn. It's still a land.\n{2}: This land becomes a 4/4 creature with flying until end of turn. It's still a land.";
    /// Synthetic: no printed land has two self-animating abilities; the nearest printed cards are Mutavault (until end of turn) and Stalking Stones (indefinitely).
    const TWO_LIFETIMES: &str = "{1}: This land becomes a 3/3 creature until end of turn. It's still a land.\n{3}: This land becomes a 3/3 creature that's still a land. (This effect lasts indefinitely.)";

    /// Passes priority until `reached` holds.
    fn pass_until(runner: &mut GameRunner, reached: impl Fn(&GameState) -> bool) {
        for _ in 0..12 {
            if reached(runner.state()) {
                return;
            }
            apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
                .expect("the player with priority passes");
        }
        panic!("not reached: {:?}", runner.state().waiting_for);
    }

    fn p0_has_priority(state: &GameState) -> bool {
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0)
    }

    /// Indices of `id`'s self-animating abilities, in ability order.
    fn animation_indices(runner: &GameRunner, id: ObjectId) -> Vec<usize> {
        runner.state().objects[&id]
            .abilities
            .iter()
            .enumerate()
            .filter(|(_, ability)| crate::manland::animates_source(ability))
            .map(|(index, _)| index)
            .collect()
    }

    fn push_ability_entry(runner: &mut GameRunner, ability: ResolvedAbility) -> ObjectId {
        let state = runner.state_mut();
        let id = ObjectId(state.next_object_id);
        state.next_object_id += 1;
        state.stack.push_back(StackEntry {
            id,
            source_id: ability.source_id,
            controller: ability.controller,
            kind: StackEntryKind::ActivatedAbility {
                source_id: ability.source_id,
                ability: Box::new(ability),
            },
        });
        id
    }

    fn stack_ability_filter() -> TargetFilter {
        TargetFilter::StackAbility {
            controller: None,
            tag: None,
            kind: None,
        }
    }

    /// Synthetic: a root that targets `pending` and does nothing to it, above `child`. Aether Spike has this shape aimed at a spell; Flavor Judge, an Un-card, aims it at a spell or ability, but its parse targets spells only.
    fn root_aimed_at(pending: ObjectId, child: ResolvedAbility) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::TargetOnly {
                target: stack_ability_filter(),
            },
            vec![TargetRef::Object(pending)],
            ObjectId(9998),
            P1,
        )
        .sub_ability(child)
    }

    /// Synthetic: the `Counter` child of `root_aimed_at`; see there for the nearest printed cards.
    fn counter_node(targets: Vec<TargetRef>, target: TargetFilter) -> ResolvedAbility {
        ResolvedAbility::new(
            Effect::Counter {
                target,
                source_rider: None,
                countered_spell_zone: None,
            },
            targets,
            ObjectId(9998),
            P1,
        )
    }

    /// What each node of `entry_id`'s reach acts on.
    fn reach_of(state: &GameState, entry_id: ObjectId) -> Vec<Vec<TargetRef>> {
        let entry = state.stack.iter().find(|e| e.id == entry_id).unwrap();
        engine::game::effects::stack_reach::stack_entry_node_reach(state, entry)
            .into_iter()
            .map(|reach| reach.acted_on)
            .collect()
    }

    #[test]
    fn a_counter_child_aimed_elsewhere_keeps_the_veto() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        let elsewhere = push_ability_entry(
            &mut runner,
            ResolvedAbility::new(Effect::NoOp, Vec::new(), ObjectId(9997), P1),
        );
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        let chain = push_ability_entry(
            &mut runner,
            root_aimed_at(
                pending,
                counter_node(vec![TargetRef::Object(elsewhere)], stack_ability_filter()),
            ),
        );
        assert_eq!(
            entry_targets(runner.state(), chain),
            vec![TargetRef::Object(pending)],
            "reach guard: the root targets the pending animation"
        );
        assert_eq!(
            reach_of(runner.state(), chain),
            vec![Vec::new(), vec![TargetRef::Object(elsewhere)]],
            "reach guard: only the child counters, and not the pending animation"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn a_counter_child_inheriting_the_root_target_lifts_the_veto() {
        let (mut runner, village, index) =
            oracle_board("Treetop Village", TREETOP_VILLAGE, &[(ManaColor::Green, 8)]);
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        let chain = push_ability_entry(
            &mut runner,
            root_aimed_at(
                pending,
                counter_node(Vec::new(), TargetFilter::ParentTarget),
            ),
        );
        assert_eq!(
            reach_of(runner.state(), chain),
            vec![Vec::new(), vec![TargetRef::Object(pending)]],
            "reach guard: the child counters the root's target"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 405.5: Stifle below the animation resolves after it, even once
    /// Deflecting Swat has pointed it at the animation (CR 115.7d).
    #[test]
    fn a_counter_below_retargeted_to_the_pending_animation_keeps_the_veto() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        let vault = scenario
            .add_land_from_oracle(P0, "Mutavault", MUTAVAULT)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let stifle = scenario
            .add_spell_to_hand_from_oracle(P1, "Stifle", true, STIFLE)
            .id();
        let swat = scenario
            .add_spell_to_hand_from_oracle(P1, "Deflecting Swat", true, DEFLECTING_SWAT)
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        for _ in 0..3 {
            scenario.add_basic_land(P1, ManaColor::Red);
        }
        let mut runner = scenario.build();
        {
            let s = runner.state_mut();
            s.turn_number = 3;
            s.active_player = P0;
            s.phase = Phase::PreCombatMain;
            s.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let village_index = animation_indices(&runner, village)[0];
        let vault_index = animation_indices(&runner, vault)[0];
        announce(&mut runner, vault, vault_index);
        let other = pass_to_opponent(&mut runner);
        {
            runner.cast(stifle).target_object(other).commit();
        }
        let stifle_entry = runner.state().stack.back().unwrap().id;
        pass_until(&mut runner, p0_has_priority);
        announce(&mut runner, village, village_index);
        let pending = pass_to_opponent(&mut runner);
        {
            runner.cast(swat).target_object(stifle_entry).commit();
        }
        pass_until(&mut runner, |s| {
            matches!(s.waiting_for, WaitingFor::OptionalEffectChoice { .. })
        });
        runner
            .act(GameAction::DecideOptionalEffect { accept: true })
            .expect("Deflecting Swat's controller chooses new targets");
        runner
            .act(GameAction::RetargetSpell {
                new_targets: vec![Some(TargetRef::Object(pending))],
            })
            .expect("Stifle can target the pending animation");
        pass_until(&mut runner, p0_has_priority);
        let order: Vec<ObjectId> = runner.state().stack.iter().map(|e| e.id).collect();
        assert_eq!(
            order,
            vec![other, stifle_entry, pending],
            "reach guard: Stifle sits below the animation"
        );
        assert_eq!(
            entry_targets(runner.state(), stifle_entry),
            vec![TargetRef::Object(pending)],
            "reach guard: Stifle now targets the pending animation"
        );
        let verdict = verdict_at(runner.state(), village, village_index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn an_identical_pending_animation_vetoes_and_a_different_one_does_not() {
        let (mut runner, land, _) =
            oracle_board("Two Bodies", TWO_BODIES, &[(ManaColor::Green, 8)]);
        let [small, large] = animation_indices(&runner, land)[..] else {
            panic!("reach guard: two self-animating abilities");
        };
        announce(&mut runner, land, small);
        assert!(
            !is_creature(runner.state(), land),
            "reach guard: only the pending animation can answer"
        );
        let same = verdict_at(runner.state(), land, small);
        assert!(is_redundant_reject(&same), "same ability: got {same:?}");
        let different = verdict_at(runner.state(), land, large);
        assert!(
            matches!(different, PolicyVerdict::Score { .. }),
            "different body: got {different:?}"
        );
    }

    #[test]
    fn a_repeat_that_outlasts_the_animation_in_force_is_not_vetoed() {
        let (mut runner, land, _) =
            oracle_board("Two Lifetimes", TWO_LIFETIMES, &[(ManaColor::Green, 8)]);
        let [this_turn, indefinite] = animation_indices(&runner, land)[..] else {
            panic!("reach guard: two self-animating abilities");
        };
        announce(&mut runner, land, this_turn);
        let pending = verdict_at(runner.state(), land, indefinite);
        assert!(
            matches!(pending, PolicyVerdict::Score { .. }),
            "pending shorter animation: got {pending:?}"
        );
        resolve_accepting_optional(&mut runner);
        assert!(is_creature(runner.state(), land), "reach guard: animated");
        let same = verdict_at(runner.state(), land, this_turn);
        assert!(is_redundant_reject(&same), "same lifetime: got {same:?}");
        let longer = verdict_at(runner.state(), land, indefinite);
        assert!(
            matches!(longer, PolicyVerdict::Score { .. }),
            "longer lifetime: got {longer:?}"
        );

        // CR 514.2: the cleanup step ends the until-end-of-turn body, and only
        // the indefinite repeat carries it into a later turn.
        let mut without = runner.state().clone();
        engine::game::layers::prune_end_of_turn_effects(&mut without);
        flush_layers(&mut without);
        assert!(!is_creature(&without, land), "control: the body ends");
        announce(&mut runner, land, indefinite);
        resolve_accepting_optional(&mut runner);
        let state = runner.state_mut();
        engine::game::layers::prune_end_of_turn_effects(state);
        flush_layers(state);
        assert!(is_creature(state, land), "the repeat outlasts the turn");
    }

    #[test]
    fn stalking_stones_repeat_after_resolution_is_rejected() {
        let (mut runner, stones, index) = oracle_board(
            "Stalking Stones",
            STALKING_STONES,
            &[(ManaColor::Green, 12)],
        );
        announce(&mut runner, stones, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            runner
                .state()
                .transient_continuous_effects
                .iter()
                .any(|effect| effect.duration == Duration::Permanent),
            "reach guard: the animation lasts indefinitely"
        );
        let verdict = verdict_at(runner.state(), stones, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    /// CR 608.2b: Bramblecrush, cast with Quicken's permission above the
    /// pending animation, loses its target if the land is a creature first.
    #[test]
    fn a_response_that_escapes_noncreature_removal_above_the_animation_is_not_vetoed() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let quicken = scenario
            .add_spell_to_hand_from_oracle(P1, "Quicken", true, QUICKEN)
            .id();
        let bramblecrush = scenario
            .add_spell_to_hand_from_oracle(P1, "Bramblecrush", false, BRAMBLECRUSH)
            .id();
        scenario.add_basic_land(P1, ManaColor::Blue);
        for _ in 0..4 {
            scenario.add_basic_land(P1, ManaColor::Green);
        }
        for _ in 0..3 {
            scenario.add_card_to_library_top(P1, "Island");
        }
        let mut runner = scenario.build();
        {
            let s = runner.state_mut();
            s.turn_number = 3;
            s.active_player = P0;
            s.phase = Phase::PreCombatMain;
            s.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        let pending = pass_to_opponent(&mut runner);
        {
            runner.cast(quicken).commit();
        }
        pass_until(&mut runner, |s| s.stack.len() == 1 && p0_has_priority(s));
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("the AI passes priority");
        {
            runner.cast(bramblecrush).target_object(village).commit();
        }
        let removal = runner.state().stack.back().unwrap().id;
        pass_until(&mut runner, p0_has_priority);
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![pending, removal],
            "reach guard: the sorcery sits above the animation"
        );
        assert_eq!(
            reach_of(runner.state(), removal),
            vec![vec![TargetRef::Object(village)]],
            "reach guard: it destroys the noncreature land"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn a_mass_counter_above_the_pending_animation_lifts_the_veto() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let answer = scenario
            .add_spell_to_hand_from_oracle(P1, "Glen Elendra's Answer", true, GLEN_ELENDRAS_ANSWER)
            .id();
        for _ in 0..4 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();
        {
            let s = runner.state_mut();
            s.turn_number = 3;
            s.active_player = P0;
            s.phase = Phase::PreCombatMain;
            s.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        pass_to_opponent(&mut runner);
        {
            runner.cast(answer).commit();
        }
        pass_until(&mut runner, p0_has_priority);
        let top = runner.state().stack.back().unwrap();
        assert!(
            top.ability()
                .is_some_and(|root| matches!(root.effect, Effect::CounterAll { .. })),
            "reach guard: a mass counter sits above the animation"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    const TIME_STOP: &str = "End the turn. (Exile all spells and abilities, including this spell. The player whose turn it is discards down to their maximum hand size. Damage heals and \"this turn\" and \"until end of turn\" effects end.)";
    const DIABOLIC_EDICT: &str = "Target player sacrifices a creature of their choice.";
    const DAY_OF_JUDGMENT: &str = "Destroy all creatures.";

    /// The AI holding priority in its own precombat main phase on turn 3.
    fn p0_main_phase(runner: &mut GameRunner) {
        let s = runner.state_mut();
        s.turn_number = 3;
        s.active_player = P0;
        s.phase = Phase::PreCombatMain;
        s.waiting_for = WaitingFor::Priority { player: P0 };
    }

    /// CR 724.1b: Time Stop exiles the pending activation, and Stalking
    /// Stones' animation lasts indefinitely, so only a repeat resolved first
    /// leaves a creature.
    #[test]
    fn ending_the_turn_above_a_lasting_pending_animation_leaves_the_repeat_eligible() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let stones = scenario
            .add_land_from_oracle(P0, "Stalking Stones", STALKING_STONES)
            .id();
        for _ in 0..12 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let time_stop = scenario
            .add_spell_to_hand_from_oracle(P1, "Time Stop", true, TIME_STOP)
            .id();
        for _ in 0..6 {
            scenario.add_basic_land(P1, ManaColor::Blue);
        }
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, stones)[0];
        announce(&mut runner, stones, index);
        assert!(
            is_redundant_reject(&verdict_at(runner.state(), stones, index)),
            "control: with nothing above it, the pending activation vetoes"
        );
        let pending = pass_to_opponent(&mut runner);
        {
            runner.cast(time_stop).commit();
        }
        pass_until(&mut runner, p0_has_priority);
        let top = runner.state().stack.back().unwrap();
        assert!(
            top.ability()
                .is_some_and(|root| matches!(root.effect, Effect::EndTheTurn)),
            "reach guard: an end-the-turn spell sits above the animation"
        );
        assert_eq!(
            runner.state().stack.front().map(|entry| entry.id),
            Some(pending),
            "reach guard: the pending activation is below it"
        );
        let verdict = verdict_at(runner.state(), stones, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 701.21a: with the land animated first, the edict's player chooses
    /// between it and Grizzly Bears.
    #[test]
    fn an_edict_above_the_pending_animation_leaves_the_repeat_eligible() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        let bears = scenario.add_creature(P0, "Grizzly Bears", 2, 2).id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let edict = scenario
            .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT)
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P1, ManaColor::Black);
        }
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        assert!(
            is_redundant_reject(&verdict_at(runner.state(), village, index)),
            "control: with nothing above it, the pending activation vetoes"
        );
        let pending = pass_to_opponent(&mut runner);
        {
            runner.cast(edict).target_player(P0).commit();
        }
        pass_until(&mut runner, p0_has_priority);
        let top = runner.state().stack.back().unwrap().id;
        assert_ne!(
            top, pending,
            "reach guard: the edict sits above the animation"
        );
        assert_eq!(
            entry_targets(runner.state(), top),
            vec![TargetRef::Player(P0)],
            "reach guard: the edict targets the AI"
        );
        assert!(
            is_creature(runner.state(), bears) && !is_creature(runner.state(), village),
            "reach guard: Grizzly Bears is the AI's only creature"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 701.8a: Day of Judgment destroys the land only if it is a creature
    /// first, so the two lines end differently.
    #[test]
    fn a_wrath_above_the_pending_animation_leaves_the_repeat_eligible() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let quicken = scenario
            .add_spell_to_hand_from_oracle(P1, "Quicken", true, QUICKEN)
            .id();
        let wrath = scenario
            .add_spell_to_hand_from_oracle(P1, "Day of Judgment", false, DAY_OF_JUDGMENT)
            .id();
        scenario.add_basic_land(P1, ManaColor::Blue);
        for _ in 0..4 {
            scenario.add_basic_land(P1, ManaColor::White);
        }
        for _ in 0..3 {
            scenario.add_card_to_library_top(P1, "Island");
        }
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        let pending = pass_to_opponent(&mut runner);
        {
            runner.cast(quicken).commit();
        }
        pass_until(&mut runner, |s| s.stack.len() == 1 && p0_has_priority(s));
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("the AI passes priority");
        {
            runner.cast(wrath).commit();
        }
        let removal = runner.state().stack.back().unwrap().id;
        pass_until(&mut runner, p0_has_priority);
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![pending, removal],
            "reach guard: the sorcery sits above the animation"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 405.5: the edict below the animation resolves after it, so the land
    /// is a creature by then on both lines.
    #[test]
    fn an_edict_below_the_pending_animation_keeps_the_veto() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        scenario.add_creature(P0, "Grizzly Bears", 2, 2);
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let edict = scenario
            .add_spell_to_hand_from_oracle(P1, "Diabolic Edict", true, DIABOLIC_EDICT)
            .id();
        for _ in 0..2 {
            scenario.add_basic_land(P1, ManaColor::Black);
        }
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, village)[0];
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("the AI passes priority");
        {
            runner.cast(edict).target_player(P0).commit();
        }
        let below = runner.state().stack.back().unwrap().id;
        pass_until(&mut runner, p0_has_priority);
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![below, pending],
            "reach guard: the edict sits below the animation"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    const INSPIRED_CHARGE: &str = "Creatures you control get +2/+1 until end of turn.";

    /// CR 611.2c: Inspired Charge's bonus applies only to the creatures on the
    /// battlefield as it resolves, so the land gets it only if it is a creature
    /// first.
    #[test]
    fn a_mass_pump_above_the_pending_animation_leaves_the_repeat_eligible() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::Green);
            scenario.add_basic_land(P0, ManaColor::White);
        }
        let charge = scenario
            .add_spell_to_hand_from_oracle(P0, "Inspired Charge", true, INSPIRED_CHARGE)
            .id();
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        {
            runner.cast(charge).commit();
        }
        let order: Vec<(ObjectId, PlayerId)> = runner
            .state()
            .stack
            .iter()
            .map(|entry| (entry.id, entry.controller))
            .collect();
        assert_eq!(order.len(), 2, "reach guard: both on the stack");
        assert_eq!(
            order[0],
            (pending, P0),
            "reach guard: the AI's pump sits above its own pending animation"
        );
        assert!(
            p0_has_priority(runner.state()),
            "reach guard: the AI holds priority"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    const CONGREGATE: &str = "Target player gains 2 life for each creature on the battlefield.";
    const RETURN_OF_THE_WILDSPEAKER: &str = "Choose one \u{2014}\n\u{2022} Draw cards equal to the greatest power among non-Human creatures you control.\n\u{2022} Non-Human creatures you control get +3/+3 until end of turn.";

    /// `state` once every player has passed until the stack is empty, after
    /// `first` if it is given.
    fn played_out(state: &GameState, first: Option<GameAction>) -> GameState {
        let mut line = state.clone();
        if let Some(action) = first {
            apply_as_current_for_simulation(&mut line, action).expect("the engine accepts it");
        }
        for _ in 0..16 {
            if line.stack.is_empty() {
                return line;
            }
            apply_as_current_for_simulation(&mut line, GameAction::PassPriority)
                .expect("the player with priority passes");
        }
        panic!("the stack did not empty: {:?}", line.waiting_for);
    }

    fn activate(source_id: ObjectId, ability_index: usize) -> GameAction {
        GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        }
    }

    /// CR 608.2h: Congregate counts the creatures as it resolves, so the land
    /// counts only if a repeat resolved first.
    #[test]
    fn a_life_gain_counting_creatures_above_the_pending_animation_leaves_the_repeat_eligible() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::Green);
            scenario.add_basic_land(P0, ManaColor::White);
        }
        let congregate = scenario
            .add_spell_to_hand_from_oracle(P0, "Congregate", true, CONGREGATE)
            .id();
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        {
            runner.cast(congregate).target_player(P0).commit();
        }
        let top = runner.state().stack.back().unwrap().id;
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![pending, top],
            "reach guard: Congregate sits above the animation"
        );
        assert!(
            p0_has_priority(runner.state()),
            "reach guard: the AI holds priority"
        );
        let without = played_out(runner.state(), None);
        let with = played_out(runner.state(), Some(activate(village, index)));
        assert_eq!(
            (without.players[0].life, with.players[0].life),
            (20, 22),
            "reach guard: the repeat is worth 2 life"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 608.2h: the first mode reads the greatest power as it resolves, and
    /// the land is a non-Human creature only if a repeat resolved first.
    #[test]
    fn a_draw_reading_power_above_the_pending_animation_leaves_the_repeat_eligible() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..10 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let wildspeaker = scenario
            .add_spell_to_hand_from_oracle(
                P0,
                "Return of the Wildspeaker",
                true,
                RETURN_OF_THE_WILDSPEAKER,
            )
            .id();
        for _ in 0..4 {
            scenario.add_card_to_library_top(P0, "Forest");
        }
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, village)[0];
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        {
            runner.cast(wildspeaker).modes(&[0]).commit();
        }
        let top = runner.state().stack.back().unwrap().id;
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![pending, top],
            "reach guard: the spell sits above the animation"
        );
        let hand = |state: &GameState| state.players[0].hand.len();
        let without = played_out(runner.state(), None);
        let with = played_out(runner.state(), Some(activate(village, index)));
        assert_eq!(
            hand(&with),
            hand(&without) + 3,
            "reach guard: the repeat is worth three cards"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 405.5: Bramblecrush below the animation resolves after it, so the
    /// land is already a creature by then and a response changes nothing.
    #[test]
    fn noncreature_removal_below_the_pending_animation_keeps_the_veto() {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let village = scenario
            .add_land_from_oracle(P0, "Treetop Village", TREETOP_VILLAGE)
            .id();
        for _ in 0..8 {
            scenario.add_basic_land(P0, ManaColor::Green);
        }
        let quicken = scenario
            .add_spell_to_hand_from_oracle(P1, "Quicken", true, QUICKEN)
            .id();
        let bramblecrush = scenario
            .add_spell_to_hand_from_oracle(P1, "Bramblecrush", false, BRAMBLECRUSH)
            .id();
        scenario.add_basic_land(P1, ManaColor::Blue);
        for _ in 0..4 {
            scenario.add_basic_land(P1, ManaColor::Green);
        }
        for _ in 0..3 {
            scenario.add_card_to_library_top(P1, "Island");
        }
        let mut runner = scenario.build();
        {
            let s = runner.state_mut();
            s.turn_number = 3;
            s.active_player = P0;
            s.phase = Phase::PreCombatMain;
            s.waiting_for = WaitingFor::Priority { player: P0 };
        }
        let index = animation_indices(&runner, village)[0];
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("the AI passes priority");
        {
            runner.cast(quicken).commit();
        }
        pass_until(&mut runner, |s| s.stack.is_empty() && p0_has_priority(s));
        apply_as_current_for_simulation(runner.state_mut(), GameAction::PassPriority)
            .expect("the AI passes priority");
        {
            runner.cast(bramblecrush).target_object(village).commit();
        }
        let removal = runner.state().stack.back().unwrap().id;
        pass_until(&mut runner, p0_has_priority);
        announce(&mut runner, village, index);
        let pending = runner.state().stack.back().unwrap().id;
        assert_eq!(
            runner
                .state()
                .stack
                .iter()
                .map(|e| e.id)
                .collect::<Vec<_>>(),
            vec![removal, pending],
            "reach guard: the sorcery sits below the animation"
        );
        assert_eq!(
            reach_of(runner.state(), removal),
            vec![vec![TargetRef::Object(village)]],
            "reach guard: it destroys the noncreature land"
        );
        let verdict = verdict_at(runner.state(), village, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    const ELROND_MOON_READER: &str = "Whenever you activate an ability of a creature, draw a card. This ability triggers only once each turn.\n{5}{U}{U}: Exile up to two other target nonland permanents you control. Return those cards to the battlefield under their owner's control at the beginning of the next end step.";
    const CRACKDOWN_CONSTRUCT: &str = "Whenever you activate an ability of an artifact or creature that isn't a mana ability, this creature gets +1/+1 until end of turn.";

    /// Mutavault animated by a resolved activation, with P0 holding priority
    /// over an empty stack.
    fn animated_mutavault(
        add: impl FnOnce(&mut GameScenario) -> ObjectId,
    ) -> (GameRunner, ObjectId, usize, ObjectId) {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        let vault = scenario
            .add_land_from_oracle(P0, "Mutavault", MUTAVAULT)
            .id();
        for _ in 0..4 {
            scenario.add_basic_land(P0, ManaColor::Blue);
        }
        for _ in 0..3 {
            scenario.add_card_to_library_top(P0, "Island");
        }
        let added = add(&mut scenario);
        let mut runner = scenario.build();
        p0_main_phase(&mut runner);
        let index = animation_indices(&runner, vault)[0];
        announce(&mut runner, vault, index);
        resolve_accepting_optional(&mut runner);
        assert!(
            is_creature(runner.state(), vault) && runner.state().stack.is_empty(),
            "reach guard: Mutavault is a creature and nothing is pending"
        );
        (runner, vault, index, added)
    }

    /// CR 603.2: once Mutavault is a creature, activating the
    /// repeat is the event Elrond, Moon-Reader triggers on.
    #[test]
    fn a_draw_triggered_by_activating_the_repeat_leaves_it_eligible() {
        let (runner, vault, index, _) = animated_mutavault(|scenario| {
            scenario
                .add_creature_from_oracle(P0, "Elrond, Moon-Reader", 3, 3, ELROND_MOON_READER)
                .as_legendary()
                .id()
        });
        let hand = |state: &GameState| state.players[0].hand.len();
        let without = played_out(runner.state(), None);
        let with = played_out(runner.state(), Some(activate(vault, index)));
        assert_eq!(
            hand(&with),
            hand(&without) + 1,
            "reach guard: activating the repeat draws a card"
        );
        let verdict = verdict_at(runner.state(), vault, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// CR 603.2: once Mutavault is a creature, activating the
    /// repeat is the event Crackdown Construct triggers on.
    #[test]
    fn a_pump_triggered_by_activating_the_repeat_leaves_it_eligible() {
        let (runner, vault, index, construct) = animated_mutavault(|scenario| {
            scenario
                .add_creature_from_oracle(P0, "Crackdown Construct", 2, 2, CRACKDOWN_CONSTRUCT)
                .as_artifact_creature()
                .id()
        });
        let power = |state: &GameState| state.objects[&construct].power;
        let without = played_out(runner.state(), None);
        let with = played_out(runner.state(), Some(activate(vault, index)));
        assert_eq!(
            (power(&without), power(&with)),
            (Some(2), Some(3)),
            "reach guard: activating the repeat pumps the Construct"
        );
        let verdict = verdict_at(runner.state(), vault, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    const HARSH_MENTOR: &str = "Whenever an opponent activates an ability of an artifact, creature, or land on the battlefield, if it isn't a mana ability, this creature deals 2 damage to that player.";

    /// Harsh Mentor's trigger only costs P0 life, and the repeat stays
    /// eligible all the same: the check abstains on any entry the activation
    /// adds besides the repeat.
    #[test]
    fn a_punisher_triggered_by_activating_the_repeat_leaves_it_eligible() {
        let (runner, vault, index, _) = animated_mutavault(|scenario| {
            scenario
                .add_creature_from_oracle(P1, "Harsh Mentor", 2, 2, HARSH_MENTOR)
                .id()
        });
        let life = |state: &GameState| state.players[0].life;
        let without = played_out(runner.state(), None);
        let with = played_out(runner.state(), Some(activate(vault, index)));
        assert_eq!(
            life(&with),
            life(&without) - 2,
            "reach guard: activating the repeat costs P0 2 life"
        );
        let verdict = verdict_at(runner.state(), vault, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    #[test]
    fn two_activation_triggers_leave_the_repeat_eligible() {
        let (runner, vault, index, _) = animated_mutavault(|scenario| {
            scenario
                .add_creature_from_oracle(P0, "Crackdown Construct", 2, 2, CRACKDOWN_CONSTRUCT)
                .as_artifact_creature();
            scenario
                .add_creature_from_oracle(P0, "Elrond, Moon-Reader", 3, 3, ELROND_MOON_READER)
                .as_legendary()
                .id()
        });
        let mut activated = runner.state().clone();
        apply_as_current_for_simulation(&mut activated, activate(vault, index))
            .expect("the engine accepts it");
        assert!(
            matches!(activated.waiting_for, WaitingFor::OrderTriggers { .. }),
            "reach guard: the two triggers wait to be ordered, got {:?}",
            activated.waiting_for
        );
        let verdict = verdict_at(runner.state(), vault, index);
        assert!(
            matches!(verdict, PolicyVerdict::Score { .. }),
            "got {verdict:?}"
        );
    }

    /// Elrond, Moon-Reader has already triggered this turn, so a further
    /// repeat triggers nothing.
    #[test]
    fn an_activation_trigger_already_spent_this_turn_keeps_the_veto() {
        let (mut runner, vault, index, _) = animated_mutavault(|scenario| {
            scenario
                .add_creature_from_oracle(P0, "Elrond, Moon-Reader", 3, 3, ELROND_MOON_READER)
                .as_legendary()
                .id()
        });
        let hand = |state: &GameState| state.players[0].hand.len();
        let before = hand(runner.state());
        *runner.state_mut() = played_out(runner.state(), Some(activate(vault, index)));
        p0_main_phase(&mut runner);
        assert_eq!(
            hand(runner.state()),
            before + 1,
            "reach guard: the first repeat drew a card"
        );
        let without = played_out(runner.state(), None);
        let with = played_out(runner.state(), Some(activate(vault, index)));
        assert_eq!(
            hand(&with),
            hand(&without),
            "reach guard: a second repeat draws nothing"
        );
        let verdict = verdict_at(runner.state(), vault, index);
        assert!(is_redundant_reject(&verdict), "got {verdict:?}");
    }

    #[test]
    fn a_redundant_repeat_is_vetoed_only_at_the_root() {
        let (mut runner, vault, index) =
            oracle_board("Mutavault", MUTAVAULT, &[(ManaColor::Green, 5)]);
        announce(&mut runner, vault, index);
        resolve_accepting_optional(&mut runner);
        let root = verdict_at(runner.state(), vault, index);
        assert!(is_redundant_reject(&root), "reach guard: got {root:?}");
        let lookahead = verdict_at_depth(runner.state(), vault, index, SearchDepth::Lookahead);
        assert!(
            matches!(lookahead, PolicyVerdict::Score { .. }),
            "got {lookahead:?}"
        );
    }
}

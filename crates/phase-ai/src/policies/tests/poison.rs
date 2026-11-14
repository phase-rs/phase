//! Unit tests for `policies::poison` — the CR 104.3d poison-clock policy.
//! No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! The `verdict` path runs against a real `PolicyContext` built over a
//! multiplayer `GameState`, mirroring the `energy_payoff` policy-test shape.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::combat::AttackTarget;
use engine::game::zones::create_object;
use engine::types::ability::{
    AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, Effect, ModalChoice, QuantityExpr,
    TargetFilter, TargetRef, TypedFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::counter::CounterType;
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, LayersDirty, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::{ManaColor, ManaCost, ManaCostShard, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::poison::POISON_CLOCK_FLOOR;
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

use crate::policies::poison::*;
use crate::test_support::shared_integration_card_db;
use engine::game::players::{PoisonLossMeasure, PoisonLossScope};
use engine::game::scenario::{GameRunner, GameScenario};
use engine::game::scenario_db::GameScenarioDbExt;

const AI: PlayerId = PlayerId(0);
const OPPONENT: PlayerId = PlayerId(1);

// ─── fixtures ───────────────────────────────────────────────────────────────

fn state_with_players(count: u8) -> GameState {
    GameState::new(FormatConfig::standard(), count, 42)
}

fn config() -> AiConfig {
    AiConfig::default()
}

fn ai_context(config: &AiConfig) -> AiContext {
    let mut context = AiContext::empty(&config.weights);
    context.player = AI;
    context
}

fn priority_decision() -> AiDecisionContext {
    AiDecisionContext {
        waiting_for: WaitingFor::Priority { player: AI },
        candidates: Vec::new(),
    }
}

fn ctx<'a>(
    state: &'a GameState,
    candidate: &'a CandidateAction,
    decision: &'a AiDecisionContext,
    context: &'a AiContext,
    config: &'a AiConfig,
) -> PolicyContext<'a> {
    PolicyContext {
        state,
        decision,
        candidate,
        ai_player: AI,
        config,
        context,
        cast_facts: None,
        search_depth: SearchDepth::Root,
    }
}

fn poison_effect(target: TargetFilter) -> Effect {
    Effect::GivePlayerCounter {
        counter_kind: PlayerCounterKind::Poison,
        count: QuantityExpr::Fixed { value: 1 },
        target,
    }
}

/// "target opponent" as the parser types it (Pulse of the Hunter Maze).
fn target_opponent() -> TargetFilter {
    TargetFilter::Typed(TypedFilter::default().controller(ControllerRef::Opponent))
}

fn draw_effect() -> Effect {
    Effect::Draw {
        count: QuantityExpr::Fixed { value: 1 },
        target: TargetFilter::Controller,
    }
}

/// A spell object in hand carrying `abilities`.
fn spell_object(state: &mut GameState, idx: u64, abilities: Vec<AbilityDefinition>) -> ObjectId {
    let oid = create_object(state, CardId(idx), AI, format!("Spell {idx}"), Zone::Hand);
    let object = state.objects.get_mut(&oid).unwrap();
    // The engine casts only the card the candidate names.
    object.card_id = CardId(oid.0);
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Instant],
        subtypes: Vec::new(),
    };
    *Arc::make_mut(&mut object.abilities) = abilities;
    // The hand-built characteristics are the settled ones.
    state.layers_dirty = LayersDirty::Clean;
    oid
}

/// A battlefield creature with `keywords` and `power`.
fn creature_object(
    state: &mut GameState,
    idx: u64,
    keywords: Vec<Keyword>,
    power: i32,
) -> ObjectId {
    let oid = create_object(
        state,
        CardId(idx),
        AI,
        format!("Creature {idx}"),
        Zone::Battlefield,
    );
    let object = state.objects.get_mut(&oid).unwrap();
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Creature],
        subtypes: Vec::new(),
    };
    object.keywords = keywords;
    object.power = Some(power);
    object.toughness = Some(power.max(1));
    state.layers_dirty = LayersDirty::Clean;
    oid
}

fn cast_candidate(object_id: ObjectId) -> CandidateAction {
    CandidateAction {
        action: GameAction::CastSpell {
            object_id,
            card_id: CardId(object_id.0),
            targets: Vec::new(),
            payment_mode: CastPaymentMode::default(),
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Spell),
    }
}

fn attack_candidate(attacks: Vec<(ObjectId, AttackTarget)>) -> CandidateAction {
    CandidateAction {
        action: GameAction::DeclareAttackers {
            attacks,
            bands: Vec::new(),
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Attack),
    }
}

fn select_modes_candidate(indices: Vec<usize>) -> CandidateAction {
    CandidateAction {
        action: GameAction::SelectModes { indices },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Selection),
    }
}

fn activate_ability_candidate(source_id: ObjectId, ability_index: usize) -> CandidateAction {
    CandidateAction {
        action: GameAction::ActivateAbility {
            source_id,
            ability_index,
            payment_mode: CastPaymentMode::Auto,
        },
        metadata: ActionMetadata::for_actor(Some(AI), TacticalClass::Ability),
    }
}

/// Unwrap a `Score` verdict into `(delta, reason)`; fail on `Reject`.
fn score_of(verdict: PolicyVerdict) -> (f64, PolicyReason) {
    match verdict {
        PolicyVerdict::Score { delta, reason } => (delta, reason),
        PolicyVerdict::Reject { reason } => panic!("unexpected Reject: {reason:?}"),
    }
}

fn fact(reason: &PolicyReason, key: &str) -> Option<i64> {
    reason
        .facts
        .iter()
        .find(|(name, _)| *name == key)
        .map(|(_, value)| *value)
}

// ─── helper boundaries ──────────────────────────────────────────────────────

fn measure(count: u32, threshold: u32) -> PoisonLossMeasure {
    PoisonLossMeasure {
        scope: PoisonLossScope::Player,
        count,
        threshold,
    }
}

/// CR 704.5c / CR 704.6b: reaching the measure's threshold is lethal, one short is not.
#[test]
fn reaches_lethal_matches_the_measure_threshold() {
    assert!(reaches_lethal(measure(9, 10), 1));
    assert!(!reaches_lethal(measure(8, 10), 1));
    assert!(reaches_lethal(measure(4, 10), 6));
    assert!(!reaches_lethal(measure(9, 10), 0));
    assert!(!reaches_lethal(measure(9, 15), 1));
    assert!(reaches_lethal(measure(14, 15), 1));
    assert!(reaches_lethal(measure(4, 5), 1));
    assert!(!reaches_lethal(measure(3, 5), 1));
    assert!(
        reaches_lethal(measure(u32::MAX, 10), 1),
        "saturating add must not wrap"
    );
}

#[test]
fn activation_opts_out_below_floor() {
    let mut features = DeckFeatures::default();
    features.poison.commitment = POISON_CLOCK_FLOOR - 0.01;
    let state = state_with_players(2);
    assert!(PoisonClockPolicy
        .activation(&features, &state, AI)
        .is_none());
}

#[test]
fn activation_opts_in_above_floor() {
    let mut features = DeckFeatures::default();
    features.poison.commitment = 0.9;
    let state = state_with_players(2);
    assert_eq!(
        PoisonClockPolicy.activation(&features, &state, AI),
        Some(0.9)
    );
}

#[test]
fn opponent_poison_seats_ignores_the_ai_itself() {
    let mut state = state_with_players(2);
    state.players[0].poison_counters = 7;
    state.players[1].poison_counters = 3;
    let seats = |me| {
        opponent_poison_seats(&state, me)
            .map(|(seat, m)| (seat, m.count))
            .collect::<Vec<_>>()
    };
    // The AI's own 7 poison must not be read as pressure it is applying.
    assert_eq!(seats(AI), vec![(OPPONENT, 3)]);
    assert_eq!(seats(OPPONENT), vec![(AI, 7)]);
}

/// CR 800.4: a multiplayer game continues after a player leaves, and the
/// eliminated seat stays in
/// `GameState.players`. Their counters must not produce phantom pressure.
#[test]
fn opponent_poison_seats_ignores_eliminated_seats() {
    let mut state = state_with_players(4);
    state.players[1].poison_counters = 9;
    state.players[1].is_eliminated = true;
    state.players[2].poison_counters = 2;
    state.players[3].poison_counters = 0;

    assert_eq!(
        opponent_poison_seats(&state, AI)
            .map(|(seat, m)| (seat, m.count))
            .collect::<Vec<_>>(),
        vec![(PlayerId(2), 2), (PlayerId(3), 0)],
        "a dead seat at 9 poison must not read as one counter from lethal"
    );
    assert_eq!(live_opponent_poison(&state, AI, PlayerId(1)), None);
    assert_eq!(
        live_opponent_poison(&state, AI, PlayerId(2)).map(|m| m.count),
        Some(2)
    );
    assert_eq!(
        live_opponent_poison(&state, AI, AI),
        None,
        "not an opponent"
    );
}

// ─── verdict: direct poison and proliferate ─────────────────────────────────

/// CR 104.3d: an opponent at nine is one counter from losing, so a direct
/// poison spell is a game-ending play, not a value play.
#[test]
fn verdict_scores_lethal_direct_poison_as_critical() {
    let config = config();
    let context = ai_context(&config);
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let spell = spell_object(
        &mut state,
        1,
        vec![AbilityDefinition::new(
            AbilityKind::Spell,
            poison_effect(target_opponent()),
        )],
    );

    let candidate = cast_candidate(spell);
    let decision = priority_decision();
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));

    assert_eq!(reason.kind, "poison_clock_lethal");
    assert_eq!(delta, config.policy_penalties.poison_clock_pressure);
    assert!(
        delta > crate::policies::registry::STRONG_MAX,
        "a game-ending play must land in the critical band"
    );
}

/// Below lethal the value scales with how far the clock has already run — the
/// last counters are worth more than the first.
///
/// Swept across every reachable clock value rather than sampled, because the
/// delta is state-dependent: a magnitude that escapes its declared band trips
/// `score_in_band`'s debug assert and silently clamps in release, flattening
/// the very progress signal this policy exists to provide. The sub-lethal
/// ceiling must also stay under `STRONG_MAX`, so that only an action reaching
/// CR 104.3d's ten can outrank one that does.
#[test]
fn verdict_scales_direct_poison_by_clock_progress() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();

    let score_at = |existing: u32| {
        let mut state = state_with_players(2);
        state.players[1].poison_counters = existing;
        let spell = spell_object(
            &mut state,
            1,
            vec![AbilityDefinition::new(
                AbilityKind::Spell,
                poison_effect(target_opponent()),
            )],
        );
        let candidate = cast_candidate(spell);
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)))
    };

    let mut previous = 0.0;
    let lethal_poison = u32::from(FormatConfig::standard().poison_counter_threshold.unwrap());
    for existing in 0..lethal_poison - 1 {
        let (delta, reason) = score_at(existing);
        assert_eq!(reason.kind, "poison_clock_pressure", "at {existing} poison");
        assert!(
            delta > crate::policies::registry::NUDGE_MAX,
            "advancing the clock is never a mere nudge (at {existing}): {delta}"
        );
        assert!(
            delta <= crate::policies::registry::STRONG_MAX,
            "a non-lethal advance must stay out of the critical band (at {existing}): {delta}"
        );
        assert!(
            delta >= previous,
            "score must not decrease as the clock advances (at {existing}): {delta} < {previous}"
        );
        previous = delta;
    }

    let (lethal_delta, lethal_reason) = score_at(lethal_poison - 1);
    assert_eq!(lethal_reason.kind, "poison_clock_lethal");
    assert!(
        lethal_delta > previous,
        "reaching ten must outrank every non-lethal advance: {lethal_delta} vs {previous}"
    );
}

/// `MIN_CLOCK_PROGRESS` floors the earliest counters onto a flat plateau: at 0
/// and 1 existing poison the projected progress (0.1, 0.2) is below the floor,
/// so both score the SAME delta. Deleting the floor would split them (0.50 vs
/// 1.00), so the equality — not just a magnitude — is what pins the constant.
#[test]
fn verdict_floors_early_clock_progress_onto_a_plateau() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();

    let score_at = |existing: u32| {
        let mut state = state_with_players(2);
        state.players[1].poison_counters = existing;
        let spell = spell_object(
            &mut state,
            1,
            vec![AbilityDefinition::new(
                AbilityKind::Spell,
                poison_effect(target_opponent()),
            )],
        );
        let candidate = cast_candidate(spell);
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)))
            .0
    };

    let at_zero = score_at(0);
    let at_one = score_at(1);
    assert_eq!(
        at_zero, at_one,
        "0 and 1 existing poison both sit on the MIN_CLOCK_PROGRESS floor"
    );
    // STRONG_MAX (the sub-lethal ceiling) × the 0.25 floor.
    assert!(
        (at_zero - crate::policies::registry::STRONG_MAX * 0.25).abs() < 1e-9,
        "plateau delta must be STRONG_MAX × 0.25, got {at_zero}"
    );
    // And the plateau ends: 2 existing poison (progress 0.3) clears the floor.
    assert!(
        score_at(2) > at_one,
        "the third counter must clear the floor and score higher"
    );
}

/// CR 701.34a: proliferate chooses among permanents and players that ALREADY
/// have a counter. With no poisoned opponent it advances nothing.
#[test]
fn verdict_declines_proliferate_with_no_poisoned_opponent() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    let spell = spell_object(
        &mut state,
        1,
        vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Proliferate,
        )],
    );

    let candidate = cast_candidate(spell);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_no_counters_to_proliferate");
    assert_eq!(delta, 0.0);
}

/// The same proliferate becomes real value once the clock has started.
#[test]
fn verdict_rewards_proliferate_on_a_poisoned_opponent() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 4;
    let spell = spell_object(
        &mut state,
        1,
        vec![AbilityDefinition::new(
            AbilityKind::Spell,
            Effect::Proliferate,
        )],
    );

    let candidate = cast_candidate(spell);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_pressure");
    assert!(delta > 0.0);
}

#[test]
fn verdict_is_neutral_for_an_unrelated_spell() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let spell = spell_object(
        &mut state,
        1,
        vec![AbilityDefinition::new(AbilityKind::Spell, draw_effect())],
    );

    let candidate = cast_candidate(spell);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

// ─── verdict: the modal seam (CR 601.2b / CR 700.2) ─────────────────────────

/// Two-mode activated ability: mode 0 draws, mode 1 poisons.
fn modal_ability() -> AbilityDefinition {
    let mut ability = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GenericEffect {
            static_abilities: Vec::new(),
            duration: None,
            target: None,
            end_cost: None,
        },
    );
    ability.modal = Some(ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        ..ModalChoice::default()
    });
    ability.mode_abilities = vec![
        AbilityDefinition::new(AbilityKind::Activated, draw_effect()),
        AbilityDefinition::new(AbilityKind::Activated, poison_effect(target_opponent())),
    ];
    ability
}

/// The mode-choice decision `action` opens when the engine applies it to `state`.
fn mode_choice_after(state: &mut GameState, action: GameAction) -> AiDecisionContext {
    engine::game::engine::apply_as_current_for_simulation(state, action).unwrap();
    assert!(matches!(
        state.waiting_for,
        WaitingFor::ModeChoice { .. } | WaitingFor::AbilityModeChoice { .. }
    ));
    AiDecisionContext {
        waiting_for: state.waiting_for.clone(),
        candidates: Vec::new(),
    }
}

/// The opponent at nine poison and the AI at the mode choice of a creature's `modal_ability`.
fn modal_ability_board() -> (GameState, AiDecisionContext) {
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let source = creature_object(&mut state, 1, Vec::new(), 1);
    *Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities) = vec![modal_ability()];
    let decision = mode_choice_after(&mut state, activate_ability_candidate(source, 0).action);
    (state, decision)
}

/// CR 601.2b: the poison mode is scored at the seam where it is chosen.
#[test]
fn verdict_scores_the_selected_poison_mode() {
    let config = config();
    let context = ai_context(&config);
    let (state, decision) = modal_ability_board();

    let candidate = select_modes_candidate(vec![1]);
    let (_, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_lethal");
}

/// The non-poison mode of the SAME ability must score nothing — the modes are
/// discriminated, not the card.
#[test]
fn verdict_ignores_a_selected_non_poison_mode() {
    let config = config();
    let context = ai_context(&config);
    let (state, decision) = modal_ability_board();

    let candidate = select_modes_candidate(vec![0]);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

/// CR 601.2b: announcing a modal spell chooses no mode yet, so the cast must
/// not be credited with a mode's poison — that would score a branch the AI may
/// never take.
#[test]
fn verdict_does_not_credit_a_modal_cast_before_mode_selection() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;

    // A modal spell's printed modes are separate spell-kind abilities on the
    // object (engine `modal_spell_mode_abilities`), one of which poisons.
    let spell = spell_object(
        &mut state,
        1,
        vec![
            AbilityDefinition::new(AbilityKind::Spell, draw_effect()),
            AbilityDefinition::new(AbilityKind::Spell, poison_effect(target_opponent())),
        ],
    );
    state.objects.get_mut(&spell).unwrap().modal = Some(ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        ..ModalChoice::default()
    });

    let candidate = cast_candidate(spell);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

/// Control for the previous test: the identical ability list on a NON-modal
/// object is unconditional and does score.
#[test]
fn verdict_credits_a_non_modal_cast_with_the_same_effect() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    create_object(
        &mut state,
        CardId(2),
        AI,
        "Swamp".to_string(),
        Zone::Library,
    );
    let spell = spell_object(
        &mut state,
        1,
        vec![
            AbilityDefinition::new(AbilityKind::Spell, draw_effect()),
            AbilityDefinition::new(AbilityKind::Spell, poison_effect(target_opponent())),
        ],
    );

    let candidate = cast_candidate(spell);
    let (_, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_lethal");
}

// ─── verdict: combat (CR 702.90b / 702.164c / 702.70a) ──────────────────────

/// An infect deck's primary progression action is attacking with its clock.
#[test]
fn verdict_scores_a_poison_source_attacking_a_player() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 3;
    let attacker = creature_object(&mut state, 1, vec![Keyword::Infect], 2);

    let candidate = attack_candidate(vec![(attacker, AttackTarget::Player(OPPONENT))]);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_pressure");
    assert!(delta > 0.0);
    assert_eq!(
        reason
            .facts
            .iter()
            .find(|(key, _)| *key == "poison_added")
            .map(|(_, value)| *value),
        Some(2),
        "CR 702.90b: a 2-power infect attacker converts two counters"
    );
}

/// CR 509.1a: an 8-poison opponent facing a 2-power infect attacker would be
/// dead if the damage connects — but a declared attack can still be blocked or
/// prevented, so the lethal combat swing is held at the `STRONG_MAX` ceiling,
/// strictly below the critical band a guaranteed direct poison reaches.
#[test]
fn verdict_caps_a_lethal_infect_attack_below_critical() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 8;
    let attacker = creature_object(&mut state, 1, vec![Keyword::Infect], 2);

    let candidate = attack_candidate(vec![(attacker, AttackTarget::Player(OPPONENT))]);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_lethal");
    assert_eq!(
        delta,
        crate::policies::registry::STRONG_MAX,
        "combat lethal is capped at STRONG_MAX (the swing can be blocked)"
    );
    assert!(
        delta < config.policy_penalties.poison_clock_pressure,
        "and stays strictly below the guaranteed-direct-poison magnitude"
    );

    // Control: a resolving direct-poison spell that ALSO reaches ten (from 9,
    // +1) is a guaranteed counter, so it may enter the critical band that the
    // blockable combat swing cannot.
    state.players[1].poison_counters = 9;
    let spell = spell_object(
        &mut state,
        9,
        vec![AbilityDefinition::new(
            AbilityKind::Spell,
            poison_effect(target_opponent()),
        )],
    );
    let cast = cast_candidate(spell);
    let (cast_delta, cast_reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &cast, &decision, &context, &config)));
    assert_eq!(cast_reason.kind, "poison_clock_lethal");
    assert!(
        cast_delta > crate::policies::registry::STRONG_MAX,
        "a guaranteed lethal counter reaches critical while a blockable combat swing does not"
    );
}

/// Poison from several attackers pointed at one seat shares that seat's clock.
#[test]
fn verdict_sums_poison_per_defending_player() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 6;
    let first = creature_object(&mut state, 1, vec![Keyword::Infect], 2);
    let second = creature_object(&mut state, 2, vec![Keyword::Toxic(2)], 1);

    let candidate = attack_candidate(vec![
        (first, AttackTarget::Player(OPPONENT)),
        (second, AttackTarget::Player(OPPONENT)),
    ]);
    let (_, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    // 6 + (2 infect + 2 toxic) == 10.
    assert_eq!(reason.kind, "poison_clock_lethal");
}

/// CR 702.90b / 702.164c / 702.70a all key on combat damage dealt to a PLAYER,
/// so an attack aimed at a planeswalker advances nothing on this axis.
#[test]
fn verdict_ignores_a_poison_source_attacking_a_planeswalker() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let attacker = creature_object(&mut state, 1, vec![Keyword::Infect], 2);
    let walker = creature_object(&mut state, 2, Vec::new(), 0);

    let candidate = attack_candidate(vec![(attacker, AttackTarget::Planeswalker(walker))]);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

/// Negative control: a vanilla beater is not a poison clock.
#[test]
fn verdict_ignores_an_attack_with_no_poison_source() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let attacker = creature_object(&mut state, 1, vec![Keyword::Flying], 5);

    let candidate = attack_candidate(vec![(attacker, AttackTarget::Player(OPPONENT))]);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

/// CR 800.4: attacking a seat that has already left the game advances no clock.
#[test]
fn verdict_ignores_an_attack_on_an_eliminated_seat() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(4);
    state.players[1].poison_counters = 9;
    state.players[1].is_eliminated = true;
    let attacker = creature_object(&mut state, 1, vec![Keyword::Infect], 2);

    let candidate = attack_candidate(vec![(attacker, AttackTarget::Player(PlayerId(1)))]);
    let (delta, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

/// With two live opponents, the seat the declaration pushes closest to ten is
/// the one scored.
#[test]
fn verdict_picks_the_defender_closest_to_lethal() {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    let mut state = state_with_players(3);
    state.players[1].poison_counters = 1;
    state.players[2].poison_counters = 9;
    let first = creature_object(&mut state, 1, vec![Keyword::Infect], 2);
    let second = creature_object(&mut state, 2, vec![Keyword::Infect], 1);

    let candidate = attack_candidate(vec![
        (first, AttackTarget::Player(PlayerId(1))),
        (second, AttackTarget::Player(PlayerId(2))),
    ]);
    let (_, reason) =
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)));
    assert_eq!(
        reason.kind, "poison_clock_lethal",
        "the 9-poison seat is one infect counter from losing"
    );
}

// ─── registry routing (the real pipeline) ───────────────────────────────────

/// An `AiContext` whose cached deck features clear `POISON_CLOCK_FLOOR`, so the
/// registry's activation gate lets the policy run.
fn committed_context(config: &AiConfig) -> AiContext {
    let mut features = DeckFeatures::default();
    features.poison.commitment = 0.9;
    let mut session = AiSession::empty();
    session.features.insert(AI, features);
    let mut context = AiContext::empty(&config.weights);
    context.session = Arc::new(session);
    context.player = AI;
    context
}

/// The poison-clock verdict as the registry produces it, or `None` when the
/// policy did not run at all (wrong `DecisionKind`, or activation opted out).
fn routed_verdict(ctx: &PolicyContext<'_>) -> Option<(f64, PolicyReason)> {
    PolicyRegistry::default()
        .verdicts(ctx)
        .into_iter()
        .find(|(id, _)| *id == PolicyId::PoisonClock)
        .map(|(_, verdict)| score_of(verdict))
}

#[test]
fn registry_registers_the_policy() {
    assert!(PolicyRegistry::default().has_policy(PolicyId::PoisonClock));
}

/// End-to-end routing for the modal seam: `WaitingFor::AbilityModeChoice`
/// classifies to `DecisionKind::ActivateAbility`, the policy declares that
/// kind, and the two mode alternatives of the SAME ability come out
/// discriminated.
#[test]
fn registry_routes_modal_alternatives_to_the_policy() {
    let config = config();
    let context = committed_context(&config);
    let (state, decision) = modal_ability_board();

    let poison_mode = select_modes_candidate(vec![1]);
    let (poison_delta, poison_reason) =
        routed_verdict(&ctx(&state, &poison_mode, &decision, &context, &config))
            .expect("the poison mode must reach the policy through the registry");
    assert_eq!(poison_reason.kind, "poison_clock_lethal");
    assert!(poison_delta > 0.0);

    let draw_mode = select_modes_candidate(vec![0]);
    let (draw_delta, draw_reason) =
        routed_verdict(&ctx(&state, &draw_mode, &decision, &context, &config))
            .expect("the non-poison mode is still routed, it just scores nothing");
    assert_eq!(draw_reason.kind, "poison_clock_na");
    assert_eq!(draw_delta, 0.0);
    assert!(
        poison_delta > draw_delta,
        "the poison mode must outrank its sibling: {poison_delta} vs {draw_delta}"
    );
}

/// End-to-end routing for combat: `WaitingFor::DeclareAttackers` classifies to
/// `DecisionKind::DeclareAttackers`, which the policy now declares — an infect
/// deck's primary progression action reaches it.
#[test]
fn registry_routes_declare_attackers_to_the_policy() {
    let config = config();
    let context = committed_context(&config);
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 4;
    let attacker = creature_object(&mut state, 1, vec![Keyword::Infect], 2);
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::DeclareAttackers {
            player: AI,
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        },
        candidates: Vec::new(),
    };

    let attacking = attack_candidate(vec![(attacker, AttackTarget::Player(OPPONENT))]);
    let (delta, reason) = routed_verdict(&ctx(&state, &attacking, &decision, &context, &config))
        .expect("a poison-source attack must reach the policy through the registry");
    assert_eq!(reason.kind, "poison_clock_pressure");
    assert!(delta > 0.0);

    let staying_home = attack_candidate(Vec::new());
    let (idle_delta, idle_reason) =
        routed_verdict(&ctx(&state, &staying_home, &decision, &context, &config))
            .expect("declining to attack is still routed");
    assert_eq!(idle_reason.kind, "poison_clock_na");
    assert!(
        delta > idle_delta,
        "attacking with the clock must outrank holding it back"
    );
}

/// The activation gate is what keeps this policy off every non-poison deck:
/// with commitment below the floor the registry never invokes it, even on the
/// exact board that would otherwise score critical.
#[test]
fn registry_skips_the_policy_for_an_uncommitted_deck() {
    let config = config();
    let context = ai_context(&config); // no cached features → commitment 0.0
    let decision = priority_decision();
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let spell = spell_object(
        &mut state,
        1,
        vec![AbilityDefinition::new(
            AbilityKind::Spell,
            poison_effect(target_opponent()),
        )],
    );

    let candidate = cast_candidate(spell);
    assert!(
        routed_verdict(&ctx(&state, &candidate, &decision, &context, &config)).is_none(),
        "below POISON_CLOCK_FLOOR the policy must not run at all"
    );
}

/// End-to-end routing for the policy's PRIMARY seam: a direct-poison
/// `CastSpell` under `WaitingFor::Priority` classifies to
/// `DecisionKind::CastSpell`, which the policy declares. Without this the
/// `CastSpell` entry in `decision_kinds()` could be deleted with the whole
/// suite still green — the seam would be dead in production while every
/// direct-poison test still passed by calling `verdict()` directly.
#[test]
fn registry_routes_cast_spell_to_the_policy() {
    let config = config();
    let context = committed_context(&config);
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let spell = spell_object(
        &mut state,
        1,
        vec![AbilityDefinition::new(
            AbilityKind::Spell,
            poison_effect(target_opponent()),
        )],
    );
    let decision = priority_decision();

    let candidate = cast_candidate(spell);
    let (delta, reason) = routed_verdict(&ctx(&state, &candidate, &decision, &context, &config))
        .expect("a direct-poison cast must reach the policy through the registry");
    assert_eq!(reason.kind, "poison_clock_lethal");
    assert!(delta > 0.0);

    // A non-poison cast is still routed under the same kind; it just scores nil.
    let inert = spell_object(
        &mut state,
        2,
        vec![AbilityDefinition::new(AbilityKind::Spell, draw_effect())],
    );
    let (inert_delta, inert_reason) = routed_verdict(&ctx(
        &state,
        &cast_candidate(inert),
        &decision,
        &context,
        &config,
    ))
    .expect("a non-poison cast is still routed under DecisionKind::CastSpell");
    assert_eq!(inert_reason.kind, "poison_clock_na");
    assert!(
        delta > inert_delta,
        "the poison cast must outrank an inert one through the pipeline"
    );
}

/// End-to-end routing for an activated poison ability: an
/// `ActivateAbility` action under `WaitingFor::Priority` classifies to
/// `DecisionKind::ActivateAbility`, exercising the `obj.abilities.get(index)`
/// lookup that `verdict()`-direct tests bypass.
#[test]
fn registry_routes_activate_ability_to_the_policy() {
    let config = config();
    let context = committed_context(&config);
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    // A permanent with two activated abilities: index 0 draws, index 1 poisons.
    let source = creature_object(&mut state, 1, Vec::new(), 1);
    *Arc::make_mut(&mut state.objects.get_mut(&source).unwrap().abilities) = vec![
        AbilityDefinition::new(AbilityKind::Activated, draw_effect()),
        AbilityDefinition::new(AbilityKind::Activated, poison_effect(target_opponent())),
    ];
    let decision = priority_decision();

    let (poison_delta, poison_reason) = routed_verdict(&ctx(
        &state,
        &activate_ability_candidate(source, 1),
        &decision,
        &context,
        &config,
    ))
    .expect("the poison ability must reach the policy through the registry");
    assert_eq!(poison_reason.kind, "poison_clock_lethal");
    assert!(poison_delta > 0.0);

    // The draw ability on the same object is routed but scores nothing — the
    // per-ability `ability_index` lookup is discriminating, not the object.
    let (draw_delta, draw_reason) = routed_verdict(&ctx(
        &state,
        &activate_ability_candidate(source, 0),
        &decision,
        &context,
        &config,
    ))
    .expect("the non-poison ability is still routed");
    assert_eq!(draw_reason.kind, "poison_clock_na");
    assert_eq!(draw_delta, 0.0);

    // An out-of-range index degrades to neutral, never panics.
    let (oob_delta, oob_reason) = routed_verdict(&ctx(
        &state,
        &activate_ability_candidate(source, 9),
        &decision,
        &context,
        &config,
    ))
    .expect("an out-of-range ability index is still routed");
    assert_eq!(oob_reason.kind, "poison_clock_na");
    assert_eq!(oob_delta, 0.0);
}

/// End-to-end routing for a modal SPELL (as opposed to the modal *ability*
/// covered above): `WaitingFor::ModeChoice` carries a `PendingCast`, and the
/// policy reads the chosen mode from the spell object's spell-kind abilities via
/// `modal_spell_mode_ability_refs` — the sole production consumer of that new
/// engine API. The poison and non-poison modes come out discriminated.
#[test]
fn registry_routes_modal_spell_mode_choice_to_the_policy() {
    let config = config();
    let context = committed_context(&config);
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;

    // A modal spell: its two printed modes are spell-kind abilities on the
    // object (mode 0 draws, mode 1 poisons).
    let spell = spell_object(
        &mut state,
        1,
        vec![
            AbilityDefinition::new(AbilityKind::Spell, draw_effect()),
            AbilityDefinition::new(AbilityKind::Spell, poison_effect(target_opponent())),
        ],
    );
    let modal = ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        ..ModalChoice::default()
    };
    state.objects.get_mut(&spell).unwrap().modal = Some(modal);
    // CR 601.2a: the spell is on the stack while its modes are chosen.
    let decision = mode_choice_after(&mut state, cast_candidate(spell).action);

    let (poison_delta, poison_reason) = routed_verdict(&ctx(
        &state,
        &select_modes_candidate(vec![1]),
        &decision,
        &context,
        &config,
    ))
    .expect("the poison spell-mode must reach the policy through the registry");
    assert_eq!(poison_reason.kind, "poison_clock_lethal");
    assert!(poison_delta > 0.0);

    let (draw_delta, draw_reason) = routed_verdict(&ctx(
        &state,
        &select_modes_candidate(vec![0]),
        &decision,
        &context,
        &config,
    ))
    .expect("the non-poison spell-mode is still routed");
    assert_eq!(draw_reason.kind, "poison_clock_na");
    assert_eq!(draw_delta, 0.0);
}

// ─── configured poison-loss threshold (real cards) ──────────────────────────

const PROLOGUE_TO_PHYRESIS: &str = "Each opponent gets a poison counter.\nDraw a card.";
const LARGEPOX: &str = "Each player discards a card, then loses 1 life, then sacrifices an artifact, a creature, an enchantment, a land, a planeswalker, and a tribal permanent, then exiles a card from their graveyard, then puts the top card of their library into their graveyard, then removes a counter from a permanent they control, then gets a poison counter.";

fn threshold(t: u8) -> FormatConfig {
    FormatConfig {
        poison_counter_threshold: Some(t),
        ..FormatConfig::standard()
    }
}

/// The row's cards are placed by `add` beside a card in the AI's library, then each
/// `(seat, count)` in `poison` is seeded.
fn board(
    format: FormatConfig,
    players: u8,
    poison: &[(PlayerId, u32)],
    add: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GameRunner, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_with_format(format, players, 42);
    scenario.with_library_top(AI, &["Swamp"]);
    let ids = add(&mut scenario);
    let mut runner = scenario.build();
    for &(seat, count) in poison {
        runner.state_mut().players[seat.0 as usize].poison_counters = count;
    }
    engine::game::layers::flush_layers(runner.state_mut());
    (runner, ids)
}

fn glistener_elf(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_real_card(
        AI,
        "Glistener Elf",
        Zone::Battlefield,
        shared_integration_card_db(),
    )
}

fn steady_progress(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_real_card(
        AI,
        "Steady Progress",
        Zone::Hand,
        shared_integration_card_db(),
    )
}

fn prologue_to_phyresis(scenario: &mut GameScenario) -> ObjectId {
    scenario
        .add_spell_to_hand_from_oracle(AI, "Prologue to Phyresis", true, PROLOGUE_TO_PHYRESIS)
        .with_mana_cost(ManaCost::Cost {
            shards: vec![ManaCostShard::Blue],
            generic: 1,
        })
        .id()
}

/// A main phase with {U}{U}{U} floating for the AI, so a cast taps no land.
fn castable(scenario: &mut GameScenario) {
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_mana_pool(AI, floating(&[ManaType::Blue; 3]));
}

fn floating(mana: &[ManaType]) -> Vec<ManaUnit> {
    mana.iter()
        .map(|&kind| ManaUnit::new(kind, ObjectId(0), false, vec![]))
        .collect()
}

fn verdict_on(state: &GameState, candidate: &CandidateAction) -> (f64, PolicyReason) {
    let config = config();
    let context = ai_context(&config);
    let decision = priority_decision();
    score_of(PoisonClockPolicy.verdict(&ctx(state, candidate, &decision, &context, &config)))
}

/// Glistener Elf attacking `defender` on a board of `format` with `poison` seeded.
fn elf_attack(
    format: FormatConfig,
    players: u8,
    poison: &[(PlayerId, u32)],
    defender: PlayerId,
) -> (f64, PolicyReason) {
    let (runner, ids) = board(format, players, poison, |s| vec![glistener_elf(s)]);
    verdict_on(
        runner.state(),
        &attack_candidate(vec![(ids[0], AttackTarget::Player(defender))]),
    )
}

fn cast_on(
    format: FormatConfig,
    players: u8,
    poison: &[(PlayerId, u32)],
    card: fn(&mut GameScenario) -> ObjectId,
) -> (f64, PolicyReason) {
    let (runner, ids) = board(format, players, poison, |s| {
        castable(s);
        vec![card(s)]
    });
    verdict_on(runner.state(), &cast_candidate(ids[0]))
}

/// CR 704.5c: the configured threshold decides lethal; at 15 an opponent at 9
/// is not one infect hit from losing.
#[test]
fn infect_attack_reads_a_raised_threshold() {
    assert_eq!(
        elf_attack(threshold(15), 2, &[(OPPONENT, 9)], OPPONENT)
            .1
            .kind,
        "poison_clock_pressure"
    );
    assert_eq!(
        elf_attack(threshold(15), 2, &[(OPPONENT, 14)], OPPONENT)
            .1
            .kind,
        "poison_clock_lethal"
    );
}

#[test]
fn infect_attack_reads_a_lowered_threshold() {
    let (delta, reason) = elf_attack(threshold(5), 2, &[(OPPONENT, 4)], OPPONENT);
    assert_eq!(reason.kind, "poison_clock_lethal");
    assert_eq!(
        delta,
        config()
            .policy_penalties
            .poison_clock_pressure
            .min(crate::policies::registry::STRONG_MAX)
    );
    assert_eq!(
        elf_attack(threshold(5), 2, &[(OPPONENT, 3)], OPPONENT)
            .1
            .kind,
        "poison_clock_pressure"
    );
}

#[test]
fn infect_attack_keeps_the_default_threshold() {
    let standard = FormatConfig::standard;
    assert_eq!(
        elf_attack(standard(), 2, &[(OPPONENT, 9)], OPPONENT).1.kind,
        "poison_clock_lethal"
    );
    assert_eq!(
        elf_attack(standard(), 2, &[(OPPONENT, 8)], OPPONENT).1.kind,
        "poison_clock_pressure"
    );
}

/// Progress is measured against the configured threshold, so 6 of 15 is less pressure than 6 of 10.
#[test]
fn clock_progress_divides_by_the_configured_threshold() {
    let (at_15, reason_15) = elf_attack(threshold(15), 2, &[(OPPONENT, 5)], OPPONENT);
    let (at_10, reason_10) = elf_attack(threshold(10), 2, &[(OPPONENT, 5)], OPPONENT);
    assert_eq!(reason_15.kind, "poison_clock_pressure");
    assert_eq!(reason_10.kind, "poison_clock_pressure");
    assert!(at_15 < at_10, "{at_15} should be below {at_10}");
}

/// A seat no poison-loss rule applies to has no poison clock to advance.
#[test]
fn no_poison_loss_rule_means_no_clock() {
    let unbounded = || FormatConfig {
        poison_counter_threshold: None,
        ..FormatConfig::standard()
    };
    assert_eq!(
        elf_attack(unbounded(), 2, &[(OPPONENT, 9)], OPPONENT)
            .1
            .kind,
        "poison_clock_na"
    );
    assert_eq!(
        elf_attack(threshold(10), 2, &[(OPPONENT, 9)], OPPONENT)
            .1
            .kind,
        "poison_clock_lethal"
    );
    assert_eq!(
        cast_on(unbounded(), 2, &[(OPPONENT, 9)], prologue_to_phyresis)
            .1
            .kind,
        "poison_clock_no_poison_loss"
    );
}

/// CR 704.6b + CR 810.10: two infect hits on the two heads of one team add to
/// one team total of fifteen.
#[test]
fn two_headed_giant_attack_reads_the_team_total() {
    let two_hg = FormatConfig::two_headed_giant;
    let poison = [(PlayerId(2), 6), (PlayerId(3), 7)];
    let (runner, ids) = board(two_hg(), 4, &poison, |s| {
        vec![glistener_elf(s), glistener_elf(s)]
    });
    let both = attack_candidate(vec![
        (ids[0], AttackTarget::Player(PlayerId(2))),
        (ids[1], AttackTarget::Player(PlayerId(3))),
    ]);
    assert_eq!(
        verdict_on(runner.state(), &both).1.kind,
        "poison_clock_lethal"
    );
    assert_eq!(
        elf_attack(two_hg(), 4, &poison, PlayerId(2)).1.kind,
        "poison_clock_pressure"
    );
}

/// CR 102.3: the AI's teammate is not an opponent, so its team's poison is not
/// pressure the AI applies.
#[test]
fn two_headed_giant_teammate_is_not_an_opponent() {
    let two_hg = FormatConfig::two_headed_giant;
    let poison = [(PlayerId(1), 14)];
    assert_eq!(
        cast_on(two_hg(), 4, &poison, prologue_to_phyresis).1.kind,
        "poison_clock_pressure"
    );
    assert_eq!(
        cast_on(two_hg(), 4, &poison, steady_progress).1.kind,
        "poison_clock_no_counters_to_proliferate"
    );
}

#[test]
fn two_headed_giant_direct_poison_and_proliferate_read_the_team_total() {
    let two_hg = FormatConfig::two_headed_giant;
    let poison = [(PlayerId(2), 7), (PlayerId(3), 7)];
    assert_eq!(
        cast_on(two_hg(), 4, &poison, prologue_to_phyresis).1.kind,
        "poison_clock_lethal"
    );
    assert_eq!(
        cast_on(two_hg(), 4, &poison, steady_progress).1.kind,
        "poison_clock_lethal"
    );
}

/// CR 810.10 + CR 704.6b: each opposing head gets Prologue's counter, so the
/// team total rises by two; CR 701.34b: one proliferate adds one.
#[test]
fn two_headed_giant_each_opponent_poison_counts_both_heads() {
    let two_hg = FormatConfig::two_headed_giant;
    let at_13 = [(PlayerId(2), 6), (PlayerId(3), 7)];
    let (_, prologue) = cast_on(two_hg(), 4, &at_13, prologue_to_phyresis);
    assert_eq!(prologue.kind, "poison_clock_lethal");
    assert_eq!(fact(&prologue, "poison_added"), Some(2));
    assert_eq!(
        cast_on(two_hg(), 4, &at_13, steady_progress).1.kind,
        "poison_clock_pressure"
    );
    let at_12 = [(PlayerId(2), 6), (PlayerId(3), 6)];
    assert_eq!(
        cast_on(two_hg(), 4, &at_12, prologue_to_phyresis).1.kind,
        "poison_clock_pressure"
    );
}

/// CR 704.5c: in a free-for-all each opponent's counter lands on its own clock.
#[test]
fn each_opponent_poison_credits_each_free_for_all_clock_once() {
    let standard = FormatConfig::standard;
    let (_, at_8_8) = cast_on(
        standard(),
        3,
        &[(PlayerId(1), 8), (PlayerId(2), 8)],
        prologue_to_phyresis,
    );
    assert_eq!(at_8_8.kind, "poison_clock_pressure");
    assert_eq!(fact(&at_8_8, "poison_added"), Some(1));
    assert_eq!(
        cast_on(
            standard(),
            3,
            &[(PlayerId(1), 9), (PlayerId(2), 8)],
            prologue_to_phyresis
        )
        .1
        .kind,
        "poison_clock_lethal"
    );
}

#[test]
fn direct_poison_and_proliferate_read_a_raised_threshold() {
    let at_nine = [(OPPONENT, 9)];
    assert_eq!(
        cast_on(threshold(15), 2, &at_nine, prologue_to_phyresis)
            .1
            .kind,
        "poison_clock_pressure"
    );
    assert_eq!(
        cast_on(threshold(15), 2, &at_nine, steady_progress).1.kind,
        "poison_clock_pressure"
    );
    assert_eq!(
        cast_on(threshold(15), 2, &[(OPPONENT, 14)], prologue_to_phyresis)
            .1
            .kind,
        "poison_clock_lethal"
    );
}

/// CR 122.1f: "each opponent gets a poison counter" is direct poison.
#[test]
fn each_opponent_poison_spell_is_direct_poison() {
    let standard = FormatConfig::standard;
    assert_eq!(
        cast_on(standard(), 2, &[(OPPONENT, 9)], prologue_to_phyresis)
            .1
            .kind,
        "poison_clock_lethal"
    );
    assert_eq!(
        cast_on(standard(), 2, &[(OPPONENT, 8)], prologue_to_phyresis)
            .1
            .kind,
        "poison_clock_pressure"
    );
}

/// CR 104.4a: Largepox poisons its caster too, and both players reaching the
/// threshold at once is a draw, not a win.
#[test]
fn each_player_poison_spell_is_not_scored_as_lethal() {
    let poison = [(AI, 9), (OPPONENT, 9)];
    let (runner, ids) = board(threshold(10), 2, &poison, |s| {
        castable(s);
        vec![
            s.add_spell_to_hand_from_oracle(AI, "Largepox", false, LARGEPOX)
                .id(),
            prologue_to_phyresis(s),
        ]
    });
    let state = runner.state();
    let largepox = state.objects.get(&ids[0]).unwrap();
    assert!(
        largepox.abilities.iter().any(|ability| {
            crate::ability_chain::collect_scoped_nodes(
                ability,
                crate::ability_chain::AbilityScope::Unconditional,
            )
            .iter()
            .any(|node| {
                matches!(
                    &*node.effect,
                    Effect::GivePlayerCounter {
                        counter_kind: PlayerCounterKind::Poison,
                        target: TargetFilter::Controller,
                        ..
                    }
                ) && node.player_scope == Some(engine::types::ability::PlayerFilter::All)
            })
        }),
        "Largepox must parse to an each-player poison counter"
    );
    assert_eq!(
        verdict_on(state, &cast_candidate(ids[0])).1.kind,
        "poison_clock_na"
    );
    assert_eq!(
        verdict_on(state, &cast_candidate(ids[1])).1.kind,
        "poison_clock_lethal"
    );
}

// ─── lethal claims read through the engine's replacement pipeline ───────────

fn solemnity(scenario: &mut GameScenario) -> ObjectId {
    scenario.add_real_card(
        PlayerId(1),
        "Solemnity",
        Zone::Battlefield,
        shared_integration_card_db(),
    )
}

fn vorinclex(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario.add_real_card(
        controller,
        "Vorinclex, Monstrous Raider",
        Zone::Battlefield,
        shared_integration_card_db(),
    )
}

/// `card` cast on a board that also holds `extra`.
fn cast_beside(
    format: FormatConfig,
    players: u8,
    poison: &[(PlayerId, u32)],
    card: fn(&mut GameScenario) -> ObjectId,
    extra: fn(&mut GameScenario) -> ObjectId,
) -> (f64, PolicyReason) {
    let (runner, ids) = board(format, players, poison, |s| {
        castable(s);
        let id = card(s);
        extra(s);
        vec![id]
    });
    verdict_on(runner.state(), &cast_candidate(ids[0]))
}

/// CR 101.2 + CR 810.10: under Solemnity neither head gets Prologue's counter, so 13 stays 13.
#[test]
fn prevented_each_opponent_poison_is_not_lethal_in_two_headed_giant() {
    let two_hg = FormatConfig::two_headed_giant;
    let at_13 = [(PlayerId(2), 6), (PlayerId(3), 7)];
    let (open_delta, open) = cast_on(two_hg(), 4, &at_13, prologue_to_phyresis);
    assert_eq!(open.kind, "poison_clock_lethal");
    assert_eq!(fact(&open, "poison_added"), Some(2));
    assert!(open_delta > crate::policies::registry::STRONG_MAX);

    let (delta, reason) = cast_beside(two_hg(), 4, &at_13, prologue_to_phyresis, solemnity);
    assert_eq!(reason.kind, "poison_clock_no_poison_loss");
    assert_eq!(delta, 0.0);
}

/// A sub-lethal estimate is scored without a preview, so it keeps its printed count.
#[test]
fn sub_lethal_poison_estimate_is_not_previewed() {
    let (_, reason) = cast_beside(
        FormatConfig::two_headed_giant(),
        4,
        &[(PlayerId(2), 6), (PlayerId(3), 6)],
        prologue_to_phyresis,
        solemnity,
    );
    assert_eq!(reason.kind, "poison_clock_pressure");
    assert_eq!(fact(&reason, "poison_added"), Some(2));
}

/// CR 101.2 + CR 701.34b: Solemnity stops the team's proliferated counter.
#[test]
fn prevented_proliferate_is_not_lethal() {
    let two_hg = FormatConfig::two_headed_giant;
    let at_14 = [(PlayerId(2), 7), (PlayerId(3), 7)];
    assert_eq!(
        cast_on(two_hg(), 4, &at_14, steady_progress).1.kind,
        "poison_clock_lethal"
    );
    let (delta, reason) = cast_beside(two_hg(), 4, &at_14, steady_progress, solemnity);
    assert_eq!(reason.kind, "poison_clock_no_counters_to_proliferate");
    assert_eq!(delta, 0.0);
}

/// CR 101.2 + CR 120.3b: infect damage to a player under Solemnity gives no poison counters.
#[test]
fn prevented_infect_attack_is_not_lethal() {
    let standard = FormatConfig::standard;
    assert_eq!(
        elf_attack(standard(), 2, &[(OPPONENT, 9)], OPPONENT).1.kind,
        "poison_clock_lethal"
    );
    let (runner, ids) = board(standard(), 2, &[(OPPONENT, 9)], |s| {
        let elf = glistener_elf(s);
        solemnity(s);
        vec![elf]
    });
    let (delta, reason) = verdict_on(
        runner.state(),
        &attack_candidate(vec![(ids[0], AttackTarget::Player(OPPONENT))]),
    );
    assert_eq!(reason.kind, "poison_clock_na");
    assert_eq!(delta, 0.0);
}

/// CR 101.2: a single opponent under Solemnity is not one counter from losing.
#[test]
fn prevented_poison_on_a_single_opponent_is_not_lethal() {
    let standard = FormatConfig::standard;
    assert_eq!(
        cast_on(standard(), 2, &[(OPPONENT, 9)], prologue_to_phyresis)
            .1
            .kind,
        "poison_clock_lethal"
    );
    let (delta, reason) = cast_beside(
        standard(),
        2,
        &[(OPPONENT, 9)],
        prologue_to_phyresis,
        solemnity,
    );
    assert_eq!(reason.kind, "poison_clock_no_poison_loss");
    assert_eq!(delta, 0.0);
}

fn poison_of(state: &GameState, seat: PlayerId) -> u32 {
    state.players[seat.0 as usize].poison_counters
}

/// CR 810.10 + CR 614.1: each head gets Prologue's counter with that head as the actor, so the
/// caster's Vorinclex halves each one to zero; without it both land.
#[test]
fn each_opponent_poison_reads_the_scoped_actor() {
    let at_13 = [(PlayerId(2), 6), (PlayerId(3), 7)];
    for with_vorinclex in [false, true] {
        let (mut runner, ids) = board(FormatConfig::two_headed_giant(), 4, &at_13, |s| {
            let prologue = prologue_to_phyresis(s);
            s.add_basic_land(AI, ManaColor::Blue);
            s.add_basic_land(AI, ManaColor::Blue);
            if with_vorinclex {
                vorinclex(s, AI);
            }
            vec![prologue]
        });
        let (delta, reason) = verdict_on(runner.state(), &cast_candidate(ids[0]));
        runner.cast(ids[0]).resolve();
        let heads = (
            poison_of(runner.state(), PlayerId(2)),
            poison_of(runner.state(), PlayerId(3)),
        );
        if with_vorinclex {
            assert_eq!(heads, (6, 7));
            assert_eq!(reason.kind, "poison_clock_no_poison_loss");
            assert_eq!(delta, 0.0);
        } else {
            assert_eq!(heads, (7, 8));
            assert_eq!(reason.kind, "poison_clock_lethal");
            assert_eq!(fact(&reason, "poison_added"), Some(2));
            assert!(delta > crate::policies::registry::STRONG_MAX);
        }
    }
}

/// Skrelv, Defector Mite: "{W/P}, {T}: Choose a color. Another target creature you control gains
/// toxic 1 and hexproof from that color until end of turn. It can't be blocked by creatures of
/// that color this turn." The keyword below stands in for that resolved grant.
fn grant_skrelv_toxic(runner: &mut GameRunner, creature: ObjectId) {
    let state = runner.state_mut();
    let obj = state.objects.get_mut(&creature).unwrap();
    obj.base_keywords.push(Keyword::Toxic(1));
    obj.keywords.push(Keyword::Toxic(1));
    state.layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(state);
}

/// A standard 2-player board in precombat main with the opponent at nine poison; the opponent
/// controls Vorinclex when `opponent_vorinclex`.
fn combat_board(card: &str, opponent_vorinclex: bool) -> (GameRunner, ObjectId) {
    let (mut runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, 9)], |s| {
        s.at_phase(Phase::PreCombatMain);
        let attacker = s.add_real_card(AI, card, Zone::Battlefield, shared_integration_card_db());
        if opponent_vorinclex {
            vorinclex(s, OPPONENT);
        }
        vec![attacker]
    });
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());
    (runner, ids[0])
}

/// The verdict on `attacker` attacking the opponent at the declare-attackers decision, and the
/// opponent's poison once production combat deals that attack's damage unblocked.
fn attack_then_combat(runner: &mut GameRunner, attacker: ObjectId) -> ((f64, PolicyReason), u32) {
    runner.advance_to_combat();
    let attack = vec![(attacker, AttackTarget::Player(OPPONENT))];
    let verdict = verdict_on(runner.state(), &attack_candidate(attack.clone()));
    runner.declare_attackers(&attack).unwrap();
    runner.combat_damage();
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.declare_blockers(&[]).unwrap();
        runner.combat_damage();
    }
    (verdict, poison_of(runner.state(), OPPONENT))
}

/// CR 120.3b + CR 120.3g: infect and toxic give their counters as separate placements, so the
/// opponent's Vorinclex halves each to zero; without it both land.
#[test]
fn infect_and_toxic_counters_are_separate_placements() {
    for opponent_vorinclex in [false, true] {
        let (mut runner, elf) = combat_board("Glistener Elf", opponent_vorinclex);
        grant_skrelv_toxic(&mut runner, elf);
        let ((delta, reason), poison) = attack_then_combat(&mut runner, elf);
        if opponent_vorinclex {
            assert_eq!(poison, 9);
            assert_eq!(reason.kind, "poison_clock_na");
            assert_eq!(delta, 0.0);
        } else {
            assert_eq!(poison, 11);
            assert_eq!(reason.kind, "poison_clock_lethal");
            assert_eq!(fact(&reason, "poison_added"), Some(2));
            assert!(delta > 0.0 && delta <= crate::policies::registry::STRONG_MAX);
        }
    }
}

/// CR 702.70a: Virulent Sliver's combat damage triggers poisonous 1, whose controller gives the
/// counter, so the opponent's Vorinclex halves it to zero; without it the counter lands.
#[test]
fn poisonous_counters_come_from_the_trigger_controller() {
    for opponent_vorinclex in [false, true] {
        let (mut runner, sliver) = combat_board("Virulent Sliver", opponent_vorinclex);
        let ((delta, reason), poison) = attack_then_combat(&mut runner, sliver);
        if opponent_vorinclex {
            assert_eq!(poison, 9);
            assert_eq!(reason.kind, "poison_clock_na");
            assert_eq!(delta, 0.0);
        } else {
            assert_eq!(poison, 10);
            assert_eq!(reason.kind, "poison_clock_lethal");
            assert_eq!(fact(&reason, "poison_added"), Some(1));
            assert!(delta > 0.0 && delta <= crate::policies::registry::STRONG_MAX);
        }
    }
}

/// `elves` Glistener Elves and Archfiend's Vessel attacking the opponent at `at` poison, with the
/// opponent's Vorinclex and the AI's Alhammarret's Archive and Leyline of Hope as asked: the verdict,
/// and the opponent's poison on the unblocked combat preview (`None` when it stops for a choice).
fn vessel_attack(
    at: u32,
    elves: usize,
    opponent_vorinclex: bool,
    gain_replacements: bool,
) -> ((f64, PolicyReason), Option<u32>) {
    let (mut runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, at)], |s| {
        s.at_phase(Phase::PreCombatMain);
        let db = shared_integration_card_db();
        let mut ids: Vec<_> = (0..elves).map(|_| glistener_elf(s)).collect();
        ids.push(s.add_real_card(AI, "Archfiend's Vessel", Zone::Battlefield, db));
        if opponent_vorinclex {
            vorinclex(s, OPPONENT);
        }
        if gain_replacements {
            s.add_real_card(AI, "Alhammarret's Archive", Zone::Battlefield, db);
            s.add_real_card(AI, "Leyline of Hope", Zone::Battlefield, db);
        }
        ids
    });
    runner.state_mut().layers_dirty.mark_full();
    engine::game::layers::evaluate_layers(runner.state_mut());
    runner.advance_to_combat();
    let attack: Vec<_> = ids
        .iter()
        .map(|&id| (id, AttackTarget::Player(OPPONENT)))
        .collect();
    let at_players: Vec<_> = ids.iter().map(|&id| (id, OPPONENT)).collect();
    let preview = engine::game::combat_damage::preview_unblocked_combat_damage(
        runner.state(),
        &at_players,
        |_| true,
    )
    .map(|after| poison_of(&after, OPPONENT));
    (
        verdict_on(runner.state(), &attack_candidate(attack)),
        preview,
    )
}

/// CR 616.1: Archfiend's Vessel's lifelink gain meets the AI's Alhammarret's Archive and Leyline
/// of Hope, so the combat preview stops for the order choice and never reads whether Glistener
/// Elf's counter survives the opponent's Vorinclex; an unread delivery is pressure, not lethal.
#[test]
fn unread_combat_delivery_is_not_lethal() {
    let ((lethal, lethal_reason), _) = vessel_attack(9, 1, false, false);
    assert_eq!(lethal_reason.kind, "poison_clock_lethal");
    for gain_replacements in [false, true] {
        let ((delta, reason), preview) = vessel_attack(9, 1, true, gain_replacements);
        assert_eq!(preview, (!gain_replacements).then_some(9));
        if gain_replacements {
            assert_eq!(reason.kind, "poison_clock_pressure");
            assert_eq!(fact(&reason, "poison_added"), Some(1));
            assert!(delta > 0.0 && delta < lethal);
        } else {
            assert_eq!(reason.kind, "poison_clock_na");
            assert_eq!(delta, 0.0);
        }
    }
}

/// An unread delivery keeps its estimate and is held one counter short of lethal, so two Elves at
/// eight score the same pressure as one Elf at nine.
#[test]
fn unread_combat_delivery_is_capped_one_short_of_lethal() {
    let ((read, read_reason), read_preview) = vessel_attack(8, 2, false, false);
    assert_eq!(read_preview, Some(10));
    assert_eq!(read_reason.kind, "poison_clock_lethal");

    let ((delta, reason), preview) = vessel_attack(8, 2, false, true);
    assert_eq!(preview, None);
    assert_eq!(reason.kind, "poison_clock_pressure");
    assert_eq!(fact(&reason, "poison_added"), Some(2));
    assert!(delta > 0.0 && delta < read);

    let ((at_nine, _), _) = vessel_attack(9, 1, false, true);
    assert_eq!(delta, at_nine);
}

/// Vorinclex, Monstrous Raider and Halving Season under `controller`'s control.
fn halving_pair(scenario: &mut GameScenario, controller: PlayerId) -> ObjectId {
    scenario.add_real_card(
        controller,
        "Halving Season",
        Zone::Battlefield,
        shared_integration_card_db(),
    );
    vorinclex(scenario, controller)
}

/// CR 616.1: Vorinclex and Halving Season both halve a counter their controller's opponent puts —
/// the opponent itself for Prologue's each-opponent counter, the AI for its proliferate — so the
/// preview stops for the order choice and the unread counter is pressure, not lethal.
#[test]
fn unread_direct_and_proliferate_deliveries_are_not_lethal() {
    let standard = FormatConfig::standard;
    let at_9 = [(OPPONENT, 9)];
    type Place = fn(&mut GameScenario) -> ObjectId;
    let rows: [(Place, Place); 2] = [
        (prologue_to_phyresis, |s| halving_pair(s, AI)),
        (steady_progress, |s| halving_pair(s, OPPONENT)),
    ];
    for (card, replacements) in rows {
        let (lethal, lethal_reason) = cast_on(standard(), 2, &at_9, card);
        assert_eq!(lethal_reason.kind, "poison_clock_lethal");
        let (delta, reason) = cast_beside(standard(), 2, &at_9, card, replacements);
        assert_eq!(reason.kind, "poison_clock_pressure");
        assert_eq!(fact(&reason, "poison_added"), Some(1));
        assert!(delta > 0.0 && delta < lethal);
    }
}

/// CR 107.1b: Vraska's −9 gives the difference up to nine, so its count is resolved, never
/// assumed: zero at nine, four at five, and lethal one at eight under a threshold of nine.
#[test]
fn dynamic_poison_count_is_resolved_before_it_is_scored() {
    for (format, at, kind, added) in [
        (
            FormatConfig::standard(),
            9,
            "poison_clock_no_poison_loss",
            None,
        ),
        (
            FormatConfig::standard(),
            5,
            "poison_clock_pressure",
            Some(4),
        ),
        (threshold(9), 8, "poison_clock_lethal", Some(1)),
    ] {
        let (mut runner, ids) = vraska_board(format, &[(OPPONENT, at)], |_| Vec::new());
        let vraska = ids[0];
        let minus_nine = runner.state().objects[&vraska]
            .abilities
            .iter()
            .position(loyalty(-9))
            .unwrap();
        let (delta, reason) = verdict_on(
            runner.state(),
            &activate_ability_candidate(vraska, minus_nine),
        );
        let _ = runner
            .activate(vraska, minus_nine)
            .target_player(OPPONENT)
            .try_resolve();
        assert_eq!(poison_of(runner.state(), OPPONENT), 9);
        assert!(!runner.state().battlefield.contains(&vraska));
        assert_eq!(reason.kind, kind);
        assert_eq!(fact(&reason, "poison_added"), added);
        match kind {
            "poison_clock_no_poison_loss" => assert_eq!(delta, 0.0),
            "poison_clock_lethal" => assert!(delta > crate::policies::registry::STRONG_MAX),
            _ => assert!(delta > 0.0 && delta <= crate::policies::registry::STRONG_MAX),
        }
    }
}

/// Activating `source`'s first ability that `pick` selects.
fn activation_of(
    state: &GameState,
    source: ObjectId,
    pick: impl Fn(&AbilityDefinition) -> bool,
) -> CandidateAction {
    let index = state.objects[&source]
        .abilities
        .iter()
        .position(pick)
        .unwrap();
    activate_ability_candidate(source, index)
}

fn loyalty(amount: i32) -> impl Fn(&AbilityDefinition) -> bool {
    move |ability| matches!(ability.cost, Some(AbilityCost::Loyalty { amount: cost }) if cost == amount)
}

/// Vraska, Betrayal's Sting at nine loyalty in a main phase, then the cards `extra` places.
fn vraska_board(
    format: FormatConfig,
    poison: &[(PlayerId, u32)],
    extra: impl FnOnce(&mut GameScenario) -> Vec<ObjectId>,
) -> (GameRunner, Vec<ObjectId>) {
    let (mut runner, ids) = board(format, 2, poison, |s| {
        s.at_phase(Phase::PreCombatMain);
        let vraska = s.add_real_card(
            AI,
            "Vraska, Betrayal's Sting",
            Zone::Battlefield,
            shared_integration_card_db(),
        );
        [vec![vraska], extra(s)].concat()
    });
    let obj = runner.state_mut().objects.get_mut(&ids[0]).unwrap();
    obj.loyalty = Some(9);
    obj.counters.insert(CounterType::Loyalty, 9);
    (runner, ids)
}

/// Leyline of Sanctity: "You have hexproof." CR 702.11c: Vraska's −9 can't target that opponent,
/// so the only legal target is the AI and the opponent gets nothing.
#[test]
fn hexproof_opponent_is_not_a_lethal_target() {
    for (leyline, kind) in [
        (false, "poison_clock_lethal"),
        (true, "poison_clock_no_poison_loss"),
    ] {
        let (runner, ids) = vraska_board(threshold(9), &[(OPPONENT, 8)], |s| {
            if leyline {
                s.add_real_card(
                    OPPONENT,
                    "Leyline of Sanctity",
                    Zone::Battlefield,
                    shared_integration_card_db(),
                );
            }
            Vec::new()
        });
        let candidate = activation_of(runner.state(), ids[0], loyalty(-9));
        let (delta, reason) = verdict_on(runner.state(), &candidate);
        assert_eq!(reason.kind, kind);
        assert_eq!(delta > crate::policies::registry::STRONG_MAX, !leyline);
        assert_eq!(delta == 0.0, leyline);
    }
}

/// Vraska's 0: "You draw a card and lose 1 life. Proliferate." At 1 life the AI hits 0 life as the
/// opponent reaches nine poison, a draw (CR 104.4a), not a win; at 2 life it wins.
#[test]
fn vraska_zero_is_lethal_only_when_the_ai_survives_it() {
    for life in [1, 2] {
        let (runner, ids) = vraska_board(threshold(9), &[(OPPONENT, 8)], |s| {
            s.with_life(AI, life);
            Vec::new()
        });
        let state = runner.state();
        let (delta, reason) = verdict_on(state, &activation_of(state, ids[0], loyalty(0)));
        if life == 1 {
            assert_ne!(reason.kind, "poison_clock_lethal");
            assert!(delta <= crate::policies::registry::STRONG_MAX);
        } else {
            assert_eq!(reason.kind, "poison_clock_lethal");
            assert_eq!(fact(&reason, "poison_added"), Some(1));
            assert!(delta > crate::policies::registry::STRONG_MAX);
        }
    }
}

const INFECTIOUS_BITE: &str = "Target creature you control deals damage equal to its power to target creature you don't control. Each opponent gets a poison counter.";
const VRASKAS_FALL: &str =
    "Each opponent sacrifices a creature or planeswalker of their choice and gets a poison counter.";

fn grizzly_bears(scenario: &mut GameScenario, controller: PlayerId) {
    scenario.add_real_card(
        controller,
        "Grizzly Bears",
        Zone::Battlefield,
        shared_integration_card_db(),
    );
}

/// Infectious Bite's creature targets are the AI's own choices, so its counter is read: lethal at
/// nine, not under Solemnity (CR 101.2) or at eight.
#[test]
fn ai_chosen_creature_targets_are_read_for_lethal() {
    for (at, solemn, kind) in [
        (9, false, "poison_clock_lethal"),
        (9, true, "poison_clock_no_poison_loss"),
        (8, false, "poison_clock_pressure"),
    ] {
        let (runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, at)], |s| {
            grizzly_bears(s, AI);
            grizzly_bears(s, OPPONENT);
            if solemn {
                solemnity(s);
            }
            vec![s
                .add_spell_to_hand_from_oracle(AI, "Infectious Bite", true, INFECTIOUS_BITE)
                .id()]
        });
        let (_, reason) = verdict_on(runner.state(), &cast_candidate(ids[0]));
        assert_eq!(reason.kind, kind, "at {at}, solemnity {solemn}");
    }
}

/// Vraska's Fall's opponent chooses which of two creatures to sacrifice, a choice the
/// AI does not answer, so the counter is pressure; with one creature there is no choice to make.
#[test]
fn an_opponent_owned_choice_withholds_the_lethal_read() {
    for (creatures, kind) in [(2, "poison_clock_pressure"), (1, "poison_clock_lethal")] {
        let (runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, 9)], |s| {
            for _ in 0..creatures {
                grizzly_bears(s, OPPONENT);
            }
            vec![s
                .add_spell_to_hand_from_oracle(AI, "Vraska's Fall", true, VRASKAS_FALL)
                .id()]
        });
        let (_, reason) = verdict_on(runner.state(), &cast_candidate(ids[0]));
        assert_eq!(reason.kind, kind, "{creatures} opposing creatures");
    }
}

/// The F1 board: the AI at five poison with `mine`, the opponent at nine with `creatures`, and
/// Infectious Bite in hand.
fn infectious_bite_board(mine: &str, creatures: &[&str]) -> (GameRunner, ObjectId) {
    let poison = [(OPPONENT, 9), (AI, 5)];
    let (runner, ids) = board(FormatConfig::standard(), 2, &poison, |s| {
        let db = shared_integration_card_db();
        s.add_real_card(AI, mine, Zone::Battlefield, db);
        for creature in creatures {
            s.add_real_card(OPPONENT, creature, Zone::Battlefield, db);
        }
        vec![s
            .add_spell_to_hand_from_oracle(AI, "Infectious Bite", true, INFECTIOUS_BITE)
            .id()]
    });
    (runner, ids[0])
}

/// CR 601.2c + CR 702.21a: a permanent that triggers when Infectious Bite targets the opponent's
/// creature withholds lethal (Ward, Frost Titan, Bonecrusher Giant, Diffusion Sliver watching
/// Muscle Sliver, Kira's granted trigger); the AI targets the first legal creature.
#[test]
fn a_ward_trigger_on_a_target_withholds_the_lethal_read() {
    let pressure = "poison_clock_pressure";
    let lethal = "poison_clock_lethal";
    let expected: [(&[&str], &str); 11] = [
        (&["The Serpent Society"], pressure),
        (&["Tolarian Terror"], pressure),
        (&["Frost Titan"], pressure),
        (&["Bonecrusher Giant"], pressure),
        (&["Muscle Sliver", "Diffusion Sliver"], pressure),
        (&["Kira, Great Glass-Spinner"], pressure),
        (&["Grizzly Bears", "The Serpent Society"], lethal),
        (&["The Serpent Society", "Grizzly Bears"], pressure),
        (&["Coppercoat Vanguard", "Elite Vanguard"], lethal),
        (&["Elite Vanguard", "Coppercoat Vanguard"], pressure),
        (&["Grizzly Bears"], lethal),
    ];
    let kinds = expected.map(|(creatures, _)| {
        let (runner, bite) = infectious_bite_board("Grizzly Bears", creatures);
        (
            creatures,
            verdict_on(runner.state(), &cast_candidate(bite)).1.kind,
        )
    });
    assert_eq!(kinds, expected);

    // CR 702.21a: the AI's own Ward (Tolarian Terror) does not trigger on its own spell.
    let (own_ward, bite) = infectious_bite_board("Tolarian Terror", &["Grizzly Bears"]);
    let reason = verdict_on(own_ward.state(), &cast_candidate(bite)).1;
    assert_eq!(reason.kind, lethal);

    let (mut unsettled, bite) = infectious_bite_board("Grizzly Bears", &["Grizzly Bears"]);
    unsettled.state_mut().layers_dirty.mark_full();
    let reason = verdict_on(unsettled.state(), &cast_candidate(bite)).1;
    assert_eq!(reason.kind, lethal);

    let unsettled = [Some(KAMBAL), None].map(|opposing| {
        let (mut runner, prologue) = race_board(opposing, false);
        runner.state_mut().layers_dirty.mark_full();
        verdict_on(runner.state(), &cast_candidate(prologue)).1.kind
    });
    assert_eq!(unsettled, [pressure, lethal]);
}

/// A spell with no target announces nothing, so Frost Titan does not withhold Prologue's lethal.
#[test]
fn an_untargeted_spell_ignores_becomes_target_triggers() {
    let (_, reason) = cast_beside(
        FormatConfig::standard(),
        2,
        &[(OPPONENT, 9)],
        prologue_to_phyresis,
        |s| {
            s.add_real_card(
                OPPONENT,
                "Frost Titan",
                Zone::Battlefield,
                shared_integration_card_db(),
            )
        },
    );
    assert_eq!(reason.kind, "poison_clock_lethal");
}

/// The AI at 2 life with Prologue to Phyresis and {1}{U}, from two Islands when `islands` and
/// floating otherwise; the opponent at nine poison, with `opposing` on the battlefield.
fn race_board(opposing: Option<&str>, islands: bool) -> (GameRunner, ObjectId) {
    let (runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, 9)], |s| {
        s.at_phase(Phase::PreCombatMain);
        s.with_life(AI, 2);
        if islands {
            s.add_basic_land(AI, ManaColor::Blue);
            s.add_basic_land(AI, ManaColor::Blue);
        } else {
            s.with_mana_pool(AI, floating(&[ManaType::Blue; 2]));
        }
        if let Some(name) = opposing {
            s.add_real_card(
                OPPONENT,
                name,
                Zone::Battlefield,
                shared_integration_card_db(),
            );
        }
        vec![prologue_to_phyresis(s)]
    });
    (runner, ids[0])
}

/// Casts Prologue, then orders triggers and passes priority until the game ends.
fn cast_and_settle(runner: &mut GameRunner, prologue: ObjectId) {
    let card_id = runner.state().objects[&prologue].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: prologue,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Prologue is cast");
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::GameOver { .. } => break,
            WaitingFor::OrderTriggers { triggers, .. } => GameAction::OrderTriggers {
                order: (0..triggers.len()).collect(),
            },
            _ => GameAction::PassPriority,
        };
        runner.act(action).expect("the game advances");
    }
}

const KAMBAL: &str = "Kambal, Consul of Allocation";

/// CR 601.2i + CR 603.3 + CR 704.5a: Kambal triggers as Prologue becomes cast and resolves first,
/// so the AI at 2 life loses before the opponent's tenth counter; the policy withholds lethal, and
/// credits it without Kambal.
#[test]
fn a_cast_trigger_that_can_eliminate_the_ai_withholds_the_lethal_read() {
    let (mut runner, prologue) = race_board(Some(KAMBAL), false);
    cast_and_settle(&mut runner, prologue);
    assert!(!engine::game::players::is_alive(runner.state(), AI));
    assert_eq!(poison_of(runner.state(), OPPONENT), 9);

    let kinds = [Some(KAMBAL), None].map(|opposing| {
        let (runner, prologue) = race_board(opposing, false);
        verdict_on(runner.state(), &cast_candidate(prologue)).1.kind
    });
    assert_eq!(kinds, ["poison_clock_pressure", "poison_clock_lethal"]);
}

/// CR 601.2g + CR 704.5a: tapping both Islands for Prologue triggers Manabarbs twice, and those
/// triggers resolve first, so the AI at 2 life loses before the opponent's tenth counter; the
/// policy withholds lethal, and credits it without Manabarbs.
#[test]
fn a_payment_trigger_that_can_eliminate_the_ai_withholds_the_lethal_read() {
    let (mut runner, prologue) = race_board(Some("Manabarbs"), true);
    cast_and_settle(&mut runner, prologue);
    assert!(!engine::game::players::is_alive(runner.state(), AI));
    assert_eq!(poison_of(runner.state(), OPPONENT), 9);

    let kinds = [Some("Manabarbs"), None].map(|opposing| {
        let (runner, prologue) = race_board(opposing, true);
        verdict_on(runner.state(), &cast_candidate(prologue)).1.kind
    });
    assert_eq!(kinds, ["poison_clock_pressure", "poison_clock_lethal"]);

    // CR 601.2g: floating mana taps no land, so Manabarbs does not trigger.
    let (runner, prologue) = race_board(Some("Manabarbs"), false);
    let reason = verdict_on(runner.state(), &cast_candidate(prologue)).1;
    assert_eq!(reason.kind, "poison_clock_lethal");
}

/// CR 601.2i: an opponent's Young Pyromancer triggers only on its controller's casts, so it does
/// not withhold Prologue's lethal, and the cast ends the game for the opponent.
#[test]
fn an_opponents_own_cast_trigger_does_not_withhold_the_lethal_read() {
    let (runner, prologue) = race_board(Some("Young Pyromancer"), false);
    let reason = verdict_on(runner.state(), &cast_candidate(prologue)).1;
    assert_eq!(reason.kind, "poison_clock_lethal");

    let (mut runner, prologue) = race_board(Some("Young Pyromancer"), false);
    cast_and_settle(&mut runner, prologue);
    assert!(!engine::game::players::is_alive(runner.state(), OPPONENT));
}

/// CR 700.14: an opponent's Bakersbane Duo triggers only when its controller expends 4, so it
/// does not withhold Prologue's lethal.
#[test]
fn an_opponents_own_expend_trigger_does_not_withhold_the_lethal_read() {
    let (runner, prologue) = race_board(Some("Bakersbane Duo"), false);
    let reason = verdict_on(runner.state(), &cast_candidate(prologue)).1;
    assert_eq!(reason.kind, "poison_clock_lethal");
}

/// CR 603.2: an opponent's Forbidden Orchard triggers only when its controller taps it for mana,
/// so the AI tapping Islands for Prologue does not withhold the lethal.
#[test]
fn an_opponents_own_tap_for_mana_trigger_does_not_withhold_the_lethal_read() {
    let (runner, prologue) = race_board(Some("Forbidden Orchard"), true);
    let reason = verdict_on(runner.state(), &cast_candidate(prologue)).1;
    assert_eq!(reason.kind, "poison_clock_lethal");
}

/// The AI at 1 life with Prologue to Phyresis, an Island, and a Swamp when `swamp` (else a second
/// Island); the opponent at nine poison with Royal Decree.
fn royal_decree_board(swamp: bool) -> (GameRunner, ObjectId) {
    let (runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, 9)], |s| {
        s.at_phase(Phase::PreCombatMain);
        s.with_life(AI, 1);
        s.add_basic_land(AI, ManaColor::Blue);
        s.add_basic_land(
            AI,
            if swamp {
                ManaColor::Black
            } else {
                ManaColor::Blue
            },
        );
        s.add_real_card(
            OPPONENT,
            "Royal Decree",
            Zone::Battlefield,
            shared_integration_card_db(),
        );
        vec![prologue_to_phyresis(s)]
    });
    (runner, ids[0])
}

/// CR 601.2g + CR 603.3 + CR 704.5a: tapping the Swamp for Prologue triggers Royal Decree, which
/// resolves first and kills the AI at 1 life; paid from two Islands nothing triggers.
#[test]
fn a_tap_trigger_that_can_eliminate_the_ai_withholds_the_lethal_read() {
    let (mut runner, prologue) = royal_decree_board(true);
    cast_and_settle(&mut runner, prologue);
    assert!(!engine::game::players::is_alive(runner.state(), AI));
    assert_eq!(poison_of(runner.state(), OPPONENT), 9);

    let kinds = [true, false].map(|swamp| {
        let (runner, prologue) = royal_decree_board(swamp);
        verdict_on(runner.state(), &cast_candidate(prologue)).1.kind
    });
    assert_eq!(kinds, ["poison_clock_pressure", "poison_clock_lethal"]);
}

/// CR 602.2b + CR 603.2: an opponent's Rings of Brighthearth and Crackdown Construct trigger only
/// on their controller's activations, so Vraska's −9 stays lethal.
#[test]
fn an_opponents_own_activation_trigger_does_not_withhold_the_lethal_read() {
    let kinds = ["Rings of Brighthearth", "Crackdown Construct"].map(|opposing| {
        let (runner, ids) = vraska_board(threshold(9), &[(OPPONENT, 8)], |s| {
            s.add_real_card(
                OPPONENT,
                opposing,
                Zone::Battlefield,
                shared_integration_card_db(),
            );
            Vec::new()
        });
        let candidate = activation_of(runner.state(), ids[0], loyalty(-9));
        verdict_on(runner.state(), &candidate).1.kind
    });
    assert_eq!(kinds, ["poison_clock_lethal"; 2]);
}

const TREACHEROUS_BLESSING: &str = "When this enchantment enters, draw three cards.\nWhenever you cast a spell, you lose 1 life.\nWhen this enchantment becomes the target of a spell or ability, sacrifice it.";

/// CR 601.2i + CR 603.3: the AI's own Teval, Arbiter of Virtue or Treacherous Blessing triggers
/// as Prologue becomes cast and sits above it, so lethal is withheld even at 20 life.
#[test]
fn the_ais_own_cast_trigger_withholds_the_lethal_read() {
    let kinds = [true, false].map(|teval| {
        let (runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, 9)], |s| {
            s.at_phase(Phase::PreCombatMain);
            s.with_mana_pool(AI, floating(&[ManaType::Blue; 2]));
            if teval {
                let db = shared_integration_card_db();
                s.add_real_card(AI, "Teval, Arbiter of Virtue", Zone::Battlefield, db);
            } else {
                s.add_enchantment_from_oracle(AI, "Treacherous Blessing", TREACHEROUS_BLESSING);
            }
            vec![prologue_to_phyresis(s)]
        });
        verdict_on(runner.state(), &cast_candidate(ids[0])).1.kind
    });
    assert_eq!(kinds, ["poison_clock_pressure"; 2]);
}

/// CR 117.4 + CR 704.3: Prologue cast in response to Lightning Bolt aimed at the AI at 3 life
/// resolves first, and the opponent at nine loses before the Bolt resolves.
#[test]
fn a_response_is_read_before_the_spell_below_it_resolves() {
    let bolter = PlayerId(1);
    let poisoned = PlayerId(2);
    let (mut runner, ids) = board(FormatConfig::standard(), 3, &[(poisoned, 9)], |s| {
        s.at_phase(Phase::PreCombatMain);
        s.with_life(AI, 3);
        s.with_mana_pool(AI, floating(&[ManaType::Blue; 2]));
        s.with_mana_pool(bolter, floating(&[ManaType::Red]));
        let bolt = s.add_real_card(
            bolter,
            "Lightning Bolt",
            Zone::Hand,
            shared_integration_card_db(),
        );
        vec![prologue_to_phyresis(s), bolt]
    });
    runner.act(GameAction::PassPriority).unwrap();
    let card_id = runner.state().objects[&ids[1]].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: ids[1],
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .unwrap();
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(AI)),
        })
        .unwrap();
    while runner.state().waiting_for != (WaitingFor::Priority { player: AI }) {
        runner.act(GameAction::PassPriority).unwrap();
    }
    assert_eq!(runner.state().stack.len(), 1, "the Bolt waits on the stack");

    let (delta, reason) = verdict_on(runner.state(), &cast_candidate(ids[0]));
    assert_eq!(reason.kind, "poison_clock_lethal");
    assert!(delta > crate::policies::registry::STRONG_MAX);
}

/// CR 704.5b + CR 104.4a: drawing from an empty library as Prologue's counter lands makes both
/// players lose at once, a draw; with a card to draw it is lethal.
#[test]
fn drawing_from_an_empty_library_withholds_the_lethal_read() {
    let kinds = [true, false].map(|empty| {
        let (mut runner, prologue) = race_board(None, false);
        if empty {
            let state = runner.state_mut();
            for card in state.players[AI.0 as usize].library.clone() {
                engine::game::zones::move_to_zone(state, card, Zone::Exile, &mut Vec::new());
            }
            assert!(state.players[AI.0 as usize].library.is_empty());
        }
        verdict_on(runner.state(), &cast_candidate(prologue)).1.kind
    });
    assert_eq!(kinds, ["poison_clock_pressure", "poison_clock_lethal"]);
}

/// CR 602.2b + CR 601.2h: Yawgmoth, Thran Physician's discard is a choice the AI makes while
/// activating, so the read withholds; answering it on the real board proliferates to lethal.
#[test]
fn a_cost_choice_while_activating_withholds_the_lethal_read() {
    let (mut runner, ids) = board(FormatConfig::standard(), 2, &[(OPPONENT, 9)], |s| {
        let db = shared_integration_card_db();
        s.at_phase(Phase::PreCombatMain);
        s.with_mana_pool(AI, floating(&[ManaType::Black; 2]));
        s.add_real_card(AI, "Grizzly Bears", Zone::Hand, db);
        vec![s.add_real_card(AI, "Yawgmoth, Thran Physician", Zone::Battlefield, db)]
    });
    let last = runner.state().objects[&ids[0]].abilities.len() - 1;
    let candidate = activate_ability_candidate(ids[0], last);
    let reason = verdict_on(runner.state(), &candidate).1;
    assert_eq!(reason.kind, "poison_clock_pressure");

    runner.act(candidate.action).unwrap();
    assert!(!matches!(
        runner.state().waiting_for,
        WaitingFor::Priority { .. }
    ));
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::GameOver { .. } => break,
            WaitingFor::Priority { .. } => GameAction::PassPriority,
            WaitingFor::ProliferateChoice { eligible, .. } => GameAction::SelectTargets {
                targets: eligible.clone(),
            },
            _ => engine::ai_support::legal_actions(runner.state())
                .into_iter()
                .next()
                .unwrap(),
        };
        runner.act(action).unwrap();
    }
    assert!(!engine::game::players::is_alive(runner.state(), OPPONENT));
}

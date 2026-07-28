//! Unit tests for `policies::poison` — the CR 104.3d poison-clock policy.
//! No `#[cfg(test)]` in SOURCE files; tests live here.
//!
//! The `verdict` path runs against a real `PolicyContext` built over a
//! multiplayer `GameState`, mirroring the `energy_payoff` policy-test shape.

use std::sync::Arc;

use engine::ai_support::{ActionMetadata, AiDecisionContext, CandidateAction, TacticalClass};
use engine::game::combat::AttackTarget;
use engine::game::zones::create_object;
use engine::types::ability::ResolvedAbility;
use engine::types::ability::{
    AbilityDefinition, AbilityKind, Effect, ModalChoice, QuantityExpr, TargetFilter,
};
use engine::types::actions::GameAction;
use engine::types::card_type::{CardType, CoreType};
use engine::types::format::FormatConfig;
use engine::types::game_state::{CastPaymentMode, GameState, PendingCast, WaitingFor};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::keywords::Keyword;
use engine::types::mana::ManaCost;
use engine::types::player::{PlayerCounterKind, PlayerId};
use engine::types::zones::Zone;

use crate::config::AiConfig;
use crate::context::AiContext;
use crate::features::poison::{LETHAL_POISON, POISON_CLOCK_FLOOR};
use crate::features::DeckFeatures;
use crate::policies::context::{PolicyContext, SearchDepth};
use crate::policies::registry::{
    PolicyId, PolicyReason, PolicyRegistry, PolicyVerdict, TacticalPolicy,
};
use crate::session::AiSession;

use crate::policies::poison::*;

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
    object.card_types = CardType {
        supertypes: Vec::new(),
        core_types: vec![CoreType::Instant],
        subtypes: Vec::new(),
    };
    *Arc::make_mut(&mut object.abilities) = abilities;
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

// ─── helper boundaries ──────────────────────────────────────────────────────

/// CR 104.3d: ten or more poison counters loses the game, so the ninth
/// counter is the lethal setup and the eighth is not.
#[test]
fn reaches_lethal_matches_cr_104_3d_boundary() {
    assert_eq!(LETHAL_POISON, 10);
    assert!(reaches_lethal(9, 1), "9 + 1 == 10 is lethal");
    assert!(!reaches_lethal(8, 1), "8 + 1 == 9 is not yet lethal");
    assert!(reaches_lethal(4, 6), "a multi-counter swing can reach ten");
    assert!(
        !reaches_lethal(9, 0),
        "nine with nothing added is not lethal"
    );
    assert!(reaches_lethal(u32::MAX, 1), "saturating add must not wrap");
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
fn most_poisoned_opponent_ignores_the_ai_itself() {
    let mut state = state_with_players(2);
    state.players[0].poison_counters = 7;
    state.players[1].poison_counters = 3;
    // The AI's own 7 poison must not be read as pressure it is applying.
    assert_eq!(most_poisoned_opponent(&state, AI), 3);
    assert_eq!(most_poisoned_opponent(&state, OPPONENT), 7);
}

/// CR 800.4: a multiplayer game continues after a player leaves, and the
/// eliminated seat stays in
/// `GameState.players`. Their counters must not produce phantom pressure.
#[test]
fn most_poisoned_opponent_ignores_eliminated_seats() {
    let mut state = state_with_players(4);
    state.players[1].poison_counters = 9;
    state.players[1].is_eliminated = true;
    state.players[2].poison_counters = 2;
    state.players[3].poison_counters = 0;

    assert_eq!(
        most_poisoned_opponent(&state, AI),
        2,
        "a dead seat at 9 poison must not read as one counter from lethal"
    );
    assert_eq!(live_opponent_poison(&state, AI, PlayerId(1)), None);
    assert_eq!(live_opponent_poison(&state, AI, PlayerId(2)), Some(2));
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
            poison_effect(TargetFilter::Opponent),
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
                poison_effect(TargetFilter::Opponent),
            )],
        );
        let candidate = cast_candidate(spell);
        score_of(PoisonClockPolicy.verdict(&ctx(&state, &candidate, &decision, &context, &config)))
    };

    let mut previous = 0.0;
    for existing in 0..LETHAL_POISON - 1 {
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

    let (lethal_delta, lethal_reason) = score_at(LETHAL_POISON - 1);
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
                poison_effect(TargetFilter::Opponent),
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
        AbilityDefinition::new(
            AbilityKind::Activated,
            poison_effect(TargetFilter::Opponent),
        ),
    ];
    ability
}

fn ability_mode_decision(source_id: ObjectId) -> AiDecisionContext {
    let ability = modal_ability();
    AiDecisionContext {
        waiting_for: WaitingFor::AbilityModeChoice {
            player: AI,
            modal: ability.modal.clone().unwrap(),
            source_id,
            mode_abilities: ability.mode_abilities.clone(),
            is_activated: true,
            ability_index: Some(0),
            ability_cost: None,
            unavailable_modes: Vec::new(),
        },
        candidates: Vec::new(),
    }
}

/// CR 601.2b: the poison mode is scored at the seam where it is chosen.
#[test]
fn verdict_scores_the_selected_poison_mode() {
    let config = config();
    let context = ai_context(&config);
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let source = creature_object(&mut state, 1, Vec::new(), 1);
    let decision = ability_mode_decision(source);

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
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let source = creature_object(&mut state, 1, Vec::new(), 1);
    let decision = ability_mode_decision(source);

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
            AbilityDefinition::new(AbilityKind::Spell, poison_effect(TargetFilter::Opponent)),
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
    let spell = spell_object(
        &mut state,
        1,
        vec![
            AbilityDefinition::new(AbilityKind::Spell, draw_effect()),
            AbilityDefinition::new(AbilityKind::Spell, poison_effect(TargetFilter::Opponent)),
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
            poison_effect(TargetFilter::Opponent),
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
    let mut state = state_with_players(2);
    state.players[1].poison_counters = 9;
    let source = creature_object(&mut state, 1, Vec::new(), 1);
    let decision = ability_mode_decision(source);

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
            poison_effect(TargetFilter::Opponent),
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
            poison_effect(TargetFilter::Opponent),
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
        AbilityDefinition::new(
            AbilityKind::Activated,
            poison_effect(TargetFilter::Opponent),
        ),
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
            AbilityDefinition::new(AbilityKind::Spell, poison_effect(TargetFilter::Opponent)),
        ],
    );
    let modal = ModalChoice {
        min_choices: 1,
        max_choices: 1,
        mode_count: 2,
        ..ModalChoice::default()
    };
    state.objects.get_mut(&spell).unwrap().modal = Some(modal.clone());

    // A `WaitingFor::ModeChoice` whose PendingCast points at that spell object.
    let resolved = ResolvedAbility::new(draw_effect(), Vec::new(), spell, AI);
    let pending_cast = PendingCast::new(spell, CardId(spell.0), resolved, ManaCost::zero());
    let decision = AiDecisionContext {
        waiting_for: WaitingFor::ModeChoice {
            player: AI,
            modal,
            pending_cast: Box::new(pending_cast),
            unavailable_modes: Vec::new(),
        },
        candidates: Vec::new(),
    };

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

//! CR 601.2d divided damage — the Bogardan Hellkite class, at the live prompts.
//!
//! Bogardan Hellkite's ETB (Oracle verified against `data/card-data.json`)
//! deals 5 damage divided as you choose among any number of targets. The engine
//! walks that as a step-wise `WaitingFor::TriggerTargetSelection` — one
//! `ChooseTarget` per slot — and then a `WaitingFor::DistributeAmong` over
//! whatever was chosen. This test exercises BOTH prompts through the production
//! AI pipeline rather than asserting a candidate exists in isolation.
//!
//! Half one, at target selection: with a 4-toughness body already declared, one
//! point of the pool remains (CR 601.2d gives every chosen target at least one),
//! so the bodies that point cannot kill are categorically rejected — the live
//! scored candidate set prices them at negative infinity — while the
//! 1-toughness body it CAN kill keeps a finite score. Pre-fix every step was
//! priced as if it received the whole 5-point pool, so nothing was ever
//! rejected.
//!
//! Half two, at the division: with those two bodies declared, the AI must
//! answer `DistributeAmong` with a split that destroys both. Pre-fix the engine
//! offered only the even split — 2 and 3 — which leaves the 4-toughness body
//! alive on 2, so exactly one creature died.
//!
//! The two targets are declared here rather than left to the AI on purpose. In
//! this pipeline `ChooseTarget { target: None }` outscores every additional
//! creature target by a few points regardless of lethality, so an autonomous run
//! takes exactly one target and stops. That is a target-COUNT incentive question
//! independent of this change, which only ever REMOVES targets the pool cannot
//! use; asserting a spontaneous second pick would be testing someone else's
//! scoring.

use std::collections::{HashMap, HashSet};

use engine::game::scenario::{GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{TargetRef, TriggerDefinition};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, DistributionUnit, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;
use phase_ai::auto_play::{run_ai_actions_bounded, AiActionsStop};
use phase_ai::config::{create_config, AiDifficulty, Platform};
use rand::rngs::SmallRng;
use rand::SeedableRng;

/// Bogardan Hellkite's current Oracle line, verified against
/// `data/card-data.json` ("bogardan hellkite").
const HELLKITE_ORACLE: &str = "When this creature enters, it deals 5 damage \
                               divided as you choose among any number of targets.";

/// The ETB trigger from the real parse, so the fixture cannot drift from what
/// the card actually produces: `mode: ChangesZone`, `execute: { DealDamage
/// { Fixed 5, Any }, multi_target { 0, 5 }, distribute: Damage }`.
fn hellkite_trigger() -> TriggerDefinition {
    parse_oracle_text(
        HELLKITE_ORACLE,
        "Bogardan Hellkite",
        &[],
        &[String::from("Creature")],
        &[],
    )
    .triggers
    .into_iter()
    .next()
    .expect("the Hellkite ETB line must parse to one trigger")
}

/// The live score the production pipeline gives one `ChooseTarget` candidate.
fn score_of(scored: &[(GameAction, f64)], target: Option<ObjectId>) -> f64 {
    scored
        .iter()
        .find_map(|(action, score)| match action {
            GameAction::ChooseTarget { target: chosen } => {
                (*chosen == target.map(TargetRef::Object)).then_some(*score)
            }
            _ => None,
        })
        .unwrap_or_else(|| {
            panic!("reach-guard: {target:?} must be a scored candidate, got {scored:?}")
        })
}

#[test]
fn divided_damage_prompts_reject_unkillable_targets_and_split_for_lethality() {
    let trigger = hellkite_trigger();
    // Fixture premise: the parsed trigger really does divide DAMAGE. Without
    // this the assertions below could pass for an unrelated reason.
    assert_eq!(
        trigger
            .execute
            .as_ref()
            .and_then(|ability| ability.distribute.clone()),
        Some(DistributionUnit::Damage),
        "the Hellkite ETB must parse as a divided-damage ability"
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Both libraries are stocked: an empty library makes every search rollout
    // end in a draw-from-empty loss, which prices every candidate at
    // `eval::WIN_SCORE` and drowns the targeting signal this test is about.
    scenario.with_library_top(P0, &vec!["Forest"; 30]);
    scenario.with_library_top(P1, &vec!["Forest"; 30]);

    // CR 704.5g: five opposing bodies with toughness 4/3/2/2/1. Five points
    // cannot kill more than two of them. Every body hits hard, so each is worth
    // removing on its own merits and the question under test is which ones the
    // remaining pool can afford.
    let victims: Vec<ObjectId> = [4, 3, 2, 2, 1]
        .into_iter()
        .enumerate()
        .map(|(index, toughness)| {
            scenario
                .add_creature(P1, &format!("Victim {index}"), 6, toughness)
                .id()
        })
        .collect();

    let hellkite = scenario
        .add_creature_to_hand(P0, "Bogardan Hellkite", 5, 5)
        .with_mana_cost(ManaCost::Cost {
            shards: Vec::new(),
            generic: 1,
        })
        .with_trigger_definition(trigger)
        .id();
    scenario.with_mana_pool(
        P0,
        vec![ManaUnit::new(
            ManaType::Colorless,
            ObjectId(9_999),
            false,
            Vec::new(),
        )],
    );

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let card_id = runner.state().objects[&hellkite].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: hellkite,
            card_id,
            targets: Vec::new(),
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("casting the Hellkite must be accepted");
    // Resolve the creature spell so it enters and its ETB trigger needs targets
    // (CR 603.3d: a triggered ability's targets are chosen as it is put on the
    // stack).
    runner.resolve_top();
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::TriggerTargetSelection { .. }
        ),
        "reach-guard: the ETB trigger must reach target selection, got {:?}",
        runner.state().waiting_for
    );

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);

    // Declare the 4-toughness body: CR 601.2d then reserves the 4 points that
    // kill it, leaving 1 of the 5-point pool.
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victims[0])),
        })
        .expect("declaring the first target must be accepted");

    // Half one, at the LIVE prompt: one point cannot kill the 3- or
    // 2-toughness bodies, so they are categorically rejected; it does kill the
    // 1-toughness body, which keeps a finite score. REVERT-FAILING: pre-fix
    // every candidate was priced against the whole pool and none was rejected.
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);
    for (index, name) in [(1usize, "3-toughness"), (2, "2-toughness")] {
        assert_eq!(
            score_of(&scored, Some(victims[index])),
            f64::NEG_INFINITY,
            "the {name} body must be rejected: 1 point of the divided pool \
             cannot kill it (CR 601.2d + CR 704.5g), got {scored:?}"
        );
    }
    assert!(
        score_of(&scored, Some(victims[4])).is_finite(),
        "the 1-toughness body must keep a finite score — the last remaining \
         point does kill it, got {scored:?}"
    );

    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(victims[4])),
        })
        .expect("declaring the second target must be accepted");

    // THE STOP MOMENT. Reserves of 4 and 1 now hold the whole 5-point pool, so
    // CR 601.2d leaves nothing for another target: every remaining candidate —
    // the surviving creatures AND the opposing player, whose one point would
    // have to come out of a lethal reserve — is rejected, and only finishing
    // keeps a finite score. REVERT-FAILING: pre-fix each step was priced
    // against the whole pool and nothing was ever rejected.
    let scored = phase_ai::score_candidates(runner.state(), P0, &config);
    let rejected: Vec<&(GameAction, f64)> = scored
        .iter()
        .filter(|(action, _)| matches!(action, GameAction::ChooseTarget { target: Some(_) }))
        .collect();
    assert!(
        rejected.len() >= 2,
        "reach-guard: further targets must still be on offer, got {scored:?}"
    );
    assert!(
        rejected.iter().all(|(_, score)| *score == f64::NEG_INFINITY),
        "every remaining target must be rejected once the pool is fully reserved (CR 601.2d), got {scored:?}"
    );
    // The rejection is recipient-blind, which is the point of an exhausted
    // pool: at least one candidate the "chip damage to the face is legitimate"
    // exemption used to spare -- an AI-controlled body, a non-creature, or the
    // opposing player -- is rejected here too. Which of those survives into the
    // scored list is up to the root beam, which caps it at five, so the PLAYER
    // case specifically is pinned at the policy level by
    // `removal_lethality::divided_damage_exhausted_budget_rejects_a_player_target_too`.
    let exempt_recipient_rejected = rejected.iter().any(|(action, _)| match action {
        GameAction::ChooseTarget {
            target: Some(TargetRef::Object(id)),
        } => runner.state().objects.get(id).is_some_and(|object| {
            object.controller == P0 || !object.card_types.core_types.contains(&CoreType::Creature)
        }),
        GameAction::ChooseTarget {
            target: Some(TargetRef::Player(_)),
        } => true,
        _ => false,
    });
    assert!(
        exempt_recipient_rejected,
        "reach-guard: a recipient the non-creature exemption used to spare must be among the rejected targets, got {scored:?}"
    );
    assert!(
        score_of(&scored, None).is_finite(),
        "finishing the selection must keep a finite score, got {scored:?}"
    );

    // Finish the selection, mirroring the client, so the engine opens the
    // division prompt.
    runner
        .act(GameAction::ChooseTarget { target: None })
        .expect("finishing the optional tail must be accepted");

    let WaitingFor::DistributeAmong { total, targets, .. } = runner.state().waiting_for.clone()
    else {
        panic!(
            "reach-guard: two declared targets must open the division prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!((total, targets.len()), (5, 2), "5 points over both targets");

    // Half two: the AI answers the division itself, through the full pipeline.
    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, config)]);
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let run = run_ai_actions_bounded(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        8,
    );

    assert!(
        !matches!(
            run.stop,
            AiActionsStop::ChooseActionNone { .. } | AiActionsStop::ApplyFailed { .. }
        ),
        "the AI must answer the division without refusing, got {:?} after {:?}",
        run.stop,
        run.results.iter().map(|r| &r.action).collect::<Vec<_>>()
    );
    assert!(
        run.results
            .iter()
            .any(|result| matches!(result.action, GameAction::DistributeAmong { .. })),
        "reach-guard: the AI must answer DistributeAmong, got {:?}",
        run.results.iter().map(|r| &r.action).collect::<Vec<_>>()
    );

    runner.advance_until_stack_empty();

    // REVERT-FAILING: the even split is 2 and 3, and 2 does not kill the
    // 4-toughness body (CR 704.5g) — pre-fix exactly one creature died.
    let dead = runner
        .state()
        .objects
        .values()
        .filter(|object| {
            object.zone == Zone::Graveyard
                && object.owner == P1
                && object.card_types.core_types.contains(&CoreType::Creature)
        })
        .count();
    assert_eq!(
        dead, 2,
        "the lethal-first division must destroy BOTH declared targets"
    );
}

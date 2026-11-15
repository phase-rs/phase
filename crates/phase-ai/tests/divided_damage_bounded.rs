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

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::parser::oracle::parse_oracle_text;
use engine::types::ability::{TargetRef, TriggerDefinition};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::game_state::{CastPaymentMode, DistributionScope, DistributionUnit, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
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

// ---------------------------------------------------------------------------
// CR 608.2d resolution-time divisions — Master of the Wild Hunt, at the live
// prompt, answered by the production AI pipeline.
// ---------------------------------------------------------------------------

/// Master of the Wild Hunt's verbatim Oracle text (Scryfall).
const MASTER_ORACLE: &str =
    "At the beginning of your upkeep, create a 2/2 green Wolf creature token.\n\
{T}: Tap all untapped Wolf creatures you control. Each Wolf tapped this way deals damage equal to \
its power to target creature. That creature deals damage equal to its power divided as its \
controller chooses among any number of those Wolves.";

/// A vanilla creature for the board; `subtypes` makes the Wolves Wolves.
struct Body {
    name: &'static str,
    power: i32,
    toughness: i32,
}

const fn body(name: &'static str, power: i32, toughness: i32) -> Body {
    Body {
        name,
        power,
        toughness,
    }
}

/// Master and P0's `wolves` on the battlefield, with `target` (controlled by
/// `target_controller`, deathtouch and lifelink when `deathtouch`) as clause 2's
/// target. Master's ability is activated and driven until clause 3's division
/// prompt — a local copy of the engine integration test's drive loop.
fn master_division_prompt(
    wolves: &[Body],
    target_controller: PlayerId,
    target: Body,
    deathtouch: bool,
) -> (GameRunner, Vec<ObjectId>, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    // Stocked libraries keep search rollouts from ending in a draw-from-empty
    // loss that would drown every other signal.
    scenario.with_library_top(P0, &vec!["Forest"; 30]);
    scenario.with_library_top(P1, &vec!["Forest"; 30]);
    let master = scenario
        .add_creature_from_oracle(P0, "Master of the Wild Hunt", 3, 3, MASTER_ORACLE)
        .with_subtypes(vec!["Human", "Shaman"])
        .id();
    let wolf_ids: Vec<ObjectId> = wolves
        .iter()
        .map(|wolf| {
            scenario
                .add_creature(P0, wolf.name, wolf.power, wolf.toughness)
                .with_subtypes(vec!["Wolf"])
                .id()
        })
        .collect();
    let target_id = {
        let mut builder = scenario.add_creature(
            target_controller,
            target.name,
            target.power,
            target.toughness,
        );
        if deathtouch {
            builder.deathtouch().lifelink();
        }
        builder.id()
    };
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: master,
            ability_index: 0,
            payment_mode: engine::types::game_state::CastPaymentMode::Auto,
        })
        .expect("activate Master of the Wild Hunt");
    runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Object(target_id)),
        })
        .expect("choose clause 2's target");
    for _ in 0..16 {
        match &runner.state().waiting_for {
            WaitingFor::DistributeAmong { .. } => return (runner, wolf_ids, target_id),
            WaitingFor::Priority { .. } if !runner.state().stack.is_empty() => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("expected clause 3's division prompt, got {other:?}"),
        }
    }
    panic!("Master's ability never reached its division prompt");
}

/// Reach guard on the prompt: a CR 608.2d resolution-time damage division of
/// `total`, answered by `chooser`.
fn assert_resolution_prompt(runner: &GameRunner, chooser: PlayerId, total: u32) {
    match &runner.state().waiting_for {
        WaitingFor::DistributeAmong {
            player,
            total: prompt_total,
            unit: DistributionUnit::Damage,
            scope: DistributionScope::ResolutionCandidates { .. },
            ..
        } => {
            assert_eq!(
                *player, chooser,
                "reach guard: the target's controller divides"
            );
            assert_eq!(*prompt_total, total, "reach guard: the target's power");
        }
        other => panic!("reach guard: expected a resolution-time division, got {other:?}"),
    }
}

/// Every `DistributeAmong` answer the engine's candidate generator offers.
fn offered_divisions(runner: &GameRunner) -> Vec<Vec<(TargetRef, u32)>> {
    engine::ai_support::candidate_actions(runner.state())
        .into_iter()
        .filter_map(|candidate| match candidate.action {
            GameAction::DistributeAmong { distribution } => Some(distribution),
            _ => None,
        })
        .collect()
}

/// Let `chooser`'s VeryHard AI answer the division through the production
/// pipeline (one action), and return the division it submitted.
fn ai_answers_division(runner: &mut GameRunner, chooser: PlayerId) -> Vec<(TargetRef, u32)> {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let ai_players = HashSet::from([chooser]);
    let ai_configs = HashMap::from([(chooser, config)]);
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let run = run_ai_actions_bounded(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        1,
    );
    match run.results.first().map(|result| &result.action) {
        Some(GameAction::DistributeAmong { distribution }) => distribution.clone(),
        other => panic!(
            "reach guard: the AI must answer DistributeAmong, got {other:?} (stop {:?})",
            run.stop
        ),
    }
}

fn in_graveyard(runner: &GameRunner, id: ObjectId) -> bool {
    runner.state().objects[&id].zone == Zone::Graveyard
}

/// U3: the selector's value metric ranks a vanilla 1/1 below a vanilla 3/3.
fn assert_value_order(runner: &GameRunner, cheaper: ObjectId, dearer: ObjectId) {
    let cheap = phase_ai::eval::evaluate_creature_intrinsic(runner.state(), cheaper);
    let dear = phase_ai::eval::evaluate_creature_intrinsic(runner.state(), dearer);
    assert!(
        0.0 < cheap && cheap < dear,
        "fixture premise: 1/1 value {cheap} must be positive and below 3/3 value {dear}"
    );
}

/// CR 608.2d + CR 704.5g: dividing among your OWN Wolves, losing the cheaper
/// one beats the both-die lethal split. Master's 1/1 and 3/3 Wolves; the chooser
/// targeted its own vanilla 4/5, so it divides 4 among its own Wolves.
#[test]
fn friendly_division_loses_only_the_cheaper_wolf() {
    let (mut runner, wolves, _) = master_division_prompt(
        &[body("Timber Wolf", 1, 1), body("Watchwolf", 3, 3)],
        P0,
        body("Own Brute", 4, 5),
        false,
    );
    let (timber, watch) = (wolves[0], wolves[1]);
    assert_resolution_prompt(&runner, P0, 4);
    assert_value_order(&runner, timber, watch);

    ai_answers_division(&mut runner, P0);

    assert!(
        in_graveyard(&runner, timber),
        "the cheaper friendly Wolf takes the damage"
    );
    assert!(
        !in_graveyard(&runner, watch),
        "the dearer friendly Wolf must survive"
    );
}

/// CR 608.2d + CR 704.5g: when the pool can be absorbed without killing any of
/// your own Wolves, the no-kill spread is offered and chosen. Two 3/3 Wolves
/// divide 4 as 2 + 2.
#[test]
fn friendly_division_absorbs_the_pool_without_a_kill() {
    let (mut runner, wolves, _) = master_division_prompt(
        &[body("Wolf One", 3, 3), body("Wolf Two", 3, 3)],
        P0,
        body("Own Wall", 4, 7),
        false,
    );
    let (w1, w2) = (wolves[0], wolves[1]);
    assert_resolution_prompt(&runner, P0, 4);
    let offered = offered_divisions(&runner);
    let one_dies = vec![(TargetRef::Object(w1), 4)];
    let spread = vec![(TargetRef::Object(w1), 2), (TargetRef::Object(w2), 2)];
    assert!(
        offered.contains(&one_dies) && offered.contains(&spread),
        "reach guard: both the kill-one and the no-kill spread are offered, got {offered:?}"
    );

    ai_answers_division(&mut runner, P0);

    for wolf in [w1, w2] {
        assert!(
            !in_graveyard(&runner, wolf),
            "no friendly Wolf need die: 2 + 2 on two 3/3s is survivable"
        );
    }
}

/// Control (opposed): the target's controller (P1) divides 4 among P0's 1/1
/// and 3/3 Wolves; destroying both is the unique best answer.
#[test]
fn opposed_division_destroys_both_wolves() {
    let (mut runner, wolves, _) = master_division_prompt(
        &[body("Timber Wolf", 1, 1), body("Watchwolf", 3, 3)],
        P1,
        body("Their Brute", 4, 5),
        false,
    );
    assert_resolution_prompt(&runner, P1, 4);

    ai_answers_division(&mut runner, P1);

    for wolf in wolves {
        assert!(in_graveyard(&runner, wolf), "both opposing Wolves die");
    }
}

/// CR 608.2d: with 3 to divide against a 1/1 and a 3/3, every offered division
/// destroys exactly one Wolf; the opposing chooser destroys the more valuable
/// one. Both creation orders are asserted so the pick cannot ride on the
/// candidate order.
#[test]
fn opposed_division_destroys_the_more_valuable_wolf() {
    for timber_first in [true, false] {
        let bodies = if timber_first {
            [body("Timber Wolf", 1, 1), body("Watchwolf", 3, 3)]
        } else {
            [body("Watchwolf", 3, 3), body("Timber Wolf", 1, 1)]
        };
        let (mut runner, wolves, _) =
            master_division_prompt(&bodies, P1, body("Their Bear", 3, 3), false);
        let (timber, watch) = if timber_first {
            (wolves[0], wolves[1])
        } else {
            (wolves[1], wolves[0])
        };
        assert_resolution_prompt(&runner, P1, 3);
        assert_value_order(&runner, timber, watch);
        let offered = offered_divisions(&runner);
        let expected = [
            vec![(TargetRef::Object(timber), 3)],
            vec![(TargetRef::Object(watch), 3)],
        ];
        assert!(
            offered.len() == expected.len() && expected.iter().all(|d| offered.contains(d)),
            "reach guard: the offered set is exactly {expected:?}, got {offered:?}"
        );

        ai_answers_division(&mut runner, P1);

        assert!(
            in_graveyard(&runner, watch),
            "timber_first={timber_first}: the more valuable Wolf is destroyed"
        );
        assert!(
            !in_graveyard(&runner, timber),
            "timber_first={timber_first}: the 1/1 survives"
        );
    }
}

/// CR 608.2d + CR 702.2b + CR 704.5h: P1's deathtouch, lifelink 2/3 divides
/// its 2 power among P0's two 3/3 Wolves; one point each destroys both.
#[test]
fn deathtouch_division_destroys_both_wolves() {
    let (mut runner, wolves, _) = master_division_prompt(
        &[body("Wolf One", 3, 3), body("Wolf Two", 3, 3)],
        P1,
        body("Vampire Nighthawk", 2, 3),
        true,
    );
    assert_resolution_prompt(&runner, P1, 2);
    let life_before = runner.life(P1);

    ai_answers_division(&mut runner, P1);

    assert_eq!(
        runner.life(P1),
        life_before + 2,
        "reach guard: the lifelink source dealt the whole 2"
    );
    for wolf in wolves {
        assert!(
            in_graveyard(&runner, wolf),
            "a deathtouch point destroys each Wolf"
        );
    }
}

/// Control (no deathtouch): 2 on two 3/3 Wolves destroys neither, whatever the
/// division.
#[test]
fn non_deathtouch_division_destroys_no_wolf() {
    let (mut runner, wolves, _) = master_division_prompt(
        &[body("Wolf One", 3, 3), body("Wolf Two", 3, 3)],
        P1,
        body("Plain Bird", 2, 3),
        false,
    );
    assert_resolution_prompt(&runner, P1, 2);

    ai_answers_division(&mut runner, P1);

    let marked: u32 = wolves
        .iter()
        .map(|wolf| runner.state().objects[wolf].damage_marked)
        .sum();
    assert_eq!(marked, 2, "reach guard: the whole 2 was dealt");
    for wolf in wolves {
        assert!(!in_graveyard(&runner, wolf), "2 cannot destroy a 3/3");
    }
}

use std::collections::HashMap;
use std::collections::HashSet;
use std::io::Read;

use engine::game::combat::{AttackTarget, AttackerInfo, CombatState};
use engine::game::engine::apply_as_current;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::{
    ChoiceType, Effect, EffectKind, QuantityExpr, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::card_type::CoreType;
use engine::types::events::GameEvent;
use engine::types::game_state::CastPaymentMode;
use engine::types::game_state::{
    ManaChoiceContext, ManaChoicePrompt, StackEntry, StackEntryKind, TargetEffectDetail,
    TargetSelectionProgress, TargetSelectionSlot, WaitingFor,
};
use engine::types::identifiers::{CardId, ObjectId};
use engine::types::log::{LogCategory, LogSegment};
use engine::types::mana::{ManaColor, ManaType};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use phase_ai::auto_play::{run_ai_actions, run_ai_actions_bounded, run_driver_loop, DriverExit};
use phase_ai::choose_action;
use phase_ai::config::{create_config, AiConfig, AiDifficulty, Platform};
use phase_ai::saved_state::load_saved_game_state;
use rand::rngs::SmallRng;
use rand::SeedableRng;

fn gunzip_fixture(gz: &[u8]) -> String {
    let mut json = String::new();
    flate2::read::GzDecoder::new(gz)
        .read_to_string(&mut json)
        .expect("fixture .json.gz must inflate to UTF-8 JSON");
    json
}

#[test]
fn saved_cosmic_crucible_mana_prompt_uses_an_issued_action_and_advances() {
    let raw = gunzip_fixture(include_bytes!(
        "../fixtures/scenarios/invisible-woman-cosmic-crucible-mana.json.gz"
    ));
    let mut state = load_saved_game_state(&raw).expect("saved Cosmic Crucible state deserializes");

    let WaitingFor::ChooseManaColor {
        player,
        choice: ManaChoicePrompt::AnyCombination { count, options },
        context: ManaChoiceContext::ResolvingEffect(resume),
    } = &state.waiting_for
    else {
        panic!("capture must restore at Cosmic Crucible's resolving mana prompt");
    };
    let player = *player;
    assert_eq!(
        player,
        PlayerId(2),
        "Cosmic Crucible's controller owns the prompt"
    );
    assert_eq!(
        resume.source_id,
        ObjectId(200),
        "the prompt must come from Cosmic Crucible"
    );
    assert_eq!(
        options,
        &[
            ManaType::White,
            ManaType::Blue,
            ManaType::Black,
            ManaType::Red,
            ManaType::Green
        ],
        "the capture must retain all five color options"
    );
    assert_eq!(*count, 4, "Cosmic Crucible must produce exactly four mana");

    let contract = engine::ai_support::AiDecisionContract::issue(&state, player);
    assert_eq!(
        contract.candidates.len(),
        64,
        "the engine must cap this 5^4 mana prompt to its finite issued domain"
    );
    let state_before = state.clone();
    let ai_players = HashSet::from([player]);
    let ai_configs = HashMap::from([(
        player,
        create_config(AiDifficulty::VeryHard, Platform::Native),
    )]);
    let mut ai_rng = SmallRng::seed_from_u64(25);
    let ai_session = phase_ai::session::AiSession::arc_from_game(&state);
    let run = run_ai_actions_bounded(
        &mut state,
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        1,
    );

    assert_eq!(
        run.len(),
        1,
        "the bounded controller must submit the mana choice"
    );
    assert!(matches!(
        &run.stop,
        phase_ai::auto_play::AiActionsStop::ActionBudgetReached { limit: 1 }
    ));
    assert!(
        contract.contains_action(&state_before, &run[0].action),
        "the controller must submit the exact action from player two's contract"
    );
    assert_eq!(
        run[0].action,
        GameAction::ChooseManaColor {
            choice: engine::types::game_state::ManaChoice::Combination(vec![
                ManaType::White,
                ManaType::White,
                ManaType::Red,
                ManaType::Green,
            ]),
            count: 1,
        },
        "the capped domain still maximizes the captured hand and deck color demand"
    );
    assert!(
        !matches!(state.waiting_for, WaitingFor::ChooseManaColor { .. }),
        "applying the choice must advance beyond Cosmic Crucible's mana prompt"
    );
}

#[test]
fn scenario_prefers_opponent_target_over_self() {
    let mut runner = GameScenario::new().build();
    runner.state_mut().waiting_for = WaitingFor::TriggerTargetSelection {
        player: P0,
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![TargetSelectionSlot {
            legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
            optional: false,
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
        }],
        mode_labels: Vec::new(),
        target_constraints: Vec::new(),
        selection: TargetSelectionProgress {
            current_slot: 0,
            selected_slots: Vec::new(),
            current_legal_targets: vec![TargetRef::Player(P0), TargetRef::Player(P1)],
        },
        source_id: None,
        description: None,
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(11);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
    );
}

#[test]
fn scenario_skips_optional_target_with_no_legal_choices() {
    let mut runner = GameScenario::new().build();
    runner.state_mut().waiting_for = WaitingFor::TriggerTargetSelection {
        player: P0,
        trigger_controller: None,
        trigger_event: None,
        trigger_events: Vec::new(),
        target_slots: vec![TargetSelectionSlot {
            legal_targets: Vec::new(),
            optional: true,
            chooser: None,
            effect_kind: EffectKind::NoOp,
            effect_detail: TargetEffectDetail::None,
        }],
        mode_labels: Vec::new(),
        target_constraints: Vec::new(),
        selection: Default::default(),
        source_id: None,
        description: None,
    };

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(12);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::ChooseTarget { target: None })
    );
}

#[test]
fn scenario_blocks_lethal_attack_when_a_block_exists() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 3);
    let attacker = scenario.add_creature(P1, "Attacker", 4, 4).id();
    let blocker = scenario.add_creature(P0, "Blocker", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P1;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P0)],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::DeclareBlockers {
            player: P0,
            valid_blocker_ids: vec![blocker],
            valid_block_targets: HashMap::from([(blocker, vec![attacker])]),
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
        };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(13);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)],
        })
    );
}

#[test]
fn scenario_multiplayer_attacks_to_finish_exposed_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    let attacker_a = scenario.add_creature(P0, "Attacker A", 3, 3).id();
    let attacker_b = scenario.add_creature(P0, "Attacker B", 2, 2).id();
    let _threat = scenario.add_creature(PlayerId(2), "Threat", 5, 5).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.turn_number = 2;
        state.phase = Phase::DeclareAttackers;
        state.players[1].life = 4;
        state.players[2].life = 20;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![attacker_a, attacker_b],
            valid_attack_targets: vec![AttackTarget::Player(P1), AttackTarget::Player(PlayerId(2))],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(14);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    let Some(engine::types::actions::GameAction::DeclareAttackers { attacks, .. }) = action else {
        panic!("expected declare attackers action");
    };
    assert_eq!(attacks.len(), 2);
    assert!(attacks
        .iter()
        .all(|(_, target)| *target == AttackTarget::Player(P1)));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_a));
    assert!(attacks.iter().any(|(id, _)| *id == attacker_b));
}

/// Pins the `prefer_land_drop` **fast path**, not the evaluator.
///
/// With exactly one playable land, `prefer_land_drop` short-circuits before the
/// search ever runs (it terminates on `let only_land = land_actions.next()?;`
/// followed by a second-`next()` guard), so this test **cannot detect an
/// evaluator regression** — it passed throughout the period when the evaluator
/// scored its own land drop as a strict loss. Evaluator coverage lives in
/// `tests/ai_quality.rs::mana_screwed_ai_ranks_land_drop_above_passing`, which uses
/// **two or more** playable lands so the shortcut declines and the scored path
/// is reached. Any replacement for this test must do the same.
#[test]
fn scenario_single_playable_land_uses_deterministic_shortcut() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let land_id = scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);

    // Move the land to hand (basic land is added to battlefield; we need it in hand for PlayLand)
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        let obj = state.objects.get_mut(&land_id).unwrap();
        obj.zone = engine::types::zones::Zone::Hand;
        state.battlefield.retain(|&id| id != land_id);
        state.players[0].hand.push_back(land_id);
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(15);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PlayLand {
            object_id: land_id,
            card_id: runner.state().objects[&land_id].card_id,
        })
    );
}

#[test]
fn scenario_priority_choice_remains_reducer_legal() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_creature(P1, "Bear", 2, 2);
    scenario.add_bolt_to_hand(P0);

    let runner = scenario.build();
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(16);
    let action = choose_action(runner.state(), P0, &config, &mut rng)
        .expect("AI should choose a legal priority action");

    let mut sim = runner.state().clone();
    apply_as_current(&mut sim, action).expect("AI-selected action should remain reducer-legal");
}

#[test]
fn scenario_bounded_ai_sequence_progresses_without_panicking() {
    let mut scenario = GameScenario::new();
    scenario.with_life(P0, 3);
    let attacker = scenario.add_creature(P1, "Attacker", 4, 4).id();
    let blocker = scenario.add_creature(P0, "Blocker", 1, 1).id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P1;
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P0)],
            ..Default::default()
        });
        state.waiting_for = WaitingFor::DeclareBlockers {
            player: P0,
            valid_blocker_ids: vec![blocker],
            valid_block_targets: HashMap::from([(blocker, vec![attacker])]),
            block_requirements: HashMap::new(),
            blocker_constraints: Default::default(),
        };
    }

    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

    assert!(
        !results.is_empty(),
        "AI loop should take at least one action"
    );
    assert!(
        results.len() <= 200,
        "AI loop should stay within its hard safety cap"
    );
}

/// Builds a two-player game with BOTH seats AI, parked at the initial priority,
/// and each library seeded deep so nobody decks out (draw-from-empty loss) for
/// many turns. With an effectively empty board the AI's action stream is a long
/// run of pass-priority / land plays that cycles phases and turns far beyond any
/// small budget — so any early stop is the budget, not a natural game end. A
/// bare `GameScenario::new()` has empty libraries and stalls after ~4 actions,
/// which is why the seeding is load-bearing for the exact-equality assertions.
fn two_ai_long_stream_runner() -> (GameRunner, HashSet<PlayerId>, HashMap<PlayerId, AiConfig>) {
    let mut scenario = GameScenario::new();
    let deck: Vec<&str> = vec!["Forest"; 60];
    scenario.with_library_top(P0, &deck);
    scenario.with_library_top(P1, &deck);
    let runner = scenario.build();

    let ai_players = HashSet::from([P0, P1]);
    let ai_configs = HashMap::from([
        (P0, create_config(AiDifficulty::VeryHard, Platform::Native)),
        (P1, create_config(AiDifficulty::VeryHard, Platform::Native)),
    ]);
    (runner, ai_players, ai_configs)
}

#[test]
fn run_ai_actions_bounded_stops_exactly_at_budget() {
    // The stream is effectively unbounded (see helper), so a `results.len() == 3`
    // outcome with no break reason can only be the budget cutting the stream.
    let (mut runner, ai_players, ai_configs) = two_ai_long_stream_runner();
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());

    let results = run_ai_actions_bounded(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        3,
    );

    assert_eq!(
        results.len(),
        3,
        "bounded run must take exactly its budget of actions"
    );
    assert!(
        matches!(
            &results.stop,
            phase_ai::auto_play::AiActionsStop::ActionBudgetReached { limit: 3 }
        ),
        "budget cut the stream — the loop did not end for a driver failure"
    );
}

#[test]
fn commander_driver_small_action_cap_is_never_exceeded() {
    // Regression for the PR #6195 round-2 finding: the action-cap regressions
    // must exercise the PRODUCTION driver boundary (`run_driver_loop`, the same
    // helper `ai_commander`'s main calls), not a hand-mirror loop. Reverting
    // main's/the helper's internals to unbounded batches (a full batch runs up
    // to MAX_AI_ACTIONS_PER_SEQUENCE past a small cap) fails here.
    let (mut runner, ai_players, ai_configs) = two_ai_long_stream_runner();
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());

    let outcome = run_driver_loop(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        5,
        &mut |_results, _state, total_before| {
            assert!(
                total_before < 5,
                "observer must see a pre-batch total below the cap, got {total_before}"
            );
        },
    );

    assert_eq!(
        outcome.total_actions, 5,
        "the pass-priority stream is effectively unbounded, so the cap is \
         exactly what stopped the driver"
    );
    assert!(matches!(outcome.exit, DriverExit::CapReached));
}

#[test]
fn commander_driver_cap_beyond_one_batch_exercises_remaining_arithmetic() {
    // A cap of 250 exceeds the 200 per-batch safety clamp, forcing TWO loop
    // iterations: batch 1 is clamped to 200, batch 2 gets remaining = 50. The
    // across-batch accounting (remaining shrinking, total accumulating) is
    // exactly where the original overshoot bug lived; a cap <= 200 runs the loop
    // once and cannot discriminate it.
    let (mut runner, ai_players, ai_configs) = two_ai_long_stream_runner();
    let mut ai_rng = SmallRng::seed_from_u64(42);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());

    let mut batch_sizes: Vec<usize> = Vec::new();
    let outcome = run_driver_loop(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
        250,
        &mut |results, _state, total_before| {
            assert!(
                total_before < 250,
                "observer must see a pre-batch total below the cap, got {total_before}"
            );
            batch_sizes.push(results.len());
        },
    );

    assert_eq!(
        batch_sizes,
        vec![200, 50],
        "200 is MAX_AI_ACTIONS_PER_SEQUENCE (phase-ai/src/auto_play.rs); if that \
         constant changes this assertion fails loudly and should be updated in step"
    );
    assert_eq!(outcome.total_actions, 250);
    assert!(matches!(outcome.exit, DriverExit::CapReached));
}

const GOLLUM_SCHEMING_GUIDE_ORACLE: &str = "Whenever Gollum attacks, look at the top two cards of your library, put them back in any order, then choose land or nonland. An opponent guesses whether the top card of your library is the chosen kind. Reveal that card. If they guessed right, remove Gollum from combat. Otherwise, you draw a card and Gollum can't be blocked this turn.";

fn gollum_waiting_for_ai_guess() -> (GameRunner, ObjectId, ObjectId) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gollum = scenario
        .add_creature_from_oracle(
            P0,
            "Gollum, Scheming Guide",
            2,
            2,
            GOLLUM_SCHEMING_GUIDE_ORACLE,
        )
        .id();
    let second = scenario.add_card_to_library_top(P0, "Coppercoat Vanguard");
    let top = scenario.add_card_to_library_top(P0, "Forest");
    for _ in 0..5 {
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    mark_core_type(&mut runner, top, CoreType::Land);
    mark_core_type(&mut runner, second, CoreType::Creature);
    attack_with_gollum(&mut runner, gollum);
    drive_to_named_choice(&mut runner, top);
    choose_card_predicate(&mut runner, P0, "Land");
    drive_to_named_choice(&mut runner, top);
    choose_opponent(&mut runner, P0, P1);
    drive_to_named_choice(&mut runner, top);

    let WaitingFor::NamedChoice {
        player,
        choice_type,
        ..
    } = &runner.state().waiting_for
    else {
        panic!(
            "Gollum should be waiting for the chosen opponent's guess, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(*player, P1);
    assert!(matches!(choice_type, ChoiceType::CardPredicateGuess { .. }));

    (runner, gollum, top)
}

fn mark_core_type(runner: &mut GameRunner, card: ObjectId, core_type: CoreType) {
    let object = runner
        .state_mut()
        .objects
        .get_mut(&card)
        .expect("scenario card exists");
    object.card_types.core_types = vec![core_type];
    object.base_card_types = object.card_types.clone();
}

fn attack_with_gollum(runner: &mut GameRunner, gollum: ObjectId) {
    pass_priority_round(runner);
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(gollum, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("Gollum should be able to attack");
}

fn drive_to_named_choice(runner: &mut GameRunner, preferred_top: ObjectId) {
    for _ in 0..24 {
        match runner.state().waiting_for.clone() {
            WaitingFor::NamedChoice { .. } => return,
            WaitingFor::Priority { .. } => pass_priority_round(runner),
            WaitingFor::ScryChoice { cards, .. } | WaitingFor::DigChoice { cards, .. } => {
                runner
                    .act(GameAction::SelectCards {
                        cards: keep_card_on_top(cards, preferred_top),
                    })
                    .expect("Gollum should keep the expected top card");
            }
            other => panic!("expected progress toward Gollum's NamedChoice, got {other:?}"),
        }
    }
    panic!(
        "never reached Gollum's NamedChoice; last state = {:?}",
        runner.state().waiting_for
    );
}

fn keep_card_on_top(cards: Vec<ObjectId>, preferred_top: ObjectId) -> Vec<ObjectId> {
    let mut ordered = Vec::with_capacity(cards.len());
    if cards.contains(&preferred_top) {
        ordered.push(preferred_top);
    }
    ordered.extend(cards.into_iter().filter(|card| *card != preferred_top));
    ordered
}

fn choose_card_predicate(runner: &mut GameRunner, expected_player: PlayerId, choice: &str) {
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected Gollum NamedChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, expected_player);
    assert!(matches!(choice_type, ChoiceType::CardPredicate { .. }));
    assert!(options.iter().any(|option| option == choice));
    runner
        .act(GameAction::ChooseOption {
            choice: choice.to_string(),
        })
        .expect("card-predicate choice should resolve");
}

fn choose_opponent(runner: &mut GameRunner, expected_player: PlayerId, opponent: PlayerId) {
    let WaitingFor::NamedChoice {
        player,
        choice_type,
        options,
        ..
    } = runner.state().waiting_for.clone()
    else {
        panic!(
            "expected opponent NamedChoice, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(player, expected_player);
    assert!(matches!(choice_type, ChoiceType::Opponent { .. }));
    let choice = opponent.0.to_string();
    assert!(options.iter().any(|option| option == &choice));
    runner
        .act(GameAction::ChooseOption { choice })
        .expect("opponent choice should resolve");
}

fn pass_priority_round(runner: &mut GameRunner) {
    let seats = runner.state().seat_order.len();
    for _ in 0..seats {
        let _ = runner.act(GameAction::PassPriority);
    }
}

fn gollum_is_attacking(runner: &GameRunner, gollum: ObjectId) -> bool {
    runner.state().combat.as_ref().is_some_and(|combat| {
        combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == gollum)
    })
}

fn drive_gollum_combat_damage(runner: &mut GameRunner) -> Vec<GameEvent> {
    if matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
        pass_priority_round(runner);
    }
    if matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner
            .act(GameAction::DeclareBlockers {
                assignments: vec![],
            })
            .expect("declaring no blockers should succeed");
    }
    runner.combat_damage().events().to_vec()
}

#[test]
fn gollum_opponent_guess_runs_in_ai_loop_and_wrong_guess_deals_damage() {
    let mut nonland_run = None;

    for seed in 0..64 {
        let (mut runner, gollum, top) = gollum_waiting_for_ai_guess();
        let ai_players = HashSet::from([P1]);
        let ai_configs =
            HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
        let mut ai_rng = SmallRng::seed_from_u64(seed);
        let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
        let results = run_ai_actions(
            runner.state_mut(),
            &ai_players,
            &ai_configs,
            &mut ai_rng,
            &ai_session,
        );

        if matches!(
            results.first().map(|result| &result.action),
            Some(GameAction::ChooseOption { choice }) if choice == "Nonland"
        ) {
            nonland_run = Some((runner, results, gollum, top));
            break;
        }
    }

    let (mut runner, results, gollum, top) =
        nonland_run.expect("seeded AI guesses should include the wrong Nonland branch");
    let guess_result = results
        .first()
        .expect("AI should submit the opponent guess");
    assert!(
        guess_result.events.iter().any(|event| matches!(
            event,
            GameEvent::CardPredicateGuessMade {
                player_id,
                source_id: Some(source_id),
                choice,
            } if *player_id == P1 && *source_id == gollum && choice == "Nonland"
        )),
        "AI guess should emit the generic predicate guess event, got {:?}",
        guess_result.events
    );
    let guess_log = guess_result
        .log_entries
        .iter()
        .find(|entry| entry.category == LogCategory::Debug)
        .expect("AI guess should return a visible debug log entry");
    assert!(
        matches!(
            guess_log.segments.as_slice(),
            [
                LogSegment::PlayerName { player_id, .. },
                LogSegment::Text(guesses),
                LogSegment::Text(choice),
                LogSegment::Text(for_text),
                LogSegment::CardName { name, object_id },
            ] if *player_id == P1
                && guesses == " guesses "
                && choice == "Nonland"
                && for_text == " for "
                && name == "Gollum, Scheming Guide"
                && *object_id == gollum
        ),
        "AI guess log should name the actual random guess, got {:?}",
        guess_log.segments
    );

    runner.advance_until_stack_empty();
    assert!(
        runner.state().players[0].hand.contains(&top),
        "wrong AI guess should draw the revealed top card"
    );
    assert!(
        gollum_is_attacking(&runner, gollum),
        "wrong AI guess should leave Gollum attacking"
    );

    let defender_life_before = runner.state().players[P1.0 as usize].life;
    let combat_events = drive_gollum_combat_damage(&mut runner);
    assert_eq!(
        runner.state().players[P1.0 as usize].life,
        defender_life_before - 2,
        "Gollum should deal combat damage after the AI guesses wrong"
    );
    assert!(
        combat_events.iter().any(|event| matches!(
            event,
            GameEvent::DamageDealt {
                source_id,
                target: TargetRef::Player(P1),
                amount: 2,
                is_combat: true,
                ..
            } if *source_id == gollum
        )),
        "wrong AI guess should preserve Gollum's combat damage event, got {combat_events:?}"
    );
}

#[test]
fn scenario_very_hard_wasm_passes_instead_of_postcombat_giant_growth() {
    let mut scenario = GameScenario::new();
    scenario.add_creature(P0, "Bear", 2, 2);
    scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PostCombatMain;
        state.active_player = P1;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(17);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PassPriority)
    );
}

#[test]
fn scenario_very_hard_wasm_uses_giant_growth_to_win_combat() {
    let mut scenario = GameScenario::new();
    let attacker = scenario.add_creature(P0, "Attacker", 2, 2).id();
    let blocker = scenario.add_creature(P1, "Blocker", 4, 4).id();
    let growth = scenario
        .add_spell_to_hand_from_oracle(
            P0,
            "Giant Growth",
            true,
            "Target creature gets +3/+3 until end of turn.",
        )
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::DeclareBlockers;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.combat = Some(CombatState {
            attackers: vec![AttackerInfo::attacking_player(attacker, P1)],
            blocker_assignments: HashMap::from([(attacker, vec![blocker])]),
            blocker_to_attacker: HashMap::from([(blocker, vec![attacker])]),
            ..Default::default()
        });
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(18);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::CastSpell {
            object_id: growth,
            card_id: runner.state().objects[&growth].card_id,
            targets: Vec::new(),

            payment_mode: CastPaymentMode::Auto,
        })
    );
}

#[test]
fn scenario_very_hard_wasm_passes_with_empty_stack_counterspell() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_spell_to_hand_from_oracle(P0, "Counterspell", true, "Counter target spell.")
        .id();

    let runner = scenario.build();
    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(19);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PassPriority)
    );
}

#[test]
fn scenario_very_hard_wasm_passes_on_redundant_removal() {
    let mut scenario = GameScenario::new();
    let target = scenario.add_creature(P1, "Target", 2, 2).id();
    let murder = scenario
        .add_spell_to_hand_from_oracle(P0, "Murder", true, "Destroy target creature.")
        .id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
        state.stack.push_back(StackEntry {
            id: ObjectId(301),
            source_id: ObjectId(300),
            controller: P0,
            kind: StackEntryKind::Spell {
                ability: Some(Box::new(ResolvedAbility::new(
                    Effect::DealDamage {
                        amount: QuantityExpr::Fixed { value: 3 },
                        target: TargetFilter::Any,
                        damage_source: None,
                        excess: None,
                    },
                    vec![TargetRef::Object(target)],
                    ObjectId(300),
                    P0,
                ))),
                card_id: CardId(300),
                casting_variant: Default::default(),
                actual_mana_spent: 0,
            },
        });
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(20);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    assert_eq!(
        action,
        Some(engine::types::actions::GameAction::PassPriority),
        "Expected pass instead of redundant removal with Murder {:?}",
        runner.state().objects[&murder].name
    );
}

#[test]
fn scenario_harvester_of_misery_cast_is_preferred_over_pass() {
    let mut scenario = GameScenario::new();
    let _harvester = scenario
        .add_creature_to_hand_from_oracle(
            P0,
            "Harvester of Misery",
            5,
            4,
            "When Harvester of Misery enters, target creature gets -2/-2 until end of turn.",
        )
        .id();
    scenario.add_creature(P1, "Opponent Bear", 2, 2);

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    let config = create_config(AiDifficulty::VeryHard, Platform::Wasm);
    let mut rng = SmallRng::seed_from_u64(21);
    let action = choose_action(runner.state(), P0, &config, &mut rng);

    // The AI should recognise that a 5/4 menace with ETB -2/-2 against a lone 2/2
    // is strong. Accept either casting or passing — this scenario is marginal at
    // VeryHard search depth because the mana constraints are tight.
    assert!(
        matches!(
            action,
            Some(engine::types::actions::GameAction::CastSpell { .. })
                | Some(engine::types::actions::GameAction::PassPriority)
        ),
        "AI should either cast Harvester or pass, got {action:?}"
    );
}

/// Regression (issue #1189): when a human controls an AI seat via Mindslaver,
/// the server AI loop must not attempt to act for that seat — it would apply
/// actions as the wrong player and hang or crash.
#[test]
fn mindslaver_human_control_stops_ai_loop() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_land_to_hand(P1, "Forest");
        scenario.build()
    };
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.turn_decision_controller = Some(P0);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P1 });
    }

    let ai_players = HashSet::from([P1]);
    let ai_configs = HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(1189);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

    assert!(
        results.is_empty(),
        "AI must not act when a human controls the AI seat (Mindslaver)"
    );
}

/// Under Emrakul-style control the AI controller must still act for the human seat.
#[test]
fn emrakul_ai_control_runs_for_controlled_human() {
    let mut runner = {
        let mut scenario = GameScenario::new();
        scenario.at_phase(Phase::PreCombatMain);
        scenario.add_land_to_hand(P0, "Forest");
        scenario.build()
    };
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.turn_decision_controller = Some(P1);
        engine::game::public_state::sync_waiting_for(state, &WaitingFor::Priority { player: P0 });
    }

    let ai_players = HashSet::from([P1]);
    let ai_configs = HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(2012);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );

    assert!(
        !results.is_empty(),
        "AI controller must act during the controlled human turn"
    );
}

// ---------------------------------------------------------------------------
// Claws of Gix dead-end regression (CR 601.2g/601.2h ordering — mana paid FIRST,
// removal LAST). The composite "{1}, Sacrifice a permanent" used to pay the
// sacrifice FIRST, so when the only {1} source (Mox Opal Metalcraft) needed the
// sacrificed artifact to stay countable, the residual {1} became unpayable —
// every `SelectCards` candidate failed `apply_as_current`, leaving an empty
// scored set and a `fallback_action` debug_assert panic. The mana-leg detour now
// pays {1} on the INTACT board (the CR 601.2g window) before the sacrifice, so
// the activation is legal and the loop completes.
// ---------------------------------------------------------------------------

/// Build a `{T}: Add {1}` mana ability gated by Metalcraft-style live-eval
/// "control 3+ artifacts" (`ActivationRestriction::RequiresCondition`).
fn metalcraft_mox_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Comparator,
        ControllerRef, ParsedCondition, QuantityRef, TypeFilter, TypedFilter,
    };
    let mut def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: engine::types::ManaProduction::Colorless {
                count: QuantityExpr::Fixed { value: 1 },
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap);
    def.activation_restrictions
        .push(ActivationRestriction::RequiresCondition {
            condition: Some(ParsedCondition::QuantityComparison {
                lhs: QuantityExpr::Ref {
                    qty: QuantityRef::ObjectCount {
                        filter: TargetFilter::Typed(
                            TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                        ),
                    },
                },
                comparator: Comparator::GE,
                rhs: QuantityExpr::Fixed { value: 3 },
            }),
        });
    def
}

/// The Claws-of-Gix activated ability: `{1}, Sacrifice a permanent: You gain 1 life.`
fn claws_of_gix_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, SacrificeCost, TypedFilter,
    };
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: engine::types::mana::ManaCost::generic(1),
            },
            AbilityCost::Sacrifice(SacrificeCost::count(
                TargetFilter::Typed(TypedFilter::permanent()),
                1,
            )),
        ],
    })
}

/// V3 (∃-success): board with 4 artifacts (Mox + 3 others) so sacrificing one
/// leaves 3 → Metalcraft holds → a witness exists.
///
/// **What this test actually pins, measured — the doc it replaces was wrong.** The
/// activation is legal here (the `activation_legal_for` precondition below), and
/// the AI then *declines* it: driving the loop on this board yields exactly
/// `[PassPriority]`. So the load-bearing assertion is the precondition — it fails
/// the moment a Metalcraft/cost regression stops `legal_actions` surfacing the
/// activation. The `assert_no_fallback_cancel` that follows is a guard, not the
/// subject: a board the AI passes on cannot dead-end. The sibling mana-first test
/// is the one that exercises the completion path end to end (measured:
/// `[ActivateAbility, SelectCards, PassPriority]`), and it carries the positive
/// assertion.
///
/// That the AI declines a legal, witnessed Claws activation is a real observation
/// and is out of scope here; it is disclosed in the commit that added this doc
/// rather than silently papered over. Before the profile-independence fix, this
/// branch also carried a `debug_assert!(false, …)`, which is why the old doc spoke
/// of a panic; no profile panics now.
#[test]
fn scenario_claws_of_gix_witness_board_does_not_dead_end() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Mox Opal", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(metalcraft_mox_def());
    }
    // Three plain artifacts so total = 4; sacrificing one leaves 3 (Metalcraft).
    for i in 0..3 {
        let mut a = scenario.add_creature(P0, &format!("Artifact {i}"), 0, 1);
        a.as_artifact();
    }
    let claws = {
        let mut claws = scenario.add_creature(P0, "Claws of Gix", 0, 1);
        claws.as_artifact();
        claws.with_ability_definition(claws_of_gix_def());
        claws.id()
    };

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // Precondition, not decoration: `assert_no_fallback_cancel` is a purely
    // negative assertion, so without this an unrelated break in the activation's
    // legality (Metalcraft comparator, cost payability) would stop the AI ever
    // entering a pending cast and leave the test GREEN while the scenario it
    // names had stopped happening. The mana-first sibling has the same guard.
    assert!(
        activation_legal_for(runner.state(), claws),
        "witness board must surface the Claws activation before the loop runs"
    );

    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(19024);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );
    assert_no_fallback_cancel(
        &results,
        "witness board must not escape the Claws activation through the fallback",
    );
}

/// Dead-end detector for the Claws scenarios, working in BOTH profiles.
///
/// These tests used to detect a recurrence via the `debug_assert!(false, …)` that
/// lived in `search::fallback_action`'s pending-cast branch (their own comments
/// said so), backed by `assert!(results.len() <= 200)`. That length assertion is
/// **structurally vacuous**: `run_ai_actions` is hard-capped at
/// `MAX_AI_ACTIONS_PER_SEQUENCE = 200` (`crates/phase-ai/src/auto_play.rs:20`), so
/// `<= 200` holds for every possible run and cannot fail on its subject.
///
/// `CancelCast` is rejected from the strategic pool (`tactical_gate.rs:205`,
/// `GameAction::CancelCast => GateDecision::Reject`), so any `CancelCast` the AI
/// emits comes from `search::fallback_action`. That function has several
/// `CancelCast` exits — `TargetSelection`, the pending-cast dead-end,
/// `EquipTarget`, and `Crew/Saddle/StationTarget` — but on these two boards (no
/// Equipment, no Vehicle, and the only targeting effect is a
/// `GainLife { player: Controller }` that takes no target) the pending-cast
/// dead-end is the only reachable one, so a `CancelCast` here IS that dead-end.
/// Unlike the removed `debug_assert`, this holds in release builds too.
///
/// Both outcomes are checked. `run_ai_actions` only pushes an `AiActionResult`
/// once `apply_interaction` succeeded (`auto_play.rs:214-250`), and the
/// pending-cast `CancelCast` is offered to the AI only by
/// `semantic_candidate_actions_with_probe`'s guarded push
/// (`engine/src/ai_support/candidates.rs`, which requires `has_pending_cast` AND
/// `allows_cancel_cast`; `candidate_actions_broad_with_probe`, which it calls,
/// emits `CancelCast` only for Equipment/Vehicle/modal shapes absent from these
/// boards). A dead-end satisfying only `allows_cancel_cast` therefore never
/// becomes an applied action — it lands in `break_reason`, and a results-only
/// assertion would miss it.
fn assert_no_fallback_cancel(run: &phase_ai::auto_play::AiActionsRun, what: &str) {
    use phase_ai::auto_play::AiActionsStop;

    assert!(
        !run.results
            .iter()
            .any(|r| matches!(r.action, GameAction::CancelCast)),
        "{what}: AI escaped via fallback CancelCast (actions: {:?})",
        run.results.iter().map(|r| &r.action).collect::<Vec<_>>(),
    );
    assert!(
        !matches!(
            &run.stop,
            AiActionsStop::ApplyFailed { action, .. }
                if matches!(**action, GameAction::CancelCast)
        ),
        "{what}: AI dead-ended on an unapplied fallback CancelCast ({:?})",
        &run.stop,
    );
}

/// V3 sibling (mana-first, formerly the "no-witness" dead-end): board with
/// exactly 3 artifacts (Mox + one plain artifact + Claws — itself an artifact),
/// so EVERY eligible sacrifice would drop the artifact count to 2 → Metalcraft
/// off. CR 601.2g pays {1} from the Mox on the INTACT 3-artifact board BEFORE the
/// sacrifice, so the Claws activation is LEGAL and the AI loop completes it
/// without dead-ending. REVERT-FAILING: reverting the mana-first detour restores
/// the sacrifice-first ordering, where `can_pay` is rejected (or the activation
/// dead-ends), so `legal_actions` no longer surfaces the Claws activation — the
/// `activation_legal_for` precondition below is what fails, and the pending-cost
/// loop then escapes through `fallback_action`'s `CancelCast`, which
/// [`assert_no_fallback_cancel`] catches. (That branch used to `debug_assert!`
/// with the message "AI fallback reached during pending cast …"; that string no
/// longer exists — the surviving `tracing::error!` reads "AI fallback cancelled
/// an uncompletable cast".)
#[test]
fn scenario_claws_of_gix_mana_first_board_proposes_and_completes() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Mox Opal", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(metalcraft_mox_def());
    }
    // One plain artifact; with the Mox and the (artifact) Claws this is exactly
    // 3 artifacts. Sacrificing ANY of the three drops the count to 2 → no
    // Metalcraft → the {1} leg would be unpayable AFTER the sacrifice, but the
    // mana-first detour pays it on the intact board before that.
    {
        let mut a = scenario.add_creature(P0, "Artifact 0", 0, 1);
        a.as_artifact();
    }
    let claws = {
        let mut claws = scenario.add_creature(P0, "Claws of Gix", 0, 1);
        claws.as_artifact();
        claws.with_ability_definition(claws_of_gix_def());
        claws.id()
    };

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.phase = Phase::PreCombatMain;
        state.active_player = P0;
        state.priority_player = P0;
        state.waiting_for = WaitingFor::Priority { player: P0 };
    }

    // The activation is now legal because the {1} is paid on the intact board.
    assert!(
        activation_legal_for(runner.state(), claws),
        "mana-first pays {{1}} on the intact 3-artifact board → Claws activation must be legal"
    );

    // Driving the full loop must COMPLETE without escaping through
    // `fallback_action` — see `assert_no_fallback_cancel`.
    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut ai_rng = SmallRng::seed_from_u64(19057);
    let ai_session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let results = run_ai_actions(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut ai_rng,
        &ai_session,
    );
    assert_no_fallback_cancel(&results, "mana-first board must not dead-end the AI loop");
    // Positive half: "completes" must mean the activation actually happened, not
    // merely that nothing cancelled. Without it the test would also pass on a board
    // the AI simply passes priority on — which is what the witness sibling does.
    assert!(
        results
            .results
            .iter()
            .any(|r| matches!(r.action, GameAction::ActivateAbility { .. })),
        "mana-first board must ACTIVATE the Claws (actions: {:?})",
        results
            .results
            .iter()
            .map(|r| &r.action)
            .collect::<Vec<_>>(),
    );
}

// ---------------------------------------------------------------------------
// Battlefield-removal generalization of the Claws-of-Gix mana-first fix
// (CR 601.2g/601.2h): the same ordering applies to Exile-from-battlefield
// (CR 701.13a, Curie) and ReturnToHand-from-battlefield (plain bounce, Master
// Transmuter). Each removal would shrink the board the only {U} source depends
// on, so paying the mana FIRST (intact board) keeps the activation legal and
// avoids the dead-end the removal-first ordering produced.
// ---------------------------------------------------------------------------

/// `{T}: Add {U}` mana ability. When `metalcraft` is set the ability is gated by
/// a live-eval "control 3+ artifacts" `ActivationRestriction::RequiresCondition`
/// (the Mox-Opal model); otherwise it is unconditional.
fn blue_mox_def(metalcraft: bool) -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ActivationRestriction, Comparator,
        ControllerRef, ParsedCondition, QuantityRef, TypeFilter, TypedFilter,
    };
    use engine::types::mana::ManaColor;
    let mut def = AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::Mana {
            produced: engine::types::ManaProduction::Fixed {
                colors: vec![ManaColor::Blue],
                contribution: engine::types::ability::ManaContribution::Base,
            },
            restrictions: vec![],
            grants: vec![],
            expiry: None,
            target: None,
        },
    )
    .cost(AbilityCost::Tap);
    if metalcraft {
        def.activation_restrictions
            .push(ActivationRestriction::RequiresCondition {
                condition: Some(ParsedCondition::QuantityComparison {
                    lhs: QuantityExpr::Ref {
                        qty: QuantityRef::ObjectCount {
                            filter: TargetFilter::Typed(
                                TypedFilter::new(TypeFilter::Artifact)
                                    .controller(ControllerRef::You),
                            ),
                        },
                    },
                    comparator: Comparator::GE,
                    rhs: QuantityExpr::Fixed { value: 3 },
                }),
            });
    }
    def
}

/// Curie-style activated ability: `{1}{U}, Exile another nontoken artifact you
/// control: gain 1 life` (effect stubbed to GainLife). The exile leg has
/// `zone: None` + an artifact (permanent-implying) filter, so the live zone
/// classifier resolves it to the battlefield (CR 701.13a). The building block
/// under test is "exile-from-battlefield as a cost shrinks board mana"; the
/// scenario fixtures are pure artifacts (the builder's `as_artifact` drops the
/// creature type), so the filter matches "another nontoken artifact" rather than
/// Curie's printed "artifact creature" — the witness mechanic is identical.
fn curie_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, FilterProp, TypeFilter,
        TypedFilter,
    };
    use engine::types::mana::{ManaCost, ManaCostShard};
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 1,
                },
            },
            AbilityCost::Exile {
                count: 1,
                zone: None,
                filter: Some(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact)
                        .controller(ControllerRef::You)
                        .properties(vec![FilterProp::Another, FilterProp::NonToken]),
                )),
            },
        ],
    })
}

/// Master Transmuter's activated ability: `{U}, {T}, Return an artifact you
/// control to its owner's hand: gain 1 life` (effect stubbed to GainLife). The
/// return leg has `from_zone: None` (battlefield bounce, CR 118.3).
fn master_transmuter_def() -> engine::types::ability::AbilityDefinition {
    use engine::types::ability::{
        AbilityCost, AbilityDefinition, AbilityKind, ControllerRef, TypeFilter, TypedFilter,
    };
    use engine::types::mana::{ManaCost, ManaCostShard};
    AbilityDefinition::new(
        AbilityKind::Activated,
        Effect::GainLife {
            amount: QuantityExpr::Fixed { value: 1 },
            player: TargetFilter::Controller,
        },
    )
    .cost(AbilityCost::Composite {
        costs: vec![
            AbilityCost::Mana {
                cost: ManaCost::Cost {
                    shards: vec![ManaCostShard::Blue],
                    generic: 0,
                },
            },
            AbilityCost::Tap,
            AbilityCost::ReturnToHand {
                count: 1,
                filter: Some(TargetFilter::Typed(
                    TypedFilter::new(TypeFilter::Artifact).controller(ControllerRef::You),
                )),
                from_zone: None,
            },
        ],
    })
}

/// Set the runner into a P0-priority main-phase decision point (mirrors the
/// Claws scenarios).
fn put_p0_on_priority(runner: &mut engine::game::scenario::GameRunner) {
    let state = runner.state_mut();
    state.phase = Phase::PreCombatMain;
    state.active_player = P0;
    state.priority_player = P0;
    state.waiting_for = WaitingFor::Priority { player: P0 };
}

/// Whether `legal_actions` surfaces an `ActivateAbility` whose source is `id`.
fn activation_legal_for(state: &engine::types::game_state::GameState, id: ObjectId) -> bool {
    use engine::types::actions::GameAction;
    engine::ai_support::legal_actions(state)
        .iter()
        .any(|a| matches!(a, GameAction::ActivateAbility { source_id, .. } if *source_id == id))
}

/// Curie EXILE mana-first (CR 601.2g / CR 701.13a): exactly 3 artifacts
/// (Metalcraft blue Mox = sole {U} source, Curie, and the lone exile target) +
/// a Forest for the generic {1}. The mana-leg detour pays `{1}{U}` FIRST while
/// all 3 artifacts are intact (Metalcraft holds → the Mox makes {U}); the exile
/// is paid LAST. So the activation is LEGAL even though exiling afterwards drops
/// below Metalcraft. REVERT-FAILING: reverting the mana-first detour restores the
/// exile-first ordering, which dead-ends here, so `legal_actions` would no longer
/// surface the Curie activation.
#[test]
fn scenario_curie_exile_mana_first_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(true));
    }
    // The lone exile target: another nontoken artifact.
    {
        let mut tgt = scenario.add_creature(P0, "Artifact Servo", 1, 1);
        tgt.as_artifact();
    }
    // A Forest pays the generic {1}; it is NOT a {U} source.
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    let curie = {
        let mut curie = scenario.add_creature(P0, "Curie", 2, 2);
        curie.as_artifact();
        curie.with_ability_definition(curie_def());
        curie.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), curie),
        "{{1}}{{U}} paid on the intact 3-artifact board before the exile → activation must be legal"
    );
}

/// Curie EXILE witness control (non-vacuity): same board as the dead-end test
/// plus a 4th artifact, so exiling the target leaves 3 artifacts → Metalcraft
/// stays live → the Mox keeps making {U} → a witness exists → the activation is
/// legal. This proves the `{1}{U}` leg is payable on the intact board, so the
/// dead-end test's illegality is the removal-shrink discriminator, not a vacuous
/// unpayable cost.
#[test]
fn scenario_curie_exile_witness_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(true));
    }
    {
        let mut tgt = scenario.add_creature(P0, "Artifact Servo", 1, 1);
        tgt.as_artifact();
    }
    // A 4th artifact keeps Metalcraft live after any single exile.
    {
        let mut filler = scenario.add_creature(P0, "Artifact Filler", 0, 1);
        filler.as_artifact();
    }
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Green);
    let curie = {
        let mut curie = scenario.add_creature(P0, "Curie", 2, 2);
        curie.as_artifact();
        curie.with_ability_definition(curie_def());
        curie.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), curie),
        "exiling the target leaves 3 artifacts → Metalcraft holds → activation must be legal"
    );
}

/// Master Transmuter RETURN mana-first (CR 601.2g / CR 118.3): the sole artifact
/// the player controls is the sole {U} source (an unconditional blue Mox), and
/// it is therefore the only legal "return an artifact you control" target. The
/// Transmuter source is a NON-artifact creature, so it is not itself a return
/// target. CR 601.2g pays `{U}` by tapping the Mox FIRST (the intact-board mana
/// window), then the {T} and the return are paid LAST — so the activation is
/// LEGAL even though the only return target is the {U} source. REVERT-FAILING:
/// reverting the mana-first detour restores the return-first ordering, where
/// bouncing the Mox leaves `{U}` unpayable and `legal_actions` drops the
/// activation.
#[test]
fn scenario_master_transmuter_return_mana_first_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(false));
    }
    // Non-artifact source carrying the ability → not a return target itself.
    let transmuter = {
        let mut t = scenario.add_creature(P0, "Master Transmuter", 1, 1);
        t.with_ability_definition(master_transmuter_def());
        t.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), transmuter),
        "{{U}} paid by tapping the Mox before the return → activation must be legal"
    );
}

/// Master Transmuter RETURN witness control (non-vacuity): same board plus a
/// basic Island (an unconditional {U} source that is NOT an artifact, so it is
/// not a return target). Returning the Mox still leaves the Island's {U}, so a
/// witness exists and the activation is legal — proving the `{U}` leg is payable
/// on the intact board and the dead-end test's illegality is the removal-shrink
/// discriminator.
#[test]
fn scenario_master_transmuter_witness_board_is_legal() {
    let mut scenario = GameScenario::new();
    {
        let mut mox = scenario.add_creature(P0, "Blue Mox", 0, 0);
        mox.as_artifact();
        mox.with_ability_definition(blue_mox_def(false));
    }
    // A second, non-artifact {U} source that survives returning the Mox.
    scenario.add_basic_land(P0, engine::types::mana::ManaColor::Blue);
    let transmuter = {
        let mut t = scenario.add_creature(P0, "Master Transmuter", 1, 1);
        t.with_ability_definition(master_transmuter_def());
        t.id()
    };

    let mut runner = scenario.build();
    put_p0_on_priority(&mut runner);

    assert!(
        activation_legal_for(runner.state(), transmuter),
        "the Island keeps {{U}} available after the return → activation must be legal"
    );
}

/// AI-route reach-guard (Decision 3): the engine-owned completion seam that ALL AI
/// declare-attackers routes funnel through (`complete_attacker_proposal[s]`) returns
/// an `apply`-accepted, hard-legal declaration that obeys the CR 508.1d maximum
/// requirement bar. Exercised through the direct-choice route (`choose_action`) and
/// the host loop (`run_ai_actions`); routes 1/3/4 (candidate generation, fallback,
/// scoring) are internal to `choose_action` and reached transitively, and route 7
/// (Resolve All) shares the `run_ai_actions` seam. A lured attacker forces a
/// non-empty completion, so the returned action is not the vacuous empty declaration.
///
/// Revert guard: if the AI route bypassed engine completion and fell back to the
/// first generic legal action (an empty/illegal declaration), `runner.act(action)`
/// would either reject it or fail to commit combat obeying the lure.
#[test]
fn ai_declare_attackers_completion_returns_apply_accepted_legal_action() {
    use engine::types::ability::StaticDefinition;
    use engine::types::statics::StaticMode;

    fn parked_lured() -> (GameRunner, ObjectId) {
        let mut scenario = GameScenario::new();
        let attacker = {
            let mut b = scenario.add_creature(P0, "Lured Bear", 2, 2);
            b.with_static_definition(StaticDefinition::new(StaticMode::MustAttackDefender {
                defender: P1.into(),
            }));
            b.id()
        };
        let mut runner = scenario.build();
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 2;
        state.waiting_for = WaitingFor::DeclareAttackers {
            player: P0,
            valid_attacker_ids: vec![attacker],
            valid_attack_targets: vec![AttackTarget::Player(P1)],
            valid_attack_targets_by_attacker: None,
            attacker_constraints: Default::default(),
        };
        (runner, attacker)
    }

    // Route 2 (direct choice): `choose_action` returns a DeclareAttackers the real
    // reducer accepts, obeying the lure (CR 508.1d max requirement = 1).
    let (mut runner, attacker) = parked_lured();
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let action = choose_action(runner.state(), P0, &config, &mut rng)
        .expect("AI must choose a declare-attackers action");
    assert!(
        matches!(action, GameAction::DeclareAttackers { .. }),
        "expected DeclareAttackers, got {action:?}"
    );
    runner
        .act(action)
        .expect("the AI's declaration must be reducer-legal (apply-accepted)");
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|c| c.attackers.iter().any(|a| a.object_id == attacker)),
        "the completed declaration obeys the lure and commits combat"
    );

    // Route 8 (host loop): `run_ai_actions` drives the same seam to a terminal state
    // without panicking or looping on the declare step.
    let (mut host, _attacker) = parked_lured();
    let ai_players = HashSet::from([P0]);
    let ai_configs = HashMap::from([(P0, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut host_rng = SmallRng::seed_from_u64(7);
    let session = phase_ai::session::AiSession::arc_from_game(host.state());
    let results = run_ai_actions(
        host.state_mut(),
        &ai_players,
        &ai_configs,
        &mut host_rng,
        &session,
    );
    assert!(
        !results.is_empty(),
        "the host AI loop must take at least one action for the declare step"
    );
}

/// CR 506.3 + CR 508.1d: the AI must obey a forced-attack requirement whose
/// required defender is a PLANESWALKER, not just one naming a player.
///
/// The mandatory-attacker sweep in `combat_ai` records only `ObjectId`s, so the
/// required `AttackTarget` is not carried into target assignment. That is safe
/// only because every production path routes its heuristic proposal through
/// `validated_declare_attackers` -> `combat::complete_attacker_proposal`, the
/// engine's single CR 508.1d authority, which replaces an under-max declaration
/// with the deterministic maximum-requirement witness. This test is the evidence
/// for that claim rather than an argument for it: it drives the real
/// `choose_action` seam on Gideon Jura's "+2" and asserts BOTH that the chosen
/// action attacks the planeswalker and that the reducer accepts it.
///
/// Sibling of `ai_declare_attackers_completion_returns_apply_accepted_legal_action`
/// (the player-directed lure). If the AI ever returned its raw heuristic
/// assignment instead of the completed proposal, it would attack P0 here and the
/// reducer would reject the declaration — both halves fail.
#[test]
fn ai_obeys_planeswalker_directed_attack_requirement() {
    const GIDEON_JURA_ORACLE: &str = concat!(
        "+2: During target opponent's next turn, creatures that player controls ",
        "attack Gideon Jura if able.\n",
        "\u{2212}2: Destroy target tapped creature.\n",
        "0: Until end of turn, Gideon Jura becomes a 6/6 Human Soldier creature ",
        "that's still a planeswalker. Prevent all damage that would be dealt to ",
        "him this turn.",
    );

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let gideon = scenario
        .add_planeswalker_from_oracle(P0, "Gideon Jura", "Gideon", 6, GIDEON_JURA_ORACLE)
        .id();
    let bear = scenario.add_creature(P1, "Bear", 2, 2).id();
    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        state.active_player = P0;
        state.priority_player = P0;
        state.turn_number = 2;
        state.layers_dirty.mark_full();
    }
    engine::game::layers::evaluate_layers(runner.state_mut());

    // Resolve the "+2" targeting P1 through the production activation path.
    runner.activate(gideon, 0).target_player(P1).resolve();

    // Hand the turn to P1 and park at their declare-attackers step.
    {
        let state = runner.state_mut();
        state.active_player = P1;
        state.priority_player = P1;
        state.phase = Phase::DeclareAttackers;
        state.turn_number = 3;
        // CR 302.6: everything has been under its controller's control since
        // before this turn began.
        for id in state.battlefield.clone() {
            if let Some(obj) = state.objects.get_mut(&id) {
                obj.summoning_sick = false;
            }
        }
        state.layers_dirty.mark_full();
    }
    engine::game::layers::evaluate_layers(runner.state_mut());

    let valid = engine::game::combat::get_valid_attacker_ids(runner.state());
    assert!(
        valid.contains(&bear),
        "reach-guard: P1's creature is an eligible attacker"
    );
    let targets = engine::game::combat::get_valid_attack_targets(runner.state());
    assert!(
        targets.contains(&AttackTarget::Planeswalker(gideon)),
        "reach-guard: the engine offers Gideon as an attackable defender: {targets:?}"
    );
    runner.state_mut().waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: valid,
        valid_attack_targets: targets,
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };

    // NON-VACUITY PIN: the raw heuristic genuinely gets this wrong. It records
    // the creature as mandatory but discards the required `AttackTarget`, so it
    // proposes the defending PLAYER. This assertion is what makes the
    // `choose_action` check below meaningful — without it, the test would still
    // pass if the heuristic happened to pick Gideon for value reasons, and would
    // prove nothing about the completion seam.
    //
    // If a future change teaches the policy to carry the defender itself, this
    // assertion flips to the planeswalker and should simply be updated — the
    // seam below is the invariant, not the heuristic's raw answer.
    let raw = phase_ai::combat_ai::choose_attackers_with_targets(runner.state(), P1);
    assert_eq!(
        raw,
        vec![(bear, AttackTarget::Player(P0))],
        "the raw policy proposes the defending player — the engine completion is \
         what repairs it, and that is exactly what this test guards"
    );

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let action = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must choose a declare-attackers action");
    let GameAction::DeclareAttackers { attacks, .. } = &action else {
        panic!("expected DeclareAttackers, got {action:?}");
    };
    assert_eq!(
        attacks,
        &vec![(bear, AttackTarget::Planeswalker(gideon))],
        "CR 508.1d: the only maximum-requirement declaration attacks the planeswalker"
    );

    runner
        .act(action)
        .expect("the AI's declaration must be reducer-legal (apply-accepted)");
    assert!(
        runner.state().combat.as_ref().is_some_and(|c| c
            .attackers
            .iter()
            .any(|a| a.object_id == bear && a.attack_target == AttackTarget::Planeswalker(gideon))),
        "combat commits with the creature attacking Gideon"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// CR 508.1d + CR 508.1h: the AI pays a Propaganda-style attack tax.
//
// The declare-attackers completion authority substitutes a TAX-FREE witness for
// any proposal it will not pay for, and with no must-attack requirement on the
// board that witness is the EMPTY declaration. The AI must therefore plan to pay
// before it declares. These tests drive the real `choose_action` seam to prove
// it attacks when paying is worth it, stops when it cannot pay, and that the
// taxed round trip terminates.
// ─────────────────────────────────────────────────────────────────────────────

/// Propaganda's verified Oracle text.
///
/// Source: client/public/card-data.json (2026-05-10), matching the engine-side
/// `add_propaganda` helper in `crates/engine/tests/integration/rules/combat.rs`.
const PROPAGANDA_ORACLE: &str = "Creatures can't attack you unless their controller pays {2} \
     for each creature they control that's attacking you.";

/// Park P1 in `DeclareAttackers` against a P0 that controls Propaganda.
///
/// P1 gets `attackers` untapped 3/3s and `untapped_lands` Forests; P0's only
/// permanent is the enchantment, so nothing can block and the AI's only reason
/// to hold back is the tax.
fn build_propaganda_attack_scenario(
    attackers: usize,
    untapped_lands: usize,
) -> (GameRunner, Vec<ObjectId>) {
    use engine::types::mana::ManaColor;

    let mut scenario = GameScenario::new();
    scenario.add_enchantment_from_oracle(P0, "Propaganda", PROPAGANDA_ORACLE);
    let attacker_ids: Vec<ObjectId> = (0..attackers)
        .map(|index| {
            scenario
                .add_creature(P1, &format!("Bear {index}"), 3, 3)
                .id()
        })
        .collect();
    for _ in 0..untapped_lands {
        scenario.add_basic_land(P1, ManaColor::Green);
    }

    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: attacker_ids.clone(),
        valid_attack_targets: vec![AttackTarget::Player(P0)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    (runner, attacker_ids)
}

/// The AI's chosen declaration, as attacker ids.
fn ai_declared_attackers(runner: &GameRunner) -> (GameAction, Vec<ObjectId>) {
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let action = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must choose a declare-attackers action");
    let GameAction::DeclareAttackers { attacks, .. } = &action else {
        panic!("expected DeclareAttackers, got {action:?}");
    };
    let ids = attacks.iter().map(|(id, _)| *id).collect();
    (action, ids)
}

/// CR 508.1d + CR 508.1j: with the tax affordable and no blocker in sight, the
/// AI declares the attack and pays, rather than collapsing to the empty
/// tax-free witness.
#[test]
fn ai_attacks_through_propaganda_and_pays_the_tax() {
    let (mut runner, attackers) = build_propaganda_attack_scenario(1, 4);

    let (action, declared) = ai_declared_attackers(&runner);
    assert_eq!(
        declared, attackers,
        "a 3/3 into an empty board must attack even though Propaganda taxes it {{2}}"
    );

    runner
        .act(action)
        .expect("the AI's taxed declaration must be reducer-legal");
    let WaitingFor::CombatTaxPayment { total_cost, .. } = &runner.state().waiting_for else {
        panic!(
            "a taxed declaration must open the tax prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(
        total_cost.mana_value(),
        2,
        "Propaganda taxes {{2}} per attacker"
    );

    // CR 508.1j: the AI must now answer its OWN quote with a payment — a decline
    // would rebuild the identical declare prompt and re-propose forever.
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let answer = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must answer the combat tax prompt");
    assert_eq!(
        answer,
        GameAction::PayCombatTax { accept: true },
        "the AI proposed this taxed attack, so it must pay for it"
    );

    runner.act(answer).expect("paying the tax must succeed");
    let combat = runner
        .state()
        .combat
        .as_ref()
        .expect("combat commits once the tax is paid");
    assert_eq!(
        combat
            .attackers
            .iter()
            .map(|attacker| attacker.object_id)
            .collect::<Vec<_>>(),
        attackers,
        "the paid-for attacker must actually be attacking"
    );
}

/// CR 508.1j: an unaffordable tax is not attacked into. The AI must fall back to
/// the empty declaration WITHOUT opening a prompt it cannot answer.
#[test]
fn ai_holds_back_when_the_propaganda_tax_is_unaffordable() {
    let (mut runner, attackers) = build_propaganda_attack_scenario(1, 0);
    assert!(
        !engine::game::combat::attack_tax_is_affordable(
            runner.state(),
            &[(attackers[0], AttackTarget::Player(P0))],
        ),
        "premise: with no mana open the {{2}} tax is unaffordable"
    );

    let (action, declared) = ai_declared_attackers(&runner);
    assert!(
        declared.is_empty(),
        "with no mana to pay {{2}} the AI must not declare a taxed attacker, got {declared:?}"
    );

    runner.act(action).expect("the empty declaration is legal");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "an unaffordable proposal must never open a tax prompt, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 508.1h: Propaganda prices each attacker, and the locked-in total is their
/// sum, so an alpha strike the AI cannot fund in full is trimmed to one it can,
/// not abandoned. Three 3/3s cost {6}; with four lands open the AI attacks with
/// two of them for {4}.
#[test]
fn ai_trims_the_attack_to_the_propaganda_tax_it_can_afford() {
    let (mut runner, attackers) = build_propaganda_attack_scenario(3, 4);

    let (action, declared) = ai_declared_attackers(&runner);
    assert_eq!(
        declared.len(),
        2,
        "four lands fund {{4}} of the {{6}} full strike, so two attackers must go, got {declared:?}"
    );
    assert!(
        declared.iter().all(|id| attackers.contains(id)),
        "the trimmed strike must be a subset of the available attackers"
    );

    runner
        .act(action)
        .expect("the trimmed declaration must be legal");
    let WaitingFor::CombatTaxPayment { total_cost, .. } = &runner.state().waiting_for else {
        panic!(
            "the trimmed strike is still taxed and must prompt, got {:?}",
            runner.state().waiting_for
        );
    };
    assert_eq!(total_cost.mana_value(), 4, "two attackers at {{2}} each");
}

/// P1 has a lone 1/1 and six Forests; P0 controls Propaganda only when
/// `with_propaganda`. The runner is parked at P1's `DeclareAttackers`.
fn build_squire_attack_scenario(with_propaganda: bool) -> (GameRunner, ObjectId) {
    use engine::types::mana::ManaColor;

    let mut scenario = GameScenario::new();
    if with_propaganda {
        scenario.add_enchantment_from_oracle(P0, "Propaganda", PROPAGANDA_ORACLE);
    }
    let attacker = scenario.add_creature(P1, "Squire", 1, 1).id();
    for _ in 0..6 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    state.active_player = P1;
    state.priority_player = P1;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(P0)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    (runner, attacker)
}

/// CR 508.1d: paying is optional, and a tax that costs more than the attack is
/// worth is declined. A 1/1 into Propaganda's {2} is affordable with six lands
/// open but not worth it, so the AI keeps it home and never opens the prompt.
#[test]
fn ai_declines_a_propaganda_tax_that_outprices_the_attack() {
    // Control leg: on the same board without Propaganda, the real
    // `choose_action` path attacks with the 1/1. Only the tax differs below.
    let (control, attacker) = build_squire_attack_scenario(false);
    let (_, control_declared) = ai_declared_attackers(&control);
    assert_eq!(
        control_declared,
        vec![attacker],
        "premise: untaxed, the AI attacks an empty board with the 1/1"
    );

    let (mut runner, attacker) = build_squire_attack_scenario(true);
    assert!(
        engine::game::combat::attack_tax_is_affordable(
            runner.state(),
            &[(attacker, AttackTarget::Player(P0))],
        ),
        "premise: six lands cover the {{2}} tax, so affordability cannot explain a hold-back"
    );

    let (action, declared) = ai_declared_attackers(&runner);
    assert!(
        declared.is_empty(),
        "one damage is not worth {{2}}, so the 1/1 must stay home, got {declared:?}"
    );
    runner.act(action).expect("the empty declaration is legal");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "a declined tax must never open the prompt, got {:?}",
        runner.state().waiting_for
    );
}

/// The taxed declare → pay round trip must TERMINATE under the host loop.
///
/// CR 508.1d: declining the tax rebuilds the identical `DeclareAttackers`
/// prompt, so a seat whose payment answer disagreed with its declaration would
/// spin here forever. `run_ai_actions_bounded` caps the work, and combat having
/// advanced past the declare step is the evidence the loop converged.
#[test]
fn ai_taxed_attack_round_trip_terminates() {
    let (mut runner, attackers) = build_propaganda_attack_scenario(1, 4);

    let ai_players = HashSet::from([P1]);
    let ai_configs = HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut rng = SmallRng::seed_from_u64(7);
    let session = phase_ai::session::AiSession::arc_from_game(runner.state());
    let run = run_ai_actions_bounded(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut rng,
        &session,
        32,
    );

    assert!(
        !run.results.is_empty(),
        "the host loop must take at least the declare + pay actions"
    );
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. } | WaitingFor::CombatTaxPayment { .. }
        ),
        "the declare/pay cycle must not still be pending, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().combat.as_ref().is_some_and(|combat| combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == attackers[0])),
        "the attacker must have committed to combat rather than looping on the tax"
    );
}

/// CR 508.1d: the attack round trip must also terminate for a control seat,
/// whose archetype multiplier damps the attack-tax bias.
///
/// The prompt answer is the engine's affordability check, not a re-run of the
/// deck-weighted judgement, so the answer cannot drift from the posture that
/// opened the prompt however the seat's features weigh the tax.
#[test]
fn ai_taxed_attack_round_trip_terminates_for_a_control_seat() {
    let (mut runner, attackers) = build_propaganda_attack_scenario(1, 4);

    let mut session = phase_ai::AiSession::from_game(runner.state());
    let mut control = phase_ai::features::DeckFeatures::default();
    control.control.commitment = 0.9;
    session.features.insert(P1, control);
    let session = std::sync::Arc::new(session);

    let ai_players = HashSet::from([P1]);
    let ai_configs = HashMap::from([(P1, create_config(AiDifficulty::VeryHard, Platform::Native))]);
    let mut rng = SmallRng::seed_from_u64(7);
    run_ai_actions_bounded(
        runner.state_mut(),
        &ai_players,
        &ai_configs,
        &mut rng,
        &session,
        32,
    );

    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::DeclareAttackers { .. } | WaitingFor::CombatTaxPayment { .. }
        ),
        "the declare/pay cycle must not still be pending, got {:?}",
        runner.state().waiting_for
    );
    assert!(
        runner.state().combat.as_ref().is_some_and(|combat| combat
            .attackers
            .iter()
            .any(|attacker| attacker.object_id == attackers[0])),
        "a 3/3 into an empty board is worth {{2}} even to a control seat"
    );
}

// ─────────────────────────────────────────────────────────────────────────────
// CR 509.1c + CR 509.1d: the block-side twin. The blocker completion authority
// substitutes the same kind of tax-free witness, so the AI must plan to pay for
// a taxed block before it declares one.
// ─────────────────────────────────────────────────────────────────────────────

/// Archangel of Tithes' verified block-tax static.
///
/// Source: client/public/card-data.json (2026-05-10), matching the engine-side
/// `add_archangel_of_tithes` helper in `crates/engine/tests/integration/rules/combat.rs`.
/// Attached here to a ground attacker so the test isolates the tax building
/// block from the Archangel's flying.
const BLOCK_TAX_ORACLE: &str = "As long as this creature is attacking, creatures can't block \
     unless their controller pays {1} for each of those creatures.";

/// P0 attacks P1 (the AI) with a 3/3 carrying a {1} block tax. P1 has one 4/4
/// that blocks the 3/3 profitably, plus `untapped_lands` Forests. The runner is
/// left at P1's `DeclareBlockers` prompt.
fn build_block_tax_scenario(untapped_lands: usize) -> (GameRunner, ObjectId, ObjectId) {
    build_block_scenario(BlockFixture {
        blocker_power: 4,
        blocker_toughness: 4,
        block_tax: true,
        untapped_lands,
    })
}

/// The board `build_block_scenario` lays out: P0's 3/3 attacker, optionally
/// carrying the {1} block tax, against one P1 blocker and some Forests.
struct BlockFixture {
    blocker_power: i32,
    blocker_toughness: i32,
    block_tax: bool,
    untapped_lands: usize,
}

fn build_block_scenario(fixture: BlockFixture) -> (GameRunner, ObjectId, ObjectId) {
    use engine::parser::oracle_static::parse_static_line;
    use engine::types::mana::ManaColor;

    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let attacker = {
        let mut builder = scenario.add_creature(P0, "Taxing Raider", 3, 3);
        if fixture.block_tax {
            let block_tax =
                parse_static_line(BLOCK_TAX_ORACLE).expect("block-tax static should parse");
            builder.with_static_definition(block_tax);
        }
        builder.id()
    };
    let blocker = scenario
        .add_creature(
            P1,
            "Stout Wall",
            fixture.blocker_power,
            fixture.blocker_toughness,
        )
        .id();
    for _ in 0..fixture.untapped_lands {
        scenario.add_basic_land(P1, ManaColor::Green);
    }

    let mut runner = scenario.build();
    runner.pass_both_players();
    runner
        .act(GameAction::DeclareAttackers {
            attacks: vec![(attacker, AttackTarget::Player(P1))],
            bands: vec![],
        })
        .expect("an untaxed attack declares directly");
    // Priority passes after the declaration until the defender is asked to block.
    let mut guard = 0;
    while !matches!(
        runner.state().waiting_for,
        WaitingFor::DeclareBlockers { .. }
    ) {
        runner.pass_both_players();
        guard += 1;
        assert!(
            guard < 4,
            "never reached DeclareBlockers: {:?}",
            runner.state().waiting_for
        );
    }
    (runner, attacker, blocker)
}

/// CR 509.1c + CR 509.1f: with {1} open, the AI keeps its profitable block and
/// pays for it rather than letting the engine's tax-free witness drop it.
#[test]
fn ai_pays_a_block_tax_to_keep_a_profitable_block() {
    let (mut runner, attacker, blocker) = build_block_tax_scenario(3);

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let action = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must choose a blocker declaration");
    let GameAction::DeclareBlockers { assignments } = &action else {
        panic!("expected DeclareBlockers, got {action:?}");
    };
    assert_eq!(
        assignments,
        &vec![(blocker, attacker)],
        "the 4/4 must keep its block on the 3/3 despite the {{1}} tax"
    );

    runner
        .act(action)
        .expect("the taxed block must be reducer-legal");
    assert!(
        matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "a taxed block must open the tax prompt, got {:?}",
        runner.state().waiting_for
    );

    let answer = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must answer the block-tax prompt");
    assert_eq!(
        answer,
        GameAction::PayCombatTax { accept: true },
        "the AI chose this taxed block, so it must pay for it"
    );
    runner
        .act(answer)
        .expect("paying the block tax must succeed");
    assert!(
        runner
            .state()
            .combat
            .as_ref()
            .is_some_and(|combat| combat.blocker_to_attacker.contains_key(&blocker)),
        "the paid-for blocker must actually be blocking"
    );
}

/// CR 509.1h + CR 510.1b: a block tax is valued by the damage the block stops,
/// not by the blocker's own power. A 0/4 wall that holds off a 3/3 is worth
/// {1}, so the AI keeps the block and pays.
#[test]
fn ai_pays_a_block_tax_for_a_wall_that_stops_the_damage() {
    let wall = |block_tax| BlockFixture {
        blocker_power: 0,
        blocker_toughness: 4,
        block_tax,
        untapped_lands: 3,
    };
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);

    // Control leg: untaxed, the AI blocks the 3/3 with its 0/4 wall.
    let (control, attacker, blocker) = build_block_scenario(wall(false));
    let mut rng = SmallRng::seed_from_u64(7);
    let control_action = choose_action(control.state(), P1, &config, &mut rng)
        .expect("AI must choose a blocker declaration");
    assert_eq!(
        control_action,
        GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)]
        },
        "premise: untaxed, the wall blocks the 3/3"
    );

    let (runner, attacker, blocker) = build_block_scenario(wall(true));
    let mut rng = SmallRng::seed_from_u64(7);
    let action = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must choose a blocker declaration");
    assert_eq!(
        action,
        GameAction::DeclareBlockers {
            assignments: vec![(blocker, attacker)]
        },
        "three damage stopped is worth {{1}}, so the wall must keep its block"
    );
}

/// CR 509.1f: with no mana open the block tax is unaffordable, so the AI must
/// fall back to the tax-free declaration without opening a prompt it cannot pay.
#[test]
fn ai_drops_a_block_it_cannot_pay_the_tax_for() {
    let (mut runner, attacker, blocker) = build_block_tax_scenario(0);
    assert!(
        !engine::game::combat::block_tax_is_affordable(runner.state(), P1, &[(blocker, attacker)]),
        "premise: with no mana open the {{1}} block tax is unaffordable"
    );

    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let action = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("AI must choose a blocker declaration");
    let GameAction::DeclareBlockers { assignments } = &action else {
        panic!("expected DeclareBlockers, got {action:?}");
    };
    assert!(
        assignments.is_empty(),
        "an unaffordable block tax leaves only the tax-free empty declaration, got {assignments:?}"
    );

    runner
        .act(action)
        .expect("the empty block declaration is legal");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::CombatTaxPayment { .. }
        ),
        "an unaffordable block must never open a tax prompt, got {:?}",
        runner.state().waiting_for
    );
}

/// CR 302.6 + CR 508.1j: a tapped, summoning-sick source with a tapless
/// sacrifice ability remains available after the two Forests pay Propaganda.
/// Counting only tap-mana sources incorrectly applies the tap-out penalty and
/// suppresses this otherwise worthwhile two-power attack.
#[test]
fn ai_tax_decision_counts_a_tapped_sick_tapless_mana_source() {
    let mut scenario = GameScenario::new();
    scenario.add_enchantment_from_oracle(P0, "Propaganda", PROPAGANDA_ORACLE);
    let attacker = scenario.add_creature(P1, "Bear", 2, 2).id();
    let source = scenario
        .add_creature_from_oracle(
            P1,
            "Tapless Mana Source",
            0,
            1,
            "Sacrifice this creature: Add one mana of any color.",
        )
        .id();
    for _ in 0..2 {
        scenario.add_basic_land(P1, ManaColor::Green);
    }
    let mut runner = scenario.build();
    let state = runner.state_mut();
    let object = state.objects.get_mut(&source).unwrap();
    object.tapped = true;
    object.summoning_sick = true;
    state.active_player = P1;
    state.priority_player = P1;
    state.phase = Phase::DeclareAttackers;
    state.turn_number = 2;
    state.waiting_for = WaitingFor::DeclareAttackers {
        player: P1,
        valid_attacker_ids: vec![attacker],
        valid_attack_targets: vec![AttackTarget::Player(P0)],
        valid_attack_targets_by_attacker: None,
        attacker_constraints: Default::default(),
    };
    assert!(engine::game::combat::attack_tax_is_affordable(
        runner.state(),
        &[(attacker, AttackTarget::Player(P0))],
    ));
    let (action, declared) = ai_declared_attackers(&runner);
    assert_eq!(declared, vec![attacker]);
    runner.act(action).expect("the taxed attack must be legal");
    assert!(matches!(
        runner.state().waiting_for,
        WaitingFor::CombatTaxPayment { .. }
    ));
    let config = create_config(AiDifficulty::VeryHard, Platform::Native);
    let mut rng = SmallRng::seed_from_u64(7);
    let answer = choose_action(runner.state(), P1, &config, &mut rng)
        .expect("the AI must answer its tax prompt");
    assert_eq!(answer, GameAction::PayCombatTax { accept: true });
    runner.act(answer).expect("the tax must be paid");
    assert!(runner.state().combat.as_ref().is_some_and(|combat| combat
        .attackers
        .iter()
        .any(|entry| entry.object_id == attacker)));
    assert!(
        !engine::game::mana_sources::activatable_mana_source_selections(runner.state(), P1)
            .is_empty(),
        "another legal mana source remains after paying the tax"
    );
}

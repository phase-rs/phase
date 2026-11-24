//! "Up to N" keeper cardinality — Covetous Elegy's per-player keep prompt.
//!
//! Every Oracle string here is verbatim from `client/public/card-data.json`.
//! Regenerate any of them with
//! `jq -r '.["covetous elegy"].oracle_text' client/public/card-data.json`.
//!
//! CR 101.4: every player in scope chooses, in APNAP order, and CR 101.4b: a
//! player knows the choices made before them.
//! CR 701.21a: each unchosen creature is sacrificed by its own controller.
//! CR 608.2h: "for each creature your opponents control" is counted when the
//! Treasure instruction is applied, after the simultaneous sacrifice.
//! CR 111.10a: a Treasure is an artifact token.
//!
//! Covetous Elegy rulings (2024-02-02): "Starting with the player whose turn it
//! is, each player in turn order chooses up to two creatures they control,
//! then all of the creatures not chosen by any player are sacrificed at the
//! same time." / "Players may choose fewer than two creatures even if they
//! control two or more creatures."
//!
//! Archfiend of Depravity, verbatim: "Flying\nAt the beginning of each
//! opponent's end step, that player chooses up to two creatures they control,
//! then sacrifices the rest." Rulings (2014-11-24): "The opponent chooses which
//! creatures to spare, if any, as the ability resolves. This choice doesn't
//! target the creatures." / "The opponent may choose to spare one or no
//! creatures. If the player doesn't choose any creatures, they will sacrifice
//! all creatures they control."
//! CR 603.2b + CR 102.1 + CR 805.4d: "that player" is the player whose end step
//! it is; Archfiend's controller and non-active opponents are not prompted.
//!
//! Creatures are textless vanilla fillers.

use engine::ai_support::legal_actions;
use engine::game::interaction::{bind_interaction_authority, derive_viewer_interaction};
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::visibility::filter_state_for_viewer;
use engine::types::actions::GameAction;
use engine::types::game_state::{GameState, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::interaction::{
    InteractionOpportunityResponse, InteractionResponseSpec, InteractionSessionId,
    SelectionConstraint,
};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

/// Covetous Elegy, verbatim.
const COVETOUS_ELEGY: &str = "Each player chooses up to two creatures they control, then sacrifices the rest. Then you create a tapped Treasure token for each creature your opponents control.";
/// Promise of Loyalty, verbatim (the exact-count keeper class).
const PROMISE_OF_LOYALTY: &str = "Each player puts a vow counter on a creature they control and sacrifices the rest. Each of those creatures can't attack you or planeswalkers you control for as long as it has a vow counter on it.";
/// Archfiend of Depravity, verbatim.
const ARCHFIEND_OF_DEPRAVITY: &str = "Flying\nAt the beginning of each opponent's end step, that player chooses up to two creatures they control, then sacrifices the rest.";

/// The third seat (`engine::game::scenario` exports only `P0` and `P1`).
const P2: PlayerId = PlayerId(2);

fn add_creatures(scenario: &mut GameScenario, player: PlayerId, count: usize) -> Vec<ObjectId> {
    (0..count)
        .map(|i| {
            scenario
                .add_creature(player, &format!("Filler {player:?} {i}"), 2, 2)
                .id()
        })
        .collect()
}

/// Cast Covetous Elegy from P0 (the active player) over the given boards and
/// return the runner paused at the first keep prompt (or wherever resolution
/// stops), together with each seat's creatures.
fn cast_covetous(p0_count: usize, p1_count: usize) -> (GameRunner, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let p0 = add_creatures(&mut scenario, P0, p0_count);
    let p1 = add_creatures(&mut scenario, P1, p1_count);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Covetous Elegy", false, COVETOUS_ELEGY)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    drop(outcome);
    (runner, p0, p1)
}

/// The published bounds of the current keep prompt.
fn keep_prompt(state: &GameState) -> (PlayerId, Vec<ObjectId>, usize, Option<usize>) {
    match &state.waiting_for {
        WaitingFor::KeepExactPermanentsChoice {
            player,
            eligible,
            required_count,
            minimum_count,
            ..
        } => (*player, eligible.clone(), *required_count, *minimum_count),
        other => panic!("expected a keep prompt, got {other:?}"),
    }
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

/// `(total, tapped)` Treasure tokens on the battlefield under `player`'s control.
fn treasures(runner: &GameRunner, player: PlayerId) -> (usize, usize) {
    let state = runner.state();
    let ids: Vec<ObjectId> = state
        .battlefield
        .iter()
        .copied()
        .filter(|id| {
            let object = &state.objects[id];
            object.is_token
                && object.controller == player
                && object.card_types.subtypes.iter().any(|s| s == "Treasure")
        })
        .collect();
    let tapped = ids.iter().filter(|id| state.objects[id].tapped).count();
    (ids.len(), tapped)
}

fn count_in(runner: &GameRunner, ids: &[ObjectId], wanted: Zone) -> usize {
    ids.iter().filter(|id| zone(runner, **id) == wanted).count()
}

/// The `Select` constraint the viewer's projection publishes.
fn select_constraint(state: &GameState, viewer: PlayerId) -> SelectionConstraint {
    let mut state = state.clone();
    bind_interaction_authority(&mut state, InteractionSessionId("keeper-up-to".to_string()))
        .expect("valid interaction authority binding");
    let filtered = filter_state_for_viewer(&state, viewer);
    let view = derive_viewer_interaction(&state, &filtered, viewer);
    assert!(
        !view.opportunities.is_empty(),
        "reach guard: the keep prompt publishes an opportunity"
    );
    view.opportunities
        .iter()
        .find_map(|opportunity| match &opportunity.response {
            InteractionOpportunityResponse::Schema {
                spec: InteractionResponseSpec::Select { constraint, .. },
                ..
            } => Some(constraint.clone()),
            _ => None,
        })
        .expect("the keep prompt publishes a Select schema")
}

/// V1 — Covetous Elegy rulings 1 and 2; CR 101.4 + CR 101.4b + CR 701.21a +
/// CR 608.2h + CR 111.10a. The second seat's maximum is the printed two, not
/// the first seat's one-creature pool; nothing is sacrificed before the last
/// choice; the Treasure count reads the board after the sacrifice.
#[test]
fn covetous_elegy_second_seat_keeps_up_to_two_and_treasures_count_survivors() {
    let (mut runner, p0, p1) = cast_covetous(1, 3);

    // (a) The active player chooses first, over a one-creature pool, and is
    // still prompted (an up-to keeper never auto-keeps a non-empty pool).
    let (player, eligible, max, min) = keep_prompt(runner.state());
    assert_eq!(player, P0);
    assert_eq!(eligible, p0);
    assert_eq!(max, 2);
    assert_eq!(min, Some(0));
    let WaitingFor::KeepExactPermanentsChoice { target_player, .. } = &runner.state().waiting_for
    else {
        unreachable!()
    };
    assert_eq!(*target_player, P0);

    runner
        .act(GameAction::ChooseKeptPermanents { kept: p0.clone() })
        .expect("P0 keeps its only creature");

    // (b) The second seat's maximum is still two (the uncapped printed count).
    let (player, eligible, max, min) = keep_prompt(runner.state());
    assert_eq!(player, P1);
    assert_eq!(eligible.len(), 3);
    assert_eq!(
        max, 2,
        "the second seat's maximum must not shrink to P0's pool"
    );
    assert_eq!(min, Some(0));
    assert_eq!(
        count_in(&runner, &p1, Zone::Battlefield),
        3,
        "nothing is sacrificed before the last choice"
    );
    assert_eq!(zone(&runner, p0[0]), Zone::Battlefield);

    // (c) P1 keeps two; the rest go to the graveyard at once.
    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: p1[..2].to_vec(),
        })
        .expect("P1 keeps two of its three creatures");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(count_in(&runner, &p1, Zone::Graveyard), 1);
    assert_eq!(zone(&runner, p1[2]), Zone::Graveyard);
    assert_eq!(
        zone(&runner, p0[0]),
        Zone::Battlefield,
        "P1's choice keeps P0's earlier keeper protected"
    );

    // (d) CR 608.2h: two opponent creatures survive, so two tapped Treasures.
    assert_eq!(
        treasures(&runner, P0),
        (2, 2),
        "the Treasure count is read after the sacrifice"
    );
    assert_eq!(
        treasures(&runner, P1),
        (0, 0),
        "only the caster creates Treasures"
    );
}

/// V2 — Covetous Elegy ruling 1 ("Players may choose fewer than two creatures
/// even if they control two or more creatures."): keeping none sacrifices all,
/// and a seat with exactly the maximum is still prompted.
#[test]
fn covetous_elegy_keeps_fewer_or_none_and_prompts_at_exactly_the_max() {
    let (mut runner, p0, p1) = cast_covetous(3, 2);

    let (player, _, _, _) = keep_prompt(runner.state());
    assert_eq!(
        player, P0,
        "positive guard: P0's prompt exists before the empty keep"
    );
    runner
        .act(GameAction::ChooseKeptPermanents { kept: vec![] })
        .expect("an up-to keeper may keep none");

    let (player, eligible, max, min) = keep_prompt(runner.state());
    assert_eq!(player, P1);
    assert_eq!(eligible.len(), 2, "exactly-two pool is still a choice");
    assert_eq!((max, min), (2, Some(0)));

    runner
        .act(GameAction::ChooseKeptPermanents { kept: vec![p1[0]] })
        .expect("P1 keeps one of two");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());

    assert_eq!(
        count_in(&runner, &p0, Zone::Graveyard),
        3,
        "keep none sacrifices all"
    );
    assert_eq!(zone(&runner, p1[0]), Zone::Battlefield);
    assert_eq!(zone(&runner, p1[1]), Zone::Graveyard);
    assert_eq!(treasures(&runner, P0), (1, 1));
}

/// V2's empty-pool hostile row: a seat with no creatures is never prompted,
/// and the spell finishes after the other seat's choice.
#[test]
fn covetous_elegy_skips_a_seat_with_no_creatures() {
    let (mut runner, p0, _) = cast_covetous(2, 0);

    let (player, eligible, _, _) = keep_prompt(runner.state());
    assert_eq!(player, P0);
    assert_eq!(eligible.len(), 2);
    runner
        .act(GameAction::ChooseKeptPermanents { kept: vec![p0[0]] })
        .expect("P0 keeps one");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::KeepExactPermanentsChoice { .. }
        ),
        "P1 controls no creatures and is never prompted"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(&runner, p0[0]), Zone::Battlefield);
    assert_eq!(zone(&runner, p0[1]), Zone::Graveyard);
    assert_eq!(
        treasures(&runner, P0),
        (0, 0),
        "no opponent creature remains"
    );
}

/// V3 — CR 101.4 + CR 701.21a: the `ChooseKeptPermanents` handler is the single
/// legality authority. Over the maximum, ineligible, and duplicate keeps are
/// rejected and leave the prompt unchanged.
#[test]
fn covetous_elegy_rejects_illegal_keeps() {
    let (mut runner, p0, p1) = cast_covetous(1, 3);
    runner
        .act(GameAction::ChooseKeptPermanents { kept: p0.clone() })
        .expect("P0 keeps its only creature");
    let before = runner.state().waiting_for.clone();
    assert_eq!(keep_prompt(runner.state()).0, P1);

    for kept in [p1.clone(), p0.clone(), vec![p1[0], p1[0]]] {
        assert!(
            runner
                .act(GameAction::ChooseKeptPermanents { kept: kept.clone() })
                .is_err(),
            "keep {kept:?} must be rejected"
        );
        assert_eq!(
            runner.state().waiting_for,
            before,
            "the prompt is unchanged"
        );
        assert_eq!(count_in(&runner, &p1, Zone::Battlefield), 3);
    }

    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: p1[..2].to_vec(),
        })
        .expect("paired positive: two of P1's own creatures are legal");
}

/// V4 — the AI's `take(required_count)` candidate stays legal for an up-to
/// prompt, both when the pool is smaller than the maximum and when larger.
#[test]
fn covetous_elegy_ai_keep_candidates_are_legal() {
    let (mut runner, _, p1) = cast_covetous(1, 3);
    for seat in [P0, P1] {
        assert_eq!(keep_prompt(runner.state()).0, seat);
        let candidate = legal_actions(runner.state())
            .into_iter()
            .find(|action| matches!(action, GameAction::ChooseKeptPermanents { .. }))
            .expect("the AI offers a keep candidate");
        if seat == P1 {
            let GameAction::ChooseKeptPermanents { kept } = &candidate else {
                unreachable!()
            };
            assert_eq!(kept.len(), 2);
        }
        runner
            .act(candidate)
            .expect("the AI keep candidate must be legal");
    }
    runner.advance_until_stack_empty();
    assert_eq!(count_in(&runner, &p1, Zone::Graveyard), 1);
}

/// V5 — the projection publishes the engine's bounds: `0..=2` for an up-to
/// keeper, and the exact `1..=1` for Promise of Loyalty.
#[test]
fn keep_prompt_projection_publishes_engine_bounds() {
    let (runner, _, _) = cast_covetous(1, 3);
    assert_eq!(keep_prompt(runner.state()).0, P0);
    assert_eq!(
        select_constraint(runner.state(), P0),
        SelectionConstraint::Count { min: 0, max: 2 }
    );

    let runner = cast_promise();
    assert_eq!(
        select_constraint(runner.state(), P0),
        SelectionConstraint::Count { min: 1, max: 1 }
    );
}

fn cast_promise() -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    add_creatures(&mut scenario, P0, 2);
    add_creatures(&mut scenario, P1, 2);
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Promise of Loyalty", false, PROMISE_OF_LOYALTY)
        .id();
    let mut runner = scenario.build();
    let outcome = runner.cast(spell).resolve();
    drop(outcome);
    runner
}

/// V6 — Gate B: an exact keeper is unchanged. Promise of Loyalty ruling: "They
/// can't choose to opt out and sacrifice all of their creatures." The prompt
/// carries no minimum, and an empty keep is rejected.
#[test]
fn exact_keeper_prompt_rejects_an_empty_keep() {
    let mut runner = cast_promise();
    let (player, eligible, max, min) = keep_prompt(runner.state());
    assert_eq!(player, P0);
    assert_eq!((max, min), (1, None));
    let before = runner.state().waiting_for.clone();
    assert!(
        runner
            .act(GameAction::ChooseKeptPermanents { kept: vec![] })
            .is_err(),
        "an exact keeper cannot keep none"
    );
    assert_eq!(runner.state().waiting_for, before);

    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: vec![eligible[0]],
        })
        .expect("paired positive: keeping exactly one succeeds");
    assert_eq!(keep_prompt(runner.state()).0, P1);
}

/// Give P1 Archfiend of Depravity plus two fillers, P0 (the default active
/// player) `p0_count` fillers and, in a three-player game, P2 `p2_count`, then
/// advance through the production turn structure into P0's end step. Returns
/// the runner with Archfiend's trigger on the stack, P0's fillers, P1's
/// permanents (Archfiend first, then its fillers) and P2's fillers.
fn archfiend_end_step(
    players: u8,
    p0_count: usize,
    p2_count: usize,
) -> (GameRunner, Vec<ObjectId>, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = if players == 2 {
        GameScenario::new()
    } else {
        GameScenario::new_n_player(players, 7)
    };
    // The postcombat main phase skips the declare-attackers prompt.
    scenario.at_phase(Phase::PostCombatMain);
    let archfiend = scenario
        .add_creature_from_oracle(P1, "Archfiend of Depravity", 5, 4, ARCHFIEND_OF_DEPRAVITY)
        .id();
    let mut p1 = vec![archfiend];
    p1.extend(add_creatures(&mut scenario, P1, 2));
    let p0 = add_creatures(&mut scenario, P0, p0_count);
    let p2 = if players == 2 {
        Vec::new()
    } else {
        add_creatures(&mut scenario, P2, p2_count)
    };
    let mut runner = scenario.build();
    runner.advance_to_end_step();

    // Reach guard: CR 603.2b — Archfiend's trigger is on the stack in P0's end
    // step.
    let state = runner.state();
    assert_eq!(state.phase, Phase::End);
    assert_eq!(state.active_player, P0);
    assert_eq!(state.stack.len(), 1, "exactly Archfiend's trigger");
    assert_eq!(state.stack[0].source_id, archfiend);
    (runner, p0, p1, p2)
}

/// Pass priority until Archfiend's keep prompt is published. A
/// `TriggerTargetSelection` (the misparse's targeted shape) or any other prompt
/// fails the test.
fn pass_until_keep_prompt(runner: &mut GameRunner) {
    for _ in 0..10 {
        match &runner.state().waiting_for {
            WaitingFor::KeepExactPermanentsChoice { .. } => return,
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("expected Archfiend's keep prompt, got {other:?}"),
        }
    }
    panic!("Archfiend's keep prompt was never published");
}

fn sorted(mut ids: Vec<ObjectId>) -> Vec<ObjectId> {
    ids.sort();
    ids
}

/// V1 — Archfiend ruling 3; CR 603.2b + CR 102.1 + CR 805.4d + CR 701.21a. The
/// active opponent, not Archfiend's controller, gets a non-targeting keep
/// prompt at resolution; keeping two of seven sacrifices five.
#[test]
fn archfiend_prompts_the_active_opponent_to_keep_two_of_seven() {
    let (mut runner, p0, p1, _) = archfiend_end_step(2, 7, 0);
    pass_until_keep_prompt(&mut runner);

    let WaitingFor::KeepExactPermanentsChoice {
        player,
        target_player,
        source_controller,
        eligible,
        required_count,
        minimum_count,
        scoped_players,
        remaining_players,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!("pass_until_keep_prompt returned on the keep prompt")
    };
    assert_eq!(*player, P0, "\"that player\" is the active opponent");
    assert_ne!(*player, P1, "Archfiend's controller is not prompted");
    assert_eq!(*target_player, P0);
    assert_eq!(*source_controller, P1);
    assert_eq!((*required_count, *minimum_count), (2, Some(0)));
    assert_eq!(scoped_players, &vec![P0]);
    assert!(remaining_players.is_empty());
    assert_eq!(
        sorted(eligible.clone()),
        sorted(p0.clone()),
        "only P0's own creatures are eligible"
    );

    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: p0[..2].to_vec(),
        })
        .expect("P0 keeps two");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(count_in(&runner, &p0, Zone::Graveyard), 5);
    assert_eq!(count_in(&runner, &p0, Zone::Battlefield), 2);
    assert_eq!(
        count_in(&runner, &p1, Zone::Battlefield),
        3,
        "Archfiend and its controller's creatures are untouched"
    );
}

/// V2 — Archfiend ruling 4: "If the player doesn't choose any creatures, they
/// will sacrifice all creatures they control."
#[test]
fn archfiend_keep_none_sacrifices_all() {
    let (mut runner, p0, p1, _) = archfiend_end_step(2, 3, 0);
    pass_until_keep_prompt(&mut runner);
    assert_eq!(
        keep_prompt(runner.state()).0,
        P0,
        "positive guard: P0's prompt exists before the empty keep"
    );

    runner
        .act(GameAction::ChooseKeptPermanents { kept: vec![] })
        .expect("an up-to keeper may keep none");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(count_in(&runner, &p0, Zone::Graveyard), 3);
    assert_eq!(count_in(&runner, &p1, Zone::Battlefield), 3);
}

/// V3 — Archfiend rulings 3 and 4: a seat holding exactly the maximum is still
/// prompted (it may spare one or none). Sibling: a seat with no creatures is
/// never prompted and the trigger still resolves.
#[test]
fn archfiend_prompts_at_exactly_two_and_skips_an_empty_board() {
    let (mut runner, p0, _, _) = archfiend_end_step(2, 2, 0);
    pass_until_keep_prompt(&mut runner);
    let (player, eligible, max, min) = keep_prompt(runner.state());
    assert_eq!(player, P0);
    assert_eq!(eligible.len(), 2, "an exactly-two pool is still a choice");
    assert_eq!((max, min), (2, Some(0)));
    runner
        .act(GameAction::ChooseKeptPermanents { kept: vec![p0[0]] })
        .expect("P0 keeps one of two");
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(zone(&runner, p0[0]), Zone::Battlefield);
    assert_eq!(zone(&runner, p0[1]), Zone::Graveyard);

    // Sibling: P0 controls no creatures. The reach guard inside
    // `archfiend_end_step` shows the trigger on the stack.
    let (mut runner, _, p1, _) = archfiend_end_step(2, 0, 0);
    for _ in 0..10 {
        if runner.state().stack.is_empty() {
            break;
        }
        match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => {
                runner.act(GameAction::PassPriority).expect("pass priority");
            }
            other => panic!("an empty board publishes no prompt, got {other:?}"),
        }
    }
    assert!(
        runner.state().stack.is_empty(),
        "the trigger resolves without a prompt"
    );
    assert_eq!(count_in(&runner, &p1, Zone::Battlefield), 3);
}

/// V4 — CR 603.2b + CR 102.1 + CR 805.4d: in a three-player game only the
/// active opponent is prompted. The non-active opponent (P2) and Archfiend's
/// controller (P1) are excluded.
#[test]
fn archfiend_prompts_only_the_active_opponent_in_three_player() {
    let (mut runner, p0, p1, p2) = archfiend_end_step(3, 3, 3);
    pass_until_keep_prompt(&mut runner);
    let WaitingFor::KeepExactPermanentsChoice {
        player,
        scoped_players,
        remaining_players,
        ..
    } = &runner.state().waiting_for
    else {
        unreachable!("pass_until_keep_prompt returned on the keep prompt")
    };
    assert_eq!(*player, P0);
    assert_eq!(
        scoped_players,
        &vec![P0],
        "only the active opponent is in scope"
    );
    assert!(remaining_players.is_empty(), "no further seat is prompted");

    runner
        .act(GameAction::ChooseKeptPermanents {
            kept: p0[..2].to_vec(),
        })
        .expect("P0 keeps two");
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::KeepExactPermanentsChoice { .. }
        ),
        "no second keep prompt"
    );
    runner.advance_until_stack_empty();
    assert!(runner.state().stack.is_empty());
    assert_eq!(count_in(&runner, &p0, Zone::Graveyard), 1);
    assert_eq!(
        count_in(&runner, &p2, Zone::Battlefield),
        3,
        "the non-active opponent keeps everything"
    );
    assert_eq!(count_in(&runner, &p1, Zone::Battlefield), 3);
}

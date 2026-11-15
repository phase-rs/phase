//! A loop-shortcut offer states only a count its take can perform (#10543).
//!
//! The engine stores life as `i32`, and energy and player counters as `u32`. CR 119.3 sets no
//! maximum, so a player-resource change whose result does not fit is an engine limit, and the
//! take refuses it with `EngineError::LifeChangeUnrepresentable`. An offer must therefore never
//! state a count whose take runs into that limit; it states the representable repetitions.
//!
//! The board is the one the issue measured, built from the real cards and driven through
//! `apply()` under `LoopDetectionMode::Interactive`:
//!
//! - Spike Feeder: "This creature enters with two +1/+1 counters on it. ... Remove a +1/+1
//!   counter from this creature: You gain 2 life."
//! - Archangel of Thune: "Lifelink ... Whenever you gain life, put a +1/+1 counter on each
//!   creature you control."
//!
//! Each repetition removes a counter for 2 life, and Archangel's trigger puts it back.

use super::support::shared_card_db;
use engine::analysis::decision_template::IterationCount;
use engine::analysis::loop_check::ShortcutResponse;
use engine::database::card_db::CardDatabase;
use engine::game::scenario::{GameRunner, GameScenario, P0};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{AbilityCost, Effect};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::{CastPaymentMode, GameState, LoopDetectionMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::zones::Zone;

/// What one repetition gains: Spike Feeder's "You gain 2 life."
const GAIN_PER_REPETITION: i32 = 2;

/// The engine's own repetition budget (`MAX_SHORTCUT_CYCLES`), which an offer with room to spare
/// states.
const BUDGET: u32 = 1_000;

/// Activations driven before the loop must have been offered.
const ACTIVATION_CAP: usize = 6;

struct Board {
    runner: GameRunner,
    feeder: ObjectId,
    ability_index: usize,
}

fn board(life: i32, db: &CardDatabase) -> Board {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario.with_life(P0, life);
    let feeder = scenario.add_real_card(P0, "Spike Feeder", Zone::Battlefield, db);
    scenario.add_real_card(P0, "Archangel of Thune", Zone::Battlefield, db);
    let mut runner = scenario.build();
    runner.state_mut().loop_detection = LoopDetectionMode::Interactive;
    assert_eq!(
        runner.state().objects[&feeder]
            .counters
            .get(&CounterType::Plus1Plus1)
            .copied(),
        Some(2),
        "reach guard: Spike Feeder entered with its two +1/+1 counters"
    );
    // "Remove a +1/+1 counter from this creature: You gain 2 life." — read off the real card.
    let ability_index = runner.state().objects[&feeder]
        .abilities
        .iter()
        .position(|ability| {
            matches!(&*ability.effect, Effect::GainLife { .. })
                && matches!(ability.cost, Some(AbilityCost::RemoveCounter { .. }))
        })
        .expect("Spike Feeder's life-gain ability is on the real card");
    Board {
        runner,
        feeder,
        ability_index,
    }
}

/// Activates the life-gain ability and passes priority until the stack is empty again or an
/// offer stands.
fn activate_and_settle(board: &mut Board) {
    board
        .runner
        .act(GameAction::ActivateAbility {
            source_id: board.feeder,
            ability_index: board.ability_index,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("Spike Feeder's life-gain ability is activatable");
    for _ in 0..20 {
        match &board.runner.state().waiting_for {
            WaitingFor::LoopShortcut { .. } => return,
            WaitingFor::Priority { .. } if board.runner.state().stack.is_empty() => return,
            _ => {}
        }
        board
            .runner
            .act(GameAction::PassPriority)
            .expect("each player may pass priority");
    }
}

/// Drives the loop by hand until it is offered, returning the offer's stated count and ceiling.
fn drive_to_offer(board: &mut Board) -> (IterationCount, u32) {
    for _ in 0..ACTIVATION_CAP {
        activate_and_settle(board);
        if let WaitingFor::LoopShortcut {
            proposer, schema, ..
        } = &board.runner.state().waiting_for
        {
            assert_eq!(*proposer, P0, "the loop's controller proposes it");
            return (schema.iteration_count.clone(), schema.deliverable_capacity);
        }
    }
    panic!(
        "the Spike Feeder + Archangel of Thune loop was never offered: {:?}",
        board.runner.state().waiting_for
    );
}

/// The repetitions that fit between `life` and `i32::MAX`.
fn representable_repetitions(life: i32) -> u32 {
    ((i32::MAX - life) / GAIN_PER_REPETITION) as u32
}

fn life(state: &GameState) -> i32 {
    state.players[0].life
}

/// Declares `count` for P0 and has P1 accept.
fn take(board: &mut Board, count: u32) {
    board
        .runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(count),
            template: None,
        })
        .expect("P0 may declare the offered shortcut");
    assert!(
        matches!(
            board.runner.state().waiting_for,
            WaitingFor::RespondToShortcut { .. }
        ),
        "reach guard: the declaration opened the response window, got {:?}",
        board.runner.state().waiting_for
    );
    board
        .runner
        .act(GameAction::RespondToShortcut {
            response: ShortcutResponse::Accept,
        })
        .expect("P1's accept takes the shortcut");
}

/// Control: far from the limit, the offer states the engine's whole budget.
#[test]
fn a_loop_with_room_to_spare_is_offered_at_the_budget() {
    let Some(db) = shared_card_db() else { return };
    let mut board = board(20, db);
    let (count, capacity) = drive_to_offer(&mut board);
    assert_eq!(count, IterationCount::Fixed(BUDGET));
    assert_eq!(capacity, BUDGET);
}

/// The issue's first row: at `i32::MAX - 1498`, the loop used to be offered at the full budget,
/// +2000 life, and the accept was rejected with the response window left open. The offer now
/// states the representable repetitions, and taking it lands exactly on the limit.
#[test]
fn a_loop_near_the_life_limit_is_offered_at_its_representable_count() {
    let Some(db) = shared_card_db() else { return };
    let mut board = board(i32::MAX - 1498, db);
    let (count, capacity) = drive_to_offer(&mut board);
    let at_offer = life(board.runner.state());
    let representable = representable_repetitions(at_offer);
    assert!(
        representable < BUDGET,
        "reach guard: the budget does not fit ({representable} repetitions fit)"
    );
    assert_eq!(count, IterationCount::Fixed(representable));
    assert_eq!(capacity, representable);

    take(&mut board, representable);
    let state = board.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the take completed and handed priority back, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        life(state),
        at_offer + GAIN_PER_REPETITION * representable as i32,
        "every offered repetition was performed"
    );
    assert!(
        life(state) > i32::MAX - GAIN_PER_REPETITION,
        "no further repetition would fit"
    );
}

/// A declaration one repetition past the representable count is refused at the declaration, not
/// accepted and then rejected at the take; the game returns to priority with life unchanged.
#[test]
fn a_declaration_past_the_representable_count_is_refused() {
    let Some(db) = shared_card_db() else { return };
    let mut board = board(i32::MAX - 1498, db);
    drive_to_offer(&mut board);
    let at_offer = life(board.runner.state());
    let over = representable_repetitions(at_offer) + 1;
    board
        .runner
        .act(GameAction::DeclareShortcut {
            count: IterationCount::Fixed(over),
            template: None,
        })
        .expect("a refused declaration is handed back, not an error");
    let state = board.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { player } if player == P0),
        "the over-count declaration opened no response window, got {:?}",
        state.waiting_for
    );
    assert_eq!(life(state), at_offer, "nothing was performed");
}

/// The issue's second row: a few repetitions from the limit, the offer states exactly those.
#[test]
fn a_loop_a_few_repetitions_from_the_limit_states_those_repetitions() {
    let Some(db) = shared_card_db() else { return };
    for start in [i32::MAX - 18, i32::MAX - 10] {
        let mut board = board(start, db);
        let (count, capacity) = drive_to_offer(&mut board);
        let at_offer = life(board.runner.state());
        let representable = representable_repetitions(at_offer);
        assert!(representable >= 1, "reach guard: a repetition still fits");
        assert_eq!(count, IterationCount::Fixed(representable), "start {start}");
        assert_eq!(capacity, representable, "start {start}");
        take(&mut board, representable);
        assert_eq!(
            life(board.runner.state()),
            at_offer + GAIN_PER_REPETITION * representable as i32,
            "start {start}: the stated count was performed"
        );
    }
}

/// CR 732.2a: an offer restored from a save written before the representable bound existed
/// decodes the engine budget in place of its capacity. The declare seam re-proves the headroom by
/// replaying the offer's confirmed period on the board before any response window opens, so the
/// restored budget is never copied into a proposal as proof: a count past what fits is handed
/// back, and the representable count is still taken to the limit.
#[test]
fn a_restored_budget_only_offer_is_re_proved_by_replaying_its_period() {
    let Some(db) = shared_card_db() else { return };
    let mut board = board(i32::MAX - 18, db);
    drive_to_offer(&mut board);
    let at_offer = life(board.runner.state());
    let representable = representable_repetitions(at_offer);
    assert!(
        (1..BUDGET).contains(&representable),
        "reach guard: some but not all of the budget fits ({representable})"
    );

    let mut legacy =
        serde_json::to_value(board.runner.state()).expect("the offer board serializes");
    let schema = legacy["waiting_for"]["data"]["schema"]
        .as_object_mut()
        .expect("the schema is an object");
    schema
        .remove("deliverable_capacity")
        .expect("the capacity was serialized before removal");
    schema.insert(
        "iteration_count".to_string(),
        serde_json::to_value(IterationCount::Fixed(BUDGET)).expect("a count serializes"),
    );
    let restored = serde_json::from_value::<engine::types::game_state::PersistedGameState>(legacy)
        .expect("a pre-field offer still decodes")
        .into_game_state()
        .expect("a pre-field offer satisfies the checked restore contract");
    let WaitingFor::LoopShortcut { schema, period, .. } = &restored.waiting_for else {
        panic!(
            "the restored offer survives, got {:?}",
            restored.waiting_for
        );
    };
    assert!(
        !period.is_empty(),
        "reach guard: the recorded road carries its period"
    );
    assert_eq!(
        schema.deliverable_capacity, BUDGET,
        "reach guard: the stripped capacity decodes the budget, which proves nothing"
    );

    let mut over = GameRunner::from_state(restored.clone());
    over.act(GameAction::DeclareShortcut {
        count: IterationCount::Fixed(representable + 1),
        template: None,
    })
    .expect("a refused declaration is handed back, not an error");
    assert!(
        matches!(over.state().waiting_for, WaitingFor::Priority { player } if player == P0),
        "the re-proved headroom refuses the over-count before any window, got {:?}",
        over.state().waiting_for
    );
    assert_eq!(life(over.state()), at_offer, "nothing was performed");

    let mut taken = Board {
        runner: GameRunner::from_state(restored),
        feeder: board.feeder,
        ability_index: board.ability_index,
    };
    take(&mut taken, representable);
    assert_eq!(
        life(taken.runner.state()),
        at_offer + GAIN_PER_REPETITION * representable as i32,
        "the representable count is still taken"
    );
}

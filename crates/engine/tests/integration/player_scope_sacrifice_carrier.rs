//! CR 101.4 + CR 608.2c: the per-player sacrifice collector of "each player
//! sacrifices…" belongs to the one instruction that opened it, and survives a
//! player leaving the game as the rules say it must.
//!
//! * CR 101.4: every player's choice for one instruction is collected in APNAP
//!   order, then the chosen permanents are sacrificed in one action.
//! * CR 608.2c: an instruction's collected choices are never merged into the
//!   answer of another instruction or another resolution.
//! * CR 800.4a + CR 608.2m: when a player leaves the game the instruction still
//!   applies to the players who remain. The departed player makes no further
//!   choice and sacrifices nothing (CR 701.21a: a player sacrifices only what
//!   they control; CR 400.7: a member that left with its owner is a new object),
//!   and the chooser whose turn it is chooses among what they control now.
//! * CR 616.1: a batch parked on a replacement ordering whose chooser left goes
//!   on with the rest of the announced action.
//! * CR 104.1: a finished game owes no sacrifice.
//! * CR 603.2 + CR 603.10a + CR 700.4: a batch the departure completes still
//!   triggers its "dies" observers exactly once, and CR 603.3d + CR 601.2c a
//!   targeted observer's target is still chosen by its controller.
//! * CR 117.3b + CR 800.4j: after an ability's batch is settled by a
//!   departure, the active player receives priority.

use std::collections::HashSet;

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::game::scenario_db::GameScenarioDbExt;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, EffectKind, QuantityExpr, ResolvedAbility,
    TargetFilter, TargetRef,
};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::events::GameEvent;
use engine::types::game_state::{
    CastPaymentMode, GameState, PayCostKind, PendingPlayerScopeSacrificeChoice,
    PendingPlayerScopeSacrificeCompletion, StackEntryKind, WaitingFor,
};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

use crate::support::shared_card_db as load_db;

const P2: PlayerId = PlayerId(2);

fn db() -> &'static engine::database::card_db::CardDatabase {
    load_db().expect("the committed integration card fixture loads")
}

fn mana(color: ManaType, amount: usize) -> Vec<ManaUnit> {
    (0..amount)
        .map(|_| ManaUnit::new(color, ObjectId(0), false, vec![]))
        .collect()
}

fn concede(runner: &mut GameRunner, player: PlayerId) -> Vec<GameEvent> {
    engine::game::engine::apply(
        runner.state_mut(),
        player,
        GameAction::Concede { player_id: player },
    )
    .expect("a player may concede at any time")
    .events
}

/// Pass priority around the table until something other than a priority
/// window is asked.
fn pass_until_a_prompt(runner: &mut GameRunner) {
    for _ in 0..12 {
        if !matches!(runner.state().waiting_for, WaitingFor::Priority { .. }) {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
    }
}

/// Pass priority and answer the resolution's questions until a player holds
/// priority over an empty stack, returning every event. `answer` takes the
/// prompts a test cares about.
fn run_until_settled(
    runner: &mut GameRunner,
    mut answer: impl FnMut(&GameState) -> Option<GameAction>,
) -> Vec<GameEvent> {
    let mut events = Vec::new();
    for _ in 0..40 {
        let state = runner.state();
        let action = match answer(state) {
            Some(action) => action,
            None => match &state.waiting_for {
                WaitingFor::Priority { .. } if state.stack.is_empty() => return events,
                WaitingFor::Priority { .. } => GameAction::PassPriority,
                other => panic!("unexpected prompt while resolving: {other:?}"),
            },
        };
        events.extend(
            runner
                .act(action)
                .expect("the engine accepts the answer")
                .events,
        );
    }
    panic!(
        "the resolution did not settle; waiting for {:?}",
        runner.state().waiting_for
    );
}

/// The resolution is over: priority over an empty stack, no resolution or
/// sacrifice carrier left, and no object stranded in the stack zone.
fn assert_settled(state: &GameState) {
    assert!(
        matches!(state.waiting_for, WaitingFor::Priority { .. }),
        "the game is back at a priority window, got {:?}",
        state.waiting_for
    );
    assert!(state.stack.is_empty(), "the stack is empty");
    assert!(
        state.resolving_stack_entry.is_none(),
        "the resolution carrier was retired"
    );
    assert!(
        state.owed_spell_final_part().is_none(),
        "the owed final part ends with its carrier"
    );
    assert!(
        state.pending_player_scope_sacrifice_choice.is_none(),
        "no player-scope sacrifice carrier is left behind"
    );
    let stranded: Vec<&str> = state
        .objects
        .values()
        .filter(|object| object.zone == Zone::Stack)
        .map(|object| object.name.as_str())
        .collect();
    assert!(
        stranded.is_empty(),
        "no object is left in the stack zone: {stranded:?}"
    );
}

fn stack_resolved_count(events: &[GameEvent], spell: ObjectId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::StackResolved { object_id } if *object_id == spell),
        )
        .count()
}

fn sacrificed_count(events: &[GameEvent], id: ObjectId) -> usize {
    events
        .iter()
        .filter(
            |event| matches!(event, GameEvent::PermanentSacrificed { object_id, .. } if *object_id == id),
        )
        .count()
}

fn zone(runner: &GameRunner, id: ObjectId) -> Zone {
    runner.state().objects[&id].zone
}

fn life(state: &GameState, player: PlayerId) -> i32 {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .life
}

fn hand_size(state: &GameState, player: PlayerId) -> usize {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .hand
        .len()
}

fn graveyard_of(state: &GameState, player: PlayerId) -> Vec<ObjectId> {
    state
        .players
        .iter()
        .find(|p| p.id == player)
        .expect("player exists")
        .graveyard
        .iter()
        .copied()
        .collect()
}

/// The open sacrifice prompt's chooser and offered cards. Panics unless the
/// game is waiting on an `EffectZoneChoice` sacrifice.
fn prompt_cards(state: &GameState) -> (PlayerId, HashSet<ObjectId>) {
    match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            player,
            cards,
            effect_kind: EffectKind::Sacrifice,
            ..
        } => (*player, cards.iter().copied().collect()),
        other => panic!("expected a sacrifice prompt, got {other:?}"),
    }
}

fn set(ids: &[ObjectId]) -> HashSet<ObjectId> {
    ids.iter().copied().collect()
}

fn choose(runner: &mut GameRunner, cards: &[ObjectId]) -> Vec<GameEvent> {
    runner
        .act(GameAction::SelectCards {
            cards: cards.to_vec(),
        })
        .expect("the sacrifice choice is accepted")
        .events
}

fn carrier_selections(state: &GameState) -> Vec<(PlayerId, Vec<ObjectId>)> {
    state
        .pending_player_scope_sacrifice_choice
        .as_ref()
        .expect("reach: a player-scope sacrifice carrier is open")
        .selections
        .clone()
}

/// CR 613.1b: give `taker` control of `id` through a real control-changing
/// effect, which ends when its controller leaves the game (CR 800.4a).
fn steal(runner: &mut GameRunner, id: ObjectId, taker: PlayerId) {
    let state = runner.state_mut();
    state.add_transient_continuous_effect(
        id,
        taker,
        Duration::Permanent,
        TargetFilter::SpecificObject { id },
        vec![ContinuousModification::ChangeController],
        None,
    );
    engine::game::layers::mark_layers_full(state);
    engine::game::layers::evaluate_layers(state);
    assert_eq!(
        runner.state().objects[&id].controller,
        taker,
        "reach: {taker:?} controls {id:?}"
    );
}

/// Pass priority over an empty stack until `player` holds it.
fn pass_priority_to(runner: &mut GameRunner, player: PlayerId) {
    for _ in 0..=runner.state().players.len() {
        let state = runner.state();
        assert!(state.stack.is_empty(), "the stack is empty while passing");
        assert!(
            matches!(state.waiting_for, WaitingFor::Priority { .. }),
            "expected a priority window, got {:?}",
            state.waiting_for
        );
        if state.priority_player == player {
            return;
        }
        runner
            .act(GameAction::PassPriority)
            .expect("passing priority is accepted");
    }
    panic!("{player:?} never received priority");
}

/// Triggered abilities from `source` on the stack.
fn triggers_from(state: &GameState, source: ObjectId) -> usize {
    state
        .stack
        .iter()
        .filter(|entry| {
            matches!(entry.kind, StackEntryKind::TriggeredAbility { .. })
                && entry.source_id == source
        })
        .count()
}

/// Answer any CR 616.1 replacement ordering with its first option.
fn answer_replacements(state: &GameState) -> Option<GameAction> {
    matches!(state.waiting_for, WaitingFor::ReplacementChoice { .. })
        .then_some(GameAction::ChooseReplacement { index: 0 })
}

/// Two Leylines of the Void ("If a card would be put into an opponent's
/// graveyard from anywhere, exile it instead.") under `controller`, so a card
/// bound for an opponent's graveyard meets two replacements (CR 616.1) while
/// `controller`'s own cards go to their graveyard normally.
fn two_leylines(scenario: &mut GameScenario, controller: PlayerId) -> [ObjectId; 2] {
    [
        scenario.add_real_card(controller, "Leyline of the Void", Zone::Battlefield, db()),
        scenario.add_real_card(controller, "Leyline of the Void", Zone::Battlefield, db()),
    ]
}

// ---------------------------------------------------------------------------
// Innocent Blood ("Each player sacrifices a creature of their choice.")
// ---------------------------------------------------------------------------

struct BloodBoard {
    runner: GameRunner,
    blood: ObjectId,
    c0a: ObjectId,
    c0b: ObjectId,
    c1a: ObjectId,
    c1b: ObjectId,
    c2a: ObjectId,
    c2b: ObjectId,
}

/// Three players, P0 active, each controlling two creatures. P0 casts
/// Innocent Blood, and the resolution is paused on P0's own choice.
fn blood_at_p0s_prompt<T>(
    black_mana: usize,
    extra: impl FnOnce(&mut GameScenario) -> T,
) -> (BloodBoard, T) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, black_mana));
    let c0a = scenario.add_creature(P0, "C0a", 2, 2).id();
    let c0b = scenario.add_creature(P0, "C0b", 2, 2).id();
    let c1a = scenario.add_creature(P1, "C1a", 2, 2).id();
    let c1b = scenario.add_creature(P1, "C1b", 2, 2).id();
    let c2a = scenario.add_creature(P2, "C2a", 2, 2).id();
    let c2b = scenario.add_creature(P2, "C2b", 2, 2).id();
    let extra = extra(&mut scenario);
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert_eq!(
        prompt_cards(runner.state()),
        (P0, set(&[c0a, c0b])),
        "reach: Innocent Blood asks P0 first (APNAP)"
    );
    (
        BloodBoard {
            runner,
            blood,
            c0a,
            c0b,
            c1a,
            c1b,
            c2a,
            c2b,
        },
        extra,
    )
}

/// P0 has chosen c0a; P1 is choosing.
fn blood_at_p1s_prompt() -> BloodBoard {
    let (mut board, ()) = blood_at_p0s_prompt(1, |_| ());
    choose(&mut board.runner, &[board.c0a]);
    assert_eq!(
        prompt_cards(board.runner.state()),
        (P1, set(&[board.c1a, board.c1b]))
    );
    assert_eq!(
        carrier_selections(board.runner.state()),
        vec![(P0, vec![board.c0a])],
        "reach: P0's choice is collected, nothing is sacrificed yet"
    );
    board
}

/// T1. CR 800.4a + CR 101.4: the player whose turn it is to choose leaves; their
/// part does nothing and the walk goes on to the next player.
#[test]
fn a_departed_current_chooser_passes_the_choice_to_the_next_player() {
    let mut board = blood_at_p1s_prompt();
    let mut events = concede(&mut board.runner, P1);

    assert_eq!(
        prompt_cards(board.runner.state()),
        (P2, set(&[board.c2a, board.c2b])),
        "CR 101.4: P2 chooses next"
    );
    assert_eq!(zone(&board.runner, board.c0a), Zone::Battlefield);
    assert_eq!(zone(&board.runner, board.blood), Zone::Stack);

    events.extend(choose(&mut board.runner, &[board.c2a]));
    events.extend(run_until_settled(&mut board.runner, |_| None));
    let state = board.runner.state();
    assert_eq!(state.objects[&board.c0a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c2a].zone, Zone::Graveyard);
    assert_eq!(sacrificed_count(&events, board.c0a), 1);
    assert_eq!(sacrificed_count(&events, board.c2a), 1);
    assert_eq!(state.objects[&board.c0b].zone, Zone::Battlefield);
    assert_eq!(state.objects[&board.c2b].zone, Zone::Battlefield);
    assert_eq!(state.objects[&board.c1a].zone, Zone::Exile);
    assert_eq!(state.objects[&board.c1b].zone, Zone::Exile);
    assert_eq!(state.objects[&board.blood].zone, Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

/// T1'. CR 701.21a + CR 400.7: a collected member owned by the departed player
/// leaves with them and is not sacrificed; its chooser does not choose again.
#[test]
fn a_collected_member_that_left_with_its_owner_is_not_sacrificed() {
    let (mut board, borrowed) = blood_at_p0s_prompt_with_borrowed();
    choose(&mut board.runner, &[borrowed]);
    assert_eq!(
        prompt_cards(board.runner.state()),
        (P1, set(&[board.c1a, board.c1b]))
    );
    let mut events = concede(&mut board.runner, P1);

    assert_eq!(
        prompt_cards(board.runner.state()),
        (P2, set(&[board.c2a, board.c2b]))
    );
    events.extend(choose(&mut board.runner, &[board.c2a]));
    events.extend(run_until_settled(&mut board.runner, |_| None));
    let state = board.runner.state();
    assert_eq!(sacrificed_count(&events, board.c2a), 1);
    assert_eq!(state.objects[&board.c2a].zone, Zone::Graveyard);
    assert_eq!(
        sacrificed_count(&events, borrowed),
        0,
        "CR 400.7: the member left with its owner"
    );
    assert_eq!(state.objects[&borrowed].zone, Zone::Exile);
    assert_eq!(
        state.objects[&board.c0a].zone,
        Zone::Battlefield,
        "CR 101.4: P0's choice is spent; P0 does not choose again"
    );
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

fn blood_at_p0s_prompt_with_borrowed() -> (BloodBoard, ObjectId) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let c0a = scenario.add_creature(P0, "C0a", 2, 2).id();
    let c0b = scenario.add_creature(P0, "C0b", 2, 2).id();
    let borrowed = scenario
        .add_creature(P1, "Borrowed", 2, 2)
        .controlled_by(P0)
        .id();
    let c1a = scenario.add_creature(P1, "C1a", 2, 2).id();
    let c1b = scenario.add_creature(P1, "C1b", 2, 2).id();
    let c2a = scenario.add_creature(P2, "C2a", 2, 2).id();
    let c2b = scenario.add_creature(P2, "C2b", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert_eq!(
        prompt_cards(runner.state()),
        (P0, set(&[c0a, c0b, borrowed]))
    );
    (
        BloodBoard {
            runner,
            blood,
            c0a,
            c0b,
            c1a,
            c1b,
            c2a,
            c2b,
        },
        borrowed,
    )
}

/// T2. CR 608.2m + CR 800.4a: the spell's controller leaves at their own
/// prompt. The spell leaves with them, but its instruction goes on for the
/// players still in the game and it is reported resolved once.
#[test]
fn the_spells_controller_leaving_at_their_own_prompt_continues_for_the_others() {
    let (mut board, ()) = blood_at_p0s_prompt(1, |_| ());
    let mut events = concede(&mut board.runner, P0);

    assert_eq!(
        prompt_cards(board.runner.state()),
        (P1, set(&[board.c1a, board.c1b])),
        "CR 608.2m: P1 still chooses for the departed controller's spell"
    );
    assert_eq!(zone(&board.runner, board.blood), Zone::Exile);

    events.extend(choose(&mut board.runner, &[board.c1a]));
    assert_eq!(
        prompt_cards(board.runner.state()),
        (P2, set(&[board.c2a, board.c2b]))
    );
    events.extend(choose(&mut board.runner, &[board.c2a]));
    events.extend(run_until_settled(&mut board.runner, |_| None));
    let state = board.runner.state();
    assert_eq!(state.objects[&board.c1a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c2a].zone, Zone::Graveyard);
    assert_eq!(sacrificed_count(&events, board.c1a), 1);
    assert_eq!(sacrificed_count(&events, board.c2a), 1);
    assert_eq!(state.objects[&board.blood].zone, Zone::Exile);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

/// T3. A player who already chose leaves; their member left with them, and the
/// open prompt stands unchanged for the player choosing now.
#[test]
fn a_player_who_already_chose_leaving_prunes_their_choice() {
    let mut board = blood_at_p1s_prompt();
    let mut events = concede(&mut board.runner, P0);

    assert_eq!(
        prompt_cards(board.runner.state()),
        (P1, set(&[board.c1a, board.c1b]))
    );
    assert!(
        carrier_selections(board.runner.state()).is_empty(),
        "CR 800.4a: the departed player's choice is pruned"
    );

    events.extend(choose(&mut board.runner, &[board.c1a]));
    assert_eq!(
        prompt_cards(board.runner.state()),
        (P2, set(&[board.c2a, board.c2b]))
    );
    events.extend(choose(&mut board.runner, &[board.c2a]));
    events.extend(run_until_settled(&mut board.runner, |_| None));
    let state = board.runner.state();
    assert_eq!(state.objects[&board.c1a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c2a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c0a].zone, Zone::Exile);
    assert_eq!(sacrificed_count(&events, board.c0a), 0);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

/// P2 controls `stolen` creatures owned by P1 plus its own `own` creatures;
/// P0 and P1 control none. P0 casts Innocent Blood, which asks only P2.
fn blood_at_p2s_prompt_over_borrowed(
    stolen: usize,
    own: usize,
) -> (GameRunner, ObjectId, Vec<ObjectId>, Vec<ObjectId>) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let stolen: Vec<ObjectId> = (0..stolen)
        .map(|i| {
            scenario
                .add_creature(P1, &format!("Borrowed {i}"), 2, 2)
                .controlled_by(P2)
                .id()
        })
        .collect();
    let own: Vec<ObjectId> = (0..own)
        .map(|i| scenario.add_creature(P2, &format!("Own {i}"), 2, 2).id())
        .collect();
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    let all: Vec<ObjectId> = stolen.iter().chain(own.iter()).copied().collect();
    assert_eq!(prompt_cards(runner.state()), (P2, set(&all)));
    (runner, blood, stolen, own)
}

/// T4. A living chooser whose other options left with their owner has exactly
/// one creature left: the departure sacrifices it with the batch.
#[test]
fn a_living_chooser_left_with_one_creature_sacrifices_it_on_departure() {
    let (mut runner, blood, stolen, own) = blood_at_p2s_prompt_over_borrowed(2, 1);
    let x = own[0];
    let mut events = concede(&mut runner, P1);

    assert_eq!(sacrificed_count(&events, x), 1);
    assert_eq!(zone(&runner, x), Zone::Graveyard);
    assert!(graveyard_of(runner.state(), P2).contains(&x));
    assert!(
        !matches!(
            runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "the choice is settled, got {:?}",
        runner.state().waiting_for
    );
    for id in &stolen {
        assert_eq!(zone(&runner, *id), Zone::Exile);
    }

    events.extend(run_until_settled(&mut runner, |_| None));
    assert_eq!(zone(&runner, blood), Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, blood), 1);
    assert_settled(runner.state());
}

/// T4b. A living chooser's prompt shrinks to what they still control.
#[test]
fn a_living_choosers_prompt_shrinks_to_what_they_still_control() {
    let (mut runner, blood, stolen, own) = blood_at_p2s_prompt_over_borrowed(1, 2);
    let (s1, x, y) = (stolen[0], own[0], own[1]);
    let arity = |state: &GameState| match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            count,
            min_count,
            up_to,
            ..
        } => (*count, *min_count, *up_to),
        other => panic!("expected a sacrifice prompt, got {other:?}"),
    };
    let before = arity(runner.state());
    assert_eq!(
        (before.0, before.2),
        (1, false),
        "reach: P2 sacrifices exactly one"
    );
    let mut events = concede(&mut runner, P1);

    assert_eq!(prompt_cards(runner.state()), (P2, set(&[x, y])));
    assert_eq!(
        arity(runner.state()),
        before,
        "P2 still sacrifices exactly one, asked the same way"
    );
    assert_eq!(zone(&runner, s1), Zone::Exile);

    events.extend(choose(&mut runner, &[x]));
    events.extend(run_until_settled(&mut runner, |_| None));
    assert_eq!(zone(&runner, x), Zone::Graveyard);
    assert_eq!(sacrificed_count(&events, x), 1);
    assert_eq!(zone(&runner, y), Zone::Battlefield);
    assert_eq!(stack_resolved_count(&events, blood), 1);
    assert_settled(runner.state());
}

/// T4c. CR 800.4a + CR 101.4: a control effect of the departed player ends, the
/// creature returns to the player choosing now, and their prompt offers it.
#[test]
fn a_creature_returned_by_an_ended_control_effect_joins_the_choosers_prompt() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let c1a = scenario.add_creature(P1, "C1a", 2, 2).id();
    let x = scenario.add_creature(P2, "X", 2, 2).id();
    let y = scenario.add_creature(P2, "Y", 2, 2).id();
    let s = scenario.add_creature(P2, "Returned", 2, 2).id();
    let mut runner = scenario.build();
    steal(&mut runner, s, P1);

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert_eq!(prompt_cards(runner.state()), (P1, set(&[c1a, s])));
    let mut events = choose(&mut runner, &[c1a]);
    assert_eq!(prompt_cards(runner.state()), (P2, set(&[x, y])));

    events.extend(concede(&mut runner, P1));
    assert_eq!(
        runner.state().objects[&s].controller,
        P2,
        "CR 800.4a: the departed player's control effect ended"
    );
    assert_eq!(
        prompt_cards(runner.state()),
        (P2, set(&[x, y, s])),
        "CR 101.4: P2 chooses among what P2 controls now"
    );
    assert!(carrier_selections(runner.state()).is_empty());

    events.extend(choose(&mut runner, &[s]));
    events.extend(run_until_settled(&mut runner, |_| None));
    let state = runner.state();
    assert_eq!(state.objects[&s].zone, Zone::Graveyard);
    assert!(graveyard_of(state, P2).contains(&s));
    assert_eq!(sacrificed_count(&events, s), 1);
    assert_eq!(state.objects[&x].zone, Zone::Battlefield);
    assert_eq!(state.objects[&y].zone, Zone::Battlefield);
    assert_eq!(state.objects[&c1a].zone, Zone::Exile);
    assert_eq!(sacrificed_count(&events, c1a), 0);
    assert_eq!(stack_resolved_count(&events, blood), 1);
    assert_settled(state);
}

/// T5. CR 104.1: a game that ends mid-collection owes no sacrifice.
#[test]
fn game_over_clears_the_sacrifice_carrier() {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    let c0a = scenario.add_creature(P0, "C0a", 2, 2).id();
    scenario.add_creature(P0, "C0b", 2, 2);
    scenario.add_creature(P1, "C1a", 2, 2);
    scenario.add_creature(P1, "C1b", 2, 2);
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    choose(&mut runner, &[c0a]);
    assert!(
        runner
            .state()
            .pending_player_scope_sacrifice_choice
            .is_some(),
        "reach: the carrier is open at P1's prompt"
    );

    concede(&mut runner, P1);
    assert_eq!(
        runner.state().waiting_for,
        WaitingFor::GameOver { winner: Some(P0) }
    );
    assert!(runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());
}

/// T6. CR 616.1 + CR 800.4a: the batch is parked on P1's replacement ordering
/// when P1 leaves; the rest of the announced action still happens.
#[test]
fn a_batch_paused_on_a_departed_choosers_replacement_ordering_resumes() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    scenario.add_real_card(P2, "Rest in Peace", Zone::Battlefield, db());
    scenario.add_real_card(P2, "Leyline of the Void", Zone::Battlefield, db());
    let a2 = scenario.add_creature(P2, "A2", 2, 2).id();
    let a1 = scenario.add_creature(P1, "A1", 2, 2).id();
    let b1 = scenario.add_creature(P1, "B1", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert_eq!(prompt_cards(runner.state()), (P1, set(&[a1, b1])));
    choose(&mut runner, &[a1]);
    assert!(
        matches!(runner.state().waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P1),
        "reach: P1 orders Rest in Peace and Leyline, got {:?}",
        runner.state().waiting_for
    );
    let carrier = runner
        .state()
        .pending_player_scope_sacrifice_choice
        .as_ref()
        .expect("reach: the batch is parked");
    assert!(carrier.completion.spans_replacement_pause);
    assert_eq!(carrier.selections, vec![(P2, vec![a2])]);

    let mut events = concede(&mut runner, P1);
    assert_eq!(sacrificed_count(&events, a2), 1);
    assert_eq!(zone(&runner, a2), Zone::Exile, "Rest in Peace exiles it");
    assert_eq!(sacrificed_count(&events, a1), 0);
    assert_eq!(zone(&runner, a1), Zone::Exile);
    assert!(runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());

    events.extend(run_until_settled(&mut runner, answer_replacements));
    assert_eq!(zone(&runner, blood), Zone::Exile);
    assert_eq!(stack_resolved_count(&events, blood), 1);
    assert_settled(runner.state());
}

struct MoldervineBoard {
    runner: GameRunner,
    blood: ObjectId,
    moldervine: ObjectId,
    a1: ObjectId,
    a2: ObjectId,
    life_before: i32,
    hand_before: usize,
}

/// P2 controls two Leylines of the Void, Moldervine Reclamation ("Whenever a
/// creature you control dies, you gain 1 life and draw a card.") and a2; P1
/// controls a1 and b1. Innocent Blood is parked on P1's ordering for a1.
fn moldervine_batch_paused_on_p1() -> MoldervineBoard {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let blood = scenario.add_real_card(P0, "Innocent Blood", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 1));
    two_leylines(&mut scenario, P2);
    let moldervine = scenario.add_real_card(P2, "Moldervine Reclamation", Zone::Battlefield, db());
    let a2 = scenario.add_creature(P2, "A2", 2, 2).id();
    scenario.with_library_top(P2, &["Draw One"]);
    let a1 = scenario.add_creature(P1, "A1", 2, 2).id();
    let b1 = scenario.add_creature(P1, "B1", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(blood).commit();
    pass_until_a_prompt(&mut runner);
    assert_eq!(prompt_cards(runner.state()), (P1, set(&[a1, b1])));
    choose(&mut runner, &[a1]);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P1),
        "reach: both Leylines apply to P1's card, got {:?}",
        state.waiting_for
    );
    let carrier = state
        .pending_player_scope_sacrifice_choice
        .as_ref()
        .expect("reach: the batch is parked");
    assert!(carrier.completion.spans_replacement_pause);
    assert_eq!(carrier.selections, vec![(P2, vec![a2])]);
    assert_eq!(triggers_from(state, moldervine), 0);
    let life_before = life(state, P2);
    let hand_before = hand_size(state, P2);
    MoldervineBoard {
        runner,
        blood,
        moldervine,
        a1,
        a2,
        life_before,
        hand_before,
    }
}

/// Settle the run, recording the most Moldervine triggers seen on the stack.
fn settle_counting_moldervine(board: &mut MoldervineBoard) -> (Vec<GameEvent>, usize) {
    let moldervine = board.moldervine;
    let mut max_seen = 0;
    let events = run_until_settled(&mut board.runner, |state| {
        max_seen = max_seen.max(triggers_from(state, moldervine));
        answer_replacements(state)
    });
    (events, max_seen)
}

/// T6b. CR 603.2 + CR 603.10a + CR 700.4: the batch a departure resumes still
/// triggers its "dies" observer, exactly once.
#[test]
fn a_departure_resumed_batch_triggers_its_dies_observer_once() {
    let mut board = moldervine_batch_paused_on_p1();
    let mut events = concede(&mut board.runner, P1);

    assert_eq!(sacrificed_count(&events, board.a2), 1);
    assert!(
        graveyard_of(board.runner.state(), P2).contains(&board.a2),
        "a2 died: neither Leyline names its controller's own graveyard"
    );
    assert_eq!(sacrificed_count(&events, board.a1), 0);
    assert_eq!(zone(&board.runner, board.a1), Zone::Exile);
    assert!(board
        .runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());

    let (settled, max_seen) = settle_counting_moldervine(&mut board);
    events.extend(settled);
    let state = board.runner.state();
    assert_eq!(max_seen, 1, "CR 603.2: one death, one trigger");
    assert_eq!(life(state, P2), board.life_before + 1);
    assert_eq!(hand_size(state, P2), board.hand_before + 1);
    assert_eq!(state.objects[&board.blood].zone, Zone::Exile);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

/// T6c. The same batch completed by its chooser's answer triggers the observer
/// exactly once too (the answered route collects it; nothing collects it twice).
#[test]
fn an_answered_resumed_batch_triggers_its_dies_observer_once() {
    let mut board = moldervine_batch_paused_on_p1();
    let mut events = board
        .runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P1 orders the replacements")
        .events;

    assert_eq!(zone(&board.runner, board.a1), Zone::Exile);
    assert_eq!(sacrificed_count(&events, board.a1), 1);
    assert!(graveyard_of(board.runner.state(), P2).contains(&board.a2));
    assert_eq!(sacrificed_count(&events, board.a2), 1);

    let (settled, max_seen) = settle_counting_moldervine(&mut board);
    events.extend(settled);
    let state = board.runner.state();
    assert_eq!(max_seen, 1);
    assert_eq!(life(state, P2), board.life_before + 1);
    assert_eq!(hand_size(state, P2), board.hand_before + 1);
    assert_eq!(state.objects[&board.blood].zone, Zone::Exile);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

// ---------------------------------------------------------------------------
// CR 608.2c: a carrier of another instruction is never consumed.
// ---------------------------------------------------------------------------

/// Which instruction an injected stale carrier names.
enum InjectedSource {
    /// The Diabolic Edict being answered.
    Edict,
    /// An unrelated object.
    Bystander,
}

/// Which resolution an injected stale carrier recorded.
enum InjectedEntry {
    /// The resolution under way.
    Current,
    /// Another resolution.
    Other,
    /// No provenance (an older save).
    Legacy,
}

struct EdictInjection {
    events: Vec<GameEvent>,
    runner: GameRunner,
    edict: ObjectId,
    bystander: ObjectId,
    first: ObjectId,
}

/// T7. Diabolic Edict ("Target player sacrifices a creature of their
/// choice.") at P1, answered while a stale collecting carrier is injected.
fn answer_edict_over_injected_carrier(
    source: InjectedSource,
    entry: InjectedEntry,
) -> EdictInjection {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    let edict = scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db());
    scenario.with_mana_pool(P0, mana(ManaType::Black, 2));
    let bystander = scenario.add_creature(P0, "Bystander", 2, 2).id();
    let first = scenario.add_creature(P1, "First", 2, 2).id();
    let second = scenario.add_creature(P1, "Second", 2, 2).id();
    let mut runner = scenario.build();

    runner.cast(edict).target_player(P1).commit();
    pass_until_a_prompt(&mut runner);
    assert_eq!(prompt_cards(runner.state()), (P1, set(&[first, second])));
    assert!(runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());
    let current = runner
        .state()
        .resolving_stack_entry
        .as_ref()
        .expect("reach: the Edict is resolving")
        .id;
    let src = match source {
        InjectedSource::Edict => edict,
        InjectedSource::Bystander => bystander,
    };
    let resolving_entry = match entry {
        InjectedEntry::Current => Some(current),
        InjectedEntry::Other => Some(bystander),
        InjectedEntry::Legacy => None,
    };
    runner.state_mut().pending_player_scope_sacrifice_choice =
        Some(PendingPlayerScopeSacrificeChoice {
            ability: Box::new(ResolvedAbility::new(
                Effect::Sacrifice {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 1,
                },
                vec![],
                src,
                P0,
            )),
            remaining_players: vec![],
            selections: vec![(P0, vec![bystander])],
            completion: Default::default(),
            resolving_entry,
        });

    let events = choose(&mut runner, &[first]);
    EdictInjection {
        events,
        runner,
        edict,
        bystander,
        first,
    }
}

fn assert_stale_carrier_discarded(outcome: &EdictInjection) {
    let state = outcome.runner.state();
    assert_eq!(
        state.objects[&outcome.bystander].zone,
        Zone::Battlefield,
        "CR 608.2c: another instruction's choice is never merged into this answer"
    );
    assert_eq!(sacrificed_count(&outcome.events, outcome.bystander), 0);
    assert_eq!(state.objects[&outcome.first].zone, Zone::Graveyard);
    assert_eq!(sacrificed_count(&outcome.events, outcome.first), 1);
    assert!(state.pending_player_scope_sacrifice_choice.is_none());
    assert_eq!(state.objects[&outcome.edict].zone, Zone::Graveyard);
}

fn assert_carrier_consumed(outcome: &EdictInjection) {
    let state = outcome.runner.state();
    assert_eq!(
        state.objects[&outcome.bystander].zone,
        Zone::Graveyard,
        "reach: the carrier of this instruction is consumed by its answer"
    );
    assert_eq!(sacrificed_count(&outcome.events, outcome.bystander), 1);
    assert_eq!(state.objects[&outcome.first].zone, Zone::Graveyard);
}

#[test]
fn an_answer_ignores_a_carrier_of_another_source() {
    let outcome =
        answer_edict_over_injected_carrier(InjectedSource::Bystander, InjectedEntry::Current);
    assert_stale_carrier_discarded(&outcome);
}

#[test]
fn an_answer_ignores_a_carrier_of_another_resolution() {
    let outcome = answer_edict_over_injected_carrier(InjectedSource::Edict, InjectedEntry::Other);
    assert_stale_carrier_discarded(&outcome);
}

#[test]
fn an_answer_ignores_a_legacy_carrier_of_another_source() {
    let outcome =
        answer_edict_over_injected_carrier(InjectedSource::Bystander, InjectedEntry::Legacy);
    assert_stale_carrier_discarded(&outcome);
}

#[test]
fn an_answer_consumes_its_own_carrier() {
    let outcome = answer_edict_over_injected_carrier(InjectedSource::Edict, InjectedEntry::Current);
    assert_carrier_consumed(&outcome);
}

#[test]
fn an_answer_consumes_its_own_legacy_carrier() {
    let outcome = answer_edict_over_injected_carrier(InjectedSource::Edict, InjectedEntry::Legacy);
    assert_carrier_consumed(&outcome);
}

/// The shape and provenance of a parked-batch carrier injected under a
/// replacement ordering.
enum ParkedInjection {
    /// Parked, recorded under another resolution.
    OtherResolution,
    /// A collecting carrier (no provenance), which no replacement resumes.
    CollectingShape,
    /// Parked, recorded under the resolution under way.
    Binding,
    /// Parked, no provenance.
    LegacyBinding,
}

/// T8. "Plain Draw" ("Draw a card.") cast by P0 while P1 controls Rest in
/// Peace and Leyline of the Void: its own move off the stack asks P0 to order
/// the two replacements (CR 616.1). A parked sacrifice carrier is injected at
/// that choice, which P0 then answers.
fn answer_ordering_over_injected_batch(
    injection: ParkedInjection,
) -> (GameRunner, ObjectId, Vec<GameEvent>) {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    scenario.add_real_card(P1, "Rest in Peace", Zone::Battlefield, db());
    scenario.add_real_card(P1, "Leyline of the Void", Zone::Battlefield, db());
    let victim = scenario.add_creature(P1, "Victim", 2, 2).id();
    let spell = scenario
        .add_spell_to_hand_from_oracle(P0, "Plain Draw", false, "Draw a card.")
        .id();
    scenario.with_library_top(P0, &["Draw One", "Pad"]);
    let mut runner = scenario.build();

    runner.cast(spell).commit();
    pass_until_a_prompt(&mut runner);
    let state = runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P0),
        "reach: the spell's own move asks P0 to order the replacements, got {:?}",
        state.waiting_for
    );
    assert_eq!(
        state.resolving_stack_entry.as_ref().map(|entry| entry.id),
        Some(spell)
    );
    let (spans_replacement_pause, resolving_entry) = match injection {
        ParkedInjection::OtherResolution => (true, Some(victim)),
        ParkedInjection::CollectingShape => (false, None),
        ParkedInjection::Binding => (true, Some(spell)),
        ParkedInjection::LegacyBinding => (true, None),
    };
    runner.state_mut().pending_player_scope_sacrifice_choice =
        Some(PendingPlayerScopeSacrificeChoice {
            ability: Box::new(ResolvedAbility::new(
                Effect::Sacrifice {
                    target: TargetFilter::Any,
                    count: QuantityExpr::Fixed { value: 1 },
                    min_count: 1,
                },
                vec![],
                spell,
                P0,
            )),
            remaining_players: vec![],
            selections: vec![(P1, vec![victim])],
            completion: PendingPlayerScopeSacrificeCompletion {
                announced: vec![victim],
                spans_replacement_pause,
                ..Default::default()
            },
            resolving_entry,
        });

    let events = runner
        .act(GameAction::ChooseReplacement { index: 0 })
        .expect("P0 orders the replacements")
        .events;
    (runner, victim, events)
}

fn assert_parked_batch_discarded(injection: ParkedInjection) {
    let (runner, victim, events) = answer_ordering_over_injected_batch(injection);
    assert_eq!(
        zone(&runner, victim),
        Zone::Battlefield,
        "CR 608.2c + CR 616.1: only the resolution under way's parked batch resumes"
    );
    assert_eq!(sacrificed_count(&events, victim), 0);
    assert!(runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());
}

fn assert_parked_batch_resumed(injection: ParkedInjection) {
    let (runner, victim, events) = answer_ordering_over_injected_batch(injection);
    assert_eq!(
        zone(&runner, victim),
        Zone::Exile,
        "reach: the replacement answer resumes this resolution's parked batch"
    );
    assert_eq!(sacrificed_count(&events, victim), 1);
}

#[test]
fn a_replacement_answer_ignores_a_batch_of_another_resolution() {
    assert_parked_batch_discarded(ParkedInjection::OtherResolution);
}

#[test]
fn a_replacement_answer_ignores_a_collecting_carrier() {
    assert_parked_batch_discarded(ParkedInjection::CollectingShape);
}

#[test]
fn a_replacement_answer_resumes_its_own_parked_batch() {
    assert_parked_batch_resumed(ParkedInjection::Binding);
}

#[test]
fn a_replacement_answer_resumes_its_own_legacy_parked_batch() {
    assert_parked_batch_resumed(ParkedInjection::LegacyBinding);
}

/// T9. A player not yet asked leaves: they are dropped from the walk and the
/// open prompt stands.
#[test]
fn a_not_yet_prompted_player_leaving_is_dropped_from_the_walk() {
    let mut board = blood_at_p1s_prompt();
    let mut events = concede(&mut board.runner, P2);

    assert_eq!(
        prompt_cards(board.runner.state()),
        (P1, set(&[board.c1a, board.c1b]))
    );
    assert!(
        board
            .runner
            .state()
            .pending_player_scope_sacrifice_choice
            .as_ref()
            .expect("the carrier is still open")
            .remaining_players
            .is_empty(),
        "CR 800.4a: the departed player is no longer asked"
    );

    events.extend(choose(&mut board.runner, &[board.c1a]));
    events.extend(run_until_settled(&mut board.runner, |_| None));
    let state = board.runner.state();
    assert_eq!(sacrificed_count(&events, board.c0a), 1);
    assert_eq!(sacrificed_count(&events, board.c1a), 1);
    assert_eq!(state.objects[&board.c0a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c1a].zone, Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_settled(state);
}

/// T9b. CR 800.4a + CR 101.4 + CR 117.3b: the last chooser of a spell's batch
/// leaves at their own prompt. The departure sacrifices the collected batch,
/// the spell resolves once, and priority never names the departed player; the
/// active player receives it.
#[test]
fn a_spells_batch_settled_by_its_last_choosers_departure_never_gives_them_priority() {
    let mut board = blood_at_p1s_prompt();
    let mut events = choose(&mut board.runner, &[board.c1a]);
    assert_eq!(
        prompt_cards(board.runner.state()),
        (P2, set(&[board.c2a, board.c2b])),
        "reach: P2 is the last chooser"
    );
    assert_eq!(
        carrier_selections(board.runner.state()),
        vec![(P0, vec![board.c0a]), (P1, vec![board.c1a])],
        "reach: P0's and P1's choices are collected, nothing is sacrificed yet"
    );

    events.extend(concede(&mut board.runner, P2));
    let state = board.runner.state();
    assert!(
        engine::game::players::is_alive(state, state.priority_player),
        "CR 800.4a: priority never names a player who has left, got {:?}",
        state.priority_player
    );
    if let Some(acting) = state.waiting_for.acting_player() {
        assert!(
            engine::game::players::is_alive(state, acting),
            "CR 800.4a: the departed player is never asked, got {:?}",
            state.waiting_for
        );
    }
    assert!(
        !matches!(state.waiting_for, WaitingFor::EffectZoneChoice { .. }),
        "the batch is settled, got {:?}",
        state.waiting_for
    );
    assert!(state.pending_player_scope_sacrifice_choice.is_none());

    events.extend(run_until_settled(&mut board.runner, |_| None));
    let state = board.runner.state();
    assert_eq!(sacrificed_count(&events, board.c0a), 1);
    assert_eq!(sacrificed_count(&events, board.c1a), 1);
    assert_eq!(state.objects[&board.c0a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c1a].zone, Zone::Graveyard);
    assert_eq!(state.objects[&board.c0b].zone, Zone::Battlefield);
    assert_eq!(state.objects[&board.c1b].zone, Zone::Battlefield);
    for id in [board.c2a, board.c2b] {
        assert_eq!(sacrificed_count(&events, id), 0);
        assert_eq!(state.objects[&id].zone, Zone::Exile);
    }
    assert_eq!(state.objects[&board.blood].zone, Zone::Graveyard);
    assert_eq!(stack_resolved_count(&events, board.blood), 1);
    assert_eq!(
        state.waiting_for,
        WaitingFor::Priority { player: P0 },
        "CR 117.3b: the active player receives priority"
    );
    assert_eq!(state.priority_player, P0);
    assert_settled(state);
}

/// T10. CR 608.2c: after a concession mid-collection, a later and unrelated
/// sacrifice prompt (Diabolic Edict) never picks up Innocent Blood's choices.
#[test]
fn a_later_sacrifice_prompt_never_merges_an_earlier_instructions_choices() {
    let (mut board, edict) = blood_at_p0s_prompt(3, |scenario| {
        // A third creature keeps P2 choosing for the later Edict.
        scenario.add_creature(P2, "C2c", 2, 2);
        scenario.add_real_card(P0, "Diabolic Edict", Zone::Hand, db())
    });
    choose(&mut board.runner, &[board.c0a]);
    assert_eq!(
        carrier_selections(board.runner.state()),
        vec![(P0, vec![board.c0a])],
        "reach: P0's choice is collected"
    );
    concede(&mut board.runner, P1);

    let blood = board.blood;
    run_until_settled(&mut board.runner, |state| match &state.waiting_for {
        WaitingFor::EffectZoneChoice {
            source_id,
            cards,
            count,
            ..
        } if *source_id == blood => Some(GameAction::SelectCards {
            cards: cards.iter().copied().take(*count).collect(),
        }),
        _ => None,
    });
    pass_priority_to(&mut board.runner, P0);

    board.runner.cast(edict).target_player(P2).commit();
    pass_until_a_prompt(&mut board.runner);
    let (chooser, cards) = prompt_cards(board.runner.state());
    assert_eq!(chooser, P2);
    assert!(
        matches!(board.runner.state().waiting_for, WaitingFor::EffectZoneChoice { source_id, .. } if source_id == edict),
        "reach: the Edict's own prompt"
    );
    assert!(cards.contains(&board.c2b));

    let answered = choose(&mut board.runner, &[board.c2b]);
    let sacrificed: Vec<ObjectId> = answered
        .iter()
        .filter_map(|event| match event {
            GameEvent::PermanentSacrificed { object_id, .. } => Some(*object_id),
            _ => None,
        })
        .collect();
    assert_eq!(
        sacrificed,
        vec![board.c2b],
        "CR 608.2c: the Edict's answer sacrifices exactly the creature P2 chose"
    );
    assert_eq!(sacrificed_count(&answered, board.c0a), 0);
    assert!(
        !matches!(
            board.runner.state().waiting_for,
            WaitingFor::EffectZoneChoice { .. }
        ),
        "nothing re-prompts after the Edict's answer, got {:?}",
        board.runner.state().waiting_for
    );
    assert!(board
        .runner
        .state()
        .pending_player_scope_sacrifice_choice
        .is_none());

    run_until_settled(&mut board.runner, |_| None);
    assert_settled(board.runner.state());
}

// ---------------------------------------------------------------------------
// Grave Pact ("Whenever a creature you control dies, each other player
// sacrifices a creature of their choice."): an ability-sourced instruction.
// ---------------------------------------------------------------------------

/// The Grave Pact board's variants.
enum GravePactBoard {
    /// P0 controls two Leylines of the Void; P2 has no creatures, so the
    /// batch parks on P1's CR 616.1 ordering for a1.
    PausedOnP1,
    /// No Leylines; P2 controls a2 and b2, so P2 chooses after P1.
    OpenAtP2,
    /// `OpenAtP2` plus P0's Blood Artist ("Whenever this creature or another
    /// creature dies, target player loses 1 life and you gain 1 life.").
    OpenAtP2WatchedByBloodArtist,
}

struct GravePactRun {
    runner: GameRunner,
    feeder: ObjectId,
    a1: ObjectId,
    a2: Option<ObjectId>,
    b2: Option<ObjectId>,
    artist: Option<ObjectId>,
}

/// Answer only the prompts g's cost sacrifice raised: order Grave Pact's
/// trigger first (it resolves last) and Blood Artist's second, and target
/// `target` with Blood Artist's.
fn place_cost_triggers(
    runner: &mut GameRunner,
    grave_pact: ObjectId,
    artist: ObjectId,
    target: PlayerId,
) {
    for _ in 0..8 {
        let action = match &runner.state().waiting_for {
            WaitingFor::Priority { .. } => return,
            WaitingFor::OrderTriggers { triggers, .. } => {
                assert_eq!(triggers.len(), 2, "both P0 triggers are ordered");
                let pact = triggers
                    .iter()
                    .position(|trigger| trigger.source_id == grave_pact)
                    .expect("Grave Pact's trigger is ordered");
                let art = triggers
                    .iter()
                    .position(|trigger| trigger.source_id == artist)
                    .expect("Blood Artist's trigger is ordered");
                GameAction::OrderTriggers {
                    order: vec![pact, art],
                }
            }
            WaitingFor::TriggerTargetSelection {
                source_id: Some(source),
                target_slots,
                selection,
                ..
            } if *source == artist => {
                assert!(
                    target_slots[selection.current_slot]
                        .legal_targets
                        .contains(&TargetRef::Player(target)),
                    "{target:?} is a legal target"
                );
                GameAction::ChooseTarget {
                    target: Some(TargetRef::Player(target)),
                }
            }
            other => panic!("unexpected prompt while placing the cost triggers: {other:?}"),
        };
        runner
            .act(action)
            .expect("the trigger placement answer is accepted");
    }
    panic!(
        "the cost triggers were not placed; waiting for {:?}",
        runner.state().waiting_for
    );
}

/// Three players, P0 active. P0 controls Grave Pact, Carrion Feeder
/// ("Sacrifice a creature: Put a +1/+1 counter on this creature.") and g; P1
/// controls a1 and b1. P0 activates the Feeder sacrificing g, and Grave Pact's
/// trigger resolves to P1's prompt.
fn grave_pact_board(board: GravePactBoard) -> GravePactRun {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);
    let grave_pact = scenario.add_real_card(P0, "Grave Pact", Zone::Battlefield, db());
    let feeder = scenario.add_real_card(P0, "Carrion Feeder", Zone::Battlefield, db());
    let g = scenario.add_creature(P0, "Fodder", 1, 1).id();
    let a1 = scenario.add_creature(P1, "A1", 2, 2).id();
    let b1 = scenario.add_creature(P1, "B1", 2, 2).id();
    let (mut a2, mut b2, mut artist) = (None, None, None);
    match board {
        GravePactBoard::PausedOnP1 => {
            two_leylines(&mut scenario, P0);
        }
        GravePactBoard::OpenAtP2 => {
            a2 = Some(scenario.add_creature(P2, "A2", 2, 2).id());
            b2 = Some(scenario.add_creature(P2, "B2", 2, 2).id());
        }
        GravePactBoard::OpenAtP2WatchedByBloodArtist => {
            a2 = Some(scenario.add_creature(P2, "A2", 2, 2).id());
            b2 = Some(scenario.add_creature(P2, "B2", 2, 2).id());
            artist = Some(scenario.add_real_card(P0, "Blood Artist", Zone::Battlefield, db()));
        }
    }
    let mut runner = scenario.build();

    runner
        .act(GameAction::ActivateAbility {
            source_id: feeder,
            ability_index: 0,
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("reach: Carrion Feeder's ability is activatable");
    match &runner.state().waiting_for {
        WaitingFor::PayCost {
            kind: PayCostKind::Sacrifice,
            choices,
            ..
        } => assert!(choices.contains(&g), "reach: g is offered to the cost"),
        other => panic!("expected the sacrifice cost, got {other:?}"),
    }
    runner
        .act(GameAction::SelectCards { cards: vec![g] })
        .expect("g is sacrificed as the cost");
    assert!(
        graveyard_of(runner.state(), P0).contains(&g),
        "reach: g died (P0's own Leylines do not name P0's graveyard)"
    );

    if let Some(artist) = artist {
        place_cost_triggers(&mut runner, grave_pact, artist, P1);
        assert_eq!(triggers_from(runner.state(), artist), 1);
    }
    let state = runner.state();
    assert_eq!(triggers_from(state, grave_pact), 1);
    assert!(
        matches!(
            state.stack.front().map(|entry| (&entry.kind, entry.source_id)),
            Some((StackEntryKind::ActivatedAbility { .. }, source)) if source == feeder
        ),
        "reach: the Feeder's ability is below the trigger"
    );
    let life_p1_before_artist = life(state, P1);

    pass_until_a_prompt(&mut runner);
    let state = runner.state();
    assert_eq!(prompt_cards(state), (P1, set(&[a1, b1])));
    assert!(
        matches!(state.waiting_for, WaitingFor::EffectZoneChoice { source_id, .. } if source_id == grave_pact),
        "reach: Grave Pact's own prompt"
    );
    if let Some(artist) = artist {
        assert_eq!(triggers_from(state, artist), 0);
        assert_eq!(
            life(state, P1),
            life_p1_before_artist - 1,
            "reach: the setup's Blood Artist trigger resolved before Grave Pact's"
        );
    }
    GravePactRun {
        runner,
        feeder,
        a1,
        a2,
        b2,
        artist,
    }
}

fn plus_one_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner.state().objects[&id]
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn assert_only_the_feeders_ability_on_the_stack(run: &GravePactRun) {
    let state = run.runner.state();
    assert_eq!(state.stack.len(), 1, "only the Feeder's ability remains");
    assert!(matches!(
        state.stack[0].kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!(state.stack[0].source_id, run.feeder);
}

/// T11. CR 616.1 + CR 800.4a + CR 117.3b: the ability's batch is parked on P1's
/// ordering when P1 leaves; the departure finishes it, settles the trigger, and
/// the active player receives priority.
#[test]
fn an_ability_batch_resumed_by_departure_gives_priority_to_the_active_player() {
    let mut run = grave_pact_board(GravePactBoard::PausedOnP1);
    choose(&mut run.runner, &[run.a1]);
    let state = run.runner.state();
    assert!(
        matches!(state.waiting_for, WaitingFor::ReplacementChoice { player, .. } if player == P1),
        "reach: P1 orders the two Leylines, got {:?}",
        state.waiting_for
    );
    let carrier = state
        .pending_player_scope_sacrifice_choice
        .as_ref()
        .expect("reach: the batch is parked");
    assert!(carrier.completion.spans_replacement_pause);
    assert!(carrier.selections.is_empty());
    assert!(matches!(
        state
            .resolving_stack_entry
            .as_ref()
            .map(|entry| &entry.kind),
        Some(StackEntryKind::TriggeredAbility { .. })
    ));

    let events = concede(&mut run.runner, P1);
    let state = run.runner.state();
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(state.priority_player, P0);
    assert!(state.resolving_stack_entry.is_none());
    assert!(state.pending_player_scope_sacrifice_choice.is_none());
    assert_only_the_feeders_ability_on_the_stack(&run);
    assert_eq!(zone(&run.runner, run.a1), Zone::Exile);
    assert_eq!(sacrificed_count(&events, run.a1), 0);

    run_until_settled(&mut run.runner, answer_replacements);
    assert_eq!(plus_one_counters(&run.runner, run.feeder), 1);
    assert_settled(run.runner.state());
}

/// P1 has chosen a1; P2 is choosing.
fn grave_pact_at_p2s_prompt(board: GravePactBoard) -> GravePactRun {
    let mut run = grave_pact_board(board);
    choose(&mut run.runner, &[run.a1]);
    let a2 = run.a2.expect("P2 controls a2");
    let b2 = run.b2.expect("P2 controls b2");
    assert_eq!(prompt_cards(run.runner.state()), (P2, set(&[a2, b2])));
    assert_eq!(
        carrier_selections(run.runner.state()),
        vec![(P1, vec![run.a1])]
    );
    run
}

/// T11b. CR 800.4a + CR 101.4 + CR 117.3b: the last chooser leaves at their
/// prompt; the departure sacrifices the collected batch, settles the trigger,
/// and the active player receives priority.
#[test]
fn an_ability_batch_settled_by_departure_gives_priority_to_the_active_player() {
    let mut run = grave_pact_at_p2s_prompt(GravePactBoard::OpenAtP2);
    let events = concede(&mut run.runner, P2);

    let state = run.runner.state();
    assert_eq!(sacrificed_count(&events, run.a1), 1);
    assert!(graveyard_of(state, P1).contains(&run.a1));
    assert_eq!(state.objects[&run.a2.unwrap()].zone, Zone::Exile);
    assert_eq!(state.objects[&run.b2.unwrap()].zone, Zone::Exile);
    assert!(state.pending_player_scope_sacrifice_choice.is_none());
    assert!(state.resolving_stack_entry.is_none());
    assert_only_the_feeders_ability_on_the_stack(&run);
    let state = run.runner.state();
    assert_eq!(state.waiting_for, WaitingFor::Priority { player: P0 });
    assert_eq!(state.priority_player, P0);

    run_until_settled(&mut run.runner, answer_replacements);
    assert_eq!(plus_one_counters(&run.runner, run.feeder), 1);
    assert_settled(run.runner.state());
}

/// T11c. CR 603.2 + CR 700.4 + CR 603.3d + CR 601.2c: the batch a departure
/// settles kills a1, which triggers P0's Blood Artist; P0 is asked for its
/// target, and the trigger resolves once.
#[test]
fn a_departure_settled_batch_asks_for_its_observers_target() {
    let mut run = grave_pact_at_p2s_prompt(GravePactBoard::OpenAtP2WatchedByBloodArtist);
    let artist = run.artist.expect("Blood Artist is on the board");
    let (l0, l1) = (life(run.runner.state(), P0), life(run.runner.state(), P1));
    let events = concede(&mut run.runner, P2);

    let state = run.runner.state();
    assert_eq!(sacrificed_count(&events, run.a1), 1);
    assert!(graveyard_of(state, P1).contains(&run.a1));
    assert_eq!(state.objects[&run.a2.unwrap()].zone, Zone::Exile);
    assert_eq!(state.objects[&run.b2.unwrap()].zone, Zone::Exile);
    assert!(state.pending_player_scope_sacrifice_choice.is_none());
    assert!(state.resolving_stack_entry.is_none());
    match &state.waiting_for {
        WaitingFor::TriggerTargetSelection {
            player,
            source_id,
            target_slots,
            selection,
            ..
        } => {
            assert_eq!(*player, P0);
            assert_eq!(*source_id, Some(artist));
            assert!(target_slots[selection.current_slot]
                .legal_targets
                .contains(&TargetRef::Player(P1)));
        }
        other => panic!("CR 603.3d + CR 601.2c: P0 chooses Blood Artist's target, got {other:?}"),
    }
    assert!(state.pending_trigger.is_some());
    // CR 603.3d + CR 601.2c: the trigger is on the stack, above the Feeder's
    // ability, while its target is chosen ("removed from the stack" if none can be).
    assert_eq!(state.stack.len(), 2);
    assert_eq!(state.stack[0].source_id, run.feeder);
    assert!(matches!(
        state.stack[0].kind,
        StackEntryKind::ActivatedAbility { .. }
    ));
    assert_eq!(state.stack[1].source_id, artist);
    assert_eq!(triggers_from(state, artist), 1, "one death, one trigger");
    assert_eq!(life(state, P0), l0);
    assert_eq!(life(state, P1), l1);

    run.runner
        .act(GameAction::ChooseTarget {
            target: Some(TargetRef::Player(P1)),
        })
        .expect("P0 targets P1");
    let state = run.runner.state();
    assert!(matches!(state.waiting_for, WaitingFor::Priority { .. }));
    assert_eq!(triggers_from(state, artist), 1);
    assert_eq!(state.stack[0].source_id, run.feeder);

    let mut max_seen = 0;
    run_until_settled(&mut run.runner, |state| {
        max_seen = max_seen.max(triggers_from(state, artist));
        answer_replacements(state)
    });
    let state = run.runner.state();
    assert_eq!(max_seen, 1);
    assert_eq!(life(state, P1), l1 - 1);
    assert_eq!(life(state, P0), l0 + 1);
    assert_eq!(plus_one_counters(&run.runner, run.feeder), 1);
    assert_settled(state);
}

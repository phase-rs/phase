//! A "this way" gate detached from a `player_scope` clause reads the WHOLE
//! clause, even when a seat's choice pauses it between players (#9225).
//!
//! CR anchors:
//!   - CR 608.2f: "each player exiles a card from their graveyard" is one action
//!     taken on multiple players; when a choice makes it non-simultaneous it is
//!     processed player by player, but it stays one action.
//!   - CR 608.2c: the following "if a nonland card was exiled this way" is the
//!     next instruction, so it reads every player's exile, once, and its "you"
//!     is the ability's controller (CR 109.5).
//!
//! Every seat below holds a nonland AND a land card, so each is prompted and
//! the clause pauses after every seat. A tail that read only the answer that
//! resumed it would see just the last seat's card. SYNTHETIC trigger text: the
//! printed card with this clause shape (Augusta, Order Returned) uses a "When"
//! reflexive head that fails closed today, so the "If" form pins the class.

use engine::game::combat::AttackTarget;
use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::events::GameEvent;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;
use engine::types::zones::Zone;

const PLAYER_COUNT: u8 = 4;
const P2: PlayerId = PlayerId(2);
const P3: PlayerId = PlayerId(3);
const ALL_PLAYERS: [PlayerId; 4] = [P0, P1, P2, P3];

const PROBE: &str = "Whenever this creature attacks, each player exiles a card from their \
     graveyard. If a nonland card was exiled this way, you gain 3 life.";

/// Which card a prompted seat exiles.
#[derive(Clone, Copy, PartialEq)]
enum Pick {
    Nonland,
    Land,
}
const N: Pick = Pick::Nonland;
const L: Pick = Pick::Land;

struct Outcome {
    runner: GameRunner,
    life_before: i32,
    /// `(seat, candidate count)` of every exile prompt, in order.
    prompts: Vec<(PlayerId, usize)>,
    /// Whether each answer's events changed P0's life.
    gained_per_answer: Vec<bool>,
    picked: Vec<ObjectId>,
    not_picked: Vec<ObjectId>,
}

/// Four players, each with one nonland and one land card in their graveyard;
/// the probe (P0) attacks, and every seat exiles its planned card.
fn attack_and_answer(picks: [Pick; 4]) -> Outcome {
    let mut scenario = GameScenario::new_n_player(PLAYER_COUNT, 9225);
    scenario.at_phase(Phase::PreCombatMain);
    let probe = scenario
        .add_creature_from_oracle(P0, "Paused Gate Probe", 2, 2, PROBE)
        .id();
    let mut nonland = Vec::new();
    let mut land = Vec::new();
    for player in ALL_PLAYERS {
        nonland.push(
            scenario
                .add_creature_to_graveyard(player, "Nonland Card", 1, 1)
                .id(),
        );
        land.push(scenario.add_land_to_graveyard(player, "Land Card").id());
    }
    let mut runner = scenario.build();
    let life_before = runner.state().players[0].life;
    runner.advance_to_combat();
    runner
        .declare_attackers(&[(probe, AttackTarget::Player(P1))])
        .expect("the probe must be able to attack");

    let mut prompts = Vec::new();
    let mut gained_per_answer = Vec::new();
    for _ in 0..100 {
        match runner.state().waiting_for.clone() {
            WaitingFor::EffectZoneChoice { player, cards, .. } => {
                prompts.push((player, cards.len()));
                let seat = player.0 as usize;
                let pick = match picks[seat] {
                    Pick::Nonland => nonland[seat],
                    Pick::Land => land[seat],
                };
                let result = runner
                    .act(GameAction::SelectCards { cards: vec![pick] })
                    .expect("the exile choice is accepted");
                gained_per_answer.push(result.events.iter().any(|event| {
                    matches!(event, GameEvent::LifeChanged { player_id, .. } if *player_id == P0)
                }));
            }
            WaitingFor::GameOver { .. } => break,
            _ => {
                let state = runner.state();
                if state.stack.is_empty() && state.resolution_stack.is_empty() {
                    break;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("priority passes");
            }
        }
    }
    let (picked, not_picked) = (0..4)
        .map(|seat| match picks[seat] {
            Pick::Nonland => (nonland[seat], land[seat]),
            Pick::Land => (land[seat], nonland[seat]),
        })
        .unzip();
    Outcome {
        runner,
        life_before,
        prompts,
        gained_per_answer,
        picked,
        not_picked,
    }
}

/// Runs the clause and returns the life P0 gained, after the reach guards.
fn life_gained(picks: [Pick; 4]) -> (i32, Outcome) {
    let outcome = attack_and_answer(picks);
    assert_eq!(
        outcome.prompts,
        ALL_PLAYERS
            .iter()
            .map(|&player| (player, 2))
            .collect::<Vec<_>>(),
        "reach guard: every seat is prompted between its two cards"
    );
    let state = outcome.runner.state();
    for (seat, (picked, kept)) in outcome.picked.iter().zip(&outcome.not_picked).enumerate() {
        assert_eq!(
            state.objects[picked].zone,
            Zone::Exile,
            "seat {seat} exiled its pick"
        );
        assert_eq!(
            state.objects[kept].zone,
            Zone::Graveyard,
            "seat {seat}'s other card stays in the graveyard"
        );
    }
    (state.players[0].life - outcome.life_before, outcome)
}

/// CR 608.2f + CR 608.2c: three seats exile nonland cards and the LAST seat a
/// land. The clause exiled nonland cards, so "you" gain 3 life — once. Reading
/// only the final answer's land would gain nothing.
#[test]
fn a_final_land_answer_does_not_hide_earlier_nonland_exiles() {
    let (gained, outcome) = life_gained([N, N, N, L]);
    assert_eq!(gained, 3, "nonland cards were exiled this way");
    assert_eq!(
        outcome.gained_per_answer,
        vec![false, false, false, true],
        "the gate resolves once, after the final seat's answer"
    );
}

/// CR 608.2c: only the FIRST seat exiles a nonland card; the gate still holds.
#[test]
fn a_first_seat_nonland_exile_satisfies_the_gate() {
    let (gained, _) = life_gained([N, L, L, L]);
    assert_eq!(gained, 3);
}

/// CR 608.2c + CR 109.5: every seat exiles a nonland card and the gate's "you"
/// gains 3 life exactly once — not once per seat.
#[test]
fn every_nonland_exile_still_gains_life_once() {
    let (gained, _) = life_gained([N, N, N, N]);
    assert_eq!(gained, 3);
}

/// CR 608.2c: four land exiles leave the gate false (the negative control).
#[test]
fn only_land_exiles_gain_nothing() {
    let (gained, _) = life_gained([L, L, L, L]);
    assert_eq!(gained, 0);
}

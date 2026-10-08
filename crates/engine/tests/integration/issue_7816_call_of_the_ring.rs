//! Issue #7816 (Call of the Ring) — "Whenever you choose a creature as your
//! Ring-bearer" over the REAL temptation pipeline: `Effect::RingTemptsYou` →
//! bearer choice (interactive AND sole-candidate auto-select) →
//! `GameEvent::RingTemptsYou { chosen_bearer }` → `TriggerMode::RingTemptsYou`
//! gated by `TriggerCondition::ChoseRingBearer` → optional PayCost(2 life) →
//! conditional Draw.
//!
//! Driver note: the boards drive from `commit()` — `resolve()` auto-answers
//! may-triggers (declining this one), which would hide the prompt under test.
//!
//! REVERT DISCRIMINATORS:
//! - without the parser head the mode is `Unknown` — no trigger ever stacks
//!   and `paying_two_life_draws_a_card` never sees its pay prompt;
//! - without the `chosen_bearer: Some(_)` gate in the evaluator,
//!   `a_temptation_with_no_creatures_stacks_no_trigger` sees a pay prompt
//!   from a temptation that chose nothing (CR 701.54a);
//! - without the `player_id == controller` gate,
//!   `an_opponents_choice_does_not_fire_your_call` sees P0's pay prompt off
//!   P1's temptation.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::mana::ManaCost;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const CALL_LINE_2: &str =
    "Whenever you choose a creature as your Ring-bearer, you may pay 2 life. If you do, draw a card.";
const TEMPT: &str = "The Ring tempts you.";

fn hand_size(runner: &GameRunner, player: PlayerId) -> usize {
    runner.state().players[player.0 as usize].hand.len()
}

fn life(runner: &GameRunner, player: PlayerId) -> i32 {
    runner.state().players[player.0 as usize].life
}

/// Drive the committed temptation to a settled empty stack. `pay` answers the
/// Call's optional pay-life prompt when it appears; returns whether that
/// prompt was seen. Unexpected prompts panic, and the loop must REACH the
/// empty-stack terminal.
fn drive_temptation(runner: &mut GameRunner, bearer: Option<ObjectId>, pay: bool) -> bool {
    let mut pay_prompt_seen = false;
    for _ in 0..64 {
        match runner.state().waiting_for.clone() {
            WaitingFor::ChooseRingBearer { candidates, .. } => {
                let target = bearer.expect("a bearer prompt appeared but none was intended");
                assert!(
                    candidates.contains(&target),
                    "intended bearer must be a legal candidate, got {candidates:?}"
                );
                runner
                    .act(GameAction::ChooseRingBearer { target })
                    .expect("the Ring-bearer choice must be accepted");
            }
            WaitingFor::OptionalEffectChoice { .. } => {
                pay_prompt_seen = true;
                runner
                    .act(GameAction::DecideOptionalEffect { accept: pay })
                    .expect("answering the pay-life prompt must succeed");
            }
            WaitingFor::Priority { .. } => {
                if runner.state().stack.is_empty() {
                    return pay_prompt_seen;
                }
                runner
                    .act(GameAction::PassPriority)
                    .expect("PassPriority must be accepted mid-drive");
            }
            other => panic!("unexpected prompt during the temptation: {other:?}"),
        }
    }
    panic!("temptation never settled to an empty stack within 64 steps");
}

/// `creatures`: 0 = choiceless temptation, 1 = sole-candidate auto-select,
/// 2 = interactive `ChooseRingBearer` prompt.
fn call_board(creatures: usize) -> (GameRunner, Option<ObjectId>) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Call of the Ring", CALL_LINE_2)
        .id();
    let bearer = (creatures >= 1).then(|| scenario.add_creature(P0, "Companion Hobbit", 1, 1).id());
    if creatures >= 2 {
        scenario.add_creature(P0, "Second Hobbit", 1, 1);
    }
    let tempt = scenario
        .add_spell_to_hand(P0, "Temptation Test", false)
        .from_oracle_text(TEMPT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.add_card_to_library_top(P0, "Library Filler");
    scenario.with_mana_pool(P0, vec![]);
    let mut runner = scenario.build();
    runner.cast(tempt).commit();
    (runner, bearer)
}

#[test]
fn paying_two_life_draws_a_card() {
    let (mut runner, bearer) = call_board(2);
    let life_before = life(&runner, P0);
    let hand_before = hand_size(&runner, P0);

    let prompted = drive_temptation(&mut runner, bearer, true);

    assert!(prompted, "the pay-life prompt must be offered");
    assert_eq!(life(&runner, P0), life_before - 2, "CR 119.4: 2 life paid");
    assert_eq!(
        hand_size(&runner, P0),
        hand_before + 1,
        "the paid-for draw must resolve"
    );
    assert_eq!(
        runner.state().ring_bearer.get(&P0).copied().flatten(),
        bearer,
        "the chosen creature must be the Ring-bearer"
    );
}

#[test]
fn declining_the_payment_keeps_life_and_hand() {
    let (mut runner, bearer) = call_board(2);
    let life_before = life(&runner, P0);
    let hand_before = hand_size(&runner, P0);

    let prompted = drive_temptation(&mut runner, bearer, false);

    assert!(prompted, "the pay-life prompt must be offered");
    assert_eq!(life(&runner, P0), life_before, "declined: no life paid");
    assert_eq!(hand_size(&runner, P0), hand_before, "declined: no draw");
}

#[test]
fn a_sole_candidate_auto_selection_also_fires_the_call() {
    // CR 701.54a: with exactly one legal candidate the choice is made without
    // a prompt — it is still "you choose a creature as your Ring-bearer".
    let (mut runner, bearer) = call_board(1);
    let life_before = life(&runner, P0);
    let hand_before = hand_size(&runner, P0);

    let prompted = drive_temptation(&mut runner, bearer, true);

    assert!(
        prompted,
        "the auto-selected choice must still fire the Call"
    );
    assert_eq!(life(&runner, P0), life_before - 2);
    assert_eq!(hand_size(&runner, P0), hand_before + 1);
    assert_eq!(
        runner.state().ring_bearer.get(&P0).copied().flatten(),
        bearer
    );
}

#[test]
fn a_temptation_with_no_creatures_stacks_no_trigger() {
    // CR 701.54a: with no legal candidates nothing is chosen — "you choose a
    // creature as your Ring-bearer" never happened, so the Call stays silent.
    let (mut runner, bearer) = call_board(0);
    let life_before = life(&runner, P0);
    let hand_before = hand_size(&runner, P0);

    let prompted = drive_temptation(&mut runner, bearer, true);

    assert!(!prompted, "a choiceless temptation must not fire the Call");
    assert_eq!(life(&runner, P0), life_before);
    assert_eq!(hand_size(&runner, P0), hand_before);
}

#[test]
fn an_opponents_choice_does_not_fire_your_call() {
    // "you choose" — P1's own temptation and bearer choice must not fire
    // P0's enchantment.
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);
    scenario
        .add_enchantment_from_oracle(P0, "Call of the Ring", CALL_LINE_2)
        .id();
    let their_bearer = scenario.add_creature(P1, "Orc Bearer", 1, 1).id();
    // Instant: P1 casts during P0's turn (a sorcery would be refused).
    let tempt = scenario
        .add_spell_to_hand(P1, "Temptation Test", true)
        .from_oracle_text(TEMPT)
        .with_mana_cost(ManaCost::generic(0))
        .id();
    scenario.with_mana_pool(P1, vec![]);
    let mut runner = scenario.build();
    let p0_life = life(&runner, P0);
    let p0_hand = hand_size(&runner, P0);
    // Hand P1 priority on the empty stack, then let P1 cast the instant.
    runner
        .act(GameAction::PassPriority)
        .expect("P0 passes so P1 holds priority");
    runner.cast(tempt).commit();

    let prompted = drive_temptation(&mut runner, Some(their_bearer), true);

    assert!(!prompted, "P1's choice must not fire P0's Call of the Ring");
    assert_eq!(
        runner.state().ring_bearer.get(&P1).copied().flatten(),
        Some(their_bearer)
    );
    assert_eq!(life(&runner, P0), p0_life);
    assert_eq!(hand_size(&runner, P0), p0_hand);
}

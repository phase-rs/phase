//! Integration test for GitHub issue #409 — Gluntch, the Bestower's end-step
//! "choose a player … choose a second player … choose a third player" chain.
//!
//! Gluntch's trigger chains three `Choose(Player)` instructions, each binding a
//! player whose dependent effect then resolves: the 1st chosen player puts two
//! +1/+1 counters on a creature THEY control, the 2nd draws a card, the 3rd
//! creates two Treasure tokens. Before the fix, the dependent `PutCounter`
//! wrongly used `controller: You` and the 2nd/3rd `Choose` clauses fell back to
//! `Effect::Unimplemented`.
//!
//! This file drives the REAL `apply` pipeline in a 3-player game: Gluntch is on
//! P0's battlefield, the turn advances into P0's end step so the engine emits
//! the `Phase` trigger and `process_triggers` puts the ability on the stack;
//! each `Choose(Player)` is answered via a real `GameAction::ChooseOption`.
//! Every observable (counters, drawn card, Treasure tokens) is engine-produced.
//!
//! CR 608.2c: "The controller of the spell or ability follows its instructions
//! in the order written." — the choose → effect → choose → effect ordering.
//! CR 109.4: a reference to "their"/"a creature they control" is a reference to
//! the player chosen by the preceding `Choose`.
//! The "three distinct players / skip the third if fewer than three players"
//! behavior is a Gluntch card ruling, not a CR rule.

use engine::game::scenario::{GameRunner, GameScenario};
use engine::types::actions::GameAction;
use engine::types::counter::CounterType;
use engine::types::game_state::WaitingFor;
use engine::types::identifiers::ObjectId;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

/// Gluntch, the Bestower's printed end-step Oracle text (the relevant trigger).
const GLUNTCH_TRIGGER: &str = "At the beginning of your end step, choose a player. \
     They put two +1/+1 counters on a creature they control. Choose a second \
     player to draw a card. Then choose a third player to create two Treasure \
     tokens.";

const P0: PlayerId = PlayerId(0);
const P1: PlayerId = PlayerId(1);
const P2: PlayerId = PlayerId(2);

fn p1p1_counters(runner: &GameRunner, id: ObjectId) -> u32 {
    runner
        .state()
        .objects
        .get(&id)
        .expect("object still present")
        .counters
        .get(&CounterType::Plus1Plus1)
        .copied()
        .unwrap_or(0)
}

fn hand_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .players
        .iter()
        .find(|p| p.id == player)
        .map(|p| p.hand.len())
        .expect("player exists")
}

fn treasure_count(runner: &GameRunner, player: PlayerId) -> usize {
    runner
        .state()
        .objects
        .values()
        .filter(|o| {
            o.controller == player
                && o.zone == engine::types::zones::Zone::Battlefield
                && o.name.eq_ignore_ascii_case("Treasure")
        })
        .count()
}

/// Drive the engine forward until it pauses on a resolution choice
/// (`NamedChoice` or a targeting state). Passes priority through empty
/// windows and declares no attackers/blockers so the turn advances through
/// combat into P0's end step where Gluntch's trigger fires.
fn advance_to_choice(runner: &mut GameRunner) {
    for _ in 0..120 {
        match &runner.state().waiting_for {
            WaitingFor::NamedChoice { .. }
            | WaitingFor::TargetSelection { .. }
            | WaitingFor::TriggerTargetSelection { .. } => return,
            WaitingFor::Priority { .. } => {
                if runner.act(GameAction::PassPriority).is_err() {
                    return;
                }
            }
            WaitingFor::DeclareAttackers { .. } => {
                if runner
                    .act(GameAction::DeclareAttackers {
                        attacks: vec![],
                        bands: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            WaitingFor::DeclareBlockers { .. } => {
                if runner
                    .act(GameAction::DeclareBlockers {
                        assignments: vec![],
                    })
                    .is_err()
                {
                    return;
                }
            }
            _ => return,
        }
    }
}

/// Answer the current `NamedChoice` by picking the given player id.
fn choose_player(runner: &mut GameRunner, player: PlayerId) {
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice { options, .. } => {
            assert!(
                options.contains(&player.0.to_string()),
                "player {player:?} must be a legal choice; options were {options:?}"
            );
        }
        other => panic!("expected NamedChoice, got {other:?}"),
    }
    runner
        .act(GameAction::ChooseOption {
            choice: player.0.to_string(),
        })
        .expect("choosing a legal player must succeed");
}

/// CR 608.2c + CR 109.4: In a 3-player game, Gluntch's chain binds three
/// distinct players. Each chosen player's dependent effect resolves against
/// THAT player — not Gluntch's controller.
#[test]
fn gluntch_three_player_chain_resolves_each_chosen_player() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    // Each player gets a small library so a draw step (or Gluntch's "draw a
    // card") never decks anyone out and ends the game before the assertions.
    for &pid in &[P0, P1, P2] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    // Gluntch on P0's battlefield, parsed from real Oracle text.
    scenario
        .add_creature_from_oracle(P0, "Gluntch, the Bestower", 3, 3, GLUNTCH_TRIGGER)
        .id();
    // One creature per player — the `PutCounter` on "a creature they control"
    // auto-resolves when the chosen player controls exactly one creature.
    let p0_creature = scenario.add_creature(P0, "P0 Beast", 1, 1).id();
    let p1_creature = scenario.add_creature(P1, "P1 Beast", 1, 1).id();
    let p2_creature = scenario.add_creature(P2, "P2 Beast", 1, 1).id();

    let mut runner = scenario.build();
    let p2_hand_before = hand_count(&runner, P2);

    // Advance into P0's end step — the `Phase` trigger fires and the chain's
    // first `Choose(Player)` surfaces.
    advance_to_choice(&mut runner);

    // 1st choose → P1. P1's creature receives the two +1/+1 counters.
    choose_player(&mut runner, P1);
    advance_to_choice(&mut runner);

    // 2nd choose → P2 (P1 must already be excluded as a distinct choice).
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice { options, .. } => {
            assert!(
                !options.contains(&P1.0.to_string()),
                "the 2nd choose must exclude the already-chosen P1; options {options:?}"
            );
        }
        other => panic!("expected the 2nd NamedChoice, got {other:?}"),
    }
    choose_player(&mut runner, P2);
    advance_to_choice(&mut runner);

    // 3rd choose → P0 (P1 and P2 excluded).
    match &runner.state().waiting_for {
        WaitingFor::NamedChoice { options, .. } => {
            assert!(
                !options.contains(&P1.0.to_string()) && !options.contains(&P2.0.to_string()),
                "the 3rd choose must exclude P1 and P2; options {options:?}"
            );
        }
        other => panic!("expected the 3rd NamedChoice, got {other:?}"),
    }
    choose_player(&mut runner, P0);
    runner.advance_until_stack_empty();

    // CR 109.4: P1 (1st chosen) — their creature gained the two counters.
    assert_eq!(
        p1p1_counters(&runner, p1_creature),
        2,
        "the 1st chosen player's creature must receive two +1/+1 counters"
    );
    // Controller-scope regression guard: P0's and P2's creatures untouched.
    assert_eq!(
        p1p1_counters(&runner, p0_creature),
        0,
        "Gluntch's controller's creature must NOT gain counters (controller scope was the bug)"
    );
    assert_eq!(
        p1p1_counters(&runner, p2_creature),
        0,
        "an unchosen player's creature must not gain counters"
    );

    // CR 109.4: P2 (2nd chosen) drew a card.
    assert_eq!(
        hand_count(&runner, P2),
        p2_hand_before + 1,
        "the 2nd chosen player must draw exactly one card"
    );

    // CR 109.4: P0 (3rd chosen) created two Treasure tokens.
    assert_eq!(
        treasure_count(&runner, P0),
        2,
        "the 3rd chosen player must create two Treasure tokens"
    );
}

/// Gluntch card ruling: with fewer than three players, the third "choose a
/// player" has no eligible (distinct) player and is skipped — resolution
/// completes without error and the third effect does nothing.
#[test]
fn gluntch_two_player_game_skips_the_third_choice() {
    let mut scenario = GameScenario::new_n_player(2, 7);
    scenario.at_phase(Phase::PreCombatMain);

    for &pid in &[P0, P1] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    scenario
        .add_creature_from_oracle(P0, "Gluntch, the Bestower", 3, 3, GLUNTCH_TRIGGER)
        .id();
    let p0_creature = scenario.add_creature(P0, "P0 Beast", 1, 1).id();
    let p1_creature = scenario.add_creature(P1, "P1 Beast", 1, 1).id();

    let mut runner = scenario.build();
    let p1_hand_before = hand_count(&runner, P1);

    advance_to_choice(&mut runner);

    // 1st choose → P1 (gets the counters).
    choose_player(&mut runner, P1);
    advance_to_choice(&mut runner);

    // 2nd choose → P0 (draws a card). P1 already chosen.
    choose_player(&mut runner, P0);
    runner.advance_until_stack_empty();

    // The third "choose a player" has no eligible distinct player (only P0/P1,
    // both already chosen) — it is skipped, and resolution completes cleanly.
    assert!(
        runner.state().stack.is_empty(),
        "resolution must complete even though the third choice is skipped"
    );

    // 1st chosen (P1) got the counters; 2nd chosen (P0) drew a card.
    assert_eq!(p1p1_counters(&runner, p1_creature), 2);
    assert_eq!(p1p1_counters(&runner, p0_creature), 0);
    assert_eq!(hand_count(&runner, P0), p1_hand_before + 1);

    // No Treasure tokens — the skipped third choice's effect did nothing.
    assert_eq!(treasure_count(&runner, P0), 0);
    assert_eq!(treasure_count(&runner, P1), 0);
}

/// CR 608.2d: When the first chosen player controls more than one creature,
/// "put two +1/+1 counters on a creature they control" offers a choice — and
/// per CR 608.2d the *chosen player* makes it. The engine must surface an
/// interactive `ChooseFromZoneChoice` scoped to that player (not auto-pick for
/// them, and not route the choice to Gluntch's controller). The player's
/// selection determines which creature receives the counters.
#[test]
fn gluntch_chosen_player_with_two_creatures_picks_interactively() {
    let mut scenario = GameScenario::new_n_player(3, 42);
    scenario.at_phase(Phase::PreCombatMain);

    for &pid in &[P0, P1, P2] {
        scenario.with_library_top(pid, &["Lib A", "Lib B", "Lib C", "Lib D"]);
    }

    scenario
        .add_creature_from_oracle(P0, "Gluntch, the Bestower", 3, 3, GLUNTCH_TRIGGER)
        .id();
    // P1 (the 1st chosen player) controls TWO creatures — the `PutCounter` on
    // "a creature they control" must surface an interactive selection.
    let p0_creature = scenario.add_creature(P0, "P0 Beast", 1, 1).id();
    let p1_creature_a = scenario.add_creature(P1, "P1 Beast A", 1, 1).id();
    let p1_creature_b = scenario.add_creature(P1, "P1 Beast B", 2, 2).id();
    let p2_creature = scenario.add_creature(P2, "P2 Beast", 1, 1).id();

    let mut runner = scenario.build();

    advance_to_choice(&mut runner);

    // 1st choose → P1.
    choose_player(&mut runner, P1);
    advance_to_choice(&mut runner);

    // CR 608.2d: P1 controls two creatures — the engine must surface an
    // interactive object selection SCOPED TO P1 (the chosen player), listing
    // both of P1's creatures and neither P0's nor P2's.
    match &runner.state().waiting_for {
        WaitingFor::ChooseFromZoneChoice {
            player,
            cards,
            count,
            ..
        } => {
            assert_eq!(
                *player, P1,
                "the chosen player (P1) makes the choice — not Gluntch's controller"
            );
            assert_eq!(*count, 1, "exactly one creature receives the counters");
            assert_eq!(cards.len(), 2, "only P1's two creatures are candidates");
            assert!(cards.contains(&p1_creature_a) && cards.contains(&p1_creature_b));
            assert!(
                !cards.contains(&p0_creature) && !cards.contains(&p2_creature),
                "another player's creatures must not be candidates"
            );
        }
        other => panic!("expected ChooseFromZoneChoice scoped to P1, got {other:?}"),
    }

    // P1 picks Beast B — the player's choice, not an engine auto-pick.
    runner
        .act(GameAction::SelectCards {
            cards: vec![p1_creature_b],
        })
        .expect("selecting one of P1's creatures must succeed");
    runner.advance_until_stack_empty();

    // CR 608.2d: the counters land on the creature P1 chose (Beast B), not the
    // first-enumerated one (Beast A).
    assert_eq!(
        p1p1_counters(&runner, p1_creature_b),
        2,
        "the counters go on the creature the chosen player picked"
    );
    assert_eq!(
        p1p1_counters(&runner, p1_creature_a),
        0,
        "the unpicked creature receives nothing"
    );
    assert_eq!(
        p1p1_counters(&runner, p0_creature),
        0,
        "Gluntch's controller's creature is untouched"
    );

    // The chain continued past the interactive pick: the 2nd and 3rd `Choose`
    // clauses still resolve (no Unimplemented, no stall).
    assert!(
        runner.state().stack.is_empty(),
        "the chain must complete after the interactive creature selection"
    );
}

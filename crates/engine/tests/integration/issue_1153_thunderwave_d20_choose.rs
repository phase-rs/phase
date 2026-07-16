//! Issue #1153 — Thunderwave, 10—19 d20 branch:
//! "You may choose a creature. Thunderwave deals 3 damage to each creature not
//! chosen this way."
//!
//! The branch previously dropped `Effect::Unimplemented { name: "choose" }`, so
//! the selection never happened. It now lowers to an optional
//! `ChooseObjectsIntoTrackedSet` (min 0, max 1) followed by a `DamageAll` whose
//! filter excludes the chosen creature via `Not(InTrackedSet)`.
//!
//! Discriminating runtime test through the real cast + roll + choose pipeline.
//! Choosing one creature spares exactly it and deals 3 to every OTHER creature;
//! declining (an empty submission — the "you may") deals 3 to ALL creatures.
//! All creatures are 3-toughness, so 3 damage is lethal: spared ⇒ battlefield,
//! damaged ⇒ graveyard.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::ability::TargetRef;
use engine::types::actions::GameAction;
use engine::types::game_state::{CastPaymentMode, WaitingFor};
use engine::types::identifiers::ObjectId;
use engine::types::mana::{ManaCost, ManaType, ManaUnit};
use engine::types::phase::Phase;
use engine::types::zones::Zone;

const THUNDERWAVE_ORACLE: &str = "Roll a d20.\n\
1—9 | Thunderwave deals 3 damage to each creature.\n\
10—19 | You may choose a creature. Thunderwave deals 3 damage to each creature not chosen this way.\n\
20 | Thunderwave deals 6 damage to each creature your opponents control.";

struct Board {
    runner: GameRunner,
    spared: ObjectId,
    victim_own: ObjectId,
    victim_opp: ObjectId,
}

/// Build the board for `seed`, cast Thunderwave, and resolve until the roll
/// lands. Returns `Some(board)` only when the roll fell in 10—19 (the sole
/// branch that pauses on `ChooseObjectsSelection`); `None` for 1—9 / 20.
fn cast_and_reach_choice(seed: u64) -> Option<Board> {
    let mut scenario = GameScenario::new_n_player(2, seed);
    scenario.at_phase(Phase::PreCombatMain);

    // Override the printed {2}{R}{R} with generic {4} so plain mana pays it.
    let thunderwave = scenario
        .add_spell_to_hand_from_oracle(P0, "Thunderwave", false, THUNDERWAVE_ORACLE)
        .with_mana_cost(ManaCost::generic(4))
        .id();
    // Three 3-toughness creatures; 3 damage is lethal to each.
    let spared = scenario.add_creature(P0, "Spared", 3, 3).id();
    let victim_own = scenario.add_creature(P0, "Victim Own", 3, 3).id();
    let victim_opp = scenario.add_creature(P1, "Victim Opp", 3, 3).id();

    let mut runner = scenario.build();
    if let Some(p) = runner.state_mut().players.iter_mut().find(|p| p.id == P0) {
        p.mana_pool.mana = (0..4)
            .map(|_| ManaUnit::new(ManaType::Colorless, ObjectId(0), false, vec![]))
            .collect();
    }

    let card_id = runner.state().objects[&thunderwave].card_id;
    runner
        .act(GameAction::CastSpell {
            object_id: thunderwave,
            card_id,
            targets: vec![],
            payment_mode: CastPaymentMode::Auto,
        })
        .expect("cast Thunderwave");
    runner.advance_until_stack_empty();

    // The choose prompt appears iff the d20 landed in 10—19.
    if matches!(
        runner.state().waiting_for,
        WaitingFor::ChooseObjectsSelection { .. }
    ) {
        Some(Board {
            runner,
            spared,
            victim_own,
            victim_opp,
        })
    } else {
        None
    }
}

fn find_10_19_board() -> Board {
    for seed in 0..128 {
        if let Some(board) = cast_and_reach_choice(seed) {
            return board;
        }
    }
    panic!("no seed in 0..128 produced a 10—19 Thunderwave roll");
}

#[test]
fn thunderwave_10_19_choosing_a_creature_spares_only_it() {
    let mut board = find_10_19_board();

    board
        .runner
        .act(GameAction::SelectTargets {
            targets: vec![TargetRef::Object(board.spared)],
        })
        .expect("choose the spared creature");
    board.runner.advance_until_stack_empty();

    let zone = |b: &Board, id: ObjectId| b.runner.state().objects.get(&id).map(|o| o.zone);
    assert_eq!(
        zone(&board, board.spared),
        Some(Zone::Battlefield),
        "the chosen creature takes 0 damage and survives"
    );
    assert_eq!(
        zone(&board, board.victim_own),
        Some(Zone::Graveyard),
        "an un-chosen creature (caster's own) takes 3 and dies"
    );
    assert_eq!(
        zone(&board, board.victim_opp),
        Some(Zone::Graveyard),
        "an un-chosen creature (opponent's) takes 3 and dies"
    );
}

#[test]
fn thunderwave_10_19_declining_damages_all_creatures() {
    let mut board = find_10_19_board();

    // Empty submission == declining the "you may" — the fresh tracked set is
    // empty, so nothing is excluded and every creature takes 3.
    board
        .runner
        .act(GameAction::SelectTargets { targets: vec![] })
        .expect("decline the optional choice");
    board.runner.advance_until_stack_empty();

    let zone = |b: &Board, id: ObjectId| b.runner.state().objects.get(&id).map(|o| o.zone);
    assert_eq!(zone(&board, board.spared), Some(Zone::Graveyard));
    assert_eq!(zone(&board, board.victim_own), Some(Zone::Graveyard));
    assert_eq!(zone(&board, board.victim_opp), Some(Zone::Graveyard));
}

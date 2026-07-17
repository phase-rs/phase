//! Ward of Bones — production-path proof that the *relative-count* LAND-PLAY
//! prohibition (the card's second line) is enforced per the per-player land
//! count, not collapsed onto an unconditional opponent lock.
//!
//! Oracle (verbatim second line): "Each opponent who controls more lands than you
//! can't play lands."
//!
//! CR 305.1 + CR 109.4 + CR 109.5 + CR 115.10: an opponent may play a land only
//! while they do NOT control more lands than Ward of Bones' controller. Under the
//! previous model this line lowered to an UNCONDITIONAL `CantPlayLand` opponent
//! lock — the "controls more lands than you" relative-count predicate was dropped,
//! so an opponent with FEWER/EQUAL lands was wrongly barred. These tests drive the
//! REAL play-land special action (`GameAction::PlayLand` through `apply()` →
//! `handle_play_land` → `player_has_static_other(.., "CantPlayLand")`) and prove
//! the play is rejected ONLY when the opponent's land count exceeds yours.
//!
//! The equal-count test is the reach-guard for the blocked test: same board shape,
//! same active-player/priority/phase, but P1's land count equals P0's — the play
//! now succeeds, proving the rejection is the relative-count prohibition, not a
//! land-limit, phase, or priority artifact. Revert the runtime
//! `check_static_other_by_name` per-player gate and the equal-count play is
//! wrongly rejected; revert the parser "play lands" branch and neither play is
//! blocked (the line lowers to an unconditional lock or falls through).

use engine::game::layers::evaluate_layers;
use engine::game::scenario::{GameScenario, P0, P1};
use engine::game::EngineError;
use engine::types::actions::GameAction;
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

// Verbatim full Oracle text (Scryfall). Parsing BOTH lines proves the land clause
// is extracted from the real multi-line card alongside the three cast statics; the
// cast statics are inert for a land play (they gate spell casts, not land drops).
const WARD_OF_BONES_ORACLE: &str =
    "Each opponent who controls more creatures than you can't cast creature spells. \
     The same is true for artifacts and enchantments.\n\
     Each opponent who controls more lands than you can't play lands.";

/// Build a Ward-of-Bones board: P0 controls Ward of Bones (an artifact) plus
/// `p0_lands` basic lands; P1 controls `p1_lands` basic lands and holds one land
/// in hand. Returns the runner (with P1 active, holding priority, in a main phase)
/// and P1's hand-land `ObjectId`.
fn ward_of_bones_land_scenario(
    p0_lands: usize,
    p1_lands: usize,
) -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PreCombatMain);

    // Ward of Bones is an artifact (never a land) — it does not count toward either
    // player's land total.
    scenario
        .add_creature(P0, "Ward of Bones", 0, 0)
        .as_artifact()
        .from_oracle_text(WARD_OF_BONES_ORACLE);

    let colors = [
        ManaColor::White,
        ManaColor::Blue,
        ManaColor::Black,
        ManaColor::Red,
        ManaColor::Green,
    ];
    for i in 0..p0_lands {
        scenario.add_basic_land(P0, colors[i % colors.len()]);
    }
    for i in 0..p1_lands {
        scenario.add_basic_land(P1, colors[i % colors.len()]);
    }

    let hand_land = scenario.add_land_to_hand(P1, "P1 Land Drop").id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        // Sorcery-speed land play requires P1's own main phase, empty stack, and
        // P1 holding priority.
        state.active_player = P1;
        state.priority_player = P1;
        state.waiting_for = WaitingFor::Priority { player: P1 };
        state.lands_played_this_turn = 0;
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    (runner, hand_land)
}

/// Submit P1's play-land special action through the real `apply()` pipeline.
fn play_hand_land(
    runner: &mut engine::game::scenario::GameRunner,
    hand_land: engine::types::identifiers::ObjectId,
) -> Result<engine::types::game_state::ActionResult, EngineError> {
    let card_id = runner.state().objects[&hand_land].card_id;
    runner.act(GameAction::PlayLand {
        object_id: hand_land,
        card_id,
    })
}

/// P1 controls MORE lands than P0 (2 vs 1). CR 305.2: the play-land special action
/// is suppressed for P1 — `handle_play_land` rejects it via the per-player
/// `CantPlayLand` gate.
#[test]
fn more_lands_blocks_opponent_land_play() {
    let (mut runner, hand_land) = ward_of_bones_land_scenario(1, 2);

    let result = play_hand_land(&mut runner, hand_land);

    // Specifically the CantPlayLand gate — not a land-limit, phase, or priority
    // rejection. The message text is the CR 305.2 gate's own.
    assert!(
        matches!(&result, Err(EngineError::ActionNotAllowed(msg)) if msg.contains("CantPlayLand")),
        "P1 controls more lands than you → the land play must be rejected by the \
         CantPlayLand gate, got {result:?}"
    );
    // The land never left P1's hand.
    assert_eq!(
        runner.state().objects[&hand_land].zone,
        engine::types::zones::Zone::Hand,
        "the rejected land must remain in P1's hand"
    );
}

/// Reach-guard + boundary: P1 controls EQUAL lands to P0 (2 vs 2). "Controls more
/// lands than you" is strict (`Comparator::GT`), so an equal count does NOT bar
/// P1 — the SAME play-land action the blocked test rejects now succeeds. This
/// proves the block above is the relative-count prohibition, not a timing/limit
/// artifact, and pins the GT strictness (revert the runtime per-player gate and
/// this play is wrongly rejected).
#[test]
fn equal_lands_allows_opponent_land_play() {
    let (mut runner, hand_land) = ward_of_bones_land_scenario(2, 2);

    let result = play_hand_land(&mut runner, hand_land);

    assert!(
        result.is_ok(),
        "P1 controls EQUAL (not more) lands than you → the land play must succeed \
         (GT is strict): {result:?}"
    );
    // The land actually resolved onto the battlefield under P1's control.
    let land = &runner.state().objects[&hand_land];
    assert_eq!(
        land.zone,
        engine::types::zones::Zone::Battlefield,
        "the permitted land must have entered the battlefield"
    );
    assert_eq!(land.controller, PlayerId(1), "P1 controls the played land");
}

/// Reach-guard (fewer): P1 controls FEWER lands than P0 (1 vs 3). Clearly below the
/// threshold — the land play succeeds. Complements the equal-count boundary case.
#[test]
fn fewer_lands_allows_opponent_land_play() {
    let (mut runner, hand_land) = ward_of_bones_land_scenario(3, 1);

    let result = play_hand_land(&mut runner, hand_land);

    assert!(
        result.is_ok(),
        "P1 controls fewer lands than you → the land play must succeed: {result:?}"
    );
}

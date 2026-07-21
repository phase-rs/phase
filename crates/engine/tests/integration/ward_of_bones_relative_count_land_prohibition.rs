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
use engine::types::format::FormatConfig;
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

// ---------------------------------------------------------------------------
// Two-Headed Giant: "each opponent" must not treat a TEAMMATE as an opponent.
//
// CR 102.2 / CR 102.3 + CR 810.1: In a multiplayer team game a player's
// opponents are only players NOT on their team. Ward of Bones' land prohibition
// lowers to `CantPlayLand` with `affected = TypedFilter{controller: Opponent}`;
// the player-context branch that consumes it (`static_filter_matches`, the
// `ControllerRef::Opponent` arm) previously implemented "opponent" as the naive
// inequality `source_controller != player_id`. In Two-Headed Giant, P0 and P1
// are TEAMMATES with different ids, so that inequality wrongly barred a teammate
// from playing lands. Routing the arm through the team-aware `is_opponent`
// authority fixes EVERY `Other`-mode static with an `Opponent` filter; in a
// two-player game `is_opponent` reduces to `!=`, so the tests above are
// unchanged.
// ---------------------------------------------------------------------------

/// Player 2 (opposing team, seat 2). Under the Two-Headed Giant
/// `FixedTeams { team_size: 2 }` topology, team A = {P0, P1} and team B =
/// {P2, P3}.
const P2: PlayerId = PlayerId(2);

/// Build a 4-player Two-Headed Giant board (team A = {P0, P1}, team B =
/// {P2, P3}). P0 controls Ward of Bones (an artifact) plus 2 lands. Teammate P1
/// and opponent P2 each control 4 lands — strictly MORE than P0 — and hold one
/// land in hand; P0 also holds a land in hand for the controller reach-guard.
/// Returns the runner (switched to the Two-Headed Giant topology) plus P0's,
/// P1's, and P2's hand-land `ObjectId`s.
fn two_hg_ward_scenario() -> (
    engine::game::scenario::GameRunner,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
    engine::types::identifiers::ObjectId,
) {
    let mut scenario = GameScenario::new_n_player(4, 42);
    scenario.at_phase(Phase::PreCombatMain);

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
    // P0 (Ward's controller): 2 lands.
    for i in 0..2 {
        scenario.add_basic_land(P0, colors[i % colors.len()]);
    }
    // Teammate P1 and opponent P2: 4 lands each — strictly MORE than P0's 2, so
    // Ward's "controls more lands than you" relative-count predicate holds for
    // BOTH. The only difference between them is team membership, which is exactly
    // the axis the fix must discriminate on.
    for i in 0..4 {
        scenario.add_basic_land(P1, colors[i % colors.len()]);
        scenario.add_basic_land(P2, colors[i % colors.len()]);
    }

    let p0_hand = scenario.add_land_to_hand(P0, "P0 Land Drop").id();
    let p1_hand = scenario.add_land_to_hand(P1, "P1 Land Drop").id();
    let p2_hand = scenario.add_land_to_hand(P2, "P2 Land Drop").id();

    let mut runner = scenario.build();
    {
        let state = runner.state_mut();
        // CR 810.1 + CR 102.3: switch to the Two-Headed Giant topology so P0/P1
        // are teammates and P2/P3 are the opposing team. `is_opponent` reads
        // `format_config.topology()`, so this is the single field that makes the
        // team relationships real for the runtime gate.
        state.format_config = FormatConfig::two_headed_giant();
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    (runner, p0_hand, p1_hand, p2_hand)
}

/// Submit `actor`'s play-land action during `active`'s (team's) turn through the
/// real `apply()` pipeline. Resets every player's per-turn land count (under
/// shared team turns `handle_play_land` reads the per-player counter) so the
/// once-per-turn allowance can never mask the `CantPlayLand` gate.
fn play_land_during_team_turn(
    runner: &mut engine::game::scenario::GameRunner,
    active: PlayerId,
    actor: PlayerId,
    hand_land: engine::types::identifiers::ObjectId,
) -> Result<engine::types::game_state::ActionResult, EngineError> {
    {
        let state = runner.state_mut();
        // CR 805.4c: under shared team turns any player on the active team may
        // play a land during that team's turn; `active` is the team's nominal
        // active player, `actor` is the teammate submitting the land drop.
        state.active_player = active;
        state.priority_player = actor;
        state.waiting_for = WaitingFor::Priority { player: actor };
        for p in state.players.iter_mut() {
            p.lands_played_this_turn = 0;
        }
        state.lands_played_this_turn = 0;
        state.layers_dirty.mark_full();
    }
    evaluate_layers(runner.state_mut());
    let card_id = runner.state().objects[&hand_land].card_id;
    runner.act(GameAction::PlayLand {
        object_id: hand_land,
        card_id,
    })
}

/// DISCRIMINATING ASSERTION: it is team A's turn (P0 active) and the non-active
/// teammate P1 — who controls MORE lands than P0 (4 vs 2) — plays a land.
///
/// Under the OLD `source_controller != player_id` opponent check, Ward's
/// `CantPlayLand`/Opponent static matched P1 (P1 != P0) and the relative-count
/// predicate (4 > 2) held, so `player_has_static_other(state, P1, "CantPlayLand")`
/// returned true and `handle_play_land` REJECTED P1's land play. The team-aware
/// `is_opponent` fix recognizes P1 as a TEAMMATE (`is_opponent(P0, P1) == false`),
/// so the static never applies and the play succeeds.
///
/// This assertion FLIPS if the `is_opponent` change is reverted: revert it and
/// `result` becomes `Err(ActionNotAllowed("... CantPlayLand ..."))` and the land
/// stays in P1's hand, failing the `result.is_ok()` and battlefield-zone checks.
#[test]
fn two_headed_giant_teammate_is_not_treated_as_opponent() {
    let (mut runner, _p0_hand, p1_hand, _p2_hand) = two_hg_ward_scenario();

    let result = play_land_during_team_turn(&mut runner, P0, P1, p1_hand);

    assert!(
        result.is_ok(),
        "P1 is P0's TEAMMATE in Two-Headed Giant, not an opponent → Ward of Bones \
         (\"each opponent who controls more lands than you can't play lands\") must \
         NOT block P1's land play even though P1 controls more lands than P0: \
         {result:?}"
    );
    let land = &runner.state().objects[&p1_hand];
    assert_eq!(
        land.zone,
        engine::types::zones::Zone::Battlefield,
        "the teammate's permitted land must have entered the battlefield"
    );
    assert_eq!(land.controller, P1, "P1 controls the played land");
}

/// POSITIVE CONTROL: it is team B's turn (P2 active) and the true opponent P2 —
/// who controls MORE lands than Ward's controller P0 (4 vs 2) — attempts a land
/// play. P2 is on the OTHER team, so `is_opponent(P0, P2)` holds and the
/// relative-count predicate fires: the play must still be rejected by the
/// CR 305.2 `CantPlayLand` gate. This proves the team-aware fix narrows
/// "opponent" to the other team WITHOUT disabling the prohibition for real
/// opponents.
#[test]
fn two_headed_giant_true_opponent_land_play_still_blocked() {
    let (mut runner, _p0_hand, _p1_hand, p2_hand) = two_hg_ward_scenario();

    let result = play_land_during_team_turn(&mut runner, P2, P2, p2_hand);

    // The `msg.contains("CantPlayLand")` assertion doubles as the reach-guard: it
    // proves the action passed the phase, team-membership, and land-limit gates
    // and was rejected SPECIFICALLY by the CR 305.2 CantPlayLand static — not by
    // an unrelated upstream short-circuit (e.g. wrong phase or "only the active
    // team may play lands").
    assert!(
        matches!(&result, Err(EngineError::ActionNotAllowed(msg)) if msg.contains("CantPlayLand")),
        "P2 is a true opponent controlling more lands than P0 → the land play must \
         be rejected by the CantPlayLand gate, got {result:?}"
    );
    assert_eq!(
        runner.state().objects[&p2_hand].zone,
        engine::types::zones::Zone::Hand,
        "the rejected land must remain in P2's hand"
    );
}

/// REACH-GUARD: Ward's own controller P0 is never their own opponent
/// (`is_opponent(P0, P0) == false`), so the `CantPlayLand`/Opponent static never
/// applies to P0 regardless of land counts — P0 plays a land normally during
/// team A's turn.
#[test]
fn two_headed_giant_ward_controller_can_play_land() {
    let (mut runner, p0_hand, _p1_hand, _p2_hand) = two_hg_ward_scenario();

    let result = play_land_during_team_turn(&mut runner, P0, P0, p0_hand);

    assert!(
        result.is_ok(),
        "P0 is never their own opponent → Ward of Bones must not block its own \
         controller's land play: {result:?}"
    );
    assert_eq!(
        runner.state().objects[&p0_hand].zone,
        engine::types::zones::Zone::Battlefield,
        "the controller's permitted land must have entered the battlefield"
    );
}

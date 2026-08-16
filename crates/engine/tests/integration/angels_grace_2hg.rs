//! CR 810.8a — Two-Headed Giant team propagation for `CantLoseTheGame` /
//! `CantWinTheGame`.
//!
//! CR 810.8a (verified against `docs/MagicCompRules.txt:6733`): "Players win
//! and lose the game only as a team, not as individuals. ... If an effect
//! says that a player can't win the game, that player's team can't win the
//! game. If an effect says that a player can't lose the game, that player's
//! team can't lose the game." The Platinum Angel example in the same rule
//! ("Neither that player nor their teammate can lose the game ... neither
//! player on the opposing team can win the game") is the exact clause Angel's
//! Grace prints ("You can't lose the game this turn and your opponents can't
//! win the game this turn").
//!
//! `angels_grace.rs` exercises only two-player fixtures, so it can't
//! distinguish "protects the named player" from "protects the named player's
//! team" — a 1v1 game has no teammate to leave unprotected. These fixtures
//! install the grant on ONE 2HG teammate only (mirroring the transient,
//! `SpecificPlayer`-scoped continuous effect Angel's Grace's resolution
//! installs — see `effect.rs::register_transient_effect`'s player-target
//! branch) and check the OTHER, non-granted teammate — both through the SBA
//! path (`check_state_based_actions`) and the effect-stated path
//! (`resolve_lose` / `resolve_win`), matching the two runtime authorities the
//! review flagged: `sba.rs`'s `player_has_cant_lose` and
//! `static_abilities.rs`'s `player_has_cant_win`.
//!
//! 2HG seating (`FormatConfig::two_headed_giant()`, 4 players): P0+P1 are one
//! team, P2+P3 the other (see `game::topology::teammates`).

use engine::game::effects::win_lose::{resolve_lose, resolve_win};
use engine::game::sba::check_state_based_actions;
use engine::types::ability::{
    ContinuousModification, Duration, Effect, ResolvedAbility, TargetFilter, TargetRef,
};
use engine::types::format::FormatConfig;
use engine::types::game_state::GameState;
use engine::types::identifiers::ObjectId;
use engine::types::player::PlayerId;
use engine::types::statics::StaticMode;

const TEAM_A_0: PlayerId = PlayerId(0);
const TEAM_A_1: PlayerId = PlayerId(1);
const TEAM_B_0: PlayerId = PlayerId(2);
const TEAM_B_1: PlayerId = PlayerId(3);

/// Install a transient, turn-bound `CantLoseTheGame` grant scoped to exactly
/// one player — mirrors the `SpecificPlayer`-targeted TCE Angel's Grace's
/// resolution installs for its caster (`effect.rs::register_transient_effect`),
/// without going through the full split-second cast pipeline.
fn grant_transient_cant_lose(state: &mut GameState, player: PlayerId) {
    state.add_transient_continuous_effect(
        ObjectId(9001),
        player,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificPlayer { id: player },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantLoseTheGame,
        }],
        None,
    );
}

/// Install a transient, turn-bound `CantWinTheGame` grant scoped to exactly
/// one player — the "your opponents can't win the game" half of Angel's
/// Grace, installed per-opponent by the same TCE dispatch.
fn grant_transient_cant_win(state: &mut GameState, player: PlayerId) {
    state.add_transient_continuous_effect(
        ObjectId(9002),
        player,
        Duration::UntilEndOfTurn,
        TargetFilter::SpecificPlayer { id: player },
        vec![ContinuousModification::AddStaticMode {
            mode: StaticMode::CantWinTheGame,
        }],
        None,
    );
}

/// (a) SBA loss: CR 810.8c team life total is <= 0 (P0 at -10, P1 at 5, team
/// total -5 — the same shape as `sba::tests::sba_2hg_team_dies_together`,
/// which proves this combined total would otherwise eliminate the team), but
/// only P1 holds the transient `CantLoseTheGame` grant. CR 810.8a: the grant
/// protects the whole team, so NEITHER member is eliminated by the life SBA,
/// even though P0 (the one actually below the individual eyeball test)
/// received no grant of its own.
#[test]
fn sba_loss_blocked_for_2hg_team_when_only_one_teammate_granted() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    state.players[0].life = -10;
    state.players[1].life = 5;
    grant_transient_cant_lose(&mut state, TEAM_A_1);
    let mut events = Vec::new();

    check_state_based_actions(&mut state, &mut events);

    assert!(
        !state.players[0].is_eliminated,
        "CR 810.8a: P0 must survive the life SBA via teammate P1's can't-lose grant"
    );
    assert!(
        !state.players[1].is_eliminated,
        "the granted teammate must also survive"
    );
}

/// (c) Negative control for (a): the opposing team holds no grant at all and
/// has the identical <= 0 combined life total — the team loses normally.
/// Proves the life-SBA elimination path is live this game, so (a)'s survival
/// isn't vacuous.
#[test]
fn sba_loss_still_applies_to_ungranted_2hg_team() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    state.players[2].life = -10;
    state.players[3].life = 5;
    let mut events = Vec::new();

    check_state_based_actions(&mut state, &mut events);

    assert!(
        state.players[2].is_eliminated,
        "unprotected team must still lose to the life SBA (reach guard)"
    );
    assert!(
        state.players[3].is_eliminated,
        "CR 810.8a: the whole team is eliminated together, not just the individual"
    );
}

/// (b) Effect-stated loss: only P1 holds the transient `CantLoseTheGame`
/// grant; a "target player loses the game" effect (Door to Nothingness /
/// Angel's-Grace-adjacent wording) is aimed at P0, the NON-granted teammate.
/// CR 810.8a: the loss is precluded for the whole team, so P0 survives.
#[test]
fn effect_stated_loss_blocked_for_non_granted_2hg_teammate() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    grant_transient_cant_lose(&mut state, TEAM_A_1);

    let doom = ResolvedAbility::new(
        Effect::LoseTheGame { target: None },
        vec![TargetRef::Player(TEAM_A_0)],
        ObjectId(1),
        TEAM_B_0,
    );
    let mut events = Vec::new();
    resolve_lose(&mut state, &doom, &mut events).unwrap();

    assert!(
        !state.players[0].is_eliminated,
        "CR 810.8a: P0 must not lose to an effect-stated loss via teammate P1's grant"
    );
    assert!(!state.players[1].is_eliminated);

    // Reach guard: the identical effect aimed at the opposing (ungranted)
    // team still eliminates it — proving the effect-loss path is live and
    // team-cascading (CR 810.8a: the whole team goes down together).
    let doom_reach = ResolvedAbility::new(
        Effect::LoseTheGame { target: None },
        vec![TargetRef::Player(TEAM_B_0)],
        ObjectId(2),
        TEAM_A_0,
    );
    resolve_lose(&mut state, &doom_reach, &mut events).unwrap();
    assert!(
        state.players[2].is_eliminated,
        "unprotected team must still lose (reach guard)"
    );
    assert!(
        state.players[3].is_eliminated,
        "CR 810.8a: the whole opposing team is eliminated together"
    );
}

/// (d) Can't-win discrimination: only P2 holds the transient `CantWinTheGame`
/// grant (the "opponents can't win" half of Angel's Grace, installed on one
/// member of the opposing team); an effect-stated win is attempted by P3,
/// P2's NON-granted teammate. CR 810.8a: the restriction covers the whole
/// team, so the win effect must resolve with no eliminations.
#[test]
fn effect_stated_win_blocked_for_non_granted_2hg_teammate() {
    let mut state = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    grant_transient_cant_win(&mut state, TEAM_B_0);

    let win = ResolvedAbility::new(
        Effect::WinTheGame { target: None },
        vec![],
        ObjectId(3),
        TEAM_B_1,
    );
    let mut events = Vec::new();
    resolve_win(&mut state, &win, &mut events).unwrap();

    assert!(
        !state.players[0].is_eliminated && !state.players[1].is_eliminated,
        "CR 810.8a: P3's win must be a no-op — teammate P2's can't-win grant covers the whole team"
    );
    assert!(
        !state.players[2].is_eliminated && !state.players[3].is_eliminated,
        "a blocked win eliminates no one, including the (non-)winning team itself"
    );

    // Reach guard: without any grant, the identical effect-stated win by the
    // opposing team's member DOES eliminate the other team, proving the win
    // path is live and this isn't vacuous.
    let mut state2 = GameState::new(FormatConfig::two_headed_giant(), 4, 42);
    let win2 = ResolvedAbility::new(
        Effect::WinTheGame { target: None },
        vec![],
        ObjectId(4),
        TEAM_B_1,
    );
    let mut events2 = Vec::new();
    resolve_win(&mut state2, &win2, &mut events2).unwrap();
    assert!(
        state2.players[0].is_eliminated && state2.players[1].is_eliminated,
        "unprotected opposing team must still lose to the win effect (reach guard)"
    );
}

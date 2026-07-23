//! Citadel of Pain (#6508) — each-player phase-trigger anaphor binding.
//!
//! Oracle (Citadel of Pain):
//!   At the beginning of each player's end step, this enchantment deals X
//!   damage to that player, where X is the number of untapped lands they
//!   control.
//!
//! The bug: the `where X is … they control` count bound to the SOURCE's
//! controller (`ControllerRef::You`) instead of the phase's active player
//! (`ScopedPlayer`), so on an opponent's end step Citadel dealt the CONTROLLER's
//! untapped-land count to the opponent (frequently 0 when the controller tapped
//! out). The recipient ("to that player") was already `ScopedPlayer` and is
//! unchanged — these tests additionally pin that the recipient is correct.
//!
//! CR references:
//!   - CR 513.1: the end step begins; "at the beginning of each player's end
//!     step" triggers fire (CR 603.2b) with the phase's active player
//!     (CR 102.1) stamped as the scoped player.
//!   - CR 503.1a: upkeep triggers (Iron Maiden) go on the stack as the upkeep
//!     step begins.
//!
//! Revert map (discriminating tests fail if Part A / Part B is reverted):
//!   * `opponent_end_step_damages_phase_player_by_their_untapped_lands` (T1) —
//!     asymmetric counts (P0=1, P1=3). Post-fix P1 takes 3; pre-fix P1 takes the
//!     controller's count (1). REVERT-FAILING for Part A.
//!   * `phase_player_with_no_untapped_lands_takes_zero` (T2) — P1's lands are all
//!     tapped, so post-fix X=0 (exercises the `Untapped` count at resolution,
//!     per the 2004-10-04 ruling). Pre-fix X = P0's untapped count (2).
//!     REVERT-FAILING for Part A.
//!   * `controller_end_step_takes_own_count` (T3) — companion, NON-discriminating
//!     (on the controller's own end step scoped == controller, so pre- and
//!     post-fix agree). Pins the caster-relative reading is preserved.
//!   * `iron_maiden_upkeep_damage_equals_scoped_hand_minus_four` (T4) — Iron
//!     Maiden's possessive hand-count. Post-fix deals hand−4 = 3; pre-fix the
//!     `TargetZoneCardCount` resolves 0 with no player target, so it deals 0.
//!     REVERT-FAILING for Part B.

use engine::game::scenario::{GameRunner, GameScenario, P0, P1};
use engine::types::game_state::WaitingFor;
use engine::types::mana::ManaColor;
use engine::types::phase::Phase;
use engine::types::player::PlayerId;

const CITADEL_ORACLE: &str = "At the beginning of each player's end step, this enchantment deals X damage to that player, where X is the number of untapped lands they control.";

const IRON_MAIDEN_ORACLE: &str = "At the beginning of each opponent's upkeep, this artifact deals X damage to that player, where X is the number of cards in their hand minus 4.";

/// Build Citadel of Pain (as an enchantment) under P0's control, give P0 and P1
/// the requested number of untapped basic lands, and set `active` as the active
/// player whose end step we will advance into. When `p1_lands_tapped` is set,
/// P1's lands are all tapped after build so they no longer count as untapped.
fn setup_citadel(
    p0_untapped_lands: usize,
    p1_lands: usize,
    p1_lands_tapped: bool,
    active: PlayerId,
) -> GameRunner {
    let mut scenario = GameScenario::new();
    // Start after combat so advancing to the end step does not halt at
    // DeclareAttackers (mirrors bre_of_clan_stoutarm_endstep).
    scenario.at_phase(Phase::PostCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Citadel of Pain", 0, 0, CITADEL_ORACLE)
        .as_enchantment();

    for _ in 0..p0_untapped_lands {
        scenario.add_basic_land(P0, ManaColor::Green);
    }
    let mut p1_land_ids = Vec::new();
    for _ in 0..p1_lands {
        p1_land_ids.push(scenario.add_basic_land(P1, ManaColor::Green));
    }

    // Library padding so nothing decks during resolution.
    for _ in 0..10 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    let mut runner = scenario.build();
    // Make the whole priority triple consistent for `active`. `at_phase` stamped
    // `waiting_for = Priority { P0 }` (the default active player at build time);
    // overriding only `active_player`/`priority_player` would leave `waiting_for`
    // stale on a priority phase (PostCombatMain), which stalls `advance_to_phase`.
    runner.state_mut().active_player = active;
    runner.state_mut().priority_player = active;
    runner.state_mut().waiting_for = WaitingFor::Priority { player: active };

    if p1_lands_tapped {
        // CR 110.5: a tapped land no longer satisfies the `Untapped` count
        // qualifier (tapped/untapped are status categories).
        for id in &p1_land_ids {
            runner.state_mut().objects.get_mut(id).unwrap().tapped = true;
        }
    }

    runner
}

/// T1 (REVERT-FAILING, Part A): Citadel under P0; P0 has 1 untapped land, P1 has
/// 3. On P1's end step the damage must equal P1's untapped-land count (3), dealt
/// to P1 (the phase player). Pre-fix the amount counts P0's untapped lands (1).
#[test]
fn opponent_end_step_damages_phase_player_by_their_untapped_lands() {
    let mut runner = setup_citadel(1, 3, false, P1);

    let p0_before = runner.state().players[P0.0 as usize].life;
    let p1_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let p1_delta = runner.state().players[P1.0 as usize].life - p1_before;
    let p0_delta = runner.state().players[P0.0 as usize].life - p0_before;

    assert_eq!(
        p1_delta, -3,
        "P1's end step: Citadel must deal P1's untapped-land count (3) to P1, \
         not the controller's count; got delta {p1_delta}"
    );
    assert_eq!(
        p0_delta, 0,
        "the damage recipient is the phase player (P1), so P0 takes none"
    );
}

/// T2 (REVERT-FAILING, Part A): P1's lands are all tapped, so at P1's end step
/// the untapped-land count is 0 and P1 takes no damage. Pre-fix the amount reads
/// P0's untapped count (2) and P1 wrongly takes 2. Exercises the `Untapped`
/// qualifier at resolution (2004-10-04 ruling).
#[test]
fn phase_player_with_no_untapped_lands_takes_zero() {
    let mut runner = setup_citadel(2, 3, true, P1);

    let p1_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let p1_delta = runner.state().players[P1.0 as usize].life - p1_before;
    assert_eq!(
        p1_delta, 0,
        "all of P1's lands are tapped, so the untapped-land count is 0 and P1 \
         takes no damage; got delta {p1_delta}"
    );
}

/// T3 (companion, NON-discriminating): on the controller's OWN end step the
/// scoped player is the controller, so pre- and post-fix agree. Pins that the
/// caster-relative reading of "they control" is preserved.
#[test]
fn controller_end_step_takes_own_count() {
    let mut runner = setup_citadel(2, 3, false, P0);

    let p0_before = runner.state().players[P0.0 as usize].life;
    let p1_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_end_step();
    runner.advance_until_stack_empty();

    let p0_delta = runner.state().players[P0.0 as usize].life - p0_before;
    let p1_delta = runner.state().players[P1.0 as usize].life - p1_before;

    assert_eq!(
        p0_delta, -2,
        "P0's own end step: Citadel deals P0's untapped-land count (2) to P0"
    );
    assert_eq!(p1_delta, 0, "P1 is not the phase player, takes none");
}

/// Build Iron Maiden (as an artifact) under P0's control and give P1 a hand of
/// `p1_hand` cards. The scenario starts on P0's post-combat main phase;
/// `advance_to_upkeep` then crosses the turn boundary into P1's turn, firing
/// Iron Maiden's each-opponent upkeep trigger from a consistently-transitioned
/// game state (active == P1, priority stamped by the engine) rather than a
/// hand-poked active player — CR 500.1 / CR 503.1a. P1's upkeep precedes its
/// draw step, so P1's hand is still `p1_hand` when the trigger resolves.
fn setup_iron_maiden(p1_hand: usize) -> GameRunner {
    let mut scenario = GameScenario::new();
    scenario.at_phase(Phase::PostCombatMain);

    scenario
        .add_creature_from_oracle(P0, "Iron Maiden", 0, 0, IRON_MAIDEN_ORACLE)
        .as_artifact();

    for _ in 0..p1_hand {
        scenario.add_card_to_hand(P1, "Plains");
    }

    // Library padding so crossing the turn boundary (draw steps) doesn't deck
    // either player during priority advancement.
    for _ in 0..20 {
        scenario.add_card_to_library_top(P0, "Plains");
        scenario.add_card_to_library_top(P1, "Plains");
    }

    scenario.build()
}

/// T4 (REVERT-FAILING, Part B): Iron Maiden under P0; P1's hand is 7. On P1's
/// upkeep the damage is hand − 4 = 3, dealt to P1. Pre-fix the possessive
/// hand-count is a `TargetZoneCardCount` that resolves 0 with no player target,
/// so Iron Maiden deals max(0, 0 − 4) = 0.
#[test]
fn iron_maiden_upkeep_damage_equals_scoped_hand_minus_four() {
    let mut runner = setup_iron_maiden(7);

    let p1_before = runner.state().players[P1.0 as usize].life;

    runner.advance_to_upkeep();
    runner.advance_until_stack_empty();

    let p1_delta = runner.state().players[P1.0 as usize].life - p1_before;
    assert_eq!(
        p1_delta, -3,
        "P1's hand is 7, so Iron Maiden deals 7 − 4 = 3 to P1; got delta {p1_delta}"
    );
}
